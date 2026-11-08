#[allow(unused_imports)]
use super::*;

#[test]
fn unmatched_settlement_hints_wait_for_tick() -> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_graph_routing::unmatched_settlement_hints_wait_for_tick")? { return Ok(()); }
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
            graph_directory: None,
            configuration_root: fixture.rig.state.clone(), cwd: fixture.rig.project.clone(), executables: Default::default(),
            executable_search_path: Vec::new(), git: provenant_os::git::git_executable().ok_or("git")?, default_account_directories: Default::default(),
            credential_roots: Default::default(),
            github_roots: Default::default(),
            provider_path_environment: Default::default(),
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
        "fabric::peer_tests::peer_graph_routing::settlement_without_hint_does_not_advance_unrelated_graphs",
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
        graph_directory: None,
            configuration_root: fixture.rig.state.clone(),
        cwd: fixture.rig.project.clone(),
        executables: Default::default(),
        executable_search_path: Vec::new(),
        git: provenant_os::git::git_executable().ok_or("git")?,
        default_account_directories: Default::default(),
        credential_roots: Default::default(),
        github_roots: Default::default(),
        provider_path_environment: Default::default(),
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
    if host_test("fabric::peer_tests::peer_graph_routing::graph_owner_fails_node_when_result_integrity_is_lost")? {
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
        let mut owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(),
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
    let mut owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(),
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
        "fabric::peer_tests::peer_graph_routing::exact_route_in_an_excluded_family_is_refused_before_launch",
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
    let mut owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(),
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
    if host_test("fabric::peer_tests::peer_graph_routing::production_owner_panel_uses_distinct_families_and_synthesises")? {
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
    let mut owner = BoundGraphOwner { writer_launch_budget: None, admission_timeout: None, launch_warnings: Vec::new(), repo_facts: Default::default(), factory: &factory, integration_base: None, integration_branch: None,
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
                    // The fixture launches its pick synthetically, so an unready
                    // redraw must fail here rather than publish as a launched route.
                    if !matches!(
                        selected.readiness,
                        provenant_records::RouteReadiness::Ready { .. }
                    ) {
                        return Err(format!(
                            "fixture selected an unready route: {:?}",
                            selected.readiness
                        )
                        .into());
                    }
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
