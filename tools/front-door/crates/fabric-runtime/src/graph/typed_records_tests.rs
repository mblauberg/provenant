//! A checkpoint stores the handoff, the pause and the review decisions typed.
use super::*;
use serde_json::{Value, json};

fn stored_with(node: Value, pause: Value) -> Value {
    let mut checkpoint: Value =
        serde_json::from_str::<Value>(include_str!("../fixtures/legacy-graph-checkpoint.json"))
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
        json!({"handoff": handoff, "decisions": [decision(true)]}),
        json!({"cause": {"kind": "budget_exceeded"}, "reason": "budget spent"}),
    );
    let decoded: GraphCheckpoint = serde_json::from_value(checkpoint).unwrap();
    assert_eq!(decoded.pause_cause(), Some(PauseCause::BudgetExceeded));
    assert_eq!(decoded.pause_reason(), Some("budget spent"));
    let node = &decoded.nodes[0];
    assert_eq!(node.handoff.as_ref().unwrap().remaining, ["tests"]);
    let decision = &node.decisions[0];
    assert_eq!(decision.reviewed_head().to_string(), "a".repeat(40));
    assert_eq!(decision.reviewed_base().to_string(), "b".repeat(40));
    let again: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(&decoded).unwrap()).unwrap();
    assert_eq!(again, decoded);
}

#[test]
fn a_decision_without_its_reviewed_base_is_not_readable() {
    let checkpoint = stored_with(json!({"decisions": [decision(false)]}), Value::Null);
    assert!(serde_json::from_value::<GraphCheckpoint>(checkpoint).is_err());
}
