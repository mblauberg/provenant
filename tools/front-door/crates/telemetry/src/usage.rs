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

pub const EVENT_KINDS: &[&str] = &["cli.verb"];
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
    /// Explicit wait mode from the parsed status command; historical values unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wait: Option<bool>,
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
    record_cli_context(
        roots,
        clock,
        verb,
        exit_code,
        duration_ms,
        project_id,
        None,
        None,
        None,
    );
}

/// Count parsed command metadata only. `session` is an existing host identity,
/// not a new telemetry identity; flags contain no argument values.
#[allow(clippy::too_many_arguments)]
pub fn record_cli_context(
    roots: &Roots,
    clock: &dyn Clock,
    verb: &str,
    exit_code: u8,
    duration_ms: u64,
    project_id: Option<ProjectId>,
    session: Option<&str>,
    attempt: Option<provenant_core::AttemptId>,
    wait: Option<bool>,
) {
    let Ok(now) = clock.wall_now() else { return };
    let session = session.filter(|value| {
        value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    });
    let _ = append(
        roots,
        Event {
            kind: EVENT_KINDS[0].into(),
            unit: verb.into(),
            exit_code: Some(exit_code),
            outcome: None,
            duration_ms,
            timestamp: now.to_string(),
            trace: Some(Trace {
                project: project_id,
                attempt,
                ..Trace::default()
            })
            .filter(|trace| trace.project.is_some() || trace.attempt.is_some()),
            account: None,
            session: session.map(str::to_owned),
            config: None,
            wait,
        },
        true,
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
            wait: None,
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

/// Local command ergonomics. Sequences require the same recorded session and
/// project; historical uncorrelated commands remain unknown, never a retry.
#[derive(Debug, Default, Serialize)]
pub struct Ergonomics {
    pub cli_calls: u64,
    pub help_calls: u64,
    pub failed_verb_then_retry: u64,
    pub first_attempt_success: u64,
    pub first_attempts: u64,
    pub sequence_unknown: u64,
    pub wait_calls: u64,
    pub poll_calls: u64,
    pub wait_mode_unknown: u64,
}

pub fn ergonomics(
    roots: &Roots,
    since: Option<jiff::Timestamp>,
    until: Option<jiff::Timestamp>,
    project: Option<ProjectId>,
) -> Result<Ergonomics, UsageError> {
    let mut observed = events(roots)?;
    observed.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    let mut result = Ergonomics::default();
    let mut previous = BTreeMap::<
        (
            Option<String>,
            Option<provenant_core::AttemptId>,
            Option<ProjectId>,
        ),
        (String, bool, jiff::Timestamp),
    >::new();
    for event in observed {
        if event.kind != "cli.verb" {
            continue;
        }
        let event_project = event.trace.as_ref().and_then(|trace| trace.project);
        let attempt = event.trace.as_ref().and_then(|trace| trace.attempt);
        if project.is_some() && event_project != project {
            continue;
        }
        let Ok(at) = event.timestamp.parse::<jiff::Timestamp>() else {
            continue;
        };
        if since.is_some_and(|since| at < since) || until.is_some_and(|until| at >= until) {
            continue;
        }
        result.cli_calls += 1;
        if event.unit == "help" {
            result.help_calls += 1;
        }
        if event.unit == "status" {
            match event.wait {
                Some(true) => result.wait_calls += 1,
                Some(false) => result.poll_calls += 1,
                None => result.wait_mode_unknown += 1,
            }
        }
        // Help does not interrupt a failed command's repair sequence.
        if event.unit == "help" {
            continue;
        }
        let Some(code) = event.exit_code else {
            result.sequence_unknown += 1;
            continue;
        };
        if (event.session.is_none() || event_project.is_none()) && attempt.is_none() {
            result.sequence_unknown += 1;
            continue;
        }
        let key = (event.session, attempt, event_project);
        let retry = previous.get(&key).is_some_and(|(verb, failed, time)| {
            verb == &event.unit && *failed && at.as_second().saturating_sub(time.as_second()) <= 300
        });
        if retry {
            result.failed_verb_then_retry += 1;
        } else {
            result.first_attempts += 1;
            if code == 0 {
                result.first_attempt_success += 1;
            }
        }
        previous.insert(key, (event.unit, code != 0, at));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
