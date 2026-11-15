#[test]
fn provider_preparation_fallback_classifies_each_error_variant() {
    use provenant_providers::ProviderError;
    use provenant_records::diagnostic::SettingSource;
    use provenant_routing::FallbackOutcome;

    let cases = [
        (ProviderError::LoginRequired, FallbackOutcome::AuthRequired),
        (
            ProviderError::CredentialNotFound,
            FallbackOutcome::AuthRequired,
        ),
        (
            ProviderError::CredentialExpired,
            FallbackOutcome::AuthRequired,
        ),
        (ProviderError::CredentialUnavailable, FallbackOutcome::Other),
        (ProviderError::RefreshDeferred, FallbackOutcome::Other),
        (
            ProviderError::CredentialReadFailed("fixture".into()),
            FallbackOutcome::Other,
        ),
        (ProviderError::IdentityMismatch, FallbackOutcome::Other),
        (ProviderError::UnknownIdentity, FallbackOutcome::Other),
        (ProviderError::InvalidPath, FallbackOutcome::Other),
        (ProviderError::UnsupportedPolicy, FallbackOutcome::Other),
        (
            ProviderError::PolicyActionUnsupported {
                code: "ARGV_PROMPT",
            },
            FallbackOutcome::Other,
        ),
        (
            ProviderError::ProbeDirectory("fixture".into()),
            FallbackOutcome::Other,
        ),
        (
            ProviderError::IsolatedKeychainUnavailable("fixture".into()),
            FallbackOutcome::Other,
        ),
        (
            ProviderError::PromptTooLarge {
                limit: 1,
                origin: SettingSource::ShippedDefault,
            },
            FallbackOutcome::Other,
        ),
        (ProviderError::Malformed, FallbackOutcome::Other),
        (
            ProviderError::MalformedResult("fixture".into()),
            FallbackOutcome::Other,
        ),
        (ProviderError::Overflow, FallbackOutcome::Other),
    ];
    for (error, expected) in cases {
        provider_error_variant_is_classified(&error);
        assert_eq!(provider_preparation_fallback(&error), expected);
    }
}

// An added ProviderError variant must be considered when this classification changes.
fn provider_error_variant_is_classified(error: &provenant_providers::ProviderError) {
    use provenant_providers::ProviderError;
    match error {
        ProviderError::PromptTooLarge { .. }
        | ProviderError::Malformed
        | ProviderError::MalformedResult(_)
        | ProviderError::Overflow
        | ProviderError::IdentityMismatch
        | ProviderError::UnknownIdentity
        | ProviderError::ProbeDirectory(_)
        | ProviderError::LoginRequired
        | ProviderError::InvalidPath
        | ProviderError::UnsupportedPolicy
        | ProviderError::PolicyActionUnsupported { .. }
        | ProviderError::IsolatedKeychainUnavailable(_)
        | ProviderError::CredentialUnavailable
        | ProviderError::RefreshDeferred
        | ProviderError::CredentialNotFound
        | ProviderError::CredentialExpired
        | ProviderError::CredentialReadFailed(_) => {}
    }
}

#[test]
fn codex_auth_reprobe_uses_selected_home_and_trusts_status()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor, Termination};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Fake {
        directory: std::path::PathBuf,
        healthy: AtomicBool,
    }
    impl Executor for Fake {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            assert_eq!(options.argv[1], "login");
            assert_eq!(options.argv[2], "status");
            assert!(
                options
                    .env
                    .unwrap()
                    .iter()
                    .any(|(key, value)| key == "CODEX_HOME" && value == self.directory.as_os_str())
            );
            Ok(ExecOutcome {
                termination: Termination::Exit(if self.healthy.load(Ordering::Relaxed) {
                    0
                } else {
                    1
                }),
                stdout: Default::default(),
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }
    let temp = tempfile::tempdir()?;
    let entry: provenant_providers::accounts::Registration = serde_json::from_value(
        serde_json::json!({"adapter":"codex","label":"work","key":"fake","directory":temp.path().join("selected")}),
    )?;
    let fake = Fake {
        directory: entry.directory.clone(),
        healthy: AtomicBool::new(true),
    };
    let executor = provenant_providers::host::HostProbeExecutor::new(&fake, temp.path())?;
    let probe = Probe {
        roots: None,
        executor: &executor,
        home: temp.path(),
        user: "fake".as_ref(),
        scratch: temp.path(),
    };
    assert!(codex_login_is_healthy(
        &entry,
        std::path::Path::new("/fake/codex"),
        &probe
    ));
    fake.healthy.store(false, Ordering::Relaxed);
    assert!(!codex_login_is_healthy(
        &entry,
        std::path::Path::new("/fake/codex"),
        &probe
    ));
    Ok(())
}

#[test]
fn cursor_auth_reprobe_accepts_only_selected_host_login() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_os::exec::{
        CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
    };
    struct Fake;
    impl Executor for Fake {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            assert_eq!(options.argv[1], "status");
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: include_bytes!(
                        "../../../../../tests/fixtures/providers/cursor/status.stdout"
                    )
                    .to_vec(),
                    ..Default::default()
                },
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }
    let temp = tempfile::tempdir()?;
    let mut entry: provenant_providers::accounts::Registration = serde_json::from_value(
        serde_json::json!({"adapter":"cursor","label":"default","key":"local","directory":temp.path()}),
    )?;
    let executor = provenant_providers::host::HostProbeExecutor::new(&Fake, temp.path())?;
    let probe = Probe {
        roots: None,
        executor: &executor,
        home: temp.path(),
        user: "fake".as_ref(),
        scratch: temp.path(),
    };
    assert!(cursor_login_is_healthy(
        &entry,
        std::path::Path::new("/fake/agent"),
        &probe
    ));
    entry.directory = temp.path().join("another");
    assert!(!cursor_login_is_healthy(
        &entry,
        std::path::Path::new("/fake/agent"),
        &probe
    ));
    Ok(())
}

#[test]
fn command_lookup_is_literal_and_preserves_symlink_invocation()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let root = super::canonical(temp.path())?;
    let executable = root.join("multicall");
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let link = root.join("cursor");
    std::os::unix::fs::symlink(&executable, &link)?;
    assert_eq!(
        super::find_command_on_path("cursor", std::slice::from_ref(&root))?,
        link
    );
    assert_eq!(super::canonical(&link)?, executable);
    // Model provider lookup keeps its cursor -> cursor-agent alias.
    assert!(super::find_executable_on_path("cursor", &[root]).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn writer_dependency_preparation_installs_skips_and_reports_failure()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let pnpm = bin.join("pnpm");
    std::fs::write(
        &pnpm,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PWD/pnpm.log\"\nif [ \"$(basename \"$PWD\")\" = failing ]; then printf '%s\\n' 'ghp_abcdefghijklmnopqrstuvwxyz0123456789' 'plain-fixture-secret-value' >&2; exit 7; fi\nmkdir -p node_modules\ntouch node_modules/.modules.yaml\n",
    )?;
    std::fs::set_permissions(&pnpm, std::fs::Permissions::from_mode(0o700))?;
    let locate = || {
        std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
            .map(|path| (path, "fixture"))
            .map_err(|error| error.to_string())
    };
    let cancel = provenant_core::CancelToken::new();
    let worktree = |name: &str| -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = temp.path().join(name);
        std::fs::create_dir_all(&path)?;
        std::fs::write(path.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n")?;
        Ok(path)
    };
    let log_path = |path: &Path| path.join("pnpm.log");

    let fresh = worktree("fresh")?;
    let disabled = super::prepare_writer_dependencies(&fresh, false, &locate, &cancel)?;
    assert_eq!(disabled["reason"], "disabled");
    assert!(!log_path(&fresh).exists());
    let installed = super::prepare_writer_dependencies(&fresh, true, &locate, &cancel)?;
    assert_eq!(installed["outcome"], "installed");
    let installed_effects = std::fs::read_to_string(log_path(&fresh))?;
    assert!(!installed_effects.is_empty());

    let fallback_tree = worktree("fallback")?;
    let fallback = || {
        super::dependency_path_with_fallback(
            || Err("probe failed".into()),
            Some(locate()?.0),
            &cancel,
        )
    };
    let installed = super::prepare_writer_dependencies(&fallback_tree, true, &fallback, &cancel)?;
    assert_eq!(installed["path_source"], "daemon_fallback");
    assert!(!std::fs::read_to_string(log_path(&fallback_tree))?.is_empty());

    // The completion marker marks an installed tree, so a second call skips.
    let skipped = super::prepare_writer_dependencies(&fresh, true, &locate, &cancel)?;
    assert_eq!(skipped["reason"], "already_installed");
    assert_eq!(
        std::fs::read_to_string(log_path(&fresh))?,
        installed_effects
    );

    // A `node_modules` without the marker is a partial install and is repaired.
    let partial = worktree("partial")?;
    std::fs::create_dir_all(partial.join("node_modules"))?;
    let repaired = super::prepare_writer_dependencies(&partial, true, &locate, &cancel)?;
    assert_eq!(repaired["outcome"], "installed");
    assert!(partial.join("node_modules/.modules.yaml").is_file());

    let failing = worktree("failing")?;
    let error = super::prepare_writer_dependencies(&failing, true, &locate, &cancel)
        .expect_err("fake pnpm failure is refused");
    assert!(!error.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!error.contains("plain-fixture-secret-value"));
    let event = super::dependency_preparation_event(
        Err(error),
        true,
        &provenant_records::run::HarnessProfile::default(),
        &provenant_records::policy::Policy::default(),
    )
    .expect("default policy records the warning");
    let warning = event["reason"].as_str().expect("retained warning reason");
    assert_eq!(event["outcome"], "warning");
    assert!(!warning.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!warning.contains("plain-fixture-secret-value"));
    assert_dependency_path_fallback_and_cancellation();
    assert_missing_pnpm_is_refused()?;
    Ok(())
}
#[test]
fn writer_preparation_uses_login_path_for_pnpm_and_node() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let root = super::canonical(temp.path())?;
    // Neither directory is on the test process PATH. Keep node separate from
    // pnpm to catch fixes that only prepend pnpm's parent directory.
    let bin = root.join("pnpm bin");
    let node_bin = root.join("node bin");
    std::fs::create_dir_all(&bin)?;
    std::fs::create_dir_all(&node_bin)?;
    let path = std::env::join_paths([&bin, &node_bin])?;
    let shell = root.join("login-shell");
    let pnpm = bin.join("pnpm");
    let node = node_bin.join("node");
    // Model a login startup file which prints a banner, then configures PATH.
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\n[ \"$1\" = -lc ] || exit 2\nprintf 'login banner\\n'\nexport PATH='{}'\neval \"$2\"\n",
            path.to_string_lossy()
        ),
    )?;
    std::fs::write(&pnpm, "#!/usr/bin/env node\n")?;
    std::fs::write(&node, "#!/bin/sh\nprintf '%s\\n' \"$*\" > node.log\n")?;
    for executable in [&shell, &pnpm, &node] {
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))?;
    }
    let cancel = provenant_core::CancelToken::new();
    let search_path =
        || super::login_dependency_path(&shell, &root, &cancel).map(|path| (path, "login_shell"));
    assert_eq!(search_path()?.0, path);
    std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n")?;
    let installed = super::prepare_writer_dependencies(&root, true, &search_path, &cancel)?;
    assert_eq!(installed["outcome"], "installed");
    assert_eq!(installed["path_source"], "login_shell");
    assert_eq!(
        std::fs::read_to_string(root.join("node.log"))?,
        format!("{} install --frozen-lockfile\n", pnpm.display())
    );

    // A failed or cancelled shell must not fall back to the daemon PATH.
    cancel.cancel();
    assert!(
        search_path()
            .expect_err("cancelled probe")
            .contains("cancelled")
    );
    Ok(())
}

fn assert_dependency_path_fallback_and_cancellation() {
    let cancel = provenant_core::CancelToken::new();
    let daemon = std::ffi::OsString::from("/daemon/bin");
    for (fallback_path, expected_path) in [(Some(daemon.clone()), Some(daemon)), (None, None)] {
        let result = super::dependency_path_with_fallback(
            || Err("fixture probe failure".into()),
            fallback_path,
            &cancel,
        );
        match (result, expected_path) {
            (Ok((path, _source)), Some(expected_path)) => assert_eq!(path, expected_path),
            (Err(_), None) => {}
            _ => panic!("fallback result does not match the declared case"),
        }
    }
    cancel.cancel();
    assert!(
        super::dependency_path_with_fallback(
            || Err("fixture probe failure".into()),
            Some(std::ffi::OsString::from("/daemon/bin")),
            &cancel,
        )
        .is_err()
    );
}

fn assert_missing_pnpm_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )?;
    let path = std::env::join_paths([temp.path()])?;
    let cancel = provenant_core::CancelToken::new();
    assert!(
        super::prepare_writer_dependencies(temp.path(), true, &|| Ok((path, "fixture")), &cancel,)
            .is_err()
    );
    Ok(())
}

use super::*;

#[test]
fn default_launch_requests_publishing_unless_switched_off() {
    let mut knobs = std::collections::BTreeMap::new();
    assert!(publishing_requested(true, &knobs));
    knobs.insert("lanes.publish".into(), serde_json::json!(false));
    assert!(!publishing_requested(true, &knobs));
    knobs.insert("lanes.publish".into(), serde_json::json!(true));
    assert!(publishing_requested(true, &knobs));
    assert!(!publishing_requested(false, &knobs));
}

#[test]
fn cleared_preparation_injects_host_recorded_github_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "PROVENANT_TEST_CLEARED_GITHUB";
    if std::env::var_os(CHILD).is_none() {
        let result = crate::test_support::rerun_test(
            module_path!(),
            "cleared_preparation_injects_host_recorded_github_credentials",
            &[(CHILD.into(), "1".into())],
            &[
                "PATH",
                "HOME",
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_CONFIG_DIR",
                "XDG_CONFIG_HOME",
                "GIT_CONFIG_GLOBAL",
            ],
            std::time::Duration::from_secs(60),
        )?;
        use provenant_os::exec::Termination;
        assert!(
            matches!(result.termination, Termination::Exit(0)) && !result.timed_out,
            "{}",
            String::from_utf8_lossy(&result.stdout.bytes)
        );
        return Ok(());
    }
    use std::os::unix::fs::PermissionsExt;
    assert!(std::env::var_os("PATH").is_none());
    assert!(std::env::var_os("HOME").is_none());
    assert!(std::env::var_os("GH_TOKEN").is_none());
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let config = home.join("relocated-gh");
    std::fs::create_dir(&config)?;
    std::fs::write(config.join("fixture-token"), "fixture-secret")?;
    let gh = home.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n[ \"$1\" = auth ] && [ \"$2\" = token ] || exit 1\n/bin/cat \"$GH_CONFIG_DIR/fixture-token\"\n",
    )?;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700))?;
    let mut host = run::HostLaunchContext::from_environment(&home, &home, "/usr/bin/git".into());
    host.executable_search_path = vec![home.clone()];
    host.provider_path_environment
        .insert("GH_CONFIG_DIR".into(), config.clone().into_os_string());
    let gh = publishing_executable(&|name| {
        super::preparation::preparation_executable(Some(&host), false, name)
    })?;
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let mut environment = Vec::new();
    let injected = super::gh_auth::inject(
        &executor,
        &gh,
        &home,
        &|key| host.provider_path_environment.get(key).cloned(),
        &provenant_core::CancelToken::new(),
        &mut None,
        &mut environment,
    );
    assert!(injected.is_ok(), "{injected:?}");
    assert!(environment.contains(&("GH_TOKEN".into(), "fixture-secret".into())));
    assert!(environment.contains(&("GH_CONFIG_DIR".into(), config.into_os_string())));
    Ok(())
}

#[test]
fn publishing_canonicalises_symlinked_gh_and_ignores_missing_tools()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    std::fs::create_dir_all(home.join(".config/gh"))?;
    let executable = std::env::current_exe()?.canonicalize()?;
    let gh = home.join("gh");
    std::os::unix::fs::symlink(&executable, &gh)?;
    assert_eq!(
        publishing_executable(&|_| Ok(gh.clone())).ok(),
        Some(executable)
    );
    assert_eq!(
        publishing_executable(&|_| Err("not installed".into())).ok(),
        None
    );
    std::fs::remove_file(&gh)?;
    assert_eq!(publishing_executable(&|_| Ok(gh.clone())).ok(), None);
    Ok(())
}

#[test]
fn version_probe_reports_outcomes_without_disclosing_output()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{
        CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
    };

    struct VersionExecutor(ExecOutcome);
    impl Executor for VersionExecutor {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            assert_eq!(options.argv[1], "--version");
            assert_eq!(options.timeout, std::time::Duration::from_secs(4));
            assert!(matches!(
                options.stdout,
                provenant_os::exec::StreamPolicy::Capture { limit: 128 }
            ));
            assert!(matches!(
                options.stderr,
                provenant_os::exec::StreamPolicy::Discard
            ));
            Ok(self.0.clone())
        }
    }
    let temp = tempfile::tempdir()?;
    let root = canonical(temp.path())?;
    for path in ["home", "project", "state/provider-updates"] {
        std::fs::create_dir_all(root.join(path))?;
    }
    let roots = provenant_core::RootInputs {
        state_dir: Some(root.join("state")),
        ..provenant_core::RootInputs::new(root.join("home"), root.join("project"))
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let last_update = provenant_providers::update::UpdateRecord {
        checked_unix_seconds: 99,
        old_version: Some("opencode v2.0.31".into()),
        new_version: Some("opencode v2.0.32".into()),
        outcome: "updated".into(),
    };
    std::fs::write(
        root.join("state/provider-updates/opencode.json"),
        serde_json::to_vec(&last_update)?,
    )?;
    let entry = serde_json::from_value(serde_json::json!({
        "adapter": "opencode", "label": "go", "key": "fixture", "directory": root,
    }))?;
    struct VersionCase {
        termination: Termination,
        timed_out: bool,
        stdout_truncated: bool,
        bytes: &'static [u8],
        stdout_read_failed: bool,
        drain_truncated: bool,
        overflow_stopped: bool,
        cancelled: bool,
        version: Option<&'static str>,
    }
    let cases = [
        VersionCase {
            termination: Termination::Exit(42),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"private output",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Signal(9),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"private output",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: true,
            stdout_truncated: false,
            bytes: b"private output",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout_truncated: true,
            bytes: b"private output",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"opencode v2.0.32\n",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            version: Some("opencode v2.0.32"),
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"opencode v2.0.32",
            stdout_read_failed: true,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"opencode v2.0.32",
            stdout_read_failed: false,
            drain_truncated: true,
            overflow_stopped: false,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"opencode v2.0.32",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: true,
            cancelled: false,
            version: None,
        },
        VersionCase {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout_truncated: false,
            bytes: b"opencode v2.0.32",
            stdout_read_failed: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: true,
            version: None,
        },
    ];
    for case in cases {
        let executor = VersionExecutor(ExecOutcome {
            termination: case.termination,
            timed_out: case.timed_out,
            stdout: CapturedStream {
                bytes: case.bytes.to_vec(),
                total_bytes: case.bytes.len() as u64,
                truncated: case.stdout_truncated,
                read_failed: case.stdout_read_failed,
                ..CapturedStream::default()
            },
            stderr: CapturedStream::default(),
            drain_truncated: case.drain_truncated,
            overflow_stopped: case.overflow_stopped,
            cancelled: case.cancelled,
            elapsed_micros: 80_000,
        });
        let executor = provenant_providers::host::HostProbeExecutor::new(&executor, &root)?;
        let probe = Probe {
            roots: Some(&roots),
            executor: &executor,
            home: &root,
            user: "fixture".as_ref(),
            scratch: &root,
        };
        match observe_account(
            &entry,
            Path::new("/fixture/opencode"),
            &probe,
            "fixture",
            100,
        ) {
            Ok(record) => {
                let expected = case.version.ok_or("invalid version outcome accepted")?;
                assert_eq!(record.version, expected);
                assert_eq!(record.last_update, Some(last_update.clone()));
            }
            Err(AccountObservationError::VersionUnavailable(detail)) => {
                assert!(case.version.is_none(), "valid version outcome refused");
                assert!(!detail.contains("private output"));
            }
            Err(error) => {
                return Err(format!("unexpected account observation outcome: {error:?}").into());
            }
        }
    }
    Ok(())
}

/// Agy has registered accounts and a local login. A user with no agy
/// registration gets a synthetic `local` entry, which has no registered identity
/// to probe: it must observe as available on the user's own login, while a
/// registered agy key is still verified against a fresh identity probe.
#[test]
fn unregistered_agy_observes_on_its_local_login_without_an_identity_probe()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{
        CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
    };

    #[derive(Default)]
    struct Agy(std::sync::Mutex<Vec<Vec<String>>>);
    impl Executor for Agy {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let argv: Vec<String> = options
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let version = argv.get(1).is_some_and(|arg| arg == "--version");
            self.0.lock().unwrap().push(argv);
            // The identity probe fails, as it does for an unsigned-in HOME.
            let (code, bytes) = if version {
                (0, b"agy 1.0.0\n".to_vec())
            } else {
                (1, Vec::new())
            };
            Ok(ExecOutcome {
                termination: Termination::Exit(code),
                timed_out: false,
                stdout: CapturedStream {
                    total_bytes: bytes.len() as u64,
                    bytes,
                    ..CapturedStream::default()
                },
                stderr: CapturedStream::default(),
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 80_000,
            })
        }
    }

    let temp = tempfile::tempdir()?;
    let root = canonical(temp.path())?;
    for path in ["home", "project", "state"] {
        std::fs::create_dir_all(root.join(path))?;
    }
    let home = root.join("home");
    let roots = provenant_core::RootInputs {
        state_dir: Some(root.join("state")),
        ..provenant_core::RootInputs::new(home.clone(), root.join("project"))
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let agy = Agy::default();
    let executor = provenant_providers::host::HostProbeExecutor::new(&agy, &root.join("state"))?;
    let probe = Probe {
        roots: Some(&roots),
        executor: &executor,
        home: &home,
        user: "fixture".as_ref(),
        scratch: &root,
    };
    let entry = |key: &str| {
        serde_json::from_value(serde_json::json!({
            "adapter": "agy", "label": "default", "key": key, "directory": home,
        }))
    };

    // The synthetic entry preparation adds when nothing is registered.
    let local = observe_account(
        &entry("local")?,
        Path::new("/fixture/agy"),
        &probe,
        "r",
        100,
    )?;
    assert_eq!(local.account_key.as_deref(), Some("local"));
    assert_eq!(local.version, "agy 1.0.0");
    assert!(
        agy.0
            .lock()
            .unwrap()
            .iter()
            .all(|argv| argv[1] == "--version"),
        "no identity probe for the local login"
    );

    // A registered key is still verified: the failed probe refuses the account.
    let error = observe_account(
        &entry("agy-google-fixture")?,
        Path::new("/fixture/agy"),
        &probe,
        "r",
        100,
    )
    .err()
    .ok_or("registered agy account skipped its identity probe")?
    .to_string();
    assert!(
        agy.0.lock().unwrap().iter().any(|argv| argv[1] == "-p"),
        "registered agy runs the /usage identity probe"
    );
    assert!(!error.is_empty());
    Ok(())
}

/// A registered isolated agy account whose keychain vanished must be refused
/// without probing: the shared login may not be signed in under the second
/// account's key.
#[test]
fn agy_account_whose_isolated_keychain_vanished_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{
        CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
    };

    /// Signs in as one Google account per `HOME`, as a keychain-backed agy does.
    struct Agy {
        home: std::path::PathBuf,
        isolated: std::path::PathBuf,
        probed_homes: std::sync::Mutex<Vec<std::path::PathBuf>>,
    }
    impl Executor for Agy {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let argv: Vec<String> = options
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let bytes = if argv[1] == "--version" {
                b"agy 1.0.0\n".to_vec()
            } else {
                let home = options
                    .env
                    .unwrap_or_default()
                    .iter()
                    .find(|(key, _)| key == "HOME")
                    .map(|(_, value)| std::path::PathBuf::from(value))
                    .expect("probe sets HOME");
                self.probed_homes.lock().unwrap().push(home.clone());
                let email = if home == self.isolated {
                    "second@example.test"
                } else {
                    assert_eq!(home, self.home, "probe HOME is a known sign-in");
                    "first@example.test"
                };
                let log = argv
                    .iter()
                    .position(|arg| arg == "--log-file")
                    .map(|index| std::path::PathBuf::from(&argv[index + 1]))
                    .expect("probe names a log file");
                std::fs::write(
                    log,
                    format!("OAuth: authenticated successfully as {email}\n"),
                )
                .expect("write probe log");
                br#"{"status":"SUCCESS"}"#.to_vec()
            };
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                timed_out: false,
                stdout: CapturedStream {
                    total_bytes: bytes.len() as u64,
                    bytes,
                    ..CapturedStream::default()
                },
                stderr: CapturedStream::default(),
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 80_000,
            })
        }
    }

    let temp = tempfile::tempdir()?;
    let root = canonical(temp.path())?;
    let home = root.join("home");
    let isolated = root.join("agy-second");
    let keychain = isolated.join("Library/Keychains/login.keychain-db");
    for path in [&home, &root.join("project"), &root.join("state")] {
        std::fs::create_dir_all(path)?;
    }
    std::fs::create_dir_all(keychain.parent().unwrap())?;
    std::fs::write(&keychain, b"")?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(root.join("state")),
        ..provenant_core::RootInputs::new(home.clone(), root.join("project"))
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let agy = Agy {
        home: home.clone(),
        isolated: isolated.clone(),
        probed_homes: Default::default(),
    };
    let executor = provenant_providers::host::HostProbeExecutor::new(&agy, &root.join("state"))?;
    let probe = Probe {
        roots: Some(&roots),
        executor: &executor,
        home: &home,
        user: "fixture".as_ref(),
        scratch: &root,
    };
    let key = format!(
        "agy-{}",
        provenant_providers::identity::agy_fingerprint(
            b"OAuth: authenticated successfully as second@example.test\n"
        )
        .ok_or("fingerprint")?
    );
    let entry = serde_json::from_value(serde_json::json!({
        "adapter": "agy", "label": "second", "key": key, "directory": isolated,
    }))?;
    let observe = || observe_account(&entry, Path::new("/fixture/agy"), &probe, "r", 100);

    // Registered with its own keychain, the account observes under its own HOME.
    assert_eq!(observe()?.account_key.as_deref(), Some(key.as_str()));
    // An unexpired identity probe still checks the routed registration. A
    // stale routed key must never launch as the currently signed-in account.
    let mut stale = entry.clone();
    stale.key = "agy-google-000000000000".into();
    assert!(matches!(
        observe_account(&stale, Path::new("/fixture/agy"), &probe, "r", 101),
        Err(AccountObservationError::Other(_))
    ));
    assert!(agy.probed_homes.lock().unwrap().contains(&isolated));
    // With the keychain gone, the probe must not sign in as the first account.
    std::fs::remove_file(&keychain)?;
    assert!(matches!(observe(), Err(AccountObservationError::Other(_))));
    assert!(!agy.probed_homes.lock().unwrap().contains(&home));
    Ok(())
}

#[test]
fn credential_failure_skips_retain_only_typed_login_causes() {
    use provenant_providers::ProviderError;
    use provenant_records::{RouteExclusionReason, run::SkipKind};
    let entry: provenant_providers::accounts::Registration =
        serde_json::from_value(serde_json::json!({
            "adapter": "claude", "label": "work", "key": "selected-work",
            "directory": "/fixture/account work"
        }))
        .unwrap();
    let cases = [
        (
            ProviderError::CredentialNotFound,
            SkipKind::AuthRequired,
            Some(RouteExclusionReason::AuthRequired),
            None,
        ),
        (
            ProviderError::CredentialExpired,
            SkipKind::AuthRequired,
            Some(RouteExclusionReason::LoginExpired),
            None,
        ),
        (
            ProviderError::CredentialUnavailable,
            SkipKind::Refused,
            None,
            None,
        ),
        (
            ProviderError::RefreshDeferred,
            SkipKind::Refused,
            None,
            None,
        ),
        (
            ProviderError::CredentialReadFailed("TOKEN_SENTINEL /private/provider\u{1b}".into()),
            SkipKind::Refused,
            None,
            Some("TOKEN_SENTINEL"),
        ),
    ];
    for (error, expected_kind, expected_cause, secret) in cases {
        let skipped =
            credential_failure_skip("claude/model", &error, &entry.label, &entry.directory);
        assert_eq!(skipped.kind, expected_kind);
        assert_eq!(skipped.cause, expected_cause);
        assert!(!skipped.detail.contains("/fixture/account work"));
        assert!(!skipped.detail.contains("CLAUDE_CONFIG_DIR"));
        if let Some(secret) = secret {
            assert!(!skipped.detail.contains(secret));
        }
        assert!(!skipped.detail.chars().any(char::is_control));
        assert_eq!(skipped.diagnostic().code, "REFUSED");
    }
}

fn prompt_paths(attempt: &Path) -> Result<run::LaunchPaths, Box<dyn std::error::Error>> {
    Ok(run::LaunchPaths {
        attempt_id: provenant_core::AttemptId::from_bytes([1; 16]),
        operation_id: provenant_core::OperationId::parse("spill-prompt")?,
        task_index: 0,
        lane_depth: 1,
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        provider_state: attempt.join("provider-state"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
    })
}

#[test]
fn launch_prompt_preserves_admitted_text_when_host_additions_exceed_one_mib()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let attempt = canonical(temp.path())?;
    let paths = prompt_paths(&attempt)?;
    let argv_limit = provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES;
    for spec in provenant_records::ADAPTER_SPECS {
        let adapter = spec.name;
        for text in ["small prompt".to_owned(), "a".repeat(argv_limit)] {
            let prompt = launch_prompt(
                &text,
                &paths,
                &attempt,
                adapter,
                argv_limit,
                provenant_records::diagnostic::SettingSource::Configuration,
                &provenant_records::policy::Policy::default(),
            )?;
            assert_eq!(prompt.text(), text);
            assert!(prompt.file().is_none());
        }
        let text = format!(
            "Durable output instructions\n\n{}",
            "a".repeat(PROMPT_LIMIT as usize)
        );
        let prompt = launch_prompt(
            &text,
            &paths,
            &attempt,
            adapter,
            argv_limit,
            provenant_records::diagnostic::SettingSource::Configuration,
            &provenant_records::policy::Policy::default(),
        )?;
        if spec.argv_prompt {
            assert_eq!(
                std::fs::read_to_string(prompt.file().ok_or("expected spill file")?)?,
                text
            );
        } else {
            assert_eq!(prompt.text(), text);
            assert!(prompt.file().is_none());
        }
    }
    Ok(())
}

#[test]
fn launch_prompt_keeps_oversized_stdin_prompts_in_memory() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let attempt = canonical(temp.path())?;
    let mut paths = prompt_paths(&attempt)?;
    let argv_limit = provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES;
    let text = format!("Durable output instructions\n\n{}", "é".repeat(argv_limit));
    for depth in [1, 2] {
        paths.lane_depth = depth;
        for spec in provenant_records::ADAPTER_SPECS
            .iter()
            .filter(|spec| !spec.argv_prompt)
        {
            let adapter = spec.name;
            for text in [&text, &"a".repeat(PROMPT_LIMIT as usize)] {
                let prompt = launch_prompt(
                    text,
                    &paths,
                    &attempt,
                    adapter,
                    argv_limit,
                    provenant_records::diagnostic::SettingSource::Configuration,
                    &provenant_records::policy::Policy::default(),
                )?;
                assert_eq!(prompt.text(), text);
                assert!(prompt.file().is_none());
            }
        }
    }
    assert!(!attempt.join("prompt.md").exists());
    Ok(())
}

#[test]
fn argv_prompt_spills_at_top_level_and_applies_nested_policy_by_strength()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES;
    use provenant_records::diagnostic::{ResourceFact, SettingSource};
    use provenant_records::policy::{Policy, Source, Strength};
    use std::os::unix::fs::PermissionsExt;

    for spec in provenant_records::ADAPTER_SPECS
        .iter()
        .filter(|spec| spec.argv_prompt)
    {
        let text = "x".repeat(DEFAULT_ARGV_PROMPT_LIMIT_BYTES + 1);
        for strength in Strength::ALL {
            let refused = strength == Strength::Refuse;
            let temp = tempfile::tempdir()?;
            let attempt = canonical(temp.path())?;
            let mut paths = prompt_paths(&attempt)?;
            paths.lane_depth = 2;
            let mut policy = Policy::default();
            if strength != Strength::Warn {
                policy.overlay(
                    &std::collections::BTreeMap::from([("ARGV_PROMPT".into(), strength)]),
                    Source::Dispatch,
                );
            }
            let result = launch_prompt(
                &text,
                &paths,
                &attempt,
                spec.name,
                DEFAULT_ARGV_PROMPT_LIMIT_BYTES,
                SettingSource::Configuration,
                &policy,
            );
            assert_eq!(policy.rule("ARGV_PROMPT").rule.strength, strength);
            if refused {
                let error = result
                    .err()
                    .ok_or("refuse strength accepted an oversized nested argv prompt")?;
                assert!(matches!(
                    error,
                    LaunchPromptError::Provider(
                        provenant_providers::ProviderError::PromptTooLarge { .. }
                    )
                ));
                assert_eq!(
                    error.resource_fact(),
                    Some(ResourceFact::ArgvPromptLimit {
                        limit: DEFAULT_ARGV_PROMPT_LIMIT_BYTES as u64,
                        origin: SettingSource::Configuration,
                    })
                );
                assert!(!attempt.join("prompt.md").exists());
            } else {
                let prompt = result?;
                assert_eq!(prompt.text(), text);
                assert!(prompt.file().is_none());
            }
        }

        let temp = tempfile::tempdir()?;
        let attempt = canonical(temp.path())?;
        let paths = prompt_paths(&attempt)?;
        let mut strict = Policy::default();
        strict.overlay(
            &std::collections::BTreeMap::from([("ARGV_PROMPT".into(), Strength::Refuse)]),
            Source::Dispatch,
        );
        let prompt = launch_prompt(
            &text,
            &paths,
            &attempt,
            spec.name,
            DEFAULT_ARGV_PROMPT_LIMIT_BYTES,
            SettingSource::Configuration,
            &strict,
        )?;
        let prompt_file = prompt
            .file()
            .ok_or("oversized top-level argv prompt was not spilled")?;
        assert!(prompt_file.starts_with(&attempt));
        assert_eq!(std::fs::read_to_string(prompt_file)?, text);
        assert_eq!(
            std::fs::metadata(prompt_file)?.permissions().mode() & 0o777,
            0o600
        );
        let repeated = launch_prompt(
            &text,
            &paths,
            &attempt,
            spec.name,
            DEFAULT_ARGV_PROMPT_LIMIT_BYTES,
            SettingSource::Configuration,
            &strict,
        )?;
        assert_eq!(repeated.file(), Some(prompt_file));

        let conflict = tempfile::tempdir()?;
        let conflict_attempt = canonical(conflict.path())?;
        let conflict_paths = prompt_paths(&conflict_attempt)?;
        let conflict_file = conflict_attempt.join("prompt.md");
        std::fs::write(&conflict_file, "existing state")?;
        assert!(
            launch_prompt(
                &text,
                &conflict_paths,
                &conflict_attempt,
                spec.name,
                DEFAULT_ARGV_PROMPT_LIMIT_BYTES,
                SettingSource::Configuration,
                &strict,
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(conflict_file)?, "existing state");
    }
    Ok(())
}

#[test]
fn launch_prompt_refuses_symlinked_attempt_ancestors() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = canonical(temp.path())?;
    std::fs::create_dir_all(state.join("actual/attempt"))?;
    std::os::unix::fs::symlink(state.join("actual"), state.join("runs"))?;
    let paths = prompt_paths(&state.join("runs/attempt"))?;
    let argv_limit = provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES;
    assert!(
        launch_prompt(
            &"a".repeat(argv_limit + 1),
            &paths,
            &state,
            "cursor",
            argv_limit,
            provenant_records::diagnostic::SettingSource::Configuration,
            &provenant_records::policy::Policy::default(),
        )
        .is_err()
    );
    assert!(!state.join("actual/attempt/prompt.md").exists());
    Ok(())
}
