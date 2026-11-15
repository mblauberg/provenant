use super::*;
use crate::Mode;
use provenant_records::CanonicalV1;

pub(super) fn capacity_reason() -> &'static str {
    provenant_records::diagnostic::ProviderFailureFact::Capacity.reason()
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

use provenant_records::custody::Scope;
use provenant_run::Providers;
use provenant_supervisor::custody::CaptureTotals;
use provenant_supervisor::custody::PublicationState;
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
                limit_extensions: Default::default(),
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
    const MARKER: &str = "PROVENANT_TEST_INVALID_CAPACITY_ROOT";
    if std::env::var_os(MARKER).is_none() {
        let out = crate::test_support::rerun_test(
            module_path!(),
            "invalid_sealed_root_never_writes_to_registry_root",
            &[(MARKER.into(), "1".into())],
            &[MARKER, provenant_core::RUN_STATE_DIRECTORY_ENV],
            Duration::from_secs(30),
        )?;
        assert_eq!(
            out.termination,
            provenant_os::exec::Termination::Exit(0),
            "{out:?}"
        );
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
    let protected = inputs.home.join(".ssh");
    let context = run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.path().join("home"),
            checkout: temp.path().join("checkout"),
            state_root: protected.clone(),
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
    assert!(!roots.state.as_path().join("quota").exists());
    assert!(!protected.exists());
    Ok(())
}
#[test]
fn lane_gitdir_resolves_the_registered_project_without_querying_primary()
-> Result<(), Box<dyn std::error::Error>> {
    if crate::fabric::in_lane() {
        let out = crate::test_support::rerun_test(
            module_path!(),
            "lane_gitdir_resolves_the_registered_project_without_querying_primary",
            &[],
            &[
                provenant_core::RUN_STATE_DIRECTORY_ENV,
                provenant_core::consumer_env::LANE_DEPTH,
                provenant_core::consumer_env::PARENT_ATTEMPT_ID,
            ],
            Duration::from_secs(30),
        )?;
        assert_eq!(
            out.termination,
            provenant_os::exec::Termination::Exit(0),
            "{out:?}"
        );
        assert!(!out.timed_out, "{out:?}");
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
    const MARKER: &str = "PROVENANT_TEST_NESTED_CAPACITY_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let out = crate::test_support::rerun_test(
            module_path!(),
            "nested_capacity_settlement_does_not_write_shared_cooldown",
            &[
                (MARKER.into(), "1".into()),
                (
                    provenant_core::RUN_STATE_DIRECTORY_ENV.into(),
                    "nested".into(),
                ),
            ],
            &[MARKER, provenant_core::RUN_STATE_DIRECTORY_ENV],
            Duration::from_secs(30),
        )?;
        assert_eq!(
            out.termination,
            provenant_os::exec::Termination::Exit(0),
            "{out:?}"
        );
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
    assert_eq!(decoded.reason.as_deref(), Some(capacity_reason()));
    assert!(!roots.state.as_path().join("quota").exists());
    Ok(())
}
fn reexec_operation_test(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_none()
        && std::env::var_os(provenant_core::consumer_env::LANE_DEPTH).is_none()
        && std::env::var_os(provenant_core::consumer_env::PARENT_ATTEMPT_ID).is_none()
    {
        return Ok(false);
    }
    let out = crate::test_support::rerun_test(
        module_path!(),
        name,
        &[],
        &[
            provenant_core::RUN_STATE_DIRECTORY_ENV,
            provenant_core::consumer_env::LANE_DEPTH,
            provenant_core::consumer_env::PARENT_ATTEMPT_ID,
        ],
        Duration::from_secs(90),
    )?;
    assert_eq!(
        out.termination,
        provenant_os::exec::Termination::Exit(0),
        "{out:?}"
    );
    assert!(!out.timed_out, "{out:?}");
    Ok(true)
}

struct OperationReplayFixture {
    _temp: tempfile::TempDir,
    roots: provenant_core::Roots,
    home: PathBuf,
    cwd: PathBuf,
    executor: provenant_os::exec::SystemExecutor,
    git: PathBuf,
    clock: provenant_core::TestClock,
    cancel: CancelToken,
    session: Session,
    chair: provenant_records::custody::Chair,
    original_base: String,
    first_attempt: AttemptId,
    graph: String,
    operation_id: OperationId,
}

impl OperationReplayFixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        use provenant_core::RootInputs;
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
        use std::ffi::OsString;

        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let cwd = root.join("checkout");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&cwd)?;
        let mut inputs = RootInputs::new(home.clone(), cwd.clone());
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let executor = provenant_os::exec::SystemExecutor::new(1)?;
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
            assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
            assert!(!out.timed_out, "{out:?}");
            Ok(String::from_utf8(out.stdout.bytes)?.trim().to_owned())
        };
        command(&["init", "-q"])?;
        command(&["commit", "--allow-empty", "-qm", "initial"])?;
        command(&["branch", "context-base"])?;
        let original_base = command(&["rev-parse", "context-base"])?;
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0)?,
            provenant_core::MonoTime::from_micros(0),
        );
        let cancel = CancelToken::new();
        let call = run::Request {
            clock: &clock,
            cancel: &cancel,
        };
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
        let chair = provenant_records::custody::Chair {
            process: None,
            extra: Default::default(),
            session: None,
            host: provenant_core::HostId::from_bytes([4; 16]),
            actor: ActorId::from_bytes([5; 16]),
        };
        let operation_id = OperationId::parse("context-ref-replay")?;
        let initial = submit(
            &session,
            &edge,
            operation_replay_submission(operation_id.clone(), Some(chair.clone()), &original_base),
            &call,
        )
        .err()
        .ok_or("unknown fixture adapter must refuse its initial launch")?;
        assert_eq!(initial.status, Status::Refused);
        assert_eq!(initial.kind, ErrorKind::Refused);
        let first_attempt = session.store.operation_attempt(&operation_id, &call)?;
        let graph = session
            .store
            .work_key(first_attempt, &call)?
            .graph()
            .to_owned();
        Ok(Self {
            _temp: temp,
            roots,
            home,
            cwd,
            executor,
            git,
            clock,
            cancel,
            session,
            chair,
            original_base,
            first_attempt,
            graph,
            operation_id,
        })
    }

    fn call(&self) -> run::Request<'_> {
        run::Request {
            clock: &self.clock,
            cancel: &self.cancel,
        }
    }

    fn edge(&self) -> Edge<'_> {
        Edge {
            roots: &self.roots,
            home: &self.home,
            assets: &self.cwd,
            cwd: &self.cwd,
        }
    }

    fn command(&self, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
        use std::ffi::OsString;
        let argv: Vec<OsString> = std::iter::once(self.git.as_os_str().to_owned())
            .chain(args.iter().map(OsString::from))
            .collect();
        let env = provenant_os::fixture::fixture_git_settings();
        let mut options = ExecOptions::new(&argv, &self.cwd, Duration::from_secs(10));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let out = self.executor.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        assert!(!out.timed_out, "{out:?}");
        Ok(String::from_utf8(out.stdout.bytes)?.trim().to_owned())
    }

    fn submission(
        &self,
        base: &str,
        chair: Option<provenant_records::custody::Chair>,
    ) -> Submission {
        operation_replay_submission(self.operation_id.clone(), chair, base)
    }
}

fn operation_replay_submission(
    operation_id: OperationId,
    chair: Option<provenant_records::custody::Chair>,
    base: &str,
) -> Submission {
    Submission {
        chair,
        operation_id,
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
    }
}

#[test]
fn operation_replay_keeps_original_context_when_base_ref_moves_or_disappears()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_operation_test(
        "operation_replay_keeps_original_context_when_base_ref_moves_or_disappears",
    )? {
        return Ok(());
    }
    let fixture = OperationReplayFixture::new()?;
    fixture.command(&["commit", "--allow-empty", "-qm", "moved"])?;
    fixture.command(&["branch", "-f", "context-base", "HEAD"])?;
    for deleted in [false, true] {
        if deleted {
            fixture.command(&["branch", "-D", "context-base"])?;
        }
        let call = fixture.call();
        let (replayed, _) = submit(
            &fixture.session,
            &fixture.edge(),
            fixture.submission("context-base", Some(fixture.chair.clone())),
            &call,
        )
        .map_err(|fail| fail.message)?;
        assert_eq!(replayed.attempt_id, fixture.first_attempt);
        assert_eq!(
            fixture
                .session
                .store
                .request(fixture.first_attempt, &call)?
                .task
                .base
                .as_deref(),
            Some(fixture.original_base.as_str())
        );
    }
    Ok(())
}

#[test]
fn operation_replay_preserves_custody_identity_when_host_index_is_unavailable()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_operation_test(
        "operation_replay_preserves_custody_identity_when_host_index_is_unavailable",
    )? {
        return Ok(());
    }
    use provenant_records::custody::Chair;
    let fixture = OperationReplayFixture::new()?;
    let call = fixture.call();
    let operation = fixture
        .session
        .store
        .operation(&fixture.operation_id, &call)?
        .ok_or("refused launch lost its admitted operation")?;
    assert_eq!(operation.state, run::OperationState::Terminal);
    let request = fixture
        .session
        .store
        .request(fixture.first_attempt, &call)?;
    assert!(request.task.profile.require_skill_location);
    with_chair_index(&fixture.roots, &call, |index, index_call| {
        let binding = index
            .read(
                fixture.session.context.project_id,
                &fixture.graph,
                index_call,
            )?
            .ok_or_else(|| {
                Fail::new(Status::Error, ErrorKind::Identity, "missing chair binding")
            })?;
        assert_eq!(binding.holder.chair(), Some(&fixture.chair));
        assert_eq!(binding.provenance, Some(request.digest().unwrap()));
        Ok(())
    })
    .map_err(|fail| fail.message)?;

    std::fs::rename(
        fixture.roots.state.as_path().join("fabric/host.sqlite3"),
        fixture
            .roots
            .state
            .as_path()
            .join("fabric/interrupted.sqlite3"),
    )?;
    let other_chair = Chair {
        process: None,
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([6; 16]),
        actor: ActorId::from_bytes([7; 16]),
    };
    let call = fixture.call();
    let (replayed, _) = submit(
        &fixture.session,
        &fixture.edge(),
        fixture.submission("context-base", Some(other_chair)),
        &call,
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(replayed.attempt_id, fixture.first_attempt);

    let stopped = CancelToken::new();
    stopped.cancel();
    let cancelled = run::Request {
        clock: &fixture.clock,
        cancel: &stopped,
    };
    let failure = with_chair_index(&fixture.roots, &cancelled, |_, _| Ok(())).unwrap_err();
    assert_eq!(failure.status, Status::Error);
    with_chair_index(&fixture.roots, &call, |index, index_call| {
        let binding = index
            .read(
                fixture.session.context.project_id,
                &fixture.graph,
                index_call,
            )?
            .ok_or_else(|| {
                Fail::new(Status::Error, ErrorKind::Identity, "missing chair binding")
            })?;
        assert_eq!(binding.holder.chair(), Some(&fixture.chair));
        Ok(())
    })
    .map_err(|fail| fail.message)?;
    Ok(())
}

#[test]
fn operation_id_conflicts_when_replayed_with_a_different_base()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_operation_test("operation_id_conflicts_when_replayed_with_a_different_base")? {
        return Ok(());
    }
    let fixture = OperationReplayFixture::new()?;
    let call = fixture.call();
    assert!(matches!(
        submit(
            &fixture.session,
            &fixture.edge(),
            fixture.submission("HEAD", Some(fixture.chair.clone())),
            &call
        ),
        Err(failure) if failure.kind == ErrorKind::OperationConflict
    ));
    Ok(())
}
#[test]
fn reconcile_reports_orphan_reservation_issues() -> Result<(), Box<dyn std::error::Error>> {
    if crate::fabric::in_lane() {
        let out = crate::test_support::rerun_test(
            module_path!(),
            "reconcile_reports_orphan_reservation_issues",
            &[],
            &[
                provenant_core::RUN_STATE_DIRECTORY_ENV,
                provenant_core::consumer_env::LANE_DEPTH,
                provenant_core::consumer_env::PARENT_ATTEMPT_ID,
            ],
            Duration::from_secs(30),
        )?;
        assert_eq!(
            out.termination,
            provenant_os::exec::Termination::Exit(0),
            "{out:?}"
        );
        assert!(!out.timed_out, "{out:?}");
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
    let clock = provenant_core::TestClock::new(
        WallTime::from_micros(0)?,
        provenant_core::MonoTime::from_micros(0),
    );
    let cancel = CancelToken::new();
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 5)?;
    let mut journal =
        FileJournal::open(&edge.roots.state, project, graph, budget, &cancel, &clock)?;
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2, "graph_id":graph,
        "nodes":[{"id":node, "access":"read_only", "prompt":"Go", "route":{"alias":"senior"}}]
    }))?;
    journal.bind(GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,

        extra: Default::default(),
        bases: None,
        directory: edge.cwd.to_path_buf(),
        document,
    })?;
    let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":2, "graph_id":graph, "plan_digest":"fixture",
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
struct ReconcileFixture {
    _linked: LinkedFixture,
    roots: provenant_core::Roots,
    home: PathBuf,
    clock: provenant_core::TestClock,
    cancel: CancelToken,
    session: Session,
    id: AttemptId,
    scope: Scope,
    attempt_path: PathBuf,
    recovery: Option<provenant_supervisor::custody::Recovery>,
    import: run::EffectDisposition,
    phase: &'static str,
    blocker: &'static str,
}

#[cfg(feature = "fixture")]
impl ReconcileFixture {
    fn new(import_done: bool, settle_failed: bool) -> Result<Self, Box<dyn std::error::Error>> {
        use provenant_core::RootInputs;
        use provenant_os::fs::{Directory, PublishMode};
        use provenant_records::custody::Scope;
        use provenant_supervisor::custody::{self};
        use std::ffi::OsStr;

        let linked = linked_fixture()?;
        let home = linked.root.join("home");
        std::fs::create_dir_all(&home)?;
        let mut inputs = RootInputs::new(home.clone(), linked.project.clone());
        inputs.state_dir = Some(linked.root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let clock = provenant_core::TestClock::new(
            WallTime::from_micros(0)?,
            provenant_core::MonoTime::from_micros(0),
        );
        let cancel = CancelToken::new();
        let call = run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let edge = Edge {
            roots: &roots,
            home: &home,
            assets: &linked.project,
            cwd: &linked.project,
        };
        let mut session = open(&edge, &call).map_err(|fail| fail.message)?;
        let mut input = trace_run(30, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
        input.request.parent_attempt = None;
        input.request.lane_depth = None;
        input.request.task.writer = Some(run::WriterRequest {
            extra: Default::default(),
            custody_project: Some(session.context.project_id),
            project: linked.project.clone(),
            worktree: linked.tree.clone(),
        });
        keyed(
            &edge,
            session.context.project_id,
            &mut input,
            ("fence", "writer"),
        )?;
        session.store.create(&input, &call)?;
        let id = input.attempt_id;
        let record =
            session
                .store
                .mark_starting_for_fixture(id, &run::RouteRecord::default(), &call)?;
        let scope = Scope {
            attempt: id,
            generation: record.generation,
        };
        let attempt_path = roots.state.as_path().join(format!("attempts/{id}"));
        let attempt = Directory::open_anchor(&attempt_path)?;
        let slot_path = roots.state.as_path().join(format!(
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
        let recovery =
            provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?.into_recovery()?;
        let fence = session
            .store
            .writer_fences()?
            .into_iter()
            .find(|fence| fence.scope == scope)
            .ok_or("fence")?;
        let evidence =
            Directory::open_anchor(fence.reservation.parent().ok_or("reservation parent")?)?;
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
                handoff: Ok(None),
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
        let blocker = if import_done {
            "release evidence missing"
        } else {
            "lane Git import failed"
        };
        let phase = if import_done { "release" } else { "publish" };
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
                    "schema_version":1,
                    "scope":scope,
                    "phase":phase,
                    "attempts":1,
                    "head": if settle_failed { Some("a".repeat(40)) } else { None },
                    "base":null,
                    "owner":null,
                    "blocking_reason":blocker
                }))?,
                PublishMode::Replace,
            )?
            .require_durable()?;
        std::fs::create_dir_all(attempt_path.join("tmp/candidate-0/git-common"))?;
        std::fs::write(
            attempt_path.join("tmp/candidate-0/git-common/evidence"),
            "private objects",
        )?;

        Ok(Self {
            _linked: linked,
            roots,
            home,
            clock,
            cancel,
            session,
            id,
            scope,
            attempt_path,
            recovery: Some(recovery),
            import,
            phase,
            blocker,
        })
    }

    fn edge(&self) -> Edge<'_> {
        Edge {
            roots: &self.roots,
            home: &self.home,
            assets: &self._linked.project,
            cwd: &self._linked.project,
        }
    }

    fn request(&self) -> run::Request<'_> {
        run::Request {
            clock: &self.clock,
            cancel: &self.cancel,
        }
    }
}

#[cfg(feature = "fixture")]
fn rerun_reconcile_test(module: &str, name: &str) -> Result<bool, Box<dyn std::error::Error>> {
    if !crate::fabric::in_lane() {
        return Ok(false);
    }
    let out = crate::test_support::rerun_test(
        module,
        name,
        &[],
        &[
            provenant_core::RUN_STATE_DIRECTORY_ENV,
            provenant_core::consumer_env::LANE_DEPTH,
            provenant_core::consumer_env::PARENT_ATTEMPT_ID,
        ],
        Duration::from_secs(30),
    )?;
    assert_eq!(
        out.termination,
        provenant_os::exec::Termination::Exit(0),
        "{out:?}"
    );
    assert!(!out.timed_out, "{out:?}");
    Ok(true)
}

#[cfg(feature = "fixture")]
fn parse_reconcile(args: &[&str]) -> Result<Reconcile, Box<dyn std::error::Error>> {
    let crate::ParseOutcome::Invocation(inv) =
        crate::parse(args.iter().map(std::ffi::OsString::from))
    else {
        return Err("parse reconcile".into());
    };
    let crate::Command::Reconcile(command) = inv.command else {
        return Err("reconcile command".into());
    };
    Ok(command)
}

#[cfg(feature = "fixture")]
fn assert_reconcile_abandonment(
    import_done: bool,
    reason: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::fs::{Directory, LockMode};

    let mut fixture = ReconcileFixture::new(import_done, false)?;
    let edge = fixture.edge();
    let call = fixture.request();
    let attempt = Directory::open_anchor(&fixture.attempt_path)?;
    let selection = "fence/writer#1";
    assert!(parse_reconcile(&["reconcile", "--abandon", selection]).is_err());
    assert!(parse_reconcile(&["reconcile", "--reason", "missing attempt"]).is_err());
    assert!(
        parse_reconcile(&[
            "reconcile",
            "fence",
            "--abandon",
            selection,
            "--reason",
            "ambiguous action"
        ])
        .is_err()
    );

    let driver = attempt.try_lock(
        std::ffi::OsStr::new("settlement-driver.lock"),
        LockMode::Exclusive,
    )?;
    let found = reconcile(parse_reconcile(&["reconcile", "fence", "--json"])?, &edge);
    let found_data = serde_json::to_value(&found.result)?;
    assert_eq!(
        found_data["data"]["runs"][0]["attempts"][0]["found"]["phase"],
        fixture.phase
    );
    assert_eq!(
        found_data["data"]["runs"][0]["attempts"][0]["found"]["blocking_reason"],
        fixture.blocker
    );

    let args = [
        "reconcile",
        "--abandon",
        selection,
        "--reason",
        reason,
        "--json",
    ];
    let while_driving = reconcile(parse_reconcile(&args)?, &edge);
    assert_ne!(while_driving.result.status, Status::Ok);
    drop(driver);
    let while_recovery_owned = reconcile(parse_reconcile(&args)?, &edge);
    assert_ne!(while_recovery_owned.result.status, Status::Ok);
    drop(fixture.recovery.take());

    let abandoned = reconcile(parse_reconcile(&args)?, &edge);
    assert_eq!(abandoned.result.status, Status::Ok);
    assert_eq!(abandoned.result.data["found"]["state"], "abandoned");
    assert_eq!(abandoned.result.data["found"]["reason"], reason);
    assert!(fixture.session.store.writer_fences()?.is_empty());
    fixture
        .session
        .store
        .reconcile_for_fixture(fixture.id, &Registry::default(), &call)?;
    assert_eq!(
        fixture
            .session
            .store
            .record(fixture.id, &call)?
            .settlement_receipts
            .ok_or("settlement receipts")?
            .git_import,
        fixture.import
    );

    let journal: Value = serde_json::from_slice(&std::fs::read(
        fixture.attempt_path.join("settlement-progress.json"),
    )?)?;
    assert_eq!(
        journal["abandonment"]["actor"],
        fixture.session.context.actor_id.to_string()
    );
    assert_eq!(journal["abandonment"]["phase"], fixture.phase);
    assert_eq!(journal["blocking_reason"], fixture.blocker);
    assert_eq!(
        reconcile(parse_reconcile(&args)?, &edge).result.status,
        Status::Ok
    );
    reconcile(parse_reconcile(&["reconcile", "fence", "--json"])?, &edge);
    assert!(
        fixture
            .attempt_path
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
        reconcile(parse_reconcile(&different)?, &edge).result.status,
        Status::Ok
    );

    let state = Directory::open_anchor(fixture.roots.state.as_path())?;
    let later = WallTime::from_micros(fixture.clock.wall_now()?.as_micros() + 90 * 86_400_000_000)?;
    let collected = fixture
        .session
        .store
        .gc_provider_state(&state, 7, later, false, &call)?;
    assert!(collected.attempt_tmp.is_empty());
    assert!(
        fixture
            .attempt_path
            .join("tmp/candidate-0/git-common/evidence")
            .is_file()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.attempt_path.join("result.md"))?,
        "retained provider evidence"
    );
    Ok(())
}

#[cfg(feature = "fixture")]
#[test]
fn reconcile_failed_settlement_keeps_import_failure_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_reconcile_test(
        module_path!(),
        "reconcile_failed_settlement_keeps_import_failure_evidence",
    )? {
        return Ok(());
    }
    use provenant_os::fs::{Directory, PublishMode};
    use std::ffi::OsStr;

    let mut fixture = ReconcileFixture::new(false, true)?;
    let edge = fixture.edge();
    let call = fixture.request();
    let attempt = Directory::open_anchor(&fixture.attempt_path)?;
    attempt
        .atomic_write(
            OsStr::new("launch.json"),
            ".test-",
            &serde_json::to_vec(&json!({
                "schema_version":1,
                "scope":fixture.scope,
                "requester":fixture.session.context.actor_id
            }))?,
            PublishMode::NoReplace,
        )?
        .require_durable()?;
    std::fs::create_dir_all(fixture.attempt_path.join("tmp/candidate-0/git-objects"))?;
    std::fs::write(
        fixture
            .attempt_path
            .join("tmp/candidate-0/git-objects/evidence"),
        b"retained objects",
    )?;
    attempt
        .atomic_write(
            OsStr::new("writer-objects.json"),
            ".test-",
            &serde_json::to_vec(&json!({
                "schema_version":1,
                "candidate":"candidate-0"
            }))?,
            PublishMode::NoReplace,
        )?
        .require_durable()?;
    drop(fixture.recovery.take());

    fixture
        .session
        .store
        .reconcile_for_fixture(fixture.id, &Registry::default(), &call)?;
    let record = fixture.session.store.record(fixture.id, &call)?;
    let expected_reason = match record
        .settlement_receipts
        .as_ref()
        .map(|receipts| &receipts.git_import)
    {
        Some(run::EffectDisposition::Failed {
            retryable: false,
            reason,
        }) => reason,
        _ => return Err("unexpected git import disposition".into()),
    };
    assert!(
        expected_reason
            .strip_prefix(fixture.blocker)
            .is_some_and(|note| !note.is_empty())
    );

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
    let reply = output_from_session(command, &edge, &fixture.session, fixture.id, &call);
    assert_eq!(reply.result.status, Status::Ok);
    let row = &reply.result.data;
    assert_eq!(row["state"], "failed");
    assert_eq!(
        row["git_import_failure"]["reason"].as_str(),
        Some(expected_reason.as_str())
    );
    assert_eq!(row["git_import_failure"]["head"], "a".repeat(40));
    assert_eq!(
        row["git_import_failure"]["retained_objects"],
        json!(provenant_records::StructuredPathWire::from_path(
            &fixture.attempt_path.join("tmp/candidate-0/git-objects")
        )?)
    );
    assert_eq!(row["bytes"]["data"], "retained provider evidence");
    assert_eq!(
        std::fs::read(
            fixture
                .attempt_path
                .join("tmp/candidate-0/git-objects/evidence")
        )?,
        b"retained objects"
    );
    assert_eq!(
        std::fs::read(
            fixture
                .attempt_path
                .join("tmp/candidate-0/git-common/evidence")
        )?,
        b"private objects"
    );

    fixture.session.store.drain_reactions(
        fixture.id,
        &fixture.clock,
        &fixture.cancel,
        &mut |receipt, _| {
            receipt.graph_done = true;
            receipt.notice_done = true;
            receipt.log_done = true;
            receipt.ledger_done = true;
            Ok(())
        },
    )?;
    let reported = reconcile(parse_reconcile(&["reconcile", "fence", "--json"])?, &edge);
    assert_eq!(reported.result.status, Status::Ok);
    assert!(
        reported.result.data["runs"][0]["attempts"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    Ok(())
}

#[cfg(feature = "fixture")]
#[test]
fn reconcile_publish_blocker_requires_explicit_abandonment()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_reconcile_test(
        module_path!(),
        "reconcile_publish_blocker_requires_explicit_abandonment",
    )? {
        return Ok(());
    }
    assert_reconcile_abandonment(false, "worktree rebuilt by chair")
}

#[cfg(feature = "fixture")]
#[test]
fn reconcile_release_blocker_records_completed_import_phase()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_reconcile_test(
        module_path!(),
        "reconcile_release_blocker_records_completed_import_phase",
    )? {
        return Ok(());
    }
    assert_reconcile_abandonment(true, &"r".repeat(4096))
}

#[path = "tests_access.rs"]
mod access_tests;
#[path = "tests_graph.rs"]
mod graph_tests;
#[path = "tests_provider.rs"]
mod provider_tests;
#[path = "tests_reporting.rs"]
mod reporting_tests;
#[path = "tests_submission.rs"]
mod submission_tests;
