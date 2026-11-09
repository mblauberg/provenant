//! Host-only GitHub login lookup. Credential bytes never enter setup files,
//! prompts, receipts or diagnostic output.
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use std::{ffi::OsString, path::Path, time::Duration};

/// Attempts for one lookup. Heavy host load can stall `gh` past a single
/// bound; a stall is retried rather than dropping access for the lane.
const ATTEMPTS: usize = 3;
const ATTEMPT_LIMIT: Duration = Duration::from_secs(20);

/// Why a lookup produced no token. Never carries credential bytes.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// Worth another attempt: spawn failure, timeout, signal or read failure.
    Transient(String),
    /// Retrying cannot help: no stored login, unusable output or cancellation.
    Final(String),
}

/// Inject the host login into `environment`. A token found earlier in this
/// preparation is reused through `cache`; a failed lookup is not cached, so a
/// later route retries it. Returns a warning naming the cause when no token
/// could be resolved.
pub(super) fn inject(
    executor: &dyn Executor,
    gh: &Path,
    home: &Path,
    host_paths: &impl Fn(&str) -> Option<OsString>,
    cancel: &provenant_core::CancelToken,
    cache: &mut Option<OsString>,
    environment: &mut Vec<(OsString, OsString)>,
) -> Option<String> {
    let config = config_directory(home, host_paths);
    if !environment.iter().any(|(key, _)| key == "GH_CONFIG_DIR") {
        environment.push(("GH_CONFIG_DIR".into(), config.as_os_str().to_owned()));
    }
    let resolved = match cache.clone() {
        Some(token) => Ok(token),
        None => token(
            executor,
            gh,
            home,
            &config,
            cancel,
            std::env::var_os("GH_TOKEN"),
        ),
    };
    match resolved {
        Ok(token) => {
            *cache = Some(token.clone());
            environment.retain(|(key, _)| key != "GH_TOKEN");
            environment.push(("GH_TOKEN".into(), token));
            None
        }
        Err(cause) => Some(format!(
            "GitHub login lookup failed ({cause}); this lane starts without a GitHub token, so git push and gh may be unauthenticated"
        )),
    }
}

/// Resolve only host-sealed paths. A guardian has cleared its ambient
/// environment; the lane's private HOME cannot select the host login store.
fn config_directory(
    home: &Path,
    host_paths: &impl Fn(&str) -> Option<OsString>,
) -> std::path::PathBuf {
    host_paths("GH_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            host_paths("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join("gh"))
        })
        .unwrap_or_else(|| home.join(".config/gh"))
}

/// Codex removes *TOKEN* by default. Override that filter only within the
/// complete, already sanitised launch environment's explicit name list.
pub(super) fn codex_shell_environment(
    environment: &[(OsString, OsString)],
    argv: &mut Vec<OsString>,
) {
    let names = environment
        .iter()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let Ok(names) = serde_json::to_string(&names) else {
        return;
    };
    let config = [
        OsString::from("-c"),
        OsString::from("shell_environment_policy.ignore_default_excludes=true"),
        OsString::from("-c"),
        OsString::from(format!("shell_environment_policy.include_only={names}")),
    ];
    let index = argv
        .len()
        .saturating_sub(usize::from(argv.last().is_some_and(|arg| arg == "-")));
    argv.splice(index..index, config);
}

pub(super) fn valid_host_token(value: &std::ffi::OsStr) -> bool {
    value.to_str().is_some_and(|text| {
        !text.is_empty() && text.len() <= 16 * 1024 && text.bytes().all(|b| b.is_ascii_graphic())
    })
}

fn token(
    executor: &dyn Executor,
    gh: &Path,
    home: &Path,
    config: &Path,
    cancel: &provenant_core::CancelToken,
    host_token: Option<OsString>,
) -> Result<OsString, String> {
    // `gh auth token` reads the persisted login and does not return GH_TOKEN.
    // Honour the host token first, with the same bounds as captured output.
    if let Some(value) = host_token
        && valid_host_token(&value)
    {
        return Ok(value);
    }
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        match attempt_token(executor, gh, home, config, cancel) {
            Ok(token) => return Ok(token),
            Err(Failure::Final(cause)) => return Err(cause),
            Err(Failure::Transient(cause)) => {
                last = format!("{cause}; attempt {attempt} of {ATTEMPTS}")
            }
        }
    }
    Err(last)
}

fn attempt_token(
    executor: &dyn Executor,
    gh: &Path,
    home: &Path,
    config: &Path,
    cancel: &provenant_core::CancelToken,
) -> Result<OsString, Failure> {
    let argv = [gh.as_os_str().to_owned(), "auth".into(), "token".into()];
    let mut options = ExecOptions::new(&argv, home, ATTEMPT_LIMIT);
    let host_environment = [
        ("HOME".into(), home.as_os_str().to_owned()),
        ("GH_CONFIG_DIR".into(), config.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GH_PROMPT_DISABLED".into(), "1".into()),
    ];
    options.env = Some(&host_environment);
    options.cancel = Some(cancel);
    options.process_group = true;
    options.cleanup_process_group = true;
    options.stdout = StreamPolicy::Capture { limit: 16 * 1024 };
    options.stderr = StreamPolicy::Discard;
    // The selected store and host home survive a cleared guardian environment.
    // A missing login does not prevent other ordinary lane work.
    let result = executor
        .execute(options)
        .map_err(|error| Failure::Transient(format!("could not run gh: {error}")))?;
    if result.cancelled {
        return Err(Failure::Final("lookup cancelled".into()));
    }
    if result.timed_out {
        return Err(Failure::Transient(format!(
            "gh auth token timed out after {}s",
            ATTEMPT_LIMIT.as_secs()
        )));
    }
    match result.termination {
        Termination::Exit(0) => {}
        Termination::Exit(code) => {
            return Err(Failure::Final(format!(
                "gh auth token exited with status {code}; no stored login for the selected configuration"
            )));
        }
        Termination::Signal(signal) => {
            return Err(Failure::Transient(format!(
                "gh auth token was killed by signal {signal}"
            )));
        }
    }
    if result.stdout.truncated || result.stdout.read_failed {
        return Err(Failure::Transient(
            "gh auth token output was unreadable".into(),
        ));
    }
    let text = std::str::from_utf8(&result.stdout.bytes)
        .map_err(|_| Failure::Final("gh auth token output was not text".into()))?
        .trim();
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(text.into())
    } else {
        Err(Failure::Final(
            "gh auth token returned no usable token".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome};

    #[test]
    fn codex_shell_policy_preserves_only_the_final_explicit_environment() {
        let environment = vec![
            ("PATH".into(), "/bin".into()),
            ("GH_TOKEN".into(), "fixture-secret".into()),
            ("PROVENANT_LANE_HEAD".into(), "fixture-head".into()),
        ];
        let mut argv = vec!["codex".into(), "exec".into(), "-".into()];
        codex_shell_environment(&environment, &mut argv);
        assert_eq!(argv.last().unwrap(), "-");
        assert!(
            argv.iter()
                .any(|arg| arg == "shell_environment_policy.ignore_default_excludes=true")
        );
        let names = argv
            .iter()
            .find_map(|arg| {
                arg.to_str()?
                    .strip_prefix("shell_environment_policy.include_only=")
            })
            .unwrap();
        let names: Vec<String> = serde_json::from_str(names).unwrap();
        assert_eq!(names, ["PATH", "GH_TOKEN", "PROVENANT_LANE_HEAD"]);
        assert!(
            !argv
                .iter()
                .any(|arg| arg.to_string_lossy().contains("fixture-secret"))
        );
    }

    #[test]
    fn sealed_gh_paths_select_the_host_store_after_environment_clearing() {
        let home = Path::new("/fixture/home");
        let host = |key: &str| match key {
            "GH_CONFIG_DIR" => Some("/fixture/relocated-gh".into()),
            "XDG_CONFIG_HOME" => Some("/fixture/xdg".into()),
            _ => None,
        };
        assert_eq!(
            config_directory(home, &host),
            Path::new("/fixture/relocated-gh")
        );
        assert_eq!(
            config_directory(home, &|key| (key == "XDG_CONFIG_HOME")
                .then(|| "/fixture/xdg".into())),
            Path::new("/fixture/xdg/gh")
        );
        assert_eq!(config_directory(home, &|_| None), home.join(".config/gh"));
    }

    #[test]
    fn host_gh_login_is_captured_only_in_memory_and_failures_are_ignored() {
        struct Login(bool);
        impl Executor for Login {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert_eq!(
                    options.argv,
                    ["/fixture/gh", "auth", "token"].map(OsString::from)
                );
                let env = options.env.expect("host login paths must be explicit");
                assert!(env.contains(&("HOME".into(), "/fixture/home".into())));
                assert!(env.contains(&("GH_CONFIG_DIR".into(), "/fixture/relocated-gh".into())));
                assert!(!env.iter().any(|(name, _)| name == "GH_TOKEN"));
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
                Path::new("/fixture/relocated-gh"),
                &cancel,
                None
            ),
            Ok("fixture-secret".into())
        );
        assert!(
            token(
                &Login(false),
                Path::new("/fixture/gh"),
                Path::new("/fixture/home"),
                Path::new("/fixture/relocated-gh"),
                &cancel,
                None
            )
            .unwrap_err()
            .contains("status 1")
        );
    }

    #[test]
    fn transient_failures_are_retried_and_warn_without_the_credential() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Flaky(AtomicUsize, usize);
        impl Executor for Flaky {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let call = self.0.fetch_add(1, Ordering::SeqCst);
                let ok = call >= self.1;
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: CapturedStream {
                        bytes: if ok {
                            b"fixture-secret\n".to_vec()
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    },
                    stderr: CapturedStream::default(),
                    timed_out: !ok,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 1,
                })
            }
        }
        let cancel = provenant_core::CancelToken::new();
        let lookup = |executor: &Flaky| {
            token(
                executor,
                Path::new("/fixture/gh"),
                Path::new("/fixture/home"),
                Path::new("/fixture/config"),
                &cancel,
                None,
            )
        };
        let executor = Flaky(AtomicUsize::new(0), 2);
        assert_eq!(lookup(&executor), Ok("fixture-secret".into()));
        assert_eq!(executor.0.load(Ordering::SeqCst), 3);
        let executor = Flaky(AtomicUsize::new(0), 99);
        let cause = lookup(&executor).unwrap_err();
        assert!(cause.contains("timed out") && cause.contains("attempt 3 of 3"));
        assert!(!cause.contains("fixture-secret"));
        // A cached token needs no lookup and a failure leaves the cache empty.
        let mut env = Vec::new();
        let mut cache = Some(OsString::from("cached"));
        let warning = inject(
            &executor,
            Path::new("/fixture/gh"),
            Path::new("/fixture/home"),
            &|_| None,
            &cancel,
            &mut cache,
            &mut env,
        );
        assert_eq!(warning, None);
        assert_eq!(executor.0.load(Ordering::SeqCst), 3);
        assert!(env.iter().any(|(k, v)| k == "GH_TOKEN" && v == "cached"));
    }
}
