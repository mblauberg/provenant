use super::*;
use serde_json::json;

#[test]
fn digest_keeps_route_provenance_exact_and_skips_long_routes() {
    assert_eq!(
        digest("fabric_run", &json!({"route":"codex/o3@high"})),
        "fabric_run route=codex/o3@high"
    );
    assert_eq!(
        digest(
            "fabric_run",
            &json!({"route":{"adapter":"codex","model":"o3","effort":"high"}})
        ),
        "fabric_run route=codex/o3@high"
    );
    let long = "r".repeat(181);
    assert_eq!(digest("fabric_run", &json!({"route":long})), "fabric_run");
}

#[test]
fn digest_keeps_routes_without_effort() {
    assert_eq!(
        digest(
            "fabric_run",
            &json!({"route":{"adapter":"codex","model":"o3"}})
        ),
        "fabric_run route=codex/o3"
    );
}

#[test]
fn digest_does_not_repeat_message_bodies_or_output_data() {
    let text = digest(
        "fabric_mail",
        &json!({"body":"secret body","data":"long output","id":"00000000000000000000000000000001","state":"open"}),
    );
    assert!(!text.contains("secret body"));
    assert!(!text.contains("long output"));
    assert!(text.contains("state=open"));
}
