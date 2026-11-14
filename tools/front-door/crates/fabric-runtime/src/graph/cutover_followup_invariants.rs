use super::*;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::convert::Infallible;

#[derive(Default)]
struct Journal(Option<GraphCheckpoint>);
impl GraphJournal for Journal {
    type Error = Infallible;
    fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self
            .0
            .as_ref()
            .filter(|cp| cp.graph_id == graph_id)
            .cloned())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.0 = Some(checkpoint.clone());
        Ok(())
    }
}

fn route(adapter: &str) -> Value {
    json!({"adapter": adapter, "model": format!("{adapter}/model")})
}

fn terminal(
    outcome: AttemptOutcome,
    stop: Option<provenant_records::StopCause>,
    text: Option<&str>,
) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: Some("fixture/attempt".into()),
        settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
        outcome,
        launched: true,
        result: text.map(|text| {
            let mut capture = crate::result_capture::ResultCapture::new(None);
            capture.push(text.as_bytes());
            let mut result = capture.finish();
            result.handoff = provenant_records::handoff::Handoff::from_turns(text, &[]);
            Box::new(result)
        }),
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop,
    }
}

#[derive(Default)]
struct Owner {
    observations: VecDeque<AttemptObservation>,
    routes: Vec<GraphRoute>,
    prompts: Vec<String>,
    worktrees: Vec<Option<String>>,
    filed: Vec<AskRequest>,
    closed: Vec<String>,
    reply: Option<AskReply>,
}
impl CiStatus for Owner {
    fn host_stamp(&self) -> Option<CiHostStamp> {
        Some(CiHostStamp {
            extra: Default::default(),
            boot_identity: "fixture".into(),
            host_monotonic_micros: 1,
        })
    }
    fn observe_ci(
        &mut self,
        _target: &str,
        _checks: &[String],
        _: usize,
        _: usize,
    ) -> CiObservation {
        CiObservation {
            extra: Default::default(),
            state: CiState::Passed,
            summary: "passed".into(),
            head_sha: Some("1111111111111111111111111111111111111111".into()),
            failed_checks: Vec::new(),
            check_names: Vec::new(),
            truncated: false,
        }
    }
}
impl AttemptOwner for Owner {
    type Error = Infallible;
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }
    fn actor(&self) -> String {
        "chair".into()
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        provenant_core::WallTime::from_unix_seconds(100).ok()
    }
    fn ci_status(&mut self) -> Option<&mut dyn CiStatus> {
        Some(self)
    }
    fn land_policy(
        &self,
        _tier: provenant_records::workflow::RiskTier,
    ) -> crate::config::LandChoice {
        crate::config::LandChoice {
            policy: crate::config::LandPolicy::Ask,
            source: crate::config::LandSource::Project,
        }
    }
    fn land(&mut self, request: LandRequest<'_>) -> Result<LandObservation, Self::Error> {
        Ok(if request.pull_request.is_none() {
            LandObservation::Identified { pull_request: 7 }
        } else {
            LandObservation::Pending {
                reason: "awaiting chair".into(),
            }
        })
    }
    fn review_range(
        &mut self,
        _attempt_ref: &str,
        _explicit_base: bool,
    ) -> Result<ReviewRange, Self::Error> {
        Ok(ReviewRange::Resolved {
            base: "3333333333333333333333333333333333333333".into(),
            head: "1111111111111111111111111111111111111111".into(),
        })
    }
    fn node_ask(&mut self, _scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        Ok(self.reply.clone())
    }
    fn observe_or_start(
        &mut self,
        _key: &AttemptKey,
        node: NodeInput<'_>,
        route: &GraphRoute,
        access: AccessMode,
        worktree: Option<&str>,
        _replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.routes.push(route.clone());
        self.prompts.push(node.prompt.to_owned());
        self.worktrees.push(worktree.map(str::to_owned));
        let mut observation = self
            .observations
            .pop_front()
            .unwrap_or(AttemptObservation::Pending { attempt_ref: None });
        if let AttemptObservation::Terminal {
            attempt_ref,
            outcome,
            branch,
            commit,
            ..
        } = &mut observation
        {
            *attempt_ref = Some(format!("fixture/{}", self.routes.len()));
            if access == AccessMode::WorktreeWrite && *outcome == AttemptOutcome::Succeeded {
                *branch = Some("feature/x".into());
                *commit = Some("1111111111111111111111111111111111111111".into());
            }
        }
        Ok(observation)
    }
    fn file_ask(&mut self, filing: AskFiling<'_>) -> Result<AskFiled, Self::Error> {
        if !self.filed.iter().any(|ask| ask.ask_id == filing.ask.ask_id) {
            self.filed.push(filing.ask.clone());
        }
        Ok(AskFiled {
            request: filing.ask.ask_id.clone(),
            deadline: filing.ask.wake_at,
            closed: self.closed.contains(&filing.ask.ask_id),
        })
    }
    fn close_node_ask(&mut self, scope: AskScope<'_>) -> Result<(), Self::Error> {
        self.closed.push(scope.ask.ask_id.clone());
        Ok(())
    }
}

fn one_route_graph(id: &str) -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version": 1, "graph_id": id, "nodes": [{
            "id":"writer", "access":"worktree_write", "prompt":"Finish this slice",
            "worktree":".worktrees/writer", "routes":[route("first")]
        }]
    }))
    .unwrap()
}

#[test]
fn single_route_token_loss_and_context_handoff_reuse_the_same_route() {
    use provenant_records::StopCause;
    let handoff = format!(
        "{}handoff\n{{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"Continue this slice\",\"remaining\":[\"finish\"],\"decisions\":[],\"verify\":[]}}\n{}",
        "\u{60}\u{60}\u{60}", "\u{60}\u{60}\u{60}"
    );
    for cause in [StopCause::LaneTokenLost, StopCause::ContextHandoff] {
        let document = one_route_graph("single-route");
        let mut owner = Owner {
            observations: [
                terminal(AttemptOutcome::TaskFailed, Some(cause), Some(&handoff)),
                terminal(AttemptOutcome::Succeeded, None, None),
            ]
            .into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut Journal::default()).unwrap();
        assert!(run.checkpoint.successful(), "{cause}");
        assert_eq!(owner.routes.len(), 2, "{cause}");
        assert_eq!(owner.routes[0], owner.routes[1], "{cause}");
        assert_eq!(owner.worktrees[0], owner.worktrees[1], "{cause}");
        assert!(owner.prompts[1].contains("finish"), "{cause}");
        let restarts: Vec<_> = run
            .checkpoint
            .expansions
            .iter()
            .filter_map(|e| e.patch.as_ref()?.restart.as_ref())
            .map(|r| (r.cause, r.step))
            .collect();
        assert_eq!(restarts, [(cause, provenant_records::NextStep::NewRoute)]);
    }
}

fn pinned_depletion_document() -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version": 1, "graph_id": "pinned-tier",
        "nodes": [{
            "id": "writer", "access":"worktree_write", "prompt":"work",
            "worktree":".worktrees/writer",
            "route":{"adapter":"claude","model":"claude/claude-sonnet-5-5",
                     "account":"pinned","tier":"senior"}
        }]
    }))
    .unwrap()
}

#[test]
fn depletion_keeps_the_pinned_account_and_refuses_a_higher_tier_candidate() {
    use provenant_records::StopCause;
    use provenant_routing::{ModelId, Readiness, ReadinessFact, Stopped};
    let document = pinned_depletion_document();
    let limits = crate::config::GraphLimits::default();
    let (plan, _) = validate_flat_plan(&document, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&document, &plan, "digest".into(), None, None, None, None);
    checkpoint.nodes[0].state = NodeState::Failed;
    checkpoint.nodes[0].attempts.push(GraphAttempt {
        extra: Default::default(),
        launched: true,
        review_substitution: false,
        key: AttemptKey {
            extra: Default::default(),
            graph_id: document.graph_id.clone(),
            node_id: "writer".into(),
            route_index: 0,
            attempt: 1,
        },
        route: plan.nodes[0].routes[0].clone(),
        state: AttemptState::Terminal,
        attempt_ref: Some("provider/1".into()),
        outcome: Some(AttemptOutcome::UsageLimited),
        launch_actor: "chair".into(),
        input_cap: None,
        input_cap_source: None,
        input_manifest: None,
        usage: None,
        settled: 1,
        stop: Some(StopCause::UsageLimited),
    });
    let same_model = ModelId::parse("claude/claude-sonnet-5-5").unwrap();
    let higher_model = ModelId::parse("claude/claude-opus-5-5").unwrap();
    let config: provenant_routing::RoutingConfig = serde_json::from_value(json!({
        "pools": {
            "senior": [{"model":"claude/claude-sonnet-5-5","weight":1,"efforts":[]}],
            "principal": [{"model":"claude/claude-opus-5-5","weight":1,"efforts":[]}]
        },
        "depletion": {
            "rungs":["same_model_other_account","adjacent_tier"],
            "tiers":["senior","principal"],
            "adjacent":["higher"]
        }
    }))
    .unwrap();
    let facts = next_step::DepletionFacts {
        catalogue: provenant_routing::Catalogue::bundled().unwrap(),
        config,
        stopped: Stopped {
            model: same_model.clone(),
            account: Some("pinned".into()),
            effort: None,
            tier: Some("senior".into()),
            family: None,
        },
        readiness: vec![
            ReadinessFact {
                model: same_model,
                account: Some("other".into()),
                state: Readiness::Ready,
            },
            ReadinessFact {
                model: higher_model.clone(),
                account: Some("pinned".into()),
                state: Readiness::Ready,
            },
        ],
        tried: Vec::new(),
        host_native: Vec::new(),
        exclusions: Vec::new(),
        intent: Default::default(),
        confidential: false,
        now: provenant_core::WallTime::from_unix_seconds(100).unwrap(),
    };
    let eligible = |facts: &next_step::DepletionFacts| {
        provenant_routing::next_after_depletion(&provenant_routing::DepletionInput {
            catalogue: &facts.catalogue,
            config: &facts.config,
            stopped: &facts.stopped,
            cause: provenant_routing::DepletionCause::UsageLimited,
            facts: &facts.readiness,
            tried: &facts.tried,
            host_native: &facts.host_native,
            exclusions: &facts.exclusions,
            intent: facts.intent,
            confidential: facts.confidential,
            now: facts.now,
        })
    };
    let mut no_pin = facts.clone();
    no_pin.stopped.account = None;
    assert!(
        matches!(
            eligible(&no_pin),
            provenant_routing::DepletionStep::Handoff(_)
        ),
        "the spare account is an otherwise eligible candidate"
    );
    let mut high_only = facts.clone();
    high_only
        .readiness
        .retain(|fact| fact.model == higher_model);
    high_only.stopped.account = None;
    assert!(
        matches!(
            eligible(&high_only),
            provenant_routing::DepletionStep::Handoff(_)
        ),
        "the higher tier is an otherwise eligible candidate"
    );

    let mut reads = 0;
    let mut lookup = |_request: next_step::DepletionRequest<'_>| {
        reads += 1;
        Ok(Some(facts.clone()))
    };
    let (_, settled) = next_step::settle_stop(
        &document,
        &plan,
        &checkpoint,
        &limits,
        &provenant_config::PortTable::builtin().worktree_layout,
        &mut lookup,
    )
    .expect("depletion stop must settle");
    assert_eq!(reads, 1);
    assert_eq!(settled.nodes[0].state, NodeState::Failed);
    assert_eq!(
        settled.nodes[0].extra.get("stop_fence"),
        Some(&Value::Bool(true))
    );
    assert!(settled.expansions.iter().all(|e| {
        e.patch
            .as_ref()
            .and_then(|patch| patch.restart.as_ref())
            .is_none()
    }));
}

fn review_shape_minima(with: Value) -> std::collections::BTreeMap<String, usize> {
    use std::collections::BTreeMap;
    let library = template::TemplateLibrary::bundled();
    let resolvers = BTreeMap::from([
        ("review-frame".to_owned(), vec!["false".to_owned()]),
        ("review-verify".to_owned(), vec!["true".to_owned()]),
        ("review-synthesis".to_owned(), vec!["false".to_owned()]),
        (
            "review-verifiers".to_owned(),
            vec!["1".to_owned(), "2".to_owned()],
        ),
        (
            "review-panel".to_owned(),
            vec!["security".to_owned(), "tests".to_owned()],
        ),
        ("review-advice-security".to_owned(), vec![String::new()]),
        ("review-advice-tests".to_owned(), vec![String::new()]),
    ]);
    let node = template::Compiler {
        library: &library,
        resolvers: &resolvers,
    }
    .compile(&json!({"id":"shape","use":"review-shape","with":with}))
    .unwrap();
    node.composite
        .unwrap()
        .nodes
        .into_iter()
        .filter(|node| matches!(node.id.as_str(), "lenses" | "verify"))
        .map(|node| (node.id, node.composite.unwrap().min_successful.unwrap()))
        .collect()
}

#[test]
fn review_minimum_is_an_input_with_the_existing_default() {
    for (with, minimum) in [(json!({}), 1), (json!({"min_ok": 2}), 2)] {
        let minima = review_shape_minima(with);
        for stage in ["lenses", "verify"] {
            assert_eq!(minima.get(stage), Some(&minimum));
        }
    }
}

fn decide_node(id: &str) -> Value {
    json!({
        "id":id, "kind":"decide", "access":"read_only",
        "decide":{"on_unanswered":"block_dependants", "question":format!("Choose for {id}"),
            "options":[{"id":"yes","label":"Yes"},{"id":"no","label":"No"}]}
    })
}

fn two_inbox_requests() -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"cancel-linked-asks",
        "decision_session":"chair", "nodes":[decide_node("first"), decide_node("second")]
    }))
    .unwrap()
}

#[test]
fn a_cancel_after_checkpoint_reload_closes_every_linked_inbox_request() {
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;

    let temp = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([2; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let document = two_inbox_requests();
    let limits = crate::config::GraphLimits::default();

    // First process persists both linked asks, then exits without resolving them.
    let mut owner = Owner::default();
    {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        journal
            .bind(GraphExecution {
                extra: Default::default(),
                bases: None,
                directory: base.clone(),
                document: document.clone(),
            })
            .unwrap();
        let waiting = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(waiting.checkpoint.asks.len(), 2);
        assert_eq!(waiting.status, GraphStatus::Pending);
    }
    let linked: Vec<_> = owner.filed.iter().map(|ask| ask.ask_id.clone()).collect();
    assert_eq!(linked.len(), 2);

    // Reload the saved graph after that interruption, then cancel it.
    {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        let mut checkpoint = journal.load(&document.graph_id).unwrap().unwrap();
        checkpoint.cancelled = true;
        journal.save(&checkpoint).unwrap();
    }
    let cancelled = {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap()
    };
    assert_eq!(cancelled.status, GraphStatus::Cancelled);
    assert!(cancelled.checkpoint.asks.is_empty());
    assert_eq!(
        owner
            .closed
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        linked.into_iter().collect()
    );
}

fn chair_reply(option: &str) -> AskReply {
    AskReply {
        option: Some(option.into()),
        text: None,
        actor: "chair".into(),
        at: None,
    }
}

fn stable_run(
    document: &GraphDocument,
    owner: &mut Owner,
    journal: &mut Journal,
) -> GraphCheckpoint {
    let mut previous = None;
    for _ in 0..20 {
        let current = advance(document, owner, journal).unwrap().checkpoint;
        if previous.as_ref() == Some(&current) {
            return current;
        }
        previous = Some(current);
    }
    previous.expect("run reached a checkpoint")
}

fn retry_context<'a>(
    library: &'a template::TemplateLibrary,
    resolvers: &'a std::collections::BTreeMap<String, Vec<String>>,
    limits: &'a crate::config::GraphLimits,
) -> patch::PatchContext<'a> {
    patch::PatchContext {
        library,
        resolvers,
        scan: &patch::scan_for_secrets,
        journal: None,
        limits,

        worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
    }
}

fn land_retry_graph() -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"land-retry", "decision_session":"chair",
        "nodes":[
            {"id":"write","access":"worktree_write","worktree":"/fixture/write",
             "delivery":"pr","prompt":"write","route":route("fixture")},
            {"id":"ci","kind":"gate","access":"read_only","depends_on":["write"],
             "condition":{"type":"ci","target":"feature/x"}},
            {"id":"review","access":"read_only","prompt":"review","depends_on":["write"],
             "source":{"type":"head","node":"write"},"route":route("fixture")},
            {"id":"accepted","kind":"gate","access":"read_only","depends_on":["review"],
             "inputs":[{"node":"review","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}},
            {"id":"land","kind":"land","access":"read_only","depends_on":["accepted","ci"],
             "land":{"node":"write","tier":"crucial"}}
        ]
    }))
    .unwrap()
}

#[test]
fn retried_land_node_files_a_fresh_inbox_id() {
    let document = land_retry_graph();
    let mut owner = Owner {
        observations: [
            terminal(AttemptOutcome::Succeeded, None, None),
            terminal(AttemptOutcome::Succeeded, None, Some("VERDICT: PASS")),
        ]
        .into(),
        reply: Some(chair_reply("hold")),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let failed = stable_run(&document, &mut owner, &mut journal);
    assert_eq!(
        failed.nodes.iter().find(|n| n.id == "land").unwrap().state,
        NodeState::Failed
    );
    let original = owner
        .filed
        .iter()
        .find(|ask| ask.kind == AskKind::Land)
        .expect("initial land request")
        .ask_id
        .clone();

    let library = template::TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::new();
    let limits = crate::config::GraphLimits::default();
    let context = retry_context(&library, &resolvers, &limits);
    let mut checkpoint = failed;
    patch::retry_node(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "land",
        None,
        &context,
    )
    .unwrap();
    journal.save(&checkpoint).unwrap();
    owner.reply = None;
    let retried = stable_run(&document, &mut owner, &mut journal);
    let replacement = retried
        .asks
        .iter()
        .find(|ask| ask.kind == AskKind::Land)
        .expect("retry leaves a fresh request linked")
        .ask_id
        .clone();
    assert_ne!(replacement, original);
    assert_eq!(
        owner
            .filed
            .iter()
            .filter(|ask| ask.kind == AskKind::Land)
            .map(|ask| ask.ask_id.as_str())
            .collect::<Vec<_>>(),
        [original.as_str(), replacement.as_str()]
    );
}

fn loop_retry_graph() -> GraphDocument {
    let mut document: GraphDocument =
        serde_json::from_str(include_str!("../fixtures/loop-extends.json")).unwrap();
    document.nodes[0]
        .loop_body
        .as_mut()
        .unwrap()
        .decision_session = Some("chair".into());
    document
}

#[test]
fn retried_loop_node_files_a_fresh_inbox_id() {
    let document = loop_retry_graph();
    let mut owner = Owner {
        observations: [terminal(
            AttemptOutcome::Succeeded,
            None,
            Some("VERDICT: FAIL"),
        )]
        .into(),
        reply: Some(chair_reply("stop")),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let stopped = stable_run(&document, &mut owner, &mut journal);
    let original = owner
        .filed
        .iter()
        .find(|ask| ask.kind == AskKind::Loop)
        .expect("initial loop request")
        .ask_id
        .clone();
    assert!(
        stopped
            .nodes
            .iter()
            .find(|n| n.id == "revise")
            .unwrap()
            .state
            .settled()
    );

    let library = template::TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::new();
    let limits = crate::config::GraphLimits {
        expansion_depth: 64,
        ..Default::default()
    };
    let context = retry_context(&library, &resolvers, &limits);
    let mut checkpoint = stopped;
    patch::retry_node(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "revise",
        None,
        &context,
    )
    .unwrap();
    journal.save(&checkpoint).unwrap();

    owner.reply = None;
    owner.observations.push_back(terminal(
        AttemptOutcome::Succeeded,
        None,
        Some("VERDICT: FAIL"),
    ));
    let waiting = stable_run(&document, &mut owner, &mut journal);
    let replacement = waiting
        .asks
        .iter()
        .find(|ask| ask.kind == AskKind::Loop)
        .expect("retry leaves a fresh request linked")
        .ask_id
        .clone();
    assert_ne!(replacement, original);
    assert_eq!(
        owner
            .filed
            .iter()
            .filter(|ask| ask.kind == AskKind::Loop)
            .map(|ask| ask.ask_id.as_str())
            .collect::<Vec<_>>(),
        [original.as_str(), replacement.as_str()]
    );
}
