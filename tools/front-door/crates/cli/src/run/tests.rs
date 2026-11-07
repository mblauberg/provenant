use super::*;
use crate::Mode;

fn reexec_test_in_lane(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
    if !crate::fabric::in_lane() {
        return Ok(false);
    }
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsStr;

    let argv = vec![
        std::env::current_exe()?.into_os_string(),
        "--exact".into(),
        name.into(),
    ];
    let removed = [
        OsStr::new(provenant_core::RUN_STATE_DIRECTORY_ENV),
        OsStr::new(run::LANE_DEPTH_ENV),
        OsStr::new(run::PARENT_ATTEMPT_ENV),
    ];
    let env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !removed.contains(&key.as_os_str()))
        .collect();
    let cwd = std::env::current_dir()?;
    let timeout = match name {
        "run::tests::wait_handles_a_stale_default_cursor" => 300,
        "run::tests::dispatch_reuses_only_free_lane_or_unowned_worktree" => 90,
        _ => 30,
    };
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(timeout));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 65_536 };
    options.stderr = StreamPolicy::Capture { limit: 65_536 };
    let out = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
    Ok(true)
}

/// A project with a linked worktree `writer` and a `nested` directory in it.
/// The fixture's Git runs without the lane's `GIT_*` variables.
struct LinkedFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    tree: PathBuf,
}

fn linked_fixture() -> Result<LinkedFixture, Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    let tree = root.join("writer");
    std::fs::create_dir_all(&project)?;
    let executor = SystemExecutor::new(1)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_"))
        .collect();
    env.extend(provenant_os::fixture::fixture_git_settings());
    for args in [
        vec!["init", "-q", "-b", "fixture-base"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec![
            "worktree",
            "add",
            "-qb",
            "writer",
            tree.to_str().ok_or("path")?,
        ],
    ] {
        let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.into_iter().map(std::ffi::OsString::from))
            .collect();
        let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(10));
        options.env = Some(&env);
        let output = executor.execute(options)?;
        assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
    }
    std::fs::create_dir(tree.join("nested"))?;
    Ok(LinkedFixture {
        _temp: temp,
        root,
        project,
        tree,
    })
}

/// Runs `check` with the edge that `run` would see from `cwd`.
fn with_edge<T>(
    root: &Path,
    cwd: &Path,
    check: impl FnOnce(&Edge<'_>) -> T,
) -> Result<T, Box<dyn std::error::Error>> {
    use provenant_core::RootInputs;
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home)?;
    let mut inputs = RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    Ok(check(&Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd,
    }))
}

use provenant_run::Providers;
use provenant_supervisor::custody::CaptureTotals;
use provenant_supervisor::custody::PublicationState;
use provenant_supervisor::custody::Scope;
use provenant_supervisor::custody::SettlementEvidence;

pub(super) fn trace_run(
    n: u8,
    parent_attempt: AttemptId,
    created_at: WallTime,
) -> Result<run::PreparedRun, Box<dyn std::error::Error>> {
    let task_id = TaskId::from_bytes([n; 16]);
    Ok(run::PreparedRun {
        run_id: RunId::from_bytes([n + 1; 16]),
        operation_id: OperationId::parse(&format!("trace-{n}"))?,
        created_at,
        request: run::CanonicalRequest {
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![run::TaskRequest {
                cloud: None,
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                source_head: None,
                base_ref: None,
                task_id,
                prompt_digest: CanonicalV1::file_digest(b"p"),
                route: run::RequestedRoute::Model {
                    extra: Default::default(),
                    provider: "fixture".into(),
                    model: "fixture/model".into(),
                    effort: None,
                },
                account: None,
                profile: Default::default(),
                authority_digest: provenant_core::Digest::from_bytes([5; 32]),
                provider_timeout_seconds: 3600,
                writer: None,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: Some(1),
            parent_attempt: Some(parent_attempt),
        },
        attempts: vec![run::NewAttempt {
            task_id,
            attempt_id: AttemptId::from_bytes([n + 2; 16]),
            prompt: "p".into(),
        }],
    })
}

fn trace_store(
    temp: &tempfile::TempDir,
    call: &run::Request<'_>,
) -> Result<run::RunStore, Box<dyn std::error::Error>> {
    let root = std::fs::canonicalize(temp.path())?;
    std::fs::create_dir_all(root.join("home"))?;
    std::fs::create_dir_all(root.join("checkout"))?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: provenant_core::ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    Ok(run::RunStore::open(&context, call)?)
}

fn render_lanes_without_cooldowns(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    next: Option<u64>,
    root: &Path,
) -> (Value, String) {
    render_lanes(lanes, waits, next, root, None)
}

/// A lane for the lane-view tests: queued until a test sets its state.
fn lane_fixture(
    attempt: u8,
    task: u8,
    state: run::AttemptState,
) -> Result<run::AttemptRecord, Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([attempt; 16]),
        "run_id": RunId::from_bytes([9; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([task; 16]),
        "operation_id": format!("lane-{attempt}"),
        "attempt_number": attempt,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state":"queued"},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        }
    }))?;
    lane.state = state;
    Ok(lane)
}

fn clean_exit() -> ExitObservation {
    ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: RunId::from_bytes([1; 16]),
            attempt: AttemptId::from_bytes([2; 16]),
            generation: 1,
        },
        termination: Some(provenant_os::exec::Termination::Exit(0)),
        never_spawned: false,
        launch_refusal: None,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        cancel_delivered_before_exit: None,
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: false,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: SettlementEvidence::NeverSpawned,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    }
}

/// Population of OpenCode's own session record: the synthetic v2
/// `session_message` database the dispatch tests pair with the fixtures.
fn seed_opencode_session(
    data: &Path,
    session: &str,
    provider: &str,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data)?;
    let db = rusqlite::Connection::open(data.join("opencode.db"))?;
    db.execute_batch("CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)")?;
    db.execute(
        "INSERT INTO session_message VALUES (?1, 'assistant', ?2)",
        (
            session,
            serde_json::to_string(&json!({"model": {"providerID": provider, "id": model}}))?,
        ),
    )?;
    Ok(())
}

const OPENCODE_DECODE_XDG_MARKER: &str = "PROVENANT_TEST_OPENCODE_DECODE_XDG";

fn opencode_decode_route() -> run::RouteRecord {
    run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode/glm-5.3-flash-free".into()),
        ..Default::default()
    }
}

fn opencode_decode_holder_context(
    temp: &Path,
) -> Result<run::HolderContext, Box<dyn std::error::Error>> {
    Ok(run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.join("home"),
            checkout: temp.join("checkout"),
            state_root: temp.join("state"),
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        run_id: RunId::from_bytes([3; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    })
}

/// Re-run `test` in a confined child process whose environment omits
/// `XDG_DATA_HOME` (when `xdg` is `None`) or pins it to `xdg`, so the
/// decode sees a controlled value without mutating this process's env.
fn run_opencode_decode_child(
    test: &str,
    xdg: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsString;
    let argv = vec![
        std::env::current_exe()?.into_os_string(),
        "--exact".into(),
        format!("run::tests::{test}").into(),
        "--nocapture".into(),
    ];
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            key != OPENCODE_DECODE_XDG_MARKER
                && key != "XDG_DATA_HOME"
                && key != provenant_core::RUN_STATE_DIRECTORY_ENV
        })
        .collect();
    env.push((
        OsString::from(OPENCODE_DECODE_XDG_MARKER),
        OsString::from("1"),
    ));
    if let Some(xdg) = xdg {
        env.push(("XDG_DATA_HOME".into(), xdg.as_os_str().into()));
    }
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 65_536 };
    options.stderr = StreamPolicy::Capture { limit: 65_536 };
    let out = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
    assert!(!out.timed_out, "{out:?}");
    Ok(())
}

#[test]
fn admission_window_deadline_is_preserved_on_restart() -> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane("run::tests::admission_window_deadline_is_preserved_on_restart")? {
        return Ok(());
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.tree,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            crate::fabric::private_dir(edge.roots.state.as_path(), edge.roots.state.as_path())?;
            provenant_config::bind_project(
                edge.roots,
                provenant_config::ProjectLocation::new(fixture.project.join(".git"))?,
            )?;
            provenant_config::bind_host(edge.roots)?;

            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;

            let op: OperationId = "test-restart-op".parse().unwrap();
            let t0 = WallTime::from_micros(10_000_000).unwrap();
            let initial_window = run::AdmissionWindow {
                deadline: t0,
                wake_at: None,
            };
            let saved = session
                .store
                .save_admission_window(&op, initial_window, &call)?;
            assert_eq!(saved.deadline, t0);
            assert_eq!(saved.wake_at, None);

            // Later restart attempts to set a later deadline (t1) and a wake_at
            let t1 = WallTime::from_micros(20_000_000).unwrap();
            let wake = WallTime::from_micros(12_000_000).unwrap();
            let restarted_window = run::AdmissionWindow {
                deadline: t1,
                wake_at: Some(wake),
            };
            let reloaded = session
                .store
                .save_admission_window(&op, restarted_window, &call)?;
            // The deadline MUST NOT be renewed: it must remain t0!
            assert_eq!(reloaded.deadline, t0);
            // wake_at was updated
            assert_eq!(reloaded.wake_at, Some(wake));
            Ok(())
        },
    )??;
    Ok(())
}
#[test]
fn invalid_sealed_root_never_writes_to_registry_root() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsString;
    const MARKER: &str = "PROVENANT_TEST_INVALID_CAPACITY_ROOT";
    if std::env::var_os(MARKER).is_none() {
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::tests::invalid_sealed_root_never_writes_to_registry_root".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| key != MARKER && key != provenant_core::RUN_STATE_DIRECTORY_ENV)
            .collect();
        env.push((OsString::from(MARKER), OsString::from("1")));
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65_536 };
        options.stderr = StreamPolicy::Capture { limit: 65_536 };
        let out = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        assert!(!out.timed_out, "{out:?}");
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("registry"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let registry = Registry {
        roots: Some(&roots),
        ..Default::default()
    };
    let protected = temp.path().join("protected");
    let context = run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.path().join("home"),
            checkout: protected.clone(),
            state_root: protected,
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        run_id: RunId::from_bytes([3; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(Termination::Exit(1));
    let decoded = registry.decode_captures(
        &route,
        stream,
        &[],
        &exit,
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert!(
        decoded
            .reason
            .unwrap_or_default()
            .contains("capacity cooldown configuration unavailable")
    );
    assert!(!roots.state.as_path().join("quota").exists());
    Ok(())
}
#[test]
fn lane_gitdir_resolves_the_registered_project_without_querying_primary()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane(
        "run::tests::lane_gitdir_resolves_the_registered_project_without_querying_primary",
    )? {
        return Ok(());
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let context = crate::fabric::Context::open(edge.roots.clone(), edge.home, edge.cwd)?;
            let gitdir = String::from_utf8(provenant_os::fixture::run_git(
                &fixture.tree,
                &["rev-parse", "--absolute-git-dir"],
            )?)?;
            let gitdir = PathBuf::from(gitdir.trim());
            assert_eq!(
                super::launch::project_id_for_lane_gitdir(edge.roots, &gitdir),
                Some(context.identity.project)
            );
            assert_eq!(
                super::launch::project_id_for_lane_gitdir(
                    edge.roots,
                    &fixture.project.join(".git")
                ),
                Some(context.identity.project)
            );
            assert_eq!(
                super::launch::project_id_for_lane_gitdir(
                    edge.roots,
                    &fixture.root.join("unrelated")
                ),
                None
            );
            Ok(())
        },
    )??;
    Ok(())
}
#[test]
fn nested_capacity_settlement_does_not_write_shared_cooldown()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsString;
    const MARKER: &str = "PROVENANT_TEST_NESTED_CAPACITY_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::tests::nested_capacity_settlement_does_not_write_shared_cooldown".into(),
            "--nocapture".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| key != MARKER && key != provenant_core::RUN_STATE_DIRECTORY_ENV)
            .collect();
        env.push((OsString::from(MARKER), OsString::from("1")));
        env.push((
            OsString::from(provenant_core::RUN_STATE_DIRECTORY_ENV),
            OsString::from("nested"),
        ));
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65_536 };
        options.stderr = StreamPolicy::Capture { limit: 65_536 };
        let out = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        assert!(!out.timed_out, "{out:?}");
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    let registry = Registry {
        roots: Some(&roots),
        ..Default::default()
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(Termination::Exit(1));
    let decoded = registry.decode(&route, stream, &exit, Path::new("/nonexistent"))?;
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(
        decoded.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    assert!(!roots.state.as_path().join("quota").exists());
    Ok(())
}
#[test]
fn operation_replays_after_its_context_ref_moves_or_is_deleted()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{ActorId, RootInputs};
    use provenant_os::exec::ExecOptions;
    use provenant_os::exec::Executor;
    use provenant_os::exec::StreamPolicy;
    use provenant_os::exec::SystemExecutor;
    use provenant_os::exec::Termination;

    use std::ffi::{OsStr, OsString};
    let executor = SystemExecutor::new(1)?;
    // The regression exercises a top-level submission, even when the
    // contributor running it is itself inside a nested lane.
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some()
        || std::env::var_os(run::LANE_DEPTH_ENV).is_some()
        || std::env::var_os(run::PARENT_ATTEMPT_ENV).is_some()
    {
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::tests::operation_replays_after_its_context_ref_moves_or_is_deleted".into(),
            "--nocapture".into(),
        ];
        let removed = [
            OsStr::new(provenant_core::RUN_STATE_DIRECTORY_ENV),
            OsStr::new(run::LANE_DEPTH_ENV),
            OsStr::new(run::PARENT_ATTEMPT_ENV),
        ];
        let env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| !removed.contains(&key.as_os_str()))
            .collect();
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let out = executor.execute(options)?;
        assert!(
            out.termination == Termination::Exit(0) && !out.timed_out,
            "{out:?}"
        );
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let cwd = root.join("checkout");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&cwd)?;
    let mut inputs = RootInputs::new(home.clone(), cwd.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let command = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.iter().map(OsString::from))
            .collect();
        let env = provenant_os::fixture::fixture_git_settings();
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(10));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let out = executor.execute(options)?;
        assert!(
            out.termination == Termination::Exit(0) && !out.timed_out,
            "{out:?}"
        );
        Ok(String::from_utf8(out.stdout.bytes)?.trim().to_owned())
    };
    command(&["init", "-q"])?;
    command(&["commit", "--allow-empty", "-qm", "initial"])?;
    command(&["branch", "context-base"])?;
    let original_base = command(&["rev-parse", "context-base"])?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let context = run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session {
        store: run::RunStore::open(&context, &call)?,
        context,
    };
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &cwd,
        cwd: &cwd,
    };
    let chair = Some(run::ChairBinding {
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([4; 16]),
        actor: ActorId::from_bytes([5; 16]),
    });
    let submission = |base: &str| Submission {
        chair: chair.clone(),
        operation_id: OperationId::parse("context-ref-replay").unwrap(),
        tasks: vec![Submitted {
            attempt: Default::default(),
            require_skill_location: true,
            settlement_retry_limit: None,
            context: None,
            title: None,
            task_id: TaskId::from_bytes([3; 16]),
            prompt: "review".into(),
            base: Some(base.into()),
            pinned_base: None,
            head: None,
            source_head: None,
            route: run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "fixture".into(),
                model: "fixture/model".into(),
                effort: None,
            },
            provider_timeout: 10,
            delivery_id: None,
            deliver: None,
            worktree: None,
            access: Default::default(),
        }],
        concurrency: Some(1),
        knobs: vec![],
    };
    // The fake adapter refuses preparation after admission. Keep the durable
    // operation as the replay subject even though the initial launch fails.
    let initial = submit(&session, &edge, submission("context-base"), &call)
        .err()
        .ok_or("unknown fixture adapter must refuse its initial launch")?;
    assert_eq!(initial.status, Status::Refused);
    assert_eq!(initial.kind, ErrorKind::Refused);
    let operation = session
        .store
        .operation(&OperationId::parse("context-ref-replay")?, &call)?
        .ok_or("refused launch lost its admitted operation")?;
    assert_eq!(operation.state, run::OperationState::Terminal);
    let first = session.store.run(operation.run_id, &call)?;
    assert!(
        session.store.request(first.run_id, &call)?.tasks[0]
            .profile
            .require_skill_location
    );
    with_chair_index(&roots, &call, |index, index_call| {
        let binding = index
            .read(session.context.project_id, first.run_id, index_call)?
            .unwrap();
        assert_eq!(binding.chair, chair);
        assert_eq!(
            binding.request_digest,
            session
                .store
                .request(first.run_id, &call)
                .unwrap()
                .digest()
                .unwrap()
        );
        Ok(())
    })
    .map_err(|f| f.message)?;
    std::fs::rename(
        roots.state.as_path().join("fabric/host.sqlite3"),
        roots.state.as_path().join("fabric/interrupted.sqlite3"),
    )?;
    let mut forged = session.store.request(first.run_id, &call)?;
    forged.concurrency = 2;
    with_chair_index(&roots, &call, |index, c| {
        index.submit(session.context.project_id, first.run_id, &forged, c)
    })
    .map_err(|f| f.message)?;
    assert_eq!(
        launch(&session, first.run_id, &edge, &call)
            .unwrap_err()
            .status,
        Status::Refused
    );
    std::fs::rename(
        roots.state.as_path().join("fabric/host.sqlite3"),
        roots.state.as_path().join("fabric/forged.sqlite3"),
    )?;
    // Reusing an operation from another chair cannot rebind even a missing
    // host index row. Repair must use the immutable original request.
    let mut replay = submission("context-base");
    replay.chair = Some(run::ChairBinding {
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([6; 16]),
        actor: ActorId::from_bytes([7; 16]),
    });
    let (replayed, _) = submit(&session, &edge, replay, &call).map_err(|f| f.message)?;
    assert_eq!(replayed.run_id, first.run_id);
    let stopped = CancelToken::new();
    stopped.cancel();
    let cancelled = run::Request {
        cancel: &stopped,
        ..call
    };
    let failure = with_chair_index(&roots, &cancelled, |_, _| Ok(())).unwrap_err();
    assert_eq!(failure.status, Status::Error);
    with_chair_index(&roots, &call, |index, call| {
        assert_eq!(
            index
                .read(session.context.project_id, first.run_id, call)?
                .unwrap()
                .chair,
            chair
        );
        Ok(())
    })
    .map_err(|f| f.message)?;
    command(&["commit", "--allow-empty", "-qm", "moved"])?;
    command(&["branch", "-f", "context-base", "HEAD"])?;
    for deleted in [false, true] {
        if deleted {
            command(&["branch", "-D", "context-base"])?;
        }
        let (replayed, _) =
            submit(&session, &edge, submission("context-base"), &call).map_err(|f| f.message)?;
        assert_eq!(replayed.run_id, first.run_id);
        assert_eq!(
            session.store.request(first.run_id, &call)?.tasks[0]
                .base
                .as_deref(),
            Some(original_base.as_str())
        );
    }
    assert!(
        matches!(submit(&session, &edge, submission("HEAD"), &call), Err(fail) if fail.kind == ErrorKind::OperationConflict)
    );
    Ok(())
}
#[test]
fn reconcile_lists_reservation_issues_from_a_selected_project()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane(
        "run::tests::reconcile_lists_reservation_issues_from_a_selected_project",
    )? {
        return Ok(());
    }
    let fixture = linked_fixture()?;
    let other = fixture.root.join("other-project");
    std::fs::create_dir(&other)?;
    provenant_os::fixture::run_git(&other, &["init", "-q", "-b", "fixture-base"])?;
    provenant_os::fixture::run_git(&other, &["commit", "--allow-empty", "-qm", "base"])?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let current = open(edge, &call).map_err(|fail| fail.message)?;
            let other_edge = Edge {
                cwd: &other,
                ..*edge
            };
            let selected = open(&other_edge, &call).map_err(|fail| fail.message)?;
            assert_ne!(current.context.project_id, selected.context.project_id);
            let mut input = trace_run(61, AttemptId::from_bytes([60; 16]), clock.wall_now()?)?;
            input.request.parent_attempt = None;
            input.request.lane_depth = None;
            selected.store.create(&input, &call)?;
            let id = input.attempts[0].attempt_id;
            selected.store.cancel(id, "fixture settled", &call)?;
            // This read-only task never launched a provider. Seed completed
            // no-effect receipts so only the unrelated orphan needs recovery.
            let generation = selected.store.record(id, &call)?.generation;
            for effect in [
                run::SettlementEffect::GitImport,
                run::SettlementEffect::ManifestConflict,
                run::SettlementEffect::CleanupCustodyRelease,
                run::SettlementEffect::Delivery,
            ] {
                selected.store.record_effect(
                    id,
                    generation,
                    effect,
                    run::EffectDisposition::Done,
                    &call,
                )?;
            }
            selected
                .store
                .drain_reactions(input.run_id, &clock, &cancel, &mut |reaction, _| {
                    reaction.graph_done = true;
                    reaction.notice_done = true;
                    reaction.log_done = true;
                    Ok(())
                })?;
            let baseline = selected
                .store
                .reconcile(input.run_id, &Registry::default(), &call);
            assert!(baseline.is_ok(), "{baseline:?}");
            let slot = selected
                .context
                .state_root
                .as_path()
                .join("worktrees")
                .join(selected.context.project_id.to_string())
                .join("orphan");
            std::fs::create_dir_all(&slot)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o700))?;
            let path = slot.join("reservation.json");
            std::fs::write(&path, b"malformed other-project reservation")?;
            let target = || Target {
                selection: None,
                run: Some(input.run_id),
                task: None,
                attempt: None,
            };
            let reply = reconcile(
                Reconcile {
                    id: vec![target(), target()],
                    abandon: None,
                    reason: None,
                },
                edge,
            );
            assert_eq!(reply.result.status, Status::Failed, "{}", reply.human);
            assert_eq!(
                reply.result.error.as_ref().ok_or("missing error")?.code,
                "RECONCILE_INCOMPLETE"
            );
            let rows = reply.result.data["runs"].as_array().ok_or("runs")?;
            assert!(!rows.is_empty());
            assert!(
                rows.iter()
                    .all(|row| row["run_id"] == json!(input.run_id) && row.get("error").is_none()),
                "{}",
                reply.result.data
            );
            let issues = reply.result.data["writer_reservation_errors"]
                .as_array()
                .ok_or("issues")?;
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0]["reservation"], json!(path));
            assert_eq!(
                std::fs::read(&path)?,
                b"malformed other-project reservation"
            );
            assert!(matches!(
                selected.store.record(id, &call)?.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::Cancelled,
                    ..
                }
            ));
            Ok(())
        },
    )??;
    Ok(())
}
#[test]
fn reconcile_reports_orphan_reservation_issues() -> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane("run::tests::reconcile_reports_orphan_reservation_issues")? {
        return Ok(());
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let slot = session
                .context
                .state_root
                .as_path()
                .join("worktrees")
                .join(session.context.project_id.to_string())
                .join("orphan");
            std::fs::create_dir_all(&slot)?;
            let path = slot.join("reservation.json");
            std::fs::write(&path, b"malformed retained reservation")?;
            let reply = reconcile(
                Reconcile {
                    id: Vec::new(),
                    abandon: None,
                    reason: None,
                },
                edge,
            );
            assert_eq!(reply.result.status, Status::Failed, "{}", reply.human);
            assert_eq!(
                reply.result.error.as_ref().ok_or("missing error")?.code,
                "RECONCILE_INCOMPLETE"
            );
            assert_eq!(reply.result.data["runs"], json!([]));
            assert_eq!(
                reply.result.data["writer_reservation_errors"][0]["reservation"],
                json!(path)
            );
            assert_eq!(std::fs::read(&path)?, b"malformed retained reservation");
            // A valid orphan whose destination cannot be published reports the
            // storage failure too, rather than only a generic retained fence.
            use provenant_supervisor::custody::{self, Scope};
            use std::os::unix::fs::PermissionsExt;
            std::fs::remove_file(&path)?;
            let directory = provenant_os::fs::Directory::open_anchor(&slot)?;
            let scope = Scope {
                run: RunId::from_bytes([6; 16]),
                attempt: AttemptId::from_bytes([7; 16]),
                generation: 1,
            };
            drop(custody::acquire_reader(
                directory.try_clone()?,
                scope,
                provenant_core::WaitBudget::nonblocking(clock.mono_now()),
                &cancel,
                &clock,
            )?);
            let child_path = slot.join(scope.attempt.to_string());
            let child = child_path.join("reservation.json");
            let mut retained: Value = serde_json::from_slice(&std::fs::read(&child)?)?;
            let anchor = directory.metadata()?;
            retained["reservation_device"] = anchor.device.into();
            retained["reservation_inode"] = anchor.inode.into();
            std::fs::write(&path, serde_json::to_vec(&retained)?)?;
            std::fs::remove_file(&child)?;
            std::fs::set_permissions(&child_path, std::fs::Permissions::from_mode(0o500))?;
            let reply = reconcile(
                Reconcile {
                    id: Vec::new(),
                    abandon: None,
                    reason: None,
                },
                edge,
            );
            std::fs::set_permissions(&child_path, std::fs::Permissions::from_mode(0o700))?;
            assert_eq!(reply.result.status, Status::Failed, "{}", reply.human);
            assert!(
                reply.result.data["writer_reservation_errors"]
                    .as_array()
                    .ok_or("issues")?
                    .iter()
                    .any(|issue| issue["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("cut over writer reservations")))
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&std::fs::read(&path)?)?,
                retained
            );
            assert!(!child.exists());
            Ok(())
        },
    )??;
    Ok(())
}
#[cfg(feature = "fixture")]
#[test]
fn reconcile_reports_publish_error_and_explicitly_abandons_a_writer_fence()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane(
        "run::tests::reconcile_reports_publish_error_and_explicitly_abandons_a_writer_fence",
    )? {
        return Ok(());
    }
    use provenant_os::fs::{Directory, LockMode, PublishMode};
    use provenant_supervisor::custody::{self, Scope};
    use std::ffi::OsStr;
    for (import_done, settle_failed) in [(false, true), (false, false), (true, false)] {
        let reason = if import_done {
            "r".repeat(4096)
        } else {
            "worktree rebuilt by chair".into()
        };
        let phase = if import_done { "release" } else { "publish" };
        let blocker = if import_done {
            "release evidence missing"
        } else {
            "lane Git import failed"
        };
        let fixture = linked_fixture()?;
        with_edge(
            &fixture.root,
            &fixture.project,
            |edge| -> Result<(), Box<dyn std::error::Error>> {
                let clock = SystemClock::new();
                let cancel = CancelToken::new();
                let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
                let session = open(edge, &call).map_err(|fail| fail.message)?;
                let mut input = trace_run(30, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
                input.request.parent_attempt = None;
                input.request.lane_depth = None;
                input.request.tasks[0].writer = Some(run::WriterRequest {
                    extra: Default::default(),
                    project: fixture.project.clone(),
                    worktree: fixture.tree.clone(),
                });
                session.store.create(&input, &call)?;
                let id = input.attempts[0].attempt_id;
                let record = session.store.mark_starting_for_fixture(
                    id,
                    &run::RouteRecord::default(),
                    &call,
                )?;
                let scope = Scope {
                    run: input.run_id,
                    attempt: id,
                    generation: record.generation,
                };
                let attempt_path = edge
                    .roots
                    .state
                    .as_path()
                    .join(format!("runs/{}/attempts/{id}", input.run_id));
                let attempt = Directory::open_anchor(&attempt_path)?;
                let slot_path = edge.roots.state.as_path().join(format!(
                    "worktrees/{}/abandon-fixture",
                    session.context.project_id
                ));
                std::fs::create_dir_all(&slot_path)?;
                let lease = provenant_supervisor::fixture::acquire_writer(
                    Directory::open_anchor(&slot_path)?,
                    scope,
                    call.lock_budget(),
                    &cancel,
                    &clock,
                )?;
                let recovery = provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?
                    .into_recovery()?;
                let fence = session
                    .store
                    .writer_fences()?
                    .into_iter()
                    .find(|f| f.scope == scope)
                    .ok_or("fence")?;
                let evidence = Directory::open_anchor(
                    fence.reservation.parent().ok_or("reservation parent")?,
                )?;
                evidence
                    .atomic_write(
                        OsStr::new("import-retained.json"),
                        ".test-",
                        &serde_json::to_vec(&json!({"schema_version":1,"scope":scope}))?,
                        PublishMode::NoReplace,
                    )?
                    .require_durable()?;
                let exit = ExitObservation {
                    scope,
                    terminal_observed: true,
                    settlement_evidence: custody::SettlementEvidence::DarwinCensusAndCustodyLease,
                    ..clean_exit()
                };
                let mut exit_json = serde_json::to_value(&exit)?;
                exit_json["schema_version"] = 1.into();
                attempt
                    .atomic_write(
                        OsStr::new("process-exit.json"),
                        ".test-",
                        &serde_json::to_vec(&exit_json)?,
                        PublishMode::NoReplace,
                    )?
                    .require_durable()?;
                session.store.publish_terminal(
                    id,
                    session.store.record(id, &call)?.revision,
                    &run::TerminalPublication {
                        terminal_cause: None,
                        terminal_note: None,
                        outcome: run::Outcome::Unsettled,
                        finished_at: clock.wall_now()?,
                        started_at: None,
                        provider_ms: None,
                        reason: Some("publish refused".into()),
                        result: "retained provider evidence",
                        complete: true,
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
                let import = if import_done {
                    run::EffectDisposition::Done
                } else {
                    run::EffectDisposition::Failed {
                        reason: blocker.into(),
                        retryable: true,
                    }
                };
                session.store.record_effect(
                    id,
                    scope.generation,
                    run::SettlementEffect::GitImport,
                    import.clone(),
                    &call,
                )?;
                attempt
                    .atomic_write(
                        OsStr::new("settlement-progress.json"),
                        ".test-",
                        &serde_json::to_vec(&json!({
                            "schema_version":1, "scope":scope, "phase":phase, "attempts":1,
                            "head": if settle_failed { Some("a".repeat(40)) } else { None }, "base":null, "owner":null, "blocking_reason":blocker
                        }))?,
                        PublishMode::Replace,
                    )?
                    .require_durable()?;
                std::fs::create_dir_all(attempt_path.join("tmp/candidate-0/git-common"))?;
                std::fs::write(
                    attempt_path.join("tmp/candidate-0/git-common/evidence"),
                    "private objects",
                )?;
                let parse = |args: &[&str]| -> Result<Reconcile, Box<dyn std::error::Error>> {
                    let crate::ParseOutcome::Invocation(inv) =
                        crate::parse(args.iter().map(std::ffi::OsString::from))
                    else {
                        return Err("parse reconcile".into());
                    };
                    let crate::Command::Reconcile(command) = inv.command else {
                        return Err("reconcile command".into());
                    };
                    Ok(command)
                };
                if settle_failed {
                    attempt.atomic_write(OsStr::new("launch.json"), ".test-",
                        &serde_json::to_vec(&json!({"schema_version":1,"scope":scope,"requester":session.context.actor_id}))?,
                        PublishMode::NoReplace)?.require_durable()?;
                    std::fs::create_dir_all(attempt_path.join("tmp/candidate-0/git-objects"))?;
                    std::fs::write(
                        attempt_path.join("tmp/candidate-0/git-objects/evidence"),
                        b"retained objects",
                    )?;
                    attempt
                        .atomic_write(
                            OsStr::new("writer-objects.json"),
                            ".test-",
                            &serde_json::to_vec(
                                &json!({"schema_version":1,"candidate":"candidate-0"}),
                            )?,
                            PublishMode::NoReplace,
                        )?
                        .require_durable()?;
                    drop(recovery);
                    let found = session.store.reconcile_for_fixture(
                        input.run_id,
                        &Registry::default(),
                        &call,
                    )?;
                    let record = session.store.record(id, &call)?;
                    assert!(
                        matches!(record.settlement_receipts.unwrap().git_import,
                        run::EffectDisposition::Failed { retryable: false, ref reason }
                        if reason == blocker),
                        "{found:?}"
                    );
                    for command in ["status", "output"] {
                        let crate::ParseOutcome::Invocation(inv) = crate::parse(
                            [command.to_owned(), id.to_string(), "--full".into()]
                                .into_iter()
                                .map(std::ffi::OsString::from),
                        ) else {
                            return Err("parse lane read".into());
                        };
                        let reply = match inv.command {
                            crate::Command::Status(command) => status(command, true, edge),
                            crate::Command::Output(command) => output(command, edge),
                            _ => return Err("lane read command".into()),
                        };
                        assert_eq!(reply.result.status, Status::Ok, "{}", reply.human);
                        let row = if command == "status" {
                            &reply.result.data["rows"][0]
                        } else {
                            &reply.result.data
                        };
                        assert_eq!(row["state"], "failed");
                        assert_eq!(row["git_import_failure"]["reason"], blocker);
                        assert_eq!(row["git_import_failure"]["head"], "a".repeat(40));
                        assert_eq!(
                            row["git_import_failure"]["retained_objects"],
                            json!(provenant_records::StructuredPathWire::from_path(
                                &attempt_path.join("tmp/candidate-0/git-objects")
                            )?)
                        );
                        let summary = reply
                            .summary
                            .as_ref()
                            .map(|summary| summary.lines.join("\n"))
                            .unwrap_or_default();
                        let mut rendered = Vec::new();
                        crate::render(
                            &reply.result,
                            inv.globals.render_mode(
                                &reply.human,
                                reply.read_only,
                                reply.summary.as_ref(),
                            ),
                            &mut rendered,
                        )?;
                        let rendered = String::from_utf8(rendered)?;
                        if command == "output" {
                            assert_eq!(
                                reply.result.data["bytes"]["data"],
                                "retained provider evidence"
                            );
                        }
                        for text in [&reply.human, &summary, &rendered] {
                            assert!(
                                text.contains("commits may not have reached its branch"),
                                "{command}: {text}"
                            );
                            assert!(text.contains(&"a".repeat(40)), "{command}: {text}");
                            assert!(text.contains("git-objects"), "{command}: {text}");
                        }
                    }
                    assert_eq!(
                        std::fs::read(attempt_path.join("tmp/candidate-0/git-objects/evidence"))?,
                        b"retained objects"
                    );
                    assert_eq!(
                        std::fs::read(attempt_path.join("tmp/candidate-0/git-common/evidence"))?,
                        b"private objects"
                    );
                    session.store.drain_reactions(
                        input.run_id,
                        &clock,
                        &cancel,
                        &mut |receipt, _| {
                            receipt.graph_done = true;
                            receipt.notice_done = true;
                            receipt.log_done = true;
                            Ok(())
                        },
                    )?;
                    let reported = reconcile(
                        parse(&["reconcile", "--id", &input.run_id.to_string(), "--json"])?,
                        edge,
                    );
                    assert_eq!(reported.result.status, Status::Ok, "{}", reported.human);
                    assert!(
                        reported.result.data["runs"][0]["attempts"]
                            .as_array()
                            .is_none_or(Vec::is_empty)
                    );
                    return Ok(());
                }
                let driver =
                    attempt.try_lock(OsStr::new("settlement-driver.lock"), LockMode::Exclusive)?;
                let run_id = input.run_id.to_string();
                let reported = reconcile(parse(&["reconcile", "--id", &run_id, "--json"])?, edge);
                let json = serde_json::to_value(&reported.result)?;
                if !import_done {
                    assert_eq!(
                        json["data"]["runs"][0]["attempts"][0]["found"]["phase"],
                        "publish"
                    );
                    assert_eq!(
                        json["data"]["runs"][0]["attempts"][0]["found"]["blocking_reason"],
                        "lane Git import failed"
                    );
                    assert!(reported.human.contains("lane Git import failed"));
                }
                let selection = format!("{run_id}/{id}");
                assert!(parse(&["reconcile", "--abandon", &selection]).is_err());
                assert!(parse(&["reconcile", "--reason", "missing attempt"]).is_err());
                assert!(
                    parse(&[
                        "reconcile",
                        "--id",
                        &run_id,
                        "--abandon",
                        &selection,
                        "--reason",
                        "ambiguous action"
                    ])
                    .is_err()
                );
                let args = [
                    "reconcile",
                    "--abandon",
                    &selection,
                    "--reason",
                    reason.as_str(),
                    "--json",
                ];
                assert_ne!(
                    reconcile(parse(&args)?, edge).result.status,
                    Status::Ok,
                    "live driver excludes abandonment"
                );
                drop(driver);
                assert_ne!(
                    reconcile(parse(&args)?, edge).result.status,
                    Status::Ok,
                    "held writer lock excludes abandonment"
                );
                drop(recovery);
                let abandoned = reconcile(parse(&args)?, edge);
                assert_eq!(
                    abandoned.result.status,
                    Status::Ok,
                    "{:?}",
                    abandoned.result.error
                );
                assert_eq!(abandoned.result.data["found"]["state"], "abandoned");
                assert_eq!(abandoned.result.data["found"]["reason"], reason);
                assert!(session.store.writer_fences()?.is_empty());
                session
                    .store
                    .reconcile_for_fixture(input.run_id, &Registry::default(), &call)?;
                assert_eq!(
                    session
                        .store
                        .record(id, &call)?
                        .settlement_receipts
                        .unwrap()
                        .git_import,
                    import
                );
                let journal: Value = serde_json::from_slice(&std::fs::read(
                    attempt_path.join("settlement-progress.json"),
                )?)?;
                assert_eq!(
                    journal["abandonment"]["actor"],
                    session.context.actor_id.to_string()
                );
                assert_eq!(journal["abandonment"]["phase"], phase);
                assert_eq!(journal["blocking_reason"], blocker);
                assert_eq!(
                    reconcile(parse(&args)?, edge).result.status,
                    Status::Ok,
                    "same reason is idempotent"
                );
                reconcile(parse(&["reconcile", "--id", &run_id, "--json"])?, edge);
                assert!(
                    attempt_path
                        .join("tmp/candidate-0/git-common/evidence")
                        .is_file()
                );
                let different = [
                    "reconcile",
                    "--abandon",
                    &selection,
                    "--reason",
                    "different explanation",
                    "--json",
                ];
                assert_ne!(
                    reconcile(parse(&different)?, edge).result.status,
                    Status::Ok,
                    "recorded reason cannot be replaced"
                );
                let state = Directory::open_anchor(edge.roots.state.as_path())?;
                let later =
                    WallTime::from_micros(clock.wall_now()?.as_micros() + 90 * 86_400_000_000)?;
                let collected = session
                    .store
                    .gc_provider_state(&state, 7, later, false, &call)?;
                assert!(collected.attempt_tmp.is_empty());
                assert!(
                    attempt_path
                        .join("tmp/candidate-0/git-common/evidence")
                        .is_file()
                );
                assert_eq!(
                    std::fs::read_to_string(attempt_path.join("result.md"))?,
                    "retained provider evidence"
                );
                Ok(())
            },
        )??;
    }
    Ok(())
}
#[test]
#[allow(clippy::disallowed_methods)] // The wait command has a real-time deadline.
fn wait_handles_a_stale_default_cursor() -> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane("run::tests::wait_handles_a_stale_default_cursor")? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let home = root.join("home");
    let project = root.join("checkout");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(root.join("state"))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.join("state"), std::fs::Permissions::from_mode(0o700))?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd: &project,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|e| e.message)?;
    let session = open(&edge, &call).map_err(|e| e.message)?;
    let make = |n: u64| -> Result<run::PreparedRun, Box<dyn std::error::Error>> {
        let mut prepared = trace_run(1, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&n.to_be_bytes());
        prepared.run_id = RunId::from_bytes(bytes);
        bytes[0] = 1;
        let task = TaskId::from_bytes(bytes);
        bytes[0] = 2;
        prepared.operation_id = OperationId::parse(&format!("backlog-{n}"))?;
        prepared.request.tasks[0].task_id = task;
        prepared.attempts[0].task_id = task;
        prepared.attempts[0].attempt_id = AttemptId::from_bytes(bytes);
        Ok(prepared)
    };
    let seed = make(1)?;
    session.store.create(&seed, &call)?;
    session.store.withdraw(
        seed.attempts[0].attempt_id,
        run::AttemptState::Queued {
            extra: Default::default(),
        },
        run::Outcome::Cancelled,
        "fixture",
        clock.wall_now()?,
        &call,
    )?;
    let cursor = session
        .store
        .last_event_sequence(seed.attempts[0].attempt_id, &call)?;
    session.store.ack_seen("default", cursor, &call)?;
    let active = make(2)?;
    session.store.create(&active, &call)?;
    let id = active.attempts[0].attempt_id;
    struct FetchHookGuard;
    impl Drop for FetchHookGuard {
        fn drop(&mut self) {
            wait::AFTER_FIRST_FETCH.with(|hook| *hook.borrow_mut() = None);
        }
    }
    let _hook_guard = FetchHookGuard;
    let context = session.context.clone();
    wait::AFTER_FIRST_FETCH.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let store = run::RunStore::open(&context, &call).unwrap();
            store
                .withdraw(
                    id,
                    run::AttemptState::Queued {
                        extra: Default::default(),
                    },
                    run::Outcome::Cancelled,
                    "fixture",
                    clock.wall_now().unwrap(),
                    &call,
                )
                .unwrap();
        }));
    });
    let picks = Picks {
        targets: &[],
        owner: session.context.actor_id,
        label: None,
        since: None,
    };
    let (data, _, timed_out) = wait_command(
        &session,
        &picks,
        Condition::Any,
        Instant::now() + Duration::from_secs(5),
        clock.wall_now()?,
        &edge,
    )
    .map_err(|e| e.message)?;
    assert!(!timed_out);
    assert!(
        data["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lane| lane["attempt_id"] == json!(id))
    );
    let (next, _, _) = wait_command(
        &session,
        &picks,
        Condition::Any,
        Instant::now() + Duration::from_secs(5),
        clock.wall_now()?,
        &edge,
    )
    .map_err(|e| e.message)?;
    assert!(next["lanes"].as_array().unwrap().is_empty());
    assert!(next.get("settled_backlog").is_none());
    Ok(())
}

#[path = "tests_access.rs"]
mod access_tests;
#[path = "tests_graph.rs"]
mod graph_tests;
#[path = "tests_lanes.rs"]
mod lanes_tests;
#[path = "tests_provider.rs"]
mod provider_tests;
#[path = "tests_reporting.rs"]
mod reporting_tests;
#[path = "tests_submission.rs"]
mod submission_tests;
