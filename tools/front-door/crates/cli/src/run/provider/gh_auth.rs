//! Host-only GitHub login lookup. Credential bytes never enter setup files,
//! prompts, receipts or diagnostic output.
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use std::{ffi::OsString, path::Path, time::Duration};

pub(super) fn inject(
    executor: &dyn Executor,
    gh: &Path,
    home: &Path,
    host_paths: &impl Fn(&str) -> Option<OsString>,
    cancel: &provenant_core::CancelToken,
    environment: &mut Vec<(OsString, OsString)>,
) {
    let config = config_directory(home, host_paths);
    if !environment.iter().any(|(key, _)| key == "GH_CONFIG_DIR") {
        environment.push(("GH_CONFIG_DIR".into(), config.as_os_str().to_owned()));
    }
    if let Some(token) = token(
        executor,
        gh,
        home,
        &config,
        cancel,
        std::env::var_os("GH_TOKEN"),
    ) {
        environment.retain(|(key, _)| key != "GH_TOKEN");
        environment.push(("GH_TOKEN".into(), token));
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
) -> Option<OsString> {
    // `gh auth token` reads the persisted login and does not return GH_TOKEN.
    // Honour the host token first, with the same bounds as captured output.
    if let Some(value) = host_token
        && valid_host_token(&value)
    {
        return Some(value);
    }
    let argv = [gh.as_os_str().to_owned(), "auth".into(), "token".into()];
    let mut options = ExecOptions::new(&argv, home, Duration::from_secs(10));
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
            Some("fixture-secret".into())
        );
        assert_eq!(
            token(
                &Login(false),
                Path::new("/fixture/gh"),
                Path::new("/fixture/home"),
                Path::new("/fixture/relocated-gh"),
                &cancel,
                None
            ),
            None
        );
    }
}
