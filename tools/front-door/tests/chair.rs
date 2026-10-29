// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]

mod support;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{SystemTime, UNIX_EPOCH},
};
use support::{Result, TestRig, code, register_account};

fn configured() -> Result<TestRig> {
    let rig = TestRig::new()?;
    let spare = rig.root.join("spare's account");
    fs::create_dir(&spare)?;
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[knobs]\n\"chair.auto_switch\" = true\n\"chair.switch_accounts\" = ['spare']\n",
    )?;
    register_account(
        &rig,
        "claude",
        "current",
        "current",
        &rig.root.join("claude"),
    )?;
    register_account(&rig, "claude", "spare", "spare", &spare)?;
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

/// Write an executable stub under the spawn lock so no concurrent fork in this
/// test binary can inherit the open write descriptor and make the exec fail.
fn write_script(rig: &TestRig, path: &std::path::Path, text: &str) -> Result {
    rig.with_process_spawn_lock(|| {
        fs::write(path, text)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        Ok(())
    })
}

fn register_pair(rig: &TestRig) -> Result {
    register_account(
        rig,
        "claude",
        "current",
        "current",
        &rig.root.join("claude"),
    )?;
    register_account(
        rig,
        "claude",
        "spare",
        "spare",
        &rig.root.join("spare's account"),
    )
}
#[test]
fn chair_switch_session_id_prefers_flag_then_code_then_legacy_environment() -> Result {
    let mut rig = configured()?;
    for (code_id, legacy_id, explicit_id, expected) in [
        (Some("code-session"), None, None, "code-session"),
        (
            Some("code-session"),
            Some("legacy-session"),
            None,
            "code-session",
        ),
        (None, Some("legacy-session"), None, "legacy-session"),
        (
            Some("code-session"),
            Some("legacy-session"),
            Some("explicit-session"),
            "explicit-session",
        ),
    ] {
        rig.env
            .retain(|(key, _)| key != "CLAUDE_CODE_SESSION_ID" && key != "CLAUDE_SESSION_ID");
        for (key, value) in [
            ("CLAUDE_CODE_SESSION_ID", code_id),
            ("CLAUDE_SESSION_ID", legacy_id),
        ] {
            if let Some(value) = value {
                rig.env.push((key.into(), value.into()));
            }
        }
        let mut args = vec!["chair", "switch", "--to", "spare", "--dry-run", "--json"];
        if let Some(id) = explicit_id {
            args.extend(["--session-id", id]);
        }
        let output = rig.command(&args)?;
        assert_eq!(code(&output), 0);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        let checkpoint = value["data"]["checkpoint"].as_str().ok_or("checkpoint")?;
        assert!(
            checkpoint.ends_with(&format!("/{expected}.json")),
            "{checkpoint}"
        );
    }
    assert!(!rig.state.join("chair-switch").exists());
    Ok(())
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
    write_script(
        &rig,
        &claude,
        "#!/bin/sh\nprintf '%s\\n' \"$CLAUDE_CONFIG_DIR\"\n",
    )?;
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
    assert_eq!(value["data"]["outcome"], "refused");
    Ok(())
}

#[test]
fn state_dir_registrations_drive_quota_and_chair_without_agents_config() -> Result {
    let rig = configured()?;
    let legacy = rig.home.join(".agents");
    if legacy.exists() {
        fs::remove_dir_all(&legacy)?;
    }
    let cached = ["current", "spare"]
        .map(|key| fs::read(rig.state.join("quota").join(key).join("claude.json")))
        .into_iter()
        .collect::<std::io::Result<Vec<_>>>()?;
    fs::remove_dir_all(rig.state.join("quota"))?;
    let tick = rig.command(&["quota", "tick", "--json"])?;
    assert_eq!(
        code(&tick),
        0,
        "{} {}",
        String::from_utf8_lossy(&tick.stdout.bytes),
        String::from_utf8_lossy(&tick.stderr.bytes)
    );
    let report: serde_json::Value = serde_json::from_slice(&tick.stdout.bytes)?;
    assert_eq!(report["data"]["accounts"].as_array().unwrap().len(), 2);
    for (key, bytes) in ["current", "spare"].into_iter().zip(cached) {
        fs::write(rig.state.join("quota").join(key).join("claude.json"), bytes)?;
    }
    let switched = rig.command(&["chair", "switch", "--to", "spare", "--dry-run", "--json"])?;
    assert_eq!(
        code(&switched),
        0,
        "{}",
        String::from_utf8_lossy(&switched.stdout.bytes)
    );
    let result: serde_json::Value = serde_json::from_slice(&switched.stdout.bytes)?;
    assert_eq!(result["data"]["target_account"], "spare");
    assert!(!legacy.exists());
    Ok(())
}

fn hook(rig: &TestRig, start: bool, session: &str) -> Result<provenant_os::exec::ExecOutcome> {
    rig.execute(std::path::Path::new(env!("CARGO_BIN_EXE_provenant")), &["hook", "claude", if start {"chair-start"} else {"chair-prompt"}], Some(&serde_json::to_vec(&json!({
        "hook_event_name":if start {"SessionStart"} else {"UserPromptSubmit"}, "session_id":session,"cwd":rig.project,"transcript_path":rig.root.join("transcript.jsonl"),"prompt":"PRIVATE PROMPT MUST NOT BE STORED"
    }))?))
}
#[test]
fn chair_start_records_serving_account_without_a_pending_handoff() -> Result {
    let rig = configured()?;
    assert_eq!(code(&rig.command(&["fabric", "identity", "--json"])?), 0);
    let config = rig.state.join("config.toml");
    let text = fs::read_to_string(&config)?;
    fs::write(
        config,
        text.replace("[knobs]", "[knobs]\n\"usage.project_attribution\" = true")
            .replace("label = \"current\"", "label = \"primary\""),
    )?;
    for _ in 0..2 {
        let output = hook(&rig, true, "ordinary-session")?;
        assert_eq!(code(&output), 0);
        assert!(output.stdout.bytes.is_empty());
    }
    let log = fs::read_to_string(rig.state.join("usage.jsonl"))?;
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let event = events
        .iter()
        .find(|e| e["kind"] == "chair.session")
        .ok_or("SessionStart did not record the serving account without a handoff")?;
    assert_eq!(event["unit"], "claude");
    assert_eq!(event["account"], "current");
    assert_eq!(event["session"], "ordinary-session");
    assert!(event["trace"]["project"].is_string());
    assert!(!log.contains("PRIVATE PROMPT"));
    assert!(!log.contains("transcript.jsonl"));
    let output = rig.command(&["quota", "usage", "--json"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    let rows = report["data"]["rows"].as_array().ok_or("usage rows")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["account"], "primary");
    assert_eq!(rows[0]["chair_sessions"], 1);
    assert!(
        report["data"]["chair_usage_note"]
            .as_str()
            .is_some_and(|s| s.contains("not recorded"))
    );
    Ok(())
}

#[test]
fn chair_start_obeys_project_attribution_opt_in() -> Result {
    let rig = configured()?;
    let output = hook(&rig, true, "private-project")?;
    assert_eq!(code(&output), 0);
    let log = fs::read_to_string(rig.state.join("usage.jsonl"))?;
    let event = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .find(|e| e["kind"] == "chair.session")
        .ok_or("chair event")?;
    assert_eq!(event["account"], "current");
    assert!(
        event.get("trace").is_none(),
        "project attribution is off: {event}"
    );
    let report = rig.command(&["quota", "usage", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&report.stdout.bytes)?;
    assert!(report["data"]["rows"][0]["project"].is_null());
    assert_eq!(report["data"]["rows"][0]["chair_sessions"], 1);
    Ok(())
}

#[test]
fn chair_attribution_does_not_register_unbound_projects() -> Result {
    let rig = configured()?;
    let config = rig.state.join("config.toml");
    let text = fs::read_to_string(&config)?;
    fs::write(
        config,
        text.replace("[knobs]", "[knobs]\n\"usage.project_attribution\" = true"),
    )?;
    assert!(!rig.state.join("projects").exists());
    let output = hook(&rig, true, "unbound-project")?;
    assert_eq!(code(&output), 0);
    assert!(
        !rig.state.join("projects").exists(),
        "telemetry registered the project"
    );
    let report = rig.command(&["quota", "usage", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&report.stdout.bytes)?;
    assert!(report["data"]["rows"][0]["project"].is_null());
    assert_eq!(report["data"]["rows"][0]["chair_sessions"], 1);
    Ok(())
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
    let plan = rig.command(&[
        "chair",
        "switch",
        "--session-id",
        "old",
        "--dry-run",
        "--json",
    ])?;
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout.bytes)?;
    let checkpoint = plan["data"]["checkpoint"].as_str().ok_or("checkpoint")?;
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
    assert!(reason.contains("CLAUDE_CONFIG_DIR="), "{reason}");
    // The public plan locates the checkpoint. Check privacy without pinning
    // the private record layout.
    let bytes = fs::read(checkpoint)?;
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE PROMPT"));
    let repeat = hook(&rig, false, "old")?;
    let repeated: serde_json::Value = serde_json::from_slice(&repeat.stdout.bytes)?;
    assert_eq!(repeated["continue"], true);
    assert!(repeated["systemMessage"].is_string());
    fs::write(rig.state.join("config.toml"), "schema_version = 1\n")?;
    register_pair(&rig)?;
    assert!(hook(&rig, false, "old")?.stdout.bytes.is_empty());
    // The source account cannot consume its own handoff on compaction.
    assert!(hook(&rig, true, "old")?.stdout.bytes.is_empty());
    rig.env.retain(|(k, _)| k != "CLAUDE_CONFIG_DIR");
    rig.env.push((
        "CLAUDE_CONFIG_DIR".into(),
        rig.root.join("spare's account").into_os_string(),
    ));
    // Project-attribution failures must not prevent checkpoint consumption.
    let config = rig.state.join("config.toml");
    let text = fs::read_to_string(&config)?;
    fs::write(
        config,
        format!("{text}\n[knobs]\n\"usage.project_attribution\" = true\n"),
    )?;
    rig.env.retain(|(key, _)| key != "PROVENANT_GIT");
    rig.env.push((
        "PROVENANT_GIT".into(),
        rig.root.join("missing-git").into_os_string(),
    ));
    let resumed = hook(&rig, true, "new")?;
    let value: serde_json::Value = serde_json::from_slice(&resumed.stdout.bytes)?;
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .ok_or("resume context")?;
    assert!(context.contains("old") && context.contains(checkpoint));
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
    register_pair(&rig)?;
    let refused = hook(&rig, false, "refused")?;
    let value: serde_json::Value = serde_json::from_slice(&refused.stdout.bytes).map_err(|e| {
        format!(
            "{e}; stderr: {}",
            String::from_utf8_lossy(&refused.stderr.bytes)
        )
    })?;
    assert_eq!(value["continue"], true);
    assert!(value["systemMessage"].as_str().is_some());
    assert!(hook(&rig, false, "refused")?.stdout.bytes.is_empty());
    fs::remove_file(rig.state.join("quota/current/claude.json"))?;
    assert!(hook(&rig, false, "unknown")?.stdout.bytes.is_empty());
    Ok(())
}
#[test]
fn chair_herdr_launch_uses_only_the_returned_new_pane_and_runs_once() -> Result {
    let mut rig = configured()?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    let herdr = bin.join("herdr");
    write_script(
        &rig,
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
            .is_some_and(|s| s.contains("new-pane")),
        "{value}; stderr: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
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

#[test]
fn chair_failed_auth_refresh_cannot_establish_target_headroom() -> Result {
    let rig = configured()?;
    let file = rig.state.join("quota/spare/claude.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&file)?)?;
    record["last_error"] = "auth".into();
    record["failures"] = 1.into();
    fs::write(file, serde_json::to_vec(&record)?)?;
    let refused = rig.command(&["chair", "switch", "--to", "spare", "--dry-run", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&refused.stdout.bytes)?;
    assert_eq!(value["status"], "refused");
    assert_eq!(value["data"]["outcome"], "refused");
    Ok(())
}

#[test]
fn chair_real_manual_switch_requires_a_stable_source_session() -> Result {
    let rig = configured()?;
    let refused = rig.command(&["chair", "switch", "--json"])?;
    assert_ne!(code(&refused), 0);
    let first = rig.command(&["chair", "switch", "--session-id", "source", "--json"])?;
    let first: serde_json::Value = serde_json::from_slice(&first.stdout.bytes)?;
    assert_eq!(first["data"]["outcome"], "prepared");
    let repeat = rig.command(&["chair", "switch", "--session-id", "source", "--json"])?;
    let repeat: serde_json::Value = serde_json::from_slice(&repeat.stdout.bytes)?;
    assert_eq!(repeat["data"]["outcome"], "already_prepared");
    Ok(())
}

#[test]
fn chair_snapshot_allows_slow_identity_without_initialising_a_run_store() -> Result {
    let mut rig = configured()?;
    let git = rig.root.join("git");
    let real_git = std::env::var_os("PROVENANT_GIT").unwrap_or_else(|| "/usr/bin/git".into());
    let real_git = real_git.to_str().ok_or("git path")?.replace('\'', "'\\''");
    // Identity discovery has a bounded utility budget of its own. Simulate a
    // slow first Git probe without spending the snapshot's store budget.
    write_script(
        &rig,
        &git,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.log\"\nif [ ! -e \"$0.started\" ]; then\n  : > \"$0.started\"\n  sleep 1.1\nfi\nexec '{real_git}' \"$@\"\n"
        ),
    )?;
    rig.env.retain(|(key, _)| key != "PROVENANT_GIT");
    rig.env
        .push(("PROVENANT_GIT".into(), git.clone().into_os_string()));
    // The prompt hook abandons a switch when under 3s of its 8s wall clock
    // remain, so a loaded machine could race the 1.1s delay. `chair switch`
    // reaches the same snapshot with no wall-clock deadline.
    let output = rig.command(&["chair", "switch", "--session-id", "slow-identity", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes).map_err(|e| {
        format!(
            "{e}; stderr: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        )
    })?;
    assert_eq!(value["data"]["outcome"], "prepared", "{value}");
    assert!(
        rig.root.join("git.started").exists(),
        "fake git never ran; PROVENANT_GIT={}; exit={:?}; stdout={}; stderr={}; git.log={}",
        git.display(),
        output.termination,
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes),
        fs::read_to_string(rig.root.join("git.log")).unwrap_or_else(|_| "absent".into())
    );
    assert!(!rig.state.join("run.sqlite3").exists());
    assert!(!rig.state.join("run.sqlite3.lock").exists());
    Ok(())
}

#[test]
fn chair_snapshot_preserves_active_lanes_while_a_writer_is_open() -> Result {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, RootInputs, RunId, TaskId,
        WaitBudget,
    };
    use provenant_os::clock::SystemClock;
    use provenant_records::CanonicalV1;
    use provenant_run as run;

    let rig = configured()?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?;
    let context = run::ExecutionContext {
        state_root: roots.state,
        project_id: binding.project_id(),
        actor_id: ActorId::from_bytes([1; 16]),
        read_prompts: false,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 10)?,
        cancel: &cancel,
    };
    let store = run::RunStore::open(&context, &call)?;
    let run_id = RunId::from_bytes([2; 16]);
    let task_id = TaskId::from_bytes([3; 16]);
    let attempt_id = AttemptId::from_bytes([4; 16]);
    store.create(
        &run::PreparedRun {
            run_id,
            operation_id: OperationId::parse("chair-snapshot")?,
            request: run::CanonicalRequest {
                extra: Default::default(),
                child_attempt_id_version: None,
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: vec![run::TaskRequest {
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    base_ref: None,
                    profile: Default::default(),
                    task_id,
                    prompt_digest: CanonicalV1::file_digest(b"existing lane"),
                    route: run::RequestedRoute::Named {
                        extra: Default::default(),
                        name: "utility".into(),
                        effort: None,
                    },
                    account: None,
                    fallback: vec![],
                    authority_digest: Digest::from_bytes([5; 32]),
                    writer: None,
                    deliver: None,
                    provider_timeout_seconds: 60,
                    delivery_id: None,
                    access: Default::default(),
                }],
                concurrency: 1,
                parent: None,
                lane_depth: Some(1),
                parent_attempt: None,
            },
            attempts: vec![run::NewAttempt {
                attempt_id,
                task_id,
                prompt: "existing lane".into(),
            }],
            created_at: clock.wall_now()?,
        },
        &call,
    )?;
    let output = rig.command(&["chair", "switch", "--session-id", "active-lane", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(value["data"]["outcome"], "prepared", "{value}");
    let checkpoint = value["data"]["checkpoint"].as_str().ok_or("checkpoint")?;
    let handoff: provenant_workflow::chair::Handoff =
        serde_json::from_slice(&fs::read(checkpoint)?)?;
    assert_eq!(
        handoff.open_lanes,
        [format!("{run_id}/{task_id}/{attempt_id}")]
    );
    // Keep the writer connection alive through the snapshot.
    drop(store);
    Ok(())
}

#[test]
fn chair_refuses_rotating_default_directory_as_a_target() -> Result {
    let rig = configured()?;
    let rotating = rig.home.join(".claude");
    fs::create_dir(&rotating)?;
    register_account(&rig, "claude", "spare", "spare", &rotating)?;
    let output = rig.command(&["chair", "switch", "--dry-run", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(value["data"]["outcome"], "refused");
    assert!(
        value["data"]["reason"]
            .as_str()
            .is_some_and(|s| s.contains("~/.claude"))
    );
    Ok(())
}

#[test]
fn chair_rejects_terminal_controls_in_cwd_and_checkpoint_paths() -> Result {
    for cwd in [false, true] {
        let mut rig = configured()?;
        if cwd {
            let unsafe_cwd = rig.root.join("line\nbreak");
            fs::rename(&rig.project, &unsafe_cwd)?;
            rig.project = unsafe_cwd;
        } else {
            let unsafe_state = rig.root.join("line\nbreak");
            fs::rename(&rig.state, &unsafe_state)?;
            rig.state = unsafe_state;
        }
        let output = rig.command(&["chair", "switch", "--dry-run", "--json"])?;
        assert_ne!(code(&output), 0);
        assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("control characters"));
    }
    Ok(())
}
