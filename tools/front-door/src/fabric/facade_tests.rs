#[cfg(test)]
#[test]
fn peer_remote_base_pins_a_commit_reachable_from_the_remote(
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{self, WorkerObserver, Occupancy};
    use std::collections::BTreeMap;
    struct Free;
    impl WorkerObserver for Free {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Available))
                .collect()
        }
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let project = temp.path().join("project");
    let remote = temp.path().join("remote.git");
    for path in [&home, &state, &project, &remote] {
        std::fs::create_dir_all(path)?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    provenant_os::fixture::run_git(&remote, &["init", "--bare", "-q"])?;
    provenant_os::fixture::run_git(&project, &["init", "-q"])?;
    provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
    provenant_os::fixture::run_git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().ok_or("remote path")?,
        ],
    )?;
    provenant_os::fixture::run_git(&project, &["push", "origin", "HEAD:refs/heads/main"])?;
    let initial = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["rev-parse", "HEAD"],
    )?)?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            let pinned = remote_base(
                context,
                "HEAD",
                std::time::Duration::from_secs(30),
                true,
                &cancel,
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert_eq!(pinned.to_string(), initial.trim());
            Ok(())
        },
    )?;
    let absent = temp.path().join("absent.git");
    provenant_os::fixture::run_git(
        &project,
        &[
            "remote",
            "add",
            "a-unavailable",
            absent.to_str().ok_or("absent path")?,
        ],
    )?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert_eq!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                )
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?
                .to_string(),
                initial.trim()
            );
            Ok(())
        },
    )?;
    provenant_os::fixture::run_git(&project, &["remote", "remove", "a-unavailable"])?;
    // Fetch leaves a deleted remote-tracking ref behind; it must not make the
    // old tip appear to remain advertised by the remote.
    provenant_os::fixture::run_git(&remote, &["update-ref", "-d", "refs/heads/main"])?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    true,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            Ok(())
        },
    )?;
    provenant_os::fixture::run_git(&remote, &["update-ref", "refs/heads/main", initial.trim()])?;
    let limited = worktrees::RepoFacts::with_limits(provenant_os::exec::DEFAULT_CAPTURE_LIMIT, 1)?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            let context = &worktrees::WorktreeContext {
                facts: &limited,
                ..*context
            };
            let error = remote_base(
                context,
                "HEAD",
                std::time::Duration::from_secs(30),
                false,
                &cancel,
            )
            .expect_err("bounded fact output must be reported");
            assert!(matches!(
                &error,
                FacadeError::Io { operation, .. }
                    if operation == provenant_fabric_runtime::coordination::facade::REPOSITORY_FACT_LIMIT_OPERATION
            ));
            let wire = serde_json::to_value(&error)
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert_eq!(wire["operation"], "repository fact output limit exceeded");
            let cancelled = CancelToken::new();
            cancelled.cancel();
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancelled
                ),
                Err(FacadeError::Cancelled)
            ));
            Ok(())
        },
    )?;
    // A replacement object must not make a local-only root look reachable
    // from the real remote-tracking commit.
    let tree = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["rev-parse", "HEAD^{tree}"],
    )?)?;
    let local = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["commit-tree", tree.trim(), "-m", "local only"],
    )?)?;
    let replacement = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            local.trim(),
            "-m",
            "poisoned ancestry",
        ],
    )?)?;
    provenant_os::fixture::run_git(&project, &["update-ref", "HEAD", local.trim()])?;
    provenant_os::fixture::run_git(&project, &["replace", initial.trim(), replacement.trim()])?;
    let contains = format!("--contains={}", local.trim());
    let poisoned = provenant_os::fixture::run_git(
        &project,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &contains,
            "refs/remotes",
        ],
    )?;
    assert!(
        !poisoned.is_empty(),
        "fixture must poison ordinary reachability"
    );
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            provenant_os::fixture::run_git(&project, &["replace", "-d", initial.trim()])
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let grafts = project.join(".git/info/grafts");
            std::fs::write(&grafts, format!("{} {}\n", initial.trim(), local.trim()))
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let poisoned = provenant_os::fixture::run_git(
                &project,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    &contains,
                    "refs/remotes",
                ],
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert!(
                !poisoned.is_empty(),
                "fixture must poison ordinary reachability"
            );
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            std::fs::remove_file(grafts).map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            std::fs::rename(&project, temp.path().join("moved-project"))
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            std::fs::create_dir(&project).map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    true,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            Ok(())
        },
    )?;
    Ok(())
}

#[cfg(test)]
#[test]
fn graph_document_decoder_checks_header_before_typed_graph() {
    for version in [1, 2] {
        let source = format!(r#"{{"schema_version":{version},"graph_id":"test","nodes":[]}}"#);
        let document = decode_graph_document(source.as_bytes()).unwrap();
        assert_eq!(document.schema_version, version);
    }
    assert!(matches!(
        decode_graph_document(br#"{"schema_version":3,"nodes":"future shape"}"#),
        Err(FacadeError::UpgradeRequired { version: 3, .. })
    ));
    assert!(decode_graph_document(br#"{"schema_version":1,"schema_version":2}"#).is_err());
    assert!(decode_graph_document(br#"{"schema_version":1,"nodes":"invalid"}"#).is_err());
    assert!(decode_graph_document(br#"{"graph_id":"test","nodes":[]}"#).is_err());
}

#[cfg(test)]
#[test]
fn cli_failure_uses_stderr_without_structured_json() {
    let error = cli_failure(b"not json", b"specific failure\nadditional detail\n");
    assert!(matches!(error, ServiceError::Cli { code, message, data }
        if code == "CLI_FAILED" && message == "specific failure" && data.is_null()));
}

#[cfg(test)]
#[test]
fn scratch_prepare_io_error_retains_its_cause() {
    let error = scratch_prepare_error("exclude lock timed out");
    assert!(error.to_string().contains("exclude lock timed out"));
    assert!(error.to_string().contains("prepare project scratch"));
    assert!(error.to_string().contains(".provenant/scratch"));
    assert!(matches!(error, FacadeError::Io { path: Some(path), .. }
        if path == ".provenant/scratch"));
}

#[test]
fn prompt_limit_refusal_keeps_safe_field_and_existing_code() {
    let error = run_error(provenant_run::RunError::PromptTooLarge {
        task_index: Some(0),
        limit: 1_048_576,
    });
    let projection = error.diagnostic();
    assert_eq!(projection.code, "invalid_input");
    let detail = projection.detail.unwrap();
    assert!(detail.contains("tasks[0].prompt"));
    assert!(detail.contains("1048576"));
    assert!(projection.next.is_some());
}

#[test]
fn memory_pressure_refusal_keeps_source_and_resource_action() {
    use provenant_records::diagnostic::{PressureLevel, PressureThreshold, SettingSource};
    let error = run_error(provenant_run::RunError::Admission(
        provenant_run::AdmissionError::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: SettingSource::Configuration,
        },
    ));
    let projection = error.diagnostic();
    assert_eq!(projection.code, "confinement_refused");
    assert!(projection.detail.unwrap().contains("host configuration"));
    assert!(projection.next.unwrap().contains("memory"));

    let fail = provenant_cli::run::Fail::from(provenant_run::RunError::Admission(
        provenant_run::AdmissionError::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: SettingSource::Configuration,
        },
    ));
    let error = dispatch_refused(fail);
    let wire = serde_json::to_vec(&error).unwrap();
    let decoded: FacadeError = serde_json::from_slice(&wire).unwrap();
    let projection = decoded.diagnostic();
    assert_eq!(projection.code, "dispatch_refused");
    assert!(projection.detail.unwrap().contains("host configuration"));
    assert!(projection.next.unwrap().contains("memory"));
}

#[test]
fn lane_cap_refusal_retains_the_setting_through_both_adapters() {
    use provenant_records::diagnostic::{LaneLimit, SettingSource};
    let failure = || provenant_run::RunError::Admission(provenant_run::AdmissionError::LaneCap {
        active: 3,
        limit: 3,
        kind: LaneLimit::ProjectWriters,
        origin: SettingSource::Configuration,
    });
    for error in [run_error(failure()), dispatch_refused(failure().into())] {
        let decoded: FacadeError = serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
        let projection = decoded.diagnostic();
        assert!(projection.detail.as_deref().unwrap().contains("max_project_writer_lanes"));
        assert!(projection.detail.as_deref().unwrap().contains("host configuration"));
        assert!(projection.next.as_deref().unwrap().contains("max_project_writer_lanes"));
    }
}

#[test]
fn process_limit_retains_io_code_and_safe_resource_facts() {
    let failure = || provenant_run::RunError::Admission(provenant_run::AdmissionError::Processes {
        used: 950,
        limit: 1000,
    });
    for (error, code) in [
        (run_error(failure()), "io"),
        (dispatch_refused(failure().into()), "dispatch_refused"),
    ] {
        let decoded: FacadeError = serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
        let projection = decoded.diagnostic();
        assert_eq!(projection.code, code);
        let detail = projection.detail.unwrap();
        assert!(detail.contains("RLIMIT_NPROC"));
        assert!(detail.contains("950"));
        assert!(detail.contains("1000"));
        assert!(detail.contains("100"));
        assert!(projection.next.unwrap().contains("process"));
    }
}

#[test]
fn tick_wakes_graph_without_deadline_but_respects_future_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "waiter", "plan_digest": "plan", "nodes": [
                {"id":"branch", "worktree":null, "state":"running", "attempts":[{
                    "key":{"graph_id":"waiter", "node_id":"branch", "route_index":0},
                    "route":{"adapter":"fixture", "model":"fixture/model"},
                    "state":"running", "launched":true, "launch_actor":"actor"
                }]}
            ]
        }))?;
    let now = provenant_core::WallTime::from_micros(1_000_000)?;
    assert!(graph_wake_due(&checkpoint, now));
    checkpoint.nodes[0].state = provenant_fabric_runtime::graph::NodeState::Pending;
    checkpoint.wake_at = Some(provenant_core::WallTime::from_micros(2_000_000)?);
    assert!(!graph_wake_due(&checkpoint, now));
    assert!(graph_wake_due(&checkpoint, checkpoint.wake_at.unwrap()));
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes these helpers without libtest.
pub(crate) mod peer_tests {
    use super::*;
    #[allow(unused_imports)]
    // The worktree driver includes this module without libtest functions.
    use provenant_confinement::worktrees;
    use provenant_core::{AttemptId, RunId, TaskId};
    use provenant_run::{AttemptState, Outcome, RunStore};

    #[test]
    fn graph_factory_reopens_the_retained_directory() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_factory_reopens_the_retained_directory")? {
            return Ok(());
        }
        let rig = crate::worktree_tests::support::TestRig::new().map_err(|e| format!("fixture: {e}"))?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone()).map_err(|e| format!("roots: {e}"))?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project).map_err(|e| format!("context: {e}"))?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let retained = rig.project.join("retained-directory");
        std::fs::create_dir(&retained)?;
        let rebound = factory
            .for_graph_directory(&retained)
            .map_err(|error| format!("rebind retained directory: {error}"))?;
        assert_eq!(rebound.cwd, retained);
        assert_eq!(rebound.context.identity.project, factory.context.identity.project);
        let foreign = crate::worktree_tests::support::TestRig::new()?;
        assert!(factory.for_graph_directory(&foreign.project).is_err());
        Ok(())
    }

    #[test]
    fn tick_reports_unreadable_once_and_advances_another_graph()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::tick_reports_unreadable_once_and_advances_another_graph")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph;
        struct Seed {
            actor: String,
            at: provenant_core::WallTime,
        }
        impl graph::AttemptOwner for Seed {
            type Error = FacadeError;
            fn actor(&self) -> String {
                self.actor.clone()
            }
            fn wall_now(&self) -> Option<provenant_core::WallTime> {
                Some(self.at)
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                Err(FacadeError::OwnerUnavailable)
            }
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
        let project = context.identity.project;
        let clock = SystemClock;
        let now = clock.wall_now()?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 10)?;
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"b-good", "decision_session":"chair-session",
            "nodes":[{"id":"choose", "kind":"decide", "access":"read_only", "decide":{
                "question":"Ship?", "options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
                "default":"hold", "timeout_seconds":60
            }}]
        }))?;
        let mut journal =
            graph::FileJournal::open(&roots.state, project, "b-good", budget, &cancel, &clock)?;
        journal.bind(graph::GraphExecution {review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: rig.project.clone(),
            document: document.clone(),
        })?;
        let mut seed = Seed {
            actor: context.identity.inbox.actor.to_string(),
            at: provenant_core::WallTime::from_micros(now.as_micros() - 120_000_000)?,
        };
        let seeded = graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        assert_eq!(seeded.checkpoint.nodes[0].state, graph::NodeState::Running);
        drop(journal);
        let directory = roots
            .state
            .as_path()
            .join("fabric/graphs")
            .join(project.to_string());
        std::fs::write(directory.join("graph-a-bad.json"), b"invalid json")?;
        let resolved = crate::roots::Resolved {
            assets: rig.project.clone(),
            home: rig.home.clone(),
            project: rig.project.clone(),
            roots: roots.clone(),
            claude_config: None,
            codex_home: None,
        };
        let first = tick_graphs(&resolved, now, &FreeWorkers)?;
        assert_eq!(first.errors.len(), 1, "{:?}", first.errors);
        assert!(first.errors[0].contains("a-bad"));
        assert_eq!(first.advanced, 1);
        let good = graph::FileJournal::read_checkpoint(&roots.state, project, "b-good")?
            .ok_or("missing good graph")?;
        assert_eq!(good.nodes[0].state, graph::NodeState::Succeeded);
        let second = tick_graphs(&resolved, now, &FreeWorkers)?;
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(second.advanced, 0);
        Ok(())
    }

    #[test]
    fn tick_dispatches_waiting_node_without_a_deadline() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::tick_dispatches_waiting_node_without_a_deadline")? { return Ok(()); }
        use provenant_fabric_runtime::graph;
        struct Seed(String);
        impl graph::AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String { self.0.clone() }
            fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute,
                _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending { attempt_ref: None })
            }
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
        let project = context.identity.project;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"waiter", "template_library":{},"nodes":[{"id":"one","prompt":"reply OK",
            "access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?;
        let mut journal = graph::FileJournal::open(&roots.state, project, "waiter", budget, &cancel, &clock)?;
        journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
            directory: rig.project.clone(), document: document.clone() })?;
        let seeded = graph::advance_with_budget(&document, &mut Seed(context.identity.inbox.actor.to_string()),
            &mut journal, &Default::default(), &Default::default())?;
        assert!(seeded.checkpoint.wake_at.is_none());
        let key = seeded.checkpoint.nodes[0].attempts[0].key.clone();
        drop(journal);
        let resolved = crate::roots::Resolved { assets: rig.project.clone(), home: rig.home.clone(), project: rig.project.clone(),
            roots: roots.clone(), claude_config: None, codex_home: None };
        let tick = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
        assert_eq!(tick.advanced, 1, "{:?}", tick.errors);
        let factory = Factory { context, project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(), assets: rig.project.clone(),
            host_launch: None, mcp_project: None, workers: &FreeWorkers };
        let owner = BinaryRunOwner { factory: &factory, allow_remote: true, actor: None, chair: None };
        let operation = graph_operation("graph-attempt-operation", &key)?;
        assert!(owner.store(&cancel)?.operation(&operation, &owner.call(&cancel))?.is_some());
        Ok(())
    }

    #[test]
    fn tick_dispatches_capacity_waiter_with_future_decision() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::tick_dispatches_capacity_waiter_with_future_decision")? { return Ok(()); }
        use provenant_fabric_runtime::graph;
        struct Seed(String);
        impl graph::AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String { self.0.clone() }
            fn wall_now(&self) -> Option<provenant_core::WallTime> { SystemClock.wall_now().ok() }
            fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute,
                _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending { attempt_ref: None })
            }
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
        let project = context.identity.project;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"waiter", "template_library":{},"decision_session":"chair-session","nodes":[
            {"id":"choose","kind":"decide","access":"read_only","decide":{"question":"Ship?",
            "options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],"default":"hold","timeout_seconds":3600}},
            {"id":"one","prompt":"reply OK","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?;
        let mut journal = graph::FileJournal::open(&roots.state, project, "waiter", budget, &cancel, &clock)?;
        journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
            directory: rig.project.clone(), document: document.clone() })?;
        let seeded = graph::advance_with_budget(&document, &mut Seed(context.identity.inbox.actor.to_string()),
            &mut journal, &Default::default(), &Default::default())?;
        assert!(seeded.checkpoint.wake_at.unwrap() > clock.wall_now()?);
        let key = seeded.checkpoint.nodes[1].attempts[0].key.clone();
        drop(journal);
        let resolved = crate::roots::Resolved { assets: rig.project.clone(), home: rig.home.clone(), project: rig.project.clone(),
            roots: roots.clone(), claude_config: None, codex_home: None };
        let tick = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
        assert_eq!(tick.advanced, 1, "{:?}", tick.errors);
        let factory = Factory { context, project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(), assets: rig.project.clone(),
            host_launch: None, mcp_project: None, workers: &FreeWorkers };
        let owner = BinaryRunOwner { factory: &factory, allow_remote: true, actor: None, chair: None };
        let operation = graph_operation("graph-attempt-operation", &key)?;
        assert!(owner.store(&cancel)?.operation(&operation, &owner.call(&cancel))?.is_some());
        Ok(())
    }

    #[test]
    fn tick_warns_once_for_missing_worktree_and_advances_another_graph()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::tick_warns_once_for_missing_worktree_and_advances_another_graph")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph;
        struct Seed { actor: String, at: provenant_core::WallTime }
        impl graph::AttemptOwner for Seed {
            type Error = FacadeError;
            fn actor(&self) -> String { self.actor.clone() }
            fn wall_now(&self) -> Option<provenant_core::WallTime> { Some(self.at) }
            fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>,
                _: &graph::GraphRoute, _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>)
                -> Result<graph::AttemptObservation, Self::Error> { Err(FacadeError::OwnerUnavailable) }
        }
        let rig = crate::worktree_tests::support::TestRig::new().map_err(|e| format!("rig: {e}"))?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone()).map_err(|e| format!("roots: {e}"))?;
        let context = Context::open(roots.clone(), &rig.home, &rig.project).map_err(|e| format!("context: {e}"))?;
        let project = context.identity.project;
        let clock = SystemClock;
        let now = clock.wall_now()?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 10)?;
        let mut seed = Seed { actor: context.identity.inbox.actor.to_string(), at: provenant_core::WallTime::from_micros(now.as_micros() - 120_000_000)? };
        for id in ["a-missing", "b-present"] {
            if id == "a-missing" { seed.at = now; }
            else { seed.at = provenant_core::WallTime::from_micros(now.as_micros() - 120_000_000)?; }
            let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version":1, "graph_id":id, "decision_session":"chair-session",
                "nodes":[{"id":"choose", "kind":"decide", "access":"read_only", "decide":{
                    "question":"Ship?", "options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
                    "default":"hold", "timeout_seconds":60
                }}]
            }))?;
            let mut journal = graph::FileJournal::open(&roots.state, project, id, budget, &cancel, &clock)?;
            journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
                directory: if id == "a-missing" { rig.root.join("removed-worktree") } else { rig.project.clone() },
                document: document.clone() })?;
            graph::advance_with_budget(&document, &mut seed, &mut journal, &Default::default(), &Default::default()).map_err(|e| format!("seed: {e}"))?;
        }
        let resolved = crate::roots::Resolved { assets: rig.project.clone(), home: rig.home.clone(),
            project: rig.project.clone(), roots: roots.clone(), claude_config: None, codex_home: None };
        let first = tick_graphs(&resolved, now, &FreeWorkers)?;
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.advanced, 1);
        assert_eq!(first.warnings.len(), 1);
        assert!(first.warnings[0].contains("a-missing"));
        let second = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert!(second.warnings.is_empty());
        assert_eq!(graph::FileJournal::collect_terminal(&roots.state, project, i128::MAX, &cancel, &clock)?, 1);
        assert!(graph::FileJournal::read_checkpoint(&roots.state, project, "a-missing")?.is_some());
        let factory = Factory {
            context, project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        factory.graph(GraphInput::Cancel { extra: Default::default(), id: "a-missing".into(), reason: None }, budget, &cancel)?;
        assert_eq!(graph::FileJournal::collect_terminal(&roots.state, project, i128::MAX, &cancel, &clock)?, 1);
        assert!(graph::FileJournal::read_checkpoint(&roots.state, project, "a-missing")?.is_none());
        Ok(())
    }

    #[test]
    fn graph_wait_observes_settlement_and_budget() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_wait_observes_settlement_and_budget")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal, GraphStatus, NodeState};
        use provenant_core::{DurationMicros, MonoTime, TestClock, WallTime};
        struct Advance<'a> { clock: &'a TestClock, settle: std::cell::RefCell<Option<Box<dyn FnOnce() + 'a>>> }
        impl provenant_core::Pause for Advance<'_> {
            fn wait(&self, delay: DurationMicros, _: &CancelToken) {
                self.clock.wait_until(self.clock.mono_now().checked_add(delay).unwrap()).unwrap();
                if self.clock.mono_now().as_micros() >= 2_000_000 && let Some(settle) = self.settle.borrow_mut().take() { settle(); }
            }
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots.clone(), &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let project = factory.context.identity.project;
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"wait-test", "nodes":[{"id":"one", "prompt":"work", "access":"read_only", "route":{"alias":"senior"}}]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"wait-test", "plan_digest":"digest", "nodes":[{"id":"one", "state":"running", "worktree":null, "attempts":[]}]
        }))?;
        let mut journal = FileJournal::open(&roots.state, project, "wait-test", budget, &cancel, &factory.clock)?;
        journal.bind(GraphExecution { extra: Default::default(), integration_base: None, review_base: None, directory: rig.project.clone(), document })?;
        journal.save(&checkpoint)?;
        drop(journal);
        let clock = TestClock::new(WallTime::from_unix_seconds(0)?, MonoTime::from_micros(0));
        let observe = || factory.graph(GraphInput::Status { extra: Default::default(), id: "wait-test".into() }, budget, &cancel);
        let pause = Advance { clock: &clock, settle: std::cell::RefCell::new(None) };
        let pending = super::wait_graph(&clock, &pause, 1, &cancel, observe)?;
        assert_eq!(pending.status, GraphStatus::Pending);
        let waited = serde_json::to_value(super::graph_command_result("status wait", pending.clone()))?;
        let status = serde_json::to_value(super::graph_command_result("status", pending.clone()))?;
        assert_eq!(waited["status"], "waiting");
        assert_eq!(waited["data"]["checkpoint"]["graph_id"], "wait-test");
        assert_eq!(waited["data"], status["data"]);
        let human = super::graph_waiting_human(&pending, &provenant_cli::Globals::default());
        assert!(human.contains("status wait-test"));
        assert!(!human.contains("graph resume"));
        assert_eq!(clock.mono_now().as_micros(), 1_000_000);
        checkpoint.nodes[0].state = NodeState::Succeeded;
        let settle = || {
            let mut journal = FileJournal::open(&roots.state, project, "wait-test", budget, &cancel, &factory.clock).unwrap();
            journal.save(&checkpoint).unwrap();
        };
        let pause = Advance { clock: &clock, settle: std::cell::RefCell::new(Some(Box::new(settle))) };
        let settled = super::wait_graph(&clock, &pause, 1, &cancel, observe)?;
        assert_eq!(settled.status, GraphStatus::Complete);
        assert_eq!(settled.checkpoint.graph_id, "wait-test");
        assert_eq!(clock.mono_now().as_micros(), 2_000_000);
        drop(pause);
        checkpoint.nodes[0].state = NodeState::Running;
        checkpoint.pause_reason = Some("decision required".into());
        let mut journal = FileJournal::open(&roots.state, project, "wait-test", budget, &cancel, &factory.clock)?;
        journal.save(&checkpoint)?;
        drop(journal);
        let pause = Advance { clock: &clock, settle: std::cell::RefCell::new(None) };
        let paused = super::wait_graph(&clock, &pause, 1, &cancel, observe)?;
        assert_eq!(paused.status, GraphStatus::Paused);
        assert_eq!(clock.mono_now().as_micros(), 2_000_000);
        Ok(())
    }

    #[test]
    fn graph_cancel_persists_reason_and_skips_pending_node() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_cancel_persists_reason_and_skips_pending_node")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal, NodeState};
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots.clone(), &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let project = factory.context.identity.project;
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "reason-test", "nodes": [
                {"id":"one", "prompt":"work", "access":"read_only", "route":{"alias":"senior"}}
            ]
        }))?;
        let checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "reason-test", "plan_digest": "digest", "nodes": [
                {"id":"one", "state":"pending", "worktree":null, "attempts":[]}
            ]
        }))?;
        let mut journal = FileJournal::open(&roots.state, project, "reason-test", budget, &cancel, &factory.clock)?;
        journal.bind(GraphExecution { extra: Default::default(), integration_base: None, review_base: None, directory: rig.project.clone(), document })?;
        journal.save(&checkpoint)?;
        drop(journal);
        let cancelled = factory.graph(GraphInput::Cancel {
            extra: Default::default(), id: "reason-test".into(), reason: Some("test reason".into())
        }, budget, &cancel)?;
        assert_eq!(cancelled.checkpoint.cancel_reason.as_deref(), Some("test reason"));
        assert_eq!(cancelled.checkpoint.nodes[0].state, NodeState::Skipped);
        assert_eq!(cancelled.checkpoint.nodes[0].skip_reason.as_deref(), Some("graph cancelled"));
        let status = factory.graph(GraphInput::Status { extra: Default::default(), id: "reason-test".into() }, budget, &cancel)?;
        assert_eq!(status.checkpoint.cancel_reason.as_deref(), Some("test reason"));
        assert!(super::graph_human(&status).contains("Cancellation reason: test reason"));
        let repeated = factory.graph(GraphInput::Cancel { extra: Default::default(), id: "reason-test".into(), reason: None }, budget, &cancel)?;
        assert_eq!(repeated.checkpoint.cancel_reason.as_deref(), Some("test reason"));
        Ok(())
    }

    #[test]
    fn graph_status_and_cancel_retire_unreadable_record() -> Result<(), Box<dyn std::error::Error>>
    {
        if host_test("fabric::peer_tests::graph_status_and_cancel_retire_unreadable_record")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{FileJournal, GraphStatus};
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots.clone(), &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let project = factory.context.identity.project;
        for (id, contents) in [
            ("broken", b"invalid json".as_slice()),
            ("g-test", include_bytes!("fixtures/pre-node-asks-checkpoint.json").as_slice()),
        ] {
            drop(FileJournal::open(
                &roots.state,
                project,
                id,
                budget,
                &cancel,
                &factory.clock,
            )?);
            let directory = roots
                .state
                .as_path()
                .join("fabric/graphs")
                .join(project.to_string());
            let path = directory.join(format!("graph-{id}.json"));
            std::fs::write(&path, contents)?;
            let status = factory.graph(
                GraphInput::Status {
                    extra: Default::default(),
                    id: id.into(),
                },
                budget,
                &cancel,
            )?;
            assert_eq!(status.status, GraphStatus::Paused);
            let explanation = status.advance_error.unwrap();
            assert!(explanation.contains(id), "{explanation}");
            assert!(explanation.contains(&format!("provenant stop {id}")), "{explanation}");
            if id == "g-test" {
                assert!(explanation.contains("predates node asks"), "{explanation}");
                assert!(explanation.contains("previous release"), "{explanation}");
            }
            let retired = factory.graph(
                GraphInput::Cancel {
                    extra: Default::default(),
                    id: id.into(),
                    reason: None,
                },
                budget,
                &cancel,
            )?;
            assert!(
                retired
                    .advance_error
                    .unwrap()
                    .contains("unidentifiable work")
            );
            assert!(!path.exists());
            assert!(matches!(
                factory.graph(
                    GraphInput::Status {
                        extra: Default::default(),
                        id: id.into()
                    },
                    budget,
                    &cancel
                ),
                Err(FacadeError::NotFound)
            ));
        }
        Ok(())
    }

    fn graph_test_context(key: &provenant_fabric_runtime::graph::AttemptKey) -> provenant_records::run::WorkContext {
        provenant_records::run::WorkContext {
            graph_id: Some(key.graph_id.clone()),
            node_id: Some(key.node_id.clone()),
            ..Default::default()
        }
    }

    pub(super) fn host_test(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        if !provenant_cli::fabric::in_lane() {
            return Ok(false);
        }
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            name.into(),
            "--nocapture".into(),
        ];
        let removed = [
            provenant_core::RUN_STATE_DIRECTORY_ENV,
            provenant_run::LANE_DEPTH_ENV,
            provenant_run::PARENT_ATTEMPT_ENV,
        ];
        let fixture_home = tempfile::tempdir()?;
        let isolate_home = name
            .ends_with("explicit_writer_refreshes_tracking_ref_while_collection_is_busy")
            || name.ends_with("adopted_writer_refresh_failure_does_not_change_receipt");
        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !removed.iter().any(|name| key == name) && !(isolate_home && key == "HOME")
            })
            .collect();
        if isolate_home {
            environment.push(("HOME".into(), fixture_home.path().as_os_str().to_owned()));
        }
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, std::time::Duration::from_secs(30));
        options.env = Some(&environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        // Keep native-backend skip evidence visible through host isolation.
        use std::io::Write;
        std::io::stderr().lock().write_all(&output.stderr.bytes)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(true)
    }

    struct FreeWorkers;
    impl WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }

    mod loop_repair_tests {
        include!("loop_repair_tests.rs");
    }

    mod tick_tests {
        include!("tick_tests.rs");
    }

    struct SettlementFixture {
        rig: crate::worktree_tests::support::TestRig,
        roots: provenant_core::Roots,
        store: RunStore,
        settled: provenant_run::HolderContext,
        worktree: PathBuf,
    }

    fn publish_success(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        publish_outcome(rig, store, record, head, Outcome::Succeeded, call)
    }

    fn publish_outcome(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        outcome: Outcome,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        publish_routed(rig, store, record, head, outcome, "fixture/model", call)
    }

    fn publish_routed(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        outcome: Outcome,
        model: &str,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let attempt = rig
            .state
            .join("runs")
            .join(record.run_id.to_string())
            .join("attempts")
            .join(record.attempt_id.to_string());
        let directory = provenant_os::fs::Directory::open_anchor(&attempt)?;
        use provenant_supervisor::custody::{
            ExitObservation, PublicationState, Scope, SettlementEvidence,
        };
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope: Scope {
                run: record.run_id,
                attempt: record.attempt_id,
                generation: record.generation,
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
            settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
            stdout: Default::default(),
            stderr: Default::default(),
        };
        let mut exit_wire = serde_json::to_value(&exit)?;
        let object = exit_wire.as_object_mut().ok_or("exit object")?;
        object.remove("publication");
        object.insert("schema_version".into(), 1.into());
        for (name, wire) in [
            ("process-exit.json", exit_wire),
            (
                "writer-terminal.json",
                serde_json::json!({"schema_version":1,"head":head,"protected_paths":[]}),
            ),
        ] {
            directory
                .atomic_write(
                    std::ffi::OsStr::new(name),
                    ".fixture-",
                    &serde_json::to_vec(&wire)?,
                    provenant_os::fs::PublishMode::NoReplace,
                )?
                .require_durable()?;
        }
        drop(directory.lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?);
        let route = provenant_run::RouteRecord {
            provider: model.split_once('/').map(|(adapter, _)| adapter.into()),
            model: Some(model.into()),
            ..Default::default()
        };
        store.publish_terminal(
            record.attempt_id,
            record.revision,
            &provenant_run::TerminalPublication {
                terminal_cause: None,
                terminal_note: None,
                outcome,
                finished_at: call.clock.wall_now()?,
                started_at: None,
                provider_ms: None,
                reason: None,
                result: "OK",
                complete: true,
                redacted: false,
                resolved_route: Some(route.clone()),
                applied_route: Some(route.clone()),
                observed_route: Some(route),
                usage: Default::default(),
                session: None,
                partial_signals: Vec::new(),
            },
            &exit,
            call,
        )?;
        Ok(())
    }

    impl SettlementFixture {
        fn resumed() -> Result<Self, Box<dyn std::error::Error>> {
            Self::resumed_as("settlement-resume")
        }

        fn resumed_as(operation: &str) -> Result<Self, Box<dyn std::error::Error>> {
            Self::settled_fixture(operation, None)
        }

        fn settled_fixture(operation: &str, graph_id: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
            use provenant_confinement::worktrees::{
                self, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
            };
            let rig = crate::worktree_tests::support::TestRig::new()?;
            let git = provenant_os::git::git_executable().ok_or("git")?;
            for args in [
                vec!["config", "user.name", "Fixture"],
                vec!["config", "user.email", "fixture@example.invalid"],
                vec!["commit", "--allow-empty", "-qm", "base"],
            ] {
                let output = rig.execute(&git, &args, None)?;
                assert_eq!(crate::worktree_tests::support::code(&output), 0);
            }
            let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
            // A nested-lane fixture may only look up an existing binding.
            if std::env::var_os("PROVENANT_PANEL_ROUTING_FIXTURE").is_some() {
                provenant_config::bind_project(&roots, provenant_config::ProjectLocation::new(rig.project.join(".git"))?)?;
            }
            let identity = Context::open(roots.clone(), &rig.home, &rig.project)?.identity;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = provenant_run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let context = provenant_run::ExecutionContext {
                state_root: roots.state.clone(),
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                read_prompts: true,
            };
            let store =
                RunStore::open(&context, &call)?.with_writer_registry(roots.state.as_path());
            let info = worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    worktrees::create(
                        &worktrees::WorktreeContext {
                            workers: &FreeWorkers,
                            ..*context
                        },
                        WorktreeCreate {
                            branch: "resumed-lane".into(),
                            base: Some("HEAD".into()),
                        },
                        &cancel,
                    )
                },
            )?;
            let run = provenant_run::Service::new(&store).dispatch(
                provenant_run::DispatchRequest {
                    chair: None,
                    operation_id: if graph_id.is_some() { operation } else { "settlement-original" }.parse()?,
                    concurrency: 1,
                    lane_depth: 1,
                    parent_attempt: None,
                    tasks: vec![provenant_run::DispatchTask {
                    source_head: None,
                        cloud: None,
                        context: graph_id.map(|id| provenant_records::run::WorkContext {
                            graph_id: Some(id.to_owned()), ..Default::default()
                        }),
                        build_heavy: false,
                        title: None,
                        task_id: TaskId::from_bytes([3; 16]),
                        prompt: "reply OK".into(),
                        base: Some(info.head.to_string()),
                        pinned_base: None,
                        head: None,
                        profile: Default::default(),
                        route: provenant_run::RequestedRoute::Model {
                            extra: Default::default(),
                            provider: "fixture".into(),
                            model: "fixture/model".into(),
                            effort: None,
                        },
                        account: None,
                        timeout_seconds: 30,
                        delivery_id: None,
                        deliver: None,
                        worktree: Some(info.path.clone()),
                        access: Default::default(),
                    }],
                },
                &rig.project,
                &call,
            )?;
            let prior = store
                .pending_attempts(run.run_id, &call)?
                .first()
                .ok_or("attempt")?
                .attempt_id;
            worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    worktrees::save_owner(
                        &tree,
                        &WorktreeOwner {
                            attempt_id: Some(prior),
                            operation_id: Some(run.operation_id.clone()),
                            branch: "resumed-lane".into(),
                            base: info.head.to_string(),
                            host: identity.host,
                            created_at: clock
                                .wall_now()
                                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                            origin: WorktreeOrigin::Lane,
                            label: None,
                        },
                    )?;
                    Ok(())
                },
            )?;
            if graph_id.is_some() {
                let record = store.record(prior, &call)?;
                publish_success(&rig, &store, &record, info.head, &call)?;
                let settled = provenant_run::HolderContext {
                    schema_version: 1,
                    roots: provenant_run::RootSeed { home: rig.home.clone(), checkout: rig.project.clone(), state_root: rig.state.clone() },
                    project_id: identity.project, actor_id: identity.inbox.actor,
                    run_id: record.run_id, attempt_id: record.attempt_id,
                    host_delivery: None, launch: None, sealed: None,
                };
                return Ok(Self { rig, roots, store, settled, worktree: info.path });
            }
            store.withdraw(
                prior,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
            let resume = provenant_run::Resume {
                prior,
                expected_generation: 0,
                operation_id: operation.parse()?,
                run_id: RunId::from_bytes([30; 16]),
                attempt_id: AttemptId::from_bytes([31; 16]),
                prompt: "continue".into(),
                created_at: clock.wall_now()?,
            };
            store.resume_fresh(&resume, "fixture", &call)?;
            let record = store.record(resume.attempt_id, &call)?;
            publish_success(&rig, &store, &record, info.head, &call)?;
            let settled = provenant_run::HolderContext {
                schema_version: 1,
                roots: provenant_run::RootSeed {
                    home: rig.home.clone(),
                    checkout: rig.project.clone(),
                    state_root: rig.state.clone(),
                },
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                run_id: record.run_id,
                attempt_id: record.attempt_id,
                host_delivery: None,
                launch: None,
                sealed: None,
            };
            Ok(Self {
                rig,
                roots,
                store,
                settled,
                worktree: info.path,
            })
        }
    }

    #[test]
    fn accepted_attempt_observes_stable_host_after_alias_is_renamed()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
        let host = context.identity.host;
        let config = |alias: &str| format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric.peers.{alias}]\nhost='{host}'\ndestination='fixture'\n"
        );
        std::fs::write(rig.state.join("config.toml"), config("old_alias"))?;
        let factory = Factory {
            context: Context::open(roots.clone(), &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        assert_eq!(factory.context.peer_host("old_alias"), Some(host));
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory,
            integration_base: None, integration_branch: None, network: None,
            capability_overrides: Default::default(), exclude: Vec::new(), budget,
            cancel: &cancel, prices: None, tick: false };
        let actor = owner.actor();
        let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"renamed-alias", "node_id":"inspect", "route_index":0
        }))?;
        let spec = graph::AttemptSpec { host: Some("old_alias".into()), ..Default::default() };
        let route = serde_json::from_value(serde_json::json!({"adapter":"fixture","model":"fixture/model"}))?;
        let node = || graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: Some(&spec),
            require_skill_location: false, settlement_retries: None, prior_attempts: &[], family_peers: &[],
            context: graph_test_context(&key), read_roots: &[], distinct_from_attempts: &[], require_distinct: false,
            source_commit: Some(&head), review_base: None, title: "inspect", prompt: "Inspect",
            required_verdict: None, deadline_seconds: None, inputs_dir: None,
        };
        let first = provenant_cli::run::with_deferred_provider_launch(|| owner.observe_or_start(
            &key, node(), &route, graph::AccessMode::ReadOnly, None,
            graph::Replay { delivery: None, branch: None, fast_forward_only: false,
                stored_ref: None, launch_actor: &actor }))?;
        let reference = match first {
            graph::AttemptObservation::Pending { attempt_ref }
            | graph::AttemptObservation::Unknown { attempt_ref }
            | graph::AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref.ok_or("missing reference")?,
        };
        std::fs::write(rig.state.join("config.toml"), config("new_alias"))?;
        let renamed = factory.for_graph_directory(&rig.project)?;
        assert_eq!(renamed.context.peer_host("old_alias"), None);
        assert_eq!(renamed.context.peer_host("new_alias"), Some(host));
        let mut owner = BoundGraphOwner { factory: &renamed, ..owner };
        let replayed = owner.observe_or_start(&key, node(), &route, graph::AccessMode::ReadOnly, None,
            graph::Replay { delivery: None, branch: None, fast_forward_only: false,
                stored_ref: Some(&reference), launch_actor: &actor })?;
        let replayed = match replayed {
            graph::AttemptObservation::Pending { attempt_ref }
            | graph::AttemptObservation::Unknown { attempt_ref }
            | graph::AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref,
        };
        assert_eq!(replayed.as_deref(), Some(reference.as_str()));
        assert!(matches!(owner.observe_or_start(&key, node(), &route, graph::AccessMode::ReadOnly, None,
            graph::Replay { delivery: None, branch: None, fast_forward_only: false,
                stored_ref: None, launch_actor: &actor }), Err(FacadeError::UnknownHost)));
        Ok(())
    }

    #[test]
    fn graph_cloud_owner_submits_once_and_replays_remote_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        // Isolate PATH and lane markers in a child instead of changing the test process.
        if std::env::var_os("GRAPH_CLOUD_FIXTURE").is_none() {
            use std::os::unix::fs::PermissionsExt;
            use provenant_os::exec::{ExecOptions, Executor, Termination};
            let scratch = tempfile::tempdir()?;
            let script = scratch.path().join("codex");
            std::fs::write(&script, r#"#!/bin/sh
case "$2" in
exec) cat >/dev/null; printf x >> "$GRAPH_CLOUD_FIXTURE/submissions"; printf '{"id":"graph-remote"}';;
status) if test -f "$GRAPH_CLOUD_FIXTURE/result.diff"; then printf completed; else printf running; fi;;
diff) cat "$GRAPH_CLOUD_FIXTURE/result.diff";;
*) exit 7;;
esac
"#)?;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
            let mut paths = vec![scratch.path().to_path_buf()];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
            let mut environment: Vec<_> = std::env::vars_os().filter(|(key, _)| {
                !["PATH", provenant_core::RUN_STATE_DIRECTORY_ENV,
                    provenant_run::LANE_DEPTH_ENV, provenant_run::PARENT_ATTEMPT_ENV]
                    .iter().any(|name| key == name)
            }).collect();
            environment.push(("PATH".into(), std::env::join_paths(paths)?));
            environment.push(("GRAPH_CLOUD_FIXTURE".into(), scratch.path().as_os_str().to_owned()));
            let args = vec![std::env::current_exe()?.into_os_string(), "--exact".into(),
                "fabric::peer_tests::graph_cloud_owner_submits_once_and_replays_remote_completion".into(),
                "--nocapture".into()];
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&args, &cwd, std::time::Duration::from_secs(30));
            options.env = Some(&environment);
            let output = SystemExecutor::new(1)?.execute(options)?;
            assert_eq!(output.termination, Termination::Exit(0), "{}\n{}",
                String::from_utf8_lossy(&output.stdout.bytes), String::from_utf8_lossy(&output.stderr.bytes));
            return Ok(());
        }
        let rig = crate::worktree_tests::repository()?;
        let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut graph_owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None,
            integration_branch: None, network: None, capability_overrides: Default::default(),
            exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false };
        let actor = graph_owner.actor();
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"cloud-owner", "node_id":"verify", "route_index":0
        }))?;
        let route = serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
        let spec: graph::AttemptSpec = serde_json::from_value(serde_json::json!({"placement":{
            "target":"cloud-test", "provider":"codex", "environment":"env", "repository":"repo",
            "branch":"main", "model":"requested", "deadline_seconds":3600
        }}))?;
        let node = || graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: Some(&spec),
            require_skill_location: false, settlement_retries: None, prior_attempts: &[], family_peers: &[],
            context: graph_test_context(&key), read_roots: &[], distinct_from_attempts: &[], require_distinct: false,
            source_commit: Some(&head), review_base: None, title: "verify", prompt: "Run targeted tests",
            required_verdict: None, deadline_seconds: None, inputs_dir: None,
        };
        let refused = graph_owner.observe_or_start(&key, node(), &route, graph::AccessMode::WorktreeWrite,
            None, graph::Replay { delivery: None, branch: None, fast_forward_only: false,
                stored_ref: None, launch_actor: &actor });
        assert!(matches!(refused, Err(FacadeError::InvalidInput { .. })));
        let refused_owner = BinaryRunOwner { factory: &factory, allow_remote: false, actor: None, chair: None };
        assert!(refused_owner.store(&cancel)?.operation(
            &graph_operation("graph-attempt-operation", &key)?, &refused_owner.call(&cancel),
        )?.is_none());
        let first = graph_owner.observe_or_start(&key, node(), &route, graph::AccessMode::ReadOnly,
            None, graph::Replay { delivery: None, branch: None, fast_forward_only: false,
                stored_ref: None, launch_actor: &actor })?;
        let graph::AttemptObservation::Pending { attempt_ref: Some(reference) } = first else {
            return Err(format!("expected submitted cloud node, got {first:?}").into());
        };
        let owner = BinaryRunOwner { factory: &factory, allow_remote: false, actor: None, chair: None };
        let store = owner.store(&cancel)?;
        let call = owner.call(&cancel);
        let id = reference.split_once('/').ok_or("attempt reference")?.1.parse()?;
        let job = provenant_cli::run::cloud_owner::load(&store, id, &call)?.ok_or("missing cloud identity")?;
        assert_eq!(job.remote_id, "graph-remote");
        let result = serde_json::json!({"job_id":job.job_id, "nonce":job.nonce, "commit":head,
            "exit_status":0, "tests_run":["targeted"], "tests_failed":[], "log_location":"log",
            "tracked_files_changed":false, "observed_model":null});
        let scratch = std::path::PathBuf::from(std::env::var_os("GRAPH_CLOUD_FIXTURE").ok_or("fixture")?);
        std::fs::write(scratch.join("result.diff"), format!(
            "diff --git a/.provenant-cloud-result.json b/.provenant-cloud-result.json\nnew file mode 100644\n--- /dev/null\n+++ b/.provenant-cloud-result.json\n@@ -0,0 +1 @@\n+{result}\n"))?;
        let terminal = graph_owner.observe_or_start(&key, node(), &route, graph::AccessMode::ReadOnly,
            None, graph::Replay { delivery: None, branch: None, fast_forward_only: false,
                stored_ref: Some(&reference), launch_actor: &actor })?;
        assert!(matches!(terminal, graph::AttemptObservation::Terminal {
            outcome: graph::AttemptOutcome::Succeeded, attempt_ref: Some(ref found), ..
        } if found == &reference));
        assert_eq!(std::fs::read_to_string(scratch.join("submissions"))?, "x");
        let downstream: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"cloud-owner", "node_id":"summarise", "route_index":0
        }))?;
        let inputs = graph_owner.materialise_inputs(&downstream, &[graph::InputRequest {
            label: "cloud verification".into(), attempt_ref: Some(reference),
            inline_findings: None, finding_prefix: None,
        }], 16_384).ok_or("missing materialised inputs")?;
        let [graph::MaterialisedInput::File { path, omitted: 0, .. }] = inputs.entries.as_slice() else {
            return Err(format!("expected readable cloud result, got {inputs:?}").into());
        };
        let content: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        assert_eq!(content["state"]["result"]["tests_run"], serde_json::json!(["targeted"]));
        assert_eq!(content["state"]["result"]["exit_status"], 0);
        assert_eq!(content["passed"], true);
        Ok(())
    }

    #[test]
    fn graph_peer_without_receipt_keeps_launch_and_cancellation_uncertain()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_peer_without_receipt_keeps_launch_and_cancellation_uncertain")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
            factory: &factory, integration_base: None, integration_branch: None,
            network: None, capability_overrides: Default::default(), exclude: Vec::new(),
            budget, cancel: &cancel, prices: None, tick: false,
        };
        let mut attempt: graph::GraphAttempt = serde_json::from_value(serde_json::json!({
            "launched":false,
            "key":{"graph_id":"peer-without-receipt", "node_id":"inspect", "route_index":0},
            "route":{"alias":"junior"}, "state":"starting", "attempt_ref":null,
            "outcome":null, "launch_actor":owner.actor()
        }))?;
        // Even a removed peer cannot turn missing local state into proof that
        // the remote operation was refused or has stopped.
        attempt.extra.insert("placement_host".into(), serde_json::json!("removed-peer"));
        assert!(!owner.launch_accepted(&attempt)?);
        assert!(!owner.launch_refused_before_acceptance(&attempt)?);
        assert!(!owner.cancelled_attempt_settled(&attempt)?);
        for placement in [
            None,
            Some("local".to_owned()),
            Some(factory.context.identity.host.to_string()),
        ] {
            attempt.extra.remove("placement_host");
            if let Some(placement) = placement {
                attempt.extra.insert("placement_host".into(), serde_json::json!(placement));
            }
            assert!(!owner.launch_accepted(&attempt)?);
            assert!(owner.launch_refused_before_acceptance(&attempt)?);
            assert!(owner.cancelled_attempt_settled(&attempt)?);
        }
        attempt.attempt_ref = Some(format!("{}/{}",
            provenant_core::HostId::from_bytes([7;16]),
            provenant_core::AttemptId::from_bytes([8;16])));
        assert!(owner.launch_accepted(&attempt)?);
        assert!(!owner.launch_refused_before_acceptance(&attempt)?);
        Ok(())
    }

    #[test]
    fn graph_run_controls_reach_the_sealed_attempt() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_run_controls_reach_the_sealed_attempt")? {
            return Ok(());
        }
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let input = serde_json::json!({
            "op":"run", "brief":"Inspect the parser", "model":"fixture/model",
            "effort":"high", "account":"fixture-account", "host":"local",
            "operation_id":"graph-run-controls",
            "set":{
                "attempt.build_heavy":true,
                "attempt.confidential":true,
                "attempt.provider_timeout":42
            }
        });
        let preview = factory.graph_document({
            let mut preview = input.clone();
            preview["explain"] = serde_json::json!(true);
            preview
        }, budget, &cancel)?;
        assert_eq!(preview["document"]["graph_id"], "g-e813e34cbb5c0bf4b1ee8313250533");
        let node = &preview["document"]["nodes"][0];
        assert_eq!(node["attempt"]["host"], "local");
        assert_eq!(node["attempt"]["build_heavy"], true);
        assert_eq!(node["attempt"]["confidential"], true);
        assert_eq!(node["attempt"]["provider_timeout"], 42);
        let launched = factory.graph_document(input.clone(), budget, &cancel)?;
        let key = serde_json::from_value(launched["checkpoint"]["nodes"][0]["attempts"][0]["key"].clone())?;
        let owner = BinaryRunOwner { factory: &factory, allow_remote: true, actor: None, chair: None };
        let store = owner.store(&cancel)?;
        let call = owner.call(&cancel);
        let operation = store.operation(&graph_operation("graph-attempt-operation", &key)?, &call)?
            .ok_or("launch operation missing")?;
        let request = store.request(operation.run_id, &call)?;
        let task = request.tasks.first().ok_or("sealed task missing")?;
        assert_eq!(task.route, serde_json::from_value(serde_json::json!({
            "kind":"model", "provider":"fixture", "model":"fixture/model", "effort":"high"
        }))?);
        assert_eq!(task.account.as_deref(), Some("fixture-account"));
        assert!(task.build_heavy);
        assert_eq!(task.provider_timeout_seconds, 42);
        assert_eq!(task.profile.extra.get("confidential"), Some(&serde_json::json!(true)));
        let replay = factory.graph_document(input, budget, &cancel)?;
        let replay_key = serde_json::from_value(replay["checkpoint"]["nodes"][0]["attempts"][0]["key"].clone())?;
        let replay_operation = store.operation(&graph_operation("graph-attempt-operation", &replay_key)?, &call)?
            .ok_or("replayed operation missing")?;
        assert_eq!(replay_operation.run_id, operation.run_id);
        Ok(())
    }

    #[test]
    fn graph_review_launch_submits_pinned_source_and_base() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_review_launch_submits_pinned_source_and_base")? { return Ok(()); }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
        assert!(git(&["for-each-ref", "--format=%(refname)", "refs/remotes/origin/HEAD"])?.is_empty());
        let base = git(&["rev-parse", "HEAD"])?;
        std::fs::write(rig.project.join("lane-change"), "change")?;
        git(&["add", "lane-change"])?;
        git(&["commit", "-qm", "lane"])?;
        let head = git(&["rev-parse", "HEAD"])?;
        git(&["checkout", "-q", "--detach", &base])?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut graph_owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None,
            integration_branch: None, network: None, capability_overrides: Default::default(), exclude: Vec::new(), budget, cancel: &cancel,
            prices: None, tick: false };
        let actor = graph_owner.actor();
        let route = serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
        let owner = BinaryRunOwner { factory: &factory, allow_remote: true, actor: None, chair: None };
        let store = owner.store(&cancel)?;
        let call = owner.call(&cancel);
        for (name, review_base, expected_base) in [
            ("lens", Some(base.as_str()), &base),
            ("evaluate", Some(base.as_str()), &base),
            ("equal", Some(head.as_str()), &head),
            ("default-no-origin-head", None, &head),
        ] {
            let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({"graph_id":"review-pins", "node_id":name, "route_index":0}))?;
            let observation = graph_owner.observe_or_start(&key, graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: None, require_skill_location: false, settlement_retries: Some(1), prior_attempts: &[], family_peers: &[],
                context: graph_test_context(&key), read_roots: &[], distinct_from_attempts: &[], require_distinct: false,
                source_commit: Some(&head), review_base, title: name, prompt: "review",
                required_verdict: None, deadline_seconds: None, inputs_dir: None,
            }, &route, graph::AccessMode::ReadOnly, None, graph::Replay {
                delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
            })?;
            let operation = store.operation(&graph_operation("graph-attempt-operation", &key)?, &call)?.ok_or("missing operation")?;
            let request = store.request(operation.run_id, &call)?;
            assert_eq!(request.tasks[0].profile.settlement_retry_limit, Some(1));
            assert_eq!(request.tasks[0].base.as_deref(), Some(expected_base.as_str()));
            assert_eq!(request.tasks[0].head.as_deref(), Some(head.as_str()));
            let reference = match observation {
                graph::AttemptObservation::Pending { attempt_ref } | graph::AttemptObservation::Unknown { attempt_ref } | graph::AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref.ok_or("missing attempt reference")?,
            };
            let range = graph_owner.review_range(&reference, review_base.is_some())?;
            if review_base.is_none() {
                assert_eq!(range, graph::ReviewRange::NoBase);
            } else {
                assert_eq!(range, graph::ReviewRange::Resolved { base: expected_base.clone(), head: head.clone() });
            }
        }
        Ok(())
    }

    #[test]
    fn child_launch_refuses_credential_and_state_roots_at_the_host()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::child_launch_refuses_credential_and_state_roots_at_the_host")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let credential = rig.home.join(".ssh");
        std::fs::create_dir_all(&credential)?;
        std::fs::create_dir_all(&rig.state)?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None,
            integration_branch: None, network: None, capability_overrides: Default::default(),
            exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false };
        let actor = owner.actor();
        let route = serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
        let run_owner = BinaryRunOwner { factory: &factory, allow_remote: false, actor: None, chair: None };
        let call = run_owner.call(&cancel);
        let store = run_owner.store(&cancel)?;
        for (name, root) in [("credential", credential), ("state", rig.state.clone())] {
            // The lane forwards the request; only the host decides access at launch.
            let child = factory.child_request(&serde_json::json!({
                "op":"run", "document":{"use":"task", "brief":"Read requested root", "read_roots":[root]}
            }))?;
            assert_eq!(child.launch.ok_or("missing child launch")?.node["read_roots"], serde_json::json!([root]));
            for write in [false, true] {
                let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
                    "graph_id":"child-roots", "node_id":format!("parent--{name}-{write}"), "route_index":0
                }))?;
                let roots = vec![root.to_string_lossy().into_owned()];
                let attempt = graph::AttemptSpec { write_roots: if write { roots.clone() } else { Vec::new() }, ..Default::default() };
                let result = owner.observe_or_start(&key, graph::NodeInput {
                    attachments: &[], capabilities: None, capability_sources: None, attempt: Some(&attempt),
                    require_skill_location: false, settlement_retries: None, prior_attempts: &[], family_peers: &[],
                    context: graph_test_context(&key), read_roots: if write { &[] } else { &roots },
                    distinct_from_attempts: &[], require_distinct: false, source_commit: None, review_base: None,
                    title: "child", prompt: "Read requested root", required_verdict: None,
                    deadline_seconds: None, inputs_dir: None,
                }, &route, graph::AccessMode::ReadOnly, None, graph::Replay {
                    delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
                });
                assert!(matches!(result, Err(FacadeError::DispatchRefused { .. })), "{name}, write={write}: {result:?}");
                assert!(store.operation(&graph_operation("graph-attempt-operation", &key)?, &call)?.is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn pinned_review_range_survives_moved_ref_and_another_checkout() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::pinned_review_range_survives_moved_ref_and_another_checkout")? { return Ok(()); }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
        let base = git(&["rev-parse", "HEAD"])?;
        git(&["branch", "review-pin", &base])?;
        std::fs::write(rig.project.join("source-change"), "source")?;
        git(&["add", "source-change"])?;
        git(&["commit", "-qm", "original source"])?;
        let original = git(&["rev-parse", "HEAD"])?;
        std::fs::write(rig.project.join("predecessor-change"), "predecessor")?;
        git(&["add", "predecessor-change"])?;
        git(&["commit", "-qm", "predecessor"])?;
        let head = git(&["rev-parse", "HEAD"])?;
        assert_ne!(base, original);
        assert_ne!(original, head);
        let other = rig.root.join("other-checkout");
        git(&["worktree", "add", "--detach", other.to_str().ok_or("other path")?, &base])?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?, project_attribution: false,
            clock: SystemClock, backoff: ThreadPause, executor: SystemExecutor::new(1)?,
            home: rig.home.clone(), cwd: rig.project.clone(), assets: rig.project.clone(),
            host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None,
            capability_overrides: (Vec::new(), Vec::new()),
            integration_branch: None, network: None, exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false };
        let pinned = owner.resolve_review_base("review-pin", Some(rig.project.to_str().ok_or("project")?))?.ok_or("base")?;
        assert_eq!(pinned, base);
        let actor = owner.actor();
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({"graph_id":"pinned-range","node_id":"review","route_index":0}))?;
        let route = serde_json::from_value(serde_json::json!({"adapter":"fixture","model":"fixture/model"}))?;
        let source_roots = vec![rig.project.to_str().ok_or("source path")?.to_owned()];
        let write_root = rig.root.join("extra-write");
        std::fs::create_dir(&write_root)?;
        let attempt_spec = graph::AttemptSpec { build_heavy: true, write_roots: vec![write_root.to_string_lossy().into_owned()], ..Default::default() };
        let node = || graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: Some(&attempt_spec), require_skill_location: false, settlement_retries: None, prior_attempts: &[], family_peers: &[], context: graph_test_context(&key),
            read_roots: &source_roots, distinct_from_attempts: &[], require_distinct: false, source_commit: Some(&head),
            review_base: Some(&pinned), title: "review", prompt: "review", required_verdict: None, deadline_seconds: None, inputs_dir: None };
        let first = owner.observe_or_start(&key, node(), &route, graph::AccessMode::ReadOnly, Some(rig.project.to_str().ok_or("source path")?),
            graph::Replay { delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor }).map_err(|error| format!("first launch: {error:?}"))?;
        let reference = match first { graph::AttemptObservation::Pending { attempt_ref } | graph::AttemptObservation::Unknown { attempt_ref } | graph::AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref.ok_or("attempt reference")? };
        git(&["branch", "-f", "review-pin", &head])?;
        git(&["commit", "--allow-empty", "-qm", "move source checkout"])?;
        assert_ne!(git(&["rev-parse", "HEAD"])?, head);
        let reattached = owner.observe_or_start(&key, node(), &route, graph::AccessMode::ReadOnly, Some(rig.project.to_str().ok_or("source path")?),
            graph::Replay { delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor }).map_err(|error| format!("operation replay: {error:?}"))?;
        let replayed = match reattached { graph::AttemptObservation::Pending { attempt_ref } | graph::AttemptObservation::Unknown { attempt_ref } | graph::AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref.ok_or("replay reference")? };
        assert_eq!(replayed, reference);
        let other_factory = factory.for_graph_directory(&other)?;
        let mut replay_owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &other_factory, integration_base: None,
            capability_overrides: (Vec::new(), Vec::new()),
            integration_branch: None, network: None, exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false };
        replay_owner.observe_or_start(&key, node(), &route, graph::AccessMode::ReadOnly, Some(rig.project.to_str().ok_or("source path")?),
            graph::Replay { delivery: None, branch: None, fast_forward_only: false, stored_ref: Some(&reference), launch_actor: &actor })?;
        let run_owner = BinaryRunOwner { factory: &other_factory, allow_remote: true, actor: None, chair: None };
        let call = run_owner.call(&cancel);
        let store = run_owner.store(&cancel)?;
        let operation = store.operation(&graph_operation("graph-attempt-operation", &key)?, &call)?.ok_or("operation")?;
        let request = store.request(operation.run_id, &call)?;
        assert_eq!(request.tasks[0].base.as_deref(), Some(base.as_str()));
        assert_eq!(request.tasks[0].head.as_deref(), Some(head.as_str()));
        assert_eq!(request.tasks[0].access.source_root.as_deref(), Some(rig.project.as_path()));
        assert!(request.tasks[0].access.read_roots.is_empty());
        assert!(request.tasks[0].build_heavy);
        assert_eq!(request.tasks[0].access.write_roots, vec![write_root]);
        let scratch = rig.root.join("review-evidence");
        std::fs::create_dir(&scratch)?;
        let directory = provenant_os::fs::Directory::open_anchor(&scratch)?;
        let git_path = provenant_os::git::git_executable().ok_or("git")?;
        provenant_run::GitContext::capture_at(&git_path, &other, &base, &head)?.write_to(&directory, &scratch)?;
        let stat = std::fs::read_to_string(scratch.join("git-diff.stat"))?;
        assert!(stat.contains("source-change"), "{stat}");
        assert!(stat.contains("predecessor-change"), "{stat}");
        Ok(())
    }

    #[test]
    fn implicit_writer_binding_uses_verified_worktree_head() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::implicit_writer_binding_uses_verified_worktree_head")? { return Ok(()); }
        use provenant_fabric_runtime::graph::AttemptOwner;
        let rig = crate::worktree_tests::repository()?;
        let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
        let initial = git(&["rev-parse", "HEAD"])?;
        let lane = rig.root.join("lane");
        git(&["worktree", "add", "-qb", "lane", lane.to_str().ok_or("lane path")?])?;
        git(&["commit", "--allow-empty", "-qm", "move invoking checkout"])?;
        assert_ne!(git(&["rev-parse", "HEAD"])?, initial);
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?, project_attribution: false,
            clock: SystemClock, backoff: ThreadPause, executor: SystemExecutor::new(1)?,
            home: rig.home.clone(), cwd: rig.project.clone(), assets: rig.project.clone(),
            host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None,
            capability_overrides: (Vec::new(), Vec::new()),
            integration_branch: None, network: None, exclude: Vec::new(), budget, cancel: &cancel,
            prices: None, tick: false };
        let binding = owner.bind_writer_worktree(lane.to_str().ok_or("lane path")?)?.ok_or("binding")?;
        assert_eq!(binding.worktree, std::fs::canonicalize(&lane)?.to_string_lossy());
        assert_eq!(binding.head, initial);
        let metadata = provenant_os::fs::Directory::open_anchor(&lane)?.metadata()?;
        assert_eq!(binding.extra["source_directory_identity"]["device"], metadata.device);
        assert_eq!(binding.extra["source_directory_identity"]["inode"], metadata.inode);
        git(&["-C", lane.to_str().ok_or("lane path")?, "checkout", "--detach", "-q"])?;
        assert!(owner.bind_writer_worktree(lane.to_str().ok_or("lane path")?).is_err());
        Ok(())
    }

    #[test]
    fn integrating_graph_writer_refuses_a_source_outside_the_pinned_base() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::integrating_graph_writer_refuses_a_source_outside_the_pinned_base")? { return Ok(()); }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let git = |args: &[&str]| crate::worktree_tests::git(&rig, args);
        let base = git(&["rev-parse", "HEAD"])?;
        let tree = git(&["rev-parse", "HEAD^{tree}"])?;
        let unrelated = git(&["commit-tree", &tree, "-m", "unrelated"])?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory,
            capability_overrides: (Vec::new(), Vec::new()),
            integration_base: Some(BoundGraphBases { integration: Some(base.parse()?), review: None }),
            integration_branch: Some("batch".into()), network: None, exclude: Vec::new(),
            budget, cancel: &cancel, prices: None, tick: false };
        let actor = owner.actor();
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({"graph_id":"batch", "node_id":"lane-outside", "route_index":0}))?;
        let route = serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
        let delivery = provenant_records::run::DeliveryMode::Integrate("batch".into());
        let error = owner.observe_or_start(&key, graph::NodeInput {
            attachments: &[],
            capabilities: None,
            capability_sources: None, attempt: None,
            require_skill_location: false, settlement_retries: None, prior_attempts: &[], family_peers: &[], context: graph_test_context(&key),
            read_roots: &[], distinct_from_attempts: &[], require_distinct: false,
            source_commit: Some(&unrelated), review_base: None, title: "lane", prompt: "write",
            required_verdict: None, deadline_seconds: None, inputs_dir: None,
        }, &route, graph::AccessMode::WorktreeWrite, None, graph::Replay {
            delivery: Some(&delivery), branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
        }).err().ok_or("unrelated lane was accepted")?;
        assert!(error.to_string().contains("lane-outside"));
        assert!(error.to_string().contains(&base));
        let binary = BinaryRunOwner { factory: &factory, allow_remote: true, actor: None, chair: None };
        assert!(binary.store(&cancel)?.operation(&graph_operation("graph-attempt-operation", &key)?, &binary.call(&cancel))?.is_none());
        Ok(())
    }

    #[test]
    fn graph_replay_preserves_accepted_context_without_a_checkpoint_reference()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_replay_preserves_accepted_context_without_a_checkpoint_reference",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let rig = crate::worktree_tests::repository()?;
        let original_head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "successor"])?;
        let successor_head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"legacy-context", "node_id":"inspect", "route_index":0
        }))?;
        let mut run_owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let replay_prompt = "Review the slice. ".repeat(5_000);
        assert!(replay_prompt.len() > provenant_records::run::OUTPUT_LIMIT as usize);
        let input: SubmitInput = serde_json::from_value(serde_json::json!({
            "op":"submit", "operation_id":graph_operation("graph-attempt-operation", &key)?,
            "tasks":[{
                "title":"inspect", "prompt":replay_prompt,
                "base":original_head,
                "head":successor_head,
                "source_head":original_head,
                "route":{"kind":"model", "provider":"fixture", "model":"fixture/model"},
                "timeout_seconds":factory.context.config.limits.graph_timeout_seconds
            }], "concurrency":1, "knobs":["routing.graph_exclude=[\"codex/gpt-6-luna\"]"]
        }))?;
        let write_root = rig.root.join("extra-write");
        std::fs::create_dir(&write_root)?;
        let grants = provenant_records::run::LaneAccess {
            write_roots: vec![write_root.clone()],
            ..Default::default()
        };
        let accepted = graph::AttemptSpec { build_heavy: true, ..Default::default() };
        let execution_store = rig.state.join("run.sqlite3");
        assert!(!execution_store.exists());
        assert!(matches!(
            run_owner.submit_with_access(input, budget, &cancel, grants.clone(), None, Some(accepted), &provenant_confinement::worktrees::RepoFacts::default()),
            Err(FacadeError::InvalidInput { .. })
        ));
        assert!(!execution_store.exists());
        // Seed a retained pre-cutover launch below the new graph-only boundary.
        // Recovery must preserve its absent context without reopening graphless submission.
        let session = run_owner.session(&cancel)?;
        let accepted_operation = graph_operation("graph-attempt-operation", &key)?;
        let accepted_task = provenant_cli::run::Submitted {
            attempt: graph::AttemptSpec { build_heavy: true, ..Default::default() },
            require_skill_location: false, settlement_retry_limit: None, context: None,
            title: Some("inspect".into()),
            task_id: provenant_cli::run::derived_task(&accepted_operation, 0).map_err(|_| "retained task id")?,
            prompt: replay_prompt.clone(), base: Some(original_head.clone()), pinned_base: None,
            head: Some(successor_head.clone()), source_head: Some(original_head.clone()),
            route: serde_json::from_value(serde_json::json!({"kind":"model", "provider":"fixture", "model":"fixture/model"}))?,
            provider_timeout: factory.context.config.limits.graph_timeout_seconds,
            delivery_id: None, deliver: None, worktree: None, access: grants,
        };
        let (accepted_run, accepted_started) = run_owner.with_edge(|edge| provenant_cli::run::submit_with_host(
            &session, edge, provenant_cli::run::Submission { chair: None, operation_id: accepted_operation.clone(),
                tasks: vec![accepted_task], concurrency: Some(1), knobs: vec!["routing.graph_exclude=[\"codex/gpt-6-luna\"]".into()] },
            &run_owner.call(&cancel), None))?;
        let receipt = run_owner.receipt(&session, accepted_operation, &accepted_run, &accepted_started, &run_owner.call(&cancel))?;
        let target = receipt.targets.first().ok_or("target")?;
        // The plain lane and graph owner both lower through the same submission
        // boundary; compare their durable launch fields, excluding identities.
        let plain_key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"plain-parity", "node_id":"inspect", "route_index":0
        }))?;
        let plain_operation = graph_operation("graph-attempt-operation", &plain_key)?;
        let session = run_owner.session(&cancel)?;
        let plain_task = provenant_cli::run::Submitted {
            attempt: graph::AttemptSpec { build_heavy: true, ..Default::default() },
            require_skill_location: false, settlement_retry_limit: None, context: None,
            title: Some("inspect".into()),
            task_id: provenant_cli::run::derived_task(&plain_operation, 0).map_err(|_| "plain task id")?,
            prompt: replay_prompt.clone(), base: Some(original_head.clone()), pinned_base: None,
            head: Some(successor_head.clone()),
            source_head: Some(original_head.clone()),
            route: serde_json::from_value(serde_json::json!({"kind":"model", "provider":"fixture", "model":"fixture/model"}))?,
            provider_timeout: factory.context.config.limits.graph_timeout_seconds,
            delivery_id: None, deliver: None, worktree: None,
            access: provenant_records::run::LaneAccess { write_roots: vec![write_root.clone()], ..Default::default() },
        };
        let (plain_run, _) = run_owner.with_edge(|edge| provenant_cli::run::submit_with_host(
            &session, edge, provenant_cli::run::Submission { chair: None, operation_id: plain_operation,
                tasks: vec![plain_task], concurrency: Some(1), knobs: vec!["routing.graph_exclude=[\"codex/gpt-6-luna\"]".into()] },
            &run_owner.call(&cancel), None))?;
        let graph_record = session.store.record(target.attempt, &run_owner.call(&cancel))?;
        let graph_request = session.store.request(graph_record.run_id, &run_owner.call(&cancel))?;
        let plain_request = session.store.request(plain_run.run_id, &run_owner.call(&cancel))?;
        let graph_task = &graph_request.tasks[0];
        let plain_task = &plain_request.tasks[0];
        assert_eq!(graph_task.cloud, plain_task.cloud);
        assert_eq!(graph_task.build_heavy, plain_task.build_heavy);
        assert_eq!(graph_task.account, plain_task.account);
        assert_eq!(graph_task.route, plain_task.route);
        assert_eq!(graph_task.access, plain_task.access);
        assert_eq!(graph_task.prompt_digest, plain_task.prompt_digest);
        let mut graph_owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
            factory: &factory,
            integration_base: None,
            integration_branch: None,
            network: None, capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let actor = graph_owner.actor();
        let route = serde_json::from_value(
            serde_json::json!({"adapter":"fixture", "model":"fixture/model"}),
        )?;
        let store = run_owner.store(&cancel)?;
        {
            let observation = graph_owner.observe_or_start(
                &key,
                graph::NodeInput { capabilities: None, capability_sources: None, attempt: None, require_skill_location: false,
                    settlement_retries: None,
                    prior_attempts: &[],
                    family_peers: &[],
                    context: provenant_records::run::WorkContext {
                        graph_id: Some(key.graph_id.clone()),
                        node_id: Some(key.node_id.clone()),
                        pattern: Some("council".into()),
                        ..Default::default()
                    },
                    read_roots: &[],
                    attachments: &[],
                    distinct_from_attempts: &[],
                    require_distinct: false,
                    source_commit: None,
                    review_base: None,
                    title: "inspect",
                    prompt: &replay_prompt,
                    required_verdict: None,
                    deadline_seconds: None,
                    inputs_dir: None,
                },
                &route,
                graph::AccessMode::ReadOnly,
                None,
                graph::Replay {
                    delivery: None,
                    branch: None,
                    fast_forward_only: false,
                    stored_ref: None,
                    launch_actor: &actor,
                },
            ).map_err(|error| format!("recover accepted graph launch: {error:?}"))?;
            let graph::AttemptObservation::Terminal { attempt_ref, .. } = observation else {
                return Err("expected settled fixture attempt".into());
            };
            assert_eq!(
                attempt_ref,
                Some(format!(
                    "{}/{}",
                    factory.context.identity.host, target.attempt
                ))
            );
            let record = store.record(target.attempt, &run_owner.call(&cancel))?;
            assert!(record.context.is_none());
            let request = store.request(record.run_id, &run_owner.call(&cancel))?;
            assert!(request.tasks[0].build_heavy);
            assert_eq!(request.tasks[0].source_head.as_deref(), Some(original_head.as_str()));
            assert_eq!(request.tasks[0].head.as_deref(), Some(successor_head.as_str()));
            assert_eq!(request.tasks[0].access.write_roots, vec![write_root]);
        }
        Ok(())
    }

    #[test]
    fn run_list_is_compact_and_pages_newest_first() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::run_list_is_compact_and_pages_newest_first")? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let first = owner.run(
            RunInput::List {
                extra: Default::default(),
                after: None,
                limit: 1,
            },
            budget,
            &cancel,
        )?;
        let wire = serde_json::to_value(&first)?;
        let row = &wire["page"]["items"][0];
        assert!(
            row["attempt_id"] == fixture.settled.attempt_id.to_string()
                && row.get("usage").is_none()
                && row.get("prompt").is_none()
                && row.get("trace").is_none(),
            "expected newest compact row, got {row}"
        );
        let RunResult::List { page, .. } = first else {
            return Err("list result".into());
        };
        assert!(page.truncated);
        let cursor = page.next_after.ok_or("next cursor")?;
        let second = owner.run(
            RunInput::List {
                extra: Default::default(),
                after: Some(cursor),
                limit: 1,
            },
            budget,
            &cancel,
        )?;
        let RunResult::List { page, .. } = second else {
            return Err("list result".into());
        };
        assert!(!page.truncated);
        assert!(page.next_after.is_none());
        let second = serde_json::to_value(&page.items[0])?;
        assert_ne!(second["attempt_id"], row["attempt_id"]);
        assert_eq!(second["state"], "cancelled");
        assert_eq!(
            second["reason"],
            provenant_records::run::Outcome::Cancelled.diagnostic_reason()
        );
        assert!(!second["reason"].as_str().unwrap().contains("fixture"));
        assert!(second["finished_at"].is_string());
        let status = owner.run(
            RunInput::Status {
                extra: Default::default(),
                target: AttemptTarget {
                    extra: Default::default(),
                    attempt: fixture.settled.attempt_id,
                    host: None,
                },
            },
            budget,
            &cancel,
        )?;
        let RunResult::Status { mut attempt, .. } = status else {
            return Err("status result".into());
        };
        let status = serde_json::to_value(RunResult::Status {
            extra: Default::default(),
            attempt: attempt.clone(),
        })?;
        assert!(status["attempt"].get("usage").is_some());
        assert!(status["attempt"].get("prompt").is_some());
        // Pool routing carries an audit receipt in full records. It must not
        // enlarge the compact route returned by list.
        attempt.applied_route = Some(serde_json::from_value(serde_json::json!({
            "provider":"fixture", "model":"fixture/model", "effort":"high",
            "account_label":"fixture", "account_key":"private-key", "source":"pool",
            "pool_selection": {
                "pool":"senior", "candidates":[], "roll":0, "total_weight":1,
                "pick":"fixture/model"
            }
        }))?);
        let summary = provenant_fabric_runtime::runs::AttemptSummary::from(attempt);
        let summary = serde_json::to_value(summary)?;
        assert_eq!(
            summary["applied_route"],
            serde_json::json!({
                "provider":"fixture", "model":"fixture/model", "effort":"high",
                "account_label":"fixture"
            })
        );
        Ok(())
    }

    #[test]
    fn graph_hint_requires_a_matching_attempt() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_hint_requires_a_matching_attempt")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{FileJournal, GraphJournal};
        let fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request { clock: &clock, cancel: &cancel };
        let mut record = fixture.store.record(fixture.settled.attempt_id, &call)?;
        let document = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"hint-test","nodes":[{"id":"one","prompt":"work","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?;
        let checkpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"hint-test","plan_digest":"digest","nodes":[{
                "id":"one","worktree":null,"state":"running","attempts":[{
                    "key":{"graph_id":"hint-test","node_id":"one","route_index":0},
                    "route":{"adapter":"fixture","model":"fixture/model"},"state":"starting",
                    "launched":true,"launch_actor":"fixture", "attempt_ref":format!("host/{}",record.attempt_id)
                }]
            }]
        }))?;
        let mut journal = FileJournal::open(&fixture.roots.state, fixture.settled.project_id, "hint-test", call.lock_budget(), &cancel, &clock)?;
        journal.bind(provenant_fabric_runtime::graph::GraphExecution { extra: Default::default(), integration_base: None, review_base: None, directory: fixture.rig.project.clone(), document })?;
        journal.save(&checkpoint)?;
        drop(journal);
        assert!(matching_graph_hint(&fixture.roots.state, fixture.settled.project_id, &record).is_none());
        record.context = Some(provenant_records::run::WorkContext { graph_id: Some("unknown".into()), ..Default::default() });
        assert!(matching_graph_hint(&fixture.roots.state, fixture.settled.project_id, &record).is_none());
        record.context.as_mut().ok_or("context")?.graph_id = Some("hint-test".into());
        assert_eq!(matching_graph_hint(&fixture.roots.state, fixture.settled.project_id, &record).as_deref(), Some("hint-test"));
        record.attempt_id = AttemptId::from_bytes([99; 16]);
        assert!(matching_graph_hint(&fixture.roots.state, fixture.settled.project_id, &record).is_none());
        Ok(())
    }

    #[test]
    fn settlement_advances_only_matching_graph_and_retains_writer()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::settlement_advances_only_matching_graph_and_retains_writer")? { return Ok(()); }
        use provenant_fabric_runtime::graph;
        struct Seed { reference: String, actor: String }
        impl graph::AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String { self.actor.clone() }
            fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute,
                _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending { attempt_ref: Some(self.reference.clone()) })
            }
        }
        let mut fixture = SettlementFixture::settled_fixture("graph-fixture", Some("matching"))?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let identity = Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.rig.project)?.identity;
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        for id in ["matching", "unrelated"] {
            let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"reply OK",
                "access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?;
            let mut journal = graph::FileJournal::open(&fixture.roots.state, fixture.settled.project_id, id, budget, &cancel, &clock)?;
            journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
                directory: fixture.rig.project.clone(), document: document.clone() })?;
            let mut seed = Seed { reference: format!("{}/{}", identity.host, fixture.settled.attempt_id), actor: identity.inbox.actor.to_string() };
            graph::advance_with_budget(&document, &mut seed, &mut journal, &Default::default(), &Default::default())?;
        }
        fixture.settled.launch = Some(provenant_run::HostLaunchContext {
            configuration_root: fixture.rig.state.clone(), cwd: fixture.rig.project.clone(), executables: Default::default(),
            executable_search_path: Vec::new(), git: provenant_os::git::git_executable().ok_or("git")?, default_account_directories: Default::default(),
            cargo_build_jobs: None, fixture_state_write: false,
        });
        advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)?;
        let matched = graph::FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, "matching")?.ok_or("matching")?;
        let other = graph::FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, "unrelated")?.ok_or("unrelated")?;
        assert_eq!(matched.nodes[0].state, graph::NodeState::Succeeded);
        assert_eq!(other.nodes[0].state, graph::NodeState::Running);
        assert!(fixture.worktree.exists());
        let call = provenant_run::Request { clock: &clock, cancel: &cancel };
        let receipt = fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?.ok_or("receipt")?;
        assert_eq!(receipt["state"], "retained");
        Ok(())
    }

    #[test]
    fn settlement_and_tick_read_no_terminal_graphs_at_scale() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::settlement_and_tick_read_no_terminal_graphs_at_scale")? { return Ok(()); }
        use provenant_fabric_runtime::graph;
        struct Seed { reference: String, actor: String, at: provenant_core::WallTime }
        impl graph::AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String { self.actor.clone() }
            fn wall_now(&self) -> Option<provenant_core::WallTime> { Some(self.at) }
            fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute,
                _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending { attempt_ref: Some(self.reference.clone()) })
            }
        }
        let mut fixture = SettlementFixture::settled_fixture("graph-count", Some("owner"))?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let identity = Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.rig.project)?.identity;
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let now = clock.wall_now()?;
        let mut seed = Seed { reference: format!("{}/{}", identity.host, fixture.settled.attempt_id), actor: identity.inbox.actor.to_string(), at: now };
        for number in 0..20 {
            let id = if number == 0 { "owner".to_owned() } else { format!("pending-{number:02}") };
            let nodes = if number == 0 { serde_json::json!([{"id":"one","prompt":"reply OK","access":"worktree_write",
                "worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]) }
            else { serde_json::json!([{"id":"choose","kind":"decide","access":"read_only","decide":{
                "question":"Ship?","options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
                "default":"hold","timeout_seconds":3600}}]) };
            let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version":1,"graph_id":id,"decision_session":"chair-session","nodes":nodes
            }))?;
            let mut journal = graph::FileJournal::open(&fixture.roots.state, fixture.settled.project_id, &id, budget, &cancel, &clock)?;
            journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
                directory: fixture.rig.project.clone(), document: document.clone() })?;
            graph::advance_with_budget(&document, &mut seed, &mut journal, &Default::default(), &Default::default())?;
        }
        let mut terminal = graph::FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, "owner")?.ok_or("owner")?;
        let mut execution = graph::FileJournal::execution(&fixture.roots.state, fixture.settled.project_id, "owner")?.ok_or("execution")?;
        terminal.nodes[0].state = graph::NodeState::Succeeded;
        terminal.nodes[0].attempts.clear();
        let directory = fixture.roots.state.as_path().join("fabric/graphs").join(fixture.settled.project_id.to_string());
        for number in 0..5000 {
            let id = format!("terminal-{number:04}");
            terminal.graph_id = id.clone();
            execution.document.graph_id = id.clone();
            let digest = provenant_records::CanonicalV1::record_digest("fabric-graph-checkpoint", 2, &(&terminal, &execution))?.to_string();
            let envelope = serde_json::json!({"schema_version":1,"digest":digest,"checkpoint":terminal,"execution":execution});
            std::fs::write(directory.join(format!("graph-{id}.json")), serde_json::to_vec(&envelope)?)?;
        }
        assert_eq!(graph::FileJournal::pending_graph_ids(&fixture.roots.state, fixture.settled.project_id)?.len(), 20);
        graph::take_checkpoint_read_ids();
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let reads = graph::take_checkpoint_read_ids();
        assert!(!reads.is_empty());
        assert!(reads.iter().all(|id| id == "owner"), "{reads:?}");
        fixture.settled.launch = Some(provenant_run::HostLaunchContext {
            configuration_root: fixture.rig.state.clone(), cwd: fixture.rig.project.clone(), executables: Default::default(),
            executable_search_path: Vec::new(), git: provenant_os::git::git_executable().ok_or("git")?, default_account_directories: Default::default(),
            cargo_build_jobs: None, fixture_state_write: false,
        });
        advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)?;
        let reads = graph::take_checkpoint_read_ids();
        assert!(!reads.is_empty());
        assert!(reads.iter().all(|id| id == "owner"), "{reads:?}");
        let resolved = crate::roots::Resolved { assets: fixture.rig.project.clone(), home: fixture.rig.home.clone(),
            project: fixture.rig.project.clone(), roots: fixture.roots.clone(), claude_config: None, codex_home: None };
        let report = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let reads = graph::take_checkpoint_read_ids();
        assert!(!reads.is_empty());
        assert!(reads.iter().all(|id| id.starts_with("pending-")), "{reads:?}");
        Ok(())
    }

    #[test]
    fn incorrect_graph_hints_defer_owning_graph_to_tick() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::incorrect_graph_hints_defer_owning_graph_to_tick")? { return Ok(()); }
        use provenant_fabric_runtime::graph::{self, GraphJournal};
        struct Seed { reference: String, actor: String }
        impl graph::AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String { self.actor.clone() }
            fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute,
                _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending { attempt_ref: Some(self.reference.clone()) })
            }
        }
        for hint in ["wrong", "unknown"] {
            let mut fixture = SettlementFixture::settled_fixture("graph-mismatch", Some(hint))?;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let identity = Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.rig.project)?.identity;
            let budget = WaitBudget::cli(clock.mono_now(), 30)?;
            for id in ["owner", "wrong"] {
                let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                    "schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"reply OK",
                    "access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]
                }))?;
                let mut journal = graph::FileJournal::open(&fixture.roots.state, fixture.settled.project_id, id, budget, &cancel, &clock)?;
                journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
                    directory: fixture.rig.project.clone(), document: document.clone() })?;
                let reference = if id == "owner" { format!("{}/{}", identity.host, fixture.settled.attempt_id) }
                    else { format!("{}/{}", identity.host, AttemptId::from_bytes([99;16])) };
                let mut seed = Seed { reference, actor: identity.inbox.actor.to_string() };
                let mut run = graph::advance_with_budget(&document, &mut seed, &mut journal, &Default::default(), &Default::default())?;
                if id == "wrong" { run.checkpoint.nodes[0].state = graph::NodeState::Succeeded; journal.save(&run.checkpoint)?; }
            }
            fixture.settled.launch = Some(provenant_run::HostLaunchContext {
                configuration_root: fixture.rig.state.clone(), cwd: fixture.rig.project.clone(), executables: Default::default(),
                executable_search_path: Vec::new(), git: provenant_os::git::git_executable().ok_or("git")?, default_account_directories: Default::default(),
                cargo_build_jobs: None, fixture_state_write: false,
            });
            advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)?;
            let before = graph::FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, "owner")?.ok_or("owner")?;
            assert_eq!(before.nodes[0].state, graph::NodeState::Running);
            let resolved = crate::roots::Resolved { assets: fixture.rig.project.clone(), home: fixture.rig.home.clone(),
                project: fixture.rig.project.clone(), roots: fixture.roots.clone(), claude_config: None, codex_home: None };
            let report = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            let after = graph::FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, "owner")?.ok_or("owner")?;
            assert_eq!(after.nodes[0].state, graph::NodeState::Succeeded);
            assert!(fixture.worktree.exists());
        }
        Ok(())
    }

    #[test]
    fn unmatched_settlement_hints_wait_for_tick() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::unmatched_settlement_hints_wait_for_tick")? { return Ok(()); }
        use provenant_fabric_runtime::graph;
        struct Seed { reference: String, actor: String }
        impl graph::AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String { self.actor.clone() }
            fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute,
                _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending { attempt_ref: Some(self.reference.clone()) })
            }
        }
        for hint in [None, Some("unknown"), Some("other")] {
            let mut fixture = SettlementFixture::settled_fixture("graph-fixture", hint)?;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let identity = Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.rig.project)?.identity;
            let budget = WaitBudget::cli(clock.mono_now(), 30)?;
            for id in ["owner", "other"] {
                let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                    "schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"reply OK",
                    "access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]
                }))?;
                let mut journal = graph::FileJournal::open(&fixture.roots.state, fixture.settled.project_id, id, budget, &cancel, &clock)?;
                journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
                    directory: fixture.rig.project.clone(), document: document.clone() })?;
                let attempt = if id == "owner" { fixture.settled.attempt_id } else { AttemptId::from_bytes([99; 16]) };
                let mut seed = Seed { reference: format!("{}/{attempt}", identity.host), actor: identity.inbox.actor.to_string() };
                graph::advance_with_budget(&document, &mut seed, &mut journal, &Default::default(), &Default::default())?;
            }
            fixture.settled.launch = Some(provenant_run::HostLaunchContext {
                configuration_root: fixture.rig.state.clone(), cwd: fixture.rig.project.clone(), executables: Default::default(),
                executable_search_path: Vec::new(), git: provenant_os::git::git_executable().ok_or("git")?, default_account_directories: Default::default(),
                cargo_build_jobs: None, fixture_state_write: false,
            });
            advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)?;
            let owner = graph::FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, "owner")?.ok_or("owner")?;
            assert_eq!(owner.nodes[0].state, graph::NodeState::Running, "hint: {hint:?}");
            let resolved = crate::roots::Resolved { assets: fixture.rig.project.clone(), home: fixture.rig.home.clone(),
                project: fixture.rig.project.clone(), roots: fixture.roots.clone(), claude_config: None, codex_home: None };
            let tick = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
            assert!(tick.advanced > 0, "hint: {hint:?}");
            let owner = graph::FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, "owner")?.ok_or("owner")?;
            assert_eq!(owner.nodes[0].state, graph::NodeState::Succeeded, "hint: {hint:?}");
            assert!(fixture.worktree.exists());
        }
        Ok(())
    }

    #[test]
    fn settlement_without_hint_does_not_advance_unrelated_graphs()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::settlement_without_hint_does_not_advance_unrelated_graphs",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{
            self, AttemptKey, AttemptObservation, AttemptOwner, FileJournal, GraphDocument,
            GraphExecution, GraphRoute, NodeState, Replay,
        };
        struct SeedAttempt {
            reference: String,
            actor: String,
        }
        impl AttemptOwner for SeedAttempt {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                self.actor.clone()
            }
            fn observe_or_start(
                &mut self,
                _: &AttemptKey,
                _: graph::NodeInput<'_>,
                _: &GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: Replay<'_>,
            ) -> Result<AttemptObservation, Self::Error> {
                Ok(AttemptObservation::Pending {
                    attempt_ref: Some(self.reference.clone()),
                })
            }
        }
        let mut fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let identity = Context::open(
            fixture.roots.clone(),
            &fixture.rig.home,
            &fixture.rig.project,
        )?
        .identity;
        // Two different terminal attempts let the settlement error be attributed
        // to A alone. B's journal still needs its running node to be observed.
        let continuation = provenant_run::Resume {
            prior: fixture.settled.attempt_id,
            expected_generation: 0,
            operation_id: "graph-b-attempt".parse()?,
            run_id: RunId::from_bytes([40; 16]),
            attempt_id: AttemptId::from_bytes([41; 16]),
            prompt: "continue".into(),
            created_at: clock.wall_now()?,
        };
        fixture
            .store
            .resume_fresh(&continuation, "fixture", &call)?;
        let second = fixture.store.record(continuation.attempt_id, &call)?;
        let head = fixture
            .store
            .record(fixture.settled.attempt_id, &call)?
            .head
            .ok_or("head")?;
        publish_success(&fixture.rig, &fixture.store, &second, head, &call)?;
        for (id, attempt) in [
            ("a-settlement-error", fixture.settled.attempt_id),
            ("b-completes", continuation.attempt_id),
        ] {
            let document: GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?;
            let mut journal = FileJournal::open(
                &fixture.roots.state,
                fixture.settled.project_id,
                id,
                call.lock_budget(),
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {review_base: None,
                extra: Default::default(),
                integration_base: None,
                directory: fixture.rig.project.clone(),
                document: document.clone(),
            })?;
            let mut seed = SeedAttempt {
                reference: format!("{}/{attempt}", identity.host),
                actor: identity.inbox.actor.to_string(),
            };
            let seeded = graph::advance_with_budget(
                &document,
                &mut seed,
                &mut journal,
                &Default::default(),
                &Default::default(),
            )?;
            assert_eq!(seeded.checkpoint.nodes[0].state, NodeState::Running);
        }
        // An unsafe receipt path makes settlement fail. With no graph hint,
        // neither graph is advanced by this settlement.
        let receipt_path = fixture
            .rig
            .state
            .join("runs")
            .join(fixture.settled.run_id.to_string())
            .join("attempts")
            .join(fixture.settled.attempt_id.to_string())
            .join("worktree-settlement.json");
        std::fs::create_dir(&receipt_path)?;
        let settlement_error =
            settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)
                .err()
                .ok_or("settlement should fail")?
                .to_string();
        fixture.settled.launch = Some(provenant_run::HostLaunchContext {
            configuration_root: fixture.rig.state.clone(),
            cwd: fixture.rig.project.clone(),
            executables: Default::default(),
            executable_search_path: Vec::new(),
            git: provenant_os::git::git_executable().ok_or("git")?,
            default_account_directories: Default::default(),
            cargo_build_jobs: None,
            fixture_state_write: false,
        });
        let error = advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)
            .err()
            .ok_or("settlement failure must propagate")?;
        assert_eq!(error.to_string(), settlement_error);
        assert_eq!(
            FileJournal::read_advance_error(
                &fixture.roots.state,
                fixture.settled.project_id,
                "a-settlement-error"
            )?
            .as_deref(),
            None
        );
        let completed = FileJournal::read_checkpoint(
            &fixture.roots.state,
            fixture.settled.project_id,
            "b-completes",
        )?
        .ok_or("B checkpoint")?;
        assert_eq!(completed.nodes[0].state, NodeState::Running);
        assert!(
            FileJournal::pending_execution(
                &fixture.roots.state,
                fixture.settled.project_id,
                "b-completes"
            )?
            .is_some()
        );
        assert!(
            FileJournal::read_advance_error(
                &fixture.roots.state,
                fixture.settled.project_id,
                "b-completes"
            )?
            .is_none()
        );
        assert!(fixture.worktree.exists());
        let resolved = crate::roots::Resolved {
            assets: fixture.rig.project.clone(), home: fixture.rig.home.clone(),
            project: fixture.rig.project.clone(), roots: fixture.roots.clone(),
            claude_config: None, codex_home: None,
        };
        let tick = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
        assert!(tick.advanced > 0);
        let completed = FileJournal::read_checkpoint(
            &fixture.roots.state, fixture.settled.project_id, "b-completes",
        )?.ok_or("B checkpoint after tick")?;
        assert_eq!(completed.nodes[0].state, NodeState::Succeeded);
        Ok(())
    }

    #[test]
    fn graph_owner_fails_node_when_result_integrity_is_lost()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_owner_fails_node_when_result_integrity_is_lost")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner, GraphJournal};
        struct Seed(String);
        impl AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.0.clone()),
                })
            }
        }
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let artifact = fixture.rig.state.join(format!(
            "runs/{}/attempts/{}/result.md",
            fixture.settled.run_id, fixture.settled.attempt_id
        ));
        std::fs::write(&artifact, b"XX")?;
        for required in [None, Some("VERDICT: PASS")] {
            let id = if required.is_some() {
                "corrupt-required"
            } else {
                "corrupt-result"
            };
            let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version":1,"graph_id":id,"nodes":[
                    {"id":"parent","prompt":"work","access":"worktree_write","worktree":fixture.worktree,"required_verdict":required,"route":{"adapter":"fixture","model":"fixture/model"}},
                    {"id":"child","prompt":"review","access":"read_only","depends_on":["parent"],"inputs":[{"node":"parent","outputs":["result"]}],"route":{"adapter":"fixture","model":"fixture/model"}}
                ]
            }))?;
            let mut journal = graph::FileJournal::open(
                &fixture.roots.state,
                fixture.settled.project_id,
                id,
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.bind(graph::GraphExecution {review_base: None,
                extra: Default::default(),
                integration_base: None,
                directory: fixture.rig.project.clone(),
                document: document.clone(),
            })?;
            let mut seed = Seed(format!(
                "{}/{}",
                factory.context.identity.host, fixture.settled.attempt_id
            ));
            graph::advance_with_budget(
                &document,
                &mut seed,
                &mut journal,
                &Default::default(),
                &Default::default(),
            )?;
            let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
                factory: &factory,
                integration_base: None,
                integration_branch: None,
                network: None, capability_overrides: Default::default(),
                exclude: Vec::new(),
                budget,
                cancel: &cancel,
                prices: None,
                tick: false,
            };
            let run = graph::advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &Default::default(),
                &Default::default(),
            )?;
            let parent = &run.checkpoint.nodes[0];
            assert_eq!(parent.state, graph::NodeState::Failed);
            assert_eq!(
                parent.attempts[0].outcome,
                Some(graph::AttemptOutcome::OtherFailure)
            );
            assert!(
                parent
                    .outputs
                    .as_ref()
                    .ok_or("outputs")?
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("digest or length"))
            );
            assert_eq!(run.checkpoint.nodes[1].state, graph::NodeState::Blocked);
            assert_eq!(
                journal.load(id)?.ok_or("checkpoint")?.nodes[0].state,
                graph::NodeState::Failed
            );
        }

        // A corrupt authentication failure must not consume a fallback route,
        // even for a read-only node where that provider outcome normally would.
        let call = provenant_run::Request {
            clock: &factory.clock,
            cancel: &cancel,
        };
        let continuation = provenant_run::Resume {
            prior: fixture.settled.attempt_id,
            expected_generation: 0,
            operation_id: "graph-corrupt-auth".parse()?,
            run_id: RunId::from_bytes([43; 16]),
            attempt_id: AttemptId::from_bytes([44; 16]),
            prompt: "continue".into(),
            created_at: factory.clock.wall_now()?,
        };
        fixture
            .store
            .resume_fresh(&continuation, "fixture", &call)?;
        let record = fixture.store.record(continuation.attempt_id, &call)?;
        let head = fixture
            .store
            .record(fixture.settled.attempt_id, &call)?
            .head
            .ok_or("head")?;
        publish_outcome(
            &fixture.rig,
            &fixture.store,
            &record,
            head,
            Outcome::AuthRequired,
            &call,
        )?;
        let artifact = fixture.rig.state.join(format!(
            "runs/{}/attempts/{}/result.md",
            continuation.run_id, continuation.attempt_id
        ));
        std::fs::write(&artifact, b"XX")?;
        let id = "corrupt-auth";
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":id,"nodes":[
                {"id":"parent","prompt":"work","access":"read_only","routes":[
                    {"adapter":"fixture","model":"fixture/model"},
                    {"adapter":"fixture","model":"fixture/other"}
                ]}
            ]
        }))?;
        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;
        let mut seed = Seed(format!(
            "{}/{}",
            factory.context.identity.host, continuation.attempt_id
        ));
        graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
            factory: &factory,
            integration_base: None,
            integration_branch: None,
            network: None, capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let parent = &run.checkpoint.nodes[0];
        assert_eq!(parent.state, graph::NodeState::Failed);
        assert_eq!(parent.attempts.len(), 1);
        assert_eq!(
            parent.attempts[0].outcome,
            Some(graph::AttemptOutcome::OtherFailure)
        );
        assert!(
            parent
                .outputs
                .as_ref()
                .ok_or("outputs")?
                .warnings
                .iter()
                .any(|warning| warning.contains("digest or length"))
        );
        std::fs::remove_file(&artifact)?;
        let mut missing_document = document.clone();
        missing_document.graph_id = "missing-auth-result".into();
        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            &missing_document.graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: fixture.rig.project.clone(),
            document: missing_document.clone(),
        })?;
        let mut seed = Seed(format!(
            "{}/{}",
            factory.context.identity.host, continuation.attempt_id
        ));
        graph::advance_with_budget(
            &missing_document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let run = graph::advance_with_budget(
            &missing_document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let parent = &run.checkpoint.nodes[0];
        assert_eq!(parent.state, graph::NodeState::Failed);
        assert_eq!(parent.attempts.len(), 1);
        assert_eq!(
            parent.attempts[0].outcome,
            Some(graph::AttemptOutcome::OtherFailure)
        );
        assert!(
            parent
                .outputs
                .as_ref()
                .ok_or("outputs")?
                .warnings
                .iter()
                .any(|warning| warning == "result artifact is missing")
        );
        Ok(())
    }

    #[test]
    fn exact_route_in_an_excluded_family_is_refused_before_launch()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::exact_route_in_an_excluded_family_is_refused_before_launch",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        struct Seed(String);
        impl AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.0.clone()),
                })
            }
        }
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let call = provenant_run::Request {
            clock: &factory.clock,
            cancel: &cancel,
        };
        // The upstream attempt ran in the OpenAI family.
        let upstream = provenant_run::Resume {
            prior: fixture.settled.attempt_id,
            expected_generation: 0,
            operation_id: "graph-exact-upstream".parse()?,
            run_id: RunId::from_bytes([53; 16]),
            attempt_id: AttemptId::from_bytes([54; 16]),
            prompt: "continue".into(),
            created_at: factory.clock.wall_now()?,
        };
        fixture.store.resume_fresh(&upstream, "fixture", &call)?;
        let record = fixture.store.record(upstream.attempt_id, &call)?;
        let head = fixture
            .store
            .record(fixture.settled.attempt_id, &call)?
            .head
            .ok_or("head")?;
        publish_routed(
            &fixture.rig,
            &fixture.store,
            &record,
            head,
            Outcome::Succeeded,
            "codex/gpt-6-luna",
            &call,
        )?;
        let id = "exact-separation";
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":id,"nodes":[
                {"id":"author","prompt":"work","access":"read_only",
                    "route":{"adapter":"fixture","model":"fixture/model"}},
                {"id":"verifier","prompt":"verify","access":"read_only","depends_on":["author"],
                    "distinct_from":"dependencies","distinct_strength":"require",
                    "route":{"adapter":"codex","model":"gpt-6.1-terra"}}
            ]
        }))?;
        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;
        graph::advance_with_budget(
            &document,
            &mut Seed(format!(
                "{}/{}",
                factory.context.identity.host, upstream.attempt_id
            )),
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
            factory: &factory,
            integration_base: None,
            integration_branch: None,
            network: None, capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let prior = format!("{}/{}", factory.context.identity.host, upstream.attempt_id);
        assert_eq!(owner.prior_models(&[prior.clone(), prior.clone()])?, ["codex/gpt-6-luna", "codex/gpt-6-luna"]);
        assert_eq!(provenant_fabric_runtime::graph::AttemptOwner::attempt_models(&mut owner, &[prior.clone(), prior.clone()])?, ["codex/gpt-6-luna", "codex/gpt-6-luna"]);
        // Resolve the same alias again with the host's per-node exclusion list.
        use provenant_routing::{AccessContext, Catalogue, ExclusionRule, ExclusionSelector, ExclusionSource, Fallback, ModelId, RouteChoice, RouteName, RouteRequest, RoutingConfig, RoutingContext};
        let catalogue = Catalogue::bundled()?;
        let members: Vec<provenant_routing::RoutePoolMember> = serde_json::from_value(serde_json::json!([
            {"model":"codex/gpt-6-luna", "weight":1}, {"model":"claude/claude-haiku-5-5", "weight":1}
        ]))?;
        let availability: Vec<_> = members.iter().map(|member| provenant_providers::AvailabilityRecord {
            schema_version: 1, adapter: member.model.adapter().into(), executable: "/fixture/provider".into(),
            version: "fixture".into(), fixture_ref: "fixture".into(), catalogue_revision: catalogue.revision().into(),
            account_key: Some(format!("{}-account", member.model.adapter())), observed_models: vec![member.model.model().into()],
            observed_unix_seconds: 80, expires_unix_seconds: 120, last_update: None,
        }).collect();
        let access: Vec<_> = members.iter().map(|member| AccessContext { adapter: member.model.adapter(), disclosure_ref: "fixture", confinement_ref: "fixture" }).collect();
        let mut config = RoutingConfig::default();
        config.pools.insert("retry-trial".into(), members.clone());
        let context = RoutingContext { policy_account_hint: None, choice_context: None, catalogue: &catalogue, config: &config,
            registry: provenant_providers::REGISTRY, availability: &availability, quota: &[], capacity_cooldowns: &[], accounts: &[], quota_policies: &[], access: &access,
            now: provenant_core::WallTime::from_micros(100_000_000)?, host: "fixture", readiness: None };
        let request = RouteRequest { intent: Default::default(), choice: RouteChoice::NamedRoute(RouteName::parse("retry-trial")?), effort: None, confidential: false, fallback: Fallback::None };
        let seed = (0..256).find(|seed| provenant_routing::resolve_with_seed(&request, &context, "retry", *seed)
            .is_ok_and(|route| format!("{}/{}", route.adapter, route.model) == "codex/gpt-6-luna")).ok_or("no first draw")?;
        let alias: graph::GraphRoute = serde_json::from_value(serde_json::json!({"alias":"retry-trial"}))?;
        // The runtime passes the prior models it resolved from attempt records.
        let prior_models = owner.prior_models(std::slice::from_ref(&prior))?;
        let exclusions = owner.route_exclusions(&alias, &prior_models, false);
        assert_eq!(exclusions, ["codex/gpt-6-luna"]);
        assert!(owner.route_exclusions(&alias, &prior_models, true).is_empty());
        let rules = [ExclusionRule { selector: ExclusionSelector::Model(ModelId::parse(&exclusions[0])?), source: ExclusionSource::Graph }];
        let second = provenant_routing::resolve_for_account_with_seed_excluding(&request, &context, None, "retry", seed, &[], &rules)?;
        assert_eq!(format!("{}/{}", second.adapter, second.model), "claude/claude-haiku-5-5");
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        assert_eq!(run.checkpoint.nodes[0].state, graph::NodeState::Succeeded);
        let verifier = &run.checkpoint.nodes[1];
        assert_eq!(verifier.state, graph::NodeState::Failed, "{verifier:?}");
        let attempt = &verifier.attempts[0];
        assert!(!attempt.launched);
        assert_eq!(attempt.attempt_ref, None, "the exact route was submitted");
        assert_eq!(
            attempt.outcome,
            Some(graph::AttemptOutcome::AdapterUnavailable)
        );
        let separation = verifier.separation.as_ref().ok_or("separation")?;
        assert_eq!(separation.strength, graph::FamilyStrength::Require);
        assert_eq!(separation.excluded, ["openai"]);
        assert_eq!(separation.outcome, graph::SeparationOutcome::Unavailable);
        Ok(())
    }

    #[test]
    fn production_owner_panel_uses_distinct_families_and_synthesises()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::production_owner_panel_uses_distinct_families_and_synthesises")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        use provenant_records::run::RequestedRoute;
        use provenant_routing::{AccessContext, Catalogue, ExclusionRule, ExclusionSelector,
            ExclusionSource, Fallback, ModelId, RouteChoice, RouteName, RouteRequest};
        // Bundled production data has only two known families. Give two
        // existing catalogue models fixture families, without changing the
        // production catalogue, so writer exclusion and sibling diversity can
        // both be observed with a fallback route available.
        let _families = provenant_routing::fixture_family_override(&[
            ("agy/gemini-3.8-flash", "google"),
            ("opencode/opencode-go/glm-5.3-flash", "zhipu"),
        ])?;
        let fixture = SettlementFixture::resumed().map_err(|error| format!("fixture: {error}"))?;
        use std::io::Write as _;
        writeln!(std::fs::OpenOptions::new().append(true).open(fixture.roots.state.as_path().join("config.toml"))?,
            "\n[domains.routing.pools]\npanel-fixture = [{{model='codex/gpt-6-luna'}},{{model='claude/claude-haiku-5-5'}},{{model='agy/gemini-3.8-flash'}},{{model='opencode/opencode-go/glm-5.3-flash'}}]")?;
        let factory = Factory {
            context: Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(), assets: fixture.rig.project.clone(),
            host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 120)?;
        let call = provenant_run::Request { clock: &factory.clock, cancel: &cancel };
        let head = fixture.store.record(fixture.settled.attempt_id, &call)?.head.ok_or("head")?;
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":2, "graph_id":"production-panel", "template_library":{}, "nodes":[
                {"id":"writer", "prompt":"write", "access":"worktree_write", "worktree":fixture.worktree,
                    "route":{"adapter":"codex","model":"gpt-6-luna"}},
                {"id":"one", "prompt":"review", "access":"read_only", "depends_on":["writer"],
                    "distinct_from":"dependencies", "distinct_strength":"require",
                    "family_group":"panel", "routes":[{"alias":"panel-fixture"},{"alias":"panel-fixture"}]},
                {"id":"two", "prompt":"review", "access":"read_only", "depends_on":["writer","one"],
                    "distinct_from":"dependencies", "distinct_strength":"require",
                    "family_group":"panel", "routes":[{"alias":"panel-fixture"},{"alias":"panel-fixture"}]},
                {"id":"three", "prompt":"review", "access":"read_only", "depends_on":["writer","two"],
                    "distinct_from":"dependencies", "distinct_strength":"require",
                    "family_group":"panel", "routes":[{"alias":"panel-fixture"},{"alias":"panel-fixture"}]},
                {"id":"synthesis", "prompt":"synthesise", "access":"read_only", "depends_on":["one","two","three"],
                    "route":{"adapter":"codex","model":"gpt-6-luna"}}
            ]
        }))?;
        let mut journal = graph::FileJournal::open(&fixture.roots.state, fixture.settled.project_id,
            &document.graph_id, budget, &cancel, &factory.clock).map_err(|error| format!("open journal: {error}"))?;
        journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(),
            integration_base: None, directory: fixture.rig.project.clone(), document: document.clone() })?;
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None, integration_branch: None,
            network: None, capability_overrides: Default::default(), exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false };
        let catalogue = Catalogue::bundled()?;
        let models = ["codex/gpt-6-luna", "claude/claude-haiku-5-5", "agy/gemini-3.8-flash", "opencode/opencode-go/glm-5.3-flash"];
        let now = factory.clock.wall_now()?.unix_seconds();
        let availability: Vec<_> = models.iter().map(|id| {
            let (adapter, model) = id.split_once('/').ok_or("model")?;
            Ok::<_, Box<dyn std::error::Error>>(provenant_providers::AvailabilityRecord {
                schema_version: 1, adapter: adapter.into(), executable: "/fixture/provider".into(),
                version: "fixture".into(), fixture_ref: "fixture".into(), catalogue_revision: catalogue.revision().into(),
                account_key: Some(format!("{adapter}-account")), observed_models: vec![model.into()],
                observed_unix_seconds: now, expires_unix_seconds: now + 300, last_update: None,
            })
        }).collect::<Result<_, _>>()?;
        let access: Vec<_> = ["codex", "claude", "agy", "opencode"].iter().map(|adapter|
            AccessContext { adapter, disclosure_ref: "fixture", confinement_ref: "fixture" }).collect();
        let mut picked: std::collections::BTreeMap<String, Vec<(String, String)>> = Default::default();
        let mut published = std::collections::BTreeSet::new();
        let run = loop {
            let run = provenant_cli::run::with_deferred_provider_launch(|| {
                graph::advance_with_budget(&document, &mut owner, &mut journal, &Default::default(), &Default::default())
            }).map_err(|error| format!("advance: {error}"))?;
            if run.checkpoint.successful() { break run; }
            let mut progressed = false;
            for node in &run.checkpoint.nodes {
                let Some(attempt) = node.attempts.last() else { continue };
                let Some(reference) = attempt.attempt_ref.as_ref() else { continue };
                if !published.insert(reference.clone()) { continue; }
                let (_, id) = reference.split_once('/').ok_or("attempt reference")?;
                let record = fixture.store.record(id.parse()?, &call)?;
                let operation = fixture.store.operation(&graph_operation("graph-attempt-operation", &attempt.key)?, &call)?.ok_or("operation")?;
                let request = fixture.store.request(operation.run_id, &call)?;
                let task = request.tasks.first().ok_or("task")?;
                let model = match &task.route {
                    RequestedRoute::Model { model, .. } => model.clone(),
                    RequestedRoute::Review { name, effort, excluded_families, preferred_families, require_distinct, distinct_families, .. } => {
                        assert!(*require_distinct && *distinct_families);
                        let peers: Vec<_> = ["one", "two", "three"].into_iter().take_while(|id| *id != node.id)
                            .flat_map(|id| picked.get(id).into_iter().flatten().map(|(family, _)| family.clone()))
                            .fold(Vec::new(), |mut out, family| { if !out.contains(&family) { out.push(family); } out });
                        let mut expected = vec!["openai".to_owned()];
                        expected.extend(peers.iter().cloned());
                        expected.sort();
                        let mut excluded = excluded_families.clone();
                        excluded.sort();
                        assert_eq!(excluded, expected);
                        assert_eq!(preferred_families, &peers);
                        let prior: Vec<String> = task.profile.overrides.iter().find_map(|value|
                            value.strip_prefix("routing.graph_exclude=")).map(serde_json::from_str).transpose()?.unwrap_or_default();
                        let rules: Vec<_> = prior.iter().map(|model| Ok(ExclusionRule {
                            selector: ExclusionSelector::Model(ModelId::parse(model)?), source: ExclusionSource::Graph,
                        })).collect::<Result<_, provenant_routing::RoutingError>>()?;
                        let route = RouteRequest { intent: provenant_routing::RouteIntent { workload: provenant_routing::Workload::Review, ..Default::default() }, choice: RouteChoice::NamedRoute(RouteName::parse(name)?),
                            effort: effort.clone(), confidential: false, fallback: Fallback::None };
                        let selected = provenant_cli::route::fixture_resolve_dispatch(&route, &fixture.roots, None,
                            (operation.operation_id.as_str(), 0), &availability, &access, "fixture",
                            Some((excluded_families, *require_distinct, preferred_families)), &rules)?;
                        let model = format!("{}/{}", selected.adapter, selected.model);
                        let family = provenant_cli::run::model_family(&model).ok_or("model family")?;
                        assert_ne!(family, "openai");
                        assert!(!prior.contains(&model));
                        let eligible_unused = models.iter().find_map(|id| {
                            let family = provenant_cli::run::model_family(id)?;
                            (family != "openai" && !peers.contains(&family) && !prior.iter().any(|p| p == id)).then_some(family)
                        });
                        if eligible_unused.is_some() { assert!(!peers.contains(&family), "sibling preference ignored"); }
                        picked.entry(node.id.clone()).or_default().push((family, model.clone()));
                        model
                    }
                    route => return Err(format!("unexpected panel route: {route:?}").into()),
                };
                let outcome = Outcome::Succeeded;
                // The provider boundary is synthetic; model selection above is
                // the production dispatch resolver using the actual request.
                publish_routed(&fixture.rig, &fixture.store, &record, head, outcome, &model, &call)?;
                let applied = fixture.store.record(record.attempt_id, &call)?.applied_route.ok_or("applied route")?;
                assert_eq!(applied.model.as_deref(), Some(model.as_str()));
                if let Some(family) = provenant_cli::run::model_family(&model) {
                    assert_eq!(owner.attempt_families(std::slice::from_ref(reference))?, [family]);
                }
                progressed = true;
            }
            assert!(progressed, "panel did not advance: {:#?}", run.checkpoint.nodes);
        };
        assert_eq!(run.checkpoint.nodes[1].attempts.len(), 1);
        assert_eq!(run.checkpoint.nodes[1].attempts[0].outcome, Some(graph::AttemptOutcome::Succeeded));
        assert_eq!(picked.len(), 3);
        assert_eq!(run.checkpoint.nodes[4].state, graph::NodeState::Succeeded);
        Ok(())
    }

    #[test]
    fn graph_owner_submits_sibling_families_as_a_preference_and_alias_retry_exclusions()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_owner_submits_sibling_families_as_a_preference_and_alias_retry_exclusions",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        use provenant_records::run::RequestedRoute;
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut bound = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
            factory: &factory,
            integration_base: None,
            integration_branch: None,
            network: None, capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let actor = bound.actor();
        let owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store(&cancel)?;
        let call = owner.call(&cancel);
        let upstream = provenant_run::Resume {
            prior: fixture.settled.attempt_id, expected_generation: 0,
            operation_id: "family-composition-writer".parse()?,
            run_id: RunId::from_bytes([71; 16]), attempt_id: AttemptId::from_bytes([72; 16]),
            prompt: "writer".into(), created_at: factory.clock.wall_now()?,
        };
        fixture.store.resume_fresh(&upstream, "fixture", &call)?;
        let record = fixture.store.record(upstream.attempt_id, &call)?;
        let head = fixture.store.record(fixture.settled.attempt_id, &call)?.head.ok_or("head")?;
        publish_routed(&fixture.rig, &fixture.store, &record, head, Outcome::Succeeded, "codex/gpt-6-luna", &call)?;
        let writer = format!("{}/{}", factory.context.identity.host, upstream.attempt_id);
        let alias: graph::GraphRoute = serde_json::from_value(serde_json::json!({"alias":"junior"}))?;
        // Submit one lens attempt through the production owner and return the
        // route and knobs it recorded.
        let mut submit = |node_id: &str,
                          route_index: u16,
                          peers: &[String],
                          prior: &[String],
                          require: bool, distinct: &[String]|
         -> Result<(RequestedRoute, Vec<String>), Box<dyn std::error::Error>> {
            let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
                "graph_id":"sibling-lenses", "node_id":node_id, "route_index":route_index
            }))?;
            bound.observe_or_start(&key, graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: None, require_skill_location: false, settlement_retries: None, review_base: None,
                prior_attempts: prior, family_peers: peers, context: graph_test_context(&key), read_roots: &[],
                distinct_from_attempts: distinct, require_distinct: require, source_commit: None,
                title: "lens", prompt: "review", required_verdict: None, deadline_seconds: None,
                inputs_dir: None,
            }, &alias, graph::AccessMode::ReadOnly, None, graph::Replay {
                delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
            })?;
            let operation = graph_operation("graph-attempt-operation", &key)?;
            let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("lens operation missing")?;
            let request = store.request(operation.run_id, &owner.call(&cancel))?;
            let task = request.tasks.first().ok_or("task missing")?;
            Ok((task.route.clone(), task.profile.overrides.clone()))
        };
        let review = |excluded: &[&str]| RequestedRoute::Review {
            extra: Default::default(),
            name: "junior".into(),
            effort: None,
            excluded_families: excluded.iter().map(|family| (*family).to_owned()).collect(),
            distinct_families: true,
            preferred_families: vec![],
            require_distinct: false,
        };
        // The first sibling has no peers yet and takes the plain alias.
        let (route, _) = submit("one", 0, &[], &[], false, &[])?;
        assert_eq!(route, RequestedRoute::Named { extra: Default::default(), name: "junior".into(), effort: None });
        // Later siblings prefer other families. A preference never refuses, so
        // with one family available every lens still runs.
        let peers = ["openai".to_owned(), "anthropic".to_owned()];
        assert_eq!(submit("two", 0, &peers[..1], &[], false, &[])?.0, review(&["openai"]));
        assert_eq!(submit("three", 0, &peers, &[], false, &[])?.0, review(&["openai", "anthropic"]));
        // A required writer separation with no writer family to enforce still
        // leaves sibling families as a preference.
        assert_eq!(submit("four", 0, &peers[..1], &[], true, &[])?.0, review(&["openai"]));
        let accepted = submit("five", 0, &peers[1..], &[], true, std::slice::from_ref(&writer))?.0;
        assert_eq!(accepted, RequestedRoute::Review {
            extra: Default::default(), name: "junior".into(), effort: None,
            excluded_families: vec!["openai".into()], distinct_families: true,
            preferred_families: vec!["anthropic".into()], require_distinct: true,
        });
        // An accepted operation without a checkpoint reference retains its
        // original route even if the newly observed peers have changed.
        assert_eq!(submit("five", 0, &[], &[], true, &[writer])?.0, accepted);
        // An alias retry excludes the model ids the runtime resolved.
        let (route, knobs) = submit("two", 1, &peers[..1], &["codex/first".to_owned()], false, &[])?;
        assert_eq!(route, review(&["openai"]));
        assert_eq!(knobs, [r#"routing.graph_exclude=["codex/first"]"#]);
        Ok(())
    }

    #[test]
    fn graph_submission_refuses_a_push_to_main_before_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_submission_refuses_a_push_to_main_before_admission",
        )? {
            return Ok(());
        }
        use provenant_confinement::worktrees::{self, WorktreeCreate};
        use provenant_fabric_runtime::graph::FileJournal;
        let fixture = SettlementFixture::resumed()?;
        let cancel = CancelToken::new();
        let info = worktrees::with_context(
            &fixture.roots,
            &fixture.rig.home,
            &fixture.rig.project,
            &SystemClock,
            &cancel,
            |context| {
                worktrees::create(
                    &worktrees::WorktreeContext {
                        workers: &FreeWorkers,
                        ..*context
                    },
                    WorktreeCreate {
                        branch: "main".into(),
                        base: Some("HEAD".into()),
                    },
                    &cancel,
                )
            },
        )?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join("protected-push-graph.json");
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":"protected-push",
                "nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write",
                    "worktree":info.path,"delivery":"push",
                    "route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?,
        )?;
        let result = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document,
            },
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
        );
        assert!(
            FileJournal::read_checkpoint(
                &fixture.roots.state,
                fixture.settled.project_id,
                "protected-push"
            )?
            .is_none(),
            "protected push was admitted: {result:?}"
        );
        assert!(
            matches!(result, Err(FacadeError::ConfinementRefused { .. })),
            "{result:?}"
        );
        Ok(())
    }

    fn integrate_graph(
        fixture: &SettlementFixture,
        id: &str,
        target: &str,
    ) -> Result<
        (
            Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
            bool,
        ),
        Box<dyn std::error::Error>,
    > {
        use provenant_fabric_runtime::graph::FileJournal;
        let cancel = CancelToken::new();
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join(format!("{id}-graph.json"));
        let route = serde_json::json!({"adapter":"fixture","model":"fixture/model"});
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":id,
                "nodes":[
                    {"id":"one","prompt":"reply OK","access":"worktree_write",
                        "worktree":fixture.worktree,"delivery":format!("integrate:{target}"),
                        "route":route},
                    // An unrelated writer on the same worktree that never integrates.
                    {"id":"two","prompt":"reply OK","access":"worktree_write",
                        "worktree":fixture.worktree,"depends_on":["one"],
                        "route":route}
                ]
            }))?,
        )?;
        let result = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document,
            },
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
        );
        let admitted = FileJournal::read_checkpoint(
            &fixture.roots.state,
            fixture.settled.project_id,
            id,
        )?
        .is_some();
        Ok((result, admitted))
    }

    #[test]
    fn graph_submission_refuses_a_worktree_on_its_integration_branch_before_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_submission_refuses_a_worktree_on_its_integration_branch_before_admission",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let (result, admitted) = integrate_graph(&fixture, "integrate-own-branch", "resumed-lane")?;
        assert!(!admitted, "graph was admitted: {result:?}");
        let Err(FacadeError::DispatchRefused { reason, .. }) = result else {
            return Err(format!("expected a dispatch refusal: {result:?}").into());
        };
        assert!(
            reason.contains("is on the integration branch resumed-lane")
                && reason.contains("git worktree add -b batch/work PATH BASE"),
            "{reason}"
        );
        Ok(())
    }

    #[test]
    fn graph_submission_admits_a_worktree_on_a_separate_branch()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_submission_admits_a_worktree_on_a_separate_branch")?
        {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let (result, admitted) = integrate_graph(&fixture, "integrate-separate", "batch/graph")?;
        assert!(admitted, "graph was refused: {result:?}");
        assert!(result.is_ok(), "{result:?}");
        Ok(())
    }

    #[test]
    fn bare_graph_miss_names_the_scratch_root_and_searches_no_legacy_root()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::bare_graph_miss_names_the_scratch_root_and_searches_no_legacy_root",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        // A readable document under the legacy root must not be found by a bare name.
        let legacy = fixture.rig.project.join(".agent-run/graphs");
        std::fs::create_dir_all(&legacy)?;
        std::fs::write(legacy.join("legacy-plan.json"), b"not a graph")?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let result = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document: "legacy-plan.json".into(),
            },
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &CancelToken::new(),
        );
        match result {
            Err(FacadeError::Io { operation, .. }) => {
                assert!(
                    operation.starts_with("read project-local graph document")
                        && operation.contains(".provenant/scratch/graphs")
                        && operation.contains("explicit path"),
                    "{operation}"
                );
            }
            other => panic!("bare-name miss must not read a legacy document: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn graph_submission_replays_an_accepted_push_after_protection_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_submission_replays_an_accepted_push_after_protection_changes",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join("replay-push-graph.json");
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":"replay-push", "template_library":{},
                "nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write",
                    "worktree":fixture.worktree,"delivery":"push",
                    "route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?,
        )?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let run = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document: document.clone(),
            },
            budget,
            &cancel,
        )?;
        assert_eq!(
            run.checkpoint.nodes[0].state,
            provenant_fabric_runtime::graph::NodeState::Failed
        );
        assert!(!run.checkpoint.nodes[0].attempts[0].launched);
        let git = provenant_os::git::git_executable().ok_or("git")?;
        for args in [
            vec![
                "remote",
                "add",
                "origin",
                fixture.rig.project.to_str().ok_or("project")?,
            ],
            vec![
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/resumed-lane",
            ],
        ] {
            let output = fixture.rig.execute(&git, &args, None)?;
            assert_eq!(crate::worktree_tests::support::code(&output), 0);
        }
        for input in [
            GraphInput::Run {
                extra: Default::default(),
                document: document.clone(),
            },
            GraphInput::Resume {
                extra: Default::default(),
                document,
                extend: vec![],
            },
        ] {
            let replay = factory.graph(input, budget, &cancel)?;
            assert_eq!(replay.checkpoint, run.checkpoint);
        }
        Ok(())
    }

    #[test]
    fn graph_submission_pins_the_integration_base_across_replay()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_submission_pins_the_integration_base_across_replay",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::FileJournal;
        let fixture = SettlementFixture::resumed()?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture
            .rig
            .execute(&git, &["branch", "integration"], None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        for args in [
            vec!["remote", "add", "origin", fixture.rig.project.to_str().ok_or("path")?],
            vec!["update-ref", "refs/heads/main", "HEAD"],
            vec!["update-ref", "refs/remotes/origin/main", "HEAD"],
        ] {
            let output = fixture.rig.execute(&git, &args, None)?;
            assert_eq!(crate::worktree_tests::support::code(&output), 0);
        }
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join("base-graph.json");
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":2, "graph_id":"pinned-base", "template_library":{}, "integration_branch":"integration", "review_base":"origin/main", "delivery":"pr",
                "nodes":[{"id":"one","prompt":"reply OK","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?,
        )?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        // The fixture adapter is unavailable, so no provider launches. The
        // first graph checkpoint must already contain its integration base.
        let run = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document: document.clone(),
            },
            budget,
            &cancel,
        )?;
        assert_eq!(
            run.checkpoint.nodes[0].state,
            provenant_fabric_runtime::graph::NodeState::Failed
        );
        assert!(!run.checkpoint.nodes[0].attempts[0].launched);
        let owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store(&cancel)?;
        let operation = graph_operation(
            "graph-attempt-operation",
            &run.checkpoint.nodes[0].attempts[0].key,
        )?;
        let operation = store
            .operation(&operation, &owner.call(&cancel))?
            .ok_or("graph run not admitted")?;
        let index_call = provenant_fabric_runtime::chairs::Request {
            clock: &factory.clock,
            cancel: &cancel,
        };
        let binding = provenant_fabric_runtime::chairs::ChairIndex::open(
            &factory.context.roots.state,
            &index_call,
        )?
        .read(
            factory.context.identity.project,
            operation.run_id,
            &index_call,
        )?
        .ok_or("graph index missing")?;
        assert!(binding.chair.is_none(), "graph created a chair association");
        let base = {
            let journal = FileJournal::open(
                &fixture.roots.state,
                fixture.settled.project_id,
                "pinned-base",
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.integration_base().ok_or("graph base missing")?
        };
        assert_eq!(
            Some(base),
            fixture
                .store
                .record(
                    fixture.settled.attempt_id,
                    &provenant_run::Request {
                        clock: &factory.clock,
                        cancel: &cancel,
                    }
                )?
                .head
        );
        let output = fixture.rig.execute(&git, &["branch", "old-review", &base.to_string()], None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        std::fs::write(fixture.rig.project.join("guide.md"), "small change\n")?;
        let output = fixture.rig.execute(&git, &["add", "guide.md"], None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        for args in [
            vec![
                "commit",
                "--allow-empty",
                "-qm",
                "Unrelated primary checkout",
            ],
            vec!["branch", "-f", "integration", "HEAD"],
            vec!["update-ref", "refs/heads/main", "HEAD"],
            vec!["update-ref", "refs/remotes/origin/main", "HEAD"],
        ] {
            let output = fixture.rig.execute(&git, &args, None)?;
            assert_eq!(crate::worktree_tests::support::code(&output), 0);
        }
        let replay = factory.graph(
            GraphInput::Resume {
                extra: Default::default(),
                document,
                extend: vec![],
            },
            budget,
            &cancel,
        )?;
        assert_eq!(replay.checkpoint, run.checkpoint);
        let journal = FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            "pinned-base",
            budget,
            &cancel,
            &factory.clock,
        )?;
        assert_eq!(journal.integration_base(), Some(base));
        let execution = FileJournal::execution(&fixture.roots.state, fixture.settled.project_id, "pinned-base")?.ok_or("execution missing")?;
        assert_eq!(execution.review_base, Some(base));
        drop(journal);
        let head_output = fixture.rig.execute(&git, &["rev-parse", "HEAD"], None)?;
        let head = String::from_utf8(head_output.stdout.bytes)?.trim().to_owned();
        // A later lens admitted after the reference moved still reads the pinned
        // review base, independently of the integration base.
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"pinned-base", "node_id":"later-lens", "route_index":0
        }))?;
        let route = serde_json::from_value(serde_json::json!({"adapter":"fixture", "model":"fixture/model"}))?;
        let mut bound = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
            factory: &factory, integration_base: None, integration_branch: None,
            network: None, capability_overrides: Default::default(), exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false,
        };
        let actor = bound.actor();
        bound.observe_or_start(&key, graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: None, require_skill_location: false, settlement_retries: None, review_base: None,
            prior_attempts: &[], family_peers: &[], context: graph_test_context(&key), read_roots: &[],
            distinct_from_attempts: &[], require_distinct: false, source_commit: Some(&head),
            title: "later lens", prompt: "review", required_verdict: None, deadline_seconds: None,
            inputs_dir: None,
        }, &route, graph::AccessMode::ReadOnly, None, graph::Replay {
            delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
        })?;
        let operation = graph_operation("graph-attempt-operation", &key)?;
        let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("later lens operation missing")?;
        let request = store.request(operation.run_id, &owner.call(&cancel))?;
        let base_text = base.to_string();
        assert_eq!(request.tasks[0].base.as_deref(), Some(base_text.as_str()));
        assert_ne!(base_text, head);
        // A different integration base does not displace the pinned review base.
        bound.integration_base = Some(BoundGraphBases {
            integration: Some(provenant_core::GitOid::parse(&head)?),
            review: Some(base),
        });
        let second: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"pinned-base", "node_id":"second-lens", "route_index":0
        }))?;
        let actor = bound.actor();
        bound.observe_or_start(&second, graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: None, require_skill_location: false, settlement_retries: None, review_base: None,
            prior_attempts: &[], family_peers: &[], context: graph_test_context(&second), read_roots: &[],
            distinct_from_attempts: &[], require_distinct: false, source_commit: Some(&head),
            title: "second lens", prompt: "review", required_verdict: None, deadline_seconds: None,
            inputs_dir: None,
        }, &route, graph::AccessMode::ReadOnly, None, graph::Replay {
            delivery: None, branch: None, fast_forward_only: false, stored_ref: None, launch_actor: &actor,
        })?;
        let operation = graph_operation("graph-attempt-operation", &second)?;
        let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("second lens operation missing")?;
        let request = store.request(operation.run_id, &owner.call(&cancel))?;
        assert_eq!(request.tasks[0].base.as_deref(), Some(base_text.as_str()));
        bound.integration_base = Some(BoundGraphBases { integration: None, review: Some(base) });
        for (id, reference, expected) in [("review-base", "old-review", base_text.as_str()), ("empty-review-base", "", head.as_str())] {
            let path = fixture.rig.project.join(format!("{id}.json"));
            std::fs::write(&path, serde_json::to_vec(&serde_json::json!({
                "schema_version":2, "graph_id":id, "template_library":{}, "review_base":reference,
                "nodes":[{"id":"lens","prompt":"review","access":"read_only", "source":{"type":"commit","commit":head}, "route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?)?;
            let result = factory.graph(GraphInput::Run { extra: Default::default(), document: path }, budget, &cancel)?;
            let operation = graph_operation("graph-attempt-operation", &result.checkpoint.nodes[0].attempts[0].key)?;
            let operation = store.operation(&operation, &owner.call(&cancel))?.ok_or("operation missing")?;
            let request = store.request(operation.run_id, &owner.call(&cancel))?;
            assert_eq!(request.tasks[0].base.as_deref(), Some(expected));
            assert_eq!(request.tasks[0].head.as_deref(), Some(head.as_str()));
        }
        Ok(())
    }

    #[test]
    fn detached_writer_reports_branch_requirement() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::detached_writer_reports_branch_requirement")? { return Ok(()); }
        let fixture = SettlementFixture::resumed()?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture.rig.execute(&git, &["-C", fixture.worktree.to_str().ok_or("path")?, "checkout", "--detach"], None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let factory = Factory { context: Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.rig.project)?, project_attribution: false, clock: SystemClock, backoff: ThreadPause, executor: SystemExecutor::new(1)?, home: fixture.rig.home.clone(), cwd: fixture.rig.project.clone(), assets: fixture.rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers };
        let document = fixture.rig.project.join("detached-writer.json");
        std::fs::write(&document, serde_json::to_vec(&serde_json::json!({"schema_version":1,"graph_id":"detached-writer", "template_library":{},"nodes":[{"id":"writer","prompt":"write","access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]}))?)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let run = factory.graph(GraphInput::Run { extra: Default::default(), document }, budget, &cancel)?;
        let node = &run.checkpoint.nodes[0];
        assert!(node.advance_error.as_deref().is_some_and(|error| error.contains("writer needs a branch")), "{node:#?}");
        let detached_factory = Factory { context: Context::open(fixture.roots.clone(), &fixture.rig.home, &fixture.worktree)?, project_attribution: false, clock: SystemClock, backoff: ThreadPause, executor: SystemExecutor::new(1)?, home: fixture.rig.home.clone(), cwd: fixture.worktree.clone(), assets: fixture.rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers };
        let head = fixture.rig.execute(&git, &["-C", fixture.worktree.to_str().ok_or("path")?, "rev-parse", "HEAD"], None)?;
        let head = String::from_utf8(head.stdout.bytes)?.trim().to_owned();
        let document = fixture.worktree.join("detached-review.json");
        std::fs::write(&document, serde_json::to_vec(&serde_json::json!({"schema_version":1,"graph_id":"detached-review", "template_library":{},"nodes":[{"id":"review","prompt":"review","access":"read_only","source":{"type":"commit","commit":head},"route":{"adapter":"fixture","model":"fixture/model"}}]}))?)?;
        let budget = WaitBudget::cli(detached_factory.clock.mono_now(), 30)?;
        let run = detached_factory.graph(GraphInput::Run { extra: Default::default(), document }, budget, &cancel)?;
        let node = &run.checkpoint.nodes[0];
        assert!(node.advance_error.is_none(), "{node:#?}");
        let key = &node.attempts.first().ok_or("review attempt missing")?.key;
        let operation = graph_operation("graph-attempt-operation", key)?;
        let owner = BinaryRunOwner { factory: &detached_factory, allow_remote: true, actor: None, chair: None };
        assert!(owner.store(&cancel)?.operation(&operation, &owner.call(&cancel))?.is_some(), "review was not submitted");
        Ok(())
    }

    #[test]
    fn retained_lane_settlement_collects_only_its_output_and_cache()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::retained_lane_settlement_collects_only_its_output_and_cache",
        )? {
            return Ok(());
        }
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let fixture = SettlementFixture::resumed()?;
        std::fs::write(
            fixture.rig.project.join(".git/info/exclude"),
            "build/\nsaved/\n",
        )?;
        std::fs::write(
            fixture.rig.state.join("config.toml"),
            format!(
                "schema_version=1\n[gc.projects.\"{}\"]\nregenerable=['build','saved']\nkeep=['saved']\nforeign_worktrees='merged-clean'\n",
                fixture.settled.project_id
            ),
        )?;
        std::fs::create_dir(fixture.worktree.join("build"))?;
        std::fs::write(fixture.worktree.join("build/artifact"), "regenerable")?;
        std::fs::create_dir(fixture.worktree.join("saved"))?;
        std::fs::write(fixture.worktree.join("saved/keep"), "keep")?;
        let other = fixture.rig.project.join(".worktrees/foreign");
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture.rig.execute(
            &git,
            &[
                "worktree",
                "add",
                "-b",
                "foreign",
                other.to_str().ok_or("path")?,
            ],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        std::fs::create_dir(other.join("build"))?;
        std::fs::write(other.join("build/artifact"), "foreign")?;
        let cache_root = fixture.rig.home.join("Library/Caches/provenant-lane-cargo");
        // Use valid cache ownership markers, including an unrelated orphan.
        let cache = cache_root.join("a".repeat(64));
        let orphan = cache_root.join("b".repeat(64));
        let identity = std::fs::metadata(&fixture.worktree)?;
        for (path, owner) in [
            (&cache, fixture.worktree.clone()),
            (&orphan, fixture.rig.root.join("missing")),
        ] {
            std::fs::create_dir_all(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            let mut marker = b"provenant-lane-cargo-owner-v2\n".to_vec();
            marker.extend_from_slice(&identity.dev().to_le_bytes());
            marker.extend_from_slice(&identity.ino().to_le_bytes());
            marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
            std::fs::write(path.join(".provenant-worktree-owner"), marker)?;
            std::fs::write(path.join("artifact"), "cached")?;
        }
        let delivery = provenant_run::WriterDeliveryReceipt {
            extra: Default::default(),
            schema_version: 1,
            remote: "origin".into(),
            branch: "resumed-lane".into(),
            head: None,
            review_acceptance: None,
            state: provenant_run::WriterDeliveryState::NotAttempted {
                extra: Default::default(),
            },
        };
        let collection = crate::worktrees::collection_lock(&fixture.roots)?;
        settle_owned_worktree(
            &fixture.settled,
            &fixture.roots,
            &FreeWorkers,
            Some(&delivery),
        )?;
        assert!(fixture.worktree.join("build/artifact").exists());
        assert!(cache.exists());
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let busy = fixture
            .store
            .worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("missing busy settlement receipt")?;
        assert_eq!(busy["state"], "retained", "{busy}");
        assert_eq!(busy["reason"], crate::worktrees::COLLECTION_BUSY);
        drop(collection);
        let state = provenant_os::fs::Directory::open_anchor(
            &fixture
                .rig
                .state
                .join("worktrees")
                .join(fixture.settled.project_id.to_string())
                .join(provenant_confinement::worktrees::state_dir_name(
                    &fixture.worktree,
                )),
        )?;
        let writer = state.lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
            WaitBudget::nonblocking(SystemClock.mono_now()),
            &CancelToken::new(),
            &SystemClock,
        )?;
        settle_owned_worktree(
            &fixture.settled,
            &fixture.roots,
            &FreeWorkers,
            Some(&delivery),
        )?;
        assert!(fixture.worktree.join("build/artifact").exists());
        assert!(cache.exists());
        drop(writer);
        for _ in 0..2 {
            settle_owned_worktree(
                &fixture.settled,
                &fixture.roots,
                &FreeWorkers,
                Some(&delivery),
            )?;
            assert!(fixture.worktree.exists());
            assert!(!fixture.worktree.join("build/artifact").exists());
            assert!(!cache.exists(), "settled lane cache survived");
            assert_eq!(
                std::fs::read_to_string(fixture.worktree.join("saved/keep"))?,
                "keep"
            );
            assert_eq!(
                std::fs::read_to_string(other.join("build/artifact"))?,
                "foreign"
            );
            assert!(orphan.exists(), "unrelated cache collected");
        }
        Ok(())
    }

    #[test]
    fn explicit_writer_refreshes_tracking_ref_while_collection_is_busy()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::explicit_writer_refreshes_tracking_ref_while_collection_is_busy",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let owner_path = worktrees::with_context(
            &fixture.roots,
            &fixture.rig.home,
            &fixture.rig.project,
            &clock,
            &cancel,
            |context| {
                let tree = provenant_confinement::verify_worktree(context.repo, &fixture.worktree)?;
                Ok(tree.repo().private_git_dir().join("provenant-owner.json"))
            },
        )?;
        std::fs::remove_file(owner_path)?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let remote = fixture.rig.root.join("remote.git");
        for args in [
            vec!["init", "--bare", remote.to_str().ok_or("remote path")?],
            vec![
                "remote",
                "add",
                "origin",
                remote.to_str().ok_or("remote path")?,
            ],
            vec![
                "push",
                "origin",
                "refs/heads/resumed-lane:refs/heads/resumed-lane",
            ],
            vec!["update-ref", "-d", "refs/remotes/origin/resumed-lane"],
        ] {
            let output = fixture.rig.execute(&git, &args, None)?;
            assert_eq!(
                crate::worktree_tests::support::code(&output),
                0,
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
        }
        let collection = crate::worktrees::collection_lock(&fixture.roots)?;
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let output = fixture.rig.execute(
            &git,
            &["rev-parse", "--verify", "refs/remotes/origin/resumed-lane"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        drop(collection);
        Ok(())
    }

    #[test]
    fn adopted_writer_refresh_failure_does_not_change_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::adopted_writer_refresh_failure_does_not_change_receipt")?
        {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        worktrees::with_context(
            &fixture.roots,
            &fixture.rig.home,
            &fixture.rig.project,
            &clock,
            &cancel,
            |context| {
                let tree = provenant_confinement::verify_worktree(context.repo, &fixture.worktree)?;
                let mut owner = provenant_confinement::worktrees::load_owner(&tree)?
                    .ok_or(provenant_confinement::worktrees::WorktreeError::InvalidOutput)?;
                owner.origin = provenant_confinement::worktrees::WorktreeOrigin::Adopted;
                provenant_confinement::worktrees::save_owner(&tree, &owner)?;
                Ok(())
            },
        )?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture.rig.execute(
            &git,
            &["remote", "add", "origin", "/nonexistent/provenant-remote"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        assert!(
            fixture
                .store
                .worktree_settlement(fixture.settled.attempt_id, &call)?
                .is_none()
        );
        let receipt = serde_json::json!({"schema_version":1, "state":"retained", "worktree":fixture.worktree, "reason":"existing reason"});
        record_worktree_settlement(&fixture.store, fixture.settled.attempt_id, &receipt)?;
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        assert_eq!(
            fixture
                .store
                .worktree_settlement(fixture.settled.attempt_id, &call)?,
            Some(receipt)
        );
        Ok(())
    }

    #[test]
    fn settlement_records_tracking_ref_failure_without_failing_lane()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::settlement_records_tracking_ref_failure_without_failing_lane",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture.rig.execute(
            &git,
            &["remote", "add", "origin", "/nonexistent/provenant-remote"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let receipt = fixture
            .store
            .worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("settlement")?;
        assert!(receipt["tracking_ref_warning"].is_string(), "{receipt}");
        assert_eq!(receipt["state"], "pruned", "{receipt}");
        Ok(())
    }

    #[test]
    fn unmatched_graph_writer_keeps_its_worktree() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::unmatched_graph_writer_keeps_its_worktree")? {
            return Ok(());
        }
        for corrupt in [false, true] {
            let fixture = if corrupt {
                SettlementFixture::settled_fixture("graph-unmatched", Some("broken"))?
            } else {
                SettlementFixture::resumed_as("graph-unmatched")?
            };
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = provenant_run::Request { clock: &clock, cancel: &cancel };
            if corrupt {
                drop(provenant_fabric_runtime::graph::FileJournal::open(
                    &fixture.roots.state, fixture.settled.project_id, "broken",
                    call.lock_budget(), &cancel, &clock,
                )?);
                let path = fixture.roots.state.as_path().join("fabric/graphs")
                    .join(fixture.settled.project_id.to_string()).join("graph-broken.json");
                std::fs::write(path, b"invalid checkpoint")?;
            }
            settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
            let receipt = fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?
                .ok_or("missing settlement")?;
            assert_eq!(receipt["state"], "retained");
            assert!(receipt["reason"].as_str().is_some_and(|reason| !reason.is_empty()));
            assert!(fixture.worktree.exists());
        }
        Ok(())
    }

    #[test]
    fn resumed_lane_settlement_ignores_terminal_graph()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::resumed_lane_settlement_ignores_terminal_graph")?
        {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, GraphJournal};
        let fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"terminal-unrelated","nodes":[{"id":"one","prompt":"work",
                "access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
        }))?;
        let checkpoint: graph::GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"terminal-unrelated","plan_digest":"digest",
            "nodes":[{"id":"one","state":"succeeded","attempts":[]}]
        }))?;
        let mut journal = graph::FileJournal::open(&fixture.roots.state, fixture.settled.project_id,
            "terminal-unrelated", WaitBudget::cli(clock.mono_now(), 10)?, &cancel, &clock)?;
        journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
            directory: fixture.rig.project.clone(), document })?;
        journal.save(&checkpoint)?;
        drop(journal);
        graph::take_checkpoint_read_ids();
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        assert!(graph::take_checkpoint_read_ids().is_empty());
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let receipt = fixture
            .store
            .worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("missing resumed settlement receipt")?;
        assert_eq!(receipt["state"], "pruned", "{receipt}");
        assert!(!fixture.worktree.exists());
        assert!(
            fixture
                .rig
                .state
                .join("runs")
                .join(fixture.settled.run_id.to_string())
                .join("attempts")
                .join(fixture.settled.attempt_id.to_string())
                .join("worktree-settlement.json")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn resumed_lane_settlement_prunes_and_records_its_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::resumed_lane_settlement_prunes_and_records_its_receipt")?
        {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 10)?;
        drop(provenant_fabric_runtime::graph::FileJournal::open(
            &fixture.roots.state, fixture.settled.project_id, "unrelated", budget, &cancel, &clock,
        )?);
        std::fs::write(fixture.roots.state.as_path().join("fabric/graphs")
            .join(fixture.settled.project_id.to_string()).join("graph-unrelated.json"), b"invalid checkpoint")?;
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let receipt = fixture
            .store
            .worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("missing resumed settlement receipt")?;
        assert_eq!(receipt["state"], "pruned", "{receipt}");
        assert!(!fixture.worktree.exists());
        assert!(
            fixture
                .rig
                .state
                .join("runs")
                .join(fixture.settled.run_id.to_string())
                .join("attempts")
                .join(fixture.settled.attempt_id.to_string())
                .join("worktree-settlement.json")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn deliver_none_does_not_try_to_settle_a_missing_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        struct NoWorkers;
        impl provenant_confinement::worktrees::WorkerObserver for NoWorkers {
            fn observe_roots(
                &self,
                _: &[std::path::PathBuf],
            ) -> std::collections::BTreeMap<
                std::path::PathBuf,
                provenant_confinement::worktrees::Occupancy,
            > {
                std::collections::BTreeMap::new()
            }
        }
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let assets = root.join("assets");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&assets)?;
        let roots = crate::roots::child(&home, &assets, root.join("state"))?;
        let edge = provenant_cli::run::Edge {
            roots: &roots,
            home: &home,
            assets: &assets,
            cwd: &assets,
        };
        let mut reply = provenant_cli::run::Reply {
            result: provenant_records::CommandResult::new(
                "lane deliver",
                provenant_records::Outcome::Ok,
                serde_json::json!({"attempt_id":"unused", "deliver":"none"}),
            ),
            human: "delivery disabled\n".into(),
            summary: None,
            read_only: false,
        };
        super::settle_delivered_attempt(&edge, &mut reply, &NoWorkers)?;
        assert_eq!(reply.human, "delivery disabled\n");
        assert!(reply.result.data["worktree_settlement"].is_null());
        Ok(())
    }

    #[test]
    fn unsuccessful_lanes_retain_the_worktree_even_after_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_run::{AttemptState, Outcome, WriterDeliveryReceipt, WriterDeliveryState};
        let delivered = WriterDeliveryReceipt {
            extra: Default::default(),
            schema_version: 1,
            remote: "origin".into(),
            branch: "lane".into(),
            head: None,
            review_acceptance: None,
            state: WriterDeliveryState::Delivered {
                extra: Default::default(),
                pull_request: None,
            },
        };
        for outcome in [
            Outcome::Failed,
            Outcome::RateLimited,
            Outcome::Cancelled,
            Outcome::TimedOut,
            Outcome::Rejected,
        ] {
            let state = AttemptState::Terminal {
                extra: Default::default(),
                outcome,
                exit_code: Some(1),
                finished_at: provenant_core::WallTime::from_micros(0)?,
                reason: None,
            };
            assert!(super::settlement_retention_reason(&state, false, None).is_some());
            assert!(super::settlement_retention_reason(&state, true, Some(&delivered)).is_some());
        }
        let succeeded = AttemptState::Terminal {
            extra: Default::default(),
            outcome: Outcome::Succeeded,
            exit_code: Some(0),
            finished_at: provenant_core::WallTime::from_micros(0)?,
            reason: None,
        };
        assert!(super::settlement_retention_reason(&succeeded, false, None).is_none());
        assert!(super::settlement_retention_reason(&succeeded, true, Some(&delivered)).is_none());
        assert!(super::settlement_retention_reason(&succeeded, true, None).is_some());
        let failed_delivery = WriterDeliveryReceipt {
            state: WriterDeliveryState::NotAttempted {
                extra: Default::default(),
            },
            ..delivered
        };
        assert!(
            super::settlement_retention_reason(&succeeded, true, Some(&failed_delivery)).is_some()
        );
        Ok(())
    }

    #[test]
    fn settlement_intent_replays_a_prune_after_a_crash() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let worktree = temp.path().join("lane");
        std::fs::create_dir(&worktree)?;
        let intent = serde_json::json!({
            "schema_version":1, "state":"pruning", "worktree":worktree
        });
        assert!(super::replay_worktree_settlement(Some(&intent), &worktree)?.is_none());
        std::fs::remove_dir(&worktree)?;
        let receipt = super::replay_worktree_settlement(Some(&intent), &worktree)?
            .ok_or("missing recovered prune receipt")?;
        assert_eq!(receipt["state"], "pruned");
        assert_eq!(receipt["recovered_from_intent"], true);
        assert_eq!(
            super::replay_worktree_settlement(Some(&receipt), &worktree)?,
            Some(receipt)
        );
        assert!(
            super::replay_worktree_settlement(Some(&intent), &temp.path().join("other"))?.is_none()
        );
        let retained = serde_json::json!({"state":"retained", "worktree":worktree});
        assert!(super::replay_worktree_settlement(Some(&retained), &worktree)?.is_none());
        Ok(())
    }

    #[test]
    fn submit_io_keeps_the_inner_operation_without_inventing_a_path() {
        let error = super::scope_io(super::FacadeError::io("access Fabric database"), "submit");
        assert_eq!(
            error,
            super::FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
                operation: "submit: access Fabric database".into(),
                path: None,
            }
        );
    }

    #[test]
    fn submit_io_retains_single_prefix_for_peer_error_and_scopes_local_error() {
        let peer_error = super::FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
            operation: "submit: open writer repository".into(),
            path: Some("/repo".into()),
        };
        let rescoped = super::scope_io(peer_error.clone(), "submit");
        assert_eq!(
            rescoped,
            super::FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
                operation: "submit: open writer repository".into(),
                path: Some("/repo".into()),
            }
        );

        let local_error = super::FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
            operation: "open writer repository".into(),
            path: Some("/repo".into()),
        };
        let scoped = super::scope_io(local_error, "submit");
        assert_eq!(
            scoped,
            super::FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
                operation: "submit: open writer repository".into(),
                path: Some("/repo".into()),
            }
        );
    }

    #[test]
    fn writer_repository_open_failure_hides_unanchored_project_path()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let request = provenant_run::WriterRequest {
            extra: Default::default(),
            project: root.clone(),
            worktree: root.join("writer"),
        };
        let project = provenant_core::ProjectId::from_bytes([1; 16]);
        let error = match provenant_run::prepare_writer(&request, &root, project) {
            Ok(_) => return Err("non-repository unexpectedly opened".into()),
            Err(error) => super::run_error(error),
        };
        assert_eq!(
            serde_json::to_value(error)?,
            serde_json::json!({
                "code": "io",
                "operation": "I/O operation",
                "path": null,
            })
        );
        Ok(())
    }

    #[test]
    fn typed_run_errors_keep_their_codes() {
        for (error, expected) in [
            (provenant_run::RunError::Nonterminal, "not_terminal"),
            (
                provenant_run::RunError::OperationConflict,
                "operation_conflict",
            ),
            (provenant_run::RunError::Conflict, "conflict"),
            (
                provenant_run::RunError::ProviderStateExpired,
                "provider_state_expired",
            ),
            // The CLI and MCP share one categorisation.
            (
                provenant_run::RunError::Validation("input"),
                "invalid_input",
            ),
            (
                provenant_run::RunError::ArtifactMismatch(provenant_run::ArtifactProvenance {
                    check: provenant_run::ArtifactCheck::RecordedDigest,
                    artifact: "result.md".into(),
                    expected: None,
                    actual: None,
                }),
                "state_corrupt",
            ),
            (provenant_run::RunError::Refused, "confinement_refused"),
            (provenant_run::RunError::Capacity, "busy"),
        ] {
            assert_eq!(super::run_error(error).code(), expected);
        }
    }
    #[test]
    fn peer_run_errors_stay_typed_through_cli_rendering() -> Result<(), Box<dyn std::error::Error>>
    {
        for error in [
            super::FacadeError::NotTerminal,
            super::FacadeError::OperationConflict,
            super::FacadeError::OwnerUnavailable,
            super::FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
                operation: "openat directory".into(),
                path: Some("/state/worktrees".into()),
            },
            super::FacadeError::WorktreeUnregistered {
                path: "/repo/.worktrees/writer".into(),
            },
        ] {
            let expected_code = error.code().to_owned();
            let response = super::peer_result::<()>(Err(error), |()| super::PeerResult::Error {
                data: provenant_fabric_runtime::hosts::PeerResultError {
                    extra: Default::default(),
                    error: super::FacadeError::io("test"),
                },
                extra: Default::default(),
            });
            let response: super::PeerResult =
                serde_json::from_slice(&serde_json::to_vec(&response)?)?;
            let super::PeerResult::Error {
                data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
                ..
            } = response
            else {
                return Err("expected error".into());
            };
            let (status, kind) = provenant_cli::fabric::facade_outcome(&error);
            let fail = provenant_cli::run::Fail {
                status,
                kind,
                message: error.to_string(),
                detail: Some(Box::new(error)),
                projection: None,
            };
            let result = super::fail_error(fail);
            assert_eq!(result.code(), expected_code);
            let encoded = serde_json::to_string(&result)?;
            assert!(!encoded.contains("/state/worktrees"));
            assert!(!encoded.contains("/repo/.worktrees/writer"));
        }
        Ok(())
    }
    #[test]
    fn submit_lane_refusal_maps_to_dispatch_refused() -> Result<(), Box<dyn std::error::Error>> {
        // A nonexistent read root is refused before any state tree is touched.
        let fail = provenant_cli::run::lane_access(
            &provenant_cli::Access {
        attach: Vec::new(),
                mode: None,
                worktree: None,
                read_root: vec!["/fixture/missing-root".into()],
                write_root: Vec::new(),
                network: None,
                capability: Vec::new(),
                no_capability: Vec::new(),
            },
            Path::new("never-opened-state"),
            Path::new("/fixture/host-home"),
        )
        .err()
        .ok_or("missing root refuses")?;
        let kind = fail.kind;
        match super::dispatch_refused(fail) {
            FacadeError::DispatchRefused { reason, .. } => {
                assert!(reason.contains("inspect"));
                assert!(!reason.contains("missing-root"));
            }
            error => panic!("a refused submit kept no reason: {error:?}"),
        }
        // The kind alone renders as invalid input; the dispatch edge must not
        // lose the refusal by rerendering it.
        assert_eq!(super::facade_error_for_kind(kind), FacadeError::InvalidInput { detail: None });
        Ok(())
    }
    #[test]
    fn events_within_trims_whole_events_and_counts_separators() {
        // Serialised sizes are 4, 12 and 3 bytes; the array adds brackets and commas.
        let events = ["aa", "bbbbbbbbbb", "c"];
        for (max_bytes, kept) in [
            (65536, 3),
            // Two events are 2 + 4 + 1 + 12 = 19 bytes; the comma counts.
            (19, 2),
            (18, 1),
            // One event is 2 + 4 bytes.
            (6, 1),
            (5, 0),
            (0, 0),
        ] {
            assert_eq!(
                super::events_within(&events, max_bytes).unwrap(),
                kept,
                "{max_bytes}"
            );
        }
        // A large event stops the page even when a later small one would fit.
        assert_eq!(super::events_within(&events, 10).unwrap(), 1);
        assert_eq!(super::events_within::<&str>(&[], 2).unwrap(), 0);
    }
    #[test]
    fn host_probe_budget_is_shared_fairly() {
        assert_eq!(super::fair_probe_micros(10_000_000, 4), Some(2_500_000));
        assert_eq!(super::fair_probe_micros(55_000_000, 100), Some(550_000));
    }
    #[test]
    fn forwarded_unobservable_cancel_keeps_its_operation() -> Result<(), Box<dyn std::error::Error>>
    {
        let error = super::FacadeError::LaunchUnknown {
            operation_id: provenant_core::OperationId::parse("stop")?,
        };
        let reply = super::peer_result::<()>(Err(error), |()| super::PeerResult::Error {
            data: provenant_fabric_runtime::hosts::PeerResultError {
                extra: Default::default(),
                error: super::FacadeError::io("test"),
            },
            extra: Default::default(),
        });
        let super::PeerResult::Error {
            data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
            ..
        } = reply
        else {
            return Err("expected error".into());
        };
        assert!(
            matches!(error, super::FacadeError::LaunchUnknown { operation_id } if operation_id.as_str() == "stop")
        );
        Ok(())
    }

    pub(crate) fn writer_graph_output_preserves_lens_diff_base()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("writer_graph_output_preserves_lens_diff_base")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        struct Seed(String);
        impl AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.0.clone()),
                })
            }
        }
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let call = provenant_run::Request {
            clock: &factory.clock,
            cancel: &cancel,
        };
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let node_source = provenant_run::resolve_git_base(&git, &fixture.rig.project, "HEAD")?
            .parse::<provenant_core::GitOid>()?;

        // 1. Dispatch writer attempt 1 on fixture.worktree with base = node_source.
        let dispatch = provenant_run::Service::new(&fixture.store).dispatch(
            provenant_run::DispatchRequest {
                chair: None,
                operation_id: "writer-output-op".parse()?,
                concurrency: 1,
                lane_depth: 1,
                parent_attempt: None,
                tasks: vec![provenant_run::DispatchTask {
                    source_head: None,
                    cloud: None,
                    context: None,
                    build_heavy: false,
                    title: Some("writer".into()),
                    task_id: TaskId::from_bytes([60; 16]),
                    prompt: "implement slice".into(),
                    base: Some(node_source.to_string()),
                    pinned_base: None,
                    head: Some(node_source.to_string()),
                    profile: Default::default(),
                    route: provenant_run::RequestedRoute::Model {
                        extra: Default::default(),
                        provider: "claude".into(),
                        model: "claude/test-model".into(),
                        effort: None,
                    },
                    account: None,
                    timeout_seconds: 30,
                    delivery_id: None,
                    deliver: None,
                    worktree: Some(fixture.worktree.clone()),
                    access: Default::default(),
                }],
            },
            &fixture.rig.project,
            &call,
        )?;
        let attempt_1 = fixture
            .store
            .pending_attempts(dispatch.run_id, &call)?
            .first()
            .ok_or("attempt_1")?
            .attempt_id;
        let attempt_1_record = fixture.store.record(attempt_1, &call)?;
        // This graph adopts the fixture's idle lane. Bind its owner to the
        // writer attempt rather than the unrelated resume fixture lineage.
        let writer = provenant_run::prepare_writer(
            &provenant_run::WriterRequest {
                extra: Default::default(), project: fixture.rig.project.clone(),
                worktree: fixture.worktree.clone(),
            }, fixture.roots.state.as_path(), fixture.settled.project_id,
        )?;
        let mut lane_owner = worktrees::load_owner(&writer.worktree)?.ok_or("lane owner")?;
        lane_owner.attempt_id = Some(attempt_1);
        lane_owner.operation_id = Some(dispatch.operation_id.clone());
        worktrees::save_owner(&writer.worktree, &lane_owner)?;


        let worktree_str = fixture.worktree.to_str().ok_or("worktree path")?;

        // The writer commits a subject distinct from its graph source.
        std::fs::write(fixture.worktree.join("writer-edited"), "writer fixture completed")?;
        std::fs::write(fixture.worktree.join("first.txt"), b"first edit\n")?;
        let output = fixture.rig.execute(&git, &["-C", worktree_str, "add", "first.txt", "writer-edited"], None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let output = fixture.rig.execute(
            &git,
            &["-C", worktree_str, "commit", "-m", "first commit"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let head_1 = provenant_run::resolve_git_base(&git, &fixture.worktree, "HEAD")?
            .parse::<provenant_core::GitOid>()?;

        publish_outcome(
            &fixture.rig,
            &fixture.store,
            &attempt_1_record,
            head_1,
            Outcome::Succeeded,
            &call,
        )?;

        // 2. Set up graph with writer node and dependent lens node.
        let graph_id = "writer-output-lens-graph";
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": graph_id,
            "nodes": [
                {
                    "id": "writer",
                    "prompt": "write",
                    "access": "worktree_write",
                    "worktree": fixture.worktree,
                    "source": { "type": "commit", "commit": node_source.to_string() },
                    "route": { "adapter": "fixture", "model": "fixture/model" }
                },
                {
                    "id": "lens",
                    "prompt": "review",
                    "access": "read_only",
                    "depends_on": ["writer"],
                    "source": { "type": "node", "node": "writer" },
                    "route": { "adapter": "fixture", "model": "fixture/model" }
                }
            ]
        }))?;

        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {
            extra: Default::default(),
            integration_base: Some(node_source),
            review_base: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;

        // Seed attempt_1 for the writer node in the graph checkpoint.
        let mut seed = Seed(format!(
            "{}/{}",
            factory.context.identity.host, attempt_1
        ));
        graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;

        // Now advance with BoundGraphOwner.
        let mut owner = BoundGraphOwner { launch_warnings: Vec::new(), repo_facts: Default::default(),
            factory: &factory,
            integration_base: Some(BoundGraphBases { integration: Some(node_source), review: None }),
            integration_branch: None,
            network: None, capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;

        // 3. Verification:
        // The graph observes the writer output directly.
        let writer_progress = &run.checkpoint.nodes[0];
        assert_eq!(writer_progress.state, graph::NodeState::Succeeded);
        assert_eq!(
            writer_progress
                .outputs
                .as_ref()
                .and_then(|o| o.commit.as_ref()),
            Some(&head_1.to_string())
        );

        // - Lens node was submitted, and its diff base stays node_source (not head_1)
        let lens_progress = &run.checkpoint.nodes[1];
        let lens_attempt_ref = lens_progress
            .attempts
            .first()
            .and_then(|a| a.attempt_ref.as_ref())
            .ok_or("lens attempt_ref")?;
        let (_, lens_attempt_id) = lens_attempt_ref.split_once('/').ok_or("attempt split")?;
        let lens_record = fixture.store.record(lens_attempt_id.parse()?, &call)?;
        let lens_request = fixture.store.request(lens_record.run_id, &call)?;
        let lens_task = lens_request
            .tasks
            .into_iter()
            .find(|t| t.task_id == lens_record.task_id)
            .ok_or("lens task")?;

        let node_source_str = node_source.to_string();
        let head_1_str = head_1.to_string();
        assert_eq!(
            lens_task.base.as_deref(),
            Some(node_source_str.as_str()),
            "lens diff base must stay the node source"
        );
        assert_ne!(
            lens_task.base.as_deref(),
            Some(head_1_str.as_str()),
            "lens diff base must not advance to the writer output"
        );
        assert_eq!(
            lens_task.head.as_deref(),
            Some(head_1_str.as_str()),
            "lens subject commit must be the writer output"
        );

        Ok(())
    }
}

#[test]
fn fallback_cause_uses_only_unanimous_typed_facts() {
    use provenant_records::{
        RouteExclusionReason as Cause,
        run::{SkipKind, SkippedRoute},
    };
    let route = |cause| SkippedRoute {
        extra: Default::default(),
        route: "/private/token".into(),
        kind: SkipKind::Unavailable,
        cause,
        detail: "prompt token \u{1b} /private/secret".into(),
    };
    let first = route(Some(Cause::StaleAvailability));
    let first: SkippedRoute = serde_json::from_slice(&serde_json::to_vec(&first).unwrap()).unwrap();
    assert_eq!(
        unanimous_preparation_cause(std::slice::from_ref(&first)),
        Some(Cause::StaleAvailability)
    );
    assert_eq!(unanimous_preparation_cause(&[first.clone(), route(None)]), None);
    assert_eq!(
        unanimous_preparation_cause(&[first, route(Some(Cause::AuthRequired))]),
        None
    );
    assert_eq!(unanimous_preparation_cause(&[]), None);
}
