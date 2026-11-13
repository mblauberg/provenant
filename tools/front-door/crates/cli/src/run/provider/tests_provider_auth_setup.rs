#[test]
fn provider_preparation_fallback_preserves_authority_and_identity_failures() {
    use provenant_providers::ProviderError;
    use provenant_routing::FallbackOutcome;
    for error in [
        ProviderError::LoginRequired,
        ProviderError::CredentialUnavailable,
        ProviderError::CredentialNotFound,
        ProviderError::CredentialExpired,
    ] {
        let outcome = provider_preparation_fallback(&error);
        assert_eq!(outcome, FallbackOutcome::AuthRequired);
        let policy = provenant_routing::FallbackPolicy {
            outcome,
            handoff: provenant_routing::FallbackHandoff::Local,
            custody_released: true,
            read_only: false,
            budget_remaining: true,
            pinned: false,
        };
        assert!(policy.allows_switch());
        assert!(
            !provenant_routing::FallbackPolicy {
                pinned: true,
                ..policy
            }
            .allows_switch()
        );
        assert!(
            !provenant_routing::FallbackPolicy {
                budget_remaining: false,
                ..policy
            }
            .allows_switch()
        );
        assert!(
            !provenant_routing::FallbackPolicy {
                custody_released: false,
                ..policy
            }
            .allows_switch()
        );
        assert!(
            !provenant_routing::FallbackPolicy {
                handoff: provenant_routing::FallbackHandoff::UncertainOrLaunched,
                ..policy
            }
            .allows_switch()
        );
    }
    for error in [
        ProviderError::UnsupportedPolicy,
        ProviderError::IdentityMismatch,
        ProviderError::UnknownIdentity,
        ProviderError::InvalidPath,
        ProviderError::IsolatedKeychainUnavailable("fixture".into()),
        ProviderError::ProbeDirectory("fixture".into()),
        ProviderError::CredentialReadFailed("fixture".into()),
        ProviderError::PromptTooLarge {
            limit: 1,
            origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
        },
        ProviderError::Overflow,
        ProviderError::Malformed,
    ] {
        assert_eq!(
            provider_preparation_fallback(&error),
            FallbackOutcome::Other
        );
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
                    bytes: b"Logged in successfully".to_vec(),
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
    let log = |path: &Path| std::fs::read_to_string(path.join("pnpm.log")).unwrap_or_default();

    let fresh = worktree("fresh")?;
    let disabled = super::prepare_writer_dependencies(&fresh, false, &locate, &cancel)?;
    assert_eq!(disabled["reason"], "disabled");
    assert_eq!(log(&fresh), "");
    let installed = super::prepare_writer_dependencies(&fresh, true, &locate, &cancel)?;
    assert_eq!(installed["outcome"], "installed");
    assert_eq!(installed["command"], "pnpm install --frozen-lockfile");
    assert_eq!(log(&fresh), "install --frozen-lockfile\n");

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
    assert_eq!(log(&fallback_tree), "install --frozen-lockfile\n");

    // The completion marker marks an installed tree, so a second call skips.
    let skipped = super::prepare_writer_dependencies(&fresh, true, &locate, &cancel)?;
    assert_eq!(skipped["reason"], "already_installed");
    assert_eq!(log(&fresh), "install --frozen-lockfile\n");

    // A `node_modules` without the marker is a partial install and is repaired.
    let partial = worktree("partial")?;
    std::fs::create_dir_all(partial.join("node_modules"))?;
    let repaired = super::prepare_writer_dependencies(&partial, true, &locate, &cancel)?;
    assert_eq!(repaired["outcome"], "installed");
    assert!(partial.join("node_modules/.modules.yaml").is_file());

    let failing = worktree("failing")?;
    let error = super::prepare_writer_dependencies(&failing, true, &locate, &cancel)
        .expect_err("fake pnpm failure is refused");
    assert!(error.contains("pnpm install --frozen-lockfile"));
    assert!(error.contains("exited with status 7"));
    assert!(!error.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!error.contains("plain-fixture-secret-value"));
    let event = super::dependency_preparation_event(
        Err(error),
        true,
        &provenant_records::run::HarnessProfile::default(),
    );
    let warning = event["reason"].as_str().expect("retained warning reason");
    assert_eq!(event["outcome"], "warning");
    assert!(warning.contains("pnpm install --frozen-lockfile"));
    assert!(warning.contains("exited with status 7"));
    assert!(warning.contains("in the worktree"));
    assert!(!warning.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!warning.contains("plain-fixture-secret-value"));
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

#[test]
fn writer_preparation_path_fallback_and_cancellation() {
    let cancel = provenant_core::CancelToken::new();
    let daemon = std::ffi::OsString::from("/daemon/bin");
    let fallback = || {
        super::dependency_path_with_fallback(
            || Err("probe failed".into()),
            Some(daemon.clone()),
            &cancel,
        )
    };
    assert_eq!(fallback().unwrap(), (daemon.clone(), "daemon_fallback"));
    assert_eq!(
        super::dependency_path_with_fallback(|| Err("probe failed".into()), None, &cancel,)
            .unwrap_err(),
        "probe failed"
    );
    cancel.cancel();
    assert!(fallback().unwrap_err().contains("cancelled"));
}

#[test]
fn writer_preparation_missing_pnpm_names_searched_path() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )?;
    let path = std::env::join_paths([temp.path()])?;
    let cancel = provenant_core::CancelToken::new();
    let error = super::prepare_writer_dependencies(
        temp.path(),
        true,
        &|| Ok((path.clone(), "fixture")),
        &cancel,
    )
    .expect_err("missing pnpm is refused");
    assert!(error.starts_with("dependency_tool_not_found:"));
    assert!(error.contains(&format!("searched PATH={}", path.to_string_lossy())));
    assert!(error.contains("source: fixture"));
    assert!(error.contains("pnpm --version"));
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
    // Isolate environment clearing from the rest of the test process.
    const CHILD: &str = "PROVENANT_TEST_CLEARED_GITHUB";
    if std::env::var_os(CHILD).is_none() {
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            std::thread::current().name().unwrap().into(),
            "--nocapture".into(),
        ];
        let cwd = std::env::current_dir()?;
        let environment = [(CHILD.into(), "1".into())];
        let mut options = ExecOptions::new(&argv, &cwd, std::time::Duration::from_secs(60));
        options.env = Some(&environment);
        let result = SystemExecutor::new(1)?.execute(options)?;
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
    let warning = super::gh_auth::inject(
        &executor,
        &gh,
        &home,
        &|key| host.provider_path_environment.get(key).cloned(),
        &provenant_core::CancelToken::new(),
        &mut None,
        &mut environment,
    );
    assert!(warning.is_none(), "{warning:?}");
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
    for (termination, timed_out, truncated, bytes, detail) in [
        (
            Termination::Exit(42),
            false,
            false,
            b"private output".as_slice(),
            Some("termination=Exit(42)"),
        ),
        (
            Termination::Signal(9),
            false,
            false,
            b"private output".as_slice(),
            Some("termination=Signal(9)"),
        ),
        (
            Termination::Exit(0),
            true,
            false,
            b"private output".as_slice(),
            Some("timed_out=true"),
        ),
        (
            Termination::Exit(0),
            false,
            true,
            b"private output".as_slice(),
            Some("stdout_truncated=true"),
        ),
        (
            Termination::Exit(0),
            false,
            false,
            b"".as_slice(),
            Some("empty version output"),
        ),
        (
            Termination::Exit(0),
            false,
            false,
            b"opencode v2.0.32\n".as_slice(),
            None,
        ),
    ] {
        let executor = VersionExecutor(ExecOutcome {
            termination,
            timed_out,
            stdout: CapturedStream {
                bytes: bytes.to_vec(),
                total_bytes: bytes.len() as u64,
                truncated,
                ..CapturedStream::default()
            },
            stderr: CapturedStream::default(),
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
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
        let result = observe_account(
            &entry,
            Path::new("/fixture/opencode"),
            &probe,
            "fixture",
            100,
        );
        match detail {
            Some(detail) => {
                let error = result.err().ok_or("failed version probe accepted")?;
                assert!(
                    matches!(error, AccountObservationError::VersionUnavailable(_)),
                    "{error}"
                );
                let error = error.to_string();
                assert!(error.contains(detail), "{error}");
                assert!(!error.contains("private output"));
            }
            None => {
                let record = result?;
                assert_eq!(record.version, "opencode v2.0.32");
                assert_eq!(record.last_update, Some(last_update.clone()));
            }
        }
    }
    for incomplete in 0..4 {
        let executor = VersionExecutor(ExecOutcome {
            termination: Termination::Exit(0),
            timed_out: false,
            stdout: CapturedStream {
                bytes: b"opencode v2.0.32".to_vec(),
                read_failed: incomplete == 0,
                ..CapturedStream::default()
            },
            stderr: CapturedStream::default(),
            drain_truncated: incomplete == 1,
            overflow_stopped: incomplete == 2,
            cancelled: incomplete == 3,
            elapsed_micros: 0,
        });
        let executor = provenant_providers::host::HostProbeExecutor::new(&executor, &root)?;
        let probe = Probe {
            roots: Some(&roots),
            executor: &executor,
            home: &root,
            user: "fixture".as_ref(),
            scratch: &root,
        };
        assert!(matches!(
            observe_account(
                &entry,
                Path::new("/fixture/opencode"),
                &probe,
                "fixture",
                100
            ),
            Err(AccountObservationError::VersionUnavailable(_))
        ));
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
    let mismatch = observe_account(&stale, Path::new("/fixture/agy"), &probe, "r", 101)
        .expect_err("cached fingerprint must reject the stale routed key")
        .to_string();
    assert!(mismatch.contains("changed identity"), "{mismatch}");
    assert_eq!(agy.probed_homes.lock().unwrap().len(), 1);
    // With the keychain gone, the probe must not sign in as the first account.
    std::fs::remove_file(&keychain)?;
    let error = observe()
        .err()
        .ok_or("account observed after its keychain vanished")?
        .to_string();
    assert!(
        error.contains(&keychain.display().to_string()) && error.contains("isolated keychain"),
        "{error}"
    );
    assert!(
        !agy.probed_homes.lock().unwrap().contains(&home),
        "the shared login must not be probed for the isolated account"
    );
    Ok(())
}

#[test]
fn credential_messages_suggest_signin_only_for_missing_or_expired_tokens() {
    use provenant_providers::ProviderError;
    let directory = Path::new("/fake/account");
    for error in [
        ProviderError::CredentialNotFound,
        ProviderError::CredentialExpired,
    ] {
        let message = credential_failure_message(&error, "work", directory);
        assert!(message.contains("sign in again"), "{message}");
        assert!(message.contains(directory.to_str().unwrap()), "{message}");
    }
    for error in [
        ProviderError::CredentialReadFailed("security exited with code 1".into()),
        ProviderError::CredentialReadFailed("stored credential JSON is invalid".into()),
        ProviderError::CredentialUnavailable,
    ] {
        let message = credential_failure_message(&error, "work", directory);
        assert!(message.contains(&error.to_string()));
        assert!(!message.contains("sign in again"));
    }
}

#[test]
fn credential_refresh_skip_retains_selected_account_login_only_for_typed_causes() {
    use provenant_providers::ProviderError;
    use provenant_records::RouteExclusionReason;
    let entry: provenant_providers::accounts::Registration =
        serde_json::from_value(serde_json::json!({
            "adapter":"claude", "label":"work", "key":"selected-work",
            "directory":"/fixture/account work"
        }))
        .unwrap();
    for (error, cause) in [
        (
            ProviderError::CredentialNotFound,
            RouteExclusionReason::AuthRequired,
        ),
        (
            ProviderError::CredentialExpired,
            RouteExclusionReason::LoginExpired,
        ),
    ] {
        let skipped =
            credential_failure_skip("claude/model", &error, &entry.label, &entry.directory);
        let skipped: SkippedRoute =
            serde_json::from_slice(&serde_json::to_vec(&skipped).unwrap()).unwrap();
        assert_eq!(skipped.kind, SkipKind::AuthRequired);
        assert_eq!(skipped.cause, Some(cause));
        assert!(skipped.detail.contains("account work"));
        assert!(
            skipped
                .detail
                .contains("CLAUDE_CONFIG_DIR='/fixture/account work'")
        );
        assert!(skipped.detail.contains("claude auth login"));
        let diagnostic = serde_json::to_string(&skipped.diagnostic()).unwrap();
        assert!(!diagnostic.contains("/fixture/account work"));
        assert_eq!(skipped.diagnostic().code, "REFUSED");
    }
    for error in [
        ProviderError::CredentialReadFailed("TOKEN_SENTINEL /private/provider\u{1b}".into()),
        ProviderError::CredentialUnavailable,
    ] {
        let skipped =
            credential_failure_skip("claude/model", &error, &entry.label, &entry.directory);
        assert_eq!(skipped.kind, SkipKind::AuthRequired);
        assert_eq!(skipped.cause, None);
        assert!(!skipped.detail.contains("TOKEN_SENTINEL"));
        assert!(!skipped.detail.contains("/private/provider"));
        assert!(!skipped.detail.contains("CLAUDE_CONFIG_DIR"));
        assert!(!skipped.detail.contains("/fixture/account work"));
        assert!(!skipped.detail.chars().any(char::is_control));
    }
}

fn prompt_paths(attempt: &Path) -> Result<run::LaunchPaths, Box<dyn std::error::Error>> {
    Ok(run::LaunchPaths {
        attempt_id: provenant_core::AttemptId::from_bytes([1; 16]),
        operation_id: provenant_core::OperationId::parse("spill-prompt")?,
        task_index: 0,
        lane_depth: 1,
        run_id: provenant_core::RunId::from_bytes([2; 16]),
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
    for entry in REGISTRY {
        let adapter = entry.adapter.id();
        for text in ["small prompt".to_owned(), "a".repeat(65_536)] {
            let prompt = launch_prompt(
                &text,
                &paths,
                &attempt,
                adapter,
                65_536,
                provenant_records::diagnostic::SettingSource::Configuration,
            )?;
            assert_eq!(prompt.text(), text);
            assert!(prompt.file().is_none());
        }
        let text = format!("Durable output instructions\n\n{}", "a".repeat(1024 * 1024));
        let prompt = launch_prompt(
            &text,
            &paths,
            &attempt,
            adapter,
            65_536,
            provenant_records::diagnostic::SettingSource::Configuration,
        )?;
        if AdapterSpec::get(adapter).is_some_and(|spec| spec.argv_prompt) {
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
    let text = format!("Durable output instructions\n\n{}", "é".repeat(75_000));
    for depth in [1, 2] {
        paths.lane_depth = depth;
        for adapter in ["claude", "codex", "opencode"] {
            for text in [&text, &"a".repeat(1024 * 1024)] {
                let prompt = launch_prompt(
                    text,
                    &paths,
                    &attempt,
                    adapter,
                    65_536,
                    provenant_records::diagnostic::SettingSource::Configuration,
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
fn launch_prompt_spills_only_top_level_argv_and_refuses_conflicting_file()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    for adapter in ["cursor", "agy"] {
        let temp = tempfile::tempdir()?;
        let attempt = canonical(temp.path())?;
        let mut paths = prompt_paths(&attempt)?;
        let text = format!("Durable output instructions\n\n{}", "é".repeat(75_000));
        paths.lane_depth = 2;
        let error = launch_prompt(
            &text,
            &paths,
            &attempt,
            adapter,
            65_536,
            provenant_records::diagnostic::SettingSource::Configuration,
        )
        .err()
        .ok_or("nested argv spill accepted")?;
        assert!(
            matches!(error, LaunchPromptError::NestedArgvTooLarge { .. }),
            "{error}"
        );
        assert!(!attempt.join("prompt.md").exists());

        paths.lane_depth = 1;
        for _ in 0..2 {
            let prompt = launch_prompt(
                &text,
                &paths,
                &attempt,
                adapter,
                65_536,
                provenant_records::diagnostic::SettingSource::Configuration,
            )?;
            assert_eq!(prompt.file(), Some(attempt.join("prompt.md").as_path()));
            assert!(prompt.text().len() < 1024);
            assert_eq!(std::fs::read_to_string(attempt.join("prompt.md"))?, text);
            assert_eq!(
                std::fs::metadata(attempt.join("prompt.md"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::write(attempt.join("prompt.md"), "conflicting prompt")?;
        assert!(
            launch_prompt(
                &text,
                &paths,
                &attempt,
                adapter,
                65_536,
                provenant_records::diagnostic::SettingSource::Configuration
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(attempt.join("prompt.md"))?,
            "conflicting prompt"
        );
    }
    Ok(())
}

#[test]
fn nested_argv_prompt_respects_configured_byte_limit() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let attempt = canonical(temp.path())?;
    let mut paths = prompt_paths(&attempt)?;
    paths.lane_depth = 2;
    let text = "a".repeat(65_537);
    assert!(matches!(
        launch_prompt(
            &text,
            &paths,
            &attempt,
            "cursor",
            65_536,
            provenant_records::diagnostic::SettingSource::Configuration
        ),
        Err(LaunchPromptError::NestedArgvTooLarge { limit: 65_536, .. })
    ));
    let prompt = launch_prompt(
        &text,
        &paths,
        &attempt,
        "cursor",
        131_072,
        provenant_records::diagnostic::SettingSource::Configuration,
    )?;
    assert_eq!(prompt.text(), text);
    assert!(prompt.file().is_none());
    Ok(())
}

#[test]
fn launch_prompt_refuses_symlinked_attempt_ancestors() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = canonical(temp.path())?;
    std::fs::create_dir_all(state.join("actual/attempt"))?;
    std::os::unix::fs::symlink(state.join("actual"), state.join("runs"))?;
    let paths = prompt_paths(&state.join("runs/attempt"))?;
    assert!(
        launch_prompt(
            &"a".repeat(65_537),
            &paths,
            &state,
            "cursor",
            65_536,
            provenant_records::diagnostic::SettingSource::Configuration
        )
        .is_err()
    );
    assert!(!state.join("actual/attempt/prompt.md").exists());
    Ok(())
}
