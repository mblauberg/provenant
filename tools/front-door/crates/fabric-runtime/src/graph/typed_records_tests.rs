//! A checkpoint stores the handoff, the pause and the review decisions typed.
use super::*;
use serde_json::{Value, json};

/// An owner that proves no carry-over, so only the reviewed head may land.
struct NoCoverage;
impl AttemptOwner for NoCoverage {
    type Error = std::convert::Infallible;
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }
    fn actor(&self) -> String {
        "fixture".into()
    }
    fn observe_or_start(
        &mut self,
        _: &AttemptKey,
        _: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        _: Option<&str>,
        _: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        unreachable!("the landing check launches nothing")
    }
}

fn acceptance(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
    head: &str,
) -> Option<String> {
    let Ok(error) = land::acceptance_error(
        plan,
        checkpoint,
        positions,
        &mut NoCoverage,
        "land",
        "write",
        ("feature", head),
    );
    error
}

fn stored_with(node: Value, pause: Value) -> Value {
    let mut checkpoint: Value = serde_json::from_str::<Value>(include_str!(
        "../fixtures/graph-checkpoint.json"
    ))
    .unwrap()["checkpoint"]
        .clone();
    for (key, value) in node.as_object().unwrap() {
        checkpoint["nodes"][0][key] = value.clone();
    }
    checkpoint["pause"] = pause;
    checkpoint
}

fn decode_typed_record<T: serde::de::DeserializeOwned>(
    value: &Value,
) -> Result<T, provenant_records::DecodeError> {
    let bytes = serde_json::to_vec(value).unwrap();
    provenant_records::json::decode(bytes.as_slice(), provenant_records::Limits::default())
}

fn decision(base: bool) -> Value {
    let mut decision = json!({
        "finding": "F1", "disposition": "act", "reason": "Fix it.",
        "reviewed_head": "a".repeat(40),
    });
    if base {
        decision["reviewed_base"] = json!("b".repeat(40));
    }
    decision
}

#[test]
fn a_checkpoint_round_trips_its_handoff_pause_and_decision_typed() {
    let handoff = json!({"schema_version":2,"status":"stopped_early","note":"half done",
        "remaining":["tests"],"decisions":[],"verify":[]});
    let checkpoint = stored_with(
        json!({"handoff": handoff, "decisions": [decision(true), decision(false)]}),
        json!({"cause": {"kind": "budget_exceeded"}, "reason": "budget spent"}),
    );
    let decoded: GraphCheckpoint = serde_json::from_value(checkpoint).unwrap();
    assert_eq!(decoded.pause_cause(), Some(PauseCause::BudgetExceeded));
    assert_eq!(decoded.pause_reason(), Some("budget spent"));
    let node = &decoded.nodes[0];
    assert_eq!(node.handoff.as_ref().unwrap().remaining, ["tests"]);
    let with_base = &node.decisions[0];
    assert_eq!(with_base.reviewed_head().to_string(), "a".repeat(40));
    assert_eq!(
        with_base.reviewed_base().unwrap().to_string(),
        "b".repeat(40)
    );
    let without_base = &node.decisions[1];
    assert_eq!(without_base.reviewed_head().to_string(), "a".repeat(40));
    assert_eq!(without_base.reviewed_base(), None);
    let again: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(&decoded).unwrap()).unwrap();
    assert_eq!(again, decoded);
}

#[test]
fn a_review_decision_without_a_reviewed_base_fails_closed_at_the_landing_check() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1,
        "graph_id": "decision-land",
        "nodes": [
            {
                "id": "write",
                "access": "worktree_write",
                "worktree": "lane",
                "delivery": "pr",
                "prompt": "write",
                "route": {"alias": "senior"}
            },
            {
                "id": "ci",
                "kind": "gate",
                "access": "read_only",
                "depends_on": ["write"],
                "condition": {"type": "ci", "target": {"node": "write"}}
            },
            {
                "id": "review",
                "access": "read_only",
                "prompt": "review",
                "depends_on": ["write"],
                "source": {"type": "head", "node": "write"},
                "route": {"alias": "senior"}
            },
            {
                "id": "accepted",
                "kind": "gate",
                "access": "read_only",
                "depends_on": ["review"],
                "inputs": [{"node": "review", "outputs": ["verdict"]}],
                "condition": {"type": "verdict", "expected": "VERDICT: PASS", "threshold": "all"}
            },
            {
                "id": "land",
                "kind": "land",
                "access": "read_only",
                "depends_on": ["accepted", "ci"],
                "land": {"node": "write", "tier": "routine"}
            }
        ]
    }))
    .unwrap();

    let limits = crate::config::GraphLimits::default();
    let (plan, positions) = validation::validate_flat_plan(&document, &limits).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
    let head = "a".repeat(40);
    let base = "b".repeat(40);

    let write_idx = positions["write"];
    checkpoint.nodes[write_idx].state = NodeState::Succeeded;
    checkpoint.nodes[write_idx].outputs = Some(NodeOutputs {
        extra: Default::default(),
        repair_design: None,
        partial: false,
        risk_tier: None,
        repair_items: None,
        spec_concerns: None,
        spec_concern_count: None,
        spec_defect_count: None,
        artifact_findings: None,
        compact_disposition_findings: None,
        verdict_error: None,
        schema_version: 2,
        decide_outcome: None,
        result: None,
        verdict: None,
        findings: Vec::new(),
        findings_available: None,
        branch: Some("feature".into()),
        commit: Some(head.clone()),
        warnings: Vec::new(),
        plan: None,
        remaining: None,
    });

    let review_idx = positions["review"];
    checkpoint.nodes[review_idx].state = NodeState::Succeeded;
    checkpoint.nodes[review_idx].source_commit = Some(head.clone());
    checkpoint.nodes[review_idx].attempts = vec![
        serde_json::from_value(json!({
            "launched": true,
            "key": {"graph_id": plan.graph_id, "node_id": "review", "route_index": 0, "attempt": 1},
            "route": {"alias": "senior"},
            "state": "terminal",
            "attempt_ref": "fixture/1",
            "outcome": "succeeded",
            "launch_actor": "fixture",
            "settled": 1
        }))
        .unwrap(),
    ];
    checkpoint.nodes[review_idx].outputs = Some(NodeOutputs {
        extra: Default::default(),
        repair_design: None,
        partial: false,
        risk_tier: None,
        repair_items: None,
        spec_concerns: None,
        spec_concern_count: None,
        spec_defect_count: None,
        artifact_findings: None,
        compact_disposition_findings: None,
        verdict_error: None,
        schema_version: 2,
        decide_outcome: None,
        result: None,
        verdict: Some("VERDICT: PASS".into()),
        findings: Vec::new(),
        findings_available: None,
        branch: None,
        commit: None,
        warnings: Vec::new(),
        plan: None,
        remaining: None,
    });

    let ci_idx = positions["ci"];
    checkpoint.nodes[ci_idx].state = NodeState::Succeeded;
    checkpoint.nodes[ci_idx].gate = Some(GateResult {
        extra: Default::default(),
        passed: true,
        reason: None,
        on_fail: None,
    });

    let accepted_idx = positions["accepted"];
    checkpoint.nodes[accepted_idx].state = NodeState::Succeeded;
    checkpoint.nodes[accepted_idx].gate = Some(GateResult {
        extra: Default::default(),
        passed: true,
        reason: None,
        on_fail: None,
    });

    // 1. With a decision without a reviewed base (None), landing fails closed.
    checkpoint.nodes[review_idx].decisions = vec![
        provenant_records::FindingDecision::new(
            "F1",
            provenant_records::FindingDisposition::Act,
            "Fix it.",
            head.parse().unwrap(),
            None,
        )
        .unwrap(),
    ];

    let error = acceptance(&plan, &checkpoint, &positions, &head);
    assert!(
        error.is_some(),
        "a decision without a reviewed base must fail closed at the landing check"
    );

    // 2. With matching reviewed base, landing succeeds.
    checkpoint.nodes[review_idx].review_base = Some(base.clone());
    checkpoint.nodes[review_idx].decisions = vec![
        provenant_records::FindingDecision::new(
            "F1",
            provenant_records::FindingDisposition::Act,
            "Fix it.",
            head.parse().unwrap(),
            Some(base.parse().unwrap()),
        )
        .unwrap(),
    ];

    let ok = acceptance(&plan, &checkpoint, &positions, &head);
    assert!(
        ok.is_none(),
        "a decision with a matching reviewed base must allow landing"
    );

    // 3. With mismatched base, landing fails closed.
    let other_base = "c".repeat(40);
    checkpoint.nodes[review_idx].decisions = vec![
        provenant_records::FindingDecision::new(
            "F1",
            provenant_records::FindingDisposition::Act,
            "Fix it.",
            head.parse().unwrap(),
            Some(other_base.parse().unwrap()),
        )
        .unwrap(),
    ];

    let mismatch = acceptance(&plan, &checkpoint, &positions, &head);
    assert!(
        mismatch.is_some(),
        "a decision with a mismatched reviewed base must fail closed at the landing check"
    );
}

#[test]
fn graph_document_host_metadata_round_trips_and_rejects_malformed_values() {
    let origin = provenant_records::WorkKey::new(
        provenant_core::ProjectId::from_bytes([3; 16]),
        "origin",
        "writer",
        1,
    )
    .unwrap();
    let digest = "a".repeat(64);
    let fields = vec![
        ("replay_digest", json!(digest), json!(false)),
        (
            "submission_patch",
            json!({"schema_version": 1, "nodes": []}),
            json!("not a patch"),
        ),
        (
            "template_library",
            json!({"task": {"brief": "Do the work"}}),
            json!(["not a template map"]),
        ),
        (
            "template_layers",
            json!({"task": "inline"}),
            json!({"task": "unknown layer"}),
        ),
        (
            "template_resolvers",
            json!({"review": ["security"]}),
            json!({"review": "security"}),
        ),
        (
            "peer_origin_work_key",
            serde_json::to_value(origin).unwrap(),
            json!("not a work key"),
        ),
        ("review_tier_override", json!("senior"), json!(7)),
        ("exclude", json!(["codex"]), json!("codex")),
        ("network", json!("off"), json!(true)),
        ("capability", json!(["network"]), json!("network")),
        ("no_capability", json!(["write"]), json!({"write": true})),
        (
            "provenance",
            json!({
                "operation_id": "submission-key",
                "parameters": {"prompt": "Do the work"},
                "source": "planner",
            }),
            json!("not a provenance record"),
        ),
    ];
    let mut raw = json!({"schema_version": 1, "graph_id": "metadata", "nodes": []});
    for (field, valid, _) in &fields {
        raw[*field] = valid.clone();
    }

    let document: GraphDocument = decode_typed_record(&raw).unwrap();
    assert_eq!(document.replay_digest(), Some(digest.as_str()));
    assert!(document.extra.get("replay_digest").is_none());
    assert_eq!(document.exclude, ["codex"]);
    assert_eq!(document.network, Some(GraphNetwork::Off));
    assert_eq!(document.capability, ["network"]);
    assert_eq!(document.no_capability, ["write"]);
    let provenance = document.provenance.as_ref().unwrap();
    assert_eq!(
        provenance.operation_id.as_ref().unwrap().as_str(),
        "submission-key"
    );
    assert_eq!(provenance.parameters["prompt"], "Do the work");
    assert_eq!(provenance.extra["source"], "planner");
    assert_eq!(
        decode_typed_record::<GraphDocument>(&serde_json::to_value(&document).unwrap()).unwrap(),
        document
    );

    for (field, _, malformed) in fields {
        let mut candidate = raw.clone();
        candidate[field] = malformed;
        assert!(
            matches!(
                decode_typed_record::<GraphDocument>(&candidate),
                Err(provenant_records::DecodeError::Typed { .. })
            ),
            "{field}"
        );
    }
    for malformed in [
        json!({"operation_id": "not a valid operation id"}),
        json!({"parameters": ["not", "an", "object"]}),
    ] {
        let mut candidate = raw.clone();
        candidate["provenance"] = malformed;
        assert!(matches!(
            decode_typed_record::<GraphDocument>(&candidate),
            Err(provenant_records::DecodeError::Typed { .. })
        ));
    }
}

#[test]
fn graph_execution_peer_fields_round_trip_and_reject_malformed_values() {
    let digest = provenant_core::Digest::from_bytes([0x4d; 32]);
    let mut raw = json!({
        "directory": "/tmp/graph",
        "document": {"schema_version": 1, "graph_id": "receiver", "nodes": []},
        "peer_origin_context": {
            "graph_id": "origin-graph",
            "node_id": "origin-node",
            "attempt": 2,
        },
        "peer_submission_digest": digest,
        "future_extension": {"kept": true},
    });
    let execution: GraphExecution = decode_typed_record(&raw).unwrap();
    let origin = execution.peer_origin_context.as_ref().unwrap();
    assert_eq!(origin.graph_id.as_deref(), Some("origin-graph"));
    assert_eq!(origin.node_id.as_deref(), Some("origin-node"));
    assert_eq!(origin.attempt, Some(2));
    assert_eq!(execution.peer_submission_digest, Some(digest));
    assert!(!execution.extra.contains_key("peer_origin_context"));
    assert!(!execution.extra.contains_key("peer_submission_digest"));
    assert_eq!(
        execution.extra.get("future_extension"),
        Some(&json!({"kept": true}))
    );
    assert_eq!(
        decode_typed_record::<GraphExecution>(&serde_json::to_value(&execution).unwrap()).unwrap(),
        execution
    );

    for (field, malformed) in [
        ("peer_origin_context", json!("not a work context")),
        ("peer_submission_digest", json!(false)),
    ] {
        let mut candidate = raw.clone();
        candidate[field] = malformed;
        assert!(
            matches!(
                decode_typed_record::<GraphExecution>(&candidate),
                Err(provenant_records::DecodeError::Typed { .. })
            ),
            "{field}"
        );
    }
}

#[test]
fn graph_checkpoint_cancellation_receipt_round_trips_and_rejects_malformed_values() {
    let mut raw = stored_with(
        json!({}),
        json!({"cause": {"kind": "budget_exceeded"}, "reason": "budget spent"}),
    );
    raw["cancellation_request_receipt"] = json!({"request_id": "cancel-1"});
    let checkpoint: GraphCheckpoint = decode_typed_record(&raw).unwrap();
    assert_eq!(
        checkpoint
            .cancellation_request_receipt
            .as_ref()
            .map(|receipt| receipt.request_id.as_str()),
        Some("cancel-1")
    );
    assert_eq!(
        decode_typed_record::<GraphCheckpoint>(&serde_json::to_value(&checkpoint).unwrap())
            .unwrap(),
        checkpoint
    );

    raw["cancellation_request_receipt"] = json!("not a receipt");
    assert!(matches!(
        decode_typed_record::<GraphCheckpoint>(&raw),
        Err(provenant_records::DecodeError::Typed { .. })
    ));
}

#[test]
fn plan_spec_host_metadata_round_trips_and_rejects_malformed_values() {
    let fields = vec![
        ("model_max_nodes", json!(3), json!("three")),
        ("planned_review", json!(true), json!("yes")),
        ("review_min_successful", json!(2), json!(false)),
        (
            "resolvers",
            json!({"review": ["security"]}),
            json!(["not a resolver map"]),
        ),
        (
            "required_floor_patterns",
            json!(["review.security"]),
            json!("review.security"),
        ),
        (
            "floor_candidate_patterns",
            json!(["review.tests"]),
            json!([false]),
        ),
        ("floor_min_reviewers", json!(1), json!("one")),
        ("floor_classes", json!(["security"]), json!([1])),
    ];
    let mut raw = json!({"max_nodes": 5});
    for (field, valid, _) in &fields {
        raw[*field] = valid.clone();
    }

    let spec: PlanSpec = decode_typed_record(&raw).unwrap();
    assert_eq!(
        decode_typed_record::<PlanSpec>(&serde_json::to_value(&spec).unwrap()).unwrap(),
        spec
    );
    for (field, _, malformed) in fields {
        let mut candidate = raw.clone();
        candidate[field] = malformed;
        assert!(
            matches!(
                decode_typed_record::<PlanSpec>(&candidate),
                Err(provenant_records::DecodeError::Typed { .. })
            ),
            "{field}"
        );
    }
}

#[test]
fn graph_attempt_prelaunch_refusal_round_trips_and_rejects_malformed_values() {
    let raw = json!({
        "launched": true,
        "host_refused_before_acceptance": true,
        "key": {"graph_id": "g", "node_id": "n", "attempt": 1, "route_index": 0},
        "route": {},
        "state": "starting",
        "attempt_ref": null,
        "outcome": null,
        "launch_actor": "fixture"
    });
    let attempt: GraphAttempt = decode_typed_record(&raw).unwrap();
    assert!(attempt.host_refused_before_acceptance);
    assert_eq!(
        decode_typed_record::<GraphAttempt>(&serde_json::to_value(&attempt).unwrap()).unwrap(),
        attempt
    );
    let mut malformed = raw;
    malformed["host_refused_before_acceptance"] = json!("true");
    assert!(matches!(
        decode_typed_record::<GraphAttempt>(&malformed),
        Err(provenant_records::DecodeError::Typed { .. })
    ));
}

#[test]
fn writer_binding_identity_round_trips_and_rejects_malformed_values() {
    let wire = serde_json::json!({"worktree":"checkout","head":"1".repeat(40),"source_directory_identity":{"device":7,"inode":9}});
    let bytes = serde_json::to_vec(&wire).unwrap();
    let binding: WriterWorktreeBinding =
        provenant_records::json::decode(bytes.as_slice(), provenant_records::Limits::default())
            .unwrap();
    assert_eq!(serde_json::to_value(binding).unwrap(), wire);
    let mut malformed = wire;
    malformed["source_directory_identity"]["device"] = serde_json::json!(false);
    let bytes = serde_json::to_vec(&malformed).unwrap();
    assert!(matches!(
        provenant_records::json::decode::<WriterWorktreeBinding, _>(
            bytes.as_slice(),
            provenant_records::Limits::default()
        ),
        Err(provenant_records::DecodeError::Typed { .. })
    ));
}
