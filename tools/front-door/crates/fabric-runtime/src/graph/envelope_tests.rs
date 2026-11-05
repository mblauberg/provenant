use super::*;
use serde_json::json;

/// Control characters take six canonical bytes each, the worst case.
fn worst(max: usize) -> String {
    bounded(&"\u{1}".repeat(max), max)
}

fn routes(count: usize) -> serde_json::Value {
    (0..count)
        .map(|index| json!({"adapter":"fixture", "model":format!("fixture/model-{index}")}))
        .collect::<Vec<_>>()
        .into()
}

fn mixed() -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"envelope-mixed", "delivery":"pr", "integration_branch":"integration/envelope",
        "decision_session":"chair", "nodes":[
            {"id":"plan", "kind":"plan", "access":"read_only", "prompt":"Slice the work",
             "route":{"alias":"senior"}, "plan":{"kinds":["task"], "max_nodes":4}},
            {"id":"review", "access":"read_only", "prompt":"Review", "routes":routes(3)},
            {"id":"choose", "kind":"decide", "access":"read_only", "depends_on":["review"],
             "decide":{"question":"Ship the reviewed change?", "default":"hold", "timeout_seconds":60,
                "options":[{"id":"ship", "label":"Ship"}, {"id":"hold", "label":"Hold", "description":"Wait for more review"}]}},
            {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["review"],
             "condition":{"type":"ci", "target":"feature/example"}},
            {"id":"repair", "kind":"loop", "access":"read_only", "depends_on":["ci"],
             "loop":{"max_rounds":3, "until":"accepted", "decision_session":"chair", "body":[
                {"id":"writer", "access":"worktree_write", "worktree":"/fixture/writer",
                 "prompt":"repair", "routes":routes(2)},
                {"id":"council", "kind":"composite", "access":"read_only", "depends_on":["writer"],
                 "source":{"type":"node","node":"writer"}, "composite":{"nodes":[
                    {"id":"lens", "access":"read_only", "prompt":"review the repair",
                     "route":{"alias":"senior"}, "depends_on":["writer"],
                     "source":{"type":"node","node":"writer"}}
                 ], "exports":[{"node":"lens","outputs":["result","verdict","findings"]}]}},
                {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["council"],
                 "inputs":[{"node":"council","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
             ]}}
        ]
    }))
    .unwrap()
}

fn planned(document: &GraphDocument) -> PlannedGraph {
    let limits = crate::config::GraphLimits::default();
    let expanded = plans::expand(document, &[], &[], &limits).unwrap();
    validate_flat_plan(&expanded, &limits).unwrap().0
}

/// Findings that fill `FINDINGS_BYTES`, the most `make_outputs` keeps.
fn findings() -> Vec<Finding> {
    let finding = |summary: String| Finding {
        extra: Default::default(),
        id: "F1".into(),
        severity: "high".into(),
        file: "src/lib.rs".into(),
        line: None,
        summary,
        status: "open".into(),
        disposition: None,
        reason: None,
    };
    let mut findings = vec![finding(String::new()); MAX_FINDINGS];
    let spare = FINDINGS_BYTES - canonical_len(&findings);
    findings[0].summary = "x".repeat(spare);
    assert_eq!(canonical_len(&findings), FINDINGS_BYTES);
    findings
}

fn filled_outputs(composite: bool, writer: bool) -> NodeOutputs {
    let text = "\u{1}".repeat(RESULT_EXCERPT_BYTES * 2);
    let mut capture = crate::result_capture::ResultCapture::new(None);
    capture.push(text.as_bytes());
    let mut outputs = make_outputs(
        Some(&capture.finish()),
        Some(worst(4 * WARNING_BYTES)),
        Some(worst(OUTPUT_FIELD_BYTES)),
        Some(worst(4 * OUTPUT_FIELD_BYTES)),
        Some(worst(4 * OUTPUT_FIELD_BYTES)),
    );
    outputs.verdict = Some(worst(OUTPUT_FIELD_BYTES));
    outputs.verdict_error = Some("verdict malformed, repeated or not final".into());
    outputs.findings = findings();
    if writer {
        outputs.warnings.push(worst(WARNING_BYTES));
    }
    if composite && let Some(result) = &mut outputs.result {
        result.excerpt = worst(COMPOSITE_EXCERPT_BYTES);
    }
    assert!(outputs.warnings.len() <= MAX_WARNINGS);
    outputs
}

/// An ask holding the longest deadline, answer and inbox error it can store.
fn filled_ask(mut ask: NodeAsk) -> NodeAsk {
    ask.wake_at = Some(envelope::latest());
    ask.answer = Some(AskAnswer {
        extra: Default::default(),
        source: AnswerSource::Answered,
        option: Some(worst(asks::OPTION_BYTES)),
        text: Some(worst(asks::ANSWER_BYTES)),
        actor: Some(worst(HOST_ID_BYTES)),
        at: Some(envelope::latest()),
        rejected: Some(worst(MAX_ADVANCE_ERROR_BYTES)),
        rejection_code: Some("c".repeat(envelope::REFUSAL_CODE_BYTES)),
    });
    ask.inbox_error = Some(worst(MAX_ADVANCE_ERROR_BYTES));
    ask
}

/// Every ask the plan can hold at once: each loop round, each decide node and
/// the pause history with one open and one awaiting its close.
fn filled_asks(plan: &PlannedGraph) -> Vec<NodeAsk> {
    let mut asks = Vec::new();
    for node in &plan.nodes {
        if let Some(session) = node
            .loop_body
            .as_ref()
            .and_then(|spec| spec.decision_session.as_deref())
        {
            let rounds = plan
                .nodes
                .iter()
                .filter_map(|n| {
                    n.round
                        .as_ref()
                        .filter(|r| r.loop_id == node.id)
                        .map(|r| r.round)
                })
                .max()
                .unwrap_or(0);
            asks.extend((1..=rounds).map(|round| {
                filled_ask(asks::loop_ask(
                    &plan.graph_id,
                    &node.id,
                    usize::MAX - round,
                    session,
                ))
            }));
        }
        if let Some(spec) = &node.land {
            asks.push(filled_ask(land::land_ask(
                &plan.graph_id,
                node,
                &filled_land(spec.tier),
                land::session(spec, plan).unwrap(),
            )));
        }
        if let Some(spec) = &node.decide {
            let session = asks::decide_session(spec, plan.decision_session.as_deref()).unwrap();
            asks.push(filled_ask(asks::decide_ask(
                &plan.graph_id,
                node,
                spec,
                session,
                u64::MAX,
                None,
                Some(&worst(MAX_ADVANCE_ERROR_BYTES)),
            )));
        }
    }
    if let Some(session) = &plan.decision_session {
        asks.extend((0..asks::MAX_PAUSE_ASKS as u64 + 2).map(|index| {
            filled_ask(asks::pause_ask(
                &plan.graph_id,
                u64::MAX - index,
                session,
                &worst(PAUSE_REASON_BYTES),
                Some(&worst(MAX_ADVANCE_ERROR_BYTES)),
            ))
        }));
    }
    asks
}

fn filled_land(tier: provenant_records::workflow::RiskTier) -> LandRecord {
    LandRecord {
        extra: Default::default(),
        tier,
        policy: crate::config::LandPolicy::Ask,
        policy_source: crate::config::LandSource::Project,
        branch: worst(OUTPUT_FIELD_BYTES),
        head: worst(OUTPUT_FIELD_BYTES),
        authorised_by: Some(LandAuthority::Chair {
            ask_id: format!("{}.ask-{}", "l".repeat(64), u64::MAX),
        }),
        pull_request: Some(u64::MAX),
        note: Some(worst(MAX_ADVANCE_ERROR_BYTES)),
        wake_at: Some(envelope::latest()),
        pending_observations: u64::MAX,
    }
}

fn filled_checkpoint(document: &GraphDocument, plan: &PlannedGraph) -> GraphCheckpoint {
    let longest = plan
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .max_by_key(String::len)
        .unwrap();
    let nodes = plan
        .nodes
        .iter()
        .map(|node| {
            let boundary = node.condition.is_some() || node.loop_body.is_some();
            NodeProgress {
                extra: Default::default(),
                children: node.composite_members.clone(),
                min_successful: node.composite.as_ref().and_then(|spec| spec.min_successful),
                source_commit: node.source.as_ref().map(|_| "1".repeat(64)),
                fragment: node.plan.as_ref().map(|_| "\"".repeat(plans::STORED_BLOCK_BYTES / 2 - 1)),
                handoff: plans::writer(node).then(|| "\"".repeat(plans::STORED_HANDOFF_BYTES / 2 - 1)),
                expansion_pause: (node.plan.is_some() || plans::writer(node))
                    .then(|| worst(MAX_ADVANCE_ERROR_BYTES)),
                loop_pause: node.loop_body.as_ref().map(|_| {
                    format!(
                        "loop {0} exhausted; decision cannot be applied: {1}; use graph resume --extend loop.{0}.rounds=N or --extend loop.{0}.decision=stop|accept",
                        node.id,
                        worst(MAX_ADVANCE_ERROR_BYTES)
                    )
                }),
                round: node.round.clone(),
                gate: node.condition.as_ref().map(|_| GateResult {
                    extra: Default::default(),
                    passed: false,
                    reason: Some(worst(OUTPUT_FIELD_BYTES)),
                    on_fail: node.on_fail.clone(),
                }),
                separation: (node.distinct_from.is_some() || node.family_group.is_some()).then(|| FamilySeparation {
                    extra: Default::default(),
                    strength: FamilyStrength::Require,
                    excluded: vec![worst(FAMILY_BYTES); SEPARATION_FAMILIES],
                    preferred: vec![worst(FAMILY_BYTES); SEPARATION_FAMILIES],
                    applied: Some(worst(FAMILY_BYTES)),
                    outcome: SeparationOutcome::Unavailable,
                }),
                ci: matches!(node.condition, Some(GateCondition::Ci { .. })).then(|| CiEvidence {
                    wake_at: Some(envelope::latest()),
                    pending_observations: u64::MAX,
                    extra: Default::default(),
                    first_observed_stamp: Some(CiHostStamp {
                        extra: Default::default(),
                        boot_identity: worst(HOST_ID_BYTES),
                        host_monotonic_micros: u64::MAX,
                    }),
                    clock_reanchors: u8::MAX,
                    clock_reanchor_reason: Some("previous checkpoint lacked a host stamp".into()),
                    state: CiState::Unavailable,
                    summary: worst(OUTPUT_FIELD_BYTES),
                    head_sha: Some("a".repeat(64)),
                    failed_checks: Vec::new(),
                    check_names: vec![worst(OUTPUT_FIELD_BYTES - 3)],
                    truncated: true,
                    elapsed_seconds: u64::MAX,
                }),
                land: node.land.as_ref().map(|spec| filled_land(spec.tier)),
                title: Some(node.title.clone().unwrap_or_else(|| node.id.clone())),
                worktree: node.worktree.clone(),
                id: node.id.clone(),
                state: NodeState::Succeeded,
                cancelled_from: Some(NodeState::Running),
                skip_reason: Some("graph cancelled".into()),
                advance_error: Some(worst(MAX_ADVANCE_ERROR_BYTES)),
                advance_error_code: Some("c".repeat(envelope::REFUSAL_CODE_BYTES)),
                launch_retry: (!boundary
                    && node.composite.is_none()
                    && node.decide.is_none()
                    && node.land.is_none())
                .then(|| LaunchRetry {
                    extra: Default::default(),
                    failures: u8::MAX,
                    deadline: Some(envelope::latest()),
                    wake_at: Some(envelope::latest()),
                    reason: Some(worst(MAX_ADVANCE_ERROR_BYTES)),
                }),
                loop_pause_code: node.loop_body.as_ref().map(|_| "c".repeat(envelope::REFUSAL_CODE_BYTES)),
                outputs: Some(if boundary {
                    gate_outputs(false)
                } else {
                    filled_outputs(node.composite.is_some(), plans::writer(node))
                }),
                attempts: node
                    .routes
                    .iter()
                    .enumerate()
                    .map(|(index, route)| GraphAttempt {
                        review_substitution: false,
                        extra: Default::default(),
                        launched: true,
                        key: AttemptKey {
                            extra: Default::default(),
                            graph_id: plan.graph_id.clone(),
                            node_id: node.id.clone(),
                            route_index: u16::MAX - index as u16,
                        },
                        route: route.clone(),
                        state: AttemptState::Unresolved,
                        attempt_ref: Some(worst(HOST_ID_BYTES)),
                        outcome: Some(AttemptOutcome::AdapterUnavailable),
                        launch_actor: worst(HOST_ID_BYTES),
                        input_cap: Some(usize::MAX),
                        settled: u64::MAX,
                        usage: Some(AttemptUsage {
                            extra: Default::default(),
                            tokens: Some(u64::MAX),
                            cost_micro_usd: Some(u64::MAX),
                            wall_time_seconds: Some(u64::MAX),
                            tokens_partial: true,
                            cost_partial: true,
                            cost_unknown_reason: Some(worst(NOTE_BYTES)),
                        }),
                    })
                    .collect(),
            }
        })
        .collect();
    GraphCheckpoint {
        extra: Default::default(),
        expansions: Vec::new(),
        remaining: Some(RemainingBudgets {
            extra: Default::default(),
            nodes: usize::MAX,
            edges: usize::MAX,
            continuations: usize::MAX,
        }),
        instance_id: Some("f".repeat(32)),
        asks: filled_asks(plan),
        ask_actor: Some(worst(HOST_ID_BYTES)),
        wake_at: Some(envelope::latest()),
        pause_asks: u64::MAX,
        delivery_required: true,
        delivered: true,
        pull_request: Some(worst(OUTPUT_FIELD_BYTES)),
        integration_pin: Some("1".repeat(64)),
        cancelled: true,
        cancel_reason: Some(worst(MAX_CANCEL_REASON_BYTES)),
        pause_reason: Some(worst(PAUSE_REASON_BYTES)),
        pause_code: Some("c".repeat(envelope::REFUSAL_CODE_BYTES)),
        unknown_usage: vec![
            format!(
                "node {longest} route {}: wall_time_seconds usage unknown",
                u16::MAX
            );
            MAX_UNKNOWN_USAGE
        ],
        unknown_usage_omitted: usize::MAX,
        usage_events: vec![
            UnknownUsageEvent {
                extra: Default::default(),
                node_id: longest.clone(),
                route_index: u16::MAX,
                metric: "wall_time_seconds".into(),
                reason: Some(worst(NOTE_BYTES)),
            };
            MAX_USAGE_EVENTS
        ],
        usage_events_omitted: u64::MAX,
        settlements: u64::MAX,
        extensions: vec![worst(256); 64],
        limit_notes: document_limit_notes(document),
        schema_version: 1,
        graph_id: plan.graph_id.clone(),
        plan_digest: format!("sha256:{}", "f".repeat(64)),
        nodes,
    }
}

#[test]
fn worst_case_checkpoint_fits_its_bound() {
    assert_fits(mixed());
}

#[test]
fn worst_case_land_checkpoint_fits_its_bound() {
    assert_fits(
        serde_json::from_value(json!({
            "schema_version":1, "graph_id":"envelope-land", "decision_session":"chair", "nodes":[
                {"id":"write", "access":"worktree_write", "worktree":"/fixture/writer", "delivery":"pr",
                 "prompt":"write", "routes":routes(2)},
                {"id":"review", "access":"read_only", "prompt":"Review", "depends_on":["write"],
                 "route":{"alias":"senior"}},
                {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["write"],
                 "condition":{"type":"ci", "target":"feature/example"}},
                {"id":"repair", "access":"worktree_write", "worktree":"/fixture/writer",
                 "continues":"write", "prompt":"repair", "depends_on":["ci"], "route":{"alias":"senior"}},
                {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["review", "repair"],
                 "inputs":[{"node":"review","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}},
                {"id":"land", "kind":"land", "access":"read_only", "depends_on":["accepted", "ci"],
                 "land":{"node":"write", "tier":"crucial", "session":"chair"}}
            ]
        }))
        .unwrap(),
    );
}

fn assert_fits(document: GraphDocument) {
    let plan = planned(&document);
    let checkpoint = filled_checkpoint(&document, &plan);
    let execution = GraphExecution {
        review_base: None,
        extra: Default::default(),
        integration_base: Some("1".repeat(40).parse().unwrap()),
        directory: format!("/{}", "\u{1}".repeat(4095)).into(),
        document: document.clone(),
    };
    let stored = StoredCheckpoint {
        extra: Default::default(),
        schema_version: 1,
        digest: CanonicalV1::record_digest(
            "fabric-graph-checkpoint",
            2,
            &(&checkpoint, &execution),
        )
        .unwrap()
        .to_string(),
        checkpoint,
        execution,
    };
    let size = canonical_len(&stored);
    let bound = envelope::checkpoint_bound(&document, &[], &stored.checkpoint.extensions, &plan);
    assert!(
        size <= bound,
        "filled checkpoint is {size} bytes, over its {bound}-byte bound"
    );
    assert!(bound < size * 2, "bound {bound} is loose against {size}");
}

#[test]
fn a_graph_at_the_default_node_limit_is_admitted() {
    let nodes = (0..crate::config::GraphLimits::default().nodes)
        .map(|index| json!({"id":format!("n{index}"), "access":"read_only", "prompt":"work", "route":{"alias":"senior"}}))
        .collect::<Vec<_>>();
    let document: GraphDocument =
        serde_json::from_value(json!({"schema_version":1, "graph_id":"wide", "nodes":nodes}))
            .unwrap();
    document.expanded(&Default::default()).unwrap();
}

#[test]
fn an_oversized_graph_is_refused_before_its_first_save_naming_the_size() {
    let nodes = (0..400)
        .map(|index| json!({"id":format!("n{index}"), "access":"read_only", "prompt":"work", "routes":routes(16)}))
        .collect::<Vec<_>>();
    let document: GraphDocument =
        serde_json::from_value(json!({"schema_version":1, "graph_id":"wide", "nodes":nodes}))
            .unwrap();
    let preflight = document.expanded(&Default::default()).unwrap_err();
    let mut journal = MemoryJournal::default();
    let error = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap_err();
    assert_eq!(error.code(), "invalid_graph");
    assert!(matches!(error, GraphError::Invalid(reason) if reason == preflight));
    assert!(journal.0.is_none());
}

#[test]
fn a_loop_extension_past_the_envelope_pauses_instead_of_growing() {
    let body = (0..18)
        .map(|index| json!({"id":format!("review{index}"), "access":"read_only", "prompt":"review", "routes":routes(16)}))
        .chain([json!({"id":"approved", "kind":"gate", "access":"read_only",
            "depends_on":(0..18).map(|index| format!("review{index}")).collect::<Vec<_>>(),
            "inputs":(0..18).map(|index| json!({"node":format!("review{index}"), "outputs":["verdict"]})).collect::<Vec<_>>(),
            "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}})])
        .collect::<Vec<_>>();
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"heavy-loop", "nodes":[{"id":"revise", "kind":"loop", "access":"read_only",
            "loop":{"max_rounds":1, "max_extension_rounds":40, "until":"approved", "body":body}}]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: (0..18)
            .map(|_| terminal_with_result(AttemptOutcome::Succeeded, "VERDICT: FAIL"))
            .collect(),
        ..Default::default()
    };
    let limits = crate::config::GraphLimits {
        expansion_depth: 64,
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let first = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    );
    assert_eq!(first.unwrap().status, GraphStatus::Paused);
    let mut cancelled_journal = MemoryJournal(journal.0.clone());
    cancelled_journal.0.as_mut().unwrap().cancelled = true;
    let cancelled = advance_with_budget(
        &document,
        &mut owner,
        &mut cancelled_journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(cancelled.status, GraphStatus::Cancelled);
    let refused = resume_with_budget(
        &document,
        &mut owner,
        &mut cancelled_journal,
        &limits,
        &Default::default(),
        &["loop.revise.rounds=25".into()],
    )
    .unwrap();
    assert_eq!(refused.status, GraphStatus::Cancelled);
    assert!(
        refused
            .checkpoint
            .nodes
            .iter()
            .all(|node| !matches!(node.state, NodeState::Pending | NodeState::Running))
    );
    assert_eq!(
        cancelled_journal.0.as_ref().unwrap().nodes,
        refused.checkpoint.nodes
    );
    let run = resume_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
        &["loop.revise.rounds=25".into()],
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Paused);
    assert_eq!(
        run.checkpoint.pause_code.as_deref(),
        Some("invalid_loop_decision")
    );
    assert!(run.checkpoint.extensions.is_empty());
    assert_eq!(owner.calls.len(), 18);
}

#[test]
fn usage_events_keep_the_newest_and_count_the_rest_without_churn() {
    let mut checkpoint = filled_checkpoint(&mixed(), &planned(&mixed()));
    checkpoint.usage_events.clear();
    checkpoint.usage_events_omitted = 0;
    let event = |index: usize| UnknownUsageEvent {
        extra: Default::default(),
        node_id: format!("n{index}"),
        route_index: 0,
        metric: "tokens".into(),
        reason: None,
    };
    for index in 0..MAX_USAGE_EVENTS + 2 {
        push_usage_event(&mut checkpoint, event(index));
    }
    push_usage_event(&mut checkpoint, event(MAX_USAGE_EVENTS + 1));
    assert_eq!(checkpoint.usage_events.len(), MAX_USAGE_EVENTS);
    assert_eq!(checkpoint.usage_events_omitted, 2);
    assert_eq!(checkpoint.usage_events[0], event(2));

    for node in &mut checkpoint.nodes {
        for attempt in &mut node.attempts {
            attempt.state = AttemptState::Terminal;
            attempt.usage = None;
        }
    }
    let first = checkpoint.nodes[0].attempts[0].clone();
    checkpoint.nodes[0].attempts = vec![first; MAX_UNKNOWN_USAGE];
    update_usage_warnings(&mut checkpoint);
    let lines = checkpoint.unknown_usage.clone();
    assert_eq!(lines.len(), MAX_UNKNOWN_USAGE);
    assert!(checkpoint.unknown_usage_omitted > 0);
    update_usage_warnings(&mut checkpoint);
    assert_eq!(checkpoint.unknown_usage, lines);
}

#[test]
fn stored_text_is_cut_by_its_canonical_size() {
    let mut capture = crate::result_capture::ResultCapture::new(None);
    capture.push("\u{1}".repeat(RESULT_EXCERPT_BYTES).as_bytes());
    let outputs = make_outputs(Some(&capture.finish()), None, None, None, None);
    let result = outputs.result.unwrap();
    assert!(result.truncated);
    assert!(encoded_len(&result.excerpt) <= RESULT_EXCERPT_BYTES + 2);
    assert_eq!(bounded("ab\u{1}", 7), "ab");
    assert_eq!(bounded("ab\u{1}", 8), "ab\u{1}");
}

#[test]
fn long_and_many_extensions_are_kept_and_count_toward_the_envelope() {
    let document = fixture_task();
    let mut journal = MemoryJournal::default();
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending { attempt_ref: None }].into(),
        ..Default::default()
    };
    advance(&document, &mut owner, &mut journal).unwrap();
    let plan = planned(&document);
    let without = envelope::checkpoint_bound(&document, &[], &[], &plan);
    let mut extensions = vec![format!("budget.per_graph.tokens={}", "9".repeat(18))];
    extensions.extend((1..=300).map(|index| format!("budget.per_graph.tokens={index}")));
    assert!(envelope::checkpoint_bound(&document, &[], &extensions, &plan) > without);
    let run = resume_with_budget(
        &document,
        &mut FakeOwner {
            observations: [AttemptObservation::Pending { attempt_ref: None }].into(),
            ..Default::default()
        },
        &mut journal,
        &Default::default(),
        &crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(1),
                ..Default::default()
            },
            ..Default::default()
        },
        &extensions,
    )
    .unwrap();
    assert_eq!(run.checkpoint.extensions.len(), 301);
}

fn fixture_task() -> GraphDocument {
    graph(vec![node("task", route("fixture"))])
}

#[test]
fn a_checkpoint_within_the_byte_limit_loads_past_the_old_member_limit() {
    use provenant_core::{Clock, ProjectId};
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let directory = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(directory.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    std::fs::create_dir(base.join("state")).unwrap();
    std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let project = ProjectId::from_bytes([9; 16]);
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let mut document = fixture_task();
    document
        .extra
        .insert("bulk".into(), json!(vec![0; 120_000]));
    let checkpoint = advance(
        &document,
        &mut FakeOwner {
            observations: [AttemptObservation::Pending { attempt_ref: None }].into(),
            ..Default::default()
        },
        &mut MemoryJournal::default(),
    )
    .unwrap()
    .checkpoint;
    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    journal
        .bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: base.clone(),
            document: document.clone(),
        })
        .unwrap();
    journal.save(&checkpoint).unwrap();
    assert_eq!(journal.load(&document.graph_id).unwrap(), Some(checkpoint));
}

#[test]
fn usage_warnings_keep_the_most_recently_settled_attempts() {
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let mut owner = FakeOwner {
        observations: [
            terminal(AttemptOutcome::Succeeded),
            terminal(AttemptOutcome::Succeeded),
        ]
        .into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    let settled: Vec<_> = checkpoint
        .nodes
        .iter()
        .map(|n| n.attempts[0].settled)
        .collect();
    assert_eq!((settled, checkpoint.settlements), (vec![1, 2], 2));

    // Node one settled last, so its lines are the newest despite node order.
    let copies = MAX_UNKNOWN_USAGE / 3 + 1;
    for (node, first) in [(0, 100), (1, 1)] {
        let attempt = checkpoint.nodes[node].attempts[0].clone();
        checkpoint.nodes[node].attempts = (0..copies as u64)
            .map(|i| GraphAttempt {
                review_substitution: false,
                settled: first + i,
                ..attempt.clone()
            })
            .collect();
    }
    update_usage_warnings(&mut checkpoint);
    assert_eq!(checkpoint.unknown_usage.len(), MAX_UNKNOWN_USAGE);
    assert!(
        checkpoint
            .unknown_usage
            .iter()
            .all(|line| line.starts_with("node one ")),
        "{:?}",
        checkpoint.unknown_usage
    );
}

#[test]
fn a_delivery_retry_takes_a_fresh_settlement_order() {
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let attempt = provenant_core::AttemptId::from_bytes([2; 16]);
    let document = graph(vec![
        node("one", route("first")),
        node("two", route("first")),
    ]);
    let mut failed = terminal(AttemptOutcome::OtherFailure);
    if let AttemptObservation::Terminal { attempt_ref, .. } = &mut failed {
        *attempt_ref = Some(format!("host/{attempt}"));
    }
    // One settles first and fails delivery; two settles after it.
    let mut owner = FakeOwner {
        observations: [failed, terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let limits = crate::config::GraphLimits::default();
    {
        let mut journal =
            FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
        journal
            .bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: None,
                directory: base.clone(),
                document: document.clone(),
            })
            .unwrap();
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        let settled: Vec<_> = run
            .checkpoint
            .nodes
            .iter()
            .map(|n| n.attempts[0].settled)
            .collect();
        assert_eq!(settled, [1, 2]);
    }
    FileJournal::retry_delivered_attempt(&root, project, attempt, budget, &cancel, &clock).unwrap();
    let reopened = FileJournal::read_checkpoint(&root, project, &document.graph_id)
        .unwrap()
        .unwrap();
    assert_eq!(reopened.nodes[0].attempts[0].settled, 0);

    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    let mut owner = FakeOwner {
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(
        (
            run.checkpoint.nodes[0].attempts[0].settled,
            run.checkpoint.settlements
        ),
        (3, 3)
    );
    assert!(
        run.checkpoint
            .unknown_usage
            .last()
            .unwrap()
            .starts_with("node one ")
    );
}

#[test]
fn ci_failed_checks_and_findings_fit_the_configured_envelope() {
    for cap in [0, 150, 16384, 65536] {
        for count in [0, 1, 20, 100] {
            let mut document = mixed();
            let Some(GateCondition::Ci {
                failed_checks_max_bytes,
                max_failed_checks,
                ..
            }) = &mut document.nodes[3].condition
            else {
                unreachable!()
            };
            *failed_checks_max_bytes = Some(cap);
            *max_failed_checks = Some(count);
            let plan = planned(&document);
            let mut checkpoint = filled_checkpoint(&document, &plan);
            for node in &mut checkpoint.nodes {
                if let Some(ci) = &mut node.ci {
                    let mut observation = CiObservation {
                        extra: Default::default(),
                        state: CiState::Failed,
                        summary: worst(OUTPUT_FIELD_BYTES),
                        head_sha: Some("a".repeat(40)),
                        check_names: Vec::new(),
                        truncated: false,
                        failed_checks: vec![
                            CiFailedCheck {
                                extra: Default::default(),
                                name: "build".into(),
                                url: Some("https://ci/1".into()),
                                excerpt: worst(cap)
                            };
                            count.saturating_add(1)
                        ],
                    };
                    observation.bound_failed_checks(cap, count);
                    ci.failed_checks = observation.failed_checks;
                    ci.truncated = observation.truncated;
                    node.outputs = Some(ci::outputs(false, Some(ci)));
                }
            }
            let execution = GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: Some("1".repeat(40).parse().unwrap()),
                directory: "/fixture".into(),
                document: document.clone(),
            };
            let stored = StoredCheckpoint {
                extra: Default::default(),
                schema_version: 1,
                digest: "sha256:".to_owned() + &"f".repeat(64),
                checkpoint,
                execution,
            };
            let bound =
                envelope::checkpoint_bound(&document, &[], &stored.checkpoint.extensions, &plan);
            assert!(canonical_len(&stored) <= bound, "cap={cap} count={count}");
        }
    }
}

/// The generated big-batch structure, with fixture-sized prompts, expands
/// every deferred panel at its configured maximum without growing its bound.
#[test]
fn five_and_seven_lane_big_batches_reserve_every_deferred_panel() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../assets/presets/fixtures/big-batch-five-lane-checkpoint.json"
    ))
    .unwrap();
    check_big_batch_panels(serde_json::from_value(fixture.clone()).unwrap());
    let mut larger = fixture;
    let lanes = larger["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|node| node["id"] == "lanes")
        .unwrap();
    for number in 5..7 {
        let composite = lanes["composite"].as_object_mut().unwrap();
        let original = composite["nodes"][0].to_string();
        let extra = original
            .replace("lane-l0", &format!("lane-l{number}"))
            .replace("/repo/w0", &format!("/repo/w{number}"));
        composite
            .get_mut("nodes")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(serde_json::from_str(&extra).unwrap());
        composite
            .get_mut("exports")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({"node":format!("lane-l{number}"),"outputs":["result","findings"]}));
    }
    check_big_batch_panels(serde_json::from_value(larger).unwrap());
}

fn check_big_batch_panels(document: GraphDocument) {
    let limits = crate::config::GraphLimits::default();
    document.admit(&limits).unwrap();
    let mut expanded = loops::expand(&document, &[], &limits).unwrap();
    let mut plan = validate_flat_plan(&expanded, &limits).unwrap().0;
    let reviews: Vec<_> = plan
        .nodes
        .iter()
        .filter(|node| node.review.is_some())
        .map(|node| node.id.clone())
        .collect();
    assert!(reviews.len() >= 5);
    let mut expansions = Vec::new();
    for id in reviews {
        let source = expanded
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap()
            .clone();
        let addition = fixture_review_expansion(&source, &"1".repeat(40));
        plans::validate_review_addition(&expanded, &addition).unwrap();
        let before = graph_envelope_bound(&document, &expansions, &[], &plan);
        plans::append(&mut expanded, &addition);
        expansions.push(addition);
        plan = validate_flat_plan(&expanded, &plans::generous()).unwrap().0;
        let after = graph_envelope_bound(&document, &expansions, &[], &plan);
        assert!(after <= before, "review expansion grew {before} to {after}");
        assert!(graph_envelope_reason(&document, &expansions, &[], &plan).is_none());
    }
}

fn fixture_review_expansion(source: &GraphNode, subject: &str) -> Expansion {
    let id = source.id.clone();
    let spec = source.review.as_ref().unwrap();
    let templates = source
        .extra
        .get("lens_templates")
        .and_then(serde_json::Value::as_object)
        .unwrap();
    let root_id = format!("{id}--review");
    let mut members = Vec::new();
    for template in templates.values().take(spec.max_nodes - 1) {
        let mut value = template.clone();
        let object = value.as_object_mut().unwrap();
        let local = object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_owned();
        object.remove("effort");
        object.insert("id".into(), json!(format!("{root_id}--{local}")));
        object.insert(
            "routes".into(),
            json!([{"alias":"senior"}, {"alias":"senior"}]),
        );
        object.insert("depends_on".into(), json!(source.depends_on));
        if !source.inputs.is_empty() {
            object.insert("inputs".into(), json!(source.inputs));
        }
        if !source.read_roots.is_empty() {
            object.insert("read_roots".into(), json!(source.read_roots));
        }
        object.insert(
            "source".into(),
            json!({"type":"commit","commit":subject.to_owned()}),
        );
        members.push(serde_json::from_value::<GraphNode>(value).unwrap());
    }
    let exports: Vec<_> = members
        .iter()
        .map(|member| json!({"node":member.id,"outputs":["result","findings"]}))
        .collect();
    let minimum = spec.min_successful.map(|n| {
        if n == 0 {
            members.len().saturating_sub(1).max(1)
        } else {
            n
        }
    });
    let root: GraphNode = serde_json::from_value(json!({"id":root_id,"kind":"composite","access":"read_only","depends_on":source.depends_on,"composite":{"min_successful":minimum,"nodes":[],"exports":exports}})).unwrap();
    let mut nodes = vec![root];
    nodes.extend(members);
    Expansion {
        review: Some(ReviewResolution {
            subject: subject.to_owned().parse().unwrap(),
            base: subject.to_owned().parse().unwrap(),
            measured_size: "large".into(),
            shape: "fixture".into(),
            source: "fixture".into(),
            substitutions: Vec::new(),
        }),
        extra: Default::default(),
        source: id,
        continuation: false,
        nodes,
        patterns: vec![],
    }
}

#[test]
fn oversized_deferred_review_is_refused_before_admission() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 2, "graph_id": "oversized-review", "nodes": [{
            "id": "review", "kind": "plan", "access": "read_only",
            "prompt": "review", "source": {"type":"commit", "commit":"1".repeat(40)},
            "review": {"max_nodes": MAX_CHECKPOINT_BYTES as usize / FINDINGS_BYTES + 1}
        }]
    }))
    .unwrap();
    let limits = crate::config::GraphLimits {
        nodes: MAX_CHECKPOINT_BYTES as usize / FINDINGS_BYTES + 10,
        ..Default::default()
    };
    let reason = document.admit(&limits).unwrap_err();
    assert!(matches!(reason, AdmissionError::Envelope(_)));
    let reason = reason.to_string();
    assert!(reason.contains("graph checkpoint could reach"), "{reason}");
    assert!(reason.contains("split the graph"), "{reason}");
}

#[test]
fn disposition_checkpoint_fits_its_reduced_bound() {
    let mut document = mixed();
    document
        .nodes
        .iter_mut()
        .find(|node| node.id == "review")
        .unwrap()
        .dispositions = vec!["accepted".into()];
    let plan = planned(&document);
    let mut checkpoint = filled_checkpoint(&document, &plan);
    let index = plan
        .nodes
        .iter()
        .position(|node| node.id == "review")
        .unwrap();
    let finding = Finding {
        extra: Default::default(),
        id: "F".into(),
        severity: "high".into(),
        file: worst(OUTPUT_FIELD_BYTES),
        line: Some(u64::MAX),
        summary: String::new(),
        status: "open".into(),
        disposition: Some("accepted".into()),
        reason: Some("ok".into()),
    };
    let outputs = checkpoint.nodes[index].outputs.as_mut().unwrap();
    outputs.findings.clear();
    loop {
        outputs.findings.push(finding.clone());
        if canonical_len(&outputs.findings) > DISPOSITION_FINDINGS_BYTES - 512 {
            outputs.findings.pop();
            break;
        }
    }
    let bound = graph_envelope_bound(&document, &[], &checkpoint.extensions, &plan);
    let execution = GraphExecution {
        review_base: None,
        extra: Default::default(),
        integration_base: Some("1".repeat(40).parse().unwrap()),
        directory: format!("/{}", "\u{1}".repeat(4095)).into(),
        document: document.clone(),
    };
    let size = |checkpoint: &GraphCheckpoint| {
        canonical_len(&StoredCheckpoint {
            extra: Default::default(),
            schema_version: 1,
            digest: "d".repeat(80),
            checkpoint: checkpoint.clone(),
            execution: execution.clone(),
        })
    };
    assert!(size(&checkpoint) <= bound);
    let outputs = checkpoint.nodes[index].outputs.as_mut().unwrap();
    outputs.findings.clear();
    outputs.extra.insert(
        "_compact_disposition_findings".into(),
        json!("\u{1}".repeat(16 * 1024)),
    );
    assert!(size(&checkpoint) <= bound);
}

#[test]
fn five_lane_big_batch_expands_through_runtime() {
    #[derive(Default)]
    struct FixtureOwner {
        launches: usize,
        deliveries: Vec<Option<provenant_records::run::DeliveryMode>>,
    }
    impl AttemptOwner for FixtureOwner {
        type Error = std::convert::Infallible;
        fn actor(&self) -> String {
            "fixture".into()
        }
        fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
            Ok(Some("1".repeat(40)))
        }
        fn resolve_review(
            &mut self,
            node: &GraphNode,
            subject: &str,
        ) -> Result<Expansion, ReviewResolveError> {
            Ok(fixture_review_expansion(node, subject))
        }
        fn observe_or_start(
            &mut self,
            _key: &AttemptKey,
            _node: NodeInput<'_>,
            _route: &GraphRoute,
            access: AccessMode,
            _worktree: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, Self::Error> {
            self.launches += 1;
            if access == AccessMode::WorktreeWrite {
                self.deliveries.push(replay.delivery.cloned());
            }
            let mut observation =
                terminal_with_result(AttemptOutcome::Succeeded, &findings_text(""));
            if let AttemptObservation::Terminal {
                attempt_ref,
                branch,
                commit,
                ..
            } = &mut observation
            {
                *attempt_ref = Some(format!("h/{}", self.launches));
                *branch = Some("batch/graph".into());
                *commit = Some("1".repeat(40));
            }
            Ok(observation)
        }
    }
    let document: GraphDocument = serde_json::from_str(include_str!(
        "../../../../assets/presets/fixtures/big-batch-five-lane-checkpoint.json"
    ))
    .unwrap();
    document.admit(&Default::default()).unwrap();
    let mut owner = FixtureOwner::default();
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        run.status,
        GraphStatus::Complete,
        "{:?}",
        run.checkpoint.pause_reason
    );
    assert!(run.checkpoint.successful());
    let errors: Vec<_> = run
        .checkpoint
        .nodes
        .iter()
        .filter_map(|node| node.advance_error.as_ref().map(|error| (&node.id, error)))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        run.checkpoint
            .expansions
            .iter()
            .filter(|expansion| expansion.review.is_some())
            .count()
            >= 5
    );
    assert!(owner.deliveries.len() >= 5);
    assert!(owner.deliveries.iter().all(|delivery| matches!(
        delivery,
        Some(provenant_records::run::DeliveryMode::Integrate(_))
    )));
}

#[test]
fn review_reservation_includes_declared_large_template_outputs() {
    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../assets/presets/fixtures/big-batch-five-lane-checkpoint.json"
    ))
    .unwrap();
    fn add_ci_limit(value: &mut serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(templates) = map
                    .get_mut("lens_templates")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    let template = templates.values_mut().next().unwrap();
                    template["condition"] =
                        json!({"type":"ci", "failed_checks_max_bytes": MAX_CHECKPOINT_BYTES});
                    return true;
                }
                map.values_mut().any(add_ci_limit)
            }
            serde_json::Value::Array(items) => items.iter_mut().any(add_ci_limit),
            _ => false,
        }
    }
    assert!(add_ci_limit(&mut fixture));
    let document: GraphDocument = serde_json::from_value(fixture).unwrap();
    let reason = document.admit(&Default::default()).unwrap_err().to_string();
    assert!(reason.contains("graph checkpoint could reach"), "{reason}");
    assert!(reason.contains("split the graph"), "{reason}");
}
