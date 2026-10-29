mod support;
use serde_json::Value;
use std::fs;
use support::{Result, TestRig, code};

#[test]
fn native_stats_runs_without_a_project_or_store_and_sanitises_output() -> Result {
    let rig = TestRig::new()?;
    fs::create_dir_all(rig.project.join("config"))?;
    fs::write(
        rig.project.join("config/telemetry-prices.toml"),
        include_str!("../../../config/telemetry-prices.toml"),
    )?;
    let directory = rig.home.join(".claude/projects/project/session/subagents");
    fs::create_dir_all(&directory)?;
    fs::write(
        directory.join("agent-one.jsonl"),
        concat!(
            "{\"type\":\"assistant\",\"message\":{\"id\":\"one\",\"model\":\"evil\\nforged\",\"usage\":{\"input_tokens\":1}}}\n",
            "{\"type\":\"assistant\""
        ),
    )?;
    let path = directory.to_str().ok_or("path")?;
    let output = rig.command(&["telemetry", "stats", "--claude-subagents", path])?;
    assert_eq!(
        code(&output),
        0,
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let human = String::from_utf8(output.stdout.bytes)?;
    assert!(!human.contains("\nforged"));
    assert!(human.contains("1 incomplete"));
    assert!(!human.contains("0 attempts"));
    let output = rig.command(&["telemetry", "stats", "--claude-subagents", path, "--json"])?;
    assert_eq!(code(&output), 0);
    let json: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        json["data"]["native_claude_subagents"]["skipped_incomplete_lines"],
        1
    );
    assert!(json["data"].get("totals").is_none());
    let output = rig.command(&["telemetry", "stats", "--claude-subagents", "/missing"])?;
    assert_eq!(code(&output), 2);
    Ok(())
}

#[test]
fn prices_refresh_proposes_a_deterministic_diff_and_never_edits_the_sheet() -> Result {
    let rig = TestRig::new()?;
    fs::create_dir_all(rig.project.join("config"))?;
    let sheet = include_str!("../../../config/telemetry-prices.toml");
    fs::write(rig.project.join("config/telemetry-prices.toml"), sheet)?;
    // Haiku 4.5 at $1/$5 per million with a 25% cache-read change; LiteLLM disagrees on output.
    fs::write(
        rig.project.join("openrouter.json"),
        r#"{"data":[{"id":"anthropic/claude-haiku-4.5","pricing":{"prompt":"0.000001","completion":"0.000005","input_cache_read":"0.000000125","input_cache_write":"0.00000125"}}]}"#,
    )?;
    fs::write(
        rig.project.join("litellm.json"),
        r#"{"sample_spec":{},"claude-haiku-4-5":{"input_cost_per_token":1e-06,"output_cost_per_token":8e-06,"cache_read_input_token_cost":1.25e-07,"cache_creation_input_token_cost":1.25e-06}}"#,
    )?;
    let run = || {
        rig.command(&[
            "telemetry",
            "prices",
            "refresh",
            "--json",
            "--openrouter-file",
            "openrouter.json",
            "--litellm-file",
            "litellm.json",
            "--out",
            "proposal.json",
        ])
    };
    let output = run()?;
    assert_eq!(
        code(&output),
        5,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let json: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let proposal = &json["data"]["proposal"];
    assert_eq!(proposal["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(proposal["changes"][0]["category"], "cache_read");
    assert_eq!(proposal["changes"][0]["current"], 100_000);
    assert_eq!(proposal["changes"][0]["proposed"], 125_000);
    assert_eq!(proposal["discrepancies"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        proposal["discrepancies"][0]["category"],
        "output_excluding_reasoning"
    );
    assert_eq!(proposal["discrepancies"][0]["difference_bps"], 3750);
    assert_eq!(json["data"]["applied"], false);
    assert!(rig.project.join("proposal.json").exists());
    assert_eq!(
        fs::read_to_string(rig.project.join("config/telemetry-prices.toml"))?,
        sheet
    );
    let again: Value = serde_json::from_slice(&run()?.stdout.bytes)?;
    assert_eq!(again["data"]["proposal"], *proposal);
    // A sheet that already matches both sources is clean (exit 0).
    fs::write(
        rig.project.join("openrouter.json"),
        r#"{"data":[{"id":"anthropic/claude-haiku-4.5","pricing":{"prompt":"0.000001","completion":"0.000005","input_cache_read":"0.0000001","input_cache_write":"0.00000125"}}]}"#,
    )?;
    fs::write(
        rig.project.join("litellm.json"),
        r#"{"claude-haiku-4-5":{"input_cost_per_token":1e-06,"output_cost_per_token":5e-06}}"#,
    )?;
    assert_eq!(code(&run()?), 0);
    Ok(())
}
