use super::*;
use crate::Mode;
use provenant_records::CanonicalV1;

pub(super) fn reexec_test_in_lane(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
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
    let attempt_id = AttemptId::from_bytes([n + 2; 16]);
    Ok(run::PreparedRun {
        attempt_id,
        operation_id: OperationId::parse(&format!("trace-{n}"))?,
        created_at,
        request: run::CanonicalRequest {
            extra: Default::default(),
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            task: run::TaskRequest {
                context: Some(provenant_records::run::WorkContext {
                    graph_id: Some(format!("lane-{attempt_id}")),
                    node_id: Some("node".into()),
                    attempt: Some(1),
                    ..Default::default()
                }),
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                source_head: None,
                base_ref: None,
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
            },
            parent: None,
            lane_depth: Some(1),
            parent_attempt: Some(parent_attempt),
        },
        prompt: "p".into(),
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

pub(super) fn clean_exit() -> ExitObservation {
    ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
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
    db.execute_batch("CREATE TABLE session_v2 (id TEXT PRIMARY KEY); CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)")?;
    db.execute("INSERT INTO session_v2 VALUES (?1)", [session])?;
    db.execute(
        "INSERT INTO session_message VALUES (?1, 'assistant', ?2)",
        (
            session,
            serde_json::to_string(&json!({"model": {"providerID": provider, "id": model}}))?,
        ),
    )?;
    Ok(())
}

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
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    })
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
    let chair = Some(provenant_records::custody::Chair {
        process: None,
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([4; 16]),
        actor: ActorId::from_bytes([5; 16]),
    });
    let submission = |base: &str| Submission {
        chair: chair.clone(),
        operation_id: OperationId::parse("context-ref-replay").unwrap(),
        task: Submitted {
            attempt: Default::default(),
            require_skill_location: true,
            settlement_retry_limit: None,
            context: Some(provenant_records::run::WorkContext {
                graph_id: Some("context-ref".into()),
                node_id: Some("node".into()),
                attempt: Some(1),
                ..Default::default()
            }),
            title: None,
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
        },
        knobs: vec![],
    };
    // The fake adapter refuses preparation after admission. Keep the durable
    // operation as the replay subject even though the initial launch fails.
    let initial = submit(&session, &edge, submission("context-base"), &call)
        .err()
        .ok_or("unknown fixture adapter must refuse its initial launch")?;
    assert_eq!(initial.status, Status::Refused, "{initial:?}");
    assert_eq!(initial.kind, ErrorKind::Refused, "{initial:?}");
    let operation = session
        .store
        .operation(&OperationId::parse("context-ref-replay")?, &call)?
        .ok_or("refused launch lost its admitted operation")?;
    assert_eq!(operation.state, run::OperationState::Terminal);
    let first = session
        .store
        .operation_attempt(&OperationId::parse("context-ref-replay")?, &call)?;
    let graph = session.store.work_key(first, &call)?.graph().to_owned();
    assert_eq!(graph, "context-ref");
    assert!(
        session
            .store
            .request(first, &call)?
            .task
            .profile
            .require_skill_location
    );
    with_chair_index(&roots, &call, |index, index_call| {
        let binding = index
            .read(session.context.project_id, &graph, index_call)?
            .unwrap();
        assert_eq!(binding.holder.chair().cloned(), chair);
        assert_eq!(
            binding.provenance,
            Some(
                session
                    .store
                    .request(first, &call)
                    .unwrap()
                    .digest()
                    .unwrap()
            )
        );
        Ok(())
    })
    .map_err(|f| f.message)?;
    std::fs::rename(
        roots.state.as_path().join("fabric/host.sqlite3"),
        roots.state.as_path().join("fabric/interrupted.sqlite3"),
    )?;
    // Reusing an operation from another chair cannot rebind even a missing
    // host index row. Repair must use the immutable original request.
    let mut replay = submission("context-base");
    replay.chair = Some(provenant_records::custody::Chair {
        process: None,
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([6; 16]),
        actor: ActorId::from_bytes([7; 16]),
    });
    let (replayed, _) = submit(&session, &edge, replay, &call).map_err(|f| f.message)?;
    assert_eq!(replayed.attempt_id, first);
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
                .read(session.context.project_id, &graph, call)?
                .unwrap()
                .holder
                .chair()
                .cloned(),
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
        assert_eq!(replayed.attempt_id, first);
        assert_eq!(
            session.store.request(first, &call)?.task.base.as_deref(),
            Some(original_base.as_str())
        );
    }
    assert!(
        matches!(submit(&session, &edge, submission("HEAD"), &call), Err(fail) if fail.kind == ErrorKind::OperationConflict)
    );
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
            // Recovery evidence is attempt-scoped inside the slot; a malformed
            // reservation there is reported and retained, never discarded.
            let attempt = slot.join(AttemptId::from_bytes([7; 16]).to_string());
            std::fs::create_dir_all(&attempt)?;
            let path = attempt.join("reservation.json");
            std::fs::write(&path, b"malformed retained reservation")?;
            let reply = reconcile(
                Reconcile {
                    targets: Vec::new(),
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
            Ok(())
        },
    )??;
    Ok(())
}
/// Give the fixture's one task the work key `graph/node#1` and save the
/// graph's checkpoint with that attempt, so `NAME[/NODE[#N]]` targets resolve.
#[cfg(feature = "fixture")]
fn keyed(
    edge: &Edge<'_>,
    project: provenant_core::ProjectId,
    prepared: &mut run::PreparedRun,
    (graph, node): (&str, &str),
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{
        FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
    };
    prepared.request.task.context = Some(provenant_records::run::WorkContext {
        graph_id: Some(graph.into()),
        node_id: Some(node.into()),
        attempt: Some(1),
        ..Default::default()
    });
    let reference = format!(
        "{}/{}",
        provenant_core::HostId::from_bytes([1; 16]),
        prepared.attempt_id
    );
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 5)?;
    let mut journal =
        FileJournal::open(&edge.roots.state, project, graph, budget, &cancel, &clock)?;
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2, "graph_id":graph,
        "nodes":[{"id":node, "access":"read_only", "prompt":"Go", "route":{"alias":"senior"}}]
    }))?;
    journal.bind(GraphExecution {
        extra: Default::default(),
        bases: None,
        directory: edge.cwd.to_path_buf(),
        document,
    })?;
    let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":1, "graph_id":graph, "plan_digest":"fixture",
        "nodes":[{"id":node, "state":"running", "attempts":[{
            "launched":true, "key":{"graph_id":graph, "node_id":node, "route_index":0, "attempt": 1},
            "route":{"alias":"senior"}, "state":"running", "attempt_ref":reference,
            "outcome":null, "launch_actor":"fixture"
        }]}]
    }))?;
    journal.save(&checkpoint)?;
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
                input.request.task.writer = Some(run::WriterRequest {
                    extra: [("custody_project".into(), json!(session.context.project_id))].into(),
                    project: fixture.project.clone(),
                    worktree: fixture.tree.clone(),
                });
                keyed(
                    edge,
                    session.context.project_id,
                    &mut input,
                    ("fence", "writer"),
                )?;
                session.store.create(&input, &call)?;
                let id = input.attempt_id;
                let record = session.store.mark_starting_for_fixture(
                    id,
                    &run::RouteRecord::default(),
                    &call,
                )?;
                let scope = Scope {
                    attempt: id,
                    generation: record.generation,
                };
                let attempt_path = edge.roots.state.as_path().join(format!("attempts/{id}"));
                let attempt = Directory::open_anchor(&attempt_path)?;
                let slot_path = edge.roots.state.as_path().join(format!(
                    "worktrees/{}/abandon-fixture",
                    session.context.project_id
                ));
                provenant_os::fixture::private_state_dir(&slot_path)?;
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
                        handoff: None,
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
                    let found =
                        session
                            .store
                            .reconcile_for_fixture(id, &Registry::default(), &call)?;
                    let record = session.store.record(id, &call)?;
                    // Settlement appends the work-location note to the blocker. This
                    // fixture's private Git holds no refs, so the note says the files
                    // were not restored, and why.
                    let expected_reason =
                        match record.settlement_receipts.as_ref().and_then(|receipts| {
                            match &receipts.git_import {
                                run::EffectDisposition::Failed {
                                    retryable: false,
                                    reason,
                                } => Some(reason.clone()),
                                _ => None,
                            }
                        }) {
                            Some(reason) => {
                                assert!(
                                    reason.strip_prefix(blocker).is_some_and(|note| {
                                        note.strip_prefix("; worktree files not restored: ")
                                            .is_some_and(|why| !why.is_empty())
                                    }),
                                    "{found:?}"
                                );
                                reason
                            }
                            None => panic!("unexpected git_import disposition: {record:?}"),
                        };
                    {
                        let crate::ParseOutcome::Invocation(inv) = crate::parse(
                            ["output", "fence/writer", "--full"]
                                .into_iter()
                                .map(std::ffi::OsString::from),
                        ) else {
                            return Err("parse output".into());
                        };
                        let crate::Command::Output(command) = inv.command else {
                            return Err("output command".into());
                        };
                        let reply = output_from_session(command, edge, &session, id, &call);
                        assert_eq!(reply.result.status, Status::Ok, "{}", reply.human);
                        let row = &reply.result.data;
                        assert_eq!(row["state"], "failed");
                        assert_eq!(row["git_import_failure"]["reason"], expected_reason);
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
                        assert_eq!(
                            reply.result.data["bytes"]["data"],
                            "retained provider evidence"
                        );
                        for text in [&reply.human, &summary, &rendered] {
                            assert!(
                                text.contains("commits may not have reached its branch"),
                                "{text}"
                            );
                            assert!(text.contains(&"a".repeat(40)), "{text}");
                            assert!(text.contains("git-objects"), "{text}");
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
                    session
                        .store
                        .drain_reactions(id, &clock, &cancel, &mut |receipt, _| {
                            receipt.graph_done = true;
                            receipt.notice_done = true;
                            receipt.log_done = true;
                            receipt.ledger_done = true;
                            Ok(())
                        })?;
                    let reported = reconcile(parse(&["reconcile", "fence", "--json"])?, edge);
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
                let reported = reconcile(parse(&["reconcile", "fence", "--json"])?, edge);
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
                let selection = "fence/writer#1";
                assert!(parse(&["reconcile", "--abandon", selection]).is_err());
                assert!(parse(&["reconcile", "--reason", "missing attempt"]).is_err());
                assert!(
                    parse(&[
                        "reconcile",
                        "fence",
                        "--abandon",
                        selection,
                        "--reason",
                        "ambiguous action"
                    ])
                    .is_err()
                );
                let args = [
                    "reconcile",
                    "--abandon",
                    selection,
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
                    .reconcile_for_fixture(id, &Registry::default(), &call)?;
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
                reconcile(parse(&["reconcile", "fence", "--json"])?, edge);
                assert!(
                    attempt_path
                        .join("tmp/candidate-0/git-common/evidence")
                        .is_file()
                );
                let different = [
                    "reconcile",
                    "--abandon",
                    selection,
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
