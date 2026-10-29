//! Lane Fabric observations and the attempt-bound host messaging endpoint.
mod support;
use provenant_core::{AttemptId, CancelToken, RootInputs};
use provenant_fabric_runtime::{
    coordination::{CoordinationService, ThreadPause},
    identity::Address,
};
use provenant_os::clock::SystemClock;
use serde_json::{Value, json};
use std::{ffi::OsString, path::Path};
use support::{Result, TestRig, code};

fn host(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let output = rig.command(args)?;
    assert_eq!(
        code(&output),
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}
fn lane_environment(rig: &mut TestRig, attempt: AttemptId) -> Result {
    let children = rig.state.join("children").join(attempt.to_string());
    std::fs::create_dir_all(&children)?;
    rig.env.extend([
        (
            provenant_core::RUN_STATE_DIRECTORY_ENV.into(),
            children.into(),
        ),
        ("PROVENANT_LANE_DEPTH".into(), "1".into()),
        (
            "PROVENANT_PARENT_ATTEMPT_ID".into(),
            attempt.to_string().into(),
        ),
    ]);
    Ok(())
}
fn mcp(rig: &TestRig, name: &str, arguments: Value) -> Result<Value> {
    let frames = [
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    ];
    let input = frames
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n")
        + "\n";
    let args: Vec<OsString> = vec![
        "fabric".into(),
        "mcp".into(),
        "--client".into(),
        "codex".into(),
    ];
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &args,
        Some(input.as_bytes()),
    )?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    for line in output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: Value = serde_json::from_slice(line)?;
        if value["id"] == 1 {
            return Ok(value["result"].clone());
        }
    }
    Err("missing MCP reply".into())
}

#[test]
fn lane_reads_wal_outside_home_without_mutating_the_database() -> Result {
    let mut rig = TestRig::new()?;
    assert!(!rig.project.starts_with(&rig.home));
    let task = host(
        &rig,
        &[
            "fabric",
            "task",
            "create",
            "--objective",
            "visible through WAL",
            "--operation-id",
            "lane-read-seed",
            "--json",
        ],
    )?;
    let files = ["", "-wal", "-shm", "-journal"];
    let before = files
        .map(|suffix| std::fs::read(rig.state.join(format!("fabric/fabric.sqlite3{suffix}"))).ok());
    lane_environment(&mut rig, AttemptId::from_bytes([13; 16]))?;
    let listed = host(&rig, &["fabric", "task", "list", "--json"])?;
    assert_eq!(
        listed["data"]["page"]["items"][0]["id"],
        task["data"]["task"]["id"]
    );
    let listed = mcp(&rig, "fabric_task", json!({"op":"list","limit":10}))?;
    assert_ne!(listed["isError"], true, "{listed}");
    assert_eq!(
        listed["structuredContent"]["page"]["items"][0]["id"],
        task["data"]["task"]["id"]
    );
    let after = files
        .map(|suffix| std::fs::read(rig.state.join(format!("fabric/fabric.sqlite3{suffix}"))).ok());
    assert_eq!(before, after);
    Ok(())
}

#[test]
fn lane_messaging_uses_the_host_attempt_actor_and_closes_with_the_endpoint() -> Result {
    // Run on the host in CI: nested lanes cannot create a host endpoint.
    let mut rig = TestRig::new()?;
    let project = rig.home.join("repo");
    std::fs::rename(&rig.project, &project)?;
    rig.project = project;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = rig.project.clone().into();
        }
    }
    let identity = host(&rig, &["fabric", "identity", "--json"])?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let mut context = provenant_cli::fabric::Context::open(roots, &rig.home, &rig.project)?;
    let attempt = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
    context.identity.inbox = Address::new(context.identity.host, &format!("lane-{attempt}"))?;
    let actor = context.identity.inbox.actor;
    let socket = provenant_providers::LaneRuntime::fabric_socket(attempt);
    let home = rig.home.clone();
    let cwd = rig.project.clone();
    let clock = SystemClock::new();
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    drop(CoordinationService::for_identity(
        &context.roots.state,
        &context.identity,
        &context.config,
        &home,
        &cwd,
        &executor,
        &clock,
        &ThreadPause,
        &CancelToken::new(),
    )?);
    let relay = provenant_fabric_mcp::lane::HostRelay::start(
        socket.clone(),
        provenant_records::Limits::CONFIG,
        move |call, cancel, within| {
            let budget = provenant_fabric_mcp::lane::host_budget(&clock, within)?;
            let service = CoordinationService::for_identity(
                &context.roots.state,
                &context.identity,
                &context.config,
                &home,
                &cwd,
                &executor,
                &clock,
                &ThreadPause,
                cancel,
            )?;
            call.execute_lane_coordination(&service, &context.config.limits, budget, cancel)
        },
    )?;
    lane_environment(&mut rig, attempt)?;
    // The request and even a forged lane environment cannot choose the sender.
    let args = json!({"op":"send","recipients":[identity["data"]["inbox"]["actor"]],"kind":"note","body":"host-relayed message","artifacts":[],"operation_id":"lane-message"});
    let first = mcp(&rig, "fabric_mail", args.clone())?;
    assert_ne!(first["isError"], true, "{first}");
    assert_eq!(first, mcp(&rig, "fabric_mail", args)?);
    let mail = host(&rig, &["fabric", "mail", "read", "--after", "0", "--json"])?;
    let items = mail["data"]["page"]["items"].as_array().ok_or("mail")?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["message"]["sender"], json!(actor));
    // The host fixture stands in for a remote owner. Its bound lane actor has
    // an empty inbox; the local lane label has one message. Both command edges
    // must observe the owner through the relay rather than the local mirror.
    let key = provenant_fabric_runtime::identity::CrossHostProjectKey::from_home(
        &rig.home,
        &rig.project.join(".git"),
    )?;
    let mut config: toml_edit::DocumentMut =
        std::fs::read_to_string(rig.state.join("config.toml"))?.parse()?;
    let remote = provenant_core::HostId::from_bytes([77; 16]);
    config["domains"]["fabric"]["home_hosts"][key.as_str()] = toml_edit::value(remote.to_string());
    config["domains"]["fabric"]["peers"]["fixture"]["host"] = toml_edit::value(remote.to_string());
    config["domains"]["fabric"]["peers"]["fixture"]["destination"] = toml_edit::value("fixture");
    std::fs::write(rig.state.join("config.toml"), config.to_string())?;
    let remote_mail = mcp(
        &rig,
        "fabric_mail",
        json!({"op":"read","after":0,"limit":10}),
    )?;
    assert_ne!(remote_mail["isError"], true, "{remote_mail}");
    assert!(
        remote_mail["structuredContent"]["page"]["items"]
            .as_array()
            .ok_or("remote mail")?
            .is_empty()
    );
    let remote_mail = host(&rig, &["fabric", "mail", "read", "--after", "0", "--json"])?;
    assert!(
        remote_mail["data"]["page"]["items"]
            .as_array()
            .ok_or("remote CLI mail")?
            .is_empty()
    );
    // A CLI wait above the MCP cap still reaches the owner. Generation zero
    // advances immediately when no landing exists, keeping this check fast.
    let waited = host(
        &rig,
        &[
            "fabric",
            "landing",
            "wait",
            "--reference",
            "refs/heads/main",
            "--generation",
            "0",
            "--wait",
            "300",
            "--json",
        ],
    )?;
    assert_eq!(waited["data"]["timed_out"], false);
    drop(relay);
    assert!(!socket.exists());
    assert!(!socket.parent().ok_or("socket parent")?.exists());
    let closed = mcp(
        &rig,
        "fabric_mail",
        json!({"op":"send","recipients":[actor],"kind":"note","body":"closed"}),
    )?;
    assert_eq!(closed["isError"], true);
    Ok(())
}
