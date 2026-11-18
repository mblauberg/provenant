use provenant_records::decode_v1;
use provenant_supervisor::custody::StoredExit;

// Spec: docs/specs/state.md#strict-bounded-records
#[test]
fn reads_provenant_2_0_0_process_exit_record() {
    let raw = include_bytes!("fixtures/5e0bd68c-process-exit.json");
    let exit: StoredExit = decode_v1(
        raw.as_slice(),
        provenant_records::Limits {
            max_bytes: 1024 * 1024,
            ..Default::default()
        },
    )
    .expect("Provenant 2.0.0 process-exit.json must parse without error");

    assert_eq!(
        exit.scope.attempt.to_string(),
        "5e0bd68cca6c67b005e8c30878b5d622"
    );
    assert_eq!(exit.schema_version, 1);
    assert!(!exit.provider_identity_conflict);
    assert!(!exit.provider_identity_evidence_incomplete);
    assert!(!exit.provider_identity_eof_fragment);
}
