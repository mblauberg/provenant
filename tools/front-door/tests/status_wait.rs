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
    io::{BufRead, BufReader, Read},
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
        "authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}],"concurrency":1,"parent":null}))?;
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
    // Graph status resolves through the host Fabric identity, as real graph
    // submission does. A raw Run store alone does not initialise that identity.
    provenant_cli::fabric::Context::open(roots.clone(), &rig.home, &rig.project)?;
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
fn fake_gh_dynamic(rig: &mut TestRig) -> Result {
    let bin = rig.root.join("bin");
    fs::create_dir_all(&bin)?;
    fs::write(
        bin.join("gh"),
        format!(
            "#!/bin/sh\ncat '{}'\ntouch '{}'\n",
            rig.root.join("pr.json").display(),
            rig.root.join("gh-started").display()
        ),
    )?;
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    fs::write(
        rig.root.join("pr.json"),
        json!({"state":"OPEN","headRefOid":"abc","statusCheckRollup":[{"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":null}]}).to_string(),
    )?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.wait]\ntimeout_seconds=2\nlocal_interval_ms=10\npr_interval_ms=10\n",
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
fn refused_operation_wait_returns_immediately_with_nonzero_status() -> Result {
    let rig = TestRig::new()?;
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let context = ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: identity["data"]["project"]
            .as_str()
            .ok_or("project")?
            .parse()?,
        actor_id: identity["data"]["inbox"]["actor"]
            .as_str()
            .ok_or("actor")?
            .parse()?,
        read_prompts: false,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let task = TaskId::from_bytes([3; 16]);
    let request: CanonicalRequest = serde_json::from_value(
        json!({"schema_version":1,"chair": {"host":identity["data"]["inbox"]["host"],"actor":identity["data"]["inbox"]["actor"]},"tasks":[{
        "task_id":task,"prompt_digest":provenant_records::CanonicalV1::file_digest(b"refused fixture"),
        "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":null},
        "fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}],"concurrency":1,"parent":null}),
    )?;
    let store = RunStore::open(&context, &call)?;
    store.record_refusal(
        &PreparedRun {
            run_id: RunId::from_bytes([6; 16]),
            operation_id: OperationId::parse("refused-fixture")?,
            created_at: clock.wall_now()?,
            request,
            attempts: vec![NewAttempt {
                task_id: task,
                attempt_id: AttemptId::from_bytes([4; 16]),
                prompt: "refused fixture".into(),
            }],
        },
        provenant_records::diagnostic::Diagnostic::new("UNSUPPORTED").category("refused"),
        None,
        &call,
    )?;
    let started = Instant::now();
    let output = rig.command(&[
        "status",
        "operation:refused-fixture",
        "--wait=30s",
        "--json",
    ])?;
    assert_eq!(output.termination, provenant_os::exec::Termination::Exit(1));
    assert!(started.elapsed() < Duration::from_secs(10));
    let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        result["data"]["operation"]["refusal"]["code"], "UNSUPPORTED",
        "{result}"
    );
    assert_eq!(result["data"]["condition_met"], true);
    assert_eq!(result["data"]["timed_out"], false);
    let output = rig.command(&[
        "status",
        "--wait=30s",
        "--target",
        "operation:refused-fixture",
        "--json",
    ])?;
    assert_eq!(output.termination, provenant_os::exec::Termination::Exit(1));
    let lines = events(&output.stdout.bytes)?;
    assert_eq!(
        lines[0]["data"]["operation"]["refusal"]["code"],
        "UNSUPPORTED"
    );
    assert_eq!(lines.last().ok_or("end")?["condition_met"], true);
    // Without --wait a target reports its current status once.
    let output = rig.command(&["status", "--target", "operation:refused-fixture", "--json"])?;
    assert_eq!(output.termination, provenant_os::exec::Termination::Exit(1));
    let lines = events(&output.stdout.bytes)?;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["event"], "snapshot");
    assert_eq!(
        lines[0]["data"]["operation"]["refusal"]["code"],
        "UNSUPPORTED"
    );
    // Simulate a crash after recording the refusal but before its notice.
    // Reconciliation retries it, and a second pass must not duplicate mail.
    assert_eq!(store.pending_refusal_notices(10, &call)?.len(), 1);
    for _ in 0..2 {
        let output = rig.command(&["reconcile", "--json"])?;
        assert_eq!(
            output.termination,
            provenant_os::exec::Termination::Exit(0),
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
    }
    assert!(store.pending_refusal_notices(10, &call)?.is_empty());
    let mail_call = provenant_fabric_store::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let mail = provenant_fabric_store::Store::open(&context.state_root, &mail_call)?;
    let page = mail.read_mail(
        context.project_id,
        context.actor_id,
        Some(0),
        50,
        &mail_call,
    )?;
    let notices: Vec<_> = page
        .items
        .iter()
        .filter(|item| item.message.body.contains("refused-fixture"))
        .collect();
    assert_eq!(notices.len(), 1);
    graph(&rig, true)?;
    let output = rig.command(&[
        "status",
        "--wait=30s",
        "--until",
        "all",
        "--target",
        "operation:refused-fixture",
        "--target",
        "graph:fixture",
        "--json",
    ])?;
    assert_eq!(output.termination, provenant_os::exec::Termination::Exit(1));
    let lines = events(&output.stdout.bytes)?;
    assert!(
        lines
            .iter()
            .any(|line| line["target"] == "operation:refused-fixture")
    );
    assert!(lines.iter().any(|line| line["target"] == "graph:fixture"));
    assert_eq!(lines.last().ok_or("end")?["condition_met"], true);
    Ok(())
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
            "--until",
            "any",
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
fn default_single_target_wait_ignores_nonterminal_transition() -> Result {
    let mut rig = TestRig::new()?;
    fake_gh_dynamic(&mut rig)?;
    let mut child = spawn(&rig, &["status", "--wait=2s", "--target", "pr:1", "--json"])?;
    provenant_test_support::wait_for(
        "initial PR snapshot",
        Duration::from_secs(5),
        || rig.root.join("gh-started").exists(),
        |ready| *ready,
    )?;
    fs::write(
        rig.root.join("pr.json"),
        json!({"state":"OPEN","headRefOid":"abc","statusCheckRollup":[{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}]}).to_string(),
    )?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("stdout")?);
    let mut output = Vec::new();
    stdout.read_to_end(&mut output)?;
    let status = child.wait()?;
    assert_eq!(status.code(), Some(124));
    let lines = events(&output)?;
    assert!(
        lines
            .iter()
            .any(|line| line["event"] == "transition" && line["terminal"] == false),
        "expected a non-terminal transition: {lines:?}"
    );
    assert_eq!(lines.last().ok_or("end")?["timed_out"], true);
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
