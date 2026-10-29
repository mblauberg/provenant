// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use support::{Result, TestRig, code, register_account};

fn message(id: &str, output: u64) -> String {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-opus-5-5\",\"content\":\"PRIVATE PROMPT\",\"usage\":{{\"input_tokens\":5,\"cache_read_input_tokens\":7,\"cache_creation_input_tokens\":11,\"output_tokens\":{output}}}}}}}\n",
        jiff::Timestamp::from_second(at as i64)
            .map(|time| time.to_string())
            .unwrap_or_default()
    )
}

fn write_session(config: &Path, session: &str, id: &str, output: u64) -> Result {
    let directory = config.join("projects/-fixture");
    fs::create_dir_all(&directory)?;
    fs::write(
        directory.join(format!("{session}.jsonl")),
        message(id, output),
    )?;
    Ok(())
}

#[test]
fn ingest_is_idempotent_and_attributes_by_directory() -> Result {
    let rig = TestRig::new()?;
    let anese = rig.home.join(".claude-anese");
    let pro = rig.home.join(".claude-pro");
    let plain = rig.home.join(".claude");
    register_account(&rig, "claude", "anese", "anese", &anese)?;
    register_account(&rig, "claude", "pro", "pro", &pro)?;
    write_session(&anese, "s1", "m1", 10)?;
    write_session(&pro, "s2", "m2", 20)?;
    write_session(&plain, "s3", "m3", 30)?;

    let first = rig.command(&["telemetry", "ingest", "--json"])?;
    assert_eq!(
        code(&first),
        0,
        "{}",
        String::from_utf8_lossy(&first.stderr.bytes)
    );
    let first: Value = serde_json::from_slice(&first.stdout.bytes)?;
    assert_eq!(first["data"]["added"], 3);
    let accounts = first["data"]["accounts"].as_object().ok_or("accounts")?;
    assert_eq!(accounts["anese"]["added"], 1);
    assert_eq!(accounts["pro"]["added"], 1);
    assert_eq!(accounts["unattributed"]["added"], 1);

    let second = rig.command(&["telemetry", "ingest", "--json"])?;
    assert_eq!(code(&second), 0);
    let second: Value = serde_json::from_slice(&second.stdout.bytes)?;
    assert_eq!(second["data"]["added"], 0);
    assert_eq!(second["data"]["already_present"], 3);

    let store = fs::read_to_string(rig.state.join("ingest.jsonl"))?;
    assert!(!store.contains("PRIVATE PROMPT"));
    assert!(store.contains("\"source\":\"lane\""));
    Ok(())
}

#[test]
fn chair_session_start_ingests_the_session_as_chair() -> Result {
    let rig = TestRig::new()?;
    let config = rig.root.join("claude");
    register_account(&rig, "claude", "current", "current", &config)?;
    write_session(&config, "chair-session", "m1", 45)?;
    let input = serde_json::to_vec(&json!({
        "hook_event_name":"SessionStart", "session_id":"chair-session", "cwd":rig.project,
        "transcript_path":rig.root.join("transcript.jsonl"), "source":"startup"
    }))?;
    let output = rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir",
            rig.state.to_str().ok_or("state")?,
            "hook",
            "claude",
            "chair-start",
        ],
        Some(&input),
    )?;
    assert_eq!(code(&output), 0);
    let store = fs::read_to_string(rig.state.join("ingest.jsonl"))?;
    let rows: Vec<Value> = store
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    let row = rows
        .first()
        .ok_or("the chair SessionStart did not ingest its session")?;
    assert_eq!(row["account"], "current");
    assert_eq!(row["source"], "chair");
    assert_eq!(row["session"], "chair-session");
    assert_eq!(row["output"], 45);
    Ok(())
}
