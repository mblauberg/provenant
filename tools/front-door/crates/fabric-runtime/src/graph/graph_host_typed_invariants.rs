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
        assert_eq!(encoded.get(field.key), Some(&field.valid), "{}", field.key);

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

type StoredCodecRoundTrip = fn(Value, provenant_records::Limits) -> Value;

struct StoredCodecCase {
    record: &'static str,
    round_trip: StoredCodecRoundTrip,
    payload: Value,
    preserved_fields: Vec<(&'static str, Value)>,
}

fn stored_codec_case<T>(
    record: &'static str,
    payload: Value,
    preserved_fields: Vec<(&'static str, Value)>,
) -> StoredCodecCase
where
    T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
{
    StoredCodecCase {
        record,
        round_trip: assert_bounded_unknown_round_trip::<T>,
        payload,
        preserved_fields,
    }
}

fn assert_bounded_unknown_round_trip<T>(value: Value, limits: provenant_records::Limits) -> Value
where
    T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
{
    let bytes = serde_json::to_vec(&value).unwrap();
    let decoded: T = provenant_records::decode(bytes.as_slice(), limits).unwrap();
    let encoded = provenant_records::encode(&decoded).unwrap();
    let decoded_again: T = provenant_records::decode(encoded.as_slice(), limits).unwrap();
    assert_eq!(decoded_again, decoded);
    serde_json::from_slice(&encoded).unwrap()
}

#[test]
fn stored_graph_records_keep_unknown_fields_through_bounded_codec() {
    let future_checkpoint = json!({"kept":true});
    let future_progress = json!(["later", "minor"]);
    let future_snapshot = json!({"field":"value"});
    let future_ci_policy = json!({"policy":"later"});
    let future_bases = json!({"new_base":"later"});

    // Runtime records live above provenant-records in the dependency graph, so
    // keep their bounded-codec registry here rather than adding a reverse crate
    // dependency. A new stored runtime record adds one typed row.
    let mut registry = vec![
        stored_codec_case::<GraphCheckpoint>(
            "GraphCheckpoint",
            json!({
                "schema_version": GraphCheckpoint::SCHEMA_VERSION,
                "graph_id": "codec",
                "plan_digest": "sha256:plan",
                "bases": {
                    "review": null,
                    "integration": null,
                    "future_base": future_bases.clone(),
                },
                "nodes": [{
                    "id": "node",
                    "state": "pending",
                    "worktree": null,
                    "attempts": [],
                    "future_progress": future_progress,
                    "ci": {
                        "state": "pending",
                        "summary": "pending",
                        "failed_checks_policy": {
                            "max_bytes": 16,
                            "max_bytes_source": "default",
                            "max_checks": 2,
                            "max_checks_source": "condition",
                            "future_ci_policy": future_ci_policy,
                        },
                    },
                    "settled_dependency_snapshots": {
                        "producer": {
                            "id": "producer",
                            "state": "succeeded",
                            "worktree": null,
                            "attempts": [],
                            "future_snapshot": future_snapshot,
                        }
                    }
                }],
                "future_checkpoint": future_checkpoint,
            }),
            vec![
                ("/future_checkpoint", future_checkpoint.clone()),
                ("/bases/future_base", future_bases.clone()),
                ("/nodes/0/future_progress", future_progress.clone()),
                (
                    "/nodes/0/settled_dependency_snapshots/producer/future_snapshot",
                    future_snapshot.clone(),
                ),
                (
                    "/nodes/0/ci/failed_checks_policy/future_ci_policy",
                    future_ci_policy.clone(),
                ),
            ],
        ),
        stored_codec_case::<NodeProgress>(
            "NodeProgress",
            json!({
                "id": "progress",
                "state": "pending",
                "worktree": null,
                "attempts": [],
                "future_progress": future_progress,
                "pause": {
                    "cause": {"kind":"loop_paused","future_pause_cause":true},
                    "reason": "paused",
                    "future_pause": true,
                },
            }),
            vec![
                ("/future_progress", json!(["later", "minor"])),
                ("/pause/future_pause", json!(true)),
                ("/pause/cause/future_pause_cause", json!(true)),
            ],
        ),
    ];

    let future_execution = json!({"execution":"later"});
    let future_document = json!({"document":"later"});
    let future_node = json!({"node":"later"});
    let future_composite = json!({"composite":"later"});
    let future_when = json!({"when":"later"});
    registry.extend([stored_codec_case::<GraphExecution>(
        "GraphExecution",
        json!({
            "directory": "/tmp/graph",
            "bases": {
                "review": null,
                "integration": null,
                "future_base": future_bases.clone(),
            },
            "document": {
                "schema_version": GRAPH_DOCUMENT_SCHEMA_VERSION,
                "graph_id": "document",
                "nodes": [{
                    "id": "group",
                    "kind": "composite",
                    "access": "read_only",
                    "future_node": future_node.clone(),
                    "composite": {
                        "nodes": [{"id": "member", "access": "read_only", "prompt": "work"}],
                        "exports": [],
                        "future_composite": future_composite.clone(),
                    },
                    "when": [{
                        "name": "enabled",
                        "expected": "true",
                        "actual": "true",
                        "future_when": future_when.clone(),
                    }],
                }],
                "future_document": future_document.clone(),
            },
            "future_execution": future_execution.clone(),
        }),
        vec![
            ("/future_execution", future_execution),
            ("/bases/future_base", future_bases),
            ("/document/future_document", future_document),
            ("/document/nodes/0/future_node", future_node),
            (
                "/document/nodes/0/composite/future_composite",
                future_composite,
            ),
            ("/document/nodes/0/when/0/future_when", future_when),
        ],
    )]);

    let future_envelope = json!({"envelope":"later"});
    registry.extend([stored_codec_case::<super::journal::StoredCheckpoint>(
        "StoredCheckpoint",
        json!({
            "schema_version": CHECKPOINT_SCHEMA_VERSION,
            "digest": "sha256:fixture",
            "checkpoint": {
                "schema_version": GraphCheckpoint::SCHEMA_VERSION,
                "graph_id": "stored",
                "plan_digest": "sha256:plan",
                "nodes": [],
            },
            "execution": {
                "directory": "/tmp/graph",
                "document": {
                    "schema_version": GRAPH_DOCUMENT_SCHEMA_VERSION,
                    "graph_id": "stored",
                    "nodes": [],
                },
            },
            "future_envelope": future_envelope.clone(),
        }),
        vec![("/future_envelope", future_envelope)],
    )]);

    registry.extend([
        stored_codec_case::<super::relay::ChildReceipt>(
            "ChildReceipt",
            json!({
                "graph_id": "g",
                "parent": "p",
                "node": "n",
                "revision": 3,
                "replayed": false,
                "lifecycle_notice": "attached",
                "future_receipt": future_snapshot,
            }),
            vec![("/future_receipt", json!({"field":"value"}))],
        ),
        stored_codec_case::<super::relay::StoredChildReceipt>(
            "StoredChildReceipt",
            json!({
                "schema_version": 1,
                "request_digest": "sha256:request",
                "receipt": {
                    "graph_id": "g",
                    "parent": "p",
                    "node": "n",
                    "revision": 3,
                    "replayed": false,
                    "lifecycle_notice": "attached",
                },
                "future_stored_receipt": future_checkpoint,
            }),
            vec![("/future_stored_receipt", json!({"kept":true}))],
        ),
    ]);

    let mut review_payload = serde_json::to_value(ReviewSettlement {
        extra: Default::default(),
        attempt_id: provenant_core::AttemptId::from_bytes([7; 16]),
        head: provenant_core::GitOid::parse(&"a".repeat(40)).unwrap(),
        base: None,
        verdict: provenant_records::workflow::ReviewVerdict::Approve,
        independent: true,
    })
    .unwrap();
    review_payload["future_review"] = json!("later");
    registry.extend([
        stored_codec_case::<GraphBaseBinding>(
            "GraphBaseBinding",
            json!({"review":null,"integration":null,"future_base":{"owner":"new"}}),
            vec![("/future_base", json!({"owner":"new"}))],
        ),
        stored_codec_case::<SourceBinding>(
            "SourceBinding::Commit",
            json!({"type":"commit","commit":"sha256:commit","future_source":true}),
            vec![("/future_source", json!(true))],
        ),
        stored_codec_case::<SourceBinding>(
            "SourceBinding::Node",
            json!({"type":"node","node":"writer","future_source":true}),
            vec![("/future_source", json!(true))],
        ),
        stored_codec_case::<SourceBinding>(
            "SourceBinding::Head",
            json!({"type":"head","node":"writer","future_source":true}),
            vec![("/future_source", json!(true))],
        ),
        stored_codec_case::<SourceBinding>(
            "SourceBinding::Integration",
            json!({"type":"integration","future_source":true}),
            vec![("/future_source", json!(true))],
        ),
        stored_codec_case::<RemovedRequirement>(
            "RemovedRequirement",
            json!({"consumer":"review","required_outputs":[],"verdict_input":false,"future_requirement":1}),
            vec![("/future_requirement", json!(1))],
        ),
        stored_codec_case::<RepairDesign>(
            "RepairDesign",
            json!({"within_scope":true,"one_way_door":false,"future_design":"later"}),
            vec![("/future_design", json!("later"))],
        ),
        stored_codec_case::<RetryAllowance>(
            "RetryAllowance",
            json!({"retries":2,"origin":"default","future_allowance":true}),
            vec![("/future_allowance", json!(true))],
        ),
        stored_codec_case::<RepairGrowth>(
            "RepairGrowth",
            json!({"before":"a","after":"b","grew":true,"future_growth":"later"}),
            vec![("/future_growth", json!("later"))],
        ),
        stored_codec_case::<DecideOutcome>(
            "DecideOutcome::Answered",
            json!({"kind":"answered","option":"yes","actor":"chair","future_decision":1}),
            vec![("/future_decision", json!(1))],
        ),
        stored_codec_case::<DecideOutcome>(
            "DecideOutcome::AnsweredByDefault",
            json!({"kind":"answered_by_default","option":"yes","future_decision":1}),
            vec![("/future_decision", json!(1))],
        ),
        stored_codec_case::<DecideOutcome>(
            "DecideOutcome::AnsweredByPolicy",
            json!({"kind":"answered_by_policy","option":"yes","rule":"safe","future_decision":1}),
            vec![("/future_decision", json!(1))],
        ),
        stored_codec_case::<DecideOutcome>(
            "DecideOutcome::Unanswered",
            json!({"kind":"unanswered","future_decision":1}),
            vec![("/future_decision", json!(1))],
        ),
        stored_codec_case::<ReviewSettlement>(
            "ReviewSettlement",
            review_payload,
            vec![("/future_review", json!("later"))],
        ),
        stored_codec_case::<FindingTrigger>(
            "FindingTrigger",
            json!({"sequence":"1","reach":"default_path","observed":true,"future_trigger":true}),
            vec![("/future_trigger", json!(true))],
        ),
        stored_codec_case::<FindingTriage>(
            "FindingTriage",
            json!({"decision":"fix","reason":"verified","worth_fixing":true,"root_cause":"shared","blocking_class":"boundary","future_triage":true}),
            vec![("/future_triage", json!(true))],
        ),
        stored_codec_case::<PendingWriterProvisioning>(
            "PendingWriterProvisioning",
            json!({"path":"writer","source":"node","future_provisioning":true}),
            vec![("/future_provisioning", json!(true))],
        ),
        stored_codec_case::<CancellationRequestReceipt>(
            "CancellationRequestReceipt",
            json!({"request_id":"cancel","future_cancellation":true}),
            vec![("/future_cancellation", json!(true))],
        ),
        stored_codec_case::<GraphStructuralLimit>(
            "GraphStructuralLimit",
            json!({"key":"nodes","actual":3,"max":2,"future_limit":true}),
            vec![("/future_limit", json!(true))],
        ),
        stored_codec_case::<GraphEnvelopeLimit>(
            "GraphEnvelopeLimit",
            json!({"bound":1024,"future_limit":true}),
            vec![("/future_limit", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::MissingBlock",
            json!({"kind":"missing_block","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::MissingResult",
            json!({"kind":"missing_result","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::InvalidBlock",
            json!({"kind":"invalid_block","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::InvalidFragment",
            json!({"kind":"invalid_fragment","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::InvalidContinuation",
            json!({"kind":"invalid_continuation","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::Schema",
            json!({"kind":"schema","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::InvalidGraph",
            json!({"kind":"invalid_graph","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::OutputKind",
            json!({"kind":"output_kind","future_failure":true}),
            vec![("/future_failure", json!(true))],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::Structural",
            json!({"kind":"structural","limit":{"key":"nodes","actual":3,"max":2,"future_nested_limit":true},"future_failure":true}),
            vec![
                ("/future_failure", json!(true)),
                ("/limit/future_nested_limit", json!(true)),
            ],
        ),
        stored_codec_case::<PlanFailureFact>(
            "PlanFailureFact::Envelope",
            json!({"kind":"envelope","limit":{"bound":1024,"future_nested_limit":true},"future_failure":true}),
            vec![
                ("/future_failure", json!(true)),
                ("/limit/future_nested_limit", json!(true)),
            ],
        ),
        stored_codec_case::<NodeDecisionCause>(
            "NodeDecisionCause::LoopPaused",
            json!({"kind":"loop_paused","future_cause":true}),
            vec![("/future_cause", json!(true))],
        ),
        stored_codec_case::<NodeDecisionCause>(
            "NodeDecisionCause::LoopAskClosed",
            json!({"kind":"loop_ask_closed","future_cause":true}),
            vec![("/future_cause", json!(true))],
        ),
        stored_codec_case::<NodeDecisionCause>(
            "NodeDecisionCause::InvalidLoopDecision",
            json!({"kind":"invalid_loop_decision","future_cause":true}),
            vec![("/future_cause", json!(true))],
        ),
        stored_codec_case::<NodeDecisionCause>(
            "NodeDecisionCause::PlanExpansionPaused",
            json!({"kind":"plan_expansion_paused","active":true,"fallback":false,"fragment_error":null,"future_cause":true}),
            vec![("/future_cause", json!(true))],
        ),
        stored_codec_case::<NodeDecisionCause>(
            "NodeDecisionCause::CompositeFindingsTruncated",
            json!({"kind":"composite_findings_truncated","warning_indices":[0],"notice_lines":1,"future_cause":true}),
            vec![("/future_cause", json!(true))],
        ),
    ]);

    for case in registry {
        let encoded = (case.round_trip)(
            case.payload,
            provenant_records::Limits {
                max_bytes: MAX_CHECKPOINT_BYTES,
                max_depth: 64,
                max_members: MAX_CHECKPOINT_MEMBERS,
            },
        );
        for (path, expected) in case.preserved_fields {
            assert_eq!(
                encoded.pointer(path),
                Some(&expected),
                "{} did not preserve {path}",
                case.record,
            );
        }
    }

    let future_binding: GraphExecution = serde_json::from_value(json!({
        "directory": "/tmp/graph",
        "bases": {"review": null, "integration": null, "future_base": true},
        "document": {
            "schema_version": GRAPH_DOCUMENT_SCHEMA_VERSION,
            "graph_id": "future-bases",
            "nodes": [],
        },
    }))
    .unwrap();
    assert!(future_binding.valid_bases());
}
