mod support;
use serde_json::Value;
use std::fs;
use support::{Result, TestRig, code};

#[test]
fn invocations_record_only_verb_status_and_timing_and_stats_include_zeroes() -> Result {
    let rig = TestRig::new()?;
    let out = rig.command(&["output", "--id", "sensitive-argument", "--json"])?;
    assert_ne!(code(&out), 0);
    let root = rig.command(&["root", "--json"])?;
    assert_eq!(code(&root), 0);
    let log = fs::read_to_string(rig.state.join("usage.jsonl"))?;
    assert!(!log.contains("sensitive-argument"));
    let events = log
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["kind"], "cli.verb");
    assert_eq!(events[0]["unit"], "parse");
    assert_eq!(events[0]["exit_code"], code(&out));
    assert_eq!(events[1]["unit"], "root");
    assert!(events[1]["duration_ms"].is_u64());
    assert!(events[1]["timestamp"].is_string());
    assert_eq!(events[1].as_object().ok_or("event")?.len(), 5);
    let report = rig.command(&["telemetry", "stats", "--usage", "--since", "30d", "--json"])?;
    assert_eq!(
        code(&report),
        0,
        "{}",
        String::from_utf8_lossy(&report.stdout.bytes)
    );
    let report: Value = serde_json::from_slice(&report.stdout.bytes)?;
    let units = report["data"]["units"].as_array().ok_or("units")?;
    assert!(units.iter().any(|v| v["unit"] == "root" && v["calls"] == 1));
    assert!(
        units
            .iter()
            .any(|v| v["unit"] == "quota tick" && v["calls"] == 0)
    );
    assert!(
        units
            .iter()
            .any(|v| v["unit"] == "fabric_identity" && v["calls"] == 0)
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn usage_failures_do_not_change_command_results() -> Result {
    let rig = TestRig::new()?;
    fs::create_dir_all(&rig.state)?;
    fs::create_dir(rig.state.join("usage.jsonl"))?;
    assert_eq!(code(&rig.command(&["root", "--json"])?), 0);
    Ok(())
}
