#[allow(
    unused_imports,
    reason = "the custom worktree driver includes this module without libtest"
)]
use super::*;
#[test]
fn peer_handoff_is_validated_and_retained_losslessly() {
    let sealed = |note: &str| {
        serde_json::json!({"Ok":{"schema_version":2,"status":"done","note":note,
                "remaining":[],"decisions":[],"verify":[]}})
    };
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let decode = |value: serde_json::Value| -> provenant_records::handoff::HandoffResult {
        serde_json::from_value(value).unwrap()
    };
    let clean = peer_handoff(Some(&decode(sealed(&format!("key {token}")))));
    let note = clean.unwrap().unwrap().note;
    assert_eq!(note, format!("key {token}"));
    // A malformed outcome stays explicit evidence; absence stays absence.
    let error = provenant_records::handoff::HandoffError::new("schema_version", "expected 2");
    assert_eq!(peer_handoff(Some(&Err(error.clone()))), Err(error));
    assert_eq!(peer_handoff(Some(&Ok(None))), Ok(None));
    assert_eq!(peer_handoff(None), Ok(None));
}
