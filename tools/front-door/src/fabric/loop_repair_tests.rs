#[allow(unused_imports)] // The worktree driver compiles without libtest.
use super::*;

#[test]
fn loop_round_settlement_retains_lane_owned_noop_worktree() -> Result<(), Box<dyn std::error::Error>>
{
    if host_test(
        "fabric::peer_tests::loop_repair_tests::loop_round_settlement_retains_lane_owned_noop_worktree",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    struct RoundOwner<'a, 'b> {
        bound: &'a mut BoundGraphOwner<'b>,
        next: Option<String>,
    }
    impl AttemptOwner for RoundOwner<'_, '_> {
        type Error = FacadeError;
        fn actor(&self) -> String {
            self.bound.actor()
        }
        fn observe_or_start(
            &mut self,
            key: &graph::AttemptKey,
            node: graph::NodeInput<'_>,
            route: &graph::GraphRoute,
            access: graph::AccessMode,
            worktree: Option<&str>,
            replay: graph::Replay<'_>,
        ) -> Result<graph::AttemptObservation, Self::Error> {
            if replay.stored_ref.is_none() {
                return Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.next.take().ok_or(FacadeError::StateCorrupt)?),
                });
            }
            self.bound
                .observe_or_start(key, node, route, access, worktree, replay)
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
        backoff: ThreadWait,
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
        budget,
    };
    let head = fixture
        .store
        .record(fixture.settled.attempt_id, &call)?
        .head
        .ok_or("head")?;
    let second = provenant_run::Service::new(&fixture.store).dispatch(
        provenant_run::DispatchRequest {
            chair: None,
            operation_id: "loop-round-two".parse()?,
            concurrency: 1,
            lane_depth: 1,
            parent_attempt: None,
            tasks: vec![provenant_run::DispatchTask {
                context: None,
                build_heavy: false,
                title: None,
                task_id: TaskId::from_bytes([71; 16]),
                prompt: "reply OK".into(),
                base: Some(head.to_string()),
                head: None,
                profile: Default::default(),
                route: provenant_run::RequestedRoute::Model {extra: Default::default(),
                    provider: "fixture".into(),
                    model: "fixture/model".into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
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
    let second_id = fixture.store.pending_attempts(second.run_id, &call)?[0].attempt_id;
    let second_record = fixture.store.record(second_id, &call)?;
    assert!(second_record.retry_of.is_none());
    publish_success(&fixture.rig, &fixture.store, &second_record, head, &call)?;
    let mut second_holder = fixture.settled.clone();
    second_holder.run_id = second.run_id;
    second_holder.attempt_id = second_id;
    let document: graph::GraphDocument = serde_json::from_value(
        serde_json::json!({"schema_version":1,"graph_id":"loop-settlement","nodes":[{"id":"revise","kind":"loop","access":"read_only","loop":{"max_rounds":2,"until":"approved","body":[{"id":"repair","prompt":"reply OK","access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}},{"id":"approved","kind":"gate","access":"read_only","depends_on":["repair"],"inputs":[{"node":"repair","outputs":["verdict"]}],"condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}]}}]}),
    )?;
    let mut journal = graph::FileJournal::open(
        &fixture.roots.state,
        fixture.settled.project_id,
        &document.graph_id,
        budget,
        &cancel,
        &factory.clock,
    )?;
    journal.bind(graph::GraphExecution {extra: Default::default(),
        integration_base: None,
        directory: fixture.rig.project.clone(),
        document: document.clone(),
    })?;
    let mut bound = BoundGraphOwner {
        integration_branch: None,
        factory: &factory,
        integration_base: None,
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    for holder in [&fixture.settled, &second_holder] {
        let mut owner = RoundOwner {
            bound: &mut bound,
            next: Some(format!(
                "{}/{}",
                factory.context.identity.host, holder.attempt_id
            )),
        };
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        assert_eq!(run.status, graph::GraphStatus::Pending);
        settle_owned_worktree(holder, &fixture.roots, &FreeWorkers, None)?;
        let receipt = fixture
            .store
            .worktree_settlement(holder.attempt_id, &call)?
            .ok_or("receipt")?;
        assert_eq!(receipt["state"], "retained", "{receipt}");
        assert!(fixture.worktree.exists());
    }
    let mut owner = RoundOwner {
        bound: &mut bound,
        next: None,
    };
    let exhausted = graph::advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &Default::default(),
    )?;
    assert_eq!(exhausted.status, graph::GraphStatus::Paused);
    for round in 1..=2 {
        let node = exhausted
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == format!("revise--r{round}--repair"))
            .ok_or("round")?;
        assert_eq!(node.state, graph::NodeState::Succeeded);
    }
    let accepted = graph::resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &Default::default(),
        &["loop.revise.decision=accept".into()],
    )?;
    assert!(accepted.checkpoint.successful());
    for holder in [&fixture.settled, &second_holder] {
        settle_owned_worktree(holder, &fixture.roots, &FreeWorkers, None)?;
    }
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec![
            "-C",
            fixture.worktree.to_str().ok_or("path")?,
            "rev-parse",
            "HEAD",
        ],
        vec!["rev-parse", "refs/heads/resumed-lane"],
    ] {
        let result = fixture.rig.execute(&git, &args, None)?;
        assert_eq!(crate::worktree_tests::support::code(&result), 0);
        assert_eq!(
            String::from_utf8_lossy(&result.stdout.bytes).trim(),
            head.to_string()
        );
    }
    Ok(())
}

#[test]
fn bound_loop_inbox_keeps_original_actor_under_another_client()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::loop_repair_tests::bound_loop_inbox_keeps_original_actor_under_another_client",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::{
        graph::{AskScope, AttemptOwner, loop_ask},
        inbox::{InboxFilter, InboxInput, InboxResult, InboxService},
    };
    use provenant_records::inbox::{AnswerAction, InteractionChannel, RequestState};
    let fixture = SettlementFixture::resumed()?;
    let mut context = Context::open(
        fixture.roots.clone(),
        &fixture.rig.home,
        &fixture.rig.project,
    )?;
    let original = context.identity.clone();
    context.identity.inbox =
        provenant_fabric_runtime::identity::Address::new(original.host, "other-client")?;
    context.identity.client = "other-client".into();
    assert_ne!(original.inbox.actor, context.identity.inbox.actor);
    let factory = Factory {
        context,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadWait,
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
    let actor = original.inbox.actor.to_string();
    let ask = loop_ask("actor-loop", "revise", 2, "original-session");
    let scope = AskScope {
        graph_id: "actor-loop",
        instance_id: "instance-one",
        actor: &actor,
        ask: &ask,
    };
    let mut owner = BoundGraphOwner {
        integration_branch: None,
        factory: &factory,
        integration_base: None,
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    assert_eq!(owner.node_ask(scope)?, None);
    let service = InboxService::open(
        &fixture.roots.state,
        &original,
        &factory.clock,
        budget,
        &cancel,
    )?;
    let InboxResult::Listed { items, .. } = service.execute(
        InboxInput::List {extra: Default::default(),
            id: None,
            session: Some(ask.session.clone()),
            state: InboxFilter::All,
            limit: 10,
            cursor: None,
        },
        budget,
        &cancel,
    )?
    else {
        return Err("listing".into());
    };
    assert_eq!(items.len(), 1);
    let ask = &items[0];
    assert_eq!(ask.chair_actor, original.inbox.actor);
    service.execute(
        InboxInput::Answer {extra: Default::default(),
            id: ask.id,
            action: AnswerAction::Choose,
            option_id: Some("accept".into()),
            text: None,
            expect_revision: ask.revision,
            channel: InteractionChannel::Cli,
            operation_id: None,
        },
        budget,
        &cancel,
    )?;
    let reply = owner.node_ask(scope)?.ok_or("missing answer")?;
    assert_eq!(reply.option.as_deref(), Some("accept"));
    assert_eq!(reply.actor, actor);
    owner.close_node_ask(scope)?;
    let InboxResult::Item { item, .. } = service.execute(
        InboxInput::List {extra: Default::default(),
            id: Some(ask.id),
            session: None,
            state: InboxFilter::All,
            limit: 1,
            cursor: None,
        },
        budget,
        &cancel,
    )?
    else {
        return Err("request".into());
    };
    assert_eq!(item.request.state, RequestState::Superseded);
    Ok(())
}

#[test]
fn loop_corrupt_unrelated_checkpoint_does_not_fail_ordinary_settlement()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::loop_repair_tests::loop_corrupt_unrelated_checkpoint_does_not_fail_ordinary_settlement",
    )? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
        budget,
    };
    let record = fixture.store.record(fixture.settled.attempt_id, &call)?;
    assert!(!record.operation_id.as_str().starts_with("graph-"));
    {
        let _journal = provenant_fabric_runtime::graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            "broken",
            budget,
            &cancel,
            &clock,
        )?;
    }
    let broken = fixture
        .roots
        .state
        .as_path()
        .join("fabric/graphs")
        .join(fixture.settled.project_id.to_string())
        .join("graph-broken.json");
    std::fs::write(broken, "incomplete unrelated checkpoint")?;
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    assert_eq!(
        fixture
            .store
            .worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("receipt")?["state"],
        "pruned"
    );
    assert!(!fixture.worktree.exists());
    Ok(())
}
