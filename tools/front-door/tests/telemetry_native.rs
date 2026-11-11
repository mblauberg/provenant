mod support;
use serde_json::Value;
use std::fs;
use support::{Result, TestRig, code, register_account};

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
        rig.project.join("config/telemetry-prices.toml"),
        r#"schema_version = 1
id = "t"
reviewed_at = "2026-10-27"
units = "usd_microdollars_per_million_tokens"
[[models]]
provider = "claude"
model = "claude/claude-haiku-4-5"
billing = "api"
sources = ["s"]
openrouter_id = "anthropic/claude-haiku-4.5"
litellm_id = "claude-haiku-4-5"
[models.rates]
uncached_input = 1000000
output_excluding_reasoning = 5000000
"#,
    )?;
    fs::write(
        rig.project.join("openrouter.json"),
        r#"{"data":[{"id":"anthropic/claude-haiku-4.5","pricing":{"prompt":"0.000001","completion":"0.000005"}}]}"#,
    )?;
    fs::write(
        rig.project.join("litellm.json"),
        r#"{"claude-haiku-4-5":{"input_cost_per_token":1e-06,"output_cost_per_token":5e-06}}"#,
    )?;
    assert_eq!(code(&run()?), 0);
    Ok(())
}

fn refresh_rig() -> std::result::Result<(TestRig, String), Box<dyn std::error::Error>> {
    let rig = TestRig::new()?;
    fs::create_dir_all(rig.project.join("config"))?;
    let sheet = include_str!("../../../config/telemetry-prices.toml").to_owned();
    fs::write(rig.project.join("config/telemetry-prices.toml"), &sheet)?;
    Ok((rig, sheet))
}

#[test]
fn prices_refresh_never_overwrites_the_sheet_even_through_an_alias() -> Result {
    let (rig, sheet) = refresh_rig()?;
    fs::write(
        rig.project.join("o.json"),
        r#"{"data":[{"id":"x","pricing":{}}]}"#,
    )?;
    fs::write(
        rig.project.join("l.json"),
        r#"{"x":{"input_cost_per_token":1e-06}}"#,
    )?;
    std::os::unix::fs::symlink(
        "config/telemetry-prices.toml",
        rig.project.join("alias.toml"),
    )?;
    for out in [
        "config/telemetry-prices.toml",
        "config/../config/telemetry-prices.toml",
        "alias.toml",
    ] {
        let output = rig.command(&[
            "telemetry",
            "prices",
            "refresh",
            "--json",
            "--openrouter-file",
            "o.json",
            "--litellm-file",
            "l.json",
            "--out",
            out,
        ])?;
        assert_eq!(
            code(&output),
            2,
            "{out}: {}",
            String::from_utf8_lossy(&output.stdout.bytes)
        );
        assert_eq!(
            fs::read_to_string(rig.project.join("config/telemetry-prices.toml"))?,
            sheet
        );
    }
    Ok(())
}

#[test]
fn prices_refresh_flags_missing_models_and_rejects_unrelated_documents() -> Result {
    let (rig, _) = refresh_rig()?;
    let run = |openrouter: &str| -> std::result::Result<i64, Box<dyn std::error::Error>> {
        fs::write(rig.project.join("o.json"), openrouter)?;
        fs::write(
            rig.project.join("l.json"),
            r#"{"nothing":{"input_cost_per_token":1e-06}}"#,
        )?;
        Ok(code(&rig.command(&[
            "telemetry",
            "prices",
            "refresh",
            "--json",
            "--openrouter-file",
            "o.json",
            "--litellm-file",
            "l.json",
            "--out",
            "p.json",
        ])?))
    };
    // A valid document that lacks every configured model is not clean.
    assert_eq!(
        run(r#"{"data":[{"id":"unrelated/model","pricing":{"prompt":"0.000001"}}]}"#)?,
        5
    );
    // An unrelated JSON file is invalid input, not a clean refresh.
    assert_eq!(run(r#"{"name":"icon"}"#)?, 2);
    Ok(())
}

#[test]
fn stats_surfaces_a_stale_price_sheet() -> Result {
    let (rig, sheet) = refresh_rig()?;
    fs::write(
        rig.project.join("config/telemetry-prices.toml"),
        sheet.replace(
            "reviewed_at = \"2026-10-27\"",
            "reviewed_at = \"2020-01-01\"",
        ),
    )?;
    let output = rig.command(&["telemetry", "stats"])?;
    assert_eq!(code(&output), 0);
    assert!(String::from_utf8(output.stdout.bytes)?.contains("price sheet reviewed"));
    let output = rig.command(&["telemetry", "stats", "--json"])?;
    let json: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(json["data"]["price_freshness"]["stale"], true);
    assert_eq!(json["data"]["price_freshness"]["reviewed_at"], "2020-01-01");
    Ok(())
}

fn message(id: &str, output: u64) -> String {
    use provenant_core::Clock;
    let timestamp = provenant_os::clock::SystemClock::new()
        .wall_now()
        .map(|t| t.as_timestamp().to_string())
        .unwrap_or_default();
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{timestamp}\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-opus-5-5\",\"content\":\"PRIVATE PROMPT\",\"usage\":{{\"input_tokens\":5,\"cache_read_input_tokens\":7,\"cache_creation_input_tokens\":11,\"output_tokens\":{output}}}}}}}\n"
    )
}

#[test]
fn stats_default_output_omits_attempt_rows_and_leaves_other_sections_unchanged() -> Result {
    let (rig, _) = refresh_rig()?;
    let config = rig.root.join("claude");
    register_account(&rig, "claude", "current", "current", &config)?;
    let directory = config.join("projects/-fixture");
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("chair-session.jsonl"), message("chair", 1))?;
    let subagents = directory.join("chair-session/subagents");
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

    let default_output = rig.command(&["telemetry", "stats"])?;
    assert_eq!(code(&default_output), 0);
    let default_text = String::from_utf8(default_output.stdout.bytes)?;

    let detail_output = rig.command(&["telemetry", "stats", "--detail"])?;
    assert_eq!(code(&detail_output), 0);
    let detail_text = String::from_utf8(detail_output.stdout.bytes)?;

    let header = "native sub-agent usage (host; may overlap chair usage):\n";
    assert!(default_text.contains(header));
    assert!(detail_text.contains(header));

    // The other sections preceding the native section are identical.
    let default_prefix = default_text.split(header).next().unwrap();
    let detail_prefix = detail_text.split(header).next().unwrap();
    assert_eq!(default_prefix, detail_prefix);

    // Default output omits per-attempt rows; `--detail` includes them.
    assert!(!default_text.contains("reviewer claude-opus-5-5:"));
    assert!(detail_text.contains("reviewer claude-opus-5-5: input=23 output=45"));
    assert_ne!(default_text, detail_text);

    // Both retain unlinked sub-agent usage.
    assert!(default_text.contains("unlinked claude-opus-5-5: 1 messages, input=23 output=30"));
    assert!(detail_text.contains("unlinked claude-opus-5-5: 1 messages, input=23 output=30"));

    Ok(())
}
