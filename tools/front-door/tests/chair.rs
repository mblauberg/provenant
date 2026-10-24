mod support;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{SystemTime, UNIX_EPOCH},
};
use support::{Result, TestRig, code};

fn configured() -> Result<TestRig> {
    let rig = TestRig::new()?;
    let spare = rig.root.join("spare's account");
    fs::create_dir(&spare)?;
    fs::create_dir_all(rig.home.join(".agents/config"))?;
    fs::write(
        rig.home.join(".agents/config/telemetry.json"),
        serde_json::to_vec(&json!({"accounts":{"claude":{
            "current":{"identity_dir":rig.root.join("claude")}, "spare":{"identity_dir":spare}
        }}}))?,
    )?;
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[knobs]\n\"chair.auto_switch\" = true\n\"chair.switch_accounts\" = ['spare']\n",
    )?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    for (key, used) in [("current", 9200), ("spare", 100)] {
        let dir = rig.state.join("quota").join(key);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("claude.json"),
            serde_json::to_vec(&json!({
                "schema_version":1, "account":{"adapter":"claude","account_key":key,"label":key}, "revision":1,
                "snapshot":{"schema_version":1,"account":{"adapter":"claude","account_key":key,"label":key},"revision":1,"status":"available","observed_unix_seconds":now,"source_ref":"fixture","authority_ref":null,"windows":[
                    {"pool":"five_hour","window_seconds":18000,"used_basis_points":used,"resets_unix_seconds":now+3600},
                    {"pool":"seven_day","window_seconds":604800,"used_basis_points":100,"resets_unix_seconds":now+604800}
                ]}, "claim":null,"last_attempt_unix_seconds":now,"backoff_until_unix_seconds":null,"failures":0,"last_error":null,"refusals":[]
            }))?,
        )?;
    }
    Ok(rig)
}
#[test]
fn chair_dry_run_reports_registered_target_and_creates_no_checkpoint() -> Result {
    let mut rig = configured()?;
    let output = rig.command(&["chair", "switch", "--to", "spare", "--dry-run", "--json"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes).map_err(|e| {
        format!(
            "{e}; stderr: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        )
    })?;
    assert_eq!(value["data"]["target_account"], "spare");
    assert_eq!(value["data"]["dry_run"], true);
    let command = value["data"]["command"].as_str().ok_or("launch command")?;
    assert!(
        command.contains("CLAUDE_CONFIG_DIR=")
            && command.contains("spare")
            && command.contains("checkpoint")
    );
    assert!(!rig.state.join("chair-switch").exists());
    // Execute only the printed shell syntax against a local stub. Apostrophes
    // in the registered account path must survive both quoting layers.
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    let claude = bin.join("claude");
    fs::write(
        &claude,
        "#!/bin/sh\nprintf '%s\\n' \"$CLAUDE_CONFIG_DIR\"\n",
    )?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let launched = rig.execute(std::path::Path::new("/bin/sh"), &["-c", command], None)?;
    assert_eq!(code(&launched), 0);
    assert_eq!(
        String::from_utf8(launched.stdout.bytes)?.trim(),
        rig.root.join("spare's account").to_str().ok_or("path")?
    );
    let refused = rig.command(&["chair", "switch", "--to", "unlisted", "--dry-run", "--json"])?;
    assert_ne!(code(&refused), 0);
    let value: serde_json::Value = serde_json::from_slice(&refused.stdout.bytes)?;
    assert_eq!(value["status"], "refused");
    assert!(value["data"]["command"].is_null());
    Ok(())
}

fn hook(
    rig: &TestRig,
    start: bool,
    session: &str,
) -> Result<provenant_supervisor::exec::ExecOutcome> {
    rig.execute(std::path::Path::new(env!("CARGO_BIN_EXE_provenant")), &["hook", "claude", if start {"chair-start"} else {"chair-prompt"}], Some(&serde_json::to_vec(&json!({
        "hook_event_name":if start {"SessionStart"} else {"UserPromptSubmit"}, "session_id":session,"cwd":rig.project,"transcript_path":rig.root.join("transcript.jsonl"),"prompt":"collect the existing lane results"
    }))?))
}
#[test]
fn chair_prompt_checkpoints_falls_back_stops_and_resumes_once() -> Result {
    let mut rig = configured()?;
    let state_text = rig.root.join("handoff.md");
    fs::write(&state_text, "## Next step\nCollect existing lane results.")?;
    let saved = rig.command(&[
        "workflow",
        "session",
        "save",
        "old",
        "--file",
        state_text.to_str().ok_or("path")?,
    ])?;
    assert_eq!(
        code(&saved),
        0,
        "{}",
        String::from_utf8_lossy(&saved.stderr.bytes)
    );
    let output = hook(&rig, false, "old")?;
    assert_eq!(code(&output), 0);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes).map_err(|e| {
        format!(
            "{e}; stderr: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        )
    })?;
    assert_eq!(value["continue"], false);
    let reason = value["stopReason"].as_str().ok_or("stop reason")?;
    assert!(
        reason.contains("exact command") && reason.contains("CLAUDE_CONFIG_DIR="),
        "{reason}"
    );
    let checkpoint = fs::read_dir(rig.state.join("chair-switch"))?
        .next()
        .ok_or("cwd directory")??
        .path()
        .join("old.json");
    let handoff: serde_json::Value = serde_json::from_slice(&fs::read(&checkpoint)?)?;
    assert_eq!(handoff["session_id"], "old");
    assert_eq!(handoff["target_account"], "spare");
    assert_eq!(handoff["next_step"], "collect the existing lane results");
    assert!(
        handoff["state_handoff_file"]
            .as_str()
            .is_some_and(|p| std::path::Path::new(p).is_file())
    );
    assert_eq!(
        handoff["transcript_path"].as_str(),
        rig.root.join("transcript.jsonl").to_str()
    );
    assert!(handoff["open_lanes"].is_array());
    let repeat = hook(&rig, false, "old")?;
    assert!(String::from_utf8_lossy(&repeat.stdout.bytes).contains("already prepared"));
    // The source account cannot consume its own handoff on compaction.
    assert!(hook(&rig, true, "old")?.stdout.bytes.is_empty());
    rig.env.retain(|(k, _)| k != "CLAUDE_CONFIG_DIR");
    rig.env.push((
        "CLAUDE_CONFIG_DIR".into(),
        rig.root.join("spare's account").into_os_string(),
    ));
    let resumed = hook(&rig, true, "new")?;
    let value: serde_json::Value = serde_json::from_slice(&resumed.stdout.bytes)?;
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .ok_or("resume context")?;
    assert!(context.contains("old") && context.contains("never re-run completed lanes"));
    assert!(hook(&rig, true, "newer")?.stdout.bytes.is_empty());
    Ok(())
}
#[test]
fn chair_disabled_and_refused_paths_never_launch() -> Result {
    let rig = configured()?;
    fs::write(rig.state.join("config.toml"), "schema_version = 1\n")?;
    assert!(hook(&rig, false, "disabled")?.stdout.bytes.is_empty());
    assert!(!rig.state.join("chair-switch").exists());
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[knobs]\n\"chair.auto_switch\" = true\n",
    )?;
    let refused = hook(&rig, false, "refused")?;
    let value: serde_json::Value = serde_json::from_slice(&refused.stdout.bytes).map_err(|e| {
        format!(
            "{e}; stderr: {}",
            String::from_utf8_lossy(&refused.stderr.bytes)
        )
    })?;
    assert!(value.get("continue").is_none());
    assert!(
        value["systemMessage"]
            .as_str()
            .is_some_and(|s| s.contains("no allowed"))
    );
    let dir = fs::read_dir(rig.state.join("chair-switch"))?
        .next()
        .ok_or("cwd directory")??
        .path();
    let logged: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("refused.json"))?)?;
    assert!(logged["target_account"].is_null());
    assert!(
        logged["reason"]
            .as_str()
            .is_some_and(|s| s.contains("no allowed"))
    );
    assert!(!dir.join("pending.json").exists());
    Ok(())
}
#[test]
fn chair_herdr_launch_uses_only_the_returned_new_pane_and_runs_once() -> Result {
    let mut rig = configured()?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    let herdr = bin.join("herdr");
    fs::write(
        &herdr,
        r#"#!/bin/sh
printf '%s\n' "$@" >> "$HERDR_CALL_LOG"
case "$2" in
split) printf '%s\n' '{"result":{"pane":{"pane_id":"new-pane"}}}' ;;
run) printf '%s\n' '{"result":{}}' ;;
*) exit 1 ;;
esac
"#,
    )?;
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700))?;
    let log = rig.root.join("herdr.log");
    rig.env.retain(|(k, _)| k != "PATH");
    rig.env.extend([
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ),
        ("HERDR_ENV".into(), "1".into()),
        ("HERDR_PANE_ID".into(), "source-pane".into()),
        ("HERDR_CALL_LOG".into(), log.clone().into_os_string()),
    ]);
    let output = hook(&rig, false, "herdr-session")?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes).map_err(|e| {
        format!(
            "{e}; stderr: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        )
    })?;
    assert_eq!(value["continue"], false);
    assert!(
        value["stopReason"]
            .as_str()
            .is_some_and(|s| s.contains("Opened pane new-pane"))
    );
    let calls = fs::read_to_string(&log)?;
    assert!(
        calls.contains("--current\n") && calls.contains("run\nnew-pane\n"),
        "{calls}"
    );
    hook(&rig, false, "herdr-session")?;
    assert_eq!(fs::read_to_string(&log)?, calls);
    Ok(())
}
