//! Stored checkpoint review fields and admission recovery.
use super::*;
use serde_json::json;

fn stored() -> StoredCheckpoint {
    super::journal::stored_checkpoint_fixture()
}

#[test]
fn graph_schema_v2_is_required_and_stored_digests_validate() {
    let legacy = stored();
    let digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        CHECKPOINT_SCHEMA_VERSION,
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

    let mut value = serde_json::to_value(&legacy.execution.document).unwrap();
    value["nodes"][0]["review_base"] = json!("main");
    value["schema_version"] = json!(GRAPH_DOCUMENT_SCHEMA_VERSION - 1);
    let v1: GraphDocument = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(
        validate_plan_with_limits(&v1, &Default::default()),
        Err(AdmissionError::UnsupportedVersion { found, supported })
            if found == GRAPH_DOCUMENT_SCHEMA_VERSION - 1
                && supported == GRAPH_DOCUMENT_SCHEMA_VERSION
    ));
    value["schema_version"] = json!(GRAPH_DOCUMENT_SCHEMA_VERSION);
    let v2: GraphDocument = serde_json::from_value(value).unwrap();
    assert_eq!(
        serde_json::from_slice::<GraphDocument>(&serde_json::to_vec(&v2).unwrap()).unwrap(),
        v2
    );
}

#[test]
fn journal_requires_and_projects_execution_bases_for_review_documents() {
    let mut record = stored();
    record.execution.document.review_base = Some("base".into());
    record.execution.document.schema_version = GRAPH_DOCUMENT_SCHEMA_VERSION;
    record.execution.bases = GraphBaseBinding::new(
        Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap()),
        None,
    );
    record.digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        CHECKPOINT_SCHEMA_VERSION,
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
        CHECKPOINT_SCHEMA_VERSION,
        &(&record.checkpoint, &record.execution),
    )
    .unwrap()
    .to_string();
    assert!(matches!(
        decode_checkpoint(&serde_json::to_vec(&record).unwrap(), "g-test"),
        Err(JournalError::BadEnvelope)
    ));
    record.execution.document.schema_version = GRAPH_DOCUMENT_SCHEMA_VERSION - 1;
    let bytes = serde_json::to_vec(&record).unwrap();
    assert!(matches!(
        decode_checkpoint(&bytes, "g-test"),
        Err(JournalError::Decode(DecodeError::UnsupportedVersion { found, .. }))
            if found == GRAPH_DOCUMENT_SCHEMA_VERSION - 1
    ));
}

#[test]
fn admission_recovery_capability() {
    fn bytes(mut record: StoredCheckpoint) -> Vec<u8> {
        record.digest = CanonicalV1::record_digest(
            "fabric-graph-checkpoint",
            CHECKPOINT_SCHEMA_VERSION,
            &(&record.checkpoint, &record.execution),
        )
        .unwrap()
        .to_string();
        serde_json::to_vec(&record).unwrap()
    }
    let seed = stored;

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
    plain_v2.execution.document.schema_version = GRAPH_DOCUMENT_SCHEMA_VERSION;
    assert!(decode_checkpoint(&bytes(plain_v2), "g-test").is_ok());

    let mut unsupported = seed();
    unsupported.execution.document.schema_version = GRAPH_DOCUMENT_SCHEMA_VERSION + 1;
    assert!(matches!(
        decode_checkpoint(&bytes(unsupported), "g-test"),
        Err(JournalError::Decode(DecodeError::UnsupportedVersion {
            found,
            ..
        })) if found == GRAPH_DOCUMENT_SCHEMA_VERSION + 1
    ));

    let mut bad_digest = seed();
    bad_digest.digest = "sha256:bad".into();
    assert!(matches!(
        decode_checkpoint(&serde_json::to_vec(&bad_digest).unwrap(), "g-test"),
        Err(JournalError::BadDigest)
    ));
}
