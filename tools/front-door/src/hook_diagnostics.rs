//! Durable diagnostics for hooks that fail inside Provenant. A failing hook
//! abstains so it cannot refuse a tool or a prompt, but the cause and duration
//! must survive after the harness has moved on.
use provenant_core::{Clock, WallTime};
use provenant_os::clock::SystemClock;
use provenant_records::diagnostic::safe_text;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

static CLOCK: SystemClock = SystemClock;
const RECORD_NAME: &str = "failures.jsonl";

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
        let Some(state) = self.state else {
            return;
        };
        let directory = state.join("hooks").join("diagnostics");
        if std::fs::create_dir_all(&directory).is_err() {
            return;
        }
        let at: Option<WallTime> = CLOCK.wall_now().ok();
        let line = serde_json::json!({
            "at": at.map(|at| at.to_string()),
            "action": safe_text(self.action),
            "event": self.event.map(safe_text),
            "cause": safe_text(cause),
            "duration_ms": u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
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

#[allow(clippy::disallowed_methods)] // next-batch wave 2 removes
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
}
