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
    fs::write(directory.join("notes.jsonl"), "not a subagent transcript\n")?;
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
    #[cfg(unix)]
    {
        let non_regular = directory.join("agent-x.jsonl");
        std::os::unix::fs::symlink("agent-one.jsonl", &non_regular)?;
        let output = rig.command(&["telemetry", "stats", "--claude-subagents", path])?;
        assert_eq!(code(&output), 2, "symlink transcript must be refused");
        fs::remove_file(&non_regular)?;
        fs::create_dir(&non_regular)?;
        let output = rig.command(&["telemetry", "stats", "--claude-subagents", path])?;
        assert_eq!(code(&output), 2, "directory transcript must be refused");
    }
    let output = rig.command(&["telemetry", "stats", "--claude-subagents", "/missing"])?;
    assert_eq!(code(&output), 2);
    Ok(())
}
