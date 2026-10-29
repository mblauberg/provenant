//! Local demand counters: a bounded, rotated JSON-lines log in the state root.
//! The command edge decides what to count; this module owns the log.
use provenant_core::{Clock, ProjectId, Roots};
use provenant_os::fs::{Directory, FsError, LockMode};
use provenant_records::Trace;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{self, Read, Seek, SeekFrom, Write},
};
use thiserror::Error;

pub const EVENT_KINDS: &[&str] = &["cli.verb", "mcp.tool"];
const CAP: u64 = 5_000_000;
const LOG: &str = "usage.jsonl";
const BACKUP: &str = "usage.jsonl.1";
const LOCK: &str = "usage.lock";
const LINE_CAP: usize = 512;

#[derive(Debug, Error)]
pub enum UsageError {
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error("cannot read the usage log: {0}")]
    Io(#[from] io::Error),
}

impl UsageError {
    /// Stable machine-readable refusal code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Filesystem(_) => "filesystem",
            Self::Io(_) => "io",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Event {
    kind: String,
    unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    duration_ms: u64,
    timestamp: String,
    /// Only the project, and only when attribution is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace: Option<Trace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    /// A chair session's Claude config directory, relative to the home
    /// directory, so its transcript can be found without a registered account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<String>,
}

fn project_trace(project_id: Option<ProjectId>) -> Option<Trace> {
    project_id.map(|project| Trace {
        project: Some(project),
        ..Trace::default()
    })
}

fn rotate(directory: &Directory, incoming: u64) -> Option<()> {
    let size = || {
        directory
            .open_read(OsStr::new(LOG))
            .ok()?
            .metadata()
            .ok()
            .map(|m| m.size)
    };
    if size()?.saturating_add(incoming) <= CAP {
        return Some(());
    }
    // Only rotation locks. Contending appenders keep their single write.
    let _lock = directory
        .try_lock(OsStr::new(LOCK), LockMode::Exclusive)
        .ok()?;
    if size()?.saturating_add(incoming) > CAP {
        directory
            .replace(OsStr::new(LOG), directory, OsStr::new(BACKUP))
            .ok()?;
    }
    Some(())
}

/// Called by existing MCP workers, never by the transport event loop.
pub fn maintain_tools(roots: &Roots) {
    if let Ok(directory) = Directory::open_anchor(roots.state.as_path()) {
        let _ = rotate(&directory, LINE_CAP as u64);
    }
}

fn append(roots: &Roots, event: Event, rotate_log: bool) -> Option<()> {
    let mut line = serde_json::to_vec(&event).ok()?;
    line.push(b'\n');
    if line.len() > LINE_CAP {
        return None;
    }
    // Counting never creates the state root: reads and help create no state.
    let directory = Directory::open_anchor(roots.state.as_path()).ok()?;
    if rotate_log {
        let _ = rotate(&directory, line.len() as u64);
    }
    let mut file = directory.open_append(OsStr::new(LOG)).ok()?;
    // O_APPEND and one small write; readers skip torn/malformed lines.
    (file.write(&line).ok()? == line.len()).then_some(())
}

pub fn record_cli(
    roots: &Roots,
    clock: &dyn Clock,
    verb: &str,
    exit_code: u8,
    duration_ms: u64,
    project_id: Option<ProjectId>,
) {
    let Ok(now) = clock.wall_now() else { return };
    let _ = append(
        roots,
        Event {
            kind: EVENT_KINDS[0].into(),
            unit: verb.into(),
            exit_code: Some(exit_code),
            outcome: None,
            duration_ms,
            timestamp: now.to_string(),
            trace: project_trace(project_id),
            account: None,
            session: None,
            config: None,
        },
        true,
    );
}
pub fn record_tool(
    roots: &Roots,
    clock: &dyn Clock,
    tool: &str,
    outcome: &str,
    duration_ms: u64,
    project_id: Option<ProjectId>,
) {
    let Ok(now) = clock.wall_now() else { return };
    let _ = append(
        roots,
        Event {
            kind: EVENT_KINDS[1].into(),
            unit: tool.into(),
            exit_code: None,
            outcome: Some(outcome.into()),
            duration_ms,
            timestamp: now.to_string(),
            trace: project_trace(project_id),
            account: None,
            session: None,
            config: None,
        },
        false,
    );
}

/// The account recorded for a chair session whose config directory is not a
/// registered account, so such sessions stay visible instead of being dropped.
pub const UNATTRIBUTED: &str = "unattributed";

/// Record the serving account resolved by the chair's SessionStart hook, or
/// [`UNATTRIBUTED`] when none resolved. `config` is the session's Claude config
/// directory relative to the home directory, when it lies under it.
pub fn record_chair(
    roots: &Roots,
    clock: &dyn Clock,
    account: &str,
    session: &str,
    project: Option<ProjectId>,
    config: Option<&str>,
) {
    let Ok(now) = clock.wall_now() else { return };
    let _ = append(
        roots,
        Event {
            kind: "chair.session".into(),
            unit: "claude".into(),
            exit_code: None,
            outcome: None,
            duration_ms: 0,
            timestamp: now.to_string(),
            trace: project_trace(project),
            account: Some(account.into()),
            session: Some(session.into()),
            config: config.map(str::to_owned),
        },
        true,
    );
}

#[derive(Debug)]
pub struct ChairSession {
    pub adapter: String,
    pub account: String,
    pub session: String,
    pub project: Option<ProjectId>,
    /// Claude config directory relative to home, as recorded by the hook.
    pub config: Option<String>,
    /// Token counts per model once [`crate::attach_chair_usage`] has read the
    /// session's transcript.
    pub usage: Vec<crate::ChairModelUsage>,
    /// Why `usage` is empty although the session ran, if it could not be read.
    pub unread: Option<crate::Unread>,
}

/// Recent chair observations. The caller deduplicates repeated SessionStart
/// events by project, account and session within the selected window.
pub fn chair_sessions(
    roots: &Roots,
    since: jiff::Timestamp,
    until: jiff::Timestamp,
) -> Result<Vec<ChairSession>, UsageError> {
    Ok(events(roots)?
        .into_iter()
        .filter_map(|event| {
            if event.kind != "chair.session" || event.unit != "claude" {
                return None;
            }
            let time = event
                .timestamp
                .parse::<provenant_core::WallTime>()
                .ok()?
                .as_timestamp();
            if time < since || time >= until {
                return None;
            }
            Some(ChairSession {
                adapter: event.unit,
                account: event.account?,
                session: event.session?,
                project: event.trace.and_then(|trace| trace.project),
                config: event.config,
                usage: Vec::new(),
                unread: None,
            })
        })
        .collect())
}

#[derive(Serialize)]
pub struct Unit {
    pub kind: String,
    pub unit: String,
    pub calls: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<&'static str>,
}
#[derive(Serialize)]
pub struct Report {
    pub since: Option<String>,
    pub until: Option<String>,
    pub units: Vec<Unit>,
}

/// A newline ends a record. So does a zero byte: zero-filled space, such as a
/// crash can leave, must not swallow the record written after it.
fn ends_record(byte: u8) -> bool {
    byte == b'\n' || byte == 0
}

fn events(roots: &Roots) -> Result<Vec<Event>, UsageError> {
    let mut events = Vec::new();
    let directory = match Directory::open_anchor(roots.state.as_path()) {
        Ok(directory) => Some(directory),
        Err(error) if error.is_not_found() => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(directory) = directory {
        for name in [BACKUP, LOG] {
            let mut file = match directory.open_read(OsStr::new(name)) {
                Ok(file) => file,
                Err(error) if error.is_not_found() => continue,
                Err(error) => return Err(error.into()),
            };
            let size = file.metadata()?.size;
            let offset = size.saturating_sub(CAP);
            // A window that starts just after a record end holds only whole records.
            let mut whole = true;
            if offset > 0 {
                let mut before = [0u8];
                file.seek(SeekFrom::Start(offset - 1))?;
                file.read_exact(&mut before)?;
                whole = ends_record(before[0]);
            }
            let mut bytes = Vec::new();
            // A writer can cross the cap while rotation is busy. Bound the read,
            // rather than failing the report when the file grows.
            file.seek(SeekFrom::Start(offset))?;
            (&mut file).take(CAP).read_to_end(&mut bytes)?;
            if !whole {
                let skip = bytes
                    .iter()
                    .position(|byte| ends_record(*byte))
                    .map_or(bytes.len(), |end| end + 1);
                bytes.drain(..skip);
            }
            // Ignore torn or malformed records; only complete bounded lines count.
            for line in bytes.split_inclusive(|byte| ends_record(*byte)) {
                if line.len() > LINE_CAP || !line.ends_with(b"\n") {
                    continue;
                }
                let Ok(event) = serde_json::from_slice::<Event>(line) else {
                    continue;
                };
                events.push(event);
            }
        }
    }
    Ok(events)
}

/// Count events in `[since, until)` over the installed `inventory` of
/// `(kind, unit)` pairs, which report zero calls until used.
pub fn report(
    roots: &Roots,
    inventory: impl IntoIterator<Item = (String, String)>,
    since: Option<jiff::Timestamp>,
    until: Option<jiff::Timestamp>,
) -> Result<Report, UsageError> {
    let mut units: BTreeMap<(String, String, Option<ProjectId>), u64> = inventory
        .into_iter()
        .map(|(kind, unit)| ((kind, unit, None), 0))
        .collect();
    for event in events(roots)? {
        if !EVENT_KINDS.contains(&event.kind.as_str()) || event.unit.len() > 128 {
            continue;
        }
        let Ok(time) = event.timestamp.parse::<provenant_core::WallTime>() else {
            continue;
        };
        let time = time.as_timestamp();
        if since.is_some_and(|since| time < since) || until.is_some_and(|until| time >= until) {
            continue;
        }
        let calls = units
            .entry((event.kind.clone(), event.unit.clone(), None))
            .or_default();
        *calls = calls.saturating_add(1);
        if let Some(project_id) = event.trace.and_then(|trace| trace.project) {
            let calls = units
                .entry((event.kind, event.unit, Some(project_id)))
                .or_default();
            *calls = calls.saturating_add(1);
        }
    }
    let mut units: Vec<_> = units
        .into_iter()
        .map(|((kind, unit, project_id), calls)| Unit {
            kind,
            unit,
            calls,
            project_id,
            scope: project_id.map(|_| "project"),
        })
        .collect();
    units.sort_by(|a, b| {
        b.calls
            .cmp(&a.calls)
            .then(a.kind.cmp(&b.kind))
            .then(a.unit.cmp(&b.unit))
            .then(a.project_id.cmp(&b.project_id))
    });
    Ok(Report {
        since: since.map(|s| s.to_string()),
        until: until.map(|s| s.to_string()),
        units,
    })
}

#[cfg(test)]
mod tests;
