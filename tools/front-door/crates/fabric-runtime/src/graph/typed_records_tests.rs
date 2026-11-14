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
        transitions::initial_checkpoint(&document, &plan, "digest".into(), None, None, None, None);
    let head = "a".repeat(40);
    let base = "b".repeat(40);

    let write_idx = positions["write"];
    checkpoint.nodes[write_idx].state = NodeState::Succeeded;
    checkpoint.nodes[write_idx].outputs = Some(NodeOutputs {
        extra: Default::default(),
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
