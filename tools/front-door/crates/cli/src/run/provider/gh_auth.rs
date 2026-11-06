//! Host-only GitHub login lookup. Credential bytes never enter setup files,
//! prompts, receipts or diagnostic output.
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use std::{ffi::OsString, path::Path, time::Duration};

pub(super) fn inject(
    executor: &dyn Executor,
    gh: &Path,
    home: &Path,
    cancel: &provenant_core::CancelToken,
    environment: &mut Vec<(OsString, OsString)>,
) {
    if let Some(token) = token(executor, gh, home, cancel) {
        environment.retain(|(key, _)| key != "GH_TOKEN");
        environment.push(("GH_TOKEN".into(), token));
    }
}

fn token(
    executor: &dyn Executor,
    gh: &Path,
    home: &Path,
    cancel: &provenant_core::CancelToken,
) -> Option<OsString> {
    let argv = [gh.as_os_str().to_owned(), "auth".into(), "token".into()];
    let mut options = ExecOptions::new(&argv, home, Duration::from_secs(10));
    options.cancel = Some(cancel);
    options.process_group = true;
    options.cleanup_process_group = true;
    options.stdout = StreamPolicy::Capture { limit: 16 * 1024 };
    options.stderr = StreamPolicy::Discard;
    // Inherit the host's ordinary gh configuration and keychain session.
    // A missing login does not prevent a lane doing other read-only work.
    let result = executor.execute(options).ok()?;
    if result.termination != Termination::Exit(0)
        || result.cancelled
        || result.timed_out
        || result.stdout.truncated
        || result.stdout.read_failed
    {
        return None;
    }
    let text = std::str::from_utf8(&result.stdout.bytes).ok()?.trim();
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic())).then(|| text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome};

    #[test]
    fn host_gh_login_is_captured_only_in_memory_and_failures_are_ignored() {
        struct Login(bool);
        impl Executor for Login {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert_eq!(
                    options.argv,
                    ["/fixture/gh", "auth", "token"].map(OsString::from)
                );
                assert!(
                    options.env.is_none(),
                    "host login discovery must be inherited"
                );
                assert!(matches!(
                    options.stdout,
                    StreamPolicy::Capture { limit: 16384 }
                ));
                assert!(matches!(options.stderr, StreamPolicy::Discard));
                assert!(options.stdin.is_none());
                Ok(ExecOutcome {
                    termination: Termination::Exit(if self.0 { 0 } else { 1 }),
                    stdout: CapturedStream {
                        bytes: b"fixture-secret\n".to_vec(),
                        ..Default::default()
                    },
                    stderr: CapturedStream::default(),
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 1,
                })
            }
        }
        let cancel = provenant_core::CancelToken::new();
        assert_eq!(
            token(
                &Login(true),
                Path::new("/fixture/gh"),
                Path::new("/fixture/home"),
                &cancel
            ),
            Some("fixture-secret".into())
        );
        assert_eq!(
            token(
                &Login(false),
                Path::new("/fixture/gh"),
                Path::new("/fixture/home"),
                &cancel
            ),
            None
        );
    }
}
