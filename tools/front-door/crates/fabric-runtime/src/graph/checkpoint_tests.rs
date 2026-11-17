//! Stored checkpoint review fields and admission recovery.
use super::*;
use serde_json::{Value, json};

fn stored() -> Value {
    serde_json::from_str(include_str!("../fixtures/graph-checkpoint.json")).unwrap()
}

// Spec: docs/specs/state.md#graph-checkpoints-cover-persisted-text
#[test]
fn deployed_2_0_5_checkpoint_and_external_prompts_still_read() {
    const CHECKPOINT: &[u8] = include_bytes!("../fixtures/checkpoint-2.0.5/graph-smoke205-ro.json");
    let value: Value = serde_json::from_slice(CHECKPOINT).unwrap();
    let graph_id = value["checkpoint"]["graph_id"].as_str().unwrap();
    let fixture_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures/checkpoint-2.0.5");
    let temp = crate::test_support::tempdir().unwrap();

    // The checked-in fixture keeps the original envelope and every content-
    // addressed prompt blob byte-for-byte from the deployed 2.0.5 graph.
    let mut blobs = std::collections::BTreeSet::new();
    for digest in value["stored_text"].as_object().unwrap().values() {
        let digest = digest.as_str().unwrap();
        if blobs.insert(digest.to_owned()) {
            let name = format!("graph-{graph_id}.prompt-{digest}");
            let bytes = std::fs::read(fixture_dir.join(&name)).unwrap();
            assert_eq!(
                provenant_confinement::redact::known_shapes(&bytes).unwrap(),
                bytes,
                "fixture blob contains credential-shaped text"
            );
            std::fs::write(temp.path().join(name), bytes).unwrap();
        }
    }
    assert_eq!(
        provenant_confinement::redact::known_shapes(CHECKPOINT).unwrap(),
        CHECKPOINT,
        "fixture checkpoint contains credential-shaped text"
    );

    let directory = Directory::open_anchor(temp.path()).unwrap();
    let stored = decode_checkpoint_in(&directory, CHECKPOINT, graph_id).unwrap();
    assert_eq!(stored.schema_version, 2);
    assert_eq!(stored.execution.document.graph_id, graph_id);
    let (plan, _) = validate_plan_with_limits(&stored.execution.document, &Default::default())
        .expect("deployed document remains valid");
    let plan_digest = CanonicalV1::record_digest("fabric-graph-plan", 1, &plan)
        .unwrap()
        .to_string();
    assert_eq!(stored.checkpoint.plan_digest, plan_digest);
    validate_checkpoint::<std::convert::Infallible, JournalError>(
        &stored.execution.document,
        &stored.checkpoint,
        &plan,
        &plan_digest,
    )
    .expect("deployed checkpoint still validates");
}

#[test]
fn review_schema_versions_and_stored_digests() {
    let fixture = stored();
    let legacy: StoredCheckpoint = serde_json::from_value(fixture.clone()).unwrap();
    assert_eq!(serde_json::to_value(&legacy).unwrap(), fixture);
    let digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&legacy.checkpoint, &legacy.execution),
    )
    .unwrap();
    assert_eq!(digest.to_string(), legacy.digest);
    let (plan, _) =
        validate_plan_with_limits(&legacy.execution.document, &Default::default()).unwrap();
    assert_eq!(
        CanonicalV1::record_digest("fabric-graph-plan", 1, &plan)
            .unwrap()
            .to_string(),
        legacy.checkpoint.plan_digest
    );

    let mut value = fixture["execution"]["document"].clone();
    value["nodes"][0]["review_base"] = json!("main");
    let v1: GraphDocument = serde_json::from_value(value.clone()).unwrap();
    assert!(validate_plan_with_limits(&v1, &Default::default()).is_err());
    value["schema_version"] = json!(2);
    let v2: GraphDocument = serde_json::from_value(value).unwrap();
    assert_eq!(
        serde_json::from_slice::<GraphDocument>(&serde_json::to_vec(&v2).unwrap()).unwrap(),
        v2
    );
}

#[test]
fn journal_requires_and_projects_execution_bases_for_review_documents() {
    let mut record: StoredCheckpoint = serde_json::from_value(stored()).unwrap();
    record.execution.document.review_base = Some("base".into());
    record.execution.document.schema_version = 2;
    record.execution.bases = GraphBaseBinding::new(
        Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap()),
        None,
    );
    record.digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&record.checkpoint, &record.execution),
    )
    .unwrap()
    .to_string();
    let bytes = serde_json::to_vec(&record).unwrap();
    let decoded = decode_checkpoint(&bytes, "g-test").unwrap();
    assert_eq!(decoded.checkpoint.bases, record.execution.bases);
    record.execution.bases = None;
    record.digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&record.checkpoint, &record.execution),
    )
    .unwrap()
    .to_string();
    assert!(matches!(
        decode_checkpoint(&serde_json::to_vec(&record).unwrap(), "g-test"),
        Err(JournalError::BadEnvelope)
    ));
    record.execution.document.schema_version = 1;
    let bytes = serde_json::to_vec(&record).unwrap();
    assert!(matches!(
        decode_checkpoint(&bytes, "g-test"),
        Err(JournalError::BadEnvelope)
    ));
}

#[test]
fn admission_recovery_capability() {
    fn bytes(mut record: StoredCheckpoint) -> Vec<u8> {
        record.digest = CanonicalV1::record_digest(
            "fabric-graph-checkpoint",
            2,
            &(&record.checkpoint, &record.execution),
        )
        .unwrap()
        .to_string();
        serde_json::to_vec(&record).unwrap()
    }
    let seed = || -> StoredCheckpoint { serde_json::from_value(stored()).unwrap() };

    let mut execution_base = seed();
    execution_base.execution.bases = GraphBaseBinding::new(
        Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap()),
        None,
    );
    assert!(matches!(
        decode_checkpoint(&bytes(execution_base), "g-test"),
        Err(JournalError::BadEnvelope)
    ));

    let mut expansion = seed();
    expansion.checkpoint.expansions.push(
        serde_json::from_value(json!({
            "source":"planner",
            "nodes":[{"id":"added","access":"read_only","family_group":"group"}]
        }))
        .unwrap(),
    );
    assert!(matches!(
        decode_checkpoint(&bytes(expansion), "g-test"),
        Err(JournalError::BadEnvelope)
    ));

    let mut node = seed();
    node.execution.document.nodes[0].family_group = Some("group".into());
    assert!(matches!(
        decode_checkpoint(&bytes(node), "g-test"),
        Err(JournalError::BadEnvelope)
    ));

    let mut plain_v2 = seed();
    plain_v2.execution.document.schema_version = 2;
    assert!(decode_checkpoint(&bytes(plain_v2), "g-test").is_ok());

    let mut unsupported = seed();
    unsupported.execution.document.schema_version = 3;
    assert!(matches!(
        decode_checkpoint(&bytes(unsupported), "g-test"),
        Err(JournalError::Decode(DecodeError::UnsupportedVersion {
            found: 3,
            ..
        }))
    ));

    let mut bad_digest = seed();
    bad_digest.digest = "sha256:bad".into();
    assert!(matches!(
        decode_checkpoint(&serde_json::to_vec(&bad_digest).unwrap(), "g-test"),
        Err(JournalError::BadDigest)
    ));
}
