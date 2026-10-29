mod support;
use support::{Result, TestRig, code};

#[test]
fn terminal_status_sees_lanes_submitted_under_harness_markers() -> Result {
    let mut rig = TestRig::new()?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    for (marker, value) in [("CLAUDECODE", "1"), ("CODEX_THREAD_ID", "chair-fixture")] {
        rig.env.push((marker.into(), value.into()));
        let out = rig.execute(
            std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "run",
                "--prompt-stdin",
                "--model",
                "codex/id",
                "--wait",
                "1s",
                "--json",
            ],
            Some(b"fixture"),
        )?;
        assert_eq!(code(&out), 1);
        let submitted: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        let attempt = &submitted["data"]["lanes"][0]["attempt_id"];
        assert!(attempt.is_string(), "{submitted}");
        rig.env.retain(|(key, _)| key != marker);
        let out = rig.command(&["status", "--json"])?;
        assert_eq!(code(&out), 0);
        let status: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(status["data"]["lanes"][0]["attempt_id"], *attempt);
        rig.env.push((marker.into(), value.into()));
        let out = rig.command(&["fabric", "identity", "--json"])?;
        assert_eq!(code(&out), 0);
        let identity: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(identity["data"]["client"], "agent");
        assert_eq!(identity["data"]["inbox"]["agent"], "agent");
        rig.env.retain(|(key, _)| key != marker);
    }
    Ok(())
}

#[test]
fn core_run_reports_generated_operation_and_status_is_newest_first() -> Result {
    let mut rig = TestRig::new()?;
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_provenant"));
    let mut attempts = Vec::new();
    for _ in 0..25 {
        let out = rig.execute(
            binary,
            &[
                "run",
                "--prompt-stdin",
                "--model",
                "codex/id",
                "--wait",
                "1s",
                "--json",
            ],
            Some(b"fixture"),
        )?;
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(code(&out), 1, "{value}");
        assert_eq!(value["command"], "run");
        assert!(
            value["data"]["operation_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("cli-"))
        );
        attempts.push(
            value["data"]["lanes"][0]["attempt_id"]
                .as_str()
                .ok_or("attempt")?
                .to_owned(),
        );
    }
    let out = rig.command(&["status", "--json"])?;
    assert_eq!(code(&out), 0);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let lanes = value["data"]["lanes"].as_array().ok_or("lanes")?;
    assert_eq!(lanes.len(), 20);
    assert_eq!(lanes[0]["attempt_id"], attempts[24]);
    assert_eq!(lanes[19]["attempt_id"], attempts[5]);
    assert!(lanes[0]["state"].is_object(), "full record: {value}");
    let human = rig.command(&["status"])?;
    let text = String::from_utf8(human.stdout.bytes)?;
    assert_eq!(text.lines().count(), 20, "{text}");
    assert!(text.starts_with(&attempts[24][..8]), "{text}");
    let all = rig.command(&["status", "--all", "--json"])?;
    let all: serde_json::Value = serde_json::from_slice(&all.stdout.bytes)?;
    assert_eq!(all["data"]["lanes"].as_array().ok_or("all")?.len(), 25);
    let page = rig.command(&["status", "--all", "--json", "--limit", "1"])?;
    let page: serde_json::Value = serde_json::from_slice(&page.stdout.bytes)?;
    assert_eq!(page["data"]["lanes"].as_array().ok_or("page")?.len(), 1);
    let cursor = page["data"]["next_cursor"]
        .as_u64()
        .ok_or("next cursor")?
        .to_string();
    let next = rig.command(&[
        "status", "--all", "--json", "--limit", "1", "--cursor", &cursor,
    ])?;
    let next: serde_json::Value = serde_json::from_slice(&next.stdout.bytes)?;
    assert_eq!(next["data"]["lanes"][0]["attempt_id"], attempts[23]);
    std::fs::create_dir_all(rig.project.join("config"))?;
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/telemetry-prices.toml"
        ),
        rig.project.join("config/telemetry-prices.toml"),
    )?;
    let stats = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(code(&stats), 0);
    let stats: serde_json::Value = serde_json::from_slice(&stats.stdout.bytes)?;
    assert!(stats["data"]["totals"]["cost"]["complete"].is_null());
    assert!(stats["data"]["totals"]["cost"]["known_lower_bound"].is_null());
    assert_eq!(stats["data"]["totals"]["cost"]["priced_attempts"], 0);
    assert_eq!(stats["data"]["totals"]["cost"]["missing_attempts"], 25);
    let stats = rig.command(&["telemetry", "stats"])?;
    assert_eq!(code(&stats), 0);
    assert_eq!(
        String::from_utf8(stats.stdout.bytes)?,
        "25 attempts, cost unknown (25 attempts missing cost data)\ncache-read share: unknown (incomplete input categories)\n"
    );
    // Named status includes old terminal lanes by run, task or attempt ID.
    let oldest = &all["data"]["lanes"][24];
    for key in ["run_id", "attempt_id"] {
        let id = oldest[key].as_str().ok_or("target ID")?;
        for target in [id, &id[..8]] {
            let selected = rig.command(&["status", target, "--json"])?;
            assert_eq!(code(&selected), 0);
            let selected: serde_json::Value = serde_json::from_slice(&selected.stdout.bytes)?;
            assert_eq!(
                selected["data"]["lanes"]
                    .as_array()
                    .ok_or("selected")?
                    .len(),
                1
            );
            assert_eq!(selected["data"]["lanes"][0]["attempt_id"], attempts[0]);
        }
    }
    let task_target = format!(
        "{}/{}",
        oldest["run_id"].as_str().ok_or("run")?,
        oldest["task_id"].as_str().ok_or("task")?
    );
    let task_status = rig.command(&["status", &task_target, "--all", "--json"])?;
    assert_eq!(code(&task_status), 0);
    let oldest_run = oldest["run_id"].as_str().ok_or("run ID")?;
    let selected = rig.command(&["lanes", "--all", "--id", oldest_run, "--json"])?;
    assert_eq!(code(&selected), 0);
    let selected: serde_json::Value = serde_json::from_slice(&selected.stdout.bytes)?;
    assert_eq!(selected["data"]["lanes"][0]["attempt_id"], attempts[0]);
    for condition in ["any", "all"] {
        let waited = rig.command(&[
            "lanes",
            "--id",
            oldest_run,
            "--wait",
            "1s",
            "--condition",
            condition,
            "--json",
        ])?;
        assert_eq!(code(&waited), 0);
        let waited: serde_json::Value = serde_json::from_slice(&waited.stdout.bytes)?;
        assert_eq!(waited["data"]["condition_met"], true);
        assert_eq!(waited["data"]["lanes"][0]["attempt_id"], attempts[0]);
    }
    // Recovery works from another registered project without --project.
    rig.project = rig.root.join("elsewhere");
    std::fs::create_dir(&rig.project)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    assert_eq!(code(&rig.execute(&git, &["init", "-q"], None)?), 0);
    let other = rig.execute(
        binary,
        &[
            "run",
            "--prompt-stdin",
            "--model",
            "codex/id",
            "--wait",
            "1s",
            "--json",
        ],
        Some(b"other project"),
    )?;
    assert_eq!(code(&other), 1);
    let other: serde_json::Value = serde_json::from_slice(&other.stdout.bytes)?;
    let prompt = rig.command(&["output", &attempts[0][..8], "--part", "prompt", "--json"])?;
    let prompt: serde_json::Value = serde_json::from_slice(&prompt.stdout.bytes)?;
    assert_eq!(prompt["data"]["attempt_id"], attempts[0], "{prompt}");
    let waited = rig.command(&["lanes", "--id", &attempts[0][..8], "--wait", "1s", "--json"])?;
    let waited: serde_json::Value = serde_json::from_slice(&waited.stdout.bytes)?;
    assert_eq!(
        waited["data"]["lanes"][0]["attempt_id"], attempts[0],
        "{waited}"
    );
    let cancelled = rig.command(&["cancel", &attempts[0][..8], "--json"])?;
    assert_eq!(
        code(&cancelled),
        0,
        "{}",
        String::from_utf8_lossy(&cancelled.stdout.bytes)
    );
    let run = lanes[0]["run_id"].as_str().ok_or("run")?;
    let reconciled = rig.command(&["reconcile", "--id", &run[..8], "--json"])?;
    assert_eq!(
        code(&reconciled),
        0,
        "{}",
        String::from_utf8_lossy(&reconciled.stdout.bytes)
    );
    // Full IDs bypass unrelated history, both in this project and others.
    let other_lane = &other["data"]["lanes"][0];
    let checkpoint = rig
        .state
        .join("runs")
        .join(other_lane["run_id"].as_str().ok_or("other run")?)
        .join("attempts")
        .join(other_lane["attempt_id"].as_str().ok_or("other attempt")?)
        .join("process.json");
    std::fs::write(&checkpoint, b"invalid observation")?;
    rig.project = rig.root.join("project");
    let full = rig.command(&["output", &attempts[0], "--part", "prompt", "--json"])?;
    assert_eq!(
        code(&full),
        0,
        "{}",
        String::from_utf8_lossy(&full.stdout.bytes)
    );
    let local_checkpoint = rig
        .state
        .join("runs")
        .join(lanes[1]["run_id"].as_str().ok_or("local run")?)
        .join("attempts")
        .join(&attempts[23])
        .join("process.json");
    std::fs::write(&local_checkpoint, b"invalid observation")?;
    let full = rig.command(&["output", &attempts[0], "--part", "prompt", "--json"])?;
    assert_eq!(
        code(&full),
        0,
        "{}",
        String::from_utf8_lossy(&full.stdout.bytes)
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn creation_commands_refuse_colliding_branch_names() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let commit = rig.execute(
        &git,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
        None,
    )?;
    assert_eq!(code(&commit), 0);
    let branch = rig.execute(&git, &["branch", "Existing"], None)?;
    assert_eq!(code(&branch), 0);

    let explicit = rig.command(&["worktree", "create", "existing", "--base", "HEAD"])?;
    assert_ne!(code(&explicit), 0);
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&explicit.stdout.bytes),
        String::from_utf8_lossy(&explicit.stderr.bytes)
    );
    assert!(message.contains("collides"), "{message}");

    for flag in ["--worktree", "--branch"] {
        let out = rig.execute(
            std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "run",
                "--prompt-stdin",
                "--model",
                "codex/id",
                flag,
                "existing",
                "--base",
                "HEAD",
                "--wait",
                "1s",
            ],
            Some(b"fixture"),
        )?;
        assert_ne!(code(&out), 0);
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        assert!(message.contains("collides"), "{flag}: {message}");
    }
    assert!(!rig.project.join(".worktrees/existing").exists());
    Ok(())
}

#[test]
fn core_run_creates_a_new_managed_worktree_from_a_name_or_branch() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let commit = rig.execute(
        &git,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
        None,
    )?;
    assert_eq!(code(&commit), 0);
    for (flag, branch, directory) in [
        ("--worktree", "new-lane", "new-lane"),
        ("--branch", "feat/new-lane", "feat-new-lane"),
    ] {
        let path = rig.project.join(".worktrees").join(directory);
        let out = rig.execute(
            std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "run",
                "--prompt-stdin",
                "--model",
                "codex/id",
                flag,
                branch,
                "--base",
                "HEAD",
                "--wait",
                "1s",
            ],
            Some(b"fixture"),
        )?;
        assert_eq!(code(&out), 1, "{out:?}");
        let text = String::from_utf8(out.stdout.bytes)?;
        assert!(
            text.contains(&format!("worktree {} ({branch})", path.display())),
            "{text}"
        );
        assert!(path.join(".git").exists());
        let out = rig.execute(
            std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "run",
                "--prompt-stdin",
                "--model",
                "codex/id",
                "--worktree",
                path.to_str().ok_or("path")?,
                "--wait",
                "1s",
            ],
            Some(b"fixture"),
        )?;
        let text = String::from_utf8(out.stdout.bytes)?;
        assert!(
            text.contains(&format!("worktree {} ({branch})", path.display())),
            "{text}"
        );
    }
    for path in [
        std::path::PathBuf::from(".worktrees/feat/nested"),
        rig.root.join("absolute"),
    ] {
        let out = rig.execute(
            std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "run",
                "--prompt-stdin",
                "--model",
                "codex/id",
                "--worktree",
                path.to_str().ok_or("path")?,
                "--wait",
                "1s",
                "--json",
            ],
            Some(b"fixture"),
        )?;
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(code(&out), 2, "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|s| s.contains("--branch BRANCH")),
            "{value}"
        );
        assert!(!rig.project.join(&path).exists());
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn invalid_json_is_an_envelope() -> Result {
    let rig = TestRig::new()?;
    let invalid = rig.command(&["lanes", "--wait", "0", "--json"])?;
    assert_eq!(code(&invalid), 2);
    let value: serde_json::Value = serde_json::from_slice(&invalid.stdout.bytes)?;
    assert_eq!(value["status"], "invalid");
    assert!(
        !value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Usage:")
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn lanes_watch_and_session_selection_report_an_empty_store() -> Result {
    let rig = TestRig::new()?;
    // Nothing to watch settles at once, as one stable line.
    let watch = rig.command(&["lanes", "--watch", "1"])?;
    assert_eq!(code(&watch), 0);
    assert_eq!(
        String::from_utf8(watch.stdout.bytes)?,
        "end settled lanes=0\n"
    );
    let json = rig.command(&["lanes", "--watch", "--json"])?;
    assert_eq!(code(&json), 0);
    let stdout = String::from_utf8(json.stdout.bytes)?;
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let value: serde_json::Value = serde_json::from_str(stdout.trim())?;
    assert_eq!(value["data"]["lanes"], serde_json::json!([]));
    // A session's wait with no lanes is met, and its list is empty.
    let waited = rig.command(&["lanes", "--wait", "1", "--session", "chair", "--json"])?;
    assert_eq!(code(&waited), 0);
    let value: serde_json::Value = serde_json::from_slice(&waited.stdout.bytes)?;
    assert_eq!(value["data"]["condition_met"], true);
    let listed = rig.command(&["lanes", "--session", "chair", "--json"])?;
    assert_eq!(code(&listed), 0);
    // Exclusive options and bad labels are usage errors.
    for args in [
        vec!["lanes", "--watch", "1", "--wait", "1"],
        vec!["lanes", "--session", "not a label"],
        vec!["lanes", "--watch", "1", "--state", "running"],
    ] {
        assert_eq!(code(&rig.command(&args)?), 2, "{args:?}");
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn run_leaves_accept_control_and_report_work_refused_per_task() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_provenant"));
    let state = rig.state.to_string_lossy().into_owned();
    let args = [
        "--state-dir",
        &state,
        "dispatch",
        "--prompt-stdin",
        "--model",
        "codex/gpt-6-luna",
        "--operation-id",
        "fixture",
        "--json",
    ];
    let out = rig.execute(binary, &args, Some(b"hello"))?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let lane = &value["data"]["lanes"][0];
    assert_eq!(lane["state"], "rejected");
    assert_eq!(lane["account"], "unknown");
    let manifest = rig.root.join("accounts.json");
    std::fs::write(&manifest, br#"{"schema_version":1,"tasks":[{"prompt":"fixture","route":{"kind":"model","provider":"codex","model":"id"},"account":"fixture-account"}]}"#)?;
    let placed = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest path")?,
        "--operation-id",
        "account-override",
        "--json",
    ])?;
    assert_eq!(code(&placed), 0);
    let placed: serde_json::Value = serde_json::from_slice(&placed.stdout.bytes)?;
    let run = placed["data"]["run_id"].as_str().ok_or("run")?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(
        rig.state.join("runs").join(run).join("request.json"),
    )?)?;
    assert_eq!(saved["tasks"][0]["account"], "fixture-account");
    // --account pins the same way a manifest task does, and an unknown one is
    // rejected at launch with the manifest path's outcome.
    let pinned = rig.execute(
        binary,
        &[
            "--state-dir",
            &state,
            "dispatch",
            "--prompt-stdin",
            "--model",
            "codex/id",
            "--account",
            "fixture-account",
            "--operation-id",
            "account-flag",
            "--json",
        ],
        Some(b"hello"),
    )?;
    assert_eq!(code(&pinned), 0);
    let pinned: serde_json::Value = serde_json::from_slice(&pinned.stdout.bytes)?;
    let run = pinned["data"]["run_id"].as_str().ok_or("run")?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(
        rig.state.join("runs").join(run).join("request.json"),
    )?)?;
    assert_eq!(saved["tasks"][0]["account"], "fixture-account");
    assert_eq!(pinned["data"]["lanes"][0]["state"], "rejected", "{pinned}");
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: serde_json::Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let reason = |run: &str| {
        lanes["data"]["lanes"]
            .as_array()
            .and_then(|all| all.iter().find(|lane| lane["run_id"] == run))
            .and_then(|lane| lane["reason"].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    assert!(
        reason(run).contains("account fixture-account is not registered"),
        "{lanes}"
    );
    // The same pin reaches a writer lane beside the access flags.
    let project = rig.project.to_str().ok_or("project")?;
    let commit = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        &[
            "-C",
            project,
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "fixture",
        ],
        None,
    )?;
    assert_eq!(
        code(&commit),
        0,
        "{}",
        String::from_utf8_lossy(&commit.stderr.bytes)
    );
    let created = rig.command(&["worktree", "create", "fixture", "--json"])?;
    assert_eq!(
        code(&created),
        0,
        "{}",
        String::from_utf8_lossy(&created.stdout.bytes)
    );
    let worktree = rig.project.join(".worktrees/fixture");
    let writer = rig.execute(
        binary,
        &[
            "--state-dir",
            &state,
            "dispatch",
            "--prompt-stdin",
            "--model",
            "codex/id",
            "--mode",
            "worktree-write",
            "--worktree",
            worktree.to_str().ok_or("worktree")?,
            "--account",
            "fixture-account",
            "--operation-id",
            "account-writer",
            "--json",
        ],
        Some(b"hello"),
    )?;
    assert_eq!(
        code(&writer),
        0,
        "{}",
        String::from_utf8_lossy(&writer.stdout.bytes)
    );
    let writer: serde_json::Value = serde_json::from_slice(&writer.stdout.bytes)?;
    let run = writer["data"]["run_id"].as_str().ok_or("run")?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(
        rig.state.join("runs").join(run).join("request.json"),
    )?)?;
    assert_eq!(saved["tasks"][0]["account"], "fixture-account");
    assert!(!saved["tasks"][0]["writer"].is_null(), "{saved}");
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: serde_json::Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let writer_reason = lanes["data"]["lanes"]
        .as_array()
        .and_then(|all| all.iter().find(|lane| lane["run_id"] == run))
        .and_then(|lane| lane["reason"].as_str())
        .unwrap_or_default();
    assert!(
        writer_reason.contains("account fixture-account is not registered"),
        "{lanes}"
    );
    // Replaying the operation returns the same run.
    let again: serde_json::Value =
        serde_json::from_slice(&rig.execute(binary, &args, Some(b"hello"))?.stdout.bytes)?;
    assert_eq!(again["data"]["run_id"], value["data"]["run_id"]);
    // Secret-bearing input is refused before any record and never echoed.
    let secret = format!("use ghp_{}", "a1".repeat(18));
    let mut refused = args;
    refused[7] = "secret";
    let out = rig.execute(binary, &refused, Some(secret.as_bytes()))?;
    let text = String::from_utf8_lossy(&out.stdout.bytes);
    assert!(
        text.contains("SECRET_INPUT") && !text.contains(&secret[4..]),
        "{text}"
    );
    // A worktree is never silently run read-only (#1310); a missing one says so (#1309).
    for (mode, worktree, expected) in [
        ("read-only", ".", "INVALID"),
        ("worktree-write", "missing-worktree", "WORKTREE_MISSING"),
    ] {
        let mut flags = args.to_vec();
        flags.extend(["--mode", mode, "--worktree", worktree]);
        let out = rig.execute(binary, &flags, Some(b"hello"))?;
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(value["error"]["code"], expected, "{value}");
    }
    let attempt = format!(
        "{}/{}/{}",
        lane["run_id"].as_str().ok_or("run")?,
        lane["task_id"].as_str().ok_or("task")?,
        lane["attempt_id"].as_str().ok_or("attempt")?
    );
    let prompt = rig.command(&["output", &attempt, "--part", "prompt", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&prompt.stdout.bytes)?;
    assert_eq!(value["data"]["bytes"]["data"], "hello");
    // `RUN/ATTEMPT` and `--host local` name the same local attempt, and a
    // `--tail` above the bound is clamped, not refused (#1837).
    let run_attempt = format!(
        "{}/{}",
        lane["run_id"].as_str().ok_or("run")?,
        lane["attempt_id"].as_str().ok_or("attempt")?
    );
    for words in [
        vec!["output", &run_attempt, "--part", "prompt", "--json"],
        vec![
            "output",
            lane["attempt_id"].as_str().ok_or("attempt")?,
            "--host",
            "local",
            "--part",
            "prompt",
            "--json",
        ],
    ] {
        let again = rig.command(&words)?;
        assert_eq!(code(&again), 0, "{words:?} {again:?}");
        let again: serde_json::Value = serde_json::from_slice(&again.stdout.bytes)?;
        assert_eq!(again["data"]["bytes"]["data"], "hello", "{words:?}");
    }
    let tail = rig.command(&[
        "output", &attempt, "--part", "prompt", "--tail", "9999", "--json",
    ])?;
    assert_eq!(code(&tail), 0, "{tail:?}");
    let tail: serde_json::Value = serde_json::from_slice(&tail.stdout.bytes)?;
    assert_eq!(tail["data"]["tail"], 500);
    assert_eq!(tail["data"]["tail_clamped"], true);
    assert_eq!(tail["data"]["bytes"]["data"], "hello");
    let wrong_run = format!(
        "{}/{}",
        "ff".repeat(16),
        lane["attempt_id"].as_str().ok_or("attempt")?
    );
    assert_ne!(code(&rig.command(&["output", &wrong_run, "--json"])?), 0);

    let trace = rig.command(&["output", &attempt, "--trace", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&trace.stdout.bytes)?;
    let hops = value["data"]["trace"].as_array().ok_or("trace")?;
    assert_eq!(hops.len(), 1, "{value}");
    assert_eq!(hops[0]["run"], lane["run_id"]);
    assert_eq!(hops[0]["attempt"], lane["attempt_id"]);
    assert!(hops[0]["parent"].is_null());
    let human = rig.command(&["output", &attempt, "--trace"])?;
    let line = String::from_utf8(human.stdout.bytes.clone())?;
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(line.contains(&format!(
        "attempt={}",
        lane["attempt_id"].as_str().ok_or("id")?
    )));

    let cancel = rig.command(&["cancel", &attempt, "--operation-id", "stop", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&cancel.stdout.bytes)?;
    assert_eq!(
        value["data"]["receipts"][0]["receipt"]["state"],
        "already_terminal"
    );
    // This rejected attempt launched no provider, and the fixture has no
    // provider executable for fresh fallback. Refusal must neither add an
    // attempt nor spend the continuation, including operation retries.
    let before = rig.command(&["lanes", "--all", "--json"])?;
    assert_eq!(code(&before), 0);
    let before: serde_json::Value = serde_json::from_slice(&before.stdout.bytes)?;
    let run_directories = || -> Result<std::collections::BTreeSet<std::ffi::OsString>> {
        std::fs::read_dir(rig.state.join("runs"))?
            .map(|entry| Ok(entry?.file_name()))
            .collect()
    };
    let directories_before = run_directories()?;
    let mut resume = [
        "--state-dir",
        &state,
        "resume",
        &attempt,
        "--prompt-stdin",
        "--operation-id",
        "again",
        "--json",
    ];
    for operation in ["again", "again", "again-new"] {
        resume[6] = operation;
        let out = rig.execute(binary, &resume, Some(b"more"))?;
        assert_eq!(code(&out), 1);
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(value["error"]["code"], "CONTINUATION_UNSUPPORTED");
        let after = rig.command(&["lanes", "--all", "--json"])?;
        assert_eq!(code(&after), 0);
        let after: serde_json::Value = serde_json::from_slice(&after.stdout.bytes)?;
        assert_eq!(after["data"]["lanes"], before["data"]["lanes"], "{value}");
        assert_eq!(run_directories()?, directories_before);
    }
    // An executable alone does not make an unregistered account launchable.
    // Refusing account preparation must also leave no operation or staging.
    let bin = rig.root.join("provider/bin");
    std::fs::create_dir_all(&bin)?;
    rig.copy_executable(std::path::Path::new("/usr/bin/true"), &bin.join("codex"))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    resume[3] = pinned["data"]["lanes"][0]["attempt_id"]
        .as_str()
        .ok_or("pinned attempt")?;
    for operation in ["account-resume", "account-resume", "account-resume-new"] {
        resume[6] = operation;
        let out = rig.execute(binary, &resume, Some(b"more"))?;
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(code(&out), 1, "{value}");
        assert_eq!(
            value["error"]["code"], "CONTINUATION_UNSUPPORTED",
            "{value}"
        );
        assert!(
            value["error"]["message"].as_str().is_some_and(
                |message| message.contains("account fixture-account is not registered")
            ),
            "{value}"
        );
        let after = rig.command(&["lanes", "--all", "--json"])?;
        let after: serde_json::Value = serde_json::from_slice(&after.stdout.bytes)?;
        assert_eq!(after["data"]["lanes"], before["data"]["lanes"], "{value}");
        assert_eq!(run_directories()?, directories_before);
    }
    let reconcile = rig.command(&["reconcile", "--json"])?;
    assert_eq!(code(&reconcile), 0);
    let value: serde_json::Value = serde_json::from_slice(&reconcile.stdout.bytes)?;
    assert_eq!(value["data"]["runs"], serde_json::json!([]));
    // Prices ship as a reviewed product asset beside the binary.
    std::fs::create_dir_all(rig.project.join("config"))?;
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/telemetry-prices.toml"
        ),
        rig.project.join("config/telemetry-prices.toml"),
    )?;
    let stats = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(
        code(&stats),
        0,
        "{}",
        String::from_utf8_lossy(&stats.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&stats.stdout.bytes)?;
    assert_eq!(value["data"]["totals"]["attempts"], 4);
    if cfg!(target_os = "macos") {
        // Repair the environment, then reuse the refused operation. Successful
        // admission prepares once and hands off that exact plan, including on replay.
        for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
            std::fs::create_dir_all(rig.home.join(path))?;
        }
        std::fs::write(rig.home.join(".rustup/settings.toml"), "")?;
        std::fs::write(
            rig.root.join("codex/auth.json"),
            br#"{"tokens":{"account_id":"fixture-one"}}"#,
        )?;
        std::fs::write(
            bin.join("codex"),
            r#"#!/bin/sh
case "$1" in
  --version)
    printf x >> "$HOME/version-probes"
    printf 'codex-cli 0.163.0\n'
    ;;
  exec)
    cat >/dev/null
    printf '%s\n' '{"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}' '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'
    ;;
  *) exit 1 ;;
esac
"#,
        )?;
        resume[3] = &attempt;
        resume[6] = "again";
        let mut waiting = resume.to_vec();
        waiting.extend(["--wait", "5"]);
        let out = rig.execute(binary, &waiting, Some(b"more"))?;
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(code(&out), 0, "{value}");
        let successor = &value["data"]["lanes"][0];
        assert_eq!(successor["continuation"], "fresh", "{value}");
        assert_eq!(successor["attempt_number"], 2, "{value}");
        // Write grants come from the consumed plan before OS handoff, which
        // can itself reject execution where the sandbox backend is unavailable.
        assert!(
            successor["write_grants"]
                .as_array()
                .is_some_and(|grants| !grants.is_empty()),
            "{value}"
        );
        assert_eq!(std::fs::read(rig.home.join("version-probes"))?, b"x");
        let replay = rig.execute(binary, &waiting, Some(b"more"))?;
        let replay: serde_json::Value = serde_json::from_slice(&replay.stdout.bytes)?;
        assert_eq!(
            replay["data"]["run_id"], value["data"]["run_id"],
            "{replay}"
        );
        assert_eq!(std::fs::read(rig.home.join("version-probes"))?, b"x");
        let after = rig.command(&["telemetry", "stats", "--json"])?;
        let after: serde_json::Value = serde_json::from_slice(&after.stdout.bytes)?;
        assert_eq!(after["data"]["totals"]["attempts"], 5, "{after}");
    }
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn help_and_version_need_no_roots_and_catalogue_help_drives_every_leaf() -> Result {
    let mut rig = TestRig::new()?.without_state()?;
    rig.env.retain(|(key, _)| {
        key != "HOME" && key != "PROVENANT_ROOT" && key != "PROVENANT_STATE_DIR"
    });
    let help = rig.command(&["help", "--json"])?;
    assert_eq!(code(&help), 0);
    let catalog: serde_json::Value = serde_json::from_slice(&help.stdout.bytes)?;
    for command in catalog["data"]["commands"]
        .as_array()
        .ok_or("missing catalogue")?
    {
        let name = command["command"].as_str().ok_or("missing command")?;
        let mut argv: Vec<_> = name.split_whitespace().skip(1).collect();
        argv.push("--help");
        let out = rig.command(&argv)?;
        assert_eq!(
            code(&out),
            0,
            "{name}: {}",
            String::from_utf8_lossy(&out.stdout.bytes)
        );
    }
    let version = rig.command(&["--json", "--version"])?;
    assert_eq!(code(&version), 0);
    let value: serde_json::Value = serde_json::from_slice(&version.stdout.bytes)?;
    assert!(value["data"]["source_digest"].is_string());
    let empty = rig.command(&[])?;
    assert_eq!(code(&empty), 2);
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}
#[test]
fn root_uses_release_location_and_explicit_state_without_creating_it() -> Result {
    use std::fs;
    let mut rig = TestRig::new()?;
    let release = rig.root.join("rélease");
    fs::create_dir_all(release.join("bin"))?;
    let executable = release.join("bin/provenant");
    fs::write(release.join("release.json"), "{}")?;
    rig.env
        .push(("PROVENANT_ROOT".into(), "/intentionally/missing".into()));
    // Environment keys are unique in real processes; replace the fixture value.
    rig.env
        .retain(|(key, value)| key != "PROVENANT_ROOT" || value == "/intentionally/missing");
    let state = rig.root.join("explicit-state");
    let args = [
        std::ffi::OsString::from("--state-dir"),
        state.clone().into_os_string(),
        "root".into(),
        "--json".into(),
    ];
    let out = rig.with_process_spawn_lock(|| {
        fs::copy(env!("CARGO_BIN_EXE_provenant"), &executable)?;
        rig.execute_os_locked(&executable, &args, None)
    })?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let assets: provenant_records::AbsolutePath =
        serde_json::from_value(value["data"]["assets"].clone())?;
    let selected: provenant_records::AbsolutePath =
        serde_json::from_value(value["data"]["state_directory"].clone())?;
    assert_eq!(assets.to_path_buf()?, fs::canonicalize(&release)?);
    assert_eq!(selected.to_path_buf()?, state);
    assert!(!state.exists());
    let human = rig.execute(&executable, &["root"], None)?;
    assert_eq!(code(&human), 0);
    assert_eq!(
        human.stdout.bytes,
        format!("{}\n", release.display()).as_bytes()
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn state_refuses_legacy_aliases_and_checks_select_the_caller_checkout() -> Result {
    use std::{fs, os::unix::fs::symlink};
    let rig = TestRig::new()?;
    let alias = rig.root.join("legacy-alias");
    symlink(rig.home.join(".agents"), &alias)?;
    let out = rig.execute_os(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "root".into(),
            "--state-dir".into(),
            alias.into_os_string(),
            "--json".into(),
        ],
        None,
    )?;
    assert_eq!(code(&out), 2);
    let fake = rig.project.join("fake");
    fs::create_dir_all(fake.join("scripts"))?;
    fs::write(fake.join(".git"), "not a Git worktree file")?;
    fs::write(fake.join("HARNESS.md"), "# fixture")?;
    let invalid_project = rig.execute_os(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--project".into(),
            fake.into_os_string(),
            "root".into(),
            "--json".into(),
        ],
        None,
    )?;
    assert_eq!(code(&invalid_project), 2);
    let value: serde_json::Value = serde_json::from_slice(&invalid_project.stdout.bytes)?;
    assert_eq!(value["command"], "root");
    let hook = rig.command(&["hook", "claude", "pre-tool-use", "--json"])?;
    assert_eq!(code(&hook), 2);
    let mcp = rig.command(&["fabric", "mcp", "--json"])?;
    assert_eq!(code(&mcp), 2);
    rig.assert_legacy_unchanged()
}

#[test]
fn ordinary_leaves_validate_roots_and_unbuilt_owners_refuse() -> Result {
    let rig = TestRig::new()?;
    let attempt = format!(
        "{}/{}/{}",
        "11".repeat(16),
        "22".repeat(16),
        "33".repeat(16)
    );
    let leaves: Vec<Vec<&str>> = vec![
        vec!["root"],
        vec!["config", "show"],
        vec!["fabric", "mcp"],
        vec!["install", "--source", "/fixture", "--harness", "claude"],
        vec!["uninstall", "--harness", "claude"],
        vec!["route", "list"],
        vec!["route", "resolve", "--model", "codex/id"],
        vec![
            "dispatch",
            "--prompt-stdin",
            "--model",
            "codex/id",
            "--operation-id",
            "fixture",
        ],
        vec!["lanes"],
        vec!["output", &attempt],
        vec!["cancel", &attempt, "--operation-id", "fixture"],
        vec![
            "resume",
            &attempt,
            "--prompt-stdin",
            "--operation-id",
            "fixture",
        ],
        vec!["reconcile"],
        vec!["telemetry", "stats"],
        vec!["quota", "show"],
        vec!["quota", "tick"],
        vec!["worktree", "create", "fixture"],
        vec!["worktree", "list"],
        vec!["worktree", "remove", "fixture"],
    ];
    for leaf in leaves {
        for (flag, path) in [
            ("--project", rig.root.join("missing")),
            ("--state-dir", rig.home.join(".agents")),
        ] {
            let mut args: Vec<_> = leaf.iter().map(std::ffi::OsString::from).collect();
            args.extend([flag.into(), path.into_os_string()]);
            let out = rig.execute_os(
                std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
                &args,
                None,
            )?;
            assert_eq!(code(&out), 2, "{leaf:?} {flag}");
        }
        if leaf == ["root"]
            || matches!(
                leaf.first(),
                Some(
                    &"quota"
                        | &"route"
                        | &"worktree"
                        | &"dispatch"
                        | &"lanes"
                        | &"output"
                        | &"cancel"
                        | &"resume"
                        | &"reconcile"
                        | &"telemetry"
                )
            )
        {
            continue;
        }
        let out = rig.command(&leaf)?;
        let expected = match leaf.first().copied() {
            Some("fabric") if leaf.get(1) == Some(&"mcp") => 0,
            Some("config") => 0,
            Some("install") => 1,
            Some("uninstall") => 0,
            Some("check") if leaf.get(1) == Some(&"install") => 1,
            _ => 4,
        };
        assert_eq!(
            code(&out),
            expected,
            "{leaf:?}: {}",
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        if expected == 4 || leaf == ["fabric", "mcp"] {
            assert!(out.stdout.bytes.is_empty());
        }
    }
    let internal = rig.command(&[
        "internal",
        "supervise",
        "2147483647",
        "2147483647",
        "2147483647",
        "2147483647",
        "2147483647",
    ])?;
    assert_eq!(code(&internal), 3); // Reaches supervisor's typed invalid-handshake error.
    rig.assert_legacy_unchanged()
}

#[test]
fn config_show_reads_typed_defaults_and_refuses_invalid_input_without_writes() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let defaults = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&defaults), 0);
    let value: serde_json::Value = serde_json::from_slice(&defaults.stdout.bytes)?;
    assert_eq!(value["data"]["profile"], "balanced");
    assert!(value["data"]["domains"]["quota"].is_object());
    let human = rig.command(&["config", "show"])?;
    assert_eq!(code(&human), 0);
    let text = String::from_utf8_lossy(&human.stdout.bytes);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains(" profile=balanced "), "{text}");
    assert!(text.ends_with(" schema_version=1\n"), "{text}");
    let removed = rig.command(&["config", "show", "--task-class", "mechanical", "--json"])?;
    assert_eq!(code(&removed), 2);
    assert!(!rig.state.exists());
    std::fs::create_dir_all(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\nprofile='economy'\n",
    )?;
    let selected = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&selected), 0);
    let value: serde_json::Value = serde_json::from_slice(&selected.stdout.bytes)?;
    assert_eq!(value["data"]["profile"], "economy");
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\nunknown='private-marker'\n",
    )?;
    let invalid = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&invalid), 2);
    assert!(!String::from_utf8_lossy(&invalid.stdout.bytes).contains("private-marker"));
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_uses_non_billing_status_and_preserves_identity_selection() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = TestRig::new()?.without_state()?;
    let bin = rig.root.join("status-bin");
    fs::create_dir(&bin)?;
    let claude = bin.join("claude");
    fs::write(&claude, b"#!/bin/sh\npwd -P >> \"$HOME/doctor-cwd\"\n[ \"$1 $2 $3\" = 'auth status --json' ] || exit 90\ncase \"$CLAUDE_CONFIG_DIR\" in\n  */.claude-pro) printf '{\"loggedIn\":false}'; exit 1;;\n  *) printf '{\"loggedIn\":true,\"email\":\"private-marker\"}';;\nesac\n")?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    fs::create_dir(rig.home.join(".claude-default"))?;
    let identity = rig.home.join(".claude-pro");
    fs::create_dir(&identity)?;
    // Only file metadata is needed. Doctor must not decode or return this value.
    fs::write(
        identity.join(".credentials.json"),
        b"private-credential-marker",
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push(("PATH".into(), bin.into_os_string()));
    let out = rig.command(&["hosts", "doctor", "--json"])?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let rows = value["data"]["claude"]
        .as_array()
        .ok_or("missing identities")?;
    assert!(rows.iter().any(|r| r["label"] == "default (~/.claude)"));
    assert!(rows.iter().any(|r| r["label"] == "default"));
    assert!(
        rows.iter()
            .any(|r| r["label"] == "pro" && r["signin"]["state"] == "credential_store_present")
    );
    assert!(!String::from_utf8_lossy(&out.stdout.bytes).contains("private-marker"));
    assert!(!String::from_utf8_lossy(&out.stdout.bytes).contains("private-credential-marker"));
    assert_eq!(
        fs::read_to_string(rig.home.join("doctor-cwd"))?,
        "/\n/\n/\n"
    );
    assert!(!rig.state.exists());
    assert_eq!(
        code(&rig.command(&["hosts", "doctor", "bad;host", "--json"])?),
        2
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_remote_uses_fixed_command_and_rejects_incomplete_responses() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = TestRig::new()?.without_state()?;
    let bin = rig.root.join("ssh-bin");
    fs::create_dir(&bin)?;
    let ssh = bin.join("ssh");
    fs::write(&ssh, b"#!/bin/sh\n[ \"$6\" = -- ] && [ \"$7\" = workshop ] && [ \"$8\" = 'exec ~/.local/bin/provenant hosts doctor --json' ] || exit 91\n[ \"$SSH_AUTH_SOCK\" = /fixture/agent.sock ] || exit 92\nprintf '{\"command\":\"hosts doctor\",\"status\":\"ok\",\"data\":{\"claude\":[{\"label\":\"pro\",\"signin\":{\"state\":\"keychain_unavailable\",\"token_validity\":\"unverified\"}}],\"peer_agent\":\"stopped\"}}'\n")?;
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700))?;
    rig.env
        .push(("SSH_AUTH_SOCK".into(), "/fixture/agent.sock".into()));
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push(("PATH".into(), bin.into_os_string()));
    let out = rig.command(&["hosts", "doctor", "workshop", "--json"])?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(
        value["data"]["claude"][0]["signin"]["state"],
        "keychain_unavailable"
    );
    assert_eq!(value["data"]["peer_agent"], "stopped");
    for reply in [
        &b"{\"command\":\"hosts doctor\""[..],
        b"{\"command\":\"hosts doctor\",\"status\":\"ok\",\"data\":{\"claude\":[]}}",
    ] {
        let mut script = b"#!/bin/sh\nprintf '".to_vec();
        script.extend_from_slice(reply);
        script.extend_from_slice(b"'\n");
        fs::write(&ssh, script)?;
        assert_eq!(
            code(&rig.command(&["hosts", "doctor", "workshop", "--json"])?),
            4
        );
    }
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}
