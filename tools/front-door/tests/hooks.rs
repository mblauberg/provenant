mod support;
use std::{fs, path::Path};
use support::{Result, TestRig, code};

fn recording(name: &str) -> Result<Vec<u8>> {
    Ok(fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/external/hooks/claude")
            .join(name),
    )?)
}
#[test]
fn recorded_claude_events_use_native_protocol_and_never_touch_legacy_state() -> Result {
    let mut rig = TestRig::new()?;
    // Exercise the lane's pinned Git and private temporary directory too.
    rig.env
        .push((provenant_run::LANE_DEPTH_ENV.into(), "1".into()));
    publish_checkpoint(&rig)?;
    for spec in provenant_cli::hooks::HOOK_SPECS
        .iter()
        .filter(|spec| spec.fixture.is_some())
    {
        let command = spec.registration.command[2];
        let file = spec.fixture.ok_or("fixture")?;
        let mut input: serde_json::Value = serde_json::from_slice(&recording(file)?)?;
        input["cwd"] = rig.project.to_str().ok_or("project path")?.into();
        let output = rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["hook", "claude", command],
            Some(&serde_json::to_vec(&input)?),
        )?;
        assert_eq!(code(&output), 0, "{command}: {:?}", output.stderr.bytes);
        if command == "pre-tool-use" {
            assert!(output.stdout.bytes.is_empty());
        } else {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
            let context = if command == "compact" {
                value["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .ok_or("context")?
            } else {
                value["systemMessage"].as_str().ok_or("advisory")?
            };
            assert!(context.contains("recorded-session"));
            assert!(context.contains("Checkpoint present"));
            if command == "compact" {
                assert!(context.contains("checkpoint fixture: retain originating authority"));
            }
        }
        assert!(output.stdout.bytes.len() <= 8192);
    }
    rig.assert_legacy_unchanged()
}
#[test]
fn bash_guard_denies_bad_input_and_allows_process_control() -> Result {
    let rig = TestRig::new()?;
    let host: serde_json::Value = serde_json::from_slice(&recording("host-deny-json.json")?)?;
    assert_eq!(host["permission_denials"][0]["tool_name"], "Bash");
    let mut input: serde_json::Value = serde_json::from_slice(&recording("pre-tool-use.json")?)?;
    input["tool_input"]["command"] = "pkill -f claude; sleep 30; $CMD".into();
    let output = rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["hook", "claude", "pre-tool-use"],
        Some(&serde_json::to_vec(&input)?),
    )?;
    assert_eq!(code(&output), 0);
    assert!(output.stdout.bytes.is_empty());
    for bytes in [b"{".to_vec(), vec![b' '; 1024 * 1024 + 1]] {
        let output = rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["hook", "claude", "pre-tool-use"],
            Some(&bytes),
        )?;
        assert_eq!(code(&output), 0);
        let deny: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            output.stdout.bytes.iter().filter(|b| **b == b'\n').count(),
            1
        );
    }
    Ok(())
}

#[test]
fn bash_guard_rules_deny_with_a_remedy_and_follow_config() -> Result {
    let rig = TestRig::new()?;
    let mut input: serde_json::Value = serde_json::from_slice(&recording("pre-tool-use.json")?)?;
    input["cwd"] = rig.project.to_str().ok_or("project path")?.into();
    let mut decide = |command: &str| -> Result<Option<String>> {
        input["tool_input"]["command"] = command.into();
        let output = rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["hook", "claude", "pre-tool-use"],
            Some(&serde_json::to_vec(&input)?),
        )?;
        assert_eq!(code(&output), 0);
        if output.stdout.bytes.is_empty() {
            return Ok(None);
        }
        let deny: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        Ok(deny["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .map(str::to_owned))
    };
    let remedy = decide("git push --force origin main")?.ok_or("force push allowed")?;
    assert!(remedy.contains("open a PR"), "{remedy}");
    assert!(decide("git push --force origin topic")?.is_none());
    assert!(decide("git stash")?.is_none());
    assert!(decide("rm -rf ~")?.is_some());

    fs::create_dir_all(&rig.state)?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.guard]\ndeny = ['rm-root']\n",
    )?;
    assert!(decide("git push --force origin main")?.is_none());
    assert!(decide("rm -rf ~")?.is_some());
    Ok(())
}
#[test]
fn unreadable_guard_config_keeps_the_default_rules_on() -> Result {
    let rig = TestRig::new()?;
    let mut input: serde_json::Value = serde_json::from_slice(&recording("pre-tool-use.json")?)?;
    input["cwd"] = rig.project.to_str().ok_or("project path")?.into();
    input["tool_input"]["command"] = "git push --force origin main".into();
    fs::create_dir_all(&rig.state)?;
    // A directory at `config.toml` is a deterministic read failure.
    for config in [
        Some("schema_version = 1\n[domains.guard]\ndeny = 'force-push'\n"),
        Some("schema_version = 1\n[domains.guard\n"),
        None,
    ] {
        let path = rig.state.join("config.toml");
        match config {
            Some(text) => fs::write(&path, text)?,
            None => {
                fs::remove_file(&path)?;
                fs::create_dir(&path)?;
            }
        }
        let output = rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["hook", "claude", "pre-tool-use"],
            Some(&serde_json::to_vec(&input)?),
        )?;
        assert_eq!(code(&output), 0);
        let deny: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(
            deny["hookSpecificOutput"]["permissionDecision"], "deny",
            "{config:?}"
        );
    }
    Ok(())
}
#[test]
fn denial_write_failure_uses_recorded_exit_two_fallback() -> Result {
    let rig = TestRig::new()?;
    // Duplicate the read-only stdin pipe onto stdout. The binary starts
    // normally but its denial cannot be written. No product-only test switch.
    let output = rig.execute(
        Path::new("/bin/sh"),
        &[
            "-c",
            "exec \"$1\" hook claude pre-tool-use 1>&0",
            "fixture",
            env!("CARGO_BIN_EXE_provenant"),
        ],
        Some(b"{"),
    )?;
    assert_eq!(code(&output), 2);
    assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("bounded-input error"));
    assert!(output.stderr.bytes.len() <= 1024);
    let host: serde_json::Value = serde_json::from_slice(&recording("host-deny-exit2.json")?)?;
    assert_eq!(host["permission_denials"][0]["tool_name"], "Bash");
    Ok(())
}
fn git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let output = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        args,
        None,
    )?;
    assert_eq!(
        code(&output),
        0,
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
}
fn commit(rig: &TestRig, message: &str, content: &str) -> Result<String> {
    fs::write(rig.project.join("file"), content)?;
    git(rig, &["add", "file"])?;
    git(
        rig,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            message,
        ],
    )?;
    git(rig, &["rev-parse", "HEAD"])
}
#[test]
fn pre_push_uses_real_git_to_refuse_data_loss_and_conflict_markers() -> Result {
    for format in ["sha1", "sha256"] {
        check_push_policy(&TestRig::with_object_format(&std::env::temp_dir(), format)?)?;
    }
    Ok(())
}
fn check_push_policy(rig: &TestRig) -> Result {
    git(rig, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
    let first = commit(rig, "initial", "initial\n")?;
    let second = commit(rig, "second", "second\n")?;
    let zero = "0".repeat(first.len());
    let cases = [
        (
            format!("(delete) {zero} refs/heads/main {second}\n"),
            1,
            "deleting the default branch",
        ),
        (
            format!("refs/heads/main {first} refs/heads/main {second}\n"),
            1,
            "non-fast-forward",
        ),
        (
            format!("refs/heads/main {second} refs/heads/main {first}\n"),
            0,
            "",
        ),
    ];
    for (input, status, reason) in cases {
        let output = rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["hook", "git", "pre-push", "origin", "fixture"],
            Some(input.as_bytes()),
        )?;
        assert_eq!(
            code(&output),
            status,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(output.stdout.bytes.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr.bytes).contains(reason),
            "{reason}: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
    }
    let conflict = commit(rig, "conflict", "<<<<<<< HEAD\nleft\n>>>>>>> topic\n")?;
    let input = format!("refs/heads/main {conflict} refs/heads/topic {second}\n");
    let output = rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["hook", "git", "pre-push", "origin", "fixture"],
        Some(input.as_bytes()),
    )?;
    assert_eq!(code(&output), 1);
    assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("conflict markers"));
    git(rig, &["update-ref", "refs/remotes/origin/main", &conflict])?;
    let fixed = commit(rig, "resolved", "resolved\n")?;
    let input = format!("refs/heads/main {fixed} refs/heads/topic {zero}\n");
    let output = rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["hook", "git", "pre-push", "origin", "fixture"],
        Some(input.as_bytes()),
    )?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(output.stdout.bytes.is_empty());
    let linked = rig.root.join("linked");
    git(
        rig,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().ok_or("linked")?,
            "HEAD",
        ],
    )?;
    let input = format!("HEAD {fixed} refs/heads/main {conflict}\n");
    let output = rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--project",
            linked.to_str().ok_or("linked")?,
            "hook",
            "git",
            "pre-push",
            "origin",
            "fixture",
        ],
        Some(input.as_bytes()),
    )?;
    assert_eq!(code(&output), 1);
    assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("linked worktree"));
    Ok(())
}

#[test]
fn unreadable_and_stalled_input_cannot_allow_bash() -> Result {
    let rig = TestRig::new()?;
    let fifo = rig.root.join("hook-input");
    let made = rig.execute(
        Path::new("/usr/bin/mkfifo"),
        &[fifo.to_str().ok_or("fifo")?],
        None,
    )?;
    assert_eq!(code(&made), 0);
    for script in [
        "exec \"$1\" hook claude pre-tool-use <&-",
        "exec 3<>\"$2\"; exec \"$1\" hook claude pre-tool-use <\"$2\"",
    ] {
        let output = rig.execute(
            Path::new("/bin/sh"),
            &[
                "-c",
                script,
                "fixture",
                env!("CARGO_BIN_EXE_provenant"),
                fifo.to_str().ok_or("fifo")?,
            ],
            None,
        )?;
        assert_eq!(code(&output), 0);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(value["hookSpecificOutput"]["permissionDecision"], "deny");
    }
    Ok(())
}

fn publish_checkpoint(rig: &TestRig) -> Result {
    use provenant_core::{CancelToken, Clock, RootInputs, WaitBudget};
    use provenant_os::clock::SystemClock;

    use provenant_workflow::session::{HostSessionId, SessionStore};
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let common = fs::canonicalize(rig.project.join(".git"))?;
    let binding =
        provenant_config::bind_project(&roots, provenant_config::ProjectLocation::new(common)?)?;
    let clock = SystemClock::new();
    let store = SessionStore::new(
        roots.state,
        binding.project_id(),
        HostSessionId::parse("recorded-session")?,
    );
    store.save(
        b"checkpoint fixture: retain originating authority",
        WaitBudget::cli(clock.mono_now(), 1)?,
        &CancelToken::new(),
        &clock,
    )?;
    Ok(())
}

fn json_command(rig: &TestRig, args: &[&str]) -> Result<serde_json::Value> {
    let mut args = args.to_vec();
    args.push("--json");
    let output = rig.command(&args)?;
    assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

#[test]
fn queued_answer_is_injected_once_only_after_the_guard_allows() -> Result {
    use serde_json::json;
    let rig = TestRig::new()?;
    let checkpoint = rig.root.join("checkpoint.md");
    fs::write(&checkpoint, "chair checkpoint")?;
    json_command(
        &rig,
        &[
            "workflow",
            "session",
            "save",
            "chair",
            "--file",
            checkpoint.to_str().ok_or("path")?,
            "--client",
            "claude",
        ],
    )?;
    json_command(&rig, &["inbox", "--state", "all"])?;
    let hook = |command: &str| -> Result<provenant_os::exec::ExecOutcome> {
        rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["hook", "claude", "pre-tool-use"],
            Some(&serde_json::to_vec(
                &json!({"hook_event_name":"PreToolUse", "tool_name":"Bash",
                "tool_input":{"command":command}, "session_id":"chair", "cwd":rig.project}),
            )?),
        )
    };
    hook("git status")?; // Warm the empty lookup; do not impose a cold-start timing assertion.
    let draft = rig.root.join("question.json");
    fs::write(
        &draft,
        serde_json::to_vec(&json!({"kind":"question","title":"Next?","body":"Choose"}))?,
    )?;
    let created = json_command(
        &rig,
        &[
            "ask",
            "--file",
            draft.to_str().ok_or("path")?,
            "--session",
            "chair",
        ],
    )?;
    let id = created["data"]["request_id"].as_str().ok_or("request")?;
    let reply = rig.root.join("reply.txt");
    fs::write(&reply, "Proceed carefully")?;
    json_command(
        &rig,
        &[
            "answer",
            id,
            "--action",
            "reply",
            "--text-file",
            reply.to_str().ok_or("path")?,
            "--expect-revision",
            "1",
            "--channel",
            "console",
        ],
    )?;
    let denied = hook("rm -rf /")?;
    assert_eq!(code(&denied), 0);
    let denial: serde_json::Value = serde_json::from_slice(&denied.stdout.bytes)?;
    assert_eq!(denial["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        denial["hookSpecificOutput"]
            .get("additionalContext")
            .is_none()
    );
    let allowed = hook("git status")?;
    assert_eq!(code(&allowed), 0, "{:?}", allowed.stderr.bytes);
    assert!(allowed.stdout.bytes.len() <= provenant_records::inbox::MAX_OFFER_BYTES);
    let wire: serde_json::Value = serde_json::from_slice(&allowed.stdout.bytes)?;
    assert!(
        wire["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .ok_or("context")?
            .contains("Proceed carefully")
    );
    assert!(
        wire["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none()
    );
    assert!(hook("git status")?.stdout.bytes.is_empty());
    let item = json_command(&rig, &["inbox", id])?;
    assert_eq!(item["data"]["item"]["delivery"]["state"], "offered");
    assert_eq!(item["data"]["item"]["delivery"]["attempts"], 1);
    assert!(item["data"]["item"]["delivery"]["last_attempt_at"].is_string());
    Ok(())
}
