use super::*;
use serde_json::json;

/// Measure the actual journal representation after durable text externalisation.
fn persisted_len(stored: &StoredCheckpoint) -> usize {
    let temp = tempfile::tempdir().unwrap();
    let directory = Directory::open_anchor(temp.path()).unwrap();
    let mut value = serde_json::to_value(stored).unwrap();
    prompt_store::externalise(&directory, "envelope", &mut value).unwrap();
    canonical_len(&value)
}

/// Control characters take six canonical bytes each, the worst case.
fn worst(max: usize) -> String {
    bounded(&"\u{1}".repeat(max), max)
}

/// Quotes escape to two bytes each: the largest handoff the record admits.
fn worst_handoff() -> provenant_records::handoff::Handoff {
    provenant_records::handoff::Handoff {
        schema_version: 2,
        status: provenant_records::handoff::HandoffStatus::StoppedEarly,
        note: "\"".repeat(provenant_records::handoff::NOTE_BYTES),
        remaining: vec!["\"".repeat(1900)],
        decisions: Vec::new(),
        verify: Vec::new(),
        spec_concerns: Vec::new(),
        extra: Default::default(),
    }
}

fn worst_decision() -> provenant_records::FindingDecision {
    provenant_records::FindingDecision::new(
        "\"".repeat(provenant_records::finding_decision::FINDING_ID_BYTES / 2),
        provenant_records::FindingDisposition::Replace,
        "\"".repeat(provenant_records::finding_decision::DECISION_REASON_BYTES / 2),
        provenant_core::GitOid::parse(&"1".repeat(64)).unwrap(),
        Some(provenant_core::GitOid::parse(&"2".repeat(64)).unwrap()),
    )
    .unwrap()
}

fn routes(count: usize) -> serde_json::Value {
    (0..count)
        .map(|index| json!({"adapter":"fixture", "model":format!("fixture/model-{index}")}))
        .collect::<Vec<_>>()
        .into()
}

fn mixed() -> GraphDocument {
    let lens = json!({
        "id":"lens", "access":"read_only", "prompt":"review the repair",
        "review_behaviour":"lens", "review_lens":"correctness",
        "route":{"alias":"senior"}, "depends_on":["writer"],
        "source":{"type":"node","node":"writer"}
    });
    serde_json::from_value(json!({
        "schema_version":1, "graph_id":"envelope-mixed", "delivery":"pr", "integration_branch":"integration/envelope",
        "decision_session":"chair", "nodes":[
            {"id":"plan", "kind":"task", "access":"read_only", "prompt":"Slice the work",
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
                 "source":{"type":"node","node":"writer"}, "composite":{"nodes":[lens], "exports":[{"node":"lens","outputs":["result","verdict","findings"]}]}},
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

/// Findings that fill the node's canonical-byte allowance.
fn findings(cap: usize) -> Vec<Finding> {
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
    let spare = cap - canonical_len(&findings);
    // Long individual strings are externalised; spread the allowance so the
    // findings remain inline in the checkpoint representation being measured.
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.summary = "x".repeat(spare / MAX_FINDINGS + usize::from(index < spare % MAX_FINDINGS));
    }
    assert_eq!(canonical_len(&findings), cap);
    findings
}

fn filled_outputs(node: &PlannedNode) -> NodeOutputs {
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
    outputs.extra.insert(review::PARTIAL.into(), true.into());
    outputs.verdict = Some(worst(OUTPUT_FIELD_BYTES));
    outputs.verdict_error = Some("verdict malformed, repeated or not final".into());
    let cap = if node.dispositions.is_empty()
        && node.composite.is_none()
        && !node.inputs.iter().any(|input| input.outputs.contains(&OutputKind::Findings))
    {
        FINDINGS_BYTES
    } else {
        DISPOSITION_FINDINGS_BYTES
    };
    outputs.findings = findings(cap);
    if plans::writer(node) {
        outputs.warnings.push(worst(WARNING_BYTES));
    }
    if node.composite.is_some() && let Some(result) = &mut outputs.result {
        result.excerpt = worst(COMPOSITE_EXCERPT_BYTES);
    }
    assert!(outputs.warnings.len() <= MAX_WARNINGS);
    outputs
}

/// The link an ask leaves in the checkpoint, holding the longest deadline and
/// request ID it can store.
fn filled_ask(ask: AskRequest) -> AskLink {
    AskLink {
        extra: Default::default(),
        ask_id: ask.ask_id,
        kind: ask.kind,
        node: ask.node,
        rounds: ask.rounds,
        session: ask.session,
        wake_at: Some(envelope::latest()),
        request: "f".repeat(asks::REQUEST_ID_BYTES),
    }
}

/// Every link the plan can hold at once: each loop round, each decide node and
/// the one open pause.
fn filled_asks(plan: &PlannedGraph) -> Vec<AskLink> {
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
                let mut ask = asks::loop_ask(
                    &plan.graph_id,
                    &node.id,
                    usize::MAX - round,
                    session,
                );
                ask.ask_id.push_str(&format!(".retry-{}", u64::MAX));
                filled_ask(ask)
            }));
        }
        if let Some(spec) = &node.land {
            asks.push(filled_ask(land::land_ask(
                &plan.graph_id,
                node,
                &filled_land(spec.tier),
                land::session(spec, plan).unwrap(),
                1,
                u64::MAX,
            )));
        }
        if let Some(spec) = &node.decide {
            let session = asks::decide_session(spec, plan.decision_session.as_deref()).unwrap_or(&plan.graph_id);
            let mut ask = asks::decide_ask(
                &plan.graph_id,
                node,
                spec,
                session,
                u64::MAX,
                None,
                Some(&worst(MAX_ADVANCE_ERROR_BYTES)),
            );
            // A node raised again after a retry carries the generation.
            ask.ask_id.push_str(&format!(".retry-{}", u64::MAX));
            asks.push(filled_ask(ask));
        }
    }
    if let Some(session) = &plan.decision_session {
        asks.push(filled_ask(asks::pause_ask(
            &plan.graph_id,
            u64::MAX,
            session,
            &worst(PAUSE_REASON_BYTES),
            Some(&worst(MAX_ADVANCE_ERROR_BYTES)),
        )));
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
                extra: if template::review_behaviour(node) == Some(template::ReviewBehaviour::Lens) {
                    [(review::CORRECTION_REFUSED.into(), worst(MAX_ADVANCE_ERROR_BYTES).into())].into()
                } else {
                    Default::default()
                },
                children: node.composite_members.clone(),
                source_binding: node.source.clone(),
                min_successful: node.composite.as_ref().and_then(|spec| spec.min_successful),
                source_commit: node.source.as_ref().map(|_| "1".repeat(64)),
                writer_binding: None,
        writer_binding_pending: plans::writer(node),
                review_base: node.extra.get("review_base").map(|_| "1".repeat(64)),
        review_base_pending: false,
                fragment: node.plan.as_ref().map(|_| "\"".repeat(plans::STORED_BLOCK_BYTES / 2 - 1)),
                handoff: plans::writer(node).then(worst_handoff),
                blocked: true,
                decisions: if template::review_node(node) {
                    vec![worst_decision()]
                } else {
                    Vec::new()
                },
                expansion_pause: (node.plan.is_some() || plans::writer(node))
                    .then(|| worst(MAX_ADVANCE_ERROR_BYTES)),
                expansion_cause: (node.plan.is_some() || plans::writer(node)).then_some(NodeDecisionCause::PlanExpansionPaused { active: false, fallback: false, fragment_error: Some(PlanFailureFact::Structural(GraphStructuralLimit { key: GraphLimitKey::GraphPromptBytes, actual: usize::MAX, max: usize::MAX })) }),
                loop_pause: node.loop_body.as_ref().map(|_| {
                    format!(
                        "loop {0} exhausted; decision cannot be applied: {1}; use graph resume --extend loop.{0}.rounds=N or --extend loop.{0}.decision=stop|accept",
                        node.id,
                        worst(MAX_ADVANCE_ERROR_BYTES)
                    )
                }),
                loop_cause: node.loop_body.as_ref().map(|_| NodeDecisionCause::InvalidLoopDecision),
                truncation_cause: node.composite.as_ref().map(|_| NodeDecisionCause::CompositeFindingsTruncated {
                    warning_indices: vec![usize::MAX; MAX_WARNINGS], notice_lines: usize::MAX,
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
                advance_owner_code: Some("c".repeat(envelope::REFUSAL_CODE_BYTES)),
                launch_retry: None,
                last_launch_retry: (!boundary
                    && node.composite.is_none()
                    && node.decide.is_none()
                    && node.land.is_none())
                .then(|| LaunchRetry {
                    extra: Default::default(),
                    policy: LaunchRetryPolicy::default(),
                    failures: u8::MAX,
                    deadline: Some(envelope::latest()),
                    wake_at: Some(envelope::latest()),
                    reason: Some(worst(MAX_ADVANCE_ERROR_BYTES)),
                }),
                loop_pause_code: node.loop_body.as_ref().map(|_| "c".repeat(envelope::REFUSAL_CODE_BYTES)),
                outputs: Some(if boundary {
                    gate_outputs(false)
                } else {
                    filled_outputs(node)
                }),
                previous_attempts: Vec::new(),
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
                        stop: Some(provenant_records::StopCause::HostNativeUnconfigured),
                        attempt_ref: Some(worst(HOST_ID_BYTES)),
                        outcome: Some(AttemptOutcome::AdapterUnavailable),
                        launch_actor: worst(HOST_ID_BYTES),
                        input_cap: Some(usize::MAX),
                        input_cap_source: Some(InputCapSource::EffectiveGraphLimits),
                        input_manifest: Some(AttemptInputManifest {
                            extra: Default::default(), schema_version: 1,
                            source_commit: node.source.as_ref().map(|_| "a".repeat(64)),
                            source_producer: node.source.as_ref().map(|_| AttemptKey { extra: Default::default(), graph_id: plan.graph_id.clone(), node_id: "n".repeat(64), route_index: u16::MAX }),
                            writer_head: plans::writer(node).then(|| "a".repeat(64)),
                            review_base: node.extra.get("review_base").map(|_| "a".repeat(64)),
                            graph_review_base: Some("a".repeat(64)), integration_base: Some("a".repeat(64)), integration_source: Some("a".repeat(64)),
                            applied_route: Some(GraphRoute { extra: Default::default(), adapter: Some("a".repeat(256)), model: Some("m".repeat(256)), effort: Some("e".repeat(256)), account: Some("a".repeat(256)), name: None, alias: None tier: None, }),
                            dependencies: Some(node.depends_on.iter().take(MANIFEST_INPUT_LIMIT).cloned().collect()), dependencies_omitted: node.depends_on.len().saturating_sub(MANIFEST_INPUT_LIMIT),
                            inputs: node.inputs.iter().flat_map(|input| input.outputs.iter().map(move |kind| AttemptInputEvidence { extra: Default::default(), producer_node: input.node.clone(), producer_attempt: Some(AttemptKey { extra: Default::default(), graph_id: plan.graph_id.clone(), node_id: input.node.clone(), route_index: u16::MAX }), kind: *kind, required: true, producer_result_digest: Some("a".repeat(71)), commit: Some("a".repeat(64)), delivery: InputDelivery::Delivered, file_delivery: InputDelivery::Delivered })).take(MANIFEST_INPUT_LIMIT).collect(),
                            omitted: node.inputs.iter().map(|input| input.outputs.len()).sum::<usize>().saturating_sub(MANIFEST_INPUT_LIMIT),
                        }),
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
        bound_review_base: Some("f".repeat(64)),
        bound_integration_base: Some("f".repeat(64)),
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
        pause: Some(
            provenant_records::pause::Pause::new(
                PauseCause::Stop(provenant_records::StopCause::CredentialHelperFailed),
                None,
                worst(PAUSE_REASON_BYTES),
            )
            .unwrap(),
        ),
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
    assert_fits_with_repair_metadata(document, "substantial", false);
}

#[test]
fn repair_growth_and_risk_metadata_fit_the_checkpoint_bound() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"repair-envelope","decision_session":"chair","nodes":[
            {"id":"initial","access":"read_only","prompt":"review","route":{"alias":"senior"}},
            {"id":"repair","kind":"loop","access":"read_only","depends_on":["initial"],
                "inputs":[{"node":"initial","outputs":["findings","verdict"]}],
                "loop":{"review_repair":true,"max_rounds":3,"until":"accepted","decision_session":"chair","body":[
                    {"id":"fix","access":"worktree_write","worktree":"/fixture/writer","prompt":"repair","route":{"alias":"senior"}},
                    {"id":"check","access":"read_only","depends_on":["fix"],"source":{"type":"node","node":"fix"},"prompt":"review repair","route":{"alias":"senior"}},
                    {"id":"accepted","kind":"gate","access":"read_only","depends_on":["check"],"inputs":[{"node":"check","outputs":["verdict"]}],
                        "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
                ]}}
        ]
    })).unwrap();
    let plan = planned(&document);
    let positions = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    assert_eq!(
        plan.nodes
            .iter()
            .filter(|node| repair::is_repair_gate(node, &plan, &positions))
            .count(),
        3
    );
    assert_fits_with_repair_metadata(document.clone(), "terminal", true);
    assert_fits_with_repair_metadata(document, "substantial", false);
}

fn assert_fits_with_repair_metadata(document: GraphDocument, tier: &str, grew: bool) {
    let plan = planned(&document);
    let mut checkpoint = filled_checkpoint(&document, &plan);
    let positions = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    for (node, planned) in checkpoint.nodes.iter_mut().zip(&plan.nodes) {
        if repair::is_repair_gate(planned, &plan, &positions) {
            node.extra.insert(
                "repair_growth".into(),
                json!({"before":"1".repeat(64),"after":"2".repeat(64),"grew":grew}),
            );
        }
        if planned.extra.get("repair_scope_gate") == Some(&json!(true)) {
            node.extra.insert("owner_notified".into(), true.into());
        }
        if let Some(outputs) = &mut node.outputs {
            outputs.extra.insert("risk_tier".into(), tier.into());
            if plan.nodes.iter().any(|consumer| {
                consumer.extra.get("repair_scope_gate") == Some(&json!(true))
                    && consumer.depends_on.contains(&planned.id)
            }) {
                outputs.extra.insert("repair_design".into(), json!({"within_scope":false,"one_way_door":false}));
            }
        }
    }
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
    let size = persisted_len(&stored);
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
    assert!(matches!(error, GraphError::Admission(reason) if reason.to_string() == preflight));
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
        run.checkpoint.pause_cause(),
        Some(PauseCause::InvalidLoopDecision)
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
            assert!(persisted_len(&stored) <= bound, "cap={cap} count={count}");
        }
    }
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
        persisted_len(&StoredCheckpoint {
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
fn routing_stratum_flows_from_graph_to_nodes() {
    let mut document = mixed();
    document.context = Some(provenant_records::run::WorkContext {
        risk_tier: Some("substantial".into()),
        task_kind: Some("repair".into()),
        ..Default::default()
    });
    document.nodes[1].context = Some(provenant_records::run::WorkContext {
        task_kind: Some("review".into()),
        ..Default::default()
    });
    let plan = planned(&document);
    let node = plan.nodes.iter().find(|node| node.id == "review").unwrap();
    let context = node_context(&plan, node);
    assert_eq!(context.risk_tier.as_deref(), Some("substantial"));
    assert_eq!(context.task_kind.as_deref(), Some("review"));
}


#[test]
fn principal_scope_report_notification_and_assessment_fit_checkpoint_bound() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"principal-envelope","nodes":[
            {"id":"design","access":"read_only","prompt":"design","route":{"alias":"principal"}},
            {"id":"scope","kind":"decide","access":"read_only","depends_on":["design"],
             "repair_scope_gate":true,"decide":{"question":"Approve changed scope?",
             "options":[{"id":"approve","label":"Approve"},{"id":"hold","label":"Hold"}]}}
        ]
    })).unwrap();
    assert_fits_with_repair_metadata(document, "substantial", false);
}

#[test]
fn live_child_patch_keeps_the_planners_final_fragment_envelope_reserve() {
    let document = mixed();
    let plan = planned(&document);
    let before = envelope::checkpoint_bound(&document, &[], &[], &plan);
    let live: Expansion = serde_json::from_value(json!({
        "source":"plan",  "nodes":[],
        "patch":{"revision":1,"digest":format!("sha256:{}", "a".repeat(64)),"parent":"plan"}
    })).unwrap();
    let live_bound = envelope::checkpoint_bound(&document, std::slice::from_ref(&live), &[], &plan);
    assert!(live_bound >= before, "a live child patch must retain space for the planner's final fragment");

    let mut final_fragment = live.clone();
    final_fragment.extra.insert("final_fragment".into(), true.into());
    let closed_bound = envelope::checkpoint_bound(&document, &[final_fragment], &[], &plan);
    assert!(closed_bound < live_bound, "a final fragment releases its producer's fragment reserve");

    let mut fallback = live;
    fallback.patch = None;
    let fallback_bound = envelope::checkpoint_bound(&document, &[fallback], &[], &plan);
    assert!(fallback_bound < live_bound, "a fallback expansion also closes its producer");
}
