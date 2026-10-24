mod support;
use serde_json::{Value, json};
use std::{fs, path::Path};
use support::{Result, TestRig, code, mcp::McpClient};

#[test]
fn cli_and_mcp_usage_records_exclude_arguments_prompts_and_results() -> Result {
    let mut rig = TestRig::new()?;
    rig.enable_usage();
    let out = rig.command(&["output", "--id", "sensitive-argument", "--json"])?;
    assert_ne!(code(&out), 0);
    assert_eq!(code(&rig.command(&["root", "--json"])?), 0);
    let mut client = McpClient::default();
    client.initialise("2025-11-25")?;
    client.initialised()?;
    client.call("fabric_identity", json!({"op":"inspect"}))?;
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
        ("fabric_identity", "ok"),
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
fn usage_report_includes_zero_count_cli_verbs_and_mcp_tools() -> Result {
    let mut rig = TestRig::new()?;
    rig.enable_usage();
    let report = rig.command(&["telemetry", "stats", "--usage", "--since", "30d", "--json"])?;
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
            .any(|v| v["kind"] == "mcp.tool" && v["unit"] == "fabric_identity")
    );
    rig.assert_legacy_unchanged()
}
