//! Host semantics have typed owners even when records preserve unknown extensions.
use provenant_core::{ActorId, AttemptId, Digest, OperationId, ProjectId};
use provenant_records::{DecodeError, Limits, decode, run::OperationRecord};
use serde_json::json;

#[test]
fn operation_fields_round_trip_and_reject_malformed_values() {
    let baseline = json!({
        "schema_version": 1,
        "project_id": ProjectId::from_bytes([1; 16]),
        "actor_id": ActorId::from_bytes([2; 16]),
        "operation_id": OperationId::parse("typed-operation").unwrap(),
        "kind": "dispatch",
        "request_digest": Digest::from_bytes([3; 32]),
        "state": "refused"
    });
    for (field, value, malformed) in [
        (
            "parent_attempt",
            json!(AttemptId::from_bytes([5; 16])),
            json!(false),
        ),
        ("preflight_digest_version", json!(1), json!("invalid")),
        ("refusal_notice", json!("inbox"), json!("invalid")),
        ("refusal_notice_done", json!(true), json!("invalid")),
        ("refusal_code", json!("dispatch_refused"), json!(false)),
        (
            "origin_intent_digest",
            json!(Digest::from_bytes([4; 32])),
            json!(false),
        ),
    ] {
        let mut wire = baseline.clone();
        wire[field] = value.clone();
        let bytes = serde_json::to_vec(&wire).unwrap();
        let record: OperationRecord = decode(bytes.as_slice(), Limits::default()).unwrap();
        let encoded = serde_json::to_value(&record).unwrap();
        assert_eq!(encoded[field], value, "{field}");
        assert!(!record.extra.contains_key(field), "{field}");
        wire[field] = malformed;
        let bytes = serde_json::to_vec(&wire).unwrap();
        assert!(
            matches!(
                decode::<OperationRecord, _>(bytes.as_slice(), Limits::default()),
                Err(DecodeError::Typed { .. })
            ),
            "{field}"
        );
    }
}

#[test]
fn delivery_reason_round_trips_and_rejects_malformed_values() {
    use provenant_records::run::WriterDeliveryState;
    for reason in ["parent busy", "publication interrupted"] {
        let wire = json!({"state":"failed","stage":"integrate","reason":reason});
        let bytes = serde_json::to_vec(&wire).unwrap();
        let state: WriterDeliveryState = decode(bytes.as_slice(), Limits::default()).unwrap();
        assert_eq!(serde_json::to_value(state).unwrap(), wire);
    }
    let malformed = br#"{"state":"failed","stage":"integrate","reason":false}"#;
    assert!(matches!(
        decode::<WriterDeliveryState, _>(malformed.as_slice(), Limits::default()),
        Err(DecodeError::Typed { .. })
    ));
}

#[test]
fn custody_diagnostics_round_trip_and_reject_malformed_values() {
    use provenant_records::{WorkKey, diagnostic::Diagnostic};
    let key = WorkKey::new(ProjectId::from_bytes([1; 16]), "typed", "writer", 1).unwrap();
    let custody = json!({
        "scope":{"attempt":AttemptId::from_bytes([2;16]),"generation":1},
        "fence":"fence.json","lane":key,"reconcile":"resume", "abandon":"recover"
    });
    for (field, malformed) in [
        ("scope", json!(false)),
        ("fence", json!(false)),
        ("lane", json!(false)),
        ("reconcile", json!(false)),
        ("abandon", json!(false)),
    ] {
        let mut wire = json!({"code":"retained_writer_import","custody":custody});
        let bytes = serde_json::to_vec(&wire).unwrap();
        let decoded: Diagnostic = decode(bytes.as_slice(), Limits::default()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
        wire["custody"][field] = malformed;
        let bytes = serde_json::to_vec(&wire).unwrap();
        assert!(
            matches!(
                decode::<Diagnostic, _>(bytes.as_slice(), Limits::default()),
                Err(DecodeError::Typed { .. })
            ),
            "{field}"
        );
    }
}

#[test]
fn access_capability_sources_round_trip_and_reject_malformed_values() {
    use provenant_records::run::LaneAccess;
    for source in ["flag", "node", "project default"] {
        let wire = json!({"capability_sources":{"postgres":source}});
        let bytes = serde_json::to_vec(&wire).unwrap();
        let access: LaneAccess = decode(bytes.as_slice(), Limits::default()).unwrap();
        assert_eq!(
            access
                .capability_sources
                .get("postgres")
                .map(String::as_str),
            Some(source)
        );
        let encoded = serde_json::to_value(&access).unwrap();
        assert_eq!(encoded["capability_sources"], wire["capability_sources"]);
    }
    for malformed in [
        json!({"capability_sources":[{"name":"postgres","source":"flag"}]}),
        json!({"capability_sources":{"postgres":false}}),
    ] {
        let bytes = serde_json::to_vec(&malformed).unwrap();
        assert!(matches!(
            decode::<LaneAccess, _>(bytes.as_slice(), Limits::default()),
            Err(DecodeError::Typed { .. })
        ));
    }
}

#[test]
fn graph_attempt_key_fields_round_trip_and_reject_malformed_values() {
    use provenant_records::graph::AttemptKey;

    let known_fields = [
        "retry_generation",
        "previous_attempt_ref",
        "graph_id",
        "node_id",
        "attempt",
        "route_index",
    ];
    let baseline = json!({
        "retry_generation": 3,
        "previous_attempt_ref": "parent/attempt-2",
        "graph_id": "typed-graph",
        "node_id": "typed-node",
        "attempt": 4,
        "route_index": 1,
        "future_extension": {"kept": true},
    });
    let bytes = serde_json::to_vec(&baseline).unwrap();
    let record: AttemptKey = decode(bytes.as_slice(), Limits::default()).unwrap();
    let encoded = serde_json::to_value(&record).unwrap();
    for field in known_fields {
        assert_eq!(encoded[field], baseline[field], "{field}");
        assert!(!record.extra.contains_key(field), "{field}");
    }
    assert_eq!(
        record.extra.get("future_extension"),
        Some(&json!({"kept": true}))
    );
    assert_eq!(encoded["future_extension"], baseline["future_extension"]);

    for (field, malformed) in [
        ("retry_generation", json!("three")),
        ("previous_attempt_ref", json!(false)),
        ("graph_id", json!(false)),
        ("node_id", json!(false)),
        ("attempt", json!("four")),
        ("route_index", json!(false)),
    ] {
        let mut candidate = baseline.clone();
        candidate[field] = malformed;
        let bytes = serde_json::to_vec(&candidate).unwrap();
        assert!(
            matches!(
                decode::<AttemptKey, _>(bytes.as_slice(), Limits::default()),
                Err(DecodeError::Typed { .. })
            ),
            "{field}"
        );
    }
}
