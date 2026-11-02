use provenant_core::{AttemptId, Digest, ProjectId, RunId, TaskId, WallTime};
use provenant_records::{
    CanonicalV1,
    run::{AttemptRecord, EffectDisposition, Outcome, SettlementReceipts, TerminalEvidence},
};

#[test]
fn receipt_legacy_record_defaults_and_unsettled_roundtrip() -> Result<(), Box<dyn std::error::Error>>
{
    let legacy = serde_json::json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "receipt-test",
        "attempt_number": 1, "revision": 1, "generation": 0,
        "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
        "authority_digest": Digest::from_bytes([5; 32]),
        "queued_at": WallTime::from_micros(1)?,
        "state": {"state":"running"}, "usage": {},
        "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"},"digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
    });
    let mut record: AttemptRecord = serde_json::from_value(legacy)?;
    assert_eq!(record.terminal_evidence, None);
    assert_eq!(record.settlement_receipts, None);
    record.schema_version = 2;
    record.state = provenant_records::run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: Outcome::Unsettled,
        exit_code: None,
        finished_at: WallTime::from_micros(2)?,
        reason: Some("missing evidence".into()),
    };
    record.terminal_evidence = Some(TerminalEvidence::Unsettled {
        reason: "missing evidence".into(),
    });
    record.settlement_receipts = Some(SettlementReceipts {
        git_import: EffectDisposition::Failed {
            reason: "busy".into(),
            retryable: true,
        },
        ..Default::default()
    });
    let bytes = serde_json::to_vec(&record)?;
    assert_eq!(serde_json::from_slice::<AttemptRecord>(&bytes)?, record);
    Ok(())
}
