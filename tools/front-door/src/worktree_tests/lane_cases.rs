// Split by lane mode so each test fits the per-test time budget (#2627);
// every case costs about 5 s of confined preparation and execution.

// The Claude read-only case runs in the nightly native harness tier.

/// A writer builds and tests a SwiftPM package in its registered worktree.
/// The real compile runs in the nightly native tier with a longer timeout.
/// This fixture prepares and executes the lane directly, so it has no queued
/// writer event to wait for.
use super::*;

pub(super) fn lane_prepare_isolates_nested_state() -> Result {
    use provenant_run::Resources;
    use provenant_supervisor::census::{ProcessObservation, ProcessObserver};

    let pid = i64::from(std::process::id());
    let ProcessObservation::Present(fact) =
        provenant_supervisor::fixture::RecordedBootObserver.observe(pid)
    else {
        return Err("fixture process identity unavailable".into());
    };
    assert_eq!(FixtureResources.guardian_identity(), Some(fact.identity));
    lane_prepare_cases(&[("codex", false, false, 91)])
}

pub(super) fn lane_prepare_isolates_nested_claude_state() -> Result {
    lane_prepare_cases(&[("claude", false, false, 93)])
}

pub(super) fn lane_prepare_isolates_nested_writer_state() -> Result {
    lane_prepare_cases(&[("codex", true, false, 92), ("claude", true, false, 94)])
}

pub(super) fn lane_prepare_isolates_nested_publishing_state() -> Result {
    lane_prepare_cases(&[("codex", true, true, 95), ("claude", true, true, 96)])
}

pub(super) fn lane_builds_and_tests_swift_in_a_registered_worktree() -> Result {
    let selected = cfg!(target_os = "macos")
        && Path::new("/usr/bin/swift").exists()
        && provenant_providers::swift_toolchain_available(&|name| std::env::var_os(name));
    if !selected {
        provenant_test_support::skip_sandbox_dependent_test("no Swift toolchain is selected")?;
        return Ok(());
    }
    lane_prepare_with(&[("claude", true, false, 97)], Probe::Swift)
}

pub(super) fn read_only_lane_builds_and_tests_swift_in_scratch() -> Result {
    let selected = cfg!(target_os = "macos")
        && Path::new("/usr/bin/swift").exists()
        && provenant_providers::swift_toolchain_available(&|name| std::env::var_os(name));
    if !selected {
        provenant_test_support::skip_sandbox_dependent_test("no Swift toolchain is selected")?;
        return Ok(());
    }
    lane_prepare_with(&[("claude", false, false, 98)], Probe::SwiftReadOnly)
}

pub(super) fn read_only_lane_builds_and_tests_cargo_in_scratch() -> Result {
    lane_prepare_with(&[("claude", false, false, 99)], Probe::CargoReadOnly)
}

/// Which native proof a lane case runs inside its compiled profile.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Probe {
    Shell,
    Swift,
    SwiftReadOnly,
    CargoReadOnly,
}

fn lane_prepare_cases(cases: &[(&str, bool, bool, u8)]) -> Result {
    lane_prepare_with(cases, Probe::Shell)
}

fn lane_prepare_with(cases: &[(&str, bool, bool, u8)], probe: Probe) -> Result {
    use provenant_confinement::permissions::PathScope;
    let rig = repository()?;
    let cleanup = provenant_supervisor::fixture::ProcessCleanupGuard::new(&rig.state);
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    ok(
        &rig,
        &[
            "worktree",
            "create",
            "nested-child",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let worktree = rig.project.join(".worktrees/writer");
    // The repository's Cargo config names a checked-in rustc wrapper.
    fs::create_dir_all(worktree.join(".cargo"))?;
    fs::write(
        worktree.join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \".cargo/rustc-wrapper.sh\"\n",
    )?;
    fs::write(
        worktree.join(".cargo/rustc-wrapper.sh"),
        "#!/bin/sh\nexec \"$@\"\n",
    )?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            worktree.join(".cargo/rustc-wrapper.sh"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    let worktree_arg = worktree.to_str().ok_or("worktree path")?;
    git(&rig, &["-C", worktree_arg, "add", ".cargo"])?;
    git(&rig, &["-C", worktree_arg, "commit", "-q", "-m", "wrapper"])?;
    let (roots, context) = writer_context(&rig)?;
    let identity = command(&rig, &["fabric", "identity", "--json"])?;
    assert_eq!(
        code(&identity),
        0,
        "{}",
        String::from_utf8_lossy(&identity.stderr.bytes)
    );
    // The host owns and initialises the Fabric store before a lane opens it
    // read-only. Identity alone does not create the coordination database.
    ok(&rig, &["fabric", "events", "read", "--json"])?;
    let home = rig.root.join("fixture-home");
    fs::create_dir_all(home.join(".claude-fixture"))?;
    if !cfg!(target_os = "macos") {
        // Off macOS, Claude keeps its login in the account directory, not a keychain.
        fs::write(
            home.join(".claude-fixture/.credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#,
        )?;
    }
    fs::create_dir_all(home.join(".codex"))?;
    fs::write(
        home.join(".codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-one"}}"#,
    )?;
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        fs::create_dir_all(home.join(path))?;
    }
    fs::write(home.join(".rustup/settings.toml"), b"fixture settings")?;
    for secret in ["credentials.toml", "credentials"] {
        fs::write(home.join(".cargo").join(secret), b"synthetic credential")?;
    }
    // Cargo fails when its user config exists but is unreadable, so lanes
    // read it; registry tokens belong in the denied credentials files.
    fs::write(home.join(".cargo/config.toml"), b"[net]\nretry = 1\n")?;
    fs::create_dir_all(home.join("Library/Keychains"))?;
    fs::write(
        home.join("Library/Keychains/login.keychain-db"),
        b"synthetic credential",
    )?;
    // Clone immutable tools into the isolated home. No test reads host
    // provider configuration, login state or Cargo credentials.
    #[cfg(target_os = "macos")]
    if probe == Probe::Shell
        && cases.iter().any(|&(adapter, writer, publishing, _)| {
            native_shell_probe_case(adapter, writer, publishing)
        })
        || probe == Probe::CargoReadOnly
    {
        clone_rust_toolchain(&rig, &home)?;
    }
    let executable = rig.root.join("provider/bin/codex");
    fs::create_dir_all(executable.parent().ok_or("provider parent")?)?;
    rig.copy_executable(&std::env::current_exe()?, &executable)?;
    // Publishing must never depend on gh or login state from the host PATH.
    std::os::unix::fs::symlink(&executable, executable.with_file_name("gh"))?;
    // Production preparation needs registered, freshly observed accounts.
    let home = fs::canonicalize(&home)?;
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let mut accounts = String::new();
    let mut claude_key = String::new();
    let probe_root = tempfile::tempdir()?;
    for (adapter, label, directory) in [
        ("codex", "default", home.join(".codex")),
        ("claude", "fixture", home.join(".claude-fixture")),
    ] {
        let key = provenant_providers::accounts::detect(
            &executor,
            adapter,
            &executable,
            &home,
            "fixture".as_ref(),
            &directory,
            probe_root.path(),
        )?;
        if adapter == "claude" {
            claude_key.clone_from(&key);
        }
        accounts.push_str(&format!(
            "[domains.accounts.entries.{adapter}-{label}]\nadapter={adapter:?}\nlabel={label:?}\nkey={key:?}\ndirectory={:?}\n",
            directory.to_str().ok_or("account path")?
        ));
    }
    // The Fabric MCP server is opt-in; these cases drive it inside the lane.
    let config = |depth: u8| {
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[knobs]\n'lanes.max_depth'={depth}\n'lanes.sandbox'='on'\n'fabric.mcp_server'=true\n{accounts}"
        )
    };
    fs::write(rig.state.join("config.toml"), config(1))?;
    for &(adapter, writer, publishing, seed) in cases {
        if publishing {
            fs::create_dir_all(home.join(".config/gh"))?;
        }
        let request =
            provenant_run::resolve_writer(roots.state.as_path(), &rig.project, &worktree)?;
        let mut prepared = writer_run(
            request,
            &format!("lane-prepare-{adapter}-{writer}-{publishing}"),
            seed,
            "fixture prompt",
        )?;
        let task = &mut prepared.request.tasks[0];
        if !writer {
            task.writer = None;
        }
        // These cases exercise confined state and write boundaries, so opt in
        // explicitly instead of relying on the default execution mode.
        task.profile
            .knobs
            .insert("lanes.sandbox".into(), serde_json::json!("on"));
        // Select publishing explicitly; missing host configuration does not
        // disable it because the login may live in a relocated store or keychain.
        task.profile
            .knobs
            .insert("lanes.publish".into(), serde_json::json!(publishing));
        // Catalogue models only: production refuses unverified exact IDs.
        task.route = provenant_run::RequestedRoute::Model {
            extra: Default::default(),
            provider: adapter.into(),
            model: if adapter == "codex" {
                "codex/gpt-6-luna"
            } else {
                "claude/claude-haiku-5-5"
            }
            .into(),
            effort: None,
        };
        let attempt = roots.state.as_path().join(format!(
            "runs/{}/attempts/{}",
            prepared.run_id, prepared.attempts[0].attempt_id
        ));
        // Launch preparation links a short alias to `tmp` on macOS. The name
        // is unique to this process and attempt, and the rig unlinks it.
        let temporary_alias = cfg!(target_os = "macos").then(|| {
            let id = u128::from_be_bytes(*prepared.attempts[0].attempt_id.as_bytes());
            let alias = PathBuf::from(format!(
                "/private/tmp/pv-{:08x}{:024x}",
                std::process::id(),
                id & ((1 << 96) - 1)
            ));
            let _ = std::os::unix::fs::symlink(attempt.join("tmp"), &alias);
            alias
        });
        let paths = provenant_run::LaunchPaths {
            attempt_id: prepared.attempts[0].attempt_id,
            lane_depth: 1,
            run_id: prepared.run_id,
            operation_id: prepared.operation_id.clone(),
            task_index: 0,
            deadline_unix_ms: 4_000_000_000_000,
            scratch: attempt.join("scratch"),
            provider_state: attempt.join("provider-state"),
            temporary: attempt.join("tmp"),
            temporary_alias,
        };
        // The active release root is its own directory, never the project.
        let release = rig.state.with_file_name("release");
        fs::create_dir_all(&release)?;
        let registry = provenant_cli::run::Registry {
            roots: Some(&roots),
            home: Some(&home),
            assets: Some(&release),
            cwd: Some(&rig.project),
            state_root: Some(roots.state.as_path()),
            project_id: Some(context.project_id),
            host: None,
            cancel: None,
            repo_facts: Default::default(),
        };
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = writer_call(&clock, &cancel)?;
        let store = provenant_run::RunStore::open(&context, &call)?;
        store.create(&prepared, &call)?;
        let task = &prepared.request.tasks[0];
        for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
            fs::create_dir_all(path)?;
        }
        // Bound to the registered account, as the host loader binds it.
        let token = provenant_providers::claude::refresh_for_account(
            &FixtureCredential,
            &provenant_providers::claude::Login {
                executable: &executable,
                home: &home,
                user: "fixture".as_ref(),
                config_dir: &home.join(".claude-fixture"),
            },
            &provenant_providers::identity::RegisteredAccount::new(
                "claude",
                &claude_key,
                home.join(".claude-fixture"),
            )?,
            &|| 1000,
            2000,
        )?;
        // Prepared below lanes.max_depth, so its children may launch; the
        // limit then drops to exercise refusal at dispatch time.
        fs::write(rig.state.join("config.toml"), config(2))?;
        let mut launch = provenant_cli::run::prepare_fixture(
            &registry,
            task,
            "fixture prompt",
            &paths,
            &executable,
            Some(&token),
        )
        .map_err(|e| format!("prepare {adapter} writer={writer}: {e}"))?;
        assert_eq!(launch.route.provider.as_deref(), Some(adapter));
        fs::write(rig.state.join("config.toml"), config(1))?;
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == provenant_run::LANE_DEPTH_ENV && value == "1")
        );
        let confinement = launch.confinement.as_ref().ok_or("confinement")?;
        assert_eq!(
            confinement.plan.permissions().writer_publishing(),
            publishing
        );
        let selected_git =
            fs::canonicalize(provenant_os::git::git_executable().ok_or("host Git")?)?;
        let scratch_git = confinement.plan.permissions().scratch.path.join("bin/git");
        let git_entry = fs::symlink_metadata(&scratch_git)?.file_type();
        assert!(
            git_entry.is_file(),
            "Git wrapper for {adapter} writer={writer} publishing={publishing}"
        );
        {
            // Repository lanes bind their private Git directory in a wrapper.
            // The wrapper runs the selected Git, and host global config does
            // not reach it.
            let leak = rig.root.join("wrapper-home");
            fs::create_dir_all(&leak)?;
            fs::write(leak.join(".gitconfig"), "[user]\n\tname = host-leak\n")?;
            let run = |args: &[&str]| {
                std::process::Command::new(&scratch_git)
                    .args(args)
                    .env("HOME", &leak)
                    .env("XDG_CONFIG_HOME", leak.join("xdg"))
                    .env_remove("GIT_CONFIG_GLOBAL")
                    .output()
            };
            let version = run(&["--version"])?;
            let expected = std::process::Command::new(&selected_git)
                .arg("--version")
                .output()?;
            assert_eq!(version.stdout, expected.stdout, "wrapper runs selected Git");
            let name = run(&["config", "--global", "--get", "user.name"])?;
            assert!(
                !String::from_utf8_lossy(&name.stdout).contains("host-leak"),
                "host global config must not reach the wrapper"
            );
        }
        let scratch = &confinement.plan.permissions().scratch.path;
        assert_eq!(scratch.join("publishing.gitconfig").exists(), publishing);
        assert_eq!(scratch.join("bin/gh").exists(), publishing);
        if publishing {
            let gh_wrapper = fs::read_to_string(scratch.join("bin/gh"))?;
            assert!(gh_wrapper.contains(&executable.display().to_string()));
        }
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == "PROVENANT_GIT" && value == scratch_git.as_os_str())
        );
        for selector in [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_INDEX_FILE",
        ] {
            assert!(
                launch.environment.iter().all(|(key, _)| key != selector),
                "private Git binding must stay in the wrapper: {selector}"
            );
        }
        assert!(
            confinement
                .profile
                .spec
                .process_executables
                .iter()
                .any(|grant| grant.path == selected_git && grant.scope == PathScope::Exact)
        );
        let permissions = confinement.plan.permissions();
        assert_eq!(
            permissions.scratch.path,
            paths.temporary.join("candidate-0")
        );

        if adapter == "codex" {
            assert!(
                fs::symlink_metadata(
                    permissions
                        .private_state
                        .as_ref()
                        .ok_or("private state")?
                        .path
                        .join("auth.json")
                )?
                .file_type()
                .is_symlink()
            );
        } else {
            let generated = permissions.scratch.path.join("provenant-mcp.json");
            let mcp: Value = serde_json::from_slice(&fs::read(generated)?)?;
            assert_eq!(
                mcp["mcpServers"]["fabric"]["env"]["PROVENANT_GIT"],
                selected_git.to_str().ok_or("Git path")?
            );
            assert_eq!(
                mcp["mcpServers"]["fabric"]["env"][provenant_run::LANE_DEPTH_ENV],
                "1"
            );
            let settings: Value = serde_json::from_slice(&fs::read(
                permissions.scratch.path.join("provenant-settings.json"),
            )?)?;
            assert!(
                settings["hooks"]
                    .as_object()
                    .is_some_and(|hooks| !hooks.is_empty())
            );
        }
        for grant in &confinement.profile.spec.state_writes {
            assert!(
                !grant.overlaps(&permissions.scratch),
                "scratch overlaps {grant:?}"
            );
            assert!(
                !grant.overlaps(permissions.private_state.as_ref().ok_or("private state")?),
                "private state overlaps {grant:?}"
            );
            for denied in [
                attempt.join("quarantine"),
                attempt.join("quarantine-result.json"),
                attempt.join("custody.lock"),
                roots
                    .state
                    .as_path()
                    .join("runs/other/attempts/other/custody.lock"),
            ] {
                assert!(
                    !(grant.path == denied
                        || (grant.scope == PathScope::Subtree && denied.starts_with(&grant.path))),
                    "custody writable through {grant:?}"
                );
            }
        }
        let frames = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":999,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"fabric_run\",\"arguments\":{\"op\":\"run\",\"use\":\"task\",\"brief\":\"read only\",\"explain\":true}}}\n"
        );
        let mut options = provenant_os::exec::ExecOptions::new(
            &launch.argv,
            &launch.cwd,
            Duration::from_secs(10),
        );
        options.env = Some(&launch.environment);
        options.stdin = Some(frames.as_bytes());
        options.stdout = provenant_os::exec::StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = provenant_os::exec::StreamPolicy::Capture { limit: 64 * 1024 };
        let output = provenant_os::exec::SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let replies = output
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let result = &replies
            .iter()
            .find(|reply| reply["id"] == 1)
            .ok_or("MCP reply")?["result"];
        assert_lane_child_preview(result)?;
        assert!(!roots.state.as_path().join("tmp/results").exists());
        assert!(permissions.scratch.path.join("results").is_dir());
        assert!(!rig.state.join("children").exists());
        assert!(!worktree.join("provenant-mcp.json").exists());
        assert!(!worktree.join("provenant-settings.json").exists());
        assert_eq!(
            git(
                &rig,
                &[
                    "-C",
                    worktree.to_str().ok_or("worktree path")?,
                    "status",
                    "--porcelain"
                ]
            )?,
            ""
        );
        let parent_context = provenant_run::HolderContext {
            schema_version: 1,
            roots: provenant_run::RootSeed {
                home: home.clone(),
                checkout: rig.project.clone(),
                state_root: rig.state.clone(),
            },
            project_id: context.project_id,
            actor_id: context.actor_id,
            run_id: prepared.run_id,
            attempt_id: paths.attempt_id,
            host_delivery: None,
            launch: Some(provenant_run::HostLaunchContext {
                graph_directory: None,
                configuration_root: rig.state.clone(),
                cwd: launch.cwd.clone(),
                executables: [("codex".into(), executable.clone())].into(),
                executable_search_path: Vec::new(),
                git: selected_git.clone(),
                default_account_directories: Default::default(),
                credential_roots: Default::default(),
                github_roots: Default::default(),
                provider_path_environment: Default::default(),
                cargo_build_jobs: None,
                fixture_state_write: true,
            }),
            sealed: Some(provenant_run::SealedTask {
                request_digest: prepared.request.digest()?,
                task_id: task.task_id,
                effective_network: task.access.network(task.writer.is_some()),
                effective_database_ipc: task.access.database_ipc,
            }),
        };
        verify_lane_profile(
            &mut launch,
            &attempt,
            roots.state.as_path(),
            frames.as_bytes(),
            &config,
            &parent_context,
            (native_shell_probe_case(adapter, writer, publishing)
                || (!writer && adapter == "claude" && probe == Probe::CargoReadOnly)
                || (!writer && adapter == "claude" && probe == Probe::SwiftReadOnly))
                .then_some(probe),
        )
        .map_err(|e| format!("confined {adapter} writer={writer}: {e}"))?;
    }
    cleanup.disarm();
    Ok(())
}

/// Fake provider forwards no provider environment to its MCP child, only a
/// baseline PATH as real providers do.
pub(crate) fn lane_provider() -> Result {
    if std::env::var(provenant_run::LANE_DEPTH_ENV).as_deref() == Ok("2") {
        let mut prompt = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut prompt)?;
        let state = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("child CODEX_HOME")?);
        fs::write(
            state.join("fake-child-launched"),
            b"confined child executed",
        )?;
        writeln!(
            std::io::stdout(),
            "{{\"type\":\"thread.started\",\"thread_id\":\"fixture-child\"}}"
        )?;
        if prompt.contains("wait for host cancellation") {
            thread::park_timeout(Duration::from_secs(5));
        }
        return Ok(());
    }
    use provenant_os::exec::ExecOptions;
    use provenant_os::exec::Executor;
    use provenant_os::exec::StreamPolicy;
    use provenant_os::exec::SystemExecutor;
    let mut entry = serde_json::Map::new();
    for arg in std::env::args().filter(|arg| arg.starts_with("mcp_servers.fabric.")) {
        let value: Value = toml_edit::de::from_str(&arg)?;
        entry.extend(
            value["mcp_servers"]["fabric"]
                .as_object()
                .ok_or("MCP config")?
                .clone(),
        );
    }
    if entry.is_empty() {
        let args: Vec<_> = std::env::args_os().collect();
        let index = args
            .iter()
            .position(|arg| arg == "--mcp-config")
            .ok_or("MCP config flag")?;
        let config: Value =
            serde_json::from_slice(&fs::read(args.get(index + 1).ok_or("MCP config path")?)?)?;
        entry = config["mcpServers"]["fabric"]
            .as_object()
            .ok_or("MCP entry")?
            .clone();
    }
    let mut argv = vec![OsString::from(
        entry["command"].as_str().ok_or("MCP command")?,
    )];
    argv.extend(
        entry["args"]
            .as_array()
            .ok_or("MCP args")?
            .iter()
            .map(|arg| arg.as_str().map(OsString::from).ok_or("MCP arg"))
            .collect::<std::result::Result<Vec<_>, _>>()?,
    );
    let env = entry["env"]
        .as_object()
        .ok_or("MCP env")?
        .iter()
        .map(|(key, value)| {
            Ok((
                OsString::from(key),
                OsString::from(value.as_str().ok_or("MCP env value")?),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    // A real provider hands its MCP child a baseline PATH. Mirror that with
    // this fake's own directory first, so the nested dispatch resolves it.
    let mut env = env;
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        env.push(("TMPDIR".into(), tmp));
    }
    if env.iter().all(|(key, _)| key != "PATH") {
        let mut directories = vec![
            std::env::current_exe()?
                .parent()
                .ok_or("provider bin")?
                .to_path_buf(),
        ];
        directories.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        env.push(("PATH".into(), std::env::join_paths(directories)?));
    }
    let mut input = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut input)?;
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(8));
    options.env = Some(&env);
    options.stdin = Some(&input);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
    let output = SystemExecutor::new(1)?
        .execute(options)
        .map_err(|e| format!("MCP spawn: {e:?}"))?;
    std::io::stdout().write_all(&output.stdout.bytes)?;
    std::io::stderr().write_all(&output.stderr.bytes)?;
    if code(&output) != 0 {
        return Err("configured MCP child failed".into());
    }
    let stripped = env
        .iter()
        .filter(|(key, _)| {
            key != provenant_run::LANE_DEPTH_ENV && key != provenant_run::PARENT_ATTEMPT_ENV
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(8));
    options.env = Some(&stripped);
    // Explanation is side-effect free. An actual child request still needs
    // the parent relay identity even if a provider drops its lane variables.
    let stripped_input =
        String::from_utf8(input.clone())?.replace("\"explain\":true", "\"explain\":false");
    options.stdin = Some(stripped_input.as_bytes());
    let stripped_output = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(
        code(&stripped_output),
        0,
        "{}",
        String::from_utf8_lossy(&stripped_output.stderr.bytes)
    );
    let reply = stripped_output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        reply
            .iter()
            .find(|reply| reply["id"] == 1)
            .map(|reply| mcp_error_code(&reply["result"]))
            .ok_or("stripped MCP reply")?,
        "graph_relay_unavailable"
    );
    // Exercise the same publisher used by CLI overflow under confinement.
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?);
    let assets = PathBuf::from(std::env::var_os("PROVENANT_ROOT").ok_or("assets")?);
    let mut inputs = provenant_core::RootInputs::new(home, assets);
    inputs.state_dir = Some(PathBuf::from(entry["args"][1].as_str().ok_or("state arg")?));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_cli::fabric::publish(
        &provenant_records::CommandResult::new(
            "fixture",
            provenant_records::Outcome::Ok,
            "overflow",
        ),
        &roots,
    )?;
    Ok(())
}

// Share the lane-side discovery decision with its injected PATH fixture.
pub(super) const LANE_NODE_PNPM_PROBE: &str = concat!(
    "set -eu; if command -v node >/dev/null && command -v pnpm >/dev/null; then ",
    "node --version; pnpm --version; ",
    "node \"$TMPDIR/cargo-probe/child-process.cjs\"; ",
    "pnpm --dir \"$TMPDIR/cargo-probe\" run compat; ",
    "printf 'lane-node-pnpm-compat: PASS\\n'; ",
    "elif [ \"${PROVENANT_LANE_TOOLS_REQUIRED:-0}\" = 1 ]; then ",
    "echo 'required node/pnpm unavailable' >&2; exit 15; ",
    "else echo 'SKIP #1: node/pnpm unavailable' >&2; fi"
);

pub(super) const LANE_NEXTEST_PROBE: &str = concat!(
    "set -eu; if cargo nextest --version >/dev/null 2>&1; then ",
    "CARGO_BUILD_JOBS=2 cargo nextest run --offline --manifest-path \"$TMPDIR/cargo-probe/Cargo.toml\"; ",
    "printf 'lane-nextest: PASS\\n'; ",
    "elif [ \"${PROVENANT_LANE_TOOLS_REQUIRED:-0}\" = 1 ]; then ",
    "echo 'required cargo-nextest unavailable' >&2; exit 15; ",
    "else echo 'SKIP: cargo-nextest unavailable' >&2; fi"
);

/// Only the narrowest Claude profile that actually grants a shell can run the
/// native cargo probe: read-only Claude has neither a worktree write nor cargo
/// on PATH, so its probe would test a profile that cannot build. A successful
/// confined build for a non-publishing writer covers the wider publishing row.
pub(super) fn native_shell_probe_case(adapter: &str, writer: bool, publishing: bool) -> bool {
    adapter == "claude" && writer && !publishing
}

fn verify_lane_profile(
    launch: &mut provenant_run::PreparedLaunch,
    attempt: &Path,
    state: &Path,
    frames: &[u8],
    _config: &dyn Fn(u8) -> String,
    parent: &provenant_run::HolderContext,
    probe: Option<Probe>,
) -> Result {
    use provenant_confinement::permissions::{
        AccessMode, AnchoredDirectory, Anchors, BackendContext, FixtureOperation, FixtureOutcome,
        prepare_writer_directories, required_anchors, system_backend,
    };
    use provenant_os::exec::StreamPolicy;
    use provenant_os::exec::SystemExecutor;

    let executor = SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    let backend = match system_backend(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        #[cfg(target_os = "macos")]
        env: &[],
    }) {
        Ok(backend) => backend,
        Err(error) => {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "lane profile execution skipped: {error}"
            ))?;
            return Ok(());
        }
    };
    let confinement = launch.confinement.as_mut().ok_or("confinement")?;
    // Synthetic fake-provider evidence exercises the compiler only. This does
    // not activate Codex or replace its missing live state-write recording.
    let profile = &mut confinement.profile;
    profile
        .observations
        .insert(FixtureOperation::StateWrite, FixtureOutcome::Allowed);
    let writer = confinement
        .writer
        .as_ref()
        .map(|w| (&w.worktree, &w.branch));
    // Run creates the writer's ref parents and lane cache under custody.
    if let (Some((worktree, branch)), AccessMode::WorktreeWrite(authority)) =
        (writer, &confinement.plan.permissions().mode)
    {
        prepare_writer_directories(worktree, branch, authority, &confinement.home)?;
    }
    if let Some(writer) = confinement.writer.as_ref() {
        writer.prepare_private_git_for_fixture(
            &provenant_os::fs::Directory::open_anchor(attempt)?,
            &confinement.plan.permissions().scratch.path,
        )?;
    }
    let directories = required_anchors(confinement.plan.permissions(), profile, writer)
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &confinement.home,
        directories: &directories,
        worktree: writer,
    };
    let compiled = confinement
        .plan
        .compile(backend.as_ref(), profile, &anchors)?;
    // Start the same host holder used by internal supervise. This manually
    // compiled parent has no guardian of its own, so its fixture holder has no
    // admission or terminal work; all child handoff/custody remains real.
    struct ParentFixture;
    impl provenant_supervisor::Holder for ParentFixture {
        fn admit(
            &self,
            _: &[u8],
            _: &provenant_os::fs::Directory,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
        fn terminal(
            &self,
            _: &[u8],
            _: &provenant_os::fs::Directory,
            _: &provenant_supervisor::custody::ExitObservation,
            _: &provenant_supervisor::custody::Recovery,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
    }
    use provenant_supervisor::Holder;
    let host = provenant_cli::run::HostHolder::new(&ParentFixture);
    host.admit(
        &parent.encode()?,
        &provenant_os::fs::Directory::open_anchor(attempt)?,
    )?;
    // Native shell proof, separate from live provider/Bash/token-scrub proof.
    // Read-only Claude has no cargo on PATH, so the narrowest probe-capable
    // profile is a non-publishing writer.
    if cfg!(target_os = "macos") && matches!(probe, Some(Probe::Swift | Probe::SwiftReadOnly)) {
        // The read-only fixture stays in the worktree so this also catches
        // SwiftPM output that ignores its scratch-path setting.
        let package = launch.cwd.join(".swift-probe");
        fs::create_dir_all(package.join("Sources/LaneProbe"))?;
        fs::create_dir_all(package.join("Tests/LaneProbeTests"))?;
        fs::write(
            package.join("Package.swift"),
            "// swift-tools-version:5.9\nimport PackageDescription\nlet package = Package(name: \"LaneProbe\", targets: [.target(name: \"LaneProbe\"), .testTarget(name: \"LaneProbeTests\", dependencies: [\"LaneProbe\"])])\n",
        )?;
        fs::write(
            package.join("Sources/LaneProbe/LaneProbe.swift"),
            "public func answer() -> Int { 42 }\n",
        )?;
        fs::write(
            package.join("Tests/LaneProbeTests/LaneProbeTests.swift"),
            "import XCTest\n@testable import LaneProbe\nfinal class LaneProbeTests: XCTestCase { func testAnswer() { XCTAssertEqual(answer(), 42) } }\n",
        )?;
        let command = if probe == Some(Probe::SwiftReadOnly) {
            "set -eu; tmp=$(cd \"$TMPDIR\" && pwd -P); test \"$(command -v swift)\" = \"$tmp/bin/swift\"; cd .swift-probe; swift build; swift test; test ! -e .build; printf 'lane-swift-read-only: PASS\\n'"
        } else {
            "set -eu; tmp=$(cd \"$TMPDIR\" && pwd -P); test \"$(command -v swift)\" = \"$tmp/bin/swift\"; cd .swift-probe; swift build; swift test; printf 'lane-swift: PASS\\n'"
        };
        let argv = ["/bin/sh", "-c", command].map(OsString::from);
        let command = compiled.command(
            &argv,
            &provenant_core::AttemptId::from_bytes([0; 16]),
            &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
        )?;
        let mut options = command.options(&launch.cwd, Duration::from_secs(120))?;
        options.process_group = true;
        options.cleanup_process_group = true;
        options.env = Some(&launch.environment);
        let output = executor.execute(options)?;
        assert_eq!(
            code(&output),
            0,
            "swift probe: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let marker = if probe == Some(Probe::SwiftReadOnly) {
            "lane-swift-read-only: PASS"
        } else {
            "lane-swift: PASS"
        };
        assert!(String::from_utf8_lossy(&output.stdout.bytes).contains(marker));
        if probe == Some(Probe::Swift) {
            // Nested child admission is exercised by the lane preparation
            // fixtures; it is unrelated to this writer's Swift build proof.
            host.provider_exited(&parent.encode()?, None)?;
            return Ok(());
        }
    }
    if cfg!(target_os = "macos") && probe == Some(Probe::CargoReadOnly) {
        let scratch = &confinement.plan.permissions().scratch.path;
        let project = launch.cwd.join(".cargo-probe");
        fs::create_dir_all(project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = 'lane-probe'\nversion = '0.1.0'\nedition = '2024'\n\n[workspace]\n",
        )?;
        let lock = project.join("Cargo.lock");
        fs::write(
            &lock,
            "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"lane-probe\"\nversion = \"0.1.0\"\n",
        )?;
        fs::copy(&lock, scratch.join("cargo-probe.lock"))?;
        fs::write(
            project.join("src/lib.rs"),
            "/// ```\n/// assert_eq!(2 + 2, 4);\n/// ```\npub fn doc() {}\n#[test] fn scratch_build_is_private() { assert_eq!(2 + 2, 4); }\n",
        )?;
        let argv = [
            "/bin/sh",
            "-c",
            "set -eu; tmp=$(cd \"$TMPDIR\" && pwd -P); test \"$CARGO_TARGET_DIR\" = \"$tmp/target\"; cd .cargo-probe; PATH=\"$HOME/.cargo/bin:$PATH\" cargo test -p lane-probe --offline; cmp Cargo.lock \"$TMPDIR/cargo-probe.lock\"; test ! -e target; printf 'lane-cargo-read-only: PASS\\n'",
        ]
        .map(OsString::from);
        let command = compiled.command(
            &argv,
            &provenant_core::AttemptId::from_bytes([0; 16]),
            &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
        )?;
        let mut options = command.options(&launch.cwd, Duration::from_secs(120))?;
        options.process_group = true;
        options.cleanup_process_group = true;
        options.env = Some(&launch.environment);
        let output = executor.execute(options)?;
        assert_eq!(
            code(&output),
            0,
            "read-only Cargo probe: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout.bytes).contains("lane-cargo-read-only: PASS")
        );
        let stdout = String::from_utf8_lossy(&output.stdout.bytes);
        assert!(stdout.contains("scratch_build_is_private ... ok"));
        // rustdoc compiles and executes the doc-test from scratch as well.
        assert!(
            stdout.contains("src/lib.rs - doc (line 1) ... ok"),
            "{stdout}"
        );
    }
    if cfg!(target_os = "macos") && probe == Some(Probe::Shell) {
        let scratch = &confinement.plan.permissions().scratch.path;
        let project = scratch.join("cargo-probe");
        fs::create_dir_all(project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = 'lane-probe'\nversion = '0.1.0'\nedition = '2024'\n",
        )?;
        fs::write(
            project.join("src/lib.rs"),
            "#[test] fn writer_can_test() { assert_eq!(env!(\"LANE_JOBSERVER\"), \"verified\"); }\n",
        )?;
        fs::write(
            project.join("build.rs"),
            r#"
use std::{env, fs::File, io::{Read, Write}, mem::ManuallyDrop, os::fd::FromRawFd};
fn main() {
    let flags = env::var("CARGO_MAKEFLAGS").expect("Cargo jobserver flags");
    let auth = flags.split_whitespace().find_map(|flag|
        flag.strip_prefix("--jobserver-auth=")
            .or_else(|| flag.strip_prefix("--jobserver-fds="))
    ).expect("inherited jobserver descriptors");
    let (read, write) = auth.split_once(',').expect("jobserver pipe pair");
    // Borrow Cargo's inherited descriptors; leave ownership with Cargo.
    let mut reader = ManuallyDrop::new(unsafe { File::from_raw_fd(read.parse().unwrap()) });
    let mut writer = ManuallyDrop::new(unsafe { File::from_raw_fd(write.parse().unwrap()) });
    let mut token = [0];
    reader.read_exact(&mut token).expect("acquire second jobserver token");
    writer.write_all(&token).expect("return jobserver token");
    println!("cargo:rustc-env=LANE_JOBSERVER=verified");
}
"#,
        )?;
        fs::write(
            project.join("package.json"),
            r#"{"private":true,"scripts":{"compat":"node child-process.cjs"}}"#,
        )?;
        fs::write(
            project.join("child-process.cjs"),
            r#"
const assert = require('node:assert/strict');
const {spawnSync, fork} = require('node:child_process');
if (process.argv[2] === 'child') {
    process.send(42, () => process.disconnect());
} else {
    const result = spawnSync(process.execPath, ['-e', 'process.stdout.write("42")'], {encoding: 'utf8'});
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, '42');
    let received = false;
    const child = fork(__filename, ['child'], {stdio: ['ignore', 'ignore', 'inherit', 'ipc']});
    const deadline = setTimeout(() => { child.kill(); process.exit(1); }, 5000);
    child.on('message', value => { assert.equal(value, 42); received = true; });
    child.on('exit', (code, signal) => {
        clearTimeout(deadline);
        assert.equal(code, 0);
        assert.equal(signal, null);
        assert.equal(received, true);
    });
}
"#,
        )?;
        // Independent processes let later cells report after a denial.
        let cells = [
            (
                "unix socket",
                concat!(
                    "set -eu; case \"$TMPDIR\" in /private/tmp/pv-*) ;; *) exit 16;; esac; ",
                    ": > \"$TMPDIR/alias-file\"; test -f \"$TMPDIR/alias-file\"; ",
                    "\"$LANE_FIXTURE\" --bind-socket \"$TMPDIR/alias.sock\""
                ),
            ),
            (
                "credential guards",
                concat!(
                    "set -eu; if /usr/bin/security help >/dev/null 2>&1; then exit 11; fi; ",
                    "if /bin/cat \"$HOME/Library/Keychains/login.keychain-db\" >/dev/null 2>&1; then exit 12; fi; ",
                    "for f in credentials.toml credentials; do ",
                    "if /bin/cat \"$HOME/.cargo/$f\" >/dev/null 2>&1; then exit 13; fi; done"
                ),
            ),
            (
                "git temporary repository",
                concat!(
                    "set -eu; if GIT_CONFIG_GLOBAL=/nonexistent/gitconfig git config --global --list >/dev/null 2>&1; then :; else exit 14; fi; ",
                    "lane_root=$PWD; lane_head=$(git rev-parse HEAD); ",
                    "cp \"$TMPDIR/git-common/index\" \"$TMPDIR/index-before-fixture\"; ",
                    "printf 'unstaged lane content\\n' > \"$lane_root/.fixture-git-probe\"; ",
                    "mkdir \"$TMPDIR/fixture-git-probe\"; cd \"$TMPDIR/fixture-git-probe\"; ",
                    "git init -q; printf 'fixture content\\n' > fixture; git add -A; ",
                    "git -c user.name=Fixture -c user.email=fixture@example.test commit -qm base; ",
                    "test -d .git; test \"$(git log -1 --format=%an)\" = Fixture; ",
                    "cd \"$lane_root\"; test \"$(git rev-parse HEAD)\" = \"$lane_head\"; ",
                    "cmp \"$TMPDIR/index-before-fixture\" \"$TMPDIR/git-common/index\"; ",
                    "unlink \"$lane_root/.fixture-git-probe\""
                ),
            ),
            // This is a native confined-shell probe of the lane's PATH Git
            // wrapper. fixture::run_git deliberately bypasses that wrapper and
            // cannot establish this boundary behaviour.
            (
                "git commit",
                concat!(
                    "set -eu; git commit -q --allow-empty -m probe; ",
                    "test \"$(command -v git)\" = \"$(cd \"$TMPDIR\" && pwd -P)/bin/git\"; ",
                    "git --version; git status --porcelain"
                ),
            ),
            (
                "git stash",
                concat!(
                    "set -eu; d=\"$TMPDIR/stash-probe\"; mkdir \"$d\"; cd \"$d\"; git init -q; ",
                    "git config user.name lane; git config user.email lane@example.test; ",
                    "printf base > file; git add file; git commit -qm base; ",
                    "printf changed > file; git stash -q; test \"$(cat file)\" = base; git stash pop -q; test \"$(cat file)\" = changed"
                ),
            ),
            (
                "git rebase",
                concat!(
                    "set -eu; d=\"$TMPDIR/rebase-probe\"; mkdir \"$d\"; cd \"$d\"; git init -q; ",
                    "git config user.name lane; git config user.email lane@example.test; ",
                    "printf base > base; git add base; git commit -qm base; git branch upstream; ",
                    "printf topic > topic; git add topic; git commit -qm topic; git checkout -q upstream; ",
                    "printf upstream > upstream; git add upstream; git commit -qm upstream; ",
                    "git checkout -q -; git rebase -q upstream; test -f topic; test -f upstream"
                ),
            ),
            (
                "rg",
                concat!(
                    "set -eu; if command -v rg >/dev/null; then ",
                    "printf 'fixture\\n' > \"$TMPDIR/rg-probe\"; rg -q fixture \"$TMPDIR/rg-probe\"; ",
                    "elif [ \"${PROVENANT_LANE_TOOLS_REQUIRED:-0}\" = 1 ]; then ",
                    "echo 'required rg unavailable' >&2; exit 15; else echo 'SKIP: rg unavailable' >&2; fi"
                ),
            ),
            (
                "cargo",
                "set -eu; cargo test -j2 --lib --offline --manifest-path \"$TMPDIR/cargo-probe/Cargo.toml\"",
            ),
            ("nextest", LANE_NEXTEST_PROBE),
            (
                "swift",
                concat!(
                    "set -eu; if command -v swift >/dev/null; then swift --version; ",
                    "elif [ \"${PROVENANT_LANE_TOOLS_REQUIRED:-0}\" = 1 ]; then ",
                    "echo 'required swift unavailable' >&2; exit 15; ",
                    "else echo 'SKIP: swift unavailable' >&2; fi"
                ),
            ),
            ("python3", "set -eu; python3 -c 'assert 2 + 2 == 4'"),
            (
                "here-document",
                "set -eu; cat > \"$TMPDIR/here-doc\" <<'END'\nfixture\nEND\ntest \"$(cat \"$TMPDIR/here-doc\")\" = fixture",
            ),
            (
                "temporary script",
                "set -eu; printf '#!/bin/sh\\nprintf fixture\\n' > \"$TMPDIR/probe-script\"; chmod +x \"$TMPDIR/probe-script\"; test \"$(\"$TMPDIR/probe-script\")\" = fixture",
            ),
            ("node/pnpm", LANE_NODE_PNPM_PROBE),
        ];
        let mut tool_env = launch.environment.clone();
        let tools_required = std::env::var("PROVENANT_LANE_TOOLS_REQUIRED").as_deref() == Ok("1");
        tool_env.push((
            "PROVENANT_LANE_TOOLS_REQUIRED".into(),
            if tools_required { "1" } else { "0" }.into(),
        ));
        tool_env.push(("LANE_FIXTURE".into(), launch.argv[0].clone()));
        let mut failures = Vec::new();
        for (name, cell) in cells.iter().copied() {
            let argv = [OsString::from("/bin/sh"), OsString::from("-c"), cell.into()];
            let command = match compiled.command(
                &argv,
                &provenant_core::AttemptId::from_bytes([0; 16]),
                &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
            ) {
                Ok(command) => command,
                Err(error) => {
                    failures.push(format!("{name}: command preparation failed: {error}"));
                    continue;
                }
            };
            let mut options = command.options(&launch.cwd, Duration::from_secs(30))?;
            options.process_group = true;
            options.cleanup_process_group = true;
            let mut cell_env = tool_env.clone();
            if name == "git commit" {
                for (key, value) in provenant_os::fixture::fixture_git_settings() {
                    cell_env.retain(|(existing, _)| existing != &key);
                    cell_env.push((key, value));
                }
            }
            options.env = Some(&cell_env);
            let output = match executor.execute(options) {
                Ok(output) => output,
                Err(error) => {
                    failures.push(format!("{name}: execution failed: {error}"));
                    continue;
                }
            };
            let stdout = String::from_utf8_lossy(&output.stdout.bytes);
            let stderr = String::from_utf8_lossy(&output.stderr.bytes);
            if code(&output) != 0 {
                failures.push(format!("{name}: {stderr}\n{stdout}"));
            } else if (name == "unix socket" && !stdout.contains("lane-socket: PASS"))
                || (name == "cargo" && !stdout.contains("1 passed"))
            {
                failures.push(format!(
                    "{name}: missing success marker: {stderr}\n{stdout}"
                ));
            } else if name == "node/pnpm" && !stdout.contains("lane-node-pnpm-compat: PASS") {
                if tools_required {
                    failures.push(format!(
                        "{name}: missing success marker: {stderr}\n{stdout}"
                    ));
                } else {
                    provenant_test_support::missing_prerequisite(
                        false,
                        "node/pnpm unavailable in lane",
                    )?;
                }
            }
        }
        assert!(
            failures.is_empty(),
            "native probe failures: {}",
            failures.join("; ")
        );
        // Deny scratch execution in the actual compiled writer profile. The
        // same table cells must still run independently: only the temporary
        // script needs to execute from scratch.
        let path = scratch
            .to_str()
            .ok_or("scratch path is not UTF-8")?
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let deny = format!("(deny process-exec (subpath \"{path}\"))\n");
        let selected = ["python3", "here-document", "temporary script"];
        let mut baseline = Vec::new();
        let mut denied = Vec::new();
        for (name, cell) in cells
            .iter()
            .copied()
            .filter(|(name, _)| selected.contains(name))
        {
            for (restrict, failures) in [(false, &mut baseline), (true, &mut denied)] {
                let argv = [OsString::from("/bin/sh"), OsString::from("-c"), cell.into()];
                let mut restricted = compiled.clone();
                if restrict {
                    let source = std::str::from_utf8(&restricted.profile)?;
                    assert!(source.lines().any(|line| line == "(allow process-exec)"));
                    restricted.profile.extend_from_slice(deny.as_bytes());
                    use sha2::Digest;
                    restricted.profile_digest = sha2::Sha256::digest(&restricted.profile).into();
                }
                let command = restricted.command(
                    &argv,
                    &provenant_core::AttemptId::from_bytes([0; 16]),
                    &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
                )?;
                let mut options = command.options(&launch.cwd, Duration::from_secs(30))?;
                options.process_group = true;
                options.cleanup_process_group = true;
                options.env = Some(&tool_env);
                let output = executor.execute(options)?;
                if code(&output) != 0 {
                    failures.push(name);
                }
            }
        }
        assert!(baseline.is_empty(), "baseline cells failed: {baseline:?}");
        assert_eq!(
            denied,
            ["temporary script"],
            "scratch execution denial affected unexpected cells"
        );
    }
    // Start the configured MCP inside the compiled profile too: an
    // unconfined fake provider cannot detect ungranted startup writes.
    let mcp_argv = compiled.command(
        &launch.argv,
        &provenant_core::AttemptId::from_bytes([0; 16]),
        &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
    )?;
    let mut options = mcp_argv.options(&launch.cwd, Duration::from_secs(10))?;
    options.env = Some(&launch.environment);
    options.stdin = Some(frames);
    let output = executor.execute(options)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let replies = output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let result = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("confined MCP reply")?["result"];
    assert_lane_child_preview(result)?;
    // The child-store submission/scanner has been deleted. Graph child
    // admission is covered through the host relay fixtures.
    host.provider_exited(&parent.encode()?, None)?;
    assert!(!state.join("children").exists());
    let denied = [
        attempt.join("custody.lock"),
        attempt.join("reserve"),
        attempt.join("quarantine/probe"),
        attempt.join("quarantine-result.json"),
        attempt.join("process.json"),
        state.join("runs/other/attempts/other/process.json"),
    ];
    for path in &denied {
        fs::create_dir_all(path.parent().ok_or("custody parent")?)?;
        fs::write(path, b"sealed custody")?;
    }
    let marker = confinement
        .plan
        .permissions()
        .scratch
        .path
        .join("allowed-write");
    let mut argv: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(),
        "printf allowed > \"$1\" || exit 10; shift; for target in \"$@\"; do if printf changed > \"$target\"; then exit 20; fi; done".into(), "probe".into(), marker.as_os_str().to_owned()];
    argv.extend(denied.iter().map(|path| path.as_os_str().to_owned()));
    let argv = compiled.command(
        &argv,
        &provenant_core::AttemptId::from_bytes([0; 16]),
        &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
    )?;
    let mut options = argv.options(&launch.cwd, Duration::from_secs(5))?;
    options.env = Some(&launch.environment);
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let output = executor.execute(options)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert_eq!(fs::read(marker)?, b"allowed");
    for path in denied {
        assert_eq!(fs::read(path)?, b"sealed custody");
    }
    Ok(())
}
