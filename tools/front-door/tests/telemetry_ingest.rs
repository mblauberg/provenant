// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]
mod support;
#[path = "support/terminal_run.rs"]
mod terminal_run;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use support::{
    Result, TestRig, code, install_fake_claude_auth, register_account,
    set_claude_config_dir, write_fake_claude_identity,
};

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

fn start_chair_session(
    rig: &TestRig,
    session: &str,
) -> Result<provenant_os::exec::ExecOutcome> {
    let input = serde_json::to_vec(&json!({
        "hook_event_name":"SessionStart", "session_id":session, "cwd":rig.project,
        "transcript_path":rig.root.join("transcript.jsonl"), "source":"startup"
    }))?;
    rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir",
            rig.state.to_str().ok_or("state")?,
            "hook",
            "claude",
            "chair-start",
        ],
        Some(&input),
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

    let store = stored(&rig)?;
    assert!(!store.contains("PRIVATE PROMPT"));
    assert!(store.contains("\"source\":\"lane\""));
    Ok(())
}

/// Every stored row, one JSON line each, across the dated segments.
fn stored(rig: &TestRig) -> std::result::Result<String, Box<dyn std::error::Error>> {
    let mut names: Vec<_> = fs::read_dir(rig.state.join("ingest"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::result::Result<_, _>>()?;
    names.sort();
    let mut text = String::new();
    for name in names {
        text.push_str(&fs::read_to_string(name)?);
    }
    Ok(text)
}

#[test]
fn chair_session_start_ingests_the_session_as_chair() -> Result {
    let mut rig = TestRig::new()?;
    install_fake_claude_auth(&mut rig)?;
    let config = rig.home.join(".claude-current");
    set_claude_config_dir(&mut rig, &config);
    let identity_key = write_fake_claude_identity(&config, "current@example.test")?;
    register_account(&rig, "claude", "current", &identity_key, &config)?;
    write_session(&config, "chair-session", "m1", 45)?;
    let output = start_chair_session(&rig, "chair-session")?;
    assert_eq!(code(&output), 0);
    let store = stored(&rig)?;
    let rows: Vec<Value> = store
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    let row = rows
        .first()
        .ok_or("the chair SessionStart did not ingest its session")?;
    assert_eq!(row["identity"]["identity"]["state"], "known");
    assert_eq!(row["identity"]["identity"]["provider"], "claude");
    assert_eq!(row["identity"]["identity"]["account_key"], identity_key);
    assert_eq!(row["identity"]["epoch"]["state"], "unknown");
    assert_eq!(row["identity"]["evidence_source"], "provider");
    assert!(row.get("account").is_none());
    assert_eq!(row["source"], "chair");
    assert_eq!(row["session"], "chair-session");
    assert_eq!(row["output"], 45);
    Ok(())
}

#[test]
fn provider_identity_rotation_is_preserved_per_chair_session_in_ingest() -> Result {
    let mut rig = TestRig::new()?;
    install_fake_claude_auth(&mut rig)?;
    let config = rig.home.join(".claude-rotating");
    set_claude_config_dir(&mut rig, &config);

    let first = write_fake_claude_identity(&config, "first@example.test")?;
    let second = provenant_providers::identity::fingerprint("second@example.test")
        .ok_or("invalid second identity")?;
    register_account(&rig, "claude", "rotating", &first, &config)?;
    write_session(&config, "session-a", "message-a", 3)?;
    assert_eq!(code(&start_chair_session(&rig, "session-a")?), 0);

    assert_eq!(
        write_fake_claude_identity(&config, "second@example.test")?,
        second
    );
    write_session(&config, "session-b", "message-b", 9)?;
    assert_eq!(code(&start_chair_session(&rig, "session-b")?), 0);

    let rows: Vec<Value> = stored(&rig)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(rows.len(), 2);
    let row_for = |session: &str| rows.iter().find(|row| row["session"] == session).unwrap();
    for (session, key) in [("session-a", &first), ("session-b", &second)] {
        let identity = &row_for(session)["identity"];
        assert_eq!(identity["identity"]["state"], "known");
        assert_eq!(identity["identity"]["provider"], "claude");
        assert_eq!(identity["identity"]["account_key"], key.as_str());
        assert_eq!(identity["epoch"]["state"], "unknown");
        assert_eq!(identity["evidence_source"], "provider");
    }
    assert_ne!(first, second);
    Ok(())
}

#[test]
fn chair_stop_ingests_new_usage() -> Result {
    let rig = TestRig::new()?;
    let config = rig.root.join("claude");
    register_account(&rig, "claude", "current", "current", &config)?;
    write_session(&config, "chair-session", "m1", 45)?;
    let input = serde_json::to_vec(&json!({
        "hook_event_name":"Stop", "session_id":"chair-session", "cwd":rig.project
    }))?;
    let invoke = || {
        rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "--state-dir",
                rig.state.to_str().ok_or("state")?,
                "hook",
                "claude",
                "chair-stop",
            ],
            Some(&input),
        )
    };
    assert_eq!(code(&invoke()?), 0);
    use std::io::Write;
    let transcript = config.join("projects/-fixture/chair-session.jsonl");
    fs::OpenOptions::new()
        .append(true)
        .open(transcript)?
        .write_all(message("m2", 60).as_bytes())?;
    let subagents = config.join("projects/-fixture/chair-session/subagents");
    fs::create_dir_all(&subagents)?;
    fs::write(subagents.join("agent-a.jsonl"), message("m3", 20))?;
    assert_eq!(code(&invoke()?), 0);
    let rows: Vec<Value> = stored(&rig)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .any(|row| row["message"] == "m2" && row["output"] == 60)
    );
    assert!(
        rows.iter()
            .any(|row| row["message"] == "m3" && row["source"] == "subagent")
    );
    Ok(())
}

fn rig() -> Result<TestRig> {
    let rig = TestRig::new()?;
    std::fs::create_dir_all(rig.project.join("config"))?;
    std::fs::write(
        rig.project.join("config/telemetry-prices.toml"),
        include_str!("../../../config/telemetry-prices.toml"),
    )?;
    Ok(rig)
}

fn assert_graph_quality(rig: &TestRig, graph: &str) -> Result {
    let output = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"]["totals"]["attempts"], 1, "{report}");
    assert_eq!(report["data"]["graph_quality"][0]["graph_id"], graph);
    assert_eq!(report["data"]["graph_quality"][0]["checkpoint"], "read");
    Ok(())
}

#[test]
fn submitted_graph_reports_its_checkpoint_quality() -> Result {
    let rig = rig()?;
    let document = rig.project.join("graph.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&json!({
            "schema_version": 1, "graph_id":"attributed-graph",
            "context": {"pattern":"graph-default", "preset":"example"},
            "nodes": [{
                "id":"inspect", "prompt":"Review the bounded slice", "access":"read_only",
                "context": {"pattern":"council"},
                "route":{"adapter":"fixture", "model":"fixture/model"}
            }]
        }))?,
    )?;
    let output = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("graph path")?,
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        value["data"]["checkpoint"]["graph_id"], "attributed-graph",
        "{value}"
    );
    assert_graph_quality(&rig, "attributed-graph")?;
    Ok(())
}

#[test]
fn telemetry_stats_reports_context_coverage_reasoning_and_acceptance_denominator() -> Result {
    let rig = rig()?;
    terminal_run::seed(&rig, "context-report", &[Value::Null])?;
    let config = rig.root.join("claude-context");
    register_account(&rig, "claude", "context", "context", &config)?;
    write_session(
        &config,
        "fixture-context-session",
        "fixture-context-message",
        13,
    )?;
    let ingest = rig.command(&["telemetry", "ingest", "--json"])?;
    assert_eq!(code(&ingest), 0, "{ingest:?}");
    let output = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let data = &value["data"];
    assert_eq!(
        data["attempt_context"]["coverage"]["system_prompt"]["unknown"],
        1
    );
    assert_eq!(
        data["reply_context"]["reasoning_unit"],
        "provider-reported reasoning tokens"
    );
    let unknown = &data["reply_context"]["thinking_per_trigger"]["unknown"];
    assert_eq!(unknown["reasoning_unknown"], 1);
    assert_eq!(unknown["reasoning_tokens"], 0);
    assert_eq!(unknown["output_tokens_proxy"], 13);
    // Spend by context band: the one reply sent 5 + 7 + 11 = 23 tokens.
    let bands = &data["context_bands"]["bands"];
    assert_eq!(bands.as_array().map(Vec::len), Some(1), "{bands}");
    assert_eq!(bands[0]["source"], "lane");
    assert_eq!(bands[0]["min_tokens"], 0);
    assert_eq!(bands[0]["below_tokens"], 50_000);
    assert_eq!(bands[0]["totals"]["attempts"], 1);
    assert_eq!(bands[0]["totals"]["usage"]["cache_read"]["known"], 7);
    let accepted = &data["task_context"]["accepted_outcomes"];
    assert_eq!(accepted["denominator"], 0);
    assert!(
        accepted["definition"]
            .as_str()
            .is_some_and(|text| text.contains("recorded accepted workflow receipt"))
    );
    assert!(data["cli_ergonomics"]["counts"].is_object());
    let store = stored(&rig)?;
    assert!(!store.contains("PRIVATE PROMPT"));
    Ok(())
}

#[test]
fn native_attempt_is_linked_in_usage() -> Result {
    let rig = rig()?;
    let config = rig.root.join("claude");
    register_account(&rig, "claude", "current", "current", &config)?;
    write_session(&config, "chair-session", "chair", 1)?;
    let subagents = config.join("projects/-fixture/chair-session/subagents");
    fs::create_dir_all(&subagents)?;
    fs::write(subagents.join("agent-a.jsonl"), message("linked", 45))?;
    fs::write(subagents.join("agent-b.jsonl"), message("unlinked", 30))?;
    let start = rig.command(&[
        "native",
        "start",
        "--id",
        "tool-1",
        "--session",
        "chair-session",
        "--alias",
        "reviewer",
        "--model",
        "claude-opus-5-5",
        "--effort",
        "high",
        "--json",
    ])?;
    assert_eq!(code(&start), 0, "{start:?}");
    let end = rig.command(&[
        "native",
        "end",
        "--id",
        "tool-1",
        "--session",
        "chair-session",
        "--agent",
        "a",
        "--settlement",
        "succeeded",
        "--json",
    ])?;
    assert_eq!(code(&end), 0, "{end:?}");
    let stats = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(code(&stats), 0, "{stats:?}");
    let stats: Value = serde_json::from_slice(&stats.stdout.bytes)?;
    let native = &stats["data"]["native_subagents"];
    assert_eq!(native["attempts_omitted"], 1, "{native}");
    assert_eq!(native["unlinked"][0]["buckets"][4], 30, "{native}");
    assert!(!fs::read_to_string(rig.state.join("native-attempts.json"))?.contains("PRIVATE"));
    Ok(())
}

#[test]
fn stats_json_counts_per_attempt_rows_instead_of_carrying_them() -> Result {
    let rig = rig()?;
    // The size limit itself is tested at the renderer with a low injected bound
    // (`unpublished_result_tests` in src/lib.rs); settling attempts is slow, so
    // this keeps a few.
    let attempts = 6usize;
    let contexts: Vec<Value> = (0..attempts)
        .map(|index| {
            json!({"graph_id": "bounded-stats-graph",
                "node_id": format!("node-{index}-{}", "n".repeat(100))})
        })
        .collect();
    terminal_run::seed(&rig, "bounded-stats", &contexts)?;
    // The default is the summary the human view shows: counts, not rows.
    let output = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        value["data"]["attempt_context"]["attempts_omitted"],
        attempts
    );
    assert!(value["data"]["attempt_context"].get("attempts").is_none());
    assert!(value["data"].get("artifact").is_none_or(Value::is_null));
    let left = fs::read_dir(rig.state.join("tmp/results")).map_or(0, Iterator::count);
    assert_eq!(left, 0);
    Ok(())
}
