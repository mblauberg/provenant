//! Advisory project setup at the new-worktree boundary, under writer custody.
use super::WorktreeContext;
use provenant_core::{CancelToken, WaitBudget};
use provenant_os::exec::{ExecOptions, StreamPolicy, Termination};
use std::{ffi::OsString, io::Write, path::Path, time::Duration};

pub(super) fn run(context: &WorktreeContext<'_>, path: &Path, cancel: &CancelToken) -> Vec<String> {
    let result = (|| -> Result<(), String> {
        let project_state = context
            .project_state
            .current_path()
            .map_err(|e| e.to_string())?;
        let state = project_state
            .parent()
            .and_then(Path::parent)
            .ok_or("could not locate worktree setup configuration")?;
        let settings = provenant_config::WorktreeSettings::read(state)
            .map_err(|e| format!("could not read worktree setup configuration: {e}"))?
            .for_project(context.binding.project_id());
        if settings.setup.is_empty() {
            return Ok(());
        }
        let budget = WaitBudget::cli(context.clock.mono_now(), settings.timeout_seconds)
            .map_err(|e| e.to_string())?;
        for command in &settings.setup {
            let remaining =
                Duration::from_micros(budget.remaining(context.clock.mono_now()).as_micros());
            if remaining.is_zero() {
                return Err("worktree setup timed out".into());
            }
            let argv = [
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(command),
            ];
            let mut options = ExecOptions::new(&argv, path, remaining);
            // Project tools need the host environment, unlike scrubbed Git reads.
            options.stdout = StreamPolicy::Tail { limit: 4096 };
            options.stderr = StreamPolicy::Tail { limit: 4096 };
            options.process_group = true;
            options.cleanup_process_group = true;
            options.group_watchdog = true;
            options.cancel = Some(cancel);
            let outcome = context
                .executor
                .execute(options)
                .map_err(|e| format!("worktree setup could not start: {e}"))?;
            if outcome.termination != Termination::Exit(0) || outcome.timed_out || outcome.cancelled
            {
                let reason = if outcome.timed_out {
                    "timed out".to_owned()
                } else if outcome.cancelled {
                    "was cancelled".to_owned()
                } else {
                    format!("failed ({:?})", outcome.termination)
                };
                return Err(format!(
                    "worktree setup {reason}\nstdout tail:\n{}\nstderr tail:\n{}",
                    String::from_utf8_lossy(&outcome.stdout.bytes),
                    String::from_utf8_lossy(&outcome.stderr.bytes)
                ));
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => Vec::new(),
        Err(error) => {
            let warning = format!("{error}; worktree remains usable at {}", path.display());
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
            vec![warning]
        }
    }
}
