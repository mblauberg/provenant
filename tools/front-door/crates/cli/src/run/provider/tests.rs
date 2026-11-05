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
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PWD/pnpm.log\"\nif [ \"$(basename \"$PWD\")\" = failing ]; then echo 'fixture stderr tail' >&2; exit 7; fi\nmkdir -p node_modules\ntouch node_modules/.modules.yaml\n",
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
    assert!(error.contains("fixture stderr tail"));
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

#[test]
fn nested_result_policy_uses_a_reliable_route_without_broadening_authority()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_routing::{
        Catalogue, Fallback, ModelId, RouteChoice, RouteIntent, RoutingConfig, RoutingContext,
    };
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let model = ModelId::parse("agy/gemini-3.8-flash")?;
    let route = provenant_routing::resolve(
        &provenant_routing::RouteRequest {
            intent: RouteIntent::default(),
            choice: RouteChoice::Model(model.clone()),
            effort: None,
            confidential: false,
            fallback: Fallback::None,
        },
        &RoutingContext {
            policy_account_hint: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            readiness: None,
            access: &[],
            now: provenant_core::WallTime::from_micros(0)?,
            host: "fixture",
        },
    )?;
    let mut input = super::super::tests::trace_run(
        30,
        provenant_core::AttemptId::from_bytes([31; 16]),
        provenant_core::WallTime::from_micros(0)?,
    )?;
    let mut task = input.request.tasks.remove(0);
    task.account = Some("agy-work".into());
    let policy = config
        .model_policy
        .effective(
            &model,
            None,
            provenant_core::WallTime::from_unix_seconds(0).expect("valid policy instant"),
        )
        .policy;
    let fallback = super::nested_policy_fallback(&task, &route, &policy)?;
    assert!(
        matches!(fallback.route, run::RequestedRoute::Model { ref model, ref effort, .. } if model == "codex/gpt-6-luna" && effort.as_deref() == Some("high"))
    );
    assert_eq!(fallback.authority_digest, task.authority_digest);
    assert_eq!(fallback.access, task.access);
    assert!(fallback.account.is_none());
    task.access.write_roots.push("/fixture/work".into());
    assert_eq!(
        super::nested_policy_fallback(&task, &route, &policy)
            .err()
            .as_deref(),
        Some("model policy fallback requires read-only authority")
    );
    Ok(())
}
use super::*;

#[test]
fn default_writer_launch_does_not_request_publishing() {
    let mut knobs = std::collections::BTreeMap::new();
    assert!(!publishing_requested(true, true, &knobs));
    knobs.insert("lanes.publish".into(), serde_json::json!(true));
    assert!(publishing_requested(true, true, &knobs));
    assert!(!publishing_requested(false, true, &knobs));
    assert!(!publishing_requested(true, false, &knobs));
}

#[test]
fn publishing_is_optional_without_gh_configuration() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let executable = std::env::current_exe()?.canonicalize()?;
    assert_eq!(
        publishing_executable(&home, &|_| Ok(executable.clone())),
        None
    );
    assert!(!home.join(".config/gh").exists());
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
        publishing_executable(&home, &|_| Ok(gh.clone())),
        Some(executable)
    );
    assert_eq!(
        publishing_executable(&home, &|_| Err("not installed".into())),
        None
    );
    std::fs::remove_file(&gh)?;
    assert_eq!(publishing_executable(&home, &|_| Ok(gh.clone())), None);
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
fn launch_prompt_preserves_small_text_and_refuses_more_than_one_mib()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let attempt = canonical(temp.path())?;
    let paths = prompt_paths(&attempt)?;
    for entry in REGISTRY {
        let adapter = entry.adapter.id();
        for text in ["small prompt".to_owned(), "a".repeat(65_536)] {
            let prompt = launch_prompt(&text, &paths, &attempt, adapter)?;
            assert_eq!(prompt.text(), text);
            assert!(prompt.file().is_none());
        }
        let text = format!("Durable output instructions\n\n{}", "a".repeat(1024 * 1024));
        let error = launch_prompt(&text, &paths, &attempt, adapter)
            .err()
            .ok_or("oversized launch accepted")?;
        assert!(
            matches!(error, LaunchPromptError::TooLarge),
            "{adapter}: {error}"
        );
    }
    assert!(!attempt.join("prompt.md").exists());
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
                let prompt = launch_prompt(text, &paths, &attempt, adapter)?;
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
        let error = launch_prompt(&text, &paths, &attempt, adapter)
            .err()
            .ok_or("nested argv spill accepted")?;
        assert!(
            matches!(error, LaunchPromptError::NestedArgvTooLarge),
            "{error}"
        );
        assert!(!attempt.join("prompt.md").exists());

        paths.lane_depth = 1;
        for _ in 0..2 {
            let prompt = launch_prompt(&text, &paths, &attempt, adapter)?;
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
        assert!(launch_prompt(&text, &paths, &attempt, adapter).is_err());
        assert_eq!(
            std::fs::read_to_string(attempt.join("prompt.md"))?,
            "conflicting prompt"
        );
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
    assert!(launch_prompt(&"a".repeat(65_537), &paths, &state, "cursor").is_err());
    assert!(!state.join("actual/attempt/prompt.md").exists());
    Ok(())
}

#[test]
fn launch_prompt_refuses_invalid_input_before_account_resolution()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let attempt = canonical(temp.path())?;
    let paths = prompt_paths(&attempt)?;
    let full = format!("Durable output instructions\n\n{}", "é".repeat(75_000));
    // Refuse invalid input before consulting accounts or executables.
    for entry in REGISTRY {
        let adapter = entry.adapter.id();
        let requested = run::TaskRequest {
            cloud: None,
            context: None,
            extra: Default::default(),
            build_heavy: false,
            title: None,
            base: None,
            head: None,
            source_head: None,
            base_ref: None,
            task_id: provenant_core::TaskId::from_bytes([3; 16]),
            profile: Default::default(),
            prompt_digest: provenant_records::CanonicalV1::file_digest(full.as_bytes()),
            route: run::RequestedRoute::Model {
                extra: Default::default(),
                provider: adapter.into(),
                model: format!("{adapter}/fixture-model"),
                effort: None,
            },
            account: None,
            fallback: vec![],
            authority_digest: provenant_core::Digest::from_bytes([4; 32]),
            writer: None,
            deliver: None,
            provider_timeout_seconds: 20,
            delivery_id: None,
            access: Default::default(),
        };
        let oversized = format!("Durable output instructions\n\n{}", "a".repeat(1024 * 1024));
        let error = prepare_with(
            &Registry::default(),
            &requested,
            &oversized,
            &paths,
            None,
            |_| unreachable!("invalid prompt must not probe executables"),
            None,
        )
        .err()
        .ok_or("oversized launch accepted")?;
        assert!(
            matches!(error, PrepareError::Prompt(LaunchPromptError::TooLarge)),
            "{adapter}: {error}"
        );
        if matches!(adapter, "cursor" | "agy") {
            let mut nested = prompt_paths(&attempt)?;
            nested.lane_depth = 2;
            let error = prepare_with(
                &Registry::default(),
                &requested,
                &full,
                &nested,
                None,
                |_| unreachable!("nested argv spill must not probe executables"),
                None,
            )
            .err()
            .ok_or("nested argv launch accepted")?;
            assert!(
                matches!(
                    error,
                    PrepareError::Prompt(LaunchPromptError::NestedArgvTooLarge)
                ),
                "{error}"
            );
            assert!(!attempt.join("prompt.md").exists());
        }
    }
    Ok(())
}

#[test]
fn detect_failure_distinguishes_probe_directory_from_unknown_identity()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::{ProviderError, accounts::Registration};

    let entry: Registration = serde_json::from_value(serde_json::json!({
        "adapter": "claude",
        "label": "fixture",
        "key": "fixture-key",
        "directory": "/home/test/.claude-fixture",
    }))?;
    let error = ProviderError::ProbeDirectory("fixture host state fault".into());
    let message = detect_failure(&entry, &error);
    assert_eq!(message, format!("claude account fixture: {error}"));
    assert!(!message.contains("re-register"));

    let error = ProviderError::UnknownIdentity;
    assert_eq!(
        detect_failure(&entry, &error),
        format!("claude account fixture: {error}; re-register the signed-in identity")
    );
    Ok(())
}

#[test]
fn candidate_leases_skip_busy_providers_and_share_one_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::Clock;
    use provenant_os::fs::LockMode;
    let temp = tempfile::tempdir()?;
    let _a = provenant_os::provider_fence::acquire(temp.path(), "codex", LockMode::Exclusive)?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0)?,
        provenant_core::MonoTime::from_micros(0),
    );
    let leases = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        None,
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(leases.contains_key("cursor"));
    assert!(!leases.contains_key("codex"));
    assert_eq!(clock.mono_now().as_micros(), 0);
    drop(leases);
    let _b = provenant_os::provider_fence::acquire(temp.path(), "cursor", LockMode::Exclusive)?;
    let leases = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        Some(1),
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(leases.is_empty());
    assert_eq!(clock.mono_now().as_micros(), 1_000_000);
    let leases = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        None,
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(leases.is_empty());
    assert_eq!(clock.mono_now().as_micros(), 31_000_000);
    Ok(())
}

#[test]
fn candidate_lease_wait_honours_cancellation() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{CancelToken, Clock, MonoTime, TestClock, TimeError, WallTime};
    use provenant_os::fs::LockMode;
    struct CancelOnWait<'a> {
        clock: TestClock,
        cancel: &'a CancelToken,
    }
    impl Clock for CancelOnWait<'_> {
        fn wall_now(&self) -> Result<WallTime, TimeError> {
            self.clock.wall_now()
        }
        fn mono_now(&self) -> MonoTime {
            self.clock.mono_now()
        }
        fn wait_until(&self, deadline: MonoTime) -> Result<(), TimeError> {
            self.clock.wait_until(deadline)?;
            self.cancel.cancel();
            Ok(())
        }
    }
    let temp = tempfile::tempdir()?;
    let updating =
        provenant_os::provider_fence::acquire(temp.path(), "codex", LockMode::Exclusive)?;
    let cancel = CancelToken::new();
    let clock = CancelOnWait {
        clock: TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0)),
        cancel: &cancel,
    };
    let error = available_provider_leases(temp.path(), &["codex"], None, &clock, &cancel)
        .err()
        .ok_or("cancelled lease wait admitted")?;
    assert!(matches!(error, ProviderLeaseError::Cancelled));
    assert_eq!(clock.mono_now().as_micros(), 50_000);
    // Cancellation is also checked when the provider becomes available.
    drop(updating);
    assert!(available_provider_leases(temp.path(), &["codex"], None, &clock, &cancel,).is_err());
    Ok(())
}

#[test]
fn executable_discovery_after_wait_observes_the_completed_update()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{Clock, DurationMicros};
    use provenant_os::fs::LockMode;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("cursor-agent");
    let host = run::HostLaunchContext {
        configuration_root: temp.path().into(),
        cwd: temp.path().into(),
        executables: Default::default(),
        executable_search_path: vec![temp.path().into()],
        git: "/usr/bin/git".into(),
        codex_home: None,
        cargo_build_jobs: None,
        #[cfg(feature = "fixture")]
        fixture_state_write: false,
    };
    assert!(host_executable(&host, "cursor").is_err());
    let updating =
        provenant_os::provider_fence::acquire(temp.path(), "cursor", LockMode::Exclusive)?;
    let clock = provenant_os::clock::SystemClock::new();
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let executable = &executable;
        let clock = &clock;
        let update = scope.spawn(move || -> Result<(), String> {
            clock
                .wait_until(
                    clock
                        .mono_now()
                        .checked_add(DurationMicros::from_micros(50_000))
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            std::fs::write(executable, "new provider").map_err(|e| e.to_string())?;
            std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            drop(updating);
            Ok(())
        });
        let leases = available_provider_leases(
            temp.path(),
            &["cursor"],
            Some(2),
            clock,
            &provenant_core::CancelToken::new(),
        )?;
        assert!(leases.contains_key("cursor"));
        assert_eq!(
            host_executable(&host, "cursor")?,
            executable.canonicalize()?
        );
        assert_eq!(std::fs::read_to_string(executable)?, "new provider");
        update.join().expect("update thread")?;
        Ok(())
    })
}

/// Initialise a repository with one commit in `project` under a minimal explicit
/// environment, and return the canonical Git executable it used.
fn init_fixture_repository(
    executor: &provenant_os::exec::SystemExecutor,
    project: &std::path::Path,
    git_home: &std::path::Path,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, Termination};
    use std::time::Duration;
    let git = provenant_os::git::git_executable()
        .ok_or("git unavailable")?
        .canonicalize()?;
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["commit", "-q", "--allow-empty", "-m", "Initial"],
    ] {
        let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.into_iter().map(Into::into))
            .collect();
        let mut env = vec![
            ("HOME".into(), git_home.as_os_str().to_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        env.extend(provenant_os::fixture::fixture_git_settings());
        let mut options = ExecOptions::new(&argv, project, Duration::from_secs(5));
        options.env = Some(&env);
        let output = executor.execute(options)?;
        assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
    }
    Ok(git)
}

#[test]
fn an_account_mismatch_is_cleared_only_by_a_check_that_names_the_account() {
    for adapter in ["codex", "cursor", "opencode"] {
        assert!(super::preparation::login_check_can_clear(adapter, false));
        assert!(!super::preparation::login_check_can_clear(adapter, true));
    }
    assert!(super::preparation::login_check_can_clear("agy", true));
}

#[test]
fn opencode_stale_auth_prepares_with_an_unverified_warning()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, CancelToken, Clock, Digest, OperationId, RunId, TaskId};
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::{
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        time::Duration,
    };
    const CHILD: &str = "PROVENANT_TEST_OPENCODE_AUTH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let argv = [
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::provider::tests::opencode_stale_auth_prepares_with_an_unverified_warning".into(),
            "--nocapture".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !matches!(
                    key.to_str(),
                    Some(
                        "XDG_CONFIG_HOME"
                            | "XDG_DATA_HOME"
                            | "XDG_CACHE_HOME"
                            | "XDG_STATE_HOME"
                            | "OPENCODE_DB"
                            | "OPENCODE_CONFIG"
                            | "OPENCODE_CONFIG_DIR"
                            | "OPENCODE_CONFIG_CONTENT"
                    )
                ) && key != provenant_core::RUN_STATE_DIRECTORY_ENV
                    && key != CHILD
            })
            .collect();
        env.push((CHILD.into(), "1".into()));
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(60));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let out = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        assert!(!out.timed_out, "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr.bytes)
                .contains("OpenCode login status is unverified")
        );
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("repo");
    let assets = base.join("release");
    let git_home = base.join("git-home");
    for path in [&home, &state, &project, &assets, &git_home] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let executor = SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = provenant_confinement::UtilityRuntime::new(&git, &git_home)?;
    let cancel = CancelToken::new();
    let repo = utility.open_repo(
        &executor,
        &project,
        provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
        &cancel,
    )?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let executable = assets.join("opencode/bin/opencode");
    std::fs::create_dir_all(executable.parent().ok_or("provider bin")?)?;
    std::fs::write(&executable, b"#!/bin/sh\nprintf 'opencode 2.2.0\\n'\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let now = provenant_os::clock::SystemClock::new().unix_seconds();
    let account = provenant_quota::AccountRecord::empty(provenant_quota::AccountScope {
        adapter: "opencode".into(),
        account_key: "go".into(),
        label: "go".into(),
    });
    let mut readiness = provenant_quota::ReadinessFile::default();
    readiness.sync_probe_result(
        &account,
        provenant_quota::LOCAL_HOST,
        now - 1,
        300,
        Some(provenant_quota::ReadinessReason::SignedOut),
    )?;
    let readiness_dir = state.join("readiness/go");
    std::fs::create_dir_all(&readiness_dir)?;
    for dir in [state.join("readiness"), readiness_dir.clone()] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let readiness_path = readiness_dir.join("opencode.json");
    std::fs::write(&readiness_path, serde_json::to_vec(&readiness)?)?;
    std::fs::set_permissions(&readiness_path, std::fs::Permissions::from_mode(0o600))?;
    let run_id = RunId::from_bytes([2; 16]);
    let attempt_id = AttemptId::from_bytes([1; 16]);
    let attempt = state.join(format!("runs/{run_id}/attempts/{attempt_id}"));
    let paths = run::LaunchPaths {
        attempt_id,
        operation_id: OperationId::parse("opencode-auth-fixture")?,
        task_index: 0,
        lane_depth: 1,
        run_id,
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        provider_state: attempt.join("provider-state"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
    };
    for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    let prompt = "fake OpenCode login recovery";
    let task = run::TaskRequest {
        cloud: None,
        context: None,
        extra: Default::default(),
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        task_id: TaskId::from_bytes([3; 16]),
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "opencode".into(),
            model: "opencode/opencode-go/deepseek-v4.1-flash".into(),
            effort: None,
        },
        account: Some("go".into()),
        fallback: vec![],
        authority_digest: Digest::from_bytes([4; 32]),
        writer: None,
        deliver: None,
        provider_timeout_seconds: 20,
        delivery_id: None,
        access: Default::default(),
    };
    let registry = Registry {
        roots: Some(&roots),
        home: Some(&home),
        assets: Some(&assets),
        cwd: Some(&project),
        state_root: Some(&state),
        project_id: Some(binding.project_id()),
        host: None,
        cancel: Some(&cancel),
    };
    let prepared = prepare_with(
        &registry,
        &task,
        prompt,
        &paths,
        None,
        |name| {
            if name == "opencode" {
                Ok(executable.clone())
            } else {
                Err("fixture provider unavailable".into())
            }
        },
        None,
    )?;
    assert_eq!(prepared.route.provider.as_deref(), Some("opencode"));
    assert_eq!(prepared.route.account_key.as_deref(), Some("go"));
    // Readiness remains recorded; only the current launch treats it as unknown.
    let scope = provenant_quota::ReadinessScope {
        host: provenant_quota::LOCAL_HOST.into(),
        adapter: "opencode".into(),
        account_key: "go".into(),
        route: "opencode/opencode-go/deepseek-v4.1-flash".into(),
    };
    assert_eq!(
        crate::quota::read_readiness(&state, &scope, now).reason,
        Some(provenant_quota::ReadinessReason::SignedOut)
    );
    Ok(())
}

#[test]
fn writer_plan_stages_scratch_in_private_temporary_directory()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime, worktrees};
    use provenant_core::{
        AttemptId, CancelToken, Clock, Digest, OperationId, RunId, TaskId, WaitBudget,
    };
    use provenant_os::clock::SystemClock;
    use provenant_os::exec::ExecOptions;
    use provenant_os::exec::Executor;
    use provenant_os::exec::SystemExecutor;
    use provenant_os::exec::Termination;
    use std::{
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        time::Duration,
    };

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("repo");
    let assets = base.join("release");
    let git_home = base.join("git-home");
    for path in [&home, &state, &project, &assets, &git_home] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let executor = SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = UtilityRuntime::new(&git, &git_home)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let repo = utility.open_repo(&executor, &project, budget, &cancel)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let worktree_state = state
        .join("worktrees")
        .join(binding.project_id().to_string());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&worktree_state)?;
    let project_state = provenant_os::fs::Directory::open_anchor(&worktree_state)?;
    let clock = SystemClock::new();
    worktrees::create(
        &worktrees::WorktreeContext {
            home: &home,
            repo: &repo,
            binding: &binding,
            project_state: &project_state,
            executor: &executor,
            utility: &utility,
            workers: &worktrees::SystemWorkerObserver,
            clock: &clock,
            utility_budget: budget,
            mutation_budget: budget,
            lock_budget: WaitBudget::cli(clock.mono_now(), 5)?,
        },
        worktrees::WorktreeCreate {
            branch: "writer".into(),
            base: Some("HEAD".into()),
        },
        &cancel,
    )?;
    // Cursor's local account needs only --version. Keep its executable
    // inside the fixture assets, independent of host packages and PATH.
    let executable = assets.join("cursor/bin/agent");
    std::fs::create_dir_all(executable.parent().ok_or("provider bin")?)?;
    std::fs::write(&executable, b"#!/bin/sh\nif [ \"$1\" = status ]; then printf 'Logged in successfully\\n'; else printf 'fixture-cursor 1.0\\n'; fi\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let run_id = RunId::from_bytes([2; 16]);
    let attempt_id = AttemptId::from_bytes([1; 16]);
    let attempt = state.join(format!("runs/{run_id}/attempts/{attempt_id}"));
    let paths = run::LaunchPaths {
        attempt_id,
        operation_id: OperationId::parse("writer-temporary")?,
        task_index: 0,
        lane_depth: 1,
        run_id,
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        provider_state: attempt.join("provider-state"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
    };
    for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    let prompt = "é".repeat(75_000);
    let review_oid = run::resolve_git_base(&git, &project, "HEAD")?;
    let task = run::TaskRequest {
        cloud: None,
        context: None,
        extra: Default::default(),
        build_heavy: false,
        title: None,
        base: Some(review_oid.clone()),
        head: Some(review_oid.clone()),
        source_head: None,
        base_ref: None,
        task_id: TaskId::from_bytes([3; 16]),
        profile: provenant_records::run::HarnessProfile {
            knobs: [("lanes.publish".into(), serde_json::json!(false))].into(),
            ..Default::default()
        },
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "cursor".into(),
            model: "cursor/gpt-5.3-codex-low".into(),
            effort: None,
        },
        account: None,
        fallback: vec![run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-5.3-codex".into()),
            ..Default::default()
        }],
        authority_digest: Digest::from_bytes([4; 32]),
        writer: Some(run::WriterRequest {
            extra: Default::default(),
            project: project.clone(),
            worktree: project.join(".worktrees/writer"),
        }),
        deliver: None,
        provider_timeout_seconds: 20,
        delivery_id: None,
        access: Default::default(),
    };
    let registry = Registry {
        roots: Some(&roots),
        home: Some(&home),
        assets: Some(&assets),
        cwd: Some(&project),
        state_root: Some(&state),
        project_id: Some(binding.project_id()),
        host: None,
        cancel: Some(&cancel),
    };
    // A recorded sign-out followed by a healthy account-scoped status probe
    // must permit preparation without waiting for the quota backoff.
    let now = clock.unix_seconds();
    let stale_scope = provenant_quota::AccountScope {
        adapter: "cursor".into(),
        account_key: "local".into(),
        label: "default".into(),
    };
    let stale_account = provenant_quota::AccountRecord::empty(stale_scope);
    let mut readiness = provenant_quota::ReadinessFile::default();
    readiness.sync_probe_result(
        &stale_account,
        provenant_quota::LOCAL_HOST,
        now - 1,
        300,
        Some(provenant_quota::ReadinessReason::SignedOut),
    )?;
    let readiness_dir = state.join("readiness/local");
    std::fs::create_dir_all(&readiness_dir)?;
    for dir in [state.join("readiness"), readiness_dir.clone()] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let readiness_path = readiness_dir.join("cursor.json");
    std::fs::write(&readiness_path, serde_json::to_vec(&readiness)?)?;
    std::fs::set_permissions(&readiness_path, std::fs::Permissions::from_mode(0o600))?;
    let auth_paths = run::LaunchPaths {
        scratch: attempt.join("auth-scratch"),
        provider_state: attempt.join("auth-state"),
        temporary: attempt.join("auth-tmp"),
        attempt_id: paths.attempt_id,
        operation_id: paths.operation_id.clone(),
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        run_id: paths.run_id,
        deadline_unix_ms: paths.deadline_unix_ms,
        temporary_alias: None,
    };
    for path in [
        &auth_paths.scratch,
        &auth_paths.provider_state,
        &auth_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let recovered = prepare_with(
        &registry,
        &task,
        "login recovery fixture",
        &auth_paths,
        None,
        |name| {
            if name == "cursor" {
                Ok(executable.clone())
            } else {
                Err("fixture provider unavailable".into())
            }
        },
        None,
    )?;
    assert_eq!(recovered.route.provider.as_deref(), Some("cursor"));
    drop(recovered);
    std::fs::remove_file(readiness_path)?;
    std::fs::remove_file(attempt.join("availability.json"))?;

    // A clean writer that moved after graph source preparation must be
    // refused before provider probing or any launch artefact is written.
    let mut stale_source = task.clone();
    stale_source.head = Some("a".repeat(40));
    let error = prepare_with(
        &registry,
        &stale_source,
        &prompt,
        &paths,
        None,
        |_| Err("provider lookup must follow source validation".into()),
        None,
    )
    .err()
    .ok_or("stale writer source was accepted")?;
    assert!(
        error
            .to_string()
            .contains("differs from pinned source commit"),
        "{error}"
    );
    assert!(error.to_string().contains(&"a".repeat(40)), "{error}");
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = run::RunStore::open(
        &run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: binding.project_id(),
            actor_id: provenant_core::ActorId::from_bytes([5; 16]),
            read_prompts: true,
        },
        &call,
    )?;
    store.create(
        &run::PreparedRun {
            run_id,
            operation_id: paths.operation_id.clone(),
            request: run::CanonicalRequest {
                extra: Default::default(),
                child_attempt_id_version: None,
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: vec![task.clone()],
                concurrency: 1,
                parent: None,
                lane_depth: Some(1),
                parent_attempt: None,
            },
            attempts: vec![run::NewAttempt {
                task_id: task.task_id,
                attempt_id,
                prompt: prompt.clone(),
            }],
            created_at: provenant_core::WallTime::from_micros(100)?,
        },
        &call,
    )?;
    // Exercise the actual adapter, sandbox compiler and fixture executable in both lane modes.
    #[cfg(target_os = "macos")]
    {
        use provenant_confinement::permissions::{
            AccessMode, AnchoredDirectory, Anchors, BackendContext, prepare_writer_directories,
            required_anchors, system_backend,
        };
        let skill_root = home.join(".gemini/skills");
        std::fs::create_dir_all(&skill_root)?;
        // Skill discovery and Rust discovery omit the same canonical root.
        // Repeated read/exec grants for that root collapse within Toolchain.
        std::os::unix::fs::symlink(&skill_root, skill_root.join("bin"))?;
        std::os::unix::fs::symlink(&skill_root, home.join(".cargo"))?;
        std::fs::create_dir_all(home.join(".rustup/toolchains"))?;
        let login = home.join("Library/Keychains/login.keychain-db");
        std::fs::create_dir_all(login.parent().unwrap())?;
        std::fs::write(&login, b"fixture")?;
        let fake_agy = assets.join("agy");
        std::fs::write(&fake_agy, b"#!/bin/sh\nprintf 'agy 1.0.0\\n'\n")?;
        std::fs::set_permissions(&fake_agy, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(state.join("config.toml"), "schema_version = 1\n")?;
        let backend = system_backend(&BackendContext {
            executor: &executor,
            cancel: &cancel,
            env: &[],
        });
        if let Err(error) = &backend {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "agy fixture execution requires native sandbox: {error}"
            ))?;
        }
        for (n, write) in [(7u8, true), (8u8, false)] {
            let mut request = task.clone();
            request.route = run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "agy".into(),
                model: "agy/gemini-3.8-flash".into(),
                effort: None,
            };
            request.fallback.clear();
            if !write {
                request.writer = None;
            }
            let attempt_id = AttemptId::from_bytes([n; 16]);
            let attempt = state.join(format!("runs/{run_id}/attempts/{attempt_id}"));
            let paths = run::LaunchPaths {
                attempt_id,
                operation_id: OperationId::parse(&format!("agy-fixture-{n}"))?,
                task_index: 0,
                lane_depth: 1,
                run_id,
                deadline_unix_ms: u64::MAX,
                scratch: attempt.join("scratch"),
                provider_state: attempt.join("provider-state"),
                temporary: attempt.join("tmp"),
                temporary_alias: None,
            };
            for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
                std::fs::create_dir_all(path)?;
            }
            let prepared = prepare_with(
                &registry,
                &request,
                "fixture",
                &paths,
                None,
                |_| Ok(fake_agy.clone()),
                None,
            )?;
            let skills = &prepared.route.extra["user_skills"];
            assert_eq!(skills["exposed"], true);
            assert!(
                skills["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning["code"] == "user_skill_skipped"
                        && warning["reason"] == "cycle")
            );
            let warnings = prepared.route.extra["discovery_warnings"]
                .as_array()
                .unwrap();
            if write {
                assert!(warnings.iter().any(|warning| {
                    warning
                        .as_str()
                        .is_some_and(|text| text.contains("~/.gemini/skills"))
                }));
            } else {
                assert!(warnings.is_empty());
            }
            let confinement = prepared.confinement.as_ref().ok_or("missing confinement")?;
            assert!(
                !confinement
                    .profile
                    .spec
                    .runtime_reads
                    .iter()
                    .any(|grant| grant.path == skill_root)
            );
            let writer = confinement
                .writer
                .as_ref()
                .map(|writer| (&writer.worktree, &writer.branch));
            if let (Some((worktree, branch)), AccessMode::WorktreeWrite(authority)) =
                (writer, &confinement.plan.permissions().mode)
            {
                prepare_writer_directories(worktree, branch, authority, &home)?;
            }
            let directories =
                required_anchors(confinement.plan.permissions(), &confinement.profile, writer)
                    .into_iter()
                    .map(AnchoredDirectory::open_canonical)
                    .collect::<Result<Vec<_>, _>>()?;
            let anchors = Anchors {
                home: &home,
                directories: &directories,
                worktree: writer,
            };
            let compiled = provenant_confinement::permissions::snapshot::compile(
                confinement.plan.permissions(),
                &confinement.profile,
                &anchors,
            )?;
            if backend.is_err() {
                continue;
            }
            let argv = compiled.command(&prepared.argv, &attempt_id)?;
            let mut options = ExecOptions::new(&argv, &prepared.cwd, Duration::from_secs(10));
            options.env = Some(&prepared.environment);
            let result = executor.execute(options)?;
            assert_eq!(result.termination, Termination::Exit(0), "{result:?}");
            assert!(String::from_utf8_lossy(&result.stdout.bytes).contains("agy 1.0.0"));
        }
        std::fs::remove_file(home.join(".cargo"))?;
    }
    std::fs::write(
        state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.accounts.entries.unrelated]\nadapter = 'claude'\nlabel = 'other'\nkey = 'fixture-other'\ndirectory = '{}'\n",
            home.join(".claude-other").display(),
        ),
    )?;
    let full = format!("Durable output instructions\n\n{prompt}");
    let failed_update = provenant_providers::update::check(
        &state,
        "cursor",
        &executable,
        &provenant_config::ProviderSettings {
            update: provenant_config::ProviderUpdate::Command {
                command: vec!["/usr/bin/false".into()],
            },
            ..Default::default()
        },
        &executor,
        100,
    )?
    .ok_or("no failed update record")?;
    assert_eq!(failed_update.outcome, "failed");
    let _updating = provenant_os::provider_fence::acquire(
        &state,
        "codex",
        provenant_os::fs::LockMode::Exclusive,
    )?;
    // A failed diagnostic write must not fail an otherwise valid launch.
    std::fs::create_dir(attempt.join("availability.json"))?;
    // Check configuration precedence through real preparation and adapter grants.
    let skills = home.join(".cursor/skills");
    let skill_target = base.join("installed-skill");
    std::fs::create_dir_all(&skills)?;
    std::fs::create_dir_all(&skill_target)?;
    std::os::unix::fs::symlink(&skill_target, skills.join("linked"))?;
    let project_id = binding.project_id().to_string();
    for (settings, expected, source) in [
        (String::new(), true, "host"),
        (
            "[domains.lanes]\nuser_skills = 'off'\n".into(),
            false,
            "host",
        ),
        (
            format!(
                "[domains.lanes]\nuser_skills = 'discover'\n[domains.lanes.projects.{project_id}]\nuser_skills = 'off'\n"
            ),
            false,
            "project",
        ),
        (
            format!(
                "[domains.lanes]\nuser_skills = 'off'\n[domains.lanes.projects.{project_id}]\nuser_skills = 'discover'\n"
            ),
            true,
            "project",
        ),
        (
            format!(
                "[domains.lanes.projects.{project_id}]\nuser_skills = 'discover'\n[[domains.routing.model_policy.rules]]\nadapter = 'cursor'\n[domains.routing.model_policy.rules.policy.lane]\nuser_skills = 'off'\n"
            ),
            false,
            "route",
        ),
        (
            format!(
                "[domains.lanes.projects.{project_id}]\nuser_skills = 'off'\n[[domains.routing.model_policy.rules]]\nadapter = 'cursor'\n[domains.routing.model_policy.rules.policy.lane]\nuser_skills = 'discover'\n"
            ),
            true,
            "route",
        ),
    ] {
        std::fs::write(
            state.join("config.toml"),
            format!("schema_version = 1\n{settings}"),
        )?;
        let prepared = prepare_with(
            &registry,
            &task,
            "skill fixture",
            &paths,
            None,
            |_| Ok(executable.clone()),
            None,
        )?;
        let confinement = prepared.confinement.as_ref().ok_or("confinement")?;
        let reads = &confinement.profile.spec.runtime_reads;
        let writes = provenant_confinement::write_grants(
            confinement.plan.permissions(),
            &confinement.profile,
        );
        for path in [&skills, &skill_target] {
            assert!(
                !reads.iter().any(|grant| grant.path == *path),
                "{settings}: {path:?}"
            );
            assert!(
                !writes
                    .iter()
                    .any(|grant| path.starts_with(&grant.path) || grant.path.starts_with(path)),
                "write grant for {path:?}: {settings}"
            );
        }
        let receipt = &prepared.route.extra["user_skills"];
        // The linked target is an empty, safe directory: discovery stages
        // an empty tree without granting its original location.
        assert_eq!(receipt["exposed"], expected, "{settings}");
        assert_eq!(receipt["staged_file_count"], 0);
        assert!(receipt["warnings"].as_array().unwrap().is_empty());
        if source == "route" {
            assert!(receipt["source"]["route_policy"].is_array());
        } else {
            let key = if source == "project" {
                format!("domains.lanes.projects.{project_id}.user_skills")
            } else {
                "domains.lanes.user_skills".into()
            };
            assert_eq!(receipt["source"]["key"], key);
        }
    }
    std::fs::write(
        state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.accounts.entries.unrelated]\nadapter = 'claude'\nlabel = 'other'\nkey = 'fixture-other'\ndirectory = '{}'\n",
            home.join(".claude-other").display()
        ),
    )?;
    let launch = prepare_with(
        &registry,
        &task,
        &full,
        &paths,
        None,
        |adapter| {
            assert_eq!(adapter, "cursor", "unrelated adapter was probed");
            Ok(executable.clone())
        },
        None,
    )?;
    assert!(
        launch
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_HEAD" && value == review_oid.as_str())
    );
    assert!(
        launch
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_BASE" && value == review_oid.as_str())
    );
    // The writer plan the CLI prepares must leave the build slot directory
    // out of reach once an adapter adds its grants: a lane cannot create,
    // unlink or replace a slot file. Cursor never runs `LaneRuntime::grant`
    // or sets up build slots, so Codex, which does both, takes the plan.
    let confinement = launch.confinement.as_ref().ok_or("confinement")?;
    let cargo = home.join(".cargo/bin/cargo");
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        std::fs::create_dir_all(home.join(path))?;
    }
    std::fs::write(home.join(".rustup/settings.toml"), "")?;
    std::fs::write(&cargo, b"#!/bin/sh\n")?;
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o700))?;
    let codex_route = provenant_records::ResolvedRoute {
        extra: Default::default(),
        adapter: "codex".into(),
        model: "fixture-model".into(),
        effort: None,
        catalogue_revision: "fixture".into(),
        account_key: Some("local".into()),
        host: "fixture".into(),
        alternatives: Default::default(),
        exclusions: Vec::new(),
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 100,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture".into(),
        },
        pool_selection: None,
    };
    let codex_account = provenant_providers::identity::RegisteredAccount::new(
        "codex",
        "local",
        home.join(".codex"),
    )?;
    let codex_prompt = provenant_providers::Prompt::new("grants".into());
    let codex_runtime = assets.join("provenant");
    let mut codex = provenant_providers::Adapter::prepare(
        &provenant_providers::codex::Codex,
        provenant_providers::PrepareInput {
            command: None,
            route: &codex_route,
            confined: &confinement.plan,
            prompt: &codex_prompt,
            continuation: None,
            grant_digest: "fixture",
            executable: &executable,
            home: &home,
            user: "fixture".as_ref(),
            host_var: &|_| None,
            skill_stage_max_bytes: 16 * 1024 * 1024,
            account: Some(&codex_account),
            token: None,
            cwd: &project.join(".worktrees/writer"),
            git_context: false,
            nonce: [9; 16],
            provenant: provenant_providers::LaneRuntime {
                executable: &codex_runtime,
                git: &git,
                state: &state,
                child_state: &state.join("children"),
                home: &home,
                depth: 1,
                attempt: attempt_id,
                assets: &assets,
            },
        },
    )?;
    assert!(
        codex.env.iter().any(|(key, _)| key == "CARGO_TARGET_DIR"),
        "the lane builds into its own target"
    );
    configure_build_slots(
        &mut codex,
        &provenant_config::LaneBuildSettings::default(),
        &home,
        &confinement.plan.permissions().scratch.path,
        &codex_runtime,
        &state,
    )?;
    let slots = state.join(provenant_os::build_slots::DIRECTORY.join("/"));
    assert!(
        codex
            .profile
            .spec
            .runtime_reads
            .iter()
            .any(|grant| grant.path == slots && grant.scope == PathScope::Subtree),
        "build slots were set up for this lane"
    );
    let grants =
        provenant_confinement::write_grants(confinement.plan.permissions(), &codex.profile);
    assert!(!grants.is_empty(), "the profile lists its write grants");
    // The shim's wait marker must be writable by the lane, or a sandboxed
    // wait is never shown.
    let shim = codex
        .setup
        .iter()
        .find_map(|step| match step {
            provenant_providers::SetupStep::WriteExecutable { path, bytes }
                if path.ends_with("bin/cargo") =>
            {
                String::from_utf8(bytes.clone()).ok()
            }
            _ => None,
        })
        .ok_or("cargo shim")?;
    let marker = confinement
        .plan
        .permissions()
        .scratch
        .path
        .join(provenant_os::build_slots::WAIT_MARKER);
    assert!(
        shim.contains(&marker.to_string_lossy().into_owned()),
        "{shim}"
    );
    assert!(
        grants
            .iter()
            .any(|grant| grant.scope == PathScope::Subtree && marker.starts_with(&grant.path)),
        "no write grant covers the wait marker {}",
        marker.display()
    );
    for grant in &grants {
        let reaches = match grant.scope {
            PathScope::Exact => grant.path == slots,
            PathScope::Subtree => slots.starts_with(&grant.path),
        };
        assert!(
            !reaches,
            "{:?} grant {} reaches {}",
            grant.source,
            grant.path.display(),
            slots.display()
        );
    }
    // Exercise the host project policy through preparation and the actual
    // generated shim, with a fake gh so no GitHub access is needed.
    std::fs::create_dir_all(home.join(".config/gh"))?;
    let gh = assets.join("gh");
    std::fs::write(&gh, b"#!/bin/sh\nprintf 'forwarded\\n'\n")?;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700))?;
    let policy_dir = state
        .join("projects")
        .join(binding.project_id().to_string());
    std::fs::create_dir_all(&policy_dir)?;
    std::fs::write(policy_dir.join("github.json"), r#"{"deny_labels":["ci"]}"#)?;
    let publishing_paths = run::LaunchPaths {
        attempt_id: paths.attempt_id,
        operation_id: paths.operation_id.clone(),
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        run_id: paths.run_id,
        deadline_unix_ms: paths.deadline_unix_ms,
        scratch: attempt.join("publishing-scratch"),
        provider_state: attempt.join("publishing-provider-state"),
        temporary: attempt.join("publishing-tmp"),
        temporary_alias: None,
    };
    for path in [
        &publishing_paths.scratch,
        &publishing_paths.provider_state,
        &publishing_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let mut publishing_task = task.clone();
    publishing_task
        .profile
        .knobs
        .insert("lanes.publish".into(), serde_json::json!(true));
    let _publishing = prepare_with(
        &registry,
        &publishing_task,
        "publishing policy fixture",
        &publishing_paths,
        None,
        |name| {
            Ok(if name == "gh" {
                gh.clone()
            } else {
                executable.clone()
            })
        },
        None,
    )?;
    let worktree = &publishing_task
        .writer
        .as_ref()
        .ok_or("writer missing")?
        .worktree;
    // The runtime populates this private Git directory after preparation.
    let private_common = publishing_paths.temporary.join("candidate-0/git-common");
    std::fs::create_dir_all(private_common.join("objects"))?;
    std::fs::create_dir_all(private_common.join("refs"))?;
    std::fs::write(private_common.join("HEAD"), b"ref: refs/heads/writer\n")?;
    std::fs::copy(project.join(".git/config"), private_common.join("config"))?;
    let private_objects = publishing_paths.temporary.join("candidate-0/git-objects");
    std::fs::create_dir_all(private_objects.join("info"))?;
    std::fs::write(
        private_objects.join("info/alternates"),
        format!("{}\n", project.join(".git/objects").display()),
    )?;
    let shared_config = std::fs::read(project.join(".git/config"))?;
    let shared_index = std::fs::read(project.join(".git/index"))?;
    let shared_refs = std::fs::read_dir(project.join(".git/refs/heads"))?.count();

    std::fs::write(
        worktree.join("private-blob-fixture"),
        b"publishing private object fixture\n",
    )?;
    let git_shim = publishing_paths.temporary.join("candidate-0/bin/git");
    let env = [
        ("HOME".into(), git_home.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    let run_git = |args: &[&str]| {
        let argv: Vec<_> = std::iter::once(git_shim.as_os_str().to_owned())
            .chain(args.iter().map(Into::into))
            .collect();
        let mut options = ExecOptions::new(&argv, worktree, Duration::from_secs(5));
        options.env = Some(&env);
        executor.execute(options)
    };
    let oid = run_git(&["hash-object", "private-blob-fixture"])?;
    assert_eq!(oid.termination, Termination::Exit(0), "{oid:?}");
    let oid = std::str::from_utf8(&oid.stdout.bytes)?.trim();
    let added = run_git(&["add", "private-blob-fixture"])?;
    assert_eq!(added.termination, Termination::Exit(0), "{added:?}");
    let object = format!("{}/{}", &oid[..2], &oid[2..]);
    assert!(
        publishing_paths
            .temporary
            .join("candidate-0/git-objects")
            .join(&object)
            .is_file()
    );
    assert!(!project.join(".git/objects").join(&object).exists());
    assert_eq!(std::fs::read(project.join(".git/config"))?, shared_config);
    assert_eq!(std::fs::read(project.join(".git/index"))?, shared_index);
    assert_eq!(
        std::fs::read_dir(project.join(".git/refs/heads"))?.count(),
        shared_refs
    );
    let credential = run_git(&["config", "--get", "credential.helper"])?;
    assert_eq!(credential.termination, Termination::Exit(0));
    assert!(String::from_utf8_lossy(&credential.stdout.bytes).contains("auth git-credential"));

    let shim = publishing_paths.temporary.join("candidate-0/bin/gh");
    let run_shim = |label: &str| {
        use provenant_os::exec::{ExecOptions, Executor};
        let argv = [
            shim.as_os_str().to_owned(),
            "pr".into(),
            "create".into(),
            "-l".into(),
            label.into(),
        ];
        executor.execute(ExecOptions::new(
            &argv,
            &attempt,
            std::time::Duration::from_secs(5),
        ))
    };
    let denied = run_shim("ci")?;
    assert_eq!(denied.termination, provenant_os::exec::Termination::Exit(1));
    assert_eq!(denied.stderr.bytes, b"github.deny_labels\n");
    let allowed = run_shim("docs")?;
    assert_eq!(
        allowed.termination,
        provenant_os::exec::Termination::Exit(0)
    );
    assert_eq!(allowed.stdout.bytes, b"forwarded\n");
    // The preferred provider is updating, so preparation must select the
    // explicit fallback rather than just omit an unrelated busy adapter.
    let mut fallback_task = task.clone();
    let fallback_prompt = "Busy preferred provider fallback";
    fallback_task.prompt_digest =
        provenant_records::CanonicalV1::file_digest(fallback_prompt.as_bytes());
    fallback_task.route = run::RequestedRoute::Model {
        extra: Default::default(),
        provider: "codex".into(),
        model: "codex/gpt-5.3-codex".into(),
        effort: None,
    };
    fallback_task.fallback = vec![run::RouteRecord {
        provider: Some("cursor".into()),
        model: Some("cursor/gpt-5.3-codex-low".into()),
        ..Default::default()
    }];
    let fallback_paths = run::LaunchPaths {
        attempt_id: paths.attempt_id,
        operation_id: paths.operation_id.clone(),
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        run_id: paths.run_id,
        deadline_unix_ms: paths.deadline_unix_ms,
        scratch: attempt.join("fallback-scratch"),
        provider_state: attempt.join("fallback-provider-state"),
        temporary: attempt.join("fallback-tmp"),
        temporary_alias: None,
    };
    for path in [
        &fallback_paths.scratch,
        &fallback_paths.provider_state,
        &fallback_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let fallback_launch = prepare_with(
        &registry,
        &fallback_task,
        fallback_prompt,
        &fallback_paths,
        None,
        |adapter| {
            assert_eq!(adapter, "cursor", "busy preferred provider was probed");
            Ok(executable.clone())
        },
        None,
    )?;
    assert_eq!(fallback_launch.route.provider.as_deref(), Some("cursor"));
    assert_eq!(
        fallback_launch.route.model.as_deref(),
        Some("cursor/gpt-5.3-codex-low")
    );
    // A named pool redraws after an adapter disappears during local preparation.
    // The first two lookups establish availability; the third refuses the first
    // selected member, and the next lookup admits the remaining member.
    let saved_config = std::fs::read(state.join("config.toml"))?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[domains.routing]\nsettlement_retries=1\n[[domains.routing.pools.redraw-fixture]]\nmodel='cursor/gpt-5.3-codex-low'\nweight=1\nefforts=[]\n[[domains.routing.pools.redraw-fixture]]\nmodel='cursor/grok-4.7'\nweight=1\nefforts=[]\n",
    )?;
    let mut redraw_task = task.clone();
    redraw_task.route = run::RequestedRoute::Named {
        extra: Default::default(),
        name: "redraw-fixture".into(),
        effort: None,
    };
    redraw_task.fallback.clear();
    let redraw_prompt = "pool redraw fixture";
    redraw_task.prompt_digest =
        provenant_records::CanonicalV1::file_digest(redraw_prompt.as_bytes());
    let redraw_paths = run::LaunchPaths {
        scratch: attempt.join("redraw-scratch"),
        provider_state: attempt.join("redraw-provider-state"),
        temporary: attempt.join("redraw-tmp"),
        temporary_alias: None,
        attempt_id: fallback_paths.attempt_id,
        operation_id: fallback_paths.operation_id.clone(),
        task_index: fallback_paths.task_index,
        lane_depth: fallback_paths.lane_depth,
        run_id: fallback_paths.run_id,
        deadline_unix_ms: fallback_paths.deadline_unix_ms,
    };
    for path in [
        &redraw_paths.scratch,
        &redraw_paths.provider_state,
        &redraw_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let lookup_count = std::cell::Cell::new(0);
    let redraw_launch = prepare_with(
        &registry,
        &redraw_task,
        redraw_prompt,
        &redraw_paths,
        None,
        |adapter| {
            assert_eq!(adapter, "cursor");
            let count = lookup_count.get() + 1;
            lookup_count.set(count);
            if count == 3 {
                Err("fixture executable disappeared".into())
            } else {
                Ok(executable.clone())
            }
        },
        None,
    )?;
    assert_eq!(redraw_launch.skipped_routes.len(), 1);
    assert!(
        redraw_launch.skipped_routes[0]
            .detail
            .contains("fixture executable disappeared")
    );
    assert_ne!(
        redraw_launch.route.model.as_deref(),
        Some(redraw_launch.skipped_routes[0].route.as_str())
    );
    let config = std::fs::read_to_string(state.join("config.toml"))?;
    std::fs::write(
        state.join("config.toml"),
        config.replace("settlement_retries=1", "settlement_retries=0"),
    )?;
    let no_retry_paths = run::LaunchPaths {
        scratch: attempt.join("no-redraw-scratch"),
        provider_state: attempt.join("no-redraw-provider-state"),
        temporary: attempt.join("no-redraw-tmp"),
        temporary_alias: None,
        attempt_id: fallback_paths.attempt_id,
        operation_id: fallback_paths.operation_id.clone(),
        task_index: fallback_paths.task_index,
        lane_depth: fallback_paths.lane_depth,
        run_id: fallback_paths.run_id,
        deadline_unix_ms: fallback_paths.deadline_unix_ms,
    };
    for path in [
        &no_retry_paths.scratch,
        &no_retry_paths.provider_state,
        &no_retry_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let calls = std::cell::Cell::new(0);
    let Err(refusal) = prepare_with(
        &registry,
        &redraw_task,
        redraw_prompt,
        &no_retry_paths,
        None,
        |_| {
            let count = calls.get() + 1;
            calls.set(count);
            if count == 3 {
                Err("fixture executable disappeared".into())
            } else {
                Ok(executable.clone())
            }
        },
        None,
    ) else {
        panic!("zero retry budget must not redraw")
    };
    assert!(
        refusal
            .to_string()
            .contains("fixture executable disappeared")
    );
    assert_eq!(calls.get(), 3);
    let PrepareError::Candidates { skipped_routes, .. } = &refusal else {
        panic!("budget refusal must retain the skipped candidate")
    };
    assert_eq!(skipped_routes.len(), 1);
    assert_eq!(skipped_routes[0].kind, SkipKind::Unavailable);
    std::fs::write(state.join("config.toml"), saved_config)?;

    let mut review_task = task.clone();
    review_task.writer = None;
    let review_paths = run::LaunchPaths {
        scratch: attempt.join("review-scratch"),
        provider_state: attempt.join("review-provider-state"),
        temporary: attempt.join("review-tmp"),
        temporary_alias: None,
        ..fallback_paths
    };
    for path in [
        &review_paths.scratch,
        &review_paths.provider_state,
        &review_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let review = prepare_with(
        &registry,
        &review_task,
        "short review",
        &review_paths,
        None,
        |_| Ok(executable.clone()),
        None,
    )?;
    assert!(
        review
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_HEAD" && value == review_oid.as_str())
    );
    assert!(
        review
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_BASE" && value == review_oid.as_str())
    );
    let executed = executor.execute(ExecOptions::new(
        &launch.argv,
        &project,
        Duration::from_secs(2),
    ))?;
    assert_eq!(executed.termination, Termination::Exit(0));
    let update = provenant_os::provider_fence::acquire(
        roots.state.as_path(),
        "cursor",
        provenant_os::fs::LockMode::Exclusive,
    )?;
    drop(update);
    assert_eq!(launch.route.provider.as_deref(), Some("cursor"));
    assert_eq!(
        launch.route.model.as_deref(),
        Some("cursor/gpt-5.3-codex-low")
    );
    assert_eq!(launch.argv[0], executable.as_os_str());
    let scratch = paths.temporary.join("candidate-0");
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch.join("git-context.json"))?)?;
    assert_eq!(metadata["base"], metadata["head"]);
    assert_eq!(std::fs::read(scratch.join("git-diff.patch"))?, b"");
    let pointer = launch
        .argv
        .last()
        .ok_or("prompt missing")?
        .to_str()
        .ok_or("prompt UTF-8")?;
    assert!(pointer.len() < 1024);
    let spilled = attempt.join("prompt.md");
    assert!(pointer.contains(spilled.to_str().ok_or("path UTF-8")?));
    let spilled_text = std::fs::read_to_string(&spilled)?;
    let location = spilled_text
        .strip_suffix(&full)
        .ok_or("spilled prompt lost the task text")?;
    assert!(location.contains("Host-written Git context"));
    assert!(location.contains(&scratch.display().to_string()));
    assert_eq!(
        store.record(attempt_id, &call)?.prompt.digest,
        task.prompt_digest
    );
    assert_ne!(
        task.prompt_digest,
        provenant_records::CanonicalV1::file_digest(pointer.as_bytes())
    );
    assert_eq!(std::fs::read_to_string(attempt.join("prompt.txt"))?, prompt);
    // The original named pool may change or disappear after dispatch.
    // A continuation must use the exact applied provider, model and
    // account without consulting that pool or the original account alias.
    let mut prior = store.record(attempt_id, &call)?;
    prior.requested_route = run::RequestedRoute::Named {
        extra: Default::default(),
        name: "retired-pool".into(),
        effort: None,
    };
    prior.applied_route = Some(launch.route.clone());
    prior.session = Some("fixture-session".into());
    let mut continued_task = task.clone();
    continued_task.route = prior.requested_route.clone();
    continued_task.account = Some("retired-account-alias".into());
    let continued_paths = run::LaunchPaths {
        attempt_id: paths.attempt_id,
        operation_id: OperationId::parse("different-resume-seed")?,
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        run_id: paths.run_id,
        deadline_unix_ms: paths.deadline_unix_ms,
        scratch: attempt.join("continued-scratch"),
        provider_state: attempt.join("continued-provider-state"),
        temporary: attempt.join("continued-tmp"),
        temporary_alias: None,
    };
    for path in [
        &continued_paths.scratch,
        &continued_paths.provider_state,
        &continued_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let continued = prepare_with(
        &registry,
        &continued_task,
        "Continue the prior task",
        &continued_paths,
        Some(&prior),
        |adapter| {
            assert_eq!(adapter, "cursor", "resume probed a different provider");
            Ok(executable.clone())
        },
        None,
    )?;
    assert_eq!(continued.route.provider, launch.route.provider);
    assert_eq!(continued.route.model, launch.route.model);
    assert_eq!(continued.route.account_key, launch.route.account_key);
    assert!(continued.route.pool_selection.is_none());
    assert!(continued.argv.iter().any(|arg| arg == "--resume"));
    let confinement = launch.confinement.ok_or("writer confinement missing")?;
    assert!(confinement.writer.is_some());
    // Exercise the actual request builder with two distinct project overrides.
    let project_a = binding.project_id();
    let project_b = ProjectId::from_bytes([0xff; 16]);
    let config = provenant_confinement::run_config::RunConfig {
        projects: [
            (
                project_a.to_string(),
                provenant_confinement::run_config::ProjectRunConfig {
                    writer_writes: Some(vec![base.join("a-locks")]),
                    ..Default::default()
                },
            ),
            (
                project_b.to_string(),
                provenant_confinement::run_config::ProjectRunConfig {
                    writer_writes: Some(vec![base.join("b-locks")]),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let request = confinement_request(
        project_a,
        ConfinementRoots {
            cwd: &project,
            home: &home,
            scratch: &paths.scratch,
            private_state: &paths.provider_state,
            assets: &assets,
            prompt_file: None,
        },
        confinement.writer.as_ref(),
        &run::LaneAccess::default(),
        &config,
        "cursor",
        false,
    )?;
    assert_eq!(
        request.permissions.write_roots,
        vec![PathGrant {
            path: base.join("a-locks"),
            scope: PathScope::Subtree,
        }]
    );
    // The default config enables the cargo seed, and prepare hands that to the
    // lane's launch confinement, where `seed_target` reads it.
    assert!(
        confinement.cargo_registry_seed,
        "the default FabricConfig must reach the writer launch with the seed enabled"
    );
    assert!(!confinement.plan.permissions().writer_publishing());
    assert!(!scratch.join("publishing.gitconfig").exists());
    assert!(
        confinement
            .plan
            .permissions()
            .read_roots
            .contains(&PathGrant {
                path: spilled,
                scope: PathScope::Exact,
            })
    );
    assert_eq!(
        confinement.plan.permissions().scratch.path,
        paths.temporary.join("candidate-0")
    );
    let mut bad_task = task.clone();
    bad_task.base = Some("absent-base".into());
    let result = prepare_with(
        &registry,
        &bad_task,
        "fixture prompt",
        &paths,
        None,
        |_| Err("provider lookup must not happen for an invalid base".into()),
        None,
    );
    assert!(
        matches!(result, Err(PrepareError::Run(run::RunError::InvalidBase(base))) if base == "absent-base")
    );
    // A writable tree the plan cannot cover names that error, not a disclosure denial.
    std::fs::write(
        state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.run]\nwriter_writes = ['{}']\n",
            paths.scratch.display(),
        ),
    )?;
    let error = prepare_with(
        &registry,
        &task,
        &full,
        &paths,
        None,
        |_| Ok(executable.clone()),
        None,
    )
    .err()
    .ok_or("unconfinable worktree accepted")?;
    assert!(
        matches!(error, PrepareError::Candidates { ref plans, .. } if plans.iter().any(|plan| matches!(plan, provenant_confinement::PlanError::InvalidRoots))),
        "{error}"
    );
    Ok(())
}

// Exercise the owner-to-preparation path so the dirty brief regression also
// fails when launch forgets to mark an unchanged predecessor HEAD.
fn prepare_policy_writer_successor(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt: &str,
    executable: &Path,
) -> Result<(run::PreparedLaunch, PathBuf), Box<dyn std::error::Error>> {
    use provenant_core::{ActorId, CancelToken, Clock};
    use provenant_supervisor::custody::{
        ExitObservation, PublicationState, Scope, SettlementEvidence,
    };
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = run::RunStore::open(
        &run::ExecutionContext {
            state_root: registry.roots.ok_or("roots")?.state.clone(),
            project_id: registry.project_id.ok_or("project")?,
            actor_id: ActorId::from_bytes([40; 16]),
            read_prompts: true,
        },
        &call,
    )?;
    let mut input = super::super::tests::trace_run(
        40,
        provenant_core::AttemptId::from_bytes([39; 16]),
        clock.wall_now()?,
    )?;
    input.request.parent_attempt = None;
    input.request.tasks = vec![task.clone()];
    input.request.tasks[0].head = task.source_head.clone();
    input.request.tasks[0].source_head = None;
    input.attempts[0].task_id = task.task_id;
    input.attempts[0].prompt = prompt.into();
    store.create(&input, &call)?;
    let old = store.record(input.attempts[0].attempt_id, &call)?;
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: old.run_id,
            attempt: old.attempt_id,
            generation: old.generation,
        },
        termination: Some(provenant_os::exec::Termination::Exit(0)),
        never_spawned: false,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        omitted_spared: 0,
        cancel_delivered_before_exit: None,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: false,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
        stdout: Default::default(),
        stderr: Default::default(),
    };
    let mut wire = serde_json::to_value(&exit)?;
    wire.as_object_mut().ok_or("exit")?.remove("publication");
    wire["schema_version"] = 1.into();
    let attempt = registry
        .state_root
        .ok_or("state")?
        .join(format!("runs/{}/attempts/{}", old.run_id, old.attempt_id));
    let directory = provenant_os::fs::Directory::open_anchor(&attempt)?;
    for (name, value) in [
        ("process-exit.json", wire),
        (
            "writer-terminal.json",
            serde_json::json!({"schema_version":1,"head":task.head,"protected_paths":[]}),
        ),
    ] {
        directory
            .atomic_write(
                name.as_ref(),
                ".fixture-",
                &serde_json::to_vec(&value)?,
                provenant_os::fs::PublishMode::NoReplace,
            )?
            .require_durable()?;
    }
    store.stage_policy_retry(
        &old,
        &run::PolicyRetry {
            schema_version: 2,
            cause: run::PolicyCause::Quota,
            action: run::PolicyAction::Retry,
            route: old.requested_route.clone(),
            resume: false,
            max_attempts: 2,
            instruction: "Continue".into(),
            not_before_unix_seconds: None,
        },
        &call,
    )?;
    store.publish_terminal(
        old.attempt_id,
        old.revision,
        &run::TerminalPublication {
            outcome: run::Outcome::QuotaLimited,
            finished_at: clock.wall_now()?,
            started_at: None,
            provider_ms: None,
            reason: None,
            result: "quota",
            complete: false,
            redacted: false,
            resolved_route: None,
            applied_route: None,
            observed_route: None,
            usage: Default::default(),
            session: None,
            partial_signals: vec![],
        },
        &exit,
        &call,
    )?;
    struct Capture<'a, 'b> {
        registry: &'a Registry<'b>,
        executable: &'a Path,
        launch: std::cell::RefCell<Option<(run::PreparedLaunch, PathBuf)>>,
    }
    impl run::Providers for Capture<'_, '_> {
        fn prepare(
            &self,
            task: &run::TaskRequest,
            prompt: &str,
            paths: &run::LaunchPaths,
            _: Option<&run::AttemptRecord>,
        ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
            let launch = prepare_with(
                self.registry,
                task,
                prompt,
                paths,
                None,
                |_| Ok(self.executable.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            self.launch.replace(Some((
                launch,
                paths
                    .provider_state
                    .parent()
                    .ok_or("attempt")?
                    .join("prompt.md"),
            )));
            Err("captured prepared successor".into())
        }
        fn decode(
            &self,
            _: &run::RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> Result<run::Decoded, String> {
            unreachable!()
        }
    }
    let capture = Capture {
        registry,
        executable,
        launch: Default::default(),
    };
    let custody = directory.try_lock(
        std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    drop(custody);
    let started = store.start(input.run_id, &capture, &|_, _| unreachable!(), &call)?;
    assert!(
        matches!(started.as_slice(), [run::Started::Refused { reason, .. }] if reason == "captured prepared successor"),
        "{started:?}"
    );
    capture
        .launch
        .into_inner()
        .ok_or_else(|| "successor was not prepared".into())
}

#[test]
fn writer_continuation_preserves_uncommitted_edits_and_reports_them_in_brief()
-> Result<(), Box<dyn std::error::Error>> {
    writer_continuation_dirty_brief(true)
}

#[test]
fn writer_continuation_with_unchanged_head_reports_dirty_paths()
-> Result<(), Box<dyn std::error::Error>> {
    writer_continuation_dirty_brief(false)
}

fn writer_continuation_dirty_brief(committed: bool) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime, worktrees};
    use provenant_core::{
        AttemptId, CancelToken, Clock, Digest, OperationId, RunId, TaskId, WaitBudget,
    };
    use provenant_os::clock::SystemClock;
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::{
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        time::Duration,
    };

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("repo");
    let assets = base.join("release");
    let git_home = base.join("git-home");
    for path in [&home, &state, &project, &assets, &git_home] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let executor = SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = UtilityRuntime::new(&git, &git_home)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let repo = utility.open_repo(&executor, &project, budget, &cancel)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let worktree_state = state
        .join("worktrees")
        .join(binding.project_id().to_string());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&worktree_state)?;
    let project_state = provenant_os::fs::Directory::open_anchor(&worktree_state)?;
    let clock = SystemClock::new();
    worktrees::create(
        &worktrees::WorktreeContext {
            home: &home,
            repo: &repo,
            binding: &binding,
            project_state: &project_state,
            executor: &executor,
            utility: &utility,
            workers: &worktrees::SystemWorkerObserver,
            clock: &clock,
            utility_budget: budget,
            mutation_budget: budget,
            lock_budget: WaitBudget::cli(clock.mono_now(), 5)?,
        },
        worktrees::WorktreeCreate {
            branch: "writer".into(),
            base: Some("HEAD".into()),
        },
        &cancel,
    )?;
    let executable = assets.join("cursor/bin/agent");
    std::fs::create_dir_all(executable.parent().ok_or("provider bin")?)?;
    std::fs::write(&executable, b"#!/bin/sh\nprintf 'fixture-cursor 1.0\\n'\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;

    let worktree = project.join(".worktrees/writer");
    let source_head = run::resolve_git_base(&git, &project, "HEAD")?;

    let run_git = |args: &[&str], cwd: &std::path::Path| {
        let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.iter().map(Into::into))
            .collect();
        let mut env = vec![
            ("HOME".into(), git_home.as_os_str().to_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        env.extend(provenant_os::fixture::fixture_git_settings());
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
        options.env = Some(&env);
        executor.execute(options)
    };
    if committed {
        std::fs::write(worktree.join("committed.txt"), "predecessor work")?;
        run_git(&["add", "committed.txt"], &worktree)?;
        let commit_res = run_git(&["commit", "-m", "predecessor commit"], &worktree)?;
        assert_eq!(commit_res.termination, Termination::Exit(0));
    }
    let predecessor_head = run::resolve_git_base(&git, &worktree, "HEAD")?;
    assert_eq!(source_head != predecessor_head, committed);

    std::fs::write(
        worktree.join("uncommitted_leftover.txt"),
        "uncommitted dirty edit",
    )?;

    let run_id = RunId::from_bytes([20; 16]);
    let attempt_id = AttemptId::from_bytes([21; 16]);
    let attempt = state.join(format!("runs/{run_id}/attempts/{attempt_id}"));
    let paths = run::LaunchPaths {
        attempt_id,
        operation_id: OperationId::parse("writer-continuation-dirty")?,
        task_index: 0,
        lane_depth: 1,
        run_id,
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        provider_state: attempt.join("provider-state"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
    };
    for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }

    let prompt = "Fix remaining issues in continuation";
    let task = run::TaskRequest {
        cloud: None,
        context: None,
        extra: Default::default(),
        build_heavy: false,
        title: None,
        base: Some(source_head.clone()),
        head: Some(predecessor_head.clone()),
        source_head: Some(source_head.clone()),
        base_ref: None,
        task_id: TaskId::from_bytes([22; 16]),
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "cursor".into(),
            model: "cursor/gpt-5.3-codex-low".into(),
            effort: None,
        },
        account: None,
        fallback: vec![],
        authority_digest: Digest::from_bytes([23; 32]),
        writer: Some(run::WriterRequest {
            extra: Default::default(),
            project: project.clone(),
            worktree: worktree.clone(),
        }),
        deliver: None,
        provider_timeout_seconds: 20,
        delivery_id: None,
        access: Default::default(),
    };

    let registry = Registry {
        roots: Some(&roots),
        home: Some(&home),
        assets: Some(&assets),
        cwd: Some(&project),
        state_root: Some(&state),
        project_id: Some(binding.project_id()),
        cancel: Some(&cancel),
        host: None,
    };

    let (launch, spilled) = prepare_policy_writer_successor(&registry, &task, prompt, &executable)?;

    // A dirty worktree continues without being reset
    assert!(worktree.join("uncommitted_leftover.txt").exists());
    assert_eq!(
        std::fs::read_to_string(worktree.join("uncommitted_leftover.txt"))?,
        "uncommitted dirty edit"
    );

    // Spilled prompt contains uncommitted predecessor edits
    let prompt_content = if spilled.exists() {
        std::fs::read_to_string(&spilled)?
    } else {
        launch.argv.last().unwrap().to_str().unwrap().to_string()
    };
    assert!(
        prompt_content
            .contains("Uncommitted predecessor edits in worktree:\n- uncommitted_leftover.txt"),
        "brief must report uncommitted predecessor edits: {prompt_content}"
    );

    // If continuation check refuses, uncommitted edits are named in the refusal message
    let mut bad_task = task.clone();
    bad_task.source_head = Some("0".repeat(40));
    let error = prepare_with(
        &registry,
        &bad_task,
        prompt,
        &paths,
        None,
        |_| Ok(executable.clone()),
        None,
    )
    .err()
    .ok_or("mismatched continuation should refuse")?;
    let error_text = error.to_string();
    assert!(
        error_text.contains("uncommitted predecessor edits: uncommitted_leftover.txt"),
        "refusal must list uncommitted predecessor edits: {error_text}"
    );

    // Inject a Git that works for ancestry but refuses status. Exercise both
    // the brief path and the mismatch path: neither may hide missing evidence.
    let status_git = base.join("status-git");
    std::fs::write(
        &status_git,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = status ]; then echo forced-status-failure >&2; exit 73; fi\ndone\nexec '{}' \"$@\"\n",
            git.display()
        ),
    )?;
    std::fs::set_permissions(&status_git, std::fs::Permissions::from_mode(0o700))?;
    let host = run::HostLaunchContext::from_environment(&state, &project, status_git);
    let failing_registry = Registry {
        host: Some(&host),
        ..registry
    };
    for candidate in [&task, &bad_task] {
        let error = prepare_with(
            &failing_registry,
            candidate,
            prompt,
            &paths,
            None,
            |_| Ok(executable.clone()),
            None,
        )
        .err()
        .ok_or("status failure must refuse preparation")?;
        assert!(
            error
                .to_string()
                .contains("cannot inspect uncommitted predecessor edits"),
            "status failure must be explicit: {error}"
        );
    }

    Ok(())
}

#[test]
fn read_only_plan_runs_host_git_with_a_private_home_outside_the_child_store()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{NetworkAccess, PlanPermissions};
    use std::os::unix::fs::PermissionsExt;
    struct Identity;
    impl provenant_core::PathResolver for Identity {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            let mut tail = PathBuf::new();
            let mut head = path;
            loop {
                if let Ok(resolved) = std::fs::canonicalize(head) {
                    return Ok(resolved.join(tail));
                }
                let (Some(parent), Some(name)) = (head.parent(), head.file_name()) else {
                    return Err(std::io::ErrorKind::NotFound.into());
                };
                tail = Path::new(name).join(tail);
                head = parent;
            }
        }
    }

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let host_root = base.join("host-state");
    // The lane controls its child store, including the old git-home path.
    let child_store = host_root.join("children/parent-attempt");
    let planted = child_store.join("tmp/git-home");
    std::fs::create_dir_all(&planted)?;
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o777))?;
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(base.join("home"))?;
    let real = provenant_os::git::git_executable().ok_or("git unavailable")?;
    // The smallest repository layout Git accepts, without spawning Git.
    for dir in [".git/objects", ".git/refs/heads"] {
        std::fs::create_dir_all(repo.join(dir))?;
    }
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n")?;
    // A git that records the HOME and TMPDIR it was given, then runs the real one.
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin)?;
    let record = base.join("seen");
    let git = bin.join("git");
    std::fs::write(
        &git,
        format!(
            "#!/bin/sh\n[ \"$HOME\" = \"$TMPDIR\" ] || exit 97\nls -ld \"$HOME\" >> {r}\nexec {g} \"$@\"\n",
            r = record.display(),
            g = real.display()
        ),
    )?;
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))?;

    let roots = provenant_core::RootInputs {
        state_dir: Some(host_root.clone()),
        ..provenant_core::RootInputs::new(base.join("home"), repo.clone())
    }
    .resolve(&Identity)?;
    let grant = |name: &str| PathGrant {
        path: base.join(name),
        scope: PathScope::Subtree,
    };
    let request = PlanRequest {
        scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
        permissions: PlanPermissions {
            read_only: None,
            mode: AccessMode::ReadOnly,
            scratch: grant("scratch"),
            private_state: Some(grant("provider-state")),
            read_roots: vec![grant("repo")],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: None,
            include_continuation: false,
            write_roots: vec![],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    };
    // The unbound project is refused, but only after host git has run.
    let result = read_only_plan(&roots, ProjectId::from_bytes([1; 16]), &repo, &git, request);
    assert!(result.is_err());

    let seen = std::fs::read_to_string(&record)?;
    assert!(!seen.is_empty(), "host git never ran");
    for line in seen.lines() {
        assert!(line.starts_with("drwx------"), "{line}");
        assert!(
            line.contains(&format!("{}/probe-", host_root.display())),
            "{line}"
        );
        assert!(!line.contains("children"), "{line}");
    }
    // The home is gone once the plan is done, and the child store is unchanged.
    let leftovers: Vec<_> = std::fs::read_dir(&host_root)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("probe-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert_eq!(std::fs::read_dir(&planted)?.count(), 0);
    Ok(())
}

#[test]
fn credential_authority_never_follows_links_below_the_host_root()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, OperationId, RunId};

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let host_root = base.join("host-state");
    let relative = Path::new("children/parent/runs/run/attempts/attempt");
    let attempt = host_root.join(relative);
    let config = base.join("home/.claude-pro");
    std::fs::create_dir_all(attempt.join("provider-state"))?;
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join(".credentials.json"), b"{}")?;
    let mut paths = run::LaunchPaths {
        attempt_id: AttemptId::from_bytes([1; 16]),
        operation_id: OperationId::parse("fixture")?,
        task_index: 0,
        lane_depth: 2,
        run_id: RunId::from_bytes([2; 16]),
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        provider_state: attempt.join("provider-state"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
    };
    let login = provenant_providers::claude::Login {
        executable: Path::new("/fixture/claude"),
        home: &base.join("home"),
        user: std::ffi::OsStr::new("fixture"),
        config_dir: &config,
    };
    credential_authority(&host_root, &paths, &login)?;
    let record = attempt.join("credential-authority.json");
    let original = std::fs::read(&record)?;
    // Test intermediate child-store ancestors as well as the final attempt.
    for ancestor in [
        Path::new("children"),
        Path::new("children/parent"),
        relative,
    ] {
        let source = host_root.join(ancestor);
        let redirected = base.join("redirected");
        std::fs::rename(&source, &redirected)?;
        std::os::unix::fs::symlink(&redirected, &source)?;
        assert!(credential_authority(&host_root, &paths, &login).is_err());
        assert_eq!(std::fs::read(&record)?, original);
        std::fs::remove_file(&source)?;
        std::fs::rename(&redirected, &source)?;
    }
    paths.provider_state = base.join("outside/provider-state");
    assert!(credential_authority(&host_root, &paths, &login).is_err());
    paths.provider_state = host_root.join("children/../outside/provider-state");
    assert!(credential_authority(&host_root, &paths, &login).is_err());
    Ok(())
}

#[test]
fn user_sessions_and_pruned_codex_state_remain_resumable() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_core::{AttemptId, Digest, RunId, TaskId};
    use provenant_records::{
        FileDigest, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    for adapter in ["cursor", "opencode", "codex"] {
        let route = ResolvedRoute {
            extra: Default::default(),
            adapter: adapter.into(),
            model: "fixture-model".into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: Some("local".into()),
            host: "fixture".into(),
            alternatives: RouteAlternatives::default(),
            exclusions: Vec::new(),
            eligibility: RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                extra: Default::default(),
                availability_ref: "fixture".into(),
                expires_unix_seconds: 100,
            },
            confidentiality: RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        };
        let mut prior: run::AttemptRecord = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]), "run_id": RunId::from_bytes([2; 16]),
            "project_id": ProjectId::from_bytes([3; 16]), "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "fixture", "attempt_number": 1, "revision": 1, "generation": 0,
            "requested_route": {"kind": "model", "provider": adapter, "model": "fixture-model"},
            "applied_route": {"provider": adapter, "model": format!("{adapter}/fixture-model"), "account_key": "local"},
            "authority_digest": Digest::from_bytes([0; 32]), "session": "fixture-session",
            "queued_at": "2026-10-25T00:00:00.000000Z", "state": {"state": "queued"}, "usage": {},
            "prompt": {"path": {"encoding": "utf8", "path": "prompt.txt"}, "digest": FileDigest::from_bytes([0; 32]), "bytes": 0, "complete": true, "redacted": false}
        }))?;
        let state = Path::new("/fixture/missing-state");
        let result = continuation_for(&prior, &route, "fixture", state, state);
        if adapter == "codex" {
            assert!(result.is_err(), "Codex still requires its retained state");
            let session = "01234567-89ab-cdef-0123-456789abcdef";
            prior.session = Some(session.into());
            let rollout =
                format!("sessions/2026/10/27/rollout-2026-10-27T00-00-00-{session}.jsonl");
            let transcript = format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session_meta", "payload":{"id":session}}),
                serde_json::json!({"type":"turn_context", "payload":{"model":"fixture-model"}}),
            );
            use provenant_core::{ActorId, CancelToken, MonoTime, RootInputs, TestClock, WallTime};
            use std::os::unix::fs::DirBuilderExt;
            let temp = tempfile::tempdir()?;
            let base = temp.path().canonicalize()?;
            let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
            inputs.state_dir = Some(base.join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let context = run::ExecutionContext {
                state_root: roots.state,
                project_id: prior.project_id,
                actor_id: ActorId::from_bytes([5; 16]),
                read_prompts: true,
            };
            let clock = TestClock::new(WallTime::from_micros(100)?, MonoTime::from_micros(0));
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let store = run::RunStore::open(&context, &call)?;
            let request = run::PreparedRun {
                run_id: prior.run_id,
                operation_id: prior.operation_id.clone(),
                created_at: WallTime::from_micros(100)?,
                attempts: vec![run::NewAttempt {
                    attempt_id: prior.attempt_id,
                    task_id: prior.task_id,
                    prompt: String::new(),
                }],
                request: serde_json::from_value(serde_json::json!({
                    "schema_version": 1, "concurrency": 1, "parent": null,
                    "tasks": [{ "task_id": prior.task_id, "prompt_digest": provenant_records::CanonicalV1::file_digest(b""),
                        "route": prior.requested_route, "fallback": [], "authority_digest": prior.authority_digest,
                        "provider_timeout_seconds": 20, "delivery_id": null }]
                }))?,
            };
            store.create(&request, &call)?;
            let candidate = context.state_root.as_path().join(format!(
                "runs/{}/attempts/{}/provider-state/candidate-0",
                prior.run_id, prior.attempt_id
            ));
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(candidate.join("sessions/2026/10/27"))?;
            std::fs::write(candidate.join("provider.txt"), "codex")?;
            std::fs::write(candidate.join(&rollout), &transcript)?;
            // Codex's thread index can point to the source attempt even
            // after its rollout has been copied into another CODEX_HOME.
            let stale_path = candidate.join(&rollout);
            for index in ["state_5.sqlite", "state_5.sqlite-wal", "state_5.sqlite-shm"] {
                std::fs::write(
                    candidate.join(index),
                    stale_path.as_os_str().as_encoded_bytes(),
                )?;
            }
            for cache in ["cache", "plugins", "skills"] {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(candidate.join(cache))?;
                std::fs::write(candidate.join(cache).join("payload"), "cache")?;
            }
            std::fs::write(candidate.join("models_cache.json"), "cache")?;
            store.withdraw(
                prior.attempt_id,
                run::AttemptState::Queued {
                    extra: Default::default(),
                },
                run::Outcome::Cancelled,
                "fixture",
                WallTime::from_micros(200)?,
                &call,
            )?;
            for cache in ["cache", "plugins", "skills", "models_cache.json"] {
                assert!(!candidate.join(cache).exists());
            }
            use std::os::unix::fs::MetadataExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(candidate.join("cache"))?;
            std::fs::write(candidate.join("cache/payload"), "pending deletion")?;
            let metadata = std::fs::metadata(candidate.join("cache"))?;
            let tombstone = format!(
                "{}{:016x}{:016x}-cache",
                run::TOMBSTONE,
                metadata.dev(),
                metadata.ino()
            );
            std::fs::rename(candidate.join("cache"), candidate.join(&tombstone))?;
            let nested_tombstone = format!("sessions/{}interrupted", run::TOMBSTONE);
            std::fs::write(candidate.join(&nested_tombstone), "pending deletion")?;

            let copied = base.join("resumed-state");
            let continuation = continuation_for(
                &prior,
                &route,
                "fixture",
                context.state_root.as_path(),
                &copied,
            )?;
            assert_eq!(continuation.session(), session);
            assert!(!copied.join(&tombstone).exists());
            assert!(!copied.join(nested_tombstone).exists());
            assert!(candidate.join(&tombstone).exists());
            assert_eq!(std::fs::read_to_string(copied.join(&rollout))?, transcript);
            assert_eq!(
                provenant_providers::codex_rollout::observe(&copied, session)?.model,
                "fixture-model"
            );
            for index in ["state_5.sqlite", "state_5.sqlite-wal", "state_5.sqlite-shm"] {
                assert!(
                    !copied.join(index).exists(),
                    "resumed Codex must discover local rollouts without the source attempt's index: {index}"
                );
                assert_eq!(
                    std::fs::read(candidate.join(index))?,
                    stale_path.as_os_str().as_encoded_bytes()
                );
            }

            // Exercise the hand-off to the real adapter without launching
            // a nested Codex process under the harness's confinement.
            let home = base.join("home");
            let cwd = base.join("checkout");
            let scratch = base.join("scratch");
            let assets = base.join("assets");
            let plan = ConfinedPlan::new(confinement_request(
                prior.project_id,
                ConfinementRoots {
                    cwd: &cwd,
                    home: &home,
                    scratch: &scratch,
                    private_state: &copied,
                    assets: &assets,
                    prompt_file: None,
                },
                None,
                &Default::default(),
                &Default::default(),
                "codex",
                true,
            )?)?;
            use provenant_providers::{Adapter, ProviderEvent, codex::Codex};
            let prompt = Prompt::new("continue".into());
            let account = provenant_providers::identity::RegisteredAccount::new(
                "codex",
                "local",
                home.join(".codex"),
            )?;
            let launch = Codex.prepare(PrepareInput {
                command: None,
                route: &route,
                confined: &plan,
                prompt: &prompt,
                continuation: Some(&continuation),
                grant_digest: "fixture",
                executable: Path::new("/opt/homebrew/Cellar/codex/fixture/bin/codex"),
                home: &home,
                user: "fixture".as_ref(),
                host_var: &|_| None,
                skill_stage_max_bytes: 16 * 1024 * 1024,
                account: Some(&account),
                token: None,
                cwd: &cwd,
                git_context: false,
                nonce: [9; 16],
                provenant: LaneRuntime {
                    executable: Path::new("/fixture/provenant/bin/provenant"),
                    git: Path::new("/usr/bin/git"),
                    state: context.state_root.as_path(),
                    child_state: &base.join("children"),
                    home: &home,
                    depth: 1,
                    attempt: AttemptId::from_bytes([6; 16]),
                    assets: &assets,
                },
            })?;
            assert_eq!(launch.argv[2], "resume");
            assert_eq!(launch.argv[3], session);
            assert!(
                launch
                    .env
                    .iter()
                    .any(|(key, value)| { key == "CODEX_HOME" && value == copied.as_os_str() })
            );
            assert!(
                plan.permissions()
                    .private_state
                    .as_ref()
                    .is_some_and(|grant| {
                        grant.path == copied && grant.scope == PathScope::Subtree
                    })
            );
            assert!(
                plan.permissions()
                    .read_roots
                    .iter()
                    .all(|grant| { !candidate.starts_with(&grant.path) })
            );
            assert!(
                launch
                    .profile
                    .spec
                    .runtime_reads
                    .iter()
                    .all(|grant| { !candidate.starts_with(&grant.path) })
            );
            let recorded = include_str!(
                "../../../../../tests/fixtures/providers/codex/live-worktree_write-resume.jsonl"
            )
            .replace("FIXTURE_SESSION_IDENTIFIER", session);
            let mut stream = Codex.stream(Some(&continuation))?;
            let events = Codex.observe(&mut stream, recorded.as_bytes())?;
            assert!(events.iter().any(|event| {
                matches!(event, ProviderEvent::Session(started) if started == session)
            }));
            assert_eq!(
                Codex
                    .finish(
                        stream,
                        provenant_providers::ExitObservation {
                            code: Some(0),
                            timed_out: false,
                        }
                    )?
                    .status(),
                provenant_providers::OutcomeStatus::Success
            );
        } else {
            assert_eq!(result?.session(), "fixture-session");
            prior.session = None;
            assert!(continuation_for(&prior, &route, "fixture", state, state).is_err());
        }
    }
    Ok(())
}

#[test]
fn retained_state_copy_keeps_only_codex_session_trees() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(source.join("sessions"))?;
    for name in ["cache", "plugins", "skills"] {
        std::fs::create_dir(source.join(name))?;
        std::fs::write(source.join(name).join("installed"), "partial cache")?;
        std::fs::create_dir(source.join("sessions").join(name))?;
        std::fs::write(source.join("sessions").join(name).join("keep"), "state")?;
    }
    std::fs::write(source.join("models_cache.json"), "cache")?;
    std::fs::write(source.join("state_5.sqlite"), "source rollout paths")?;
    std::fs::create_dir(source.join("archived_sessions"))?;
    std::fs::write(
        source.join("archived_sessions/rollout.jsonl"),
        "archived transcript",
    )?;
    std::fs::write(source.join("sessions/models_cache.json"), "state")?;
    std::fs::write(source.join("sessions/rollout.jsonl"), "transcript")?;
    for provider in ["codex", "claude"] {
        std::fs::write(source.join("provider.txt"), provider)?;
        let copied = base.join(provider);
        copy_state(&source, &copied, provider == "codex")?;
        for name in [
            "cache",
            "plugins",
            "skills",
            "models_cache.json",
            "state_5.sqlite",
        ] {
            assert_eq!(copied.join(name).exists(), provider != "codex");
            assert!(source.join(name).exists());
        }
        for name in ["cache", "plugins", "skills", "models_cache.json"] {
            assert!(copied.join("sessions").join(name).exists());
        }
        assert_eq!(
            std::fs::read_to_string(copied.join("archived_sessions/rollout.jsonl"))?,
            "archived transcript"
        );
        assert_eq!(
            std::fs::read_to_string(copied.join("sessions/rollout.jsonl"))?,
            "transcript"
        );
    }
    Ok(())
}

#[test]
fn retained_state_never_follows_a_symlink() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let state = base.join("provider-state");
    std::fs::create_dir_all(base.join("outside/fixture-session"))?;
    std::fs::create_dir_all(&state)?;
    std::os::unix::fs::symlink(base.join("outside"), state.join("candidate-0"))?;
    assert_eq!(find_session_state(&state, "fixture-session"), Ok(None));
    std::fs::create_dir_all(state.join("candidate-1/fixture-session"))?;
    assert_eq!(
        find_session_state(&state, "fixture-session"),
        Ok(Some(state.join("candidate-1")))
    );
    std::os::unix::fs::symlink(&state, base.join("alias"))?;
    assert_eq!(
        find_session_state(&base.join("alias"), "fixture-session"),
        Ok(None)
    );
    assert!(copy_state(&state.join("candidate-0"), &base.join("copy"), false).is_err());
    std::fs::write(state.join("candidate-1/fixture-session/own"), "lane")?;
    copy_state(&state.join("candidate-1"), &base.join("copy"), false)?;
    assert!(base.join("copy/fixture-session/own").is_file());
    std::fs::write(base.join("outside/host-secret"), "host")?;
    std::fs::hard_link(
        base.join("outside/host-secret"),
        state.join("candidate-1/fixture-session/linked"),
    )?;
    let copied = base.join("linked-copy");
    assert_eq!(
        copy_state(&state.join("candidate-1"), &copied, false),
        Err("retained provider state holds a hard-linked file".into())
    );
    assert!(!copied.join("fixture-session/linked").exists());
    Ok(())
}

// Test-only: std has no mkfifo and this crate forbids unsafe code.
#[allow(clippy::disallowed_methods)]
fn mkfifo(path: &Path) -> std::io::Result<()> {
    let status = std::process::Command::new("mkfifo").arg(path).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("mkfifo failed"))
    }
}

/// Copy `source` while `swap` changes the entry `victim` after its check,
/// on a thread so a blocking open fails the test instead of hanging it.
fn copy_with_swap(
    source: &Path,
    copied: &Path,
    swap: impl FnOnce(&Path) -> std::io::Result<()> + Send + 'static,
) -> Result<(), String> {
    let (source, copied) = (source.to_owned(), copied.to_owned());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut swap = Some(swap);
        let entry = source.join("victim");
        BEFORE_OPEN.set(Some(Box::new(move |name| {
            if name == "victim"
                && let Some(swap) = swap.take()
            {
                swap(&entry).expect("swap the checked entry");
            }
        })));
        let _ = sender.send(copy_state(&source, &copied, false));
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the copy finishes without blocking")
}

#[test]
fn retained_state_copy_skips_entries_swapped_after_their_check()
-> Result<(), Box<dyn std::error::Error>> {
    type Swap = fn(&Path, &Path) -> std::io::Result<()>;
    let swaps: [(&str, bool, Swap); 6] = [
        ("file to outside link", false, |entry, outside| {
            std::fs::remove_file(entry)?;
            std::os::unix::fs::symlink(outside.join("host-secret"), entry)
        }),
        ("directory to outside link", true, |entry, outside| {
            std::fs::remove_dir_all(entry)?;
            std::os::unix::fs::symlink(outside, entry)
        }),
        ("file to FIFO", false, |entry, _| {
            std::fs::remove_file(entry)?;
            mkfifo(entry)
        }),
        ("file to socket", false, |entry, _| {
            std::fs::remove_file(entry)?;
            std::os::unix::net::UnixListener::bind(entry).map(drop)
        }),
        ("directory to file", true, |entry, _| {
            std::fs::remove_dir_all(entry)?;
            std::fs::write(entry, "replaced")
        }),
        ("file removed", false, |entry, _| {
            std::fs::remove_file(entry)
        }),
    ];
    for (label, directory, swap) in swaps {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let (source, outside) = (base.join("candidate-0"), base.join("outside"));
        std::fs::create_dir_all(&outside)?;
        std::fs::write(outside.join("host-secret"), "host")?;
        std::fs::create_dir_all(&source)?;
        std::fs::write(source.join("keep"), "lane")?;
        if directory {
            std::fs::create_dir(source.join("victim"))?;
            std::fs::write(source.join("victim/inner"), "lane")?;
        } else {
            std::fs::write(source.join("victim"), "lane")?;
        }
        let copied = base.join("copy");
        let swap_outside = outside.clone();
        copy_with_swap(&source, &copied, move |entry| swap(entry, &swap_outside))
            .map_err(|error| format!("{label}: {error}"))?;
        assert_eq!(std::fs::read_to_string(copied.join("keep"))?, "lane");
        let entries = std::fs::read_dir(&copied)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(entries, ["keep"], "{label}: the swapped entry is skipped");
    }
    Ok(())
}

#[test]
fn retained_state_copy_refuses_a_file_swapped_for_a_hard_link()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(&source)?;
    std::fs::write(base.join("host-secret"), "host")?;
    std::fs::write(source.join("victim"), "lane")?;
    let copied = base.join("copy");
    let secret = base.join("host-secret");
    let result = copy_with_swap(&source, &copied, move |entry| {
        std::fs::remove_file(entry)?;
        std::fs::hard_link(&secret, entry)
    });
    assert_eq!(
        result,
        Err("retained provider state holds a hard-linked file".into())
    );
    assert!(std::fs::symlink_metadata(copied.join("victim")).is_err());
    Ok(())
}

#[test]
fn retained_state_copy_skips_nested_symlinks() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(base.join("outside/secrets"))?;
    std::fs::write(base.join("outside/host-secret"), "host")?;
    std::fs::write(base.join("outside/secrets/token"), "host")?;
    std::fs::create_dir_all(source.join("sessions/2026"))?;
    std::fs::write(source.join("config.toml"), "lane")?;
    std::fs::write(source.join("sessions/2026/rollout.jsonl"), "turn")?;
    std::fs::write(source.join("tool"), "#!/bin/sh\n")?;
    std::fs::set_permissions(source.join("tool"), std::fs::Permissions::from_mode(0o755))?;
    std::os::unix::fs::symlink(base.join("outside/host-secret"), source.join("file-link"))?;
    std::os::unix::fs::symlink(base.join("outside/secrets"), source.join("dir-link"))?;
    std::os::unix::fs::symlink(
        base.join("outside/host-secret"),
        source.join("sessions/2026/nested-link"),
    )?;
    let copied = base.join("copy");
    copy_state(&source, &copied, false)?;
    assert_eq!(std::fs::read_to_string(copied.join("config.toml"))?, "lane");
    assert_eq!(
        std::fs::read_to_string(copied.join("sessions/2026/rollout.jsonl"))?,
        "turn"
    );
    let tool = std::fs::metadata(copied.join("tool"))?.permissions().mode();
    assert_eq!(tool & 0o100, 0o100, "owner execute survives the copy");
    for skipped in ["file-link", "dir-link", "sessions/2026/nested-link"] {
        assert!(
            std::fs::symlink_metadata(copied.join(skipped)).is_err(),
            "{skipped} must not be copied"
        );
    }
    let codex_copy = base.join("codex-copy");
    copy_state(&source, &codex_copy, true)?;
    assert_eq!(
        std::fs::read_to_string(codex_copy.join("sessions/2026/rollout.jsonl"))?,
        "turn"
    );
    assert!(std::fs::symlink_metadata(codex_copy.join("sessions/2026/nested-link")).is_err());
    Ok(())
}

#[test]
fn retained_state_copy_refuses_state_past_its_byte_budget() -> Result<(), Box<dyn std::error::Error>>
{
    const LIMIT: u64 = 1 << 20;
    let limits = StateLimits {
        bytes: LIMIT,
        ..RETAINED_STATE
    };
    let refusal = Err(format!(
        "retained provider state exceeds its {LIMIT} byte copy budget"
    ));
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    // A sparse file costs the lane no blocks but claims far past the budget.
    let sparse = base.join("sparse");
    std::fs::create_dir(&sparse)?;
    std::fs::File::create(sparse.join("rollout.jsonl"))?.set_len(64 * LIMIT)?;
    let copied = base.join("sparse-copy");
    assert_eq!(copy_state_within(&sparse, &copied, limits, false), refusal);
    assert!(
        !copied.join("rollout.jsonl").exists(),
        "the partial copy is removed"
    );
    // The budget spans the tree, not each file.
    let split = base.join("split");
    std::fs::create_dir_all(split.join("sessions"))?;
    let half = vec![b'x'; usize::try_from(LIMIT / 2 + 1)?];
    std::fs::write(split.join("history.jsonl"), &half)?;
    std::fs::write(split.join("sessions/rollout.jsonl"), &half)?;
    assert_eq!(
        copy_state_within(&split, &base.join("split-copy"), limits, false),
        refusal
    );
    // State of exactly the budget still copies whole.
    let exact = base.join("exact");
    std::fs::create_dir(&exact)?;
    std::fs::write(exact.join("a"), &half)?;
    std::fs::write(exact.join("b"), vec![b'y'; usize::try_from(LIMIT / 2 - 1)?])?;
    copy_state_within(&exact, &base.join("exact-copy"), limits, false)?;
    assert_eq!(std::fs::read(base.join("exact-copy/a"))?, half);
    assert_eq!(
        std::fs::metadata(base.join("exact-copy/b"))?.len(),
        LIMIT / 2 - 1
    );
    Ok(())
}

#[test]
fn retained_state_copy_refuses_trees_past_its_entry_and_depth_budget()
-> Result<(), Box<dyn std::error::Error>> {
    let limits = StateLimits {
        depth: 2,
        entries: 4,
        ..RETAINED_STATE
    };
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let within = base.join("within");
    std::fs::create_dir_all(within.join("a/b"))?;
    std::fs::write(within.join("a/b/file"), "lane")?;
    std::fs::write(within.join("top"), "lane")?;
    copy_state_within(&within, &base.join("within-copy"), limits, false)?;
    assert_eq!(
        std::fs::read_to_string(base.join("within-copy/a/b/file"))?,
        "lane"
    );

    let deep = base.join("deep");
    std::fs::create_dir_all(deep.join("a/b/c"))?;
    assert_eq!(
        copy_state_within(&deep, &base.join("deep-copy"), limits, false),
        Err("retained provider state nests deeper than 2 directories".into())
    );

    let wide = base.join("wide");
    std::fs::create_dir(&wide)?;
    for name in ["1", "2", "3", "4", "5"] {
        std::fs::write(wide.join(name), "lane")?;
    }
    assert_eq!(
        copy_state_within(&wide, &base.join("wide-copy"), limits, false),
        Err("retained provider state exceeds 4 entries".into())
    );
    Ok(())
}

#[test]
fn retained_state_copy_leaves_nothing_when_refused() -> Result<(), Box<dyn std::error::Error>> {
    const LIMIT: u64 = 1 << 20;
    let limits = StateLimits {
        bytes: LIMIT,
        ..RETAINED_STATE
    };
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(source.join("sessions.lock"))?;
    std::fs::write(source.join("sessions.lock/rollout.jsonl"), "turn")?;
    std::fs::write(source.join("keep"), "lane")?;
    // A lock-named partial, which ordinary removal refuses.
    std::fs::File::create(source.join("Cargo.lock"))?.set_len(64 * LIMIT)?;
    // Like a launch, the destination is an empty candidate directory.
    let state = base.join("provider-state");
    let copied = state.join("candidate-0");
    std::fs::create_dir_all(&copied)?;
    assert_eq!(
        copy_state_within(&source, &copied, limits, false),
        Err(format!(
            "retained provider state exceeds its {LIMIT} byte copy budget"
        ))
    );
    let names = |path: &Path| -> std::io::Result<Vec<_>> {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    };
    assert_eq!(names(&state)?, ["candidate-0"], "no staging survives");
    assert!(names(&copied)?.is_empty(), "no partial output survives");
    std::fs::remove_file(source.join("Cargo.lock"))?;
    copy_state_within(&source, &copied, limits, false)?;
    assert_eq!(names(&state)?, ["candidate-0"]);
    assert_eq!(
        std::fs::read_to_string(copied.join("sessions.lock/rollout.jsonl"))?,
        "turn"
    );
    Ok(())
}

#[test]
fn session_search_reports_its_spent_entry_budget() -> Result<(), Box<dyn std::error::Error>> {
    let limits = StateLimits {
        entries: 4,
        ..RETAINED_STATE
    };
    let temp = tempfile::tempdir()?;
    let state = std::fs::canonicalize(temp.path())?;
    std::fs::create_dir_all(state.join("candidate-0/sessions"))?;
    for name in ["1", "2", "3"] {
        std::fs::write(state.join("candidate-0/sessions").join(name), "lane")?;
    }
    // An unfinished search is not a missing session.
    assert_eq!(
        find_session_state_within(&state, "fixture-session", limits),
        Err(SearchExhausted)
    );
    std::fs::remove_file(state.join("candidate-0/sessions/3"))?;
    assert_eq!(
        find_session_state_within(&state, "fixture-session", limits),
        Ok(None)
    );
    Ok(())
}

// No unprivileged test can mount a filesystem inside a temporary tree, so
// this runs the walk as if its anchor sat on another device.
#[test]
fn retained_state_copy_refuses_a_filesystem_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(source.join("sessions"))?;
    std::fs::create_dir(base.join("copy"))?;
    let (from, to) = (
        Directory::open_anchor(&source)?,
        Directory::open_anchor(&base.join("copy"))?,
    );
    let mut budget = Budget {
        entries: RETAINED_STATE.entries,
        bytes: RETAINED_STATE.bytes,
        device: from.metadata()?.device ^ 1,
    };
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = std::rc::Rc::clone(&opened);
    BEFORE_OPEN.set(Some(Box::new(move |_| seen.set(true))));
    let result = copy_tree(&from, &to, RETAINED_STATE.depth, &mut budget, true);
    BEFORE_OPEN.set(None);
    assert!(matches!(result, Err(CopyRefusal::CrossDevice)));
    assert!(!opened.get(), "a mount point is refused before its open");
    assert!(!base.join("copy/sessions").exists());
    Ok(())
}

#[test]
fn codex_rollout_is_found_in_the_launched_candidate() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join(format!("provenant-candidates-{}", std::process::id()));
    let thread = "01234567-89ab-cdef-0123-456789abcdef";
    let day = base.join("candidate-1/sessions/2026/10/24");
    std::fs::create_dir_all(base.join("candidate-0"))?;
    std::fs::create_dir_all(&day)?;
    std::fs::write(
        day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
        format!(
            "{}\n{}\n",
            serde_json::json!({"type":"session_meta","payload":{"id":thread}}),
            serde_json::json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
        ),
    )?;
    let observed = observe_codex_rollout(&base, thread);
    std::fs::remove_dir_all(&base)?;
    assert_eq!(observed?.model, "gpt-6-luna");
    Ok(())
}

#[test]
fn explicit_write_root_keeps_read_only_git_authority() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let write = root.join("write");
    std::fs::create_dir(&write)?;
    let access = run::LaneAccess {
        write_roots: vec![write.clone()],
        ..Default::default()
    };
    let request = confinement_request(
        ProjectId::parse("0123456789abcdef0123456789abcdef")?,
        ConfinementRoots {
            cwd: &root,
            home: &root,
            scratch: &root,
            private_state: &root,
            assets: &root,
            prompt_file: None,
        },
        None,
        &access,
        &Default::default(),
        "codex",
        false,
    )?;
    assert!(matches!(request.permissions.mode, AccessMode::ReadOnly));
    assert_eq!(request.permissions.write_roots[0].path, write);
    Ok(())
}

#[test]
fn lane_plan_reads_the_active_release_root() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = Path::new("/project");
    let assets = Path::new("/release/sha256:active");
    let home = Path::new("/home/user");
    let scratch = Path::new("/tmp/lane-scratch");
    let private_state = Path::new("/state/attempt/provider-state");
    let prompt = Path::new("/state/attempt/prompt.md");
    for entry in REGISTRY {
        let request = confinement_request(
            ProjectId::parse("0123456789abcdef0123456789abcdef")?,
            ConfinementRoots {
                cwd,
                home,
                scratch,
                private_state,
                assets,
                prompt_file: Some(prompt),
            },
            None,
            &run::LaneAccess::default(),
            &Default::default(),
            entry.adapter.id(),
            true,
        )?;
        assert!(request.permissions.read_roots.contains(&PathGrant {
            path: assets.to_owned(),
            scope: PathScope::Subtree,
        }));
        let plan = ConfinedPlan::new(request)?;
        assert!(plan.permissions().read_roots.contains(&PathGrant {
            path: prompt.to_owned(),
            scope: PathScope::Exact,
        }));
        assert!(
            !plan
                .permissions()
                .read_roots
                .iter()
                .any(|grant| { grant.path == prompt.parent().expect("attempt") })
        );
    }
    Ok(())
}

#[test]
fn writers_read_the_primary_checkouts_untracked_scratch() -> Result<(), Box<dyn std::error::Error>>
{
    let project = tempfile::tempdir()?;
    let canonical = std::fs::canonicalize(project.path())?;
    let root = canonical.as_path();
    assert!(project_scratch_reads(root).is_empty());
    std::fs::create_dir_all(root.join(".provenant/scratch"))?;
    std::fs::create_dir(root.join(".agent-run"))?;
    std::fs::create_dir(root.join(".provenant/other"))?;
    let paths = |grants: Vec<PathGrant>| -> Vec<PathBuf> {
        assert!(grants.iter().all(|grant| grant.scope == PathScope::Subtree));
        grants.into_iter().map(|grant| grant.path).collect()
    };
    assert_eq!(
        paths(project_scratch_reads(root)),
        [root.join(".provenant/scratch"), root.join(".agent-run")]
    );
    // A link could point the grant anywhere.
    std::fs::remove_dir(root.join(".agent-run"))?;
    std::os::unix::fs::symlink("/", root.join(".agent-run"))?;
    assert_eq!(
        paths(project_scratch_reads(root)),
        [root.join(".provenant/scratch")]
    );
    Ok(())
}

#[test]
fn project_scratch_reads_rejects_a_symlinked_parent_outside_the_checkout()
-> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::create_dir(outside.path().join("scratch"))?;
    std::os::unix::fs::symlink(outside.path(), project.path().join(".provenant"))?;
    assert!(project_scratch_reads(project.path()).is_empty());
    // An in-checkout parent link is safe, and grants its canonical target.
    std::fs::remove_file(project.path().join(".provenant"))?;
    std::fs::create_dir_all(project.path().join("owned/scratch"))?;
    std::os::unix::fs::symlink("owned", project.path().join(".provenant"))?;
    let canonical = std::fs::canonicalize(project.path())?;
    assert_eq!(
        project_scratch_reads(project.path()),
        vec![PathGrant {
            path: canonical.join("owned/scratch"),
            scope: PathScope::Subtree,
        }]
    );
    Ok(())
}

#[test]
fn host_write_paths_reach_writers_only() {
    let host = provenant_confinement::run_config::RunConfig {
        writer_writes: vec!["/p/.agent-run/locks".into()],
        ..Default::default()
    };
    let access = run::LaneAccess {
        write_roots: vec!["/p/extra".into()],
        ..Default::default()
    };
    let paths = |writer| -> Vec<PathBuf> {
        write_roots(&access, &host, writer, "project")
            .into_iter()
            .map(|grant| grant.path)
            .collect()
    };
    assert_eq!(
        paths(true),
        [PathBuf::from("/p/extra"), "/p/.agent-run/locks".into()]
    );
    assert_eq!(paths(false), [PathBuf::from("/p/extra")]);
}

#[test]
fn writer_plan_roots_are_resolved_for_the_selected_project() {
    use provenant_confinement::run_config::{ProjectRunConfig, RunConfig};
    let config = RunConfig {
        writer_writes: vec!["/host/locks".into()],
        projects: [
            (
                "a".into(),
                ProjectRunConfig {
                    writer_writes: Some(vec!["/a/locks".into()]),
                    ..Default::default()
                },
            ),
            (
                "b".into(),
                ProjectRunConfig {
                    writer_writes: Some(vec!["/b/locks".into()]),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let roots = write_roots(&run::LaneAccess::default(), &config, true, "a");
    assert_eq!(
        roots,
        vec![PathGrant {
            path: "/a/locks".into(),
            scope: PathScope::Subtree
        }]
    );
    assert!(
        !roots
            .iter()
            .any(|grant| grant.path == Path::new("/b/locks"))
    );
}

#[test]
fn setup_stays_below_run_owned_roots_without_following_links()
-> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join(format!("provenant-setup-{}", std::process::id()));
    std::fs::create_dir(&base)?;
    let base = canonical(&base)?;
    let (scratch, state, outside) = (base.join("tmp"), base.join("state"), base.join("out"));
    for path in [&scratch, &state, &outside] {
        std::fs::create_dir(path)?;
    }
    let roots = [scratch.as_path(), state.as_path()];
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        apply_setup(
            &[
                SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                SetupStep::Symlink {
                    link: scratch.join("cargo-home/registry"),
                    target: outside.clone(),
                },
            ],
            &roots,
            None,
        )?;
        assert!(scratch.join("cargo-home/nested").is_dir());
        apply_setup(
            &[SetupStep::CreateDir(outside.join("projects/session"))],
            &roots,
            Some(&outside),
        )?;
        assert!(outside.join("projects/session").is_dir());
        assert!(
            apply_setup(
                &[SetupStep::CreateDir(outside.join("credentials/new"))],
                &roots,
                Some(&outside)
            )
            .is_err()
        );
        assert!(
            apply_setup(
                &[SetupStep::Symlink {
                    link: outside.join("projects/link"),
                    target: state.clone()
                }],
                &roots,
                Some(&outside)
            )
            .is_err()
        );
        std::os::unix::fs::symlink(&outside, scratch.join("escape"))?;
        for step in [
            SetupStep::CreateDir(outside.join("x")),
            SetupStep::CreateDir(scratch.join("../out/x")),
            SetupStep::CreateDir(scratch.clone()),
            SetupStep::CreateDir(scratch.join("escape/x")),
            SetupStep::Symlink {
                link: scratch.join("cargo-home/registry/x"),
                target: outside.clone(),
            },
        ] {
            assert!(apply_setup(&[step], &roots, None).is_err());
        }
        assert!(!outside.join("x").exists());
        Ok(())
    })();
    std::fs::remove_dir_all(&base)?;
    result
}

#[test]
fn provider_setup_recovers_after_interrupted_preparation_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    let base =
        std::env::temp_dir().join(format!("provenant-setup-recovery-{}", std::process::id()));
    std::fs::create_dir(&base)?;
    let base = canonical(&base)?;
    let (scratch, state) = (base.join("tmp"), base.join("state"));
    for path in [&scratch, &state] {
        std::fs::create_dir(path)?;
    }
    let roots = [scratch.as_path(), state.as_path()];
    let file = scratch.join("settings.json");
    let setup = |contents: &[u8]| {
        apply_setup(
            &[SetupStep::WriteFile {
                path: file.clone(),
                bytes: contents.to_vec(),
            }],
            &roots,
            None,
        )
    };
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        setup(b"first")?;
        // A crash after setup but before the launch claim leaves these
        // entries in place for the next preparation of the same attempt.
        setup(b"first")?;
        assert_eq!(std::fs::read(&file)?, b"first");

        setup(b"second")?;
        assert_eq!(std::fs::read(&file)?, b"second");
        Ok(())
    })();
    std::fs::remove_dir_all(&base)?;
    result
}

#[test]
fn a_task_may_read_only_its_own_nodes_graph_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = super::canonical(temp.path())?;
    let project = "p1";
    let context = |graph: &str, node: &str| provenant_records::run::WorkContext {
        graph_id: Some(graph.into()),
        node_id: Some(node.into()),
        ..Default::default()
    };
    let inputs = |graph: &str, node: &str| {
        super::graph_node_inputs_directory(&state, project, graph, node).expect("path-safe")
    };
    for (graph, node) in [("g", "mine"), ("g", "sibling"), ("other", "mine")] {
        std::fs::create_dir_all(inputs(graph, node))?;
    }
    let check = |context: Option<&provenant_records::run::WorkContext>, root: &Path| {
        super::check_graph_inputs_root(&state, project, context, Some(root))
    };
    let mine = context("g", "mine");
    // No grant requested, and its own directory, are accepted.
    assert!(super::check_graph_inputs_root(&state, project, None, None).is_ok());
    assert!(check(Some(&mine), &inputs("g", "mine")).is_ok());
    // The shared root, every ancestor, other nodes and other graphs are not.
    let shared = super::graph_inputs_directory(&state);
    for refused in [
        shared.clone(),
        shared.join(project),
        shared.join(project).join("g"),
        inputs("g", "sibling"),
        inputs("other", "mine"),
        inputs("g", "mine").join("nested"),
        inputs("g", "mine").join(".."),
        state.clone(),
    ] {
        assert!(
            check(Some(&mine), &refused).is_err(),
            "{} must be refused",
            refused.display()
        );
    }
    // A task naming no graph node has no directory to be granted.
    assert!(check(None, &inputs("g", "mine")).is_err());
    // Names a case-insensitive filesystem would merge, and names that are not
    // path-safe, still get separate directories, and one is refused the other.
    assert_ne!(inputs("g", "Review"), inputs("g", "review"));
    assert_ne!(inputs("G", "review"), inputs("g", "review"));
    assert_ne!(inputs("g", ".."), inputs("g", "."));
    for (graph, node) in [("g", "Review"), ("g", "review"), ("g", "..")] {
        std::fs::create_dir_all(inputs(graph, node))?;
    }
    let upper = context("g", "Review");
    let lower = context("g", "review");
    assert!(check(Some(&upper), &inputs("g", "Review")).is_ok());
    assert!(check(Some(&lower), &inputs("g", "review")).is_ok());
    assert!(check(Some(&upper), &inputs("g", "review")).is_err());
    assert!(check(Some(&lower), &inputs("g", "Review")).is_err());
    assert!(check(Some(&context("g", "..")), &shared.join(project).join("g")).is_err());
    // A link standing in for the node's directory is not followed.
    std::fs::remove_dir(inputs("g", "mine"))?;
    std::os::unix::fs::symlink(inputs("g", "sibling"), inputs("g", "mine"))?;
    assert!(check(Some(&mine), &inputs("g", "mine")).is_err());
    Ok(())
}

#[test]
fn prelaunch_login_failures_keep_auth_when_availability_is_absent()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::{ProviderError, accounts::Registration};
    assert!(!violates_account_pin(None, None, Some("other"), "codex"));
    assert!(!violates_account_pin(
        Some("key"),
        Some("codex"),
        Some("key"),
        "codex"
    ));
    assert!(violates_account_pin(
        Some("key"),
        Some("codex"),
        Some("other"),
        "codex"
    ));
    assert!(violates_account_pin(
        Some("key"),
        Some("codex"),
        Some("key"),
        "claude"
    ));
    let entries: Vec<Registration> = ["work", "personal"].into_iter().map(|label| {
        serde_json::from_value(serde_json::json!({"adapter": "codex", "label": label, "key": label, "directory": format!("/fixture/{label}")}))
    }).collect::<Result<_, _>>()?;
    let route: ResolvedRoute = serde_json::from_value(serde_json::json!({
        "adapter": "codex", "model": "example", "catalogue_revision": "fixture",
        "host": "fixture", "alternatives": [], "exclusions": [],
        "confidentiality": {"evidence_ref": "fixture"}, "eligibility": "eligible",
        "readiness": {"status": "unavailable", "reason": "unknown_availability"}
    }))?;
    let mut failures = std::collections::BTreeMap::new();
    for (entry, error) in entries.iter().zip([
        ProviderError::LoginRequired,
        ProviderError::CredentialNotFound,
    ]) {
        let AccountObservationError::AuthRequired(detail) =
            account_observation_error(entry, &error)
        else {
            panic!("login failure lost its type")
        };
        assert!(detail.contains("codex login"));
        assert!(detail.contains(&format!("CODEX_HOME='/fixture/{}'", entry.label)));
        failures.insert((entry.adapter.clone(), entry.key.clone()), detail);
    }
    assert!(unavailable_auth_detail(&route, None, &entries, &failures).is_some());
    failures.remove(&("codex".into(), "personal".into()));
    assert!(unavailable_auth_detail(&route, None, &entries, &failures).is_none());
    assert!(unavailable_auth_detail(&route, Some("work"), &entries, &failures).is_some());
    assert!(matches!(
        account_observation_error(&entries[0], &ProviderError::IdentityMismatch),
        AccountObservationError::Other(_)
    ));
    assert!(matches!(
        account_observation_error(&entries[0], &ProviderError::CredentialUnavailable),
        AccountObservationError::Other(_)
    ));
    Ok(())
}

#[test]
fn absent_default_login_differs_from_directory_integrity_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let missing = temp.path().join("missing-codex");
    assert_eq!(
        default_directory_error(&missing, "not found".into()).skip_kind(),
        SkipKind::AuthRequired
    );
    assert_eq!(
        default_directory_error(temp.path(), "permission denied".into()).skip_kind(),
        SkipKind::Account
    );
    #[cfg(unix)]
    {
        let broken = temp.path().join("broken-link");
        std::os::unix::fs::symlink(&missing, &broken)?;
        assert_eq!(
            default_directory_error(&broken, "broken link".into()).skip_kind(),
            SkipKind::Account
        );
    }
    assert_eq!(
        AccountObservationError::VersionUnavailable("codex executable missing".into()).skip_kind(),
        SkipKind::Unavailable
    );
    Ok(())
}

#[test]
fn staged_skill_files_are_written_outside_writable_scratch()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let temporary = temp.path().join("tmp");
    std::fs::create_dir_all(&temporary)?;
    let scratch = temporary.join("candidate-0");
    let stage = temporary.join("candidate-0-skills");
    apply_setup_directory(&[&temporary], &scratch)?;
    apply_setup_directory(&[&temporary], &stage)?;
    apply_setup(
        &[
            SetupStep::CreateDir(stage.join("example")),
            SetupStep::WriteFile {
                path: stage.join("example/SKILL.md"),
                bytes: b"staged".to_vec(),
            },
        ],
        &[&stage],
        None,
    )?;
    assert_eq!(std::fs::read(stage.join("example/SKILL.md"))?, b"staged");
    assert!(!stage.starts_with(&scratch));
    assert!(!scratch.starts_with(&stage));
    Ok(())
}
