//! Local demand counters. The command edge owns these effects, not the reducer.
use provenant_cli::{Globals, Stats};
use provenant_core::{ProjectId, Roots};
use provenant_records::{CommandResult, Outcome, Trace};
use provenant_supervisor::fs::{Directory, LockMode};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{Read, Write},
};

pub const EVENT_KINDS: &[&str] = &["cli.verb", "mcp.tool"];
const CAP: u64 = 5_000_000;
const LOG: &str = "usage.jsonl";
const BACKUP: &str = "usage.jsonl.1";
const LOCK: &str = "usage.lock";
const LINE_CAP: usize = 512;

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
    /// Only the project, and only when attribution is on: counting records no
    /// other identity and never creates state to learn one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace: Option<Trace>,
}

fn project_trace(project_id: Option<ProjectId>) -> Option<Trace> {
    project_id.map(|project| Trace {
        project: Some(project),
        ..Trace::default()
    })
}

pub fn selected_roots(globals: &Globals) -> Option<Roots> {
    // No product installation, Git probe or provider access is needed to count.
    let project = match &globals.project {
        Some(project) => std::fs::canonicalize(project).ok()?,
        None => std::env::current_dir().ok()?,
    };
    let assets = crate::roots::product_root().unwrap_or_else(|_| project.clone());
    crate::roots::roots(globals, &assets, &project).ok()
}

pub fn attribution_enabled(roots: &Roots) -> bool {
    use provenant_supervisor::fs::LocalFs;
    provenant_config::read_with_domains(roots, &LocalFs, &provenant_cli::domains::SECTIONS)
        .ok()
        .and_then(|config| config.knobs.get("usage.project_attribution").cloned())
        .and_then(|knob| knob.value.as_bool())
        .unwrap_or(false)
}

pub fn project_id(roots: &Roots, globals: &Globals) -> Option<ProjectId> {
    if !attribution_enabled(roots) {
        return None;
    }
    let project = match &globals.project {
        Some(project) => project.clone(),
        None => std::env::current_dir().ok()?,
    };
    use provenant_supervisor::fs::LocalFs;
    use std::{sync::Arc, time::Duration};
    let git = crate::roots::git_binary()?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(
        Arc::new(LocalFs),
        std::fs::canonicalize(git).ok()?,
    )
    .ok()?;
    let executor = provenant_supervisor::exec::SystemExecutor::new(1).ok()?;
    let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(1)).ok()?;
    let location = runtime
        .project_location(
            &executor,
            &project,
            budget,
            &provenant_core::CancelToken::new(),
        )
        .ok()?
        .location;
    provenant_config::lookup_project(roots, &location, &LocalFs)
        .ok()?
        .map(|binding| binding.project_id())
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
    verb: &str,
    exit_code: u8,
    duration_ms: u64,
    project_id: Option<ProjectId>,
) {
    let Some(timestamp) = timestamp() else { return };
    let _ = append(
        roots,
        Event {
            kind: EVENT_KINDS[0].into(),
            unit: verb.into(),
            exit_code: Some(exit_code),
            outcome: None,
            duration_ms,
            timestamp,
            trace: project_trace(project_id),
        },
        true,
    );
}
pub fn record_tool(
    roots: &Roots,
    tool: &str,
    outcome: &str,
    duration_ms: u64,
    project_id: Option<ProjectId>,
) {
    let Some(timestamp) = timestamp() else { return };
    let _ = append(
        roots,
        Event {
            kind: EVENT_KINDS[1].into(),
            unit: tool.into(),
            exit_code: None,
            outcome: Some(outcome.into()),
            duration_ms,
            timestamp,
            trace: project_trace(project_id),
        },
        false,
    );
}
fn timestamp() -> Option<String> {
    // SystemClock's wall time is the shared timestamp boundary.
    use provenant_core::Clock;
    provenant_supervisor::clock::SystemClock
        .wall_now()
        .ok()
        .map(|time| time.to_string())
}

#[derive(Serialize)]
struct Unit {
    kind: String,
    unit: String,
    calls: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<ProjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'static str>,
}
#[derive(Serialize)]
struct Report {
    since: Option<String>,
    until: Option<String>,
    units: Vec<Unit>,
}
fn inventory() -> BTreeMap<(String, String, Option<ProjectId>), u64> {
    provenant_cli::usage_verbs()
        .into_iter()
        .map(|verb| ((EVENT_KINDS[0].into(), verb, None), 0))
        .chain(
            provenant_fabric_mcp::tools::catalogue()
                .iter()
                .filter_map(|tool| {
                    tool["name"]
                        .as_str()
                        .map(|name| ((EVENT_KINDS[1].into(), name.into(), None), 0))
                }),
        )
        .collect()
}
fn report(roots: &Roots, stats: &Stats) -> Result<Report, Box<dyn std::error::Error>> {
    let mut units = inventory();
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
            let mut bytes = Vec::new();
            // A writer can cross the cap while rotation is busy. Bound the read,
            // rather than failing the report when the file grows.
            (&mut file).take(CAP).read_to_end(&mut bytes)?;
            // Ignore torn or malformed records; only complete bounded lines count.
            for line in bytes.split_inclusive(|byte| *byte == b'\n') {
                if line.len() > LINE_CAP || !line.ends_with(b"\n") {
                    continue;
                }
                let Ok(event) = serde_json::from_slice::<Event>(line) else {
                    continue;
                };
                if !EVENT_KINDS.contains(&event.kind.as_str()) || event.unit.len() > 128 {
                    continue;
                }
                let Ok(time) = event.timestamp.parse::<provenant_core::WallTime>() else {
                    continue;
                };
                let time = time.as_timestamp();
                if stats.since.is_some_and(|since| time < since)
                    || stats.until.is_some_and(|until| time >= until)
                {
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
        since: stats.since.map(|s| s.to_string()),
        until: stats.until.map(|s| s.to_string()),
        units,
    })
}
pub fn run(stats: &Stats, globals: &Globals) -> u8 {
    let Some(roots) = selected_roots(globals) else {
        return crate::fail(
            "telemetry stats",
            Outcome::Error,
            "ROOT",
            "cannot resolve usage state root",
            globals,
        );
    };
    match report(&roots, stats) {
        Ok(data) => {
            let text = data
                .units
                .iter()
                .map(|unit| {
                    let project = unit
                        .project_id
                        .map(|id| format!("  project={id}"))
                        .unwrap_or_default();
                    format!("{:>8}  {}  {}{project}\n", unit.calls, unit.kind, unit.unit)
                })
                .collect::<String>();
            crate::emit(
                &CommandResult::new("telemetry stats", Outcome::Ok, data),
                globals,
                &text,
                true,
            )
        }
        Err(error) => crate::fail(
            "telemetry stats",
            Outcome::Error,
            "USAGE",
            &error.to_string(),
            globals,
        ),
    }
}

#[cfg(test)]
mod tests;
