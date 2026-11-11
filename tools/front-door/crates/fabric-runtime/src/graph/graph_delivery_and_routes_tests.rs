use super::*;

#[test]
fn matched_required_contract_cannot_override_ambiguous_verdict_evidence() {
    let mut review = node("review", route("fixture"));
    review.required_verdict = Some("VERDICT: FAIL".into());
    let text = format!(
        "VERDICT: PASS\n{}\nVERDICT: FAIL\n{}",
        "x".repeat(5000),
        "y".repeat(20000)
    );
    let mut capture = crate::result_capture::ResultCapture::new(Some("VERDICT: FAIL"));
    capture.push(text.as_bytes());
    let mut observation = terminal(AttemptOutcome::Succeeded);
    if let AttemptObservation::Terminal { result, .. } = &mut observation {
        *result = Some(Box::new(capture.finish()));
    }
    let mut owner = FakeOwner {
        observations: VecDeque::from([observation]),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(
        run.checkpoint.nodes[0]
            .outputs
            .as_ref()
            .unwrap()
            .verdict
            .as_deref(),
        None
    );
}

#[test]
fn graph_delivers_nodes_and_opens_one_final_pr_only_after_success() {
    use provenant_records::run::DeliveryMode;
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/writer".into());
    writer.delivery = Some(DeliveryMode::Integrate("integration".into()));
    let mut document = graph(vec![writer]);
    document.integration_branch = Some("integration".into());
    document.delivery = Some(DeliveryMode::Pr);
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let result = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(result.checkpoint.delivered);
    assert_eq!(owner.deliveries, 1);
    assert_eq!(
        owner.delivery_modes,
        vec![Some(DeliveryMode::Integrate("integration".into()))]
    );
    advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.deliveries, 1);
    let mut failed = FakeOwner {
        observations: VecDeque::from([terminal(AttemptOutcome::OtherFailure)]),
        ..Default::default()
    };
    let result = advance(&document, &mut failed, &mut MemoryJournal::default()).unwrap();
    assert!(!result.checkpoint.delivered);
    assert_eq!(failed.deliveries, 0);
    document.nodes[0].access = AccessMode::ReadOnly;
    document.nodes[0].worktree = None;
    assert!(validate_plan(&document).is_err());
}

#[test]
fn graph_pr_identity_save_failure_reconciles_without_a_second_creation() {
    use provenant_records::run::DeliveryMode;
    #[derive(Default)]
    struct FailingJournal {
        checkpoint: Option<GraphCheckpoint>,
        before_failure: Option<Vec<u8>>,
        failed: bool,
    }
    impl GraphJournal for FailingJournal {
        type Error = std::io::Error;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self.checkpoint.clone())
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            if checkpoint.delivered && !self.failed {
                self.failed = true;
                self.before_failure = Some(serde_json::to_vec(&self.checkpoint).unwrap());
                return Err(std::io::Error::other("injected PR identity save failure"));
            }
            self.checkpoint = Some(checkpoint.clone());
            Ok(())
        }
    }
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/writer".into());
    writer.delivery = Some(DeliveryMode::Integrate("integration".into()));
    let mut document = graph(vec![writer]);
    document.integration_branch = Some("integration".into());
    document.delivery = Some(DeliveryMode::Pr);
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = FailingJournal::default();
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::Journal(_))
    ));
    assert_eq!(
        serde_json::to_vec(&journal.checkpoint).unwrap(),
        journal.before_failure.clone().unwrap()
    );
    let saved = journal.checkpoint.as_ref().unwrap();
    assert!(!saved.delivered && saved.pull_request.is_none());
    assert_eq!(owner.delivery_effects, 1);
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(replay.checkpoint.delivered);
    assert_eq!(
        replay.checkpoint.pull_request.as_deref(),
        Some("https://example.invalid/pr/1")
    );
    assert_eq!(owner.delivery_effects, 1);
    assert_eq!(owner.deliveries, 2);
}

#[test]
fn graph_pr_delivery_never_saves_missing_or_clipped_identity() {
    use provenant_records::run::DeliveryMode;
    for identity in [
        None,
        Some("x".repeat(OUTPUT_FIELD_BYTES + 1)),
        Some("bad\nurl".into()),
    ] {
        let mut writer = node("writer", route("fixture"));
        writer.access = AccessMode::WorktreeWrite;
        writer.worktree = Some(".worktrees/writer".into());
        writer.delivery = Some(DeliveryMode::Integrate("integration".into()));
        let mut document = graph(vec![writer]);
        document.integration_branch = Some("integration".into());
        document.delivery = Some(DeliveryMode::Pr);
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
            delivery_identity: Some(identity),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        assert!(matches!(
            advance(&document, &mut owner, &mut journal),
            Err(GraphError::Refused(_))
        ));
        let saved = journal.0.as_ref().unwrap();
        assert!(!saved.delivered && saved.pull_request.is_none());
        assert_eq!(saved.nodes[0].state, NodeState::Succeeded);
    }
}

#[test]
fn graph_refuses_node_integration_target_mismatch() {
    use provenant_records::run::DeliveryMode;
    let mut writer = node("writer", route("fixture"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/writer".into());
    writer.delivery = Some(DeliveryMode::Integrate("origin/other".into()));
    let mut document = graph(vec![writer]);
    document.integration_branch = Some("origin/integration".into());
    document.delivery = Some(DeliveryMode::Pr);
    assert_eq!(
        validate_plan(&document).err(),
        Some("node writer: node integration target differs from graph integration branch")
    );
}

#[test]
fn tries_routes_in_order_and_persists_each_terminal_attempt() {
    let routes = vec![
        route("first"),
        route("second"),
        route("third"),
        route("fourth"),
    ];
    let document = graph(vec![GraphNode {
        on_stop: Default::default(),
        attempt: None,
        attachments: Vec::new(),
        family_group: None,
        context: None,
        extra: Default::default(),
        composite_members: Vec::new(),
        topology_present: Default::default(),
        distinct_from: None,
        distinct_strength: None,
        dispositions: Vec::new(),
        composite: None,
        role: None,
        read_roots: Vec::new(),
        capabilities: None,
        source: None,
        after_expansion: Vec::new(),
        plan: None,
        write_scope: Vec::new(),
        acceptance_checks: Vec::new(),
        decide: None,
        continues: None,
        land: None,
        loop_body: None,
        round: None,
        kind: None,
        condition: None,
        on_fail: None,
        delivery: None,
        require_skill_location: false,
        access: AccessMode::ReadOnly,
        worktree: None,
        id: "node".to_owned(),
        title: None,
        deadline_seconds: None,
        prompt: "do the work".to_owned(),
        depends_on: Vec::new(),
        advisory: false,
        inputs: Vec::new(),
        route: None,
        routes: Some(routes.clone()),
        required_verdict: None,
        require_findings: false,
        skip_without_findings: None,
    }]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            preparation_refusal("host/a", AttemptOutcome::AuthRequired),
            preparation_refusal("host/b", AttemptOutcome::UsageLimited),
            preparation_refusal("host/c", AttemptOutcome::AdapterUnavailable),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();

    let run = advance(&document, &mut owner, &mut journal).expect("graph advances");

    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(
        owner
            .calls
            .iter()
            .map(|call| call.1.clone())
            .collect::<Vec<_>>(),
        routes
    );
    let attempts = &run.checkpoint.nodes[0].attempts;
    assert_eq!(attempts.len(), 4);
    assert_eq!(attempts[0].outcome, Some(AttemptOutcome::AuthRequired));
    assert_eq!(attempts[1].outcome, Some(AttemptOutcome::UsageLimited));
    assert_eq!(
        attempts[2].outcome,
        Some(AttemptOutcome::AdapterUnavailable)
    );
    assert_eq!(attempts[3].outcome, Some(AttemptOutcome::Succeeded));
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
}

#[test]
fn read_only_sign_in_failure_falls_back_to_next_route() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "signed-out", "nodes": [{
            "id": "review", "prompt": "review", "access": "read_only", "routes": [
                {"adapter": "cursor", "model": "cursor/example"},
                {"adapter": "codex", "model": "codex/example"}
            ]
        }]
    }))
    .expect("graph fixture");
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            preparation_refusal("host/a", AttemptOutcome::AuthRequired),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let run =
        advance(&document, &mut owner, &mut MemoryJournal::default()).expect("fallback advances");
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(
        run.checkpoint.nodes[0].attempts[0].outcome,
        Some(AttemptOutcome::AuthRequired)
    );
    assert_eq!(
        run.checkpoint.nodes[0].attempts[1].outcome,
        Some(AttemptOutcome::Succeeded)
    );
}

#[test]
fn route_fallback_preserves_each_selected_account() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "accounts", "nodes": [{
            "id": "review", "prompt": "review", "access": "read_only", "routes": [
                {"adapter": "claude", "model": "claude/model", "account": "work"},
                {"adapter": "claude", "model": "claude/model", "account": "pro"}
            ]
        }]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            preparation_refusal("host/first", AttemptOutcome::AuthRequired),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(
        owner
            .calls
            .iter()
            .map(|call| call.1.account.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("work"), Some("pro")]
    );
    assert_eq!(
        run.checkpoint.nodes[0].attempts[1].route.account.as_deref(),
        Some("pro")
    );
}

#[test]
fn single_route_form_remains_valid() {
    let document: GraphDocument = serde_json::from_str(
        r#"{"schema_version":1,"graph_id":"g-test","nodes":[{"id":"one","prompt":"work for one","access":"read_only","route":{"adapter":"only","alias":"only-alias","effort":"high"}}]}"#,
    )
    .expect("legacy single-route document parses");
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();

    let run = advance(&document, &mut owner, &mut journal).expect("single route advances");

    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(run.checkpoint.nodes[0].attempts.len(), 1);
}

#[test]
fn pool_only_and_mixed_routes_advance_in_order() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "pools", "nodes": [
            {"id": "one", "prompt": "work", "access": "read_only", "route": {"alias": "senior"}},
            {"id": "two", "prompt": "work", "access": "read_only", "depends_on": ["one"], "routes": [
                {"adapter": "codex", "model": "codex/model"}, {"alias": "senior"}
            ]}
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::Succeeded),
            preparation_refusal("host/1", AttemptOutcome::AuthRequired),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.calls.len(), 3);
    assert_eq!(owner.calls[0].1.adapter, None);
    assert_eq!(owner.calls[1].1.adapter.as_deref(), Some("codex"));
    assert_eq!(owner.calls[2].1.alias.as_deref(), Some("senior"));
}

#[test]
fn route_effort_is_checked_against_the_adapter_table() {
    for (adapter, accepted) in [
        ("claude", true),
        ("codex", true),
        ("agy", true),
        ("opencode", true),
        ("cursor", false),
    ] {
        let mut leaf = route(adapter);
        leaf.effort = Some("high".into());
        let document = graph(vec![node("one", leaf)]);
        assert_eq!(
            validate_plan(&document).err(),
            (!accepted).then_some("node one: effort is not supported by the route's adapter"),
            "{adapter}"
        );
    }
}

#[test]
fn host_native_requires_a_later_non_native_fallback() {
    let mut host = route("host-native");
    host.model = None;
    host.name = Some("ci-fixer".into());
    let mut repair = node("repair", host.clone());
    repair.access = AccessMode::WorktreeWrite;
    repair.worktree = Some("/repo/writer".into());
    assert_eq!(
        validate_plan(&graph(vec![repair.clone()])).err(),
        Some("node repair: host-native route requires a later non-native fallback")
    );
    repair.route = None;
    repair.routes = Some(vec![host.clone(), host.clone()]);
    assert_eq!(
        validate_plan(&graph(vec![repair.clone()])).err(),
        Some("node repair: host-native route requires a later non-native fallback")
    );
    repair.routes = Some(vec![host, route("claude")]);
    assert!(validate_plan(&graph(vec![repair])).is_ok());
}

#[test]
fn named_adapter_route_has_no_model_and_rejects_mixed_selectors() {
    let mut leaf = route("command");
    leaf.model = None;
    leaf.name = Some("fmt-check".into());
    assert!(validate_plan(&graph(vec![node("check", leaf.clone())])).is_ok());
    for field in ["model", "alias", "effort", "account"] {
        let mut value = serde_json::to_value(&leaf).unwrap();
        value[field] = "other".into();
        let mixed = serde_json::from_value(value).unwrap();
        assert!(
            validate_plan(&graph(vec![node("check", mixed)])).is_err(),
            "{field}"
        );
    }
    leaf.adapter = None;
    assert_eq!(
        validate_plan(&graph(vec![node("check", leaf)])).err(),
        Some("node check: route requires an adapter or alias")
    );
}

#[test]
fn route_requires_adapter_or_alias() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "g", "nodes": [{
            "id": "one", "prompt": "work", "access": "read_only", "route": {}
        }]
    }))
    .unwrap();
    assert_eq!(
        validate_plan(&document).err(),
        Some("node one: route requires an adapter or alias")
    );
}

#[test]
fn route_with_exact_model_alias_fails_validation_for_dry_run_and_launch() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "g", "nodes": [{
            "id": "one", "prompt": "work", "access": "read_only", "route": { "alias": "codex/gpt-6-luna" }
        }]
    }))
    .unwrap();
    assert_eq!(
        validate_plan(&document).err(),
        Some("node one: route alias must be an identifier; exact models require an adapter or --model")
    );
    let mut exact = document.clone();
    let route = exact.nodes[0].route.as_mut().unwrap();
    route.adapter = Some("codex".into());
    route.model = Some("gpt-6-luna".into());
    assert!(validate_plan(&exact).is_ok());
}

#[test]
fn pending_and_unknown_keep_observing_the_same_route_without_fallback() {
    for first in [
        AttemptObservation::Pending { attempt_ref: None },
        AttemptObservation::Unknown { attempt_ref: None },
    ] {
        let routes = vec![route("first"), route("second")];
        let document = graph(vec![GraphNode {
            on_stop: Default::default(),
            attempt: None,
            attachments: Vec::new(),
            family_group: None,
            context: None,
            extra: Default::default(),
            composite_members: Vec::new(),
            topology_present: Default::default(),
            distinct_from: None,
            distinct_strength: None,
            dispositions: Vec::new(),
            composite: None,
            role: None,
            read_roots: Vec::new(),
            capabilities: None,
            source: None,
            after_expansion: Vec::new(),
            plan: None,
            write_scope: Vec::new(),
            acceptance_checks: Vec::new(),
            decide: None,
            continues: None,
            land: None,
            loop_body: None,
            round: None,
            kind: None,
            condition: None,
            on_fail: None,
            delivery: None,
            require_skill_location: false,
            access: AccessMode::ReadOnly,
            worktree: None,
            id: "node".to_owned(),
            title: None,
            deadline_seconds: None,
            prompt: "do the work".to_owned(),
            depends_on: Vec::new(),
            advisory: false,
            inputs: Vec::new(),
            route: None,
            routes: Some(routes.clone()),
            required_verdict: None,
            require_findings: false,
            skip_without_findings: None,
        }]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([first, terminal(AttemptOutcome::Succeeded)]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();

        let pending = advance(&document, &mut owner, &mut journal).expect("first observation");
        assert_eq!(pending.status, GraphStatus::Pending);
        assert_eq!(pending.checkpoint.nodes[0].attempts.len(), 1);
        assert_eq!(owner.calls[0].1, routes[0]);

        let completed = advance(&document, &mut owner, &mut journal).expect("same attempt settles");
        assert_eq!(completed.status, GraphStatus::Complete);
        assert_eq!(completed.checkpoint.nodes[0].attempts.len(), 1);
        assert_eq!(owner.calls[1].0, owner.calls[0].0);
        assert_eq!(owner.calls[1].1, routes[0]);
    }
}

#[test]
fn writer_limit_after_launch_restarts_in_place_on_the_second_route() {
    let mut writer = node("writer", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("/private/tmp/writer".into());
    writer.route = None;
    writer.routes = Some(vec![route("first"), route("second")]);
    let document = graph(vec![writer]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::UsageLimited),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[1].1, route("second"));
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
}

#[test]
fn usage_limit_failure_reports_cause_instead_of_missing_findings() {
    let mut pool = route("senior");
    pool.adapter = None;
    pool.model = None;
    pool.alias = Some("senior".into());
    let document = graph(vec![node("task", pool)]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([terminal_with_result(AttemptOutcome::UsageLimited, "")]),
        ..FakeOwner::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let node = &run.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(node.advance_error_code.as_deref(), Some("usage_limited"));
    assert!(
        node.advance_error
            .as_deref()
            .unwrap()
            .contains("quota or rate limit")
    );
    assert!(
        !node
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("findings block missing"))
    );
}

#[test]
fn exhausted_pool_names_the_cause_and_the_pool() {
    let alias = GraphRoute {
        extra: Default::default(),
        adapter: None,
        name: None,
        model: None,
        alias: Some("junior".into()),
        effort: None,
        account: None,
    };
    let mut lens = node("lens", route("first"));
    lens.route = None;
    lens.routes = Some(vec![alias.clone(), alias]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            preparation_refusal("host/1", AttemptOutcome::UsageLimited),
            preparation_refusal("host/2", AttemptOutcome::UsageLimited),
        ]),
        ..FakeOwner::default()
    };
    let run = advance(
        &graph(vec![lens]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let node = &run.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    assert_eq!(node.attempts.len(), 2);
    assert_eq!(node.advance_error_code.as_deref(), Some("usage_limited"));
    let error = node.advance_error.as_deref().unwrap();
    assert!(error.contains("quota or rate limit"), "{error}");
    assert!(error.contains("pool junior"), "{error}");
}

#[test]
fn fallback_outcomes_are_named_by_access_and_launch_state() {
    for (case, access, launched, outcome, expected) in [
        (
            "writer usage limit before launch",
            AccessMode::WorktreeWrite,
            false,
            AttemptOutcome::UsageLimited,
            true,
        ),
        (
            "writer auth refusal before launch",
            AccessMode::WorktreeWrite,
            false,
            AttemptOutcome::AuthRequired,
            true,
        ),
        (
            "writer unavailable adapter before launch",
            AccessMode::WorktreeWrite,
            false,
            AttemptOutcome::AdapterUnavailable,
            true,
        ),
        (
            "reader usage limit after launch",
            AccessMode::ReadOnly,
            true,
            AttemptOutcome::UsageLimited,
            true,
        ),
        (
            "reader auth refusal after launch",
            AccessMode::ReadOnly,
            true,
            AttemptOutcome::AuthRequired,
            true,
        ),
        (
            "reader unavailable adapter after launch",
            AccessMode::ReadOnly,
            true,
            AttemptOutcome::AdapterUnavailable,
            false,
        ),
        (
            "writer usage limit after launch",
            AccessMode::WorktreeWrite,
            true,
            AttemptOutcome::UsageLimited,
            false,
        ),
        (
            "reader task failure after launch",
            AccessMode::ReadOnly,
            true,
            AttemptOutcome::TaskFailed,
            false,
        ),
        (
            "reader review required before launch",
            AccessMode::ReadOnly,
            false,
            AttemptOutcome::ReviewRequired,
            false,
        ),
        (
            "reader unclassified failure after launch",
            AccessMode::ReadOnly,
            true,
            AttemptOutcome::OtherFailure,
            false,
        ),
        (
            "reader deadline before launch",
            AccessMode::ReadOnly,
            false,
            AttemptOutcome::DeadlineExceeded,
            false,
        ),
        (
            "writer auth refusal after launch",
            AccessMode::WorktreeWrite,
            true,
            AttemptOutcome::AuthRequired,
            false,
        ),
        (
            "writer unavailable adapter after launch",
            AccessMode::WorktreeWrite,
            true,
            AttemptOutcome::AdapterUnavailable,
            false,
        ),
    ] {
        check_fallback(case, access, launched, outcome, expected);
    }
}

#[test]
fn launched_reader_fallback_respects_retry_budget() {
    let mut task = node("node", route("first"));
    task.access = AccessMode::ReadOnly;
    task.route = None;
    task.routes = Some(vec![route("first"), route("second")]);
    let document = graph(vec![task]);
    let mut owner = FakeOwner {
        retry_limit: Some(0),
        observations: VecDeque::from([AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::UsageLimited,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        }]),
        ..FakeOwner::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        run.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("fallback_budget_exhausted")
    );
}

#[test]
fn launched_reader_reports_pinned_route() {
    let task = node("node", route("first"));
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::UsageLimited,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        }]),
        ..FakeOwner::default()
    };
    let run = advance(
        &graph(vec![task]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(
        run.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("fallback_route_pinned")
    );
}

#[test]
fn launched_reader_fallback_keeps_account_pin() {
    let mut task = node("node", route("first"));
    let mut first = route("first");
    first.account = Some("account-one".into());
    let mut second = route("second");
    second.account = Some("account-two".into());
    task.route = None;
    task.routes = Some(vec![first, second]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::AuthRequired,
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        }]),
        ..FakeOwner::default()
    };
    let run = advance(
        &graph(vec![task]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(
        run.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("fallback_account_pinned")
    );
}

#[test]
fn fallback_reports_unreadable_evidence() {
    let mut task = node("node", route("first"));
    task.route = None;
    task.routes = Some(vec![route("first"), route("second")]);
    let mut owner = FakeOwner {
        fail_fallback_evidence: true,
        observations: VecDeque::from([AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::AuthRequired,
            launched: false,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        }]),
        ..FakeOwner::default()
    };
    let run = advance(
        &graph(vec![task]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(
        run.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("fallback_evidence_unreadable")
    );
}

#[test]
fn live_fallback_without_release_proof_stops() {
    let mut task = node("node", route("first"));
    task.route = None;
    task.routes = Some(vec![route("first"), route("second")]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([preparation_refusal(
            "host/attempt",
            AttemptOutcome::AuthRequired,
        )]),
        omit_fallback_evidence: true,
        fallback_cause: Some(provenant_records::RouteExclusionReason::StaleAvailability),
        ..Default::default()
    };
    let result = advance(
        &graph(vec![task]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(result.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(owner.calls.len(), 1);
    let reason = result.checkpoint.nodes[0].advance_error.as_deref().unwrap();
    assert!(reason.contains("authentication"));
    assert!(reason.contains("custody"));
    assert!(reason.contains("stale"));
    assert!(reason.contains("provenant quota tick"));
}

#[test]
fn resumed_fallback_requires_current_local_custody_evidence() {
    let mut task = node("node", route("first"));
    task.route = None;
    task.routes = Some(vec![route("first"), route("second")]);
    let document = graph(vec![task]);
    let first = AttemptObservation::Terminal {
        attempt_ref: Some("host/attempt".into()),
        outcome: AttemptOutcome::AuthRequired,
        launched: false,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    };
    let mut owner = FakeOwner {
        observations: VecDeque::from([first, terminal(AttemptOutcome::Succeeded)]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let result = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(result.checkpoint.nodes[0].attempts.len(), 2);
    let mut checkpoint = result.checkpoint;
    let progress = &mut checkpoint.nodes[0];
    progress.attempts.pop();
    progress.state = NodeState::Running;
    progress.outputs = None;
    let mut restarted = FakeOwner::default();
    let mut journal = MemoryJournal(Some(checkpoint));
    let stopped = advance(&document, &mut restarted, &mut journal).unwrap();
    assert_eq!(stopped.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        stopped.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("fallback_evidence_unavailable")
    );
    assert!(restarted.calls.is_empty());
    let reason = stopped.checkpoint.nodes[0]
        .advance_error
        .as_deref()
        .unwrap();
    assert!(reason.contains("authentication"));
    assert!(reason.contains("custody"));
}

#[test]
fn checkpoint_attempts_require_launch_evidence_and_actor() {
    let document = graph(vec![node("node", route("route"))]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([AttemptObservation::Pending { attempt_ref: None }]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    for member in ["launched", "launch_actor"] {
        let mut value = serde_json::to_value(&run.checkpoint).unwrap();
        value["nodes"][0]["attempts"][0]
            .as_object_mut()
            .unwrap()
            .remove(member);
        assert!(serde_json::from_value::<GraphCheckpoint>(value).is_err());
    }
}
