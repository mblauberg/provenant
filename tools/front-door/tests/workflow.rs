//! Workflow leaves through the isolated binary: receipts, review coverage, sessions.
mod support;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn call(rig: &TestRig, args: &[&str], expected: i64) -> Result<Value> {
    let mut args = args.to_vec();
    args.push("--json");
    let output = rig.command(&args)?;
    assert_eq!(
        code(&output),
        expected,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}
fn request(rig: &TestRig, name: &str, value: Value) -> Result<String> {
    let path = rig.root.join(name);
    std::fs::write(&path, serde_json::to_vec(&value)?)?;
    Ok(path.to_string_lossy().into_owned())
}
fn receipt_request(critical: &str, decision: Value) -> Value {
    let digest = format!("sha256:{}", "0".repeat(64));
    json!({
        "subject": {"kind": "document", "path": {"encoding": "utf8", "path": "docs/x.md"}, "digest": digest},
        "authors": [{"kind": "user", "actor_id": "01".repeat(16)}],
        "risk": {
            "factors": {
                "blast_radius": "local", "reversibility": "easy", "data_sensitivity": "public",
                "migration": "none", "oracle_quality": "strong", "external_effects": "none",
                "critical_surface": critical
            },
            "tier": if critical == "none" { "routine" } else { "substantial" }
        },
        "decision": decision
    })
}
const DELIVERY: &str = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";

#[test]
fn session_checkpoints_round_trip_and_reject_unsafe_keys() -> Result {
    let rig = TestRig::new()?;
    let file = rig.root.join("checkpoint.md");
    std::fs::write(&file, "# Goal\nship it\n")?;
    let file = file.to_string_lossy().into_owned();
    call(
        &rig,
        &["workflow", "session", "save", "s-1", "--file", &file],
        0,
    )?;
    let shown = call(&rig, &["workflow", "session", "show", "s-1"], 0)?;
    assert_eq!(shown["data"]["markdown"], "# Goal\nship it\n");
    call(&rig, &["workflow", "session", "show", "s-2"], 1)?;
    call(
        &rig,
        &["workflow", "session", "save", "../x", "--file", &file],
        2,
    )?;
    Ok(())
}

#[test]
fn routine_receipt_saves_conflicts_and_accepts_without_reviews() -> Result {
    let rig = TestRig::new()?;
    let pending = request(
        &rig,
        "r1.json",
        receipt_request("none", json!({"status": "pending"})),
    )?;
    let saved = call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "0",
            "--request",
            &pending,
        ],
        0,
    )?;
    assert_eq!(saved["data"]["receipt"]["revision"], 1);
    call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "0",
            "--request",
            &pending,
        ],
        1,
    )?;
    let accept = request(
        &rig,
        "r2.json",
        receipt_request(
            "none",
            json!({"status": "accepted", "reason": "checks green"}),
        ),
    )?;
    let saved = call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "1",
            "--request",
            &accept,
        ],
        0,
    )?;
    assert_eq!(saved["data"]["receipt"]["decision"]["status"], "accepted");
    let shown = call(&rig, &["workflow", "receipt", "show", DELIVERY], 0)?;
    assert_eq!(shown["data"]["revision"], 2);
    Ok(())
}

#[test]
fn substantial_acceptance_is_refused_until_reviews_cover_it() -> Result {
    let rig = TestRig::new()?;
    let pending = request(
        &rig,
        "s1.json",
        receipt_request("public_contract", json!({"status": "pending"})),
    )?;
    call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "0",
            "--request",
            &pending,
        ],
        0,
    )?;
    let accept = request(
        &rig,
        "s2.json",
        receipt_request(
            "public_contract",
            json!({"status": "accepted", "reason": "looks fine"}),
        ),
    )?;
    let refused = call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "1",
            "--request",
            &accept,
        ],
        1,
    )?;
    assert_eq!(refused["error"]["code"], "REVIEW_INCOMPLETE");
    assert_eq!(refused["data"]["status"], "incomplete");
    let review = request(&rig, "s3.json", json!({"delivery_id": DELIVERY}))?;
    let report = call(&rig, &["workflow", "review", "--request", &review], 1)?;
    assert_eq!(report["data"]["status"], "incomplete");
    assert_eq!(report["data"]["required"], 1);
    Ok(())
}
