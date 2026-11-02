//! Native console contracts and replay fixture generation.
mod support;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn cli(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let mut args = args.to_vec();
    args.push("--json");
    let output = rig.command(&args)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["status"], "ok");
    assert!(envelope["error"].is_null());
    for key in ["schema_version", "command", "status", "data", "error"] {
        assert!(envelope.get(key).is_some(), "{key}");
    }
    Ok(envelope)
}

fn fixture(rig: &TestRig, name: &str, envelope: &Value) -> Result {
    // Opt-in regeneration keeps ordinary tests read-only with respect to the checkout.
    if let Some(directory) = std::env::var_os("PROVENANT_CONSOLE_FIXTURES") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        let text = serde_json::to_string_pretty(envelope)?;
        let mut bytes = text
            .replace(rig.root.to_str().ok_or("fixture root")?, "/fixture")
            .into_bytes();
        bytes.push(b'\n');
        std::fs::write(directory.join(format!("{name}.json")), bytes)?;
    }
    Ok(())
}

#[test]
fn console_catalogues_use_registered_labels_and_saved_summaries() -> Result {
    let rig = TestRig::new()?;
    let checkpoint = rig.root.join("checkpoint.md");
    std::fs::write(&checkpoint, "# Console checkpoint\n")?;
    cli(
        &rig,
        &[
            "workflow",
            "session",
            "save",
            "console-chair",
            "--file",
            checkpoint.to_str().ok_or("path")?,
        ],
    )?;
    let without_summary = cli(&rig, &["console", "sessions"])?;
    assert!(without_summary["data"]["sessions"][0]["summary"].is_null());
    assert_eq!(
        without_summary["data"]["sessions"][0]["summary_status"],
        "unavailable"
    );
    fixture(&rig, "sessions-without-summary", &without_summary)?;
    let patch = rig.root.join("summary.json");
    std::fs::write(
        &patch,
        serde_json::to_vec(
            &json!({"now":"Implementing console", "next":"Review", "needs_you":[], "risk":"none"}),
        )?,
    )?;
    cli(
        &rig,
        &[
            "chair",
            "summary",
            "update",
            "console-chair",
            "--file",
            patch.to_str().ok_or("path")?,
            "--expect-revision",
            "0",
            "--operation-id",
            "console-summary",
        ],
    )?;
    let projects = cli(&rig, &["console", "projects"])?;
    assert_eq!(projects["data"]["schema_version"], 2);
    assert_eq!(projects["data"]["projects"][0]["label"], "project");
    fixture(&rig, "projects", &projects)?;
    let sessions = cli(&rig, &["console", "sessions"])?;
    assert_eq!(sessions["data"]["schema_version"], 2);
    let session = &sessions["data"]["sessions"][0];
    assert_eq!(session["summary"]["now"], "Implementing console");
    assert_eq!(session["summary"], session["record"]["summary"]);
    assert_eq!(session["summary_status"], "available");
    fixture(&rig, "sessions", &sessions)?;
    let draft = rig.root.join("request.json");
    std::fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "kind":"decision", "title":"Review console", "body":"Review request",
            "blocking":true, "options":[], "recommended_option_id":null, "proceeding_with":null
        }))?,
    )?;
    cli(
        &rig,
        &[
            "ask",
            "--file",
            draft.to_str().ok_or("path")?,
            "--session",
            "console-chair",
        ],
    )?;
    let overview = cli(&rig, &["console", "overview"])?;
    assert_eq!(overview["data"]["schema_version"], 2);
    let attention = &overview["data"]["needs_you"][0];
    assert_eq!(attention["title"], "Review console");
    assert_eq!(attention["project_label"], "project");
    assert!(attention.get("task_label").is_none());
    fixture(&rig, "overview", &overview)?;
    let board = cli(&rig, &["console", "board"])?;
    assert_eq!(board["data"]["schema_version"], 3);
    assert!(board["data"]["coverage"].get("estimated_cost").is_none());
    assert!(board["data"]["coverage"].get("verified_links").is_none());
    fixture(&rig, "board-empty", &board)?;
    fixture(&rig, "changes", &cli(&rig, &["console", "changes"])?)?;
    Ok(())
}
