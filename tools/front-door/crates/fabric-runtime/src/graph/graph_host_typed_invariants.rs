use super::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

struct FieldCase {
    key: &'static str,
    valid: Value,
    malformed: Value,
}

fn field(key: &'static str, valid: Value, malformed: Value) -> FieldCase {
    FieldCase {
        key,
        valid,
        malformed,
    }
}

fn roundtrip_and_reject<T: DeserializeOwned + Serialize>(base: &T, fields: &[FieldCase]) {
    let baseline = serde_json::to_value(base).unwrap();
    for field in fields {
        let mut value = baseline.clone();
        value
            .as_object_mut()
            .unwrap()
            .insert(field.key.to_owned(), field.valid.clone());
        let decoded: T = serde_json::from_value(value).unwrap();
        let encoded = serde_json::to_value(decoded).unwrap();
        // Empty/default fields may be omitted by their declared wire policy.
        // The typed value must still survive its canonical representation.
        let decoded_again: T = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded_again).unwrap(),
            encoded,
            "{}",
            field.key
        );
        for (key, value) in baseline.as_object().unwrap() {
            if key.starts_with("future_") {
                assert_eq!(encoded.get(key), Some(value), "{key}");
            }
        }

        let mut malformed = baseline.clone();
        malformed
            .as_object_mut()
            .unwrap()
            .insert(field.key.to_owned(), field.malformed.clone());
        assert!(
            serde_json::from_value::<T>(malformed).is_err(),
            "{} must reject a malformed typed value",
            field.key
        );
    }
}

#[test]
fn graph_document_host_fields_round_trip_reject_wrong_types_and_keep_unknowns() {
    let base: GraphDocument = serde_json::from_value(json!({
        "schema_version": 2,
        "graph_id": "typed-document",
        "nodes": [],
        "future_graph_fact": {"retained": true}
    }))
    .unwrap();
    let fields = [
        field(
            "review",
            json!({"shape":"standard", "minimum_family_counts":{"openai":2}}),
            json!(false),
        ),
        field("review_subject_missing", json!(true), json!("true")),
        field("exclude", json!(["codex"]), json!("codex")),
        field("template_library", json!({}), json!([])),
        field("template_layers", json!({}), json!([])),
        field(
            "template_resolvers",
            json!({"review-panel":["correctness"]}),
            json!([]),
        ),
    ];
    roundtrip_and_reject(&base, &fields);
    let mut encoded = serde_json::to_value(base).unwrap();
    encoded["review"] = json!({"shape":"standard"});
    let decoded: GraphDocument = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["future_graph_fact"]["retained"],
        true
    );
}

#[test]
fn graph_route_host_fields_round_trip_and_reject_malformed_values() {
    let base: GraphRoute = serde_json::from_value(json!({"future_route":true})).unwrap();
    let fields = [
        field("required_family", json!("openai"), json!(3)),
        field("peer_source_head", json!("a".repeat(40)), json!(false)),
        field(
            "peer_requested_route",
            json!({"kind":"named","name":"senior","effort":null,"required_family":"openai"}),
            json!(false),
        ),
        field("peer_network", json!("provider"), json!([])),
        field("peer_retry_limit", json!(2), json!("two")),
    ];
    roundtrip_and_reject(&base, &fields);
}

#[test]
fn graph_attempt_placement_round_trips_and_rejects_malformed_values() {
    let base: GraphAttempt = serde_json::from_value(json!({
        "future_attempt_fact": true,
        "launched": true,
        "key": {"graph_id":"g", "node_id":"n", "route_index":0, "attempt":1},
        "route": {},
        "state":"starting",
        "attempt_ref":null,
        "outcome":null,
        "launch_actor":"chair"
    }))
    .unwrap();
    let fields = [
        field("placement_host", json!("peer-label"), json!(false)),
        field(
            "placement_host_id",
            json!("a".repeat(32)),
            json!("invalid-host-id"),
        ),
    ];
    roundtrip_and_reject(&base, &fields);
}

#[test]
fn attempt_usage_route_round_trips_and_rejects_wrong_types() {
    let base: AttemptUsage = serde_json::from_value(json!({
        "future_usage_fact": {"retained": true}
    }))
    .unwrap();
    let fields = [field(
        "resolved_route",
        json!({
            "provider":"openai", "model":"reasoning", "effort":"high",
            "future_route_fact":{"retained":true}
        }),
        json!({"provider":7}),
    )];
    roundtrip_and_reject(&base, &fields);
    let mut value = serde_json::to_value(base).unwrap();
    value["resolved_route"] = fields[0].valid.clone();
    let decoded: AttemptUsage = serde_json::from_value(value).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(encoded["future_usage_fact"]["retained"], true);
    assert_eq!(
        encoded["resolved_route"]["future_route_fact"]["retained"],
        true
    );
}

#[test]
fn attempt_resolved_route_fields_round_trip_and_reject_wrong_types() {
    let base: AttemptResolvedRoute = serde_json::from_value(json!({
        "future_route_fact": {"retained": true}
    }))
    .unwrap();
    let fields = [
        field("provider", json!("openai"), json!(false)),
        field("model", json!("reasoning"), json!(3)),
        field("effort", json!("high"), json!([])),
    ];
    roundtrip_and_reject(&base, &fields);
}

#[test]
fn graph_node_host_fields_round_trip_and_reject_malformed_values() {
    let removed = json!([{"consumer":"review","required_outputs":["findings"]}]);
    let fields = [
        field("repair_scope_gate", json!(true), json!("true")),
        field("detached", json!(true), json!("true")),
        field("provision_worktree", json!(true), json!("true")),
        field("template", json!("review.tests"), json!(7)),
        field("review_base", json!("a".repeat(40)), json!(false)),
        field("parent_node", json!("parent"), json!(false)),
        field(
            "capability_sources",
            json!({"postgres":"flag"}),
            json!([{"name":"postgres","source":"flag"}]),
        ),
        field("review_lens", json!("correctness"), json!(false)),
        field("repair_lens", json!("security"), json!(false)),
        field("repair_writer", json!(true), json!("true")),
        field("repair_triage", json!(true), json!("true")),
        field("repair_risk_assessment", json!(true), json!("true")),
        field("retry_generation", json!(3), json!("three")),
        field("container_retry", json!(true), json!("true")),
        field("review_behaviour", json!("lens"), json!(7)),
        field("removed_requirements", removed, json!(false)),
    ];
    roundtrip_and_reject(&GraphNode::default(), &fields);
}

#[test]
fn node_progress_host_fields_round_trip_and_reject_malformed_values() {
    let base: NodeProgress = serde_json::from_value(json!({
        "id":"node", "state":"pending", "worktree":null, "attempts":[]
    }))
    .unwrap();
    let snapshot = json!({
        "id":"producer", "state":"succeeded", "worktree":null, "attempts":[]
    });
    let fields = [
        field("owner_notified", json!(true), json!("true")),
        field("retry", json!({"retries":2,"origin":"lane"}), json!(false)),
        field("quota_redraw_spent", json!(2), json!("two")),
        field("quota_redraw_archived", json!(3), json!("three")),
        field("retry_generation", json!(4), json!("four")),
        field("container_retry", json!(true), json!("true")),
        field("concurrency_warning", json!("cap occupied"), json!(false)),
        field(
            "repair_growth",
            json!({"before":"a","after":"b","grew":true}),
            json!(false),
        ),
        field(
            "review_verdict_correction_refused",
            json!("bounded reason"),
            json!(false),
        ),
        field("provisioned_source", json!("abc123"), json!(false)),
        field("warnings", json!(["redacted launch warning"]), json!(false)),
        field(
            "removed_requirements",
            json!([{"consumer":"review","verdict_input":true}]),
            json!(false),
        ),
        field(
            "settled_dependency_snapshots",
            json!({"producer":snapshot}),
            json!(false),
        ),
        field("new_worktree_base", json!("base-ref"), json!(false)),
        field("children_cancelled", json!(true), json!("true")),
        field("detached_from_graph_cancel", json!(true), json!("true")),
        field("subtree_cancel", json!("parent"), json!(false)),
        field("yielded_slot", json!(true), json!("true")),
        field("stop_fence", json!(true), json!("true")),
    ];
    roundtrip_and_reject(&base, &fields);
}

#[test]
fn node_outputs_host_fields_round_trip_and_reject_malformed_values() {
    let base: NodeOutputs = serde_json::from_value(json!({
        "schema_version":2, "result":null, "verdict":null, "findings":[],
        "branch":null, "commit":null, "warnings":[]
    }))
    .unwrap();
    let fields = [
        field(
            "repair_design",
            json!({"within_scope":true,"one_way_door":false}),
            json!(false),
        ),
        field("review_partial", json!(true), json!("true")),
        field("risk_tier", json!("substantial"), json!(false)),
        field(
            "repair_items",
            json!([{"id":"repair","status":"fixed","reason":"verified"}]),
            json!([{"id":"repair","status":"unknown","reason":"bad"}]),
        ),
        field(
            "spec_concerns",
            json!([{"criterion":"rule","claim":"old shape","evidence":"caller found","proposed_change":"remove reader","status":"as_written"}]),
            json!(false),
        ),
        field("spec_concern_count", json!(1), json!("one")),
        field("spec_defect_count", json!(2), json!("two")),
        field(
            "_artifact_findings",
            json!("```findings\n{}\n```"),
            json!(false),
        ),
        field(
            "_compact_disposition_findings",
            json!("```findings\n{}\n```"),
            json!(false),
        ),
    ];
    roundtrip_and_reject(&base, &fields);
}

#[test]
fn attempt_key_host_fields_round_trip_and_reject_malformed_values() {
    let base = AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: "graph".into(),
        node_id: "node".into(),
        attempt: 1,
        route_index: 0,
    };
    let fields = [
        field("retry_generation", json!(2), json!("two")),
        field("previous_attempt_ref", json!("run/attempt"), json!(false)),
    ];
    roundtrip_and_reject(&base, &fields);
}

#[test]
fn expansion_host_fields_round_trip_and_reject_malformed_values() {
    let base: super::plans::Expansion =
        serde_json::from_value(json!({"source":"planner","nodes":[]})).unwrap();
    let fields = [
        field(
            "retired",
            json!({"reason":"redo","expansion":{"source":"old","nodes":[]},"progress":[]}),
            json!({"reason":false,"expansion":{"source":"old","nodes":[]},"progress":[]}),
        ),
        field("retry_generation", json!(2), json!("two")),
        field("final_result", json!(true), json!("true")),
        field("final_fragment", json!(true), json!("true")),
        field(
            "floor_warnings",
            json!(["path reviewer floor unmet"]),
            json!(false),
        ),
    ];
    roundtrip_and_reject(&base, &fields);
}

#[test]
fn loop_spec_review_repair_round_trips_and_rejects_malformed_values() {
    let base: super::loops::LoopSpec = serde_json::from_value(json!({
        "body":[], "until":"VERDICT: PASS", "max_rounds":1
    }))
    .unwrap();
    roundtrip_and_reject(&base, &[field("review_repair", json!(true), json!("true"))]);
}
