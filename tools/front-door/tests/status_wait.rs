//! Mixed-target waits exercise real Run store reads and isolated GitHub snapshots.
#![allow(clippy::disallowed_methods)]
mod support;
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, RootInputs, RunId, TaskId,
};
use provenant_os::clock::SystemClock;
use provenant_run::{
    CanonicalRequest, ExecutionContext, NewAttempt, PreparedRun, Request, RunStore,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use support::{Result, TestRig};

fn seed(rig: &TestRig) -> Result<(RunStore, AttemptId)> {
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let project = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id();
    let context = ExecutionContext {
        state_root: roots.state,
        project_id: project,
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let task = TaskId::from_bytes([3; 16]);
    let request: CanonicalRequest = serde_json::from_value(json!({"schema_version":1,"tasks":[{
        "task_id":task,"prompt_digest":provenant_records::CanonicalV1::file_digest(b"fake lane"),
        "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":null},
        "fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}],"concurrency":1,"parent":null}))?;
    let store = RunStore::open(&context, &call)?;
    let attempt = AttemptId::from_bytes([4; 16]);
    store.create(
        &PreparedRun {
            run_id: RunId::from_bytes([6; 16]),
            operation_id: OperationId::parse("status-wait")?,
            created_at: clock.wall_now()?,
            request,
            attempts: vec![NewAttempt {
                task_id: task,
                attempt_id: attempt,
                prompt: "fake lane".into(),
            }],
        },
        &call,
    )?;
    Ok((store, attempt))
}
fn graph(rig: &TestRig, delivered: bool) -> Result {
    use provenant_fabric_runtime::graph::{
        FileJournal, GraphCheckpoint, GraphExecution, GraphJournal,
    };
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let project = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &roots.state,
        project,
        "fixture",
        provenant_core::WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
    )?;
    let execution: GraphExecution = serde_json::from_value(
        json!({"directory":rig.project,"document":{"schema_version":1,"graph_id":"fixture","nodes":[]}}),
    )?;
    journal.bind(execution)?;
    let checkpoint: GraphCheckpoint = serde_json::from_value(
        json!({"schema_version":1,"graph_id":"fixture","plan_digest":"fixture","nodes":[],"delivery_required":true,"delivered":delivered}),
    )?;
    journal.save(&checkpoint)?;
    Ok(())
}
fn finish(store: &RunStore, attempt: AttemptId) -> Result {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    store.cancel(
        attempt,
        "fixture finished",
        &Request {
            clock: &clock,
            cancel: &cancel,
        },
    )?;
    Ok(())
}
fn fake_gh(rig: &mut TestRig, blocking: bool) -> Result {
    let bin = rig.root.join("bin");
    fs::create_dir_all(&bin)?;
    let script = if blocking {
        let fifo = rig.root.join("gh-release");
        assert!(Command::new("mkfifo").arg(&fifo).status()?.success());
        format!(
            "#!/bin/sh\ntouch '{}'\nexec /bin/cat '{}'\n",
            rig.root.join("gh-started").display(),
            fifo.display()
        )
    } else {
        format!(
            "#!/bin/sh\nif [ ! -e '{}' ]; then touch '{}'; echo 'HTTP 503 Service Unavailable' >&2; exit 1; fi\ncat '{}'\n",
            rig.root.join("gh-started").display(),
            rig.root.join("gh-started").display(),
            rig.root.join("pr.json").display()
        )
    };
    fs::write(bin.join("gh"), script)?;
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    fs::write(rig.root.join("pr.json"), json!({"state":"OPEN","headRefOid":"abc","statusCheckRollup":[{"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":null}]}).to_string())?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.wait]\ntimeout_seconds=1\nlocal_interval_ms=10\npr_interval_ms=10\n",
    )?;
    Ok(())
}
fn spawn(rig: &TestRig, args: &[&str]) -> Result<Child> {
    Ok(Command::new(env!("CARGO_BIN_EXE_provenant"))
        .args(args)
        .env_clear()
        .envs(rig.env.iter().cloned())
        .current_dir(&rig.project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}
fn events(bytes: &[u8]) -> Result<Vec<Value>> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
        .map(|line| serde_json::from_slice(line).map_err(Into::into))
        .collect()
}
#[test]
fn mixed_status_wait_returns_on_fake_lane_while_pr_read_is_blocked() -> Result {
    let mut rig = TestRig::new()?;
    let (store, attempt) = seed(&rig)?;
    fake_gh(&mut rig, true)?;
    let target = format!("lane:{attempt}");
    let child = spawn(
        &rig,
        &[
            "status",
            "--wait=10s",
            "--target",
            &target,
            "--target",
            "pr:1",
            "--json",
        ],
    )?;
    provenant_test_support::wait_for(
        "PR fixture readiness",
        Duration::from_secs(5),
        || rig.root.join("gh-started").exists(),
        |ready| *ready,
    )?;
    let started = Instant::now();
    finish(&store, attempt)?;
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{output:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "slow PR delayed the lane event"
    );
    let lines = events(&output.stdout)?;
    assert_eq!(lines[0]["target"], target);
    assert_eq!(lines[0]["terminal"], true);
    assert_eq!(lines.last().ok_or("end")?["condition_met"], true);
    Ok(())
}
#[test]
fn mixed_status_follow_streams_once_per_transition_until_all_terminal() -> Result {
    let mut rig = TestRig::new()?;
    let (store, attempt) = seed(&rig)?;
    fake_gh(&mut rig, false)?;
    graph(&rig, false)?;
    let target = format!("lane:{attempt}");
    let mut child = spawn(
        &rig,
        &[
            "status",
            "--wait=10s",
            "--follow",
            "--target",
            "graph:fixture",
            "--target",
            &target,
            "--target",
            "pr:1",
            "--json",
        ],
    )?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("stdout")?);
    let read = |stdout: &mut BufReader<std::process::ChildStdout>| -> Result<Value> {
        let mut line = String::new();
        stdout.read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    };
    let first = read(&mut stdout)?;
    let second = read(&mut stdout)?;
    let third = read(&mut stdout)?;
    assert_eq!(third["event"], "snapshot");
    assert_eq!(first["event"], "snapshot");
    assert_eq!(second["event"], "snapshot");
    finish(&store, attempt)?;
    let settled = read(&mut stdout)?;
    assert_eq!(settled["target"], target);
    assert_eq!(settled["event"], "transition");
    assert!(
        child.try_wait()?.is_none(),
        "follow exited before PR terminal"
    );
    fs::write(
        rig.root.join("pr.json"),
        json!({"state":"MERGED","headRefOid":"abc"}).to_string(),
    )?;
    let merged = read(&mut stdout)?;
    assert_eq!(merged["target"], "pr:1");
    assert_eq!(merged["terminal"], true);
    assert!(
        child.try_wait()?.is_none(),
        "follow exited before graph terminal"
    );
    graph(&rig, true)?;
    let completed = read(&mut stdout)?;
    assert_eq!(completed["target"], "graph:fixture");
    assert_eq!(completed["terminal"], true);
    assert_eq!(read(&mut stdout)?["event"], "end");
    assert!(child.wait()?.success());
    Ok(())
}
#[test]
fn mixed_status_wait_uses_configured_timeout_and_per_call_override() -> Result {
    let mut rig = TestRig::new()?;
    let (_store, attempt) = seed(&rig)?;
    fake_gh(&mut rig, false)?;
    let target = format!("lane:{attempt}");
    for (wait, seconds) in [("--wait", 1), ("--wait=2s", 2)] {
        let started = Instant::now();
        let output =
            spawn(&rig, &["status", wait, "--target", &target, "--json"])?.wait_with_output()?;
        assert_eq!(output.status.code(), Some(124), "{output:?}");
        assert!(started.elapsed() >= Duration::from_secs(seconds));
        assert!(started.elapsed() < Duration::from_secs(seconds + 30));
        assert_eq!(
            events(&output.stdout)?.last().ok_or("end")?["timed_out"],
            true
        );
    }
    Ok(())
}

#[test]
fn run_wait_returns_the_registry_lock_refusal_at_deadline() -> Result {
    use provenant_os::fs::{Directory, LockMode};
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?;
    assert_eq!(support::code(&commit), 0);
    let create = rig.command(&[
        "worktree",
        "create",
        "wait-lock",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(
        support::code(&create),
        0,
        "{}",
        String::from_utf8_lossy(&create.stderr.bytes)
    );
    let registry = fs::read_dir(rig.state.join("worktrees"))?
        .next()
        .ok_or("registry")??
        .path();
    let directory = Directory::open_anchor(&registry)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let _lock = directory.lock(
        std::ffi::OsStr::new("worktrees.lock"),
        LockMode::Exclusive,
        provenant_core::WaitBudget::cli(clock.mono_now(), 10)?,
        &cancel,
        &clock,
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "Reply OK")?;
    let result = rig.command(&[
        "run",
        "--operation-id",
        "wait-lock-timeout",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree",
        ".worktrees/wait-lock",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "1s",
        "--json",
    ])?;
    assert_ne!(support::code(&result), 0);
    let response: Value = serde_json::from_slice(&result.stdout.bytes)?;
    assert_eq!(response["error"]["code"], "WRITER_LOCK_BUSY", "{response}");
    assert!(
        response["error"]["next"]
            .as_str()
            .is_some_and(|next| !next.is_empty()),
        "{response}"
    );
    Ok(())
}

#[test]
fn run_wait_retries_until_the_registry_lock_is_released() -> Result {
    use provenant_os::fs::{Directory, LockMode};
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?;
    assert_eq!(support::code(&commit), 0);
    let create = rig.command(&[
        "worktree",
        "create",
        "wait-release",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(
        support::code(&create),
        0,
        "{}",
        String::from_utf8_lossy(&create.stderr.bytes)
    );
    let registry = fs::read_dir(rig.state.join("worktrees"))?
        .next()
        .ok_or("registry")??
        .path();
    let directory = Directory::open_anchor(&registry)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let lock = directory.lock(
        std::ffi::OsStr::new("worktrees.lock"),
        LockMode::Exclusive,
        provenant_core::WaitBudget::cli(clock.mono_now(), 10)?,
        &cancel,
        &clock,
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "Reply OK")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .args([
            "--state-dir",
            rig.state.to_str().ok_or("state")?,
            "run",
            "--operation-id",
            "wait-lock-release",
            "--model",
            "codex/gpt-6-luna",
            "--mode",
            "worktree-write",
            "--worktree",
            ".worktrees/wait-release",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--wait",
            "60s",
            "--json",
        ])
        .env_clear()
        .envs(rig.env.iter().cloned())
        .current_dir(&rig.project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    let observed = provenant_test_support::wait_for(
        "registry lock held for seven seconds",
        Duration::from_secs(10),
        || (started.elapsed(), child.try_wait()),
        |(elapsed, result)| *elapsed >= Duration::from_secs(7) || !matches!(result, Ok(None)),
    )?;
    if let Ok(Some(_)) = observed.1 {
        let output = child.wait_with_output()?;
        return Err(format!(
            "run exited before lock release: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    observed.1?;
    drop(lock);
    let completed = provenant_test_support::wait_for(
        "run after registry lock release",
        Duration::from_secs(65),
        || child.try_wait(),
        |result| !matches!(result, Ok(None)),
    );
    if completed.is_err() {
        child.kill()?;
    }
    let _ = completed??;
    let output = child.wait_with_output()?;
    let response: Value = serde_json::from_slice(&output.stdout)?;
    let run = response["data"]["run_id"]
        .as_str()
        .ok_or_else(|| format!("missing admitted run: {response}"))?;
    let lanes = rig.command(&["lanes", "--id", run, "--all", "--json"])?;
    assert_eq!(
        support::code(&lanes),
        0,
        "{}",
        String::from_utf8_lossy(&lanes.stderr.bytes)
    );
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    assert_eq!(
        lanes["data"]["lanes"].as_array().map(Vec::len),
        Some(1),
        "{lanes}"
    );
    Ok(())
}

#[test]
fn saved_refusal_is_visible_in_status_and_lanes() -> Result {
    let rig = TestRig::new()?;
    let (store, attempt) = seed(&rig)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let lane = store.record(attempt, &call)?;
    let refusal = provenant_run::OperationRecord {
        extra: Default::default(),
        schema_version: 1,
        project_id: lane.project_id,
        actor_id: ActorId::from_bytes([2; 16]),
        operation_id: OperationId::parse("visible-refusal")?,
        kind: provenant_run::OperationKind::Dispatch,
        request_digest: Digest::from_bytes([11; 32]),
        run_id: RunId::from_bytes([12; 16]),
        state: provenant_run::OperationState::Terminal,
        refusal: Some(provenant_run::OperationRefusal {
            extra: Default::default(),
            diagnostic: provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
                .category("integrity")
                .detail("writer scope refused")
                .next("inspect the worktree"),
            worktree: Some(provenant_records::PathWire::from_relative(
                &provenant_core::RelativePath::parse("writer")?,
            )?),
            refused_at: clock.wall_now()?,
            chair: None,
        }),
    };
    store.record_refusal(&refusal, &call)?;
    let hex_id = "ABCDEF0123456789ABCDEF0123456789";
    let mut hex_refusal = refusal.clone();
    hex_refusal.operation_id = OperationId::parse(hex_id)?;
    hex_refusal.run_id = RunId::from_bytes([13; 16]);
    store.record_refusal(&hex_refusal, &call)?;
    let mut collision = refusal.clone();
    collision.operation_id = OperationId::parse(&attempt.to_string())?;
    collision.run_id = RunId::from_bytes([14; 16]);
    store.record_refusal(&collision, &call)?;
    drop(store);
    for command in ["status", "lanes"] {
        let json = rig.command(&[command, "--json"])?;
        assert_eq!(
            support::code(&json),
            0,
            "{}",
            String::from_utf8_lossy(&json.stderr.bytes)
        );
        let data: Value = serde_json::from_slice(&json.stdout.bytes)?;
        let rows = data["data"]["refusals"]
            .as_array()
            .ok_or("missing refusal list")?;
        assert_eq!(rows.len(), 3, "{data}");
        let row = rows
            .iter()
            .find(|row| row["operation_id"] == "visible-refusal")
            .ok_or("missing operation")?;
        assert_eq!(row["code"], "WRITER_SCOPE");
        assert_eq!(row["next"], "inspect the worktree");
        assert_eq!(row["worktree"]["path"], "writer");
        assert!(row["refused_at"].is_string());
        assert!(row.get("run_id").is_none());
        assert!(row.get("attempt_id").is_none());
        let human = rig.command(&[command])?;
        assert_eq!(support::code(&human), 0);
        let text = String::from_utf8(human.stdout.bytes)?;
        assert!(
            text.contains("visible-refusal")
                && text.contains("WRITER_SCOPE")
                && text.contains("inspect the worktree"),
            "{text}"
        );
    }
    for args in [
        vec!["status", "visible-refusal", "--json"],
        vec!["lanes", "--id", "visible-refusal", "--json"],
        vec!["status", hex_id, "--json"],
        vec!["lanes", "--id", hex_id, "--json"],
    ] {
        let output = rig.command(&args)?;
        assert_eq!(
            support::code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let data: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(
            data["data"]["refusals"][0]["code"], "WRITER_SCOPE",
            "{data}"
        );
    }
    let lane_id = attempt.to_string();
    let mixed = rig.command(&[
        "lanes",
        "--id",
        "visible-refusal",
        "--id",
        &lane_id,
        "--json",
    ])?;
    assert_eq!(support::code(&mixed), 0);
    let mixed: Value = serde_json::from_slice(&mixed.stdout.bytes)?;
    assert_eq!(
        mixed["data"]["refusals"][0]["operation_id"],
        "visible-refusal"
    );
    assert_eq!(mixed["data"]["lanes"].as_array().map(Vec::len), Some(1));
    assert_eq!(mixed["data"]["refusals"].as_array().map(Vec::len), Some(1));
    for id in ["visible-refusal", hex_id] {
        let started = Instant::now();
        let waited = rig.command(&["lanes", "--id", id, "--wait", "60s", "--json"])?;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_ne!(support::code(&waited), 0);
        let data: Value = serde_json::from_slice(&waited.stdout.bytes)?;
        assert_eq!(data["error"]["code"], "WRITER_SCOPE", "{data}");
        assert_eq!(data["error"]["next"], "inspect the worktree", "{data}");
    }
    Ok(())
}

#[test]
fn refusal_only_store_never_says_there_are_no_lanes() -> Result {
    let rig = TestRig::new()?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let project = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id();
    let context = ExecutionContext {
        state_root: roots.state,
        project_id: project,
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    store.record_refusal(
        &provenant_run::OperationRecord {
            extra: Default::default(),
            schema_version: 1,
            project_id: project,
            actor_id: context.actor_id,
            operation_id: OperationId::parse("only-refusal")?,
            kind: provenant_run::OperationKind::Dispatch,
            request_digest: Digest::from_bytes([31; 32]),
            run_id: RunId::from_bytes([32; 16]),
            state: provenant_run::OperationState::Terminal,
            refusal: Some(provenant_run::OperationRefusal {
                extra: Default::default(),
                diagnostic: provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
                    .category("integrity")
                    .detail("writer scope refused")
                    .next("inspect the worktree"),
                worktree: Some(provenant_records::PathWire::from_relative(
                    &provenant_core::RelativePath::parse("writer")?,
                )?),
                refused_at: clock.wall_now()?,
                chair: None,
            }),
        },
        &call,
    )?;
    drop(store);
    for command in ["status", "lanes"] {
        for full in [false, true] {
            let mut args = vec![command];
            if full {
                args.push("--full");
            }
            let output = rig.command(&args)?;
            assert_eq!(support::code(&output), 0);
            let text = String::from_utf8(output.stdout.bytes)?;
            assert!(
                text.contains("only-refusal") && text.contains("WRITER_SCOPE"),
                "{text}"
            );
            assert!(
                !text.contains("no lanes") && !text.contains("no matching lanes"),
                "{text}"
            );
        }
    }
    Ok(())
}
