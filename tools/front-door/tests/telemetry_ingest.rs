// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
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
    let rows: Vec<Value> = fs::read_to_string(rig.state.join("ingest.jsonl"))?
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

fn assert_group(rig: &TestRig, by: &str, value: &str, graph: Option<&str>) -> Result {
    let output = rig.command(&["telemetry", "stats", "--by", by, "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"]["totals"]["attempts"], 1, "{report}");
    let groups = &report["data"]["context_groups"];
    assert_eq!(groups.as_array().ok_or("groups")?.len(), 1);
    assert_eq!(groups[0]["value"], value, "{report}");
    assert_eq!(groups[0]["totals"]["attempts"], 1);
    if let Some(graph) = graph {
        assert_eq!(report["data"]["graph_quality"][0]["graph_id"], graph);
        assert_eq!(report["data"]["graph_quality"][0]["checkpoint"], "read");
    }
    Ok(())
}

#[test]
fn manifest_submission_retains_optional_work_context() -> Result {
    let rig = rig()?;
    let manifest = rig.project.join("tasks.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "tasks": [{
                "prompt": "Review the bounded slice",
                "context": {"graph_id":"manifest-graph", "node_id":"review", "pattern":"council"},
                "route": {"kind":"model", "provider":"codex", "model":"gpt-6-luna"}
            }]
        }))?,
    )?;
    // An unavailable provider account settles locally and supplies an attempt fact.
    let output = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest path")?,
        "--operation-id",
        "manifest-context",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(value["data"]["run_id"].is_string(), "{value}");
    assert_group(&rig, "node", "review", None)?;
    assert_group(&rig, "pattern", "council", None)?;
    Ok(())
}

#[test]
fn submitted_graph_node_is_grouped_by_node_and_pattern() -> Result {
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
        "fabric",
        "graph",
        "run",
        "--document",
        document.to_str().ok_or("graph path")?,
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        value["data"]["checkpoint"]["graph_id"], "attributed-graph",
        "{value}"
    );
    assert_group(&rig, "node", "inspect", Some("attributed-graph"))?;
    assert_group(&rig, "pattern", "council", Some("attributed-graph"))?;
    assert_group(&rig, "preset", "example", Some("attributed-graph"))?;
    Ok(())
}

fn route_stats_golden(actual: &str) -> Result {
    let rest = actual
        .strip_prefix("scope: project ")
        .ok_or("scope prefix")?;
    let (project, rest) = rest.split_once(',').ok_or("scope project delimiter")?;
    assert!(
        !project.is_empty() && !project.contains('\n'),
        "invalid scope project"
    );
    let actual = format!("scope: project <project-id>,{rest}");
    let actual = format!("{}\n", actual.trim_end());
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/telemetry-stats-by-route.txt");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, &actual)?;
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(actual, expected, "rerun with UPDATE_GOLDEN=1 to refresh");
    Ok(())
}

#[test]
fn route_stats_human_output_matches_golden() -> Result {
    let rig = rig()?;
    let manifest = rig.project.join("tasks.json");
    fs::write(
        &manifest,
        serde_json::to_vec(&json!({
            "schema_version": 1, "tasks": [{"prompt": "fixture", "route": {
                "kind": "model", "provider": "codex", "model": "gpt-6-luna"}}]
        }))?,
    )?;
    let output = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest")?,
        "--operation-id",
        "route-golden",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(value["data"]["run_id"].is_string(), "{value}");
    // Seed a settled receipt into the locally rejected attempt, without launching a provider.
    let db = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    db.execute_batch("DROP TRIGGER immutable_terminal_update")?;
    let bytes: Vec<u8> = db.query_row("SELECT record FROM attempts", [], |row| row.get(0))?;
    let mut record: Value = serde_json::from_slice(&bytes)?;
    record["resolved_route"] =
        json!({"provider":"codex", "model":"codex/gpt-6-luna", "effort":"low"});
    record["usage"]["uncached_input"] = json!(1000);
    record["usage"]["output_excluding_reasoning"] = json!(200);
    assert_eq!(
        db.execute(
            "UPDATE attempts SET record=?1",
            [serde_json::to_vec(&record)?]
        )?,
        1
    );
    drop(db);
    let output = rig.command(&["telemetry", "stats", "--by", "route"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let human = String::from_utf8(output.stdout.bytes)?;
    assert!(human.contains("codex/gpt-6-luna@low"), "{human}");
    route_stats_golden(&human)
}
