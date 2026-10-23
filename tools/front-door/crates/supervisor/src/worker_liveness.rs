//! Non-mutating worker liveness policy. Silence is advisory, never stop authority.
//!
//! OS observations (census, resolved cwd, repository ID and session-log facts)
//! are supplied as values. This module does not inspect a live writer's worktree.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const STALL_WINDOW_SECONDS: f64 = 60.0;
pub const TERMINAL_CLASSIFICATIONS: [&str; 5] =
    ["blocked", "question", "unavailable", "failed", "complete"];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExit {
    pub pid: i64,
    pub exit_status: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalReport {
    pub classification: String,
    pub pid: i64,
    pub observed_exit: bool,
    pub exit_status: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    pub pid: i64,
    pub ppid: i64,
    pub elapsed: String,
    pub cpu: String,
    pub command: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Worker {
    pub pid: i64,
    pub elapsed: String,
    pub elapsed_seconds: f64,
    pub cpu: String,
    pub cpu_seconds: f64,
    pub cwd: String,
    pub session_log: String,
    pub session_log_mtime: String,
    pub session_log_age_seconds: Option<f64>,
    pub output_size: Option<i64>,
    pub worktree: String,
    pub stalled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum LivenessError {
    #[error("unknown terminal classification: {0}")]
    Classification(String),
    #[error("cannot {action} while owned worker PID {pid} remains live")]
    LiveWriter { pid: i64, action: String },
    #[error("terminal report requires observed process exit for the owned PID")]
    MissingExit,
    #[error("poll budget must be positive")]
    PollBudget,
    #[error("could not convert string to float: {0}")]
    Duration(String),
    #[error("cannot inspection without an observed PID set")]
    MissingPidSet,
    #[error("not a git repository: {0}")]
    Repository(String),
}

pub fn require_worker_quiescent(
    pid: i64,
    process_live: bool,
    action: &str,
) -> Result<(), LivenessError> {
    if process_live {
        return Err(LivenessError::LiveWriter {
            pid,
            action: action.into(),
        });
    }
    Ok(())
}
pub fn terminal_report(
    classification: &str,
    pid: i64,
    process_live: bool,
    exit: Option<WorkerExit>,
) -> Result<TerminalReport, LivenessError> {
    if !TERMINAL_CLASSIFICATIONS.contains(&classification) {
        return Err(LivenessError::Classification(classification.into()));
    }
    require_worker_quiescent(pid, process_live, "terminal-report")?;
    let exit = exit
        .filter(|exit| exit.pid == pid)
        .ok_or(LivenessError::MissingExit)?;
    Ok(TerminalReport {
        classification: classification.into(),
        pid,
        observed_exit: true,
        exit_status: exit.exit_status,
    })
}
pub fn output_grew(previous: i64, current: i64) -> bool {
    current > previous
}
pub fn liveness_signal(process_live: bool, previous: i64, current: i64) -> &'static str {
    if !process_live {
        "exited"
    } else if output_grew(previous, current) {
        "progress"
    } else {
        "live-no-growth"
    }
}
pub fn rearm_poll_deadline(
    now: f64,
    budget_seconds: f64,
    process_live: bool,
) -> Result<Option<f64>, LivenessError> {
    if budget_seconds <= 0.0 {
        return Err(LivenessError::PollBudget);
    }
    Ok(process_live.then_some(now + budget_seconds))
}
pub fn parse_duration(value: &str) -> f64 {
    let Some(parts) = crate::pending::python_strip(value)
        .split(':')
        .map(crate::pending::python_float)
        .collect::<Option<Vec<_>>>()
    else {
        return 0.0;
    };
    match parts.as_slice() {
        [hours, minutes, seconds] => hours * 3600.0 + minutes * 60.0 + seconds,
        [minutes, seconds] => minutes * 60.0 + seconds,
        [seconds] => *seconds,
        _ => 0.0,
    }
}
pub fn parse_elapsed(value: &str) -> Result<f64, LivenessError> {
    let value = crate::pending::python_strip(value);
    let (days, value) = match value.split_once('-') {
        Some((days, rest)) => (
            crate::pending::python_float(days)
                .ok_or_else(|| LivenessError::Duration(crate::pending::python_repr(days)))?
                * 86400.0,
            rest,
        ),
        None => (0.0, value),
    };
    Ok(days + parse_duration(value))
}
pub fn command_cwd(command: &str) -> Option<String> {
    let arguments = crate::pending::shell_split(command)?;
    for (index, argument) in arguments.iter().enumerate() {
        if matches!(argument.as_str(), "--cd" | "-C") {
            if let Some(next) = arguments.get(index + 1) {
                return Some(next.clone());
            }
        }
        if let Some(value) = argument.strip_prefix("--cd=") {
            return Some(value.into());
        }
        if let Some(value) = argument.strip_prefix("-C").filter(|s| !s.is_empty()) {
            return Some(value.into());
        }
    }
    None
}
/// Preserve census order and remove candidate ancestors, with cycle fencing.
pub fn live_processes(processes: &[Process]) -> Vec<Process> {
    let by_pid: BTreeMap<_, _> = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect();
    let candidates: Vec<_> = processes
        .iter()
        .filter(|process| {
            process.command.contains("codex exec")
                && !process.command.contains("worker_liveness.py")
        })
        .collect();
    let candidate_pids: BTreeSet<_> = candidates.iter().map(|process| process.pid).collect();
    let mut wrappers = BTreeSet::new();
    for process in &candidates {
        let mut ancestor = process.ppid;
        let mut seen = BTreeSet::new();
        while let Some(parent) = by_pid.get(&ancestor).filter(|_| seen.insert(ancestor)) {
            if candidate_pids.contains(&ancestor) {
                wrappers.insert(ancestor);
            }
            ancestor = parent.ppid;
        }
    }
    candidates
        .into_iter()
        .filter(|process| !wrappers.contains(&process.pid))
        .cloned()
        .collect()
}
/// Fence the OS inspection *before* any git status operation is attempted.
pub fn require_inspection_quiescent(
    owned_pid: Option<i64>,
    live_pids: Option<&BTreeSet<i64>>,
) -> Result<(), LivenessError> {
    if let Some(pid) = owned_pid {
        let live = live_pids
            .ok_or(LivenessError::MissingPidSet)?
            .contains(&pid);
        require_worker_quiescent(pid, live, "inspection")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
