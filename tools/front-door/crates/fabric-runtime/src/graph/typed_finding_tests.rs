use super::*;

#[test]
fn finding_typed_fields_round_trip_and_reject_malformed_values() {
    let finding = Finding {
        extra: [(
            "future_field".into(),
            serde_json::json!({"from":"newer writer"}),
        )]
        .into(),
        url: Some("https://checks/1".into()),
        trigger: Some(FindingTrigger {
            sequence: "invoke default path".into(),
            reach: FindingReach::DefaultPath,
            observed: true,
        }),
        triage: Some(FindingTriage {
            decision: "confirmed".into(),
            reason: "reproduced".into(),
            worth_fixing: true,
            root_cause: "bad boundary".into(),
            blocking_class: "boundary".into(),
            shared_flaw: false,
        }),
        class: Some("boundary".into()),
        verification: Some("falsified".into()),
        evidence: Some("regression reproduces".into()),
        proposed_fix: Some("validate before use".into()),
        kind: Some("spec_defect".into()),
        lens: Some("security".into()),
        propagates: Some("review:F1".into()),
        design_defect: Some(true),
        risk_grew: Some(false),
        scope_grew: Some(true),
        id: "F1".into(),
        severity: "high".into(),
        file: "src/lib.rs".into(),
        line: Some(12),
        summary: "invalid boundary".into(),
        status: "open".into(),
        disposition: Some("accepted".into()),
        reason: Some("verified".into()),
    };
    let encoded = serde_json::to_value(&finding).unwrap();
    assert_eq!(serde_json::from_value::<Finding>(encoded).unwrap(), finding);

    let malformed = [
        ("url", serde_json::json!(false)),
        ("trigger", serde_json::json!("default")),
        ("triage", serde_json::json!("confirmed")),
        ("class", serde_json::json!(false)),
        ("verification", serde_json::json!(false)),
        ("evidence", serde_json::json!(7)),
        ("proposed_fix", serde_json::json!(false)),
        ("kind", serde_json::json!(7)),
        ("lens", serde_json::json!(false)),
        ("propagates", serde_json::json!(false)),
        ("design_defect", serde_json::json!("yes")),
        ("risk_grew", serde_json::json!("yes")),
        ("scope_grew", serde_json::json!("yes")),
    ];
    for (field, value) in malformed {
        let mut encoded = serde_json::to_value(&finding).unwrap();
        encoded[field] = value;
        let bytes = serde_json::to_vec(&encoded).unwrap();
        assert!(
            matches!(
                provenant_records::decode::<Finding, _>(
                    bytes.as_slice(),
                    provenant_records::Limits::default()
                ),
                Err(provenant_records::DecodeError::Typed { .. })
            ),
            "{field}"
        );
    }
}
