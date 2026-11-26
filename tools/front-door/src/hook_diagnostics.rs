//! Durable diagnostics for hooks that fail inside Provenant. A failing hook
//! abstains so it cannot refuse a tool or a prompt, but the cause and duration
//! must survive after the harness has moved on.
use provenant_core::{Clock, WallTime};
use provenant_os::clock::SystemClock;
use provenant_records::diagnostic::safe_text;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

static CLOCK: SystemClock = SystemClock;
const RECORD_NAME: &str = "failures.jsonl";
/// How far back doctor looks for hook overruns.
pub(crate) const DOCTOR_WINDOW: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// One overrun, as reported by doctor. The diagnostic file keeps the action,
/// cause, source and duration; this view adds nothing it cannot read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HookOverrun {
    pub at: Option<String>,
    pub action: String,
    pub event: Option<String>,
    pub cause: String,
    pub source: String,
    pub duration_ms: u64,
    pub budget_ms: u64,
    pub overrun_ms: u64,
}

/// Read hook overruns seen inside `window`. A partial or unreadable line is
/// skipped: diagnostics are advisory and never fail the report that shows them.
pub(crate) fn recent_overruns(state: &Path, window: std::time::Duration) -> Vec<HookOverrun> {
    let Some(at) = CLOCK.wall_now().ok() else {
        return Vec::new();
    };
    let cutoff = at
        .as_timestamp()
        .as_second()
        .saturating_sub(window.as_secs() as i64);
    let bytes = std::fs::read(state.join("hooks").join("diagnostics").join(RECORD_NAME))
        .unwrap_or_default();
    let mut rows = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let Some(overrun_ms) = value.get("overrun_ms").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        let seen = value
            .get("at")
            .and_then(serde_json::Value::as_str)
            .and_then(|text| jiff::Timestamp::parse(text).ok())
            .is_some_and(|seen| seen.as_second() >= cutoff);
        if !seen {
            continue;
        }
        let Some(action) = value.get("action").and_then(serde_json::Value::as_str) else {
            continue;
        };
        rows.push(HookOverrun {
            at: value
                .get("at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            action: action.to_owned(),
            event: value
                .get("event")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            cause: value
                .get("cause")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            source: value
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            duration_ms: value
                .get("duration_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(overrun_ms),
            budget_ms: value
                .get("budget_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
            overrun_ms,
        });
    }
    rows
}

/// The hook invocation's own edge, handed to the paths that can fail.
pub(crate) struct Failure<'a> {
    state: Option<&'a Path>,
    action: &'a str,
    event: Option<&'a str>,
    started: Instant,
}

impl<'a> Failure<'a> {
    pub(crate) fn new(
        state: Option<&'a Path>,
        action: &'a str,
        event: Option<&'a str>,
        started: Instant,
    ) -> Self {
        Self {
            state,
            action,
            event,
            started,
        }
    }

    /// Record the cause under the hook's state root, then tell the operator.
    /// A record that cannot be written never changes the hook's abstention.
    pub(crate) fn note(&self, cause: &str) -> u8 {
        self.record(cause);
        diagnostic(cause);
        0
    }

    fn record(&self, cause: &str) {
        let line = serde_json::json!({
            "at": self.at(),
            "action": safe_text(self.action),
            "event": self.event.map(safe_text),
            "cause": safe_text(cause),
            "duration_ms": self.duration_ms(),
        });
        self.write_record(line);
    }

    /// Persist a local-budget overrun with the numbers doctor reports. The
    /// source names the layer that set `hooks.timeout_secs`.
    pub(crate) fn record_overrun(
        &self,
        cause: &str,
        budget: std::time::Duration,
        overrun: std::time::Duration,
        source: &str,
    ) {
        let mut line = serde_json::json!({
            "at": self.at(),
            "action": safe_text(self.action),
            "event": self.event.map(safe_text),
            "cause": safe_text(cause),
            "duration_ms": self.duration_ms(),
            "budget_ms": u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
            "overrun_ms": u64::try_from(overrun.as_millis()).unwrap_or(u64::MAX),
            "source": safe_text(source),
        });
        if let Some(object) = line.as_object_mut() {
            object.retain(|_, value| !value.is_null());
        }
        self.write_record(line);
    }

    fn at(&self) -> Option<String> {
        CLOCK.wall_now().ok().map(|at| at.to_string())
    }

    fn duration_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn write_record(&self, line: serde_json::Value) {
        let Some(state) = self.state else {
            return;
        };
        let directory = state.join("hooks").join("diagnostics");
        if std::fs::create_dir_all(&directory).is_err() {
            return;
        }
        let mut bytes = match serde_json::to_vec(&line) {
            Ok(bytes) => bytes,
            Err(_) => return,
        };
        bytes.push(b'\n');
        let path: PathBuf = directory.join(RECORD_NAME);
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        let _ = file.write_all(&bytes);
    }
}

// Hook diagnostics are the hook wire's stderr protocol, not warning records.
#[allow(clippy::disallowed_methods)]
pub(crate) fn diagnostic(message: &str) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "{}",
        message.chars().take(5000).collect::<String>()
    );
}

#[cfg(test)]
mod tests {
    // The worktree driver includes this module without a unit-test harness.
    #[allow(unused_imports)]
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods)] // The hook edge samples monotonic time once.
    fn failure_record_keeps_cause_and_duration_under_the_state_root() {
        let temp = tempfile::tempdir().unwrap();
        let failure = Failure::new(
            Some(temp.path()),
            "post-tool-use",
            Some("PostToolUse"),
            Instant::now(),
        );
        assert_eq!(failure.note("hook input unreadable"), 0);
        let bytes = std::fs::read(temp.path().join("hooks/diagnostics/failures.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(line["action"], "post-tool-use");
        assert_eq!(line["event"], "PostToolUse");
        assert_eq!(line["cause"], "hook input unreadable");
        assert!(line["at"].is_string());
        assert!(line["duration_ms"].is_u64());
    }

    #[test]
    fn recent_overruns_are_read_from_the_durable_hook_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("hooks/diagnostics");
        std::fs::create_dir_all(&directory).unwrap();
        let at = provenant_os::clock::SystemClock::new().wall_now().unwrap();
        let rows = [
            serde_json::json!({
                "at": at.to_string(), "action": "chair-stop", "event": "Stop",
                "cause": "hook deadline exceeded", "duration_ms": 6100,
                "budget_ms": 6000, "overrun_ms": 100, "source": "config.toml (domains.hooks.timeout_secs)",
            }),
            serde_json::json!({"at": at.to_string(), "action": "chair-start", "cause": "other"}),
            serde_json::json!({"at": "1999-01-01T00:00:00Z", "action": "old", "overrun_ms": 9}),
        ];
        let text = rows
            .iter()
            .map(|row| row.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(directory.join(RECORD_NAME), text).unwrap();
        let found = recent_overruns(temp.path(), DOCTOR_WINDOW);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].action, "chair-stop");
        assert_eq!(found[0].overrun_ms, 100);
        assert_eq!(found[0].source, "config.toml (domains.hooks.timeout_secs)");
    }
}
