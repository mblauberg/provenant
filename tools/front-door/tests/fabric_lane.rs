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

#[test]
fn lane_run_submits_child_through_parent_relay() -> Result {
    use provenant_fabric_runtime::graph::relay::RelayOp;
    let mut rig = TestRig::new()?;
    host(&rig, &["fabric", "identity", "--json"])?;
    let attempt = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
    let (sent, received) = std::sync::mpsc::channel();
    let relay = match provenant_fabric_mcp::lane::HostRelay::start_with_graph(
        provenant_providers::LaneRuntime::fabric_socket(attempt),
        provenant_records::Limits::CONFIG,
        |_, _, _| {
            Err(provenant_fabric_mcp::tools::ServiceError::Cli {
                code: "unexpected_coordination".into(),
                message: "expected a graph frame".into(),
                data: Value::Null,
            })
        },
        move |arguments, _, _| {
            sent.send(arguments)
                .map_err(|_| provenant_fabric_mcp::tools::ServiceError::Cli {
                    code: "fixture_closed".into(),
                    message: "fixture receiver closed".into(),
                    data: Value::Null,
                })?;
            Ok(json!({"parent_attempt":attempt.to_string(),"revision":2}))
        },
    ) {
        Ok(relay) => relay,
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && provenant_cli::fabric::in_lane() =>
        {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "relay socket unavailable in lane: {error}"
            ))?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    lane_environment(&mut rig, attempt)?;
    let launched = host(
        &rig,
        &[
            "run",
            "Inspect the child",
            "--use",
            "explore",
            "--tier",
            "junior",
            "--json",
        ],
    )?;
    assert_eq!(launched["data"]["parent_attempt"], attempt.to_string());
    let frame = received.recv_timeout(std::time::Duration::from_secs(2))?;
    let RelayOp::Add(child) = serde_json::from_value(frame)? else {
        return Err("expected child admission".into());
    };
    assert_eq!(child.template, "explore");
    assert_eq!(child.brief, "Inspect the child");
    assert_eq!(child.tier.as_deref(), Some("junior"));
    assert!(!child.detached);
    host(
        &rig,
        &[
            "run",
            "Inspect the child",
            "--use",
            "explore",
            "--tier",
            "junior",
            "--json",
        ],
    )?;
    let frame = received.recv_timeout(std::time::Duration::from_secs(2))?;
    let RelayOp::Add(repeated) = serde_json::from_value(frame)? else {
        return Err("expected repeated child admission".into());
    };
    assert_eq!(repeated.brief, child.brief);
    assert_ne!(repeated.id, child.id);
    assert_ne!(repeated.idempotency_key, child.idempotency_key);
    // The legacy CLI recognised the parent marker on its own. Keep that
    // confinement boundary even when the other lane markers are absent.
    rig.env.retain(|(key, _)| {
        key != provenant_core::RUN_STATE_DIRECTORY_ENV && key != provenant_run::LANE_DEPTH_ENV
    });
    let launched = host(&rig, &["run", "Parent marker only", "--json"])?;
    assert_eq!(launched["data"]["parent_attempt"], attempt.to_string());
    let frame = received.recv_timeout(std::time::Duration::from_secs(2))?;
    let RelayOp::Add(child) = serde_json::from_value(frame)? else {
        return Err("expected child admission from parent marker".into());
    };
    assert_eq!(child.brief, "Parent marker only");
    assert_eq!(child.template, "task");
    let stopped = host(&rig, &["stop", "parent--child", "--json"])?;
    assert_eq!(stopped["data"]["parent_attempt"], attempt.to_string());
    let frame = received.recv_timeout(std::time::Duration::from_secs(2))?;
    let RelayOp::Stop { node } = serde_json::from_value(frame)? else {
        return Err("expected subtree stop through parent relay".into());
    };
    assert_eq!(node, "parent--child");
    // Relay admission and control must not write a second run store in the lane.
    let child_store = rig.state.join("children").join(attempt.to_string());
    assert_eq!(std::fs::read_dir(child_store)?.count(), 0);
    drop(relay);
    Ok(())
}

#[test]
fn lane_edit_admits_revision_through_host_relay() -> Result {
    use provenant_core::{Clock, WaitBudget};
    use provenant_fabric_runtime::graph::{
        FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
    };
    let mut rig = TestRig::new()?;
    let explanation = host(&rig, &["run", "Initial", "--explain", "--json"])?;
    let document: GraphDocument = serde_json::from_value(explanation["data"]["document"].clone())?;
    let graph_id = document.graph_id.clone();
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let context = provenant_cli::fabric::Context::open(roots, &rig.home, &rig.project)?;
    let attempt = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
    let reference = format!("{}/{attempt}", context.identity.host);
    let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":1,"graph_id":graph_id,"plan_digest":"test",
        "nodes":[{"id":"task","worktree":null,"state":"running","attempts":[{
            "key":{"graph_id":graph_id,"node_id":"task","route_index":0},
            "launched":true,"route":{"alias":"senior"},"state":"running",
            "attempt_ref":reference,"launch_actor":"actor"
        }]}]
    }))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 55)?;
    let mut journal = FileJournal::open(
        &context.roots.state,
        context.identity.project,
        &graph_id,
        budget,
        &cancel,
        &clock,
    )?;
    journal.bind(GraphExecution {
        extra: Default::default(),
        directory: rig.project.clone(),
        document,
        review_base: None,
        integration_base: None,
    })?;
    journal.save(&checkpoint)?;
    drop(journal);
    let state = context.roots.state.clone();
    let project = context.identity.project;
    let patch = json!({"schema_version":1,"expected_revision":0,"idempotency_key":"direct-edit","nodes":[{"id":"direct","use":"task","brief":"Direct"}]});
    let request = json!({"op":"edit","id":graph_id,"patch":patch});
    let result = provenant_cli::fabric::graph_relay::serve(
        &context,
        &attempt.to_string(),
        request.clone(),
        budget,
        &cancel,
        &clock,
    )?;
    assert_eq!(result["revision"], 1);
    let replay = provenant_cli::fabric::graph_relay::serve(
        &context,
        &attempt.to_string(),
        request,
        budget,
        &cancel,
        &clock,
    )?;
    assert_eq!(replay["replayed"], true);
    let saved = FileJournal::read_status_checkpoint(&state, project, &graph_id)?
        .ok_or("missing checkpoint")?;
    assert!(saved.nodes.iter().any(|node| node.id == "task--direct"));
    let events = FileJournal::read_events(&state, project, &graph_id)?;
    assert!(
        events
            .iter()
            .any(|event| event.kind == provenant_fabric_runtime::graph::GraphEventKind::Patch)
    );
    let relay = provenant_fabric_mcp::lane::HostRelay::start_with_graph(
        provenant_providers::LaneRuntime::fabric_socket(attempt),
        provenant_records::Limits::CONFIG,
        |_, _, _| {
            Err(provenant_fabric_mcp::tools::ServiceError::Cli {
                code: "unexpected".into(),
                message: "unexpected coordination".into(),
                data: Value::Null,
            })
        },
        move |arguments, cancel, within| {
            let budget = provenant_fabric_mcp::lane::host_budget(&clock, within)?;
            provenant_cli::fabric::graph_relay::serve(
                &context,
                &attempt.to_string(),
                arguments,
                budget,
                cancel,
                &clock,
            )
        },
    );
    let relay = match relay {
        Ok(relay) => relay,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "native lane socket unavailable: {error}"
            ))?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    lane_environment(&mut rig, attempt)?;
    let path = rig.root.join("patch.json");
    std::fs::write(
        &path,
        serde_json::to_vec(
            &json!({"schema_version":1,"expected_revision":1,"idempotency_key":"live-edit","nodes":[{"id":"next","use":"task","brief":"Next"}]}),
        )?,
    )?;
    let path = path.to_str().ok_or("patch path")?;
    let result = host(&rig, &["edit", &graph_id, "--file", path, "--json"])?;
    assert_eq!(result["data"]["revision"], 2);
    let replay = host(&rig, &["edit", &graph_id, "--file", path, "--json"])?;
    assert_eq!(replay["data"]["replayed"], true);
    let saved = FileJournal::read_status_checkpoint(&state, project, &graph_id)?
        .ok_or("missing checkpoint")?;
    assert!(saved.nodes.iter().any(|node| node.id == "task--next"));
    let events = FileJournal::read_events(&state, project, &graph_id)?;
    assert!(
        events
            .iter()
            .any(|event| event.kind == provenant_fabric_runtime::graph::GraphEventKind::Patch)
    );
    drop(relay);
    Ok(())
}

#[test]
fn lane_cli_and_mcp_compile_rich_child_launches_without_local_submission() -> Result {
    let mut rig = TestRig::new()?;
    host(&rig, &["fabric", "identity", "--json"])?;
    host(&rig, &["fabric", "task", "list", "--json"])?;
    lane_environment(&mut rig, AttemptId::from_bytes([78; 16]))?;
    let preview = host(
        &rig,
        &[
            "run",
            "Fix ${item}",
            "--use",
            "implement",
            "--over",
            "one,two",
            "--detached",
            "--explain",
            "--json",
        ],
    )?;
    assert_eq!(preview["data"]["detached"], true);
    let children = preview["data"]["launch"]["node"]["composite"]["nodes"]
        .as_array()
        .ok_or("CLI compiled fanout")?;
    assert_eq!(children.len(), 2);
    assert!(
        children
            .iter()
            .all(|child| child["access"] == "worktree_write")
    );
    let arguments = json!({"op":"run", "document":{
        "parameters":{"subject":{"type":"string"}},
        "brief":"Inspect ${subject}", "tier":"junior", "access":"read_only",
        "detached":true, "idempotency_key":"rich-mcp-preview"
    }, "set":{"subject":"compiled MCP"}, "explain":true});
    let preview = mcp(&rig, "fabric_run", arguments.clone())?;
    assert_ne!(preview["isError"], true, "{preview}");
    let value = &preview["structuredContent"];
    assert_eq!(value["launch"]["node"]["prompt"], "Inspect compiled MCP");
    assert_eq!(value["detached"], true);
    let repeated = mcp(&rig, "fabric_run", arguments)?;
    assert_eq!(repeated["structuredContent"], *value);
    Ok(())
}

#[test]
fn lane_cli_and_mcp_share_wait_receipts_and_observation_errors() -> Result {
    use provenant_fabric_runtime::graph::relay::RelayOp;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let mut rig = TestRig::new()?;
    host(&rig, &["fabric", "task", "list", "--json"])?;
    let attempt = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
    let fail_observation = Arc::new(AtomicBool::new(false));
    let failure = Arc::clone(&fail_observation);
    let _relay = provenant_fabric_mcp::lane::HostRelay::start_with_graph(
        provenant_providers::LaneRuntime::fabric_socket(attempt),
        provenant_records::Limits::CONFIG,
        |_, _, _| Err(provenant_fabric_mcp::tools::ServiceError::Cli {
            code: "unexpected_coordination".into(), message: "expected graph request".into(), data: Value::Null,
        }),
        move |arguments, _, _| {
            match serde_json::from_value::<RelayOp>(arguments)? {
                RelayOp::Add(child) => {
                    assert_eq!(child.brief, "Observe child");
                    Ok(json!({"parent_attempt":attempt.to_string(),"revision":2}))
                }
                RelayOp::Wait { .. } => Ok(json!({"waiting":true})),
                RelayOp::Children if failure.load(Ordering::SeqCst) => Err(provenant_fabric_mcp::tools::ServiceError::Cli {
                    code:"observation_fixture".into(), message:"scripted observation failure".into(), data:Value::Null,
                }),
                RelayOp::Children => Ok(json!({"children":[{"node":"parent--child","settled":true}]})),
                _ => panic!("unexpected graph operation"),
            }
        },
    )?;
    lane_environment(&mut rig, attempt)?;
    for fail in [false, true] {
        fail_observation.store(fail, Ordering::SeqCst);
        let cli = host(&rig, &["run", "Observe child", "--wait=1s", "--json"])?;
        let mcp = mcp(&rig, "fabric_run", json!({"op":"run","brief":"Observe child","wait_seconds":1}))?;
        assert_ne!(mcp["isError"], true, "{mcp}");
        let value = &cli["data"];
        assert_eq!(*value, mcp["structuredContent"]);
        assert_eq!(value["receipt"]["revision"], 2);
        if fail {
            assert!(value["observation_error"].as_str().ok_or("observation diagnostic")?.contains("observation_fixture"));
            assert!(value.get("children").is_none());
        } else {
            assert_eq!(value["children"]["children"][0]["settled"], true);
            assert!(value.get("observation_error").is_none());
        }
    }
    Ok(())
}
