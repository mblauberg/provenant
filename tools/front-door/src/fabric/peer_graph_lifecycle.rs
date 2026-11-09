#[allow(unused_imports)]
use super::*;

#[test]
fn graph_factory_reopens_the_retained_directory() -> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_graph_lifecycle::graph_factory_reopens_the_retained_directory")? {
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::tick_reports_unreadable_once_and_advances_another_graph")? {
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::tick_dispatches_waiting_node_without_a_deadline")? { return Ok(()); }
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::tick_dispatches_capacity_waiter_with_future_decision")? { return Ok(()); }
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::tick_warns_once_for_missing_worktree_and_advances_another_graph")? {
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::graph_wait_observes_settlement_and_budget")? {
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::graph_cancel_persists_reason_and_skips_pending_node")? {
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::graph_status_and_cancel_retire_unreadable_record")? {
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
        assert!(explanation.contains(&format!("provenant stop -- {id}")), "{explanation}");
        assert!(explanation.contains("Other graphs"), "{explanation}");
        if id == "g-test" {
            assert!(explanation.contains("predates node asks"), "{explanation}");
            assert!(explanation.contains("previous release"), "{explanation}");
        }
        assert!(!explanation.contains("invalid json"), "{explanation}");
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
    let mut owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory,
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
    let mut owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, factory: &renamed, ..owner };
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
                "fabric::peer_tests::peer_graph_lifecycle::graph_cloud_owner_submits_once_and_replays_remote_completion".into(),
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
        let mut graph_owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None,
            integration_branch: None, network: None, capability_overrides: Default::default(),
            exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false };
        let actor = graph_owner.actor();
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"cloud-owner", "node_id":"verify", "route_index":0
        }))?;
        let route = serde_json::from_value(serde_json::json!({"alias":"senior"}))?;
        let spec: graph::AttemptSpec = serde_json::from_value(serde_json::json!({"placement":{
            "target":"cloud-test", "provider":"codex", "environment":"env", "repository":"repo",
            "branch":"main", "model":"requested", "deadline_seconds":3600
        }}))?;
        let node = || graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: Some(&spec),
            require_skill_location: false, settlement_retries: None, prior_attempts: &[], family_peers: &[],
            context: graph_test_context(&key), read_roots: &[], distinct_from_attempts: &[], require_distinct: false,
            source_commit: None, review_base: None, title: "verify", prompt: "Run targeted tests",
            required_verdict: None, deadline_seconds: None, inputs_dir: None,
        };
        let refused = graph_owner.observe_or_start(&key, node(), &route, graph::AccessMode::WorktreeWrite,
            None, graph::Replay { delivery: None, branch: None, fast_forward_only: false,
                stored_ref: None, launch_actor: &actor });
        assert!(matches!(refused, Err(FacadeError::InvalidInput { .. })));
        let refused_owner = BinaryRunOwner { factory: &factory, allow_remote: false, actor: None, chair: None };
        let refusal = refused_owner.store(&cancel)?.operation(
            &graph_operation("graph-attempt-operation", &key)?, &refused_owner.call(&cancel),
        )?.ok_or("refusal operation missing")?;
        assert_eq!(refusal.state, provenant_run::OperationState::Refused);
        let key: graph::AttemptKey = serde_json::from_value(serde_json::json!({
            "graph_id":"cloud-owner", "node_id":"verify-read-only", "route_index":0
        }))?;
        let node = || graph::NodeInput {
            attachments: &[], capabilities: None, capability_sources: None, attempt: Some(&spec),
            require_skill_location: false, settlement_retries: None, prior_attempts: &[], family_peers: &[],
            context: graph_test_context(&key), read_roots: &[], distinct_from_attempts: &[], require_distinct: false,
            source_commit: None, review_base: None, title: "verify", prompt: "Run targeted tests",
            required_verdict: None, deadline_seconds: None, inputs_dir: None,
        };
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
        assert_eq!(job.commit, head);
        let result = serde_json::json!({"job_id":job.job_id, "nonce":job.nonce, "commit":head,
            "exit_status":0, "tests_run":["targeted"], "tests_failed":[], "log_location":"log",
            "observed_model":null});
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::graph_peer_without_receipt_keeps_launch_and_cancellation_uncertain")? {
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
    let mut owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(),
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
    if host_test("fabric::peer_tests::peer_graph_lifecycle::graph_run_controls_reach_the_sealed_attempt")? {
        return Ok(());
    }
    use provenant_records::run::DeliveryMode;
    for (writer, new_worktree, delivery_mode) in [
        (false, false, None),
        (true, false, Some(DeliveryMode::Integrate("controls-integration".into()))),
        (true, true, Some(DeliveryMode::Integrate("controls-integration".into()))),
        (true, true, Some(DeliveryMode::Push)),
    ] {
        let rig = crate::worktree_tests::repository()?;
        crate::worktree_tests::git(&rig, &["branch", "controls-integration"])?;
        crate::worktree_tests::git(&rig, &["remote", "add", "origin", rig.project.to_str().ok_or("project path")?])?;
        let write_root = rig.root.join("extra-write");
        std::fs::create_dir(&write_root)?;
        let write_root = write_root.canonicalize()?;
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
        let mut input = serde_json::json!({
            "op":"run", "brief":"Inspect the parser", "model":"fixture/model",
            "effort":"high", "account":"fixture-account", "host":"local",
            "operation_id":"graph-run-controls",
            "document":{"use":"task", "limits":{"concurrency":3}},
            "set":{
                "attempt.build_heavy":true,
                "attempt.confidential":true,
                "attempt.provider_timeout":42,
                "attempt.capabilities":["postgres"],
                "attempt.write_roots":[write_root]
            }
        });
        if writer {
            // An ordinary custom task leaf exercises writer delivery while
            // retaining the graph document's separate delivery policy.
            input["document"]["templates"] = serde_json::json!({"task":{
                "brief":"Do the work", "tier":"senior", "access":"worktree_write",
                "node":{"delivery":delivery_mode}
            }});
            if new_worktree {
                input["branch"] = serde_json::json!("controls-writer");
                input["base"] = serde_json::json!("HEAD");
            } else {
                use provenant_confinement::worktrees::{self, WorktreeCreate, WorktreeContext};
                let info = worktrees::with_context(
                    &factory.context.roots, &rig.home, &rig.project, &factory.clock, &cancel,
                    |context| worktrees::create(
                        &WorktreeContext { workers: &FreeWorkers, ..*context },
                        WorktreeCreate { branch: "controls-writer".into(), base: Some("HEAD".into()) },
                        &cancel,
                    ),
                )?;
                input["worktree"] = serde_json::json!(info.path);
            }
            input["write_scope"] = serde_json::json!(["src"]);
        }
        let preview = factory.graph_document({
            let mut preview = input.clone();
            preview["explain"] = serde_json::json!(true);
            preview
        }, budget, &cancel).map_err(|error| format!("preview controls (writer={writer}, new={new_worktree}, delivery={delivery_mode:?}): {error:?}"))?;
        assert_eq!(preview["document"]["graph_id"], "g-e813e34cbb5c0bf4b1ee8313250533");
        assert_eq!(preview["document"]["limits"]["concurrency"], 3);
        let node = &preview["document"]["nodes"][0];
        assert_eq!(node["attempt"]["host"], "local");
        assert_eq!(node["attempt"]["build_heavy"], true);
        assert_eq!(node["attempt"]["confidential"], true);
        assert_eq!(node["attempt"]["provider_timeout"], 42);
        let launched = factory.graph_document(input.clone(), budget, &cancel)
            .map_err(|error| format!("launch controls (writer={writer}, new={new_worktree}, delivery={delivery_mode:?}): {error:?}"))?;
        let attempt = &launched["checkpoint"]["nodes"][0]["attempts"][0];
        let reference = attempt["attempt_ref"].as_str().ok_or("local attempt reference missing")?;
        let (host, attempt_id) = reference.split_once('/').ok_or("host-qualified reference missing")?;
        assert_eq!(host, factory.context.identity.host.to_string());
        let key = serde_json::from_value(attempt["key"].clone())?;
        let owner = BinaryRunOwner { factory: &factory, allow_remote: true, actor: None, chair: None };
        let store = owner.store(&cancel)?;
        let call = owner.call(&cancel);
        let operation = store.operation(&graph_operation("graph-attempt-operation", &key)?, &call)?
            .ok_or("launch operation missing")?;
        let execution = provenant_fabric_runtime::graph::FileJournal::execution(
            &factory.context.roots.state,
            factory.context.identity.project,
            launched["checkpoint"]["graph_id"].as_str().ok_or("graph ID missing")?,
        )?.ok_or("sealed graph execution missing")?;
        assert_eq!(execution.document.limits.as_ref().and_then(|limits| limits["concurrency"].as_u64()), Some(3));
        let record = store.record(attempt_id.parse()?, &call)?;
        assert_eq!(record.run_id, operation.run_id);
        let request = store.request(operation.run_id, &call)?;
        // The graph schedules up to its requested concurrency; each node
        // seals one provider process in a one-task run.
        assert_eq!(request.concurrency, 1);
        let task = request.tasks.first().ok_or("sealed task missing")?;
        assert_eq!(task.route, serde_json::from_value(serde_json::json!({
            "kind":"model", "provider":"fixture", "model":"fixture/model", "effort":"high"
        }))?);
        assert_eq!(task.account.as_deref(), Some("fixture-account"));
        assert!(task.build_heavy);
        assert_eq!(task.provider_timeout_seconds, 42);
        assert_eq!(task.profile.extra.get("confidential"), Some(&serde_json::json!(true)));
        assert!(task.access.database_ipc, "the explicit postgres capability reaches admission");
        assert_eq!(task.access.write_roots, [write_root]);
        if writer {
            let writer = task.writer.as_ref().ok_or("sealed writer missing")?;
            assert_eq!(writer.worktree, rig.project.join(".worktrees/controls-writer").canonicalize()?);
            assert_eq!(writer.project, rig.project.canonicalize()?);
            let delivery = task.deliver.as_ref().ok_or("sealed delivery missing")?;
            assert_eq!(Some(&delivery.mode), delivery_mode.as_ref());
            assert_eq!(delivery.branch, if delivery_mode == Some(DeliveryMode::Push) {
                "controls-writer"
            } else {
                "controls-integration"
            });
        } else {
            assert!(task.writer.is_none());
            assert!(task.deliver.is_none());
        }
        let replay = factory.graph_document(input, budget, &cancel)?;
        let replay_key = serde_json::from_value(replay["checkpoint"]["nodes"][0]["attempts"][0]["key"].clone())?;
        let replay_operation = store.operation(&graph_operation("graph-attempt-operation", &replay_key)?, &call)?
            .ok_or("replayed operation missing")?;
        assert_eq!(replay_operation.run_id, operation.run_id);
    }
    Ok(())
}

#[test]
fn new_writer_delivery_refusals_precede_worktree_provisioning()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_graph_lifecycle::new_writer_delivery_refusals_precede_worktree_provisioning")? {
        return Ok(());
    }
    for protected in [false, true] {
        let rig = crate::worktree_tests::repository()?;
        crate::worktree_tests::git(&rig, &["remote", "add", "origin", rig.project.to_str().ok_or("project path")?])?;
        // Use a non-main default branch to exercise configured protection.
        crate::worktree_tests::git(&rig, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/protected-writer"])?;
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
        let branch = if protected { "protected-writer" } else { "controls-writer" };
        let delivery = if protected { "push" } else { "integrate:controls-writer" };
        let input = serde_json::json!({
            "op":"run", "brief":"Inspect the parser", "model":"fixture/model",
            "branch":branch, "base":"HEAD", "write_scope":["src"],
            "operation_id":"refused-writer-delivery",
            "document":{"use":"task", "templates":{"task":{
                "brief":"Do the work", "tier":"senior", "access":"worktree_write",
                "node":{"delivery":delivery}
            }}}
        });
        let error = factory.graph_document(input, budget, &cancel)
            .err().ok_or("unsafe new writer delivery was admitted")?;
        if protected {
            assert!(matches!(error, FacadeError::ConfinementRefused { .. }), "{error:?}");
        } else {
            assert!(matches!(error, FacadeError::DispatchRefused { .. }), "{error:?}");
        }
        assert!(!rig.project.join(format!(".worktrees/{branch}")).exists());
        assert!(provenant_fabric_runtime::graph::FileJournal::graph_ids(
            &roots.state, factory.context.identity.project,
        )?.is_empty(), "refusal must precede the first checkpoint");
        assert!(!roots.state.as_path().join("run.sqlite3").exists(),
            "refusal must precede attempt admission");
    }
    Ok(())
}

