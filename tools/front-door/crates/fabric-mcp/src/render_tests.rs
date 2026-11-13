use super::*;
use serde_json::json;

#[test]
fn digest_keeps_route_provenance_exact_and_skips_long_routes() {
    let with_effort = digest("fabric_run", &json!({"route":"codex/o3@high"}));
    assert!(with_effort.starts_with("fabric_run"));
    assert!(with_effort.contains("route=codex/o3@high"));

    let obj_effort = digest(
        "fabric_run",
        &json!({"route":{"adapter":"codex","model":"o3","effort":"high"}}),
    );
    assert!(obj_effort.contains("route=codex/o3@high"));

    // Merged from digest_keeps_routes_without_effort:
    let no_effort = digest(
        "fabric_run",
        &json!({"route":{"adapter":"codex","model":"o3"}}),
    );
    assert!(no_effort.contains("route=codex/o3"));
    assert!(!no_effort.contains("@"));

    let long = "r".repeat(181);
    let bounded = digest("fabric_run", &json!({"route":long}));
    assert!(!bounded.contains(&long));
    assert!(bounded.len() < 181);
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
