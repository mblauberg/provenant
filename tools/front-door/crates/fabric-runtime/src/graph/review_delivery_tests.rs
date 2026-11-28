use super::*;

#[test]
fn delivery_review_requires_complete_nonblocking_evidence() {
    let mut outputs = NodeOutputs {
        verdict: Some("VERDICT: INCONCLUSIVE".into()),
        findings_available: Some(true),
        ..Default::default()
    };
    assert!(delivery_matches(
        AcceptBelow::Blocking,
        &outputs,
        "VERDICT: PASS"
    ));
    assert!(!delivery_matches(
        AcceptBelow::Off,
        &outputs,
        "VERDICT: PASS"
    ));
    outputs.findings_available = None;
    assert!(!delivery_matches(
        AcceptBelow::Blocking,
        &outputs,
        "VERDICT: PASS"
    ));
    outputs.findings_available = Some(true);
    outputs.partial = true;
    assert!(!delivery_matches(
        AcceptBelow::Blocking,
        &outputs,
        "VERDICT: PASS"
    ));
    outputs.partial = false;
    outputs.findings.push(
        serde_json::from_value(serde_json::json!({
            "id":"F1", "severity":"blocking", "file":"a.rs", "line":1,
            "summary":"defect", "status":"open"
        }))
        .unwrap(),
    );
    assert!(!delivery_matches(
        AcceptBelow::Blocking,
        &outputs,
        "VERDICT: PASS"
    ));
    outputs.findings.clear();
    outputs.verdict = Some("VERDICT: FAIL".into());
    assert!(!delivery_matches(
        AcceptBelow::Blocking,
        &outputs,
        "VERDICT: PASS"
    ));
}

#[test]
fn delivery_review_evidence_parks_contract_errors_but_not_valid_failures() {
    assert!(malformed_delivery_evidence_error(Some(
        "review_verdict_invalid"
    )));
    assert!(malformed_delivery_evidence_error(Some(
        "review_findings_invalid"
    )));
    assert!(malformed_delivery_evidence_error(Some(
        "review_triage_missing"
    )));
    assert!(!malformed_delivery_evidence_error(None));
    assert!(!malformed_delivery_evidence_error(Some("review_failed")));
}
