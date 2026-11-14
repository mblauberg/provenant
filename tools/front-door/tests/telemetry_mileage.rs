// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use support::{Result, TestRig, code};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0) as i64
}

fn reading(at: i64, used: u64, resets: i64) -> Value {
    json!({
        "schema_version": 1,
        "observed_unix_seconds": at,
        "account_key": "work",
        "label": "work",
        "provider": "claude",
        "status": "available",
        "plan": "max_5x",
        "tier": null,
        "windows": [{"pool":"five_hour","window_seconds":18000,"used_basis_points":used,"resets_unix_seconds":resets}],
        "credit_balance": null,
        "reset_credits": null,
        "markers": [],
    })
}

fn message(at: i64, id: &str, output: u64) -> Value {
    json!({
        "type": "assistant",
        "timestamp": jiff::Timestamp::from_second(at)
            .map(|time| time.to_string())
            .unwrap_or_default(),
        "message": {
            "id": id,
            "model": "claude-opus-5-5",
            "content": "fixture",
            "usage": {
                "input_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "output_tokens": output
            }
        }
    })
}

fn fixture() -> Result<TestRig> {
    let rig = TestRig::new()?;
    fs::create_dir_all(rig.project.join("config"))?;
    fs::write(
        rig.project.join("config/telemetry-prices.toml"),
        include_str!("../../../config/telemetry-prices.toml"),
    )?;
    fs::create_dir_all(rig.state.join("quota/history"))?;
    let now = now();
    let resets = now + 3600;
    // Three readings: 10%, 30% and 60% of one five-hour window.
    let history = [
        reading(now - 1_800, 1_000, resets),
        reading(now - 900, 3_000, resets),
        reading(now, 6_000, resets),
    ]
    .into_iter()
    .map(|record| serde_json::to_string(&record))
    .collect::<std::result::Result<Vec<_>, _>>()?
    .join("\n");
    fs::write(
        rig.state.join("quota/history/work.claude.jsonl"),
        format!("{history}\n"),
    )?;
    // $20 of output priced usage across the first 20 points and $30 across the
    // next 30. Output is $20 per million tokens. Feed the public native ingest
    // boundary so this fixture follows the current segmented store format.
    let native = rig.root.join("claude");
    let session = native.join("projects/-fixture/s1.jsonl");
    fs::create_dir_all(session.parent().ok_or("session parent")?)?;
    let transcript = [
        message(now - 900, "m1", 1_000_000),
        message(now, "m2", 1_500_000),
    ]
    .into_iter()
    .map(|record| serde_json::to_string(&record))
    .collect::<std::result::Result<Vec<_>, _>>()?
    .join("\n");
    fs::write(&session, format!("{transcript}\n"))?;
    let config = format!(
        "schema_version = 1\n[domains.accounts.entries.claude-work]\nadapter = \"claude\"\nlabel = \"work\"\nkey = \"work\"\ndirectory = \"{}\"\n[[domains.accounts.entries.claude-work.plans]]\nname = \"max_5x\"\nprice = 100.0\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"\n",
        native.display()
    );
    fs::write(rig.state.join("config.toml"), config)?;
    let ingested = rig.command(&["telemetry", "ingest", "--provider", "claude", "--json"])?;
    assert_eq!(
        code(&ingested),
        0,
        "{}",
        String::from_utf8_lossy(&ingested.stderr.bytes)
    );
    Ok(rig)
}

#[test]
fn three_readings_give_exact_mileage_and_capacity() -> Result {
    let rig = fixture()?;
    let output = rig.command(&["telemetry", "mileage", "--json"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let json: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let account = &json["data"]["accounts"][0];
    assert_eq!(account["account_key"], "work");
    assert_eq!(account["price_usd"]["usd"], "100.000000");
    let window = &account["windows"][0];
    assert_eq!(window["window"], "five_hour");
    assert_eq!(window["mileage_usd_per_percent"], "1.000000");
    assert_eq!(window["capacity_usd_per_full_window"], "100.000000");
    assert_eq!(window["n"], 2);
    assert_eq!(window["priced_share_basis_points"], 10_000);
    assert_eq!(window["step_change"]["status"], "inconclusive");
    assert!(window.get("effective").is_none());
    Ok(())
}

#[test]
fn filters_narrow_the_report_and_missing_usage_is_a_lower_bound() -> Result {
    let rig = fixture()?;
    let output = rig.command(&["telemetry", "mileage", "--window", "weekly", "--json"])?;
    assert_eq!(code(&output), 0);
    let json: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let account = &json["data"]["accounts"][0];
    assert_eq!(account["windows"].as_array().map(Vec::len), Some(0));
    let output = rig.command(&["telemetry", "mileage", "--model", "missing-model", "--json"])?;
    assert_eq!(code(&output), 0);
    let json: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let window = &json["data"]["accounts"][0]["windows"][0];
    assert!(window["mileage_usd_per_percent"].is_null());
    assert_eq!(window["priced_share_basis_points"], 0);
    Ok(())
}
