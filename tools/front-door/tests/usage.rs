mod support;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use support::{Result, TestRig, code, mcp::McpClient};

#[test]
fn cli_and_mcp_usage_records_exclude_arguments_prompts_and_results() -> Result {
    let rig = TestRig::new()?;
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    let out = rig.command(&["output", "--id", "sensitive-argument", "--json"])?;
    assert_ne!(code(&out), 0);
    assert_eq!(code(&rig.command(&["root", "--json"])?), 0);
    let mut client = McpClient::default();
    client.initialise("2025-11-25")?;
    client.initialised()?;
    client.call("fabric_context", json!({"op":"inspect"}))?;
    client.call("fabric_mail", json!({"op":"send","body":"secret-body"}))?;
    client.call("secret-unknown-tool", json!({"secret":"secret-value"}))?;
    client.call("fabric_run", json!({"op":"list","limit":1,"cursor":null}))?;
    client.eof(
        &rig,
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["fabric", "mcp", "--client", "codex"],
    )?;
    let log = fs::read_to_string(rig.state.join("usage.jsonl"))?;
    assert!(!log.contains("sensitive-argument"));
    assert!(!log.contains("secret"));
    let events = log
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(events.len(), 7);
    assert_eq!(events[0]["unit"], "parse");
    assert_eq!(events[0]["exit_code"], code(&out));
    for event in &events {
        let object = event.as_object().ok_or("event")?;
        assert!(!object.contains_key("project_id"));
        assert_eq!(object.len(), 5);
        assert!(event["duration_ms"].is_u64());
        assert!(event["timestamp"].is_string());
        assert!(object.keys().all(|key| {
            [
                "kind",
                "unit",
                "exit_code",
                "outcome",
                "duration_ms",
                "timestamp",
            ]
            .contains(&key.as_str())
        }));
    }
    for (tool, outcome) in [
        ("fabric_context", "ok"),
        ("fabric_mail", "error"),
        ("unknown", "error"),
        ("fabric_run", "error"),
    ] {
        assert!(events.iter().any(|row| row["kind"] == "mcp.tool"
            && row["unit"] == tool
            && row["outcome"] == outcome));
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn project_usage_is_captured_before_commands_and_labels_subset_rows() -> Result {
    let rig = TestRig::new()?;
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[knobs]\n\"usage.project_attribution\" = true\n",
    )?;
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = identity["data"]["project"].as_str().ok_or("project")?;
    let log = fs::read_to_string(rig.state.join("usage.jsonl"))?;
    let first: Value = serde_json::from_str(log.lines().next().ok_or("event")?)?;
    // This command allocated the binding, so it had no project at entry.
    assert!(first.get("project_id").is_none());
    assert_eq!(code(&rig.command(&["root"])?), 0);
    let json = rig.command(&["telemetry", "stats", "--usage", "--json"])?;
    assert_eq!(code(&json), 0);
    let json: Value = serde_json::from_slice(&json.stdout.bytes)?;
    let units = json["data"]["units"].as_array().ok_or("units")?;
    assert!(units.iter().any(|row| row["unit"] == "root"
        && row["project_id"] == project
        && row["scope"] == "project"
        && row["calls"] == 1));
    let aggregate = units
        .iter()
        .find(|row| row["unit"] == "root" && row.get("project_id").is_none())
        .ok_or("aggregate")?;
    assert_eq!(aggregate["calls"], 1);
    assert!(aggregate.get("scope").is_none());
    let text = rig.command(&["telemetry", "stats", "--usage"])?;
    assert_eq!(code(&text), 0);
    assert!(
        String::from_utf8_lossy(&text.stdout.bytes)
            .lines()
            .any(|line| line.contains(&format!("cli.verb  root  project={project}")))
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn usage_report_includes_zero_count_cli_verbs_and_mcp_tools() -> Result {
    let rig = TestRig::new()?;
    let report = rig.command(&[
        "telemetry",
        "stats",
        "--usage",
        "--since",
        "2592000",
        "--json",
    ])?;
    assert_eq!(
        code(&report),
        0,
        "{}",
        String::from_utf8_lossy(&report.stdout.bytes)
    );
    let report: Value = serde_json::from_slice(&report.stdout.bytes)?;
    let units = report["data"]["units"].as_array().ok_or("units")?;
    assert!(units.iter().all(|v| v["calls"] == 0));
    assert!(
        units
            .iter()
            .any(|v| v["kind"] == "cli.verb" && v["unit"] == "quota tick")
    );
    assert!(
        units
            .iter()
            .any(|v| v["kind"] == "mcp.tool" && v["unit"] == "fabric_context")
    );
    rig.assert_legacy_unchanged()
}
