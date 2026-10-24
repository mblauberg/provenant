//! The real Rust command edge runs without Python, Node or ambient state.
mod support;
use serde_json::{Value, json};
use std::{ffi::OsString, os::unix::fs::DirBuilderExt, path::Path};
use support::{Result, TestRig, code};

fn mcp(rig: &TestRig, frames: &[Value]) -> Result<Vec<Value>> {
    let mut input = Vec::new();
    if frames
        .first()
        .is_some_and(|frame| frame["method"] != "initialize")
    {
        input.extend(serde_json::to_vec(&json!({"jsonrpc":"2.0","id":9999,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}))?);
        input.push(b'\n');
        input.extend(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n");
    }
    for frame in frames {
        input.extend(serde_json::to_vec(frame)?);
        input.push(b'\n');
    }
    let args: Vec<OsString> = vec![
        "--state-dir".into(),
        rig.state.clone().into(),
        "fabric".into(),
        "mcp".into(),
        "--client".into(),
        "codex".into(),
    ];
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &args,
        Some(&input),
    )?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let replies = output
        .stdout
        .bytes
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
        .map(serde_json::from_slice)
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    Ok(replies)
}
#[test]
fn rust_mcp_negotiates_lists_tools_and_persists_mail_without_interpreters() -> Result {
    let mut rig = TestRig::new()?;
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = "/nonexistent".into();
        }
    }
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fabric_identity","arguments":{"op":"inspect"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fabric_mail","arguments":{"op":"send","recipients":["11111111111111111111111111111111"],"kind":"note","body":"Rust transport","artifacts":[],"reply_to":null,"task":null}}}),
        ],
    )?;
    assert_eq!(
        replies.iter().find(|r| r["id"] == 1).ok_or("initialise")?["result"]["protocolVersion"],
        "2025-11-25"
    );
    let list = &replies.iter().find(|r| r["id"] == 2).ok_or("catalogue")?["result"]["tools"];
    assert_eq!(list.as_array().ok_or("tools")?.len(), 11);
    let identity =
        &replies.iter().find(|r| r["id"] == 3).ok_or("identity")?["result"]["structuredContent"];
    assert_eq!(identity["client"], "codex");
    assert_eq!(identity["inbox"]["agent"], "fixture");
    let sent = &replies.iter().find(|r| r["id"] == 4).ok_or("mail")?["result"];
    assert_ne!(sent["isError"], true, "{sent}");
    assert!(sent["structuredContent"]["id"].is_string());
    let events = rig.command(&["fabric", "events", "read", "--after", "0", "--json"])?;
    assert_eq!(code(&events), 0);
    assert!(String::from_utf8(events.stdout.bytes)?.contains("mail_sent"));
    rig.assert_legacy_unchanged()
}
#[test]
fn peer_ingress_rejects_wrong_host_before_coordination_mutation() -> Result {
    let rig = TestRig::new()?;
    let input = json!({"version":1,"target_host":"11111111111111111111111111111111","source_host":"22222222222222222222222222222222","agent":"fixture","project":{"key":"not-a-project","common_dir":"../escape"},"action":{"action":"health"}});
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["fabric".into(), "peer".into()],
        Some(&serde_json::to_vec(&input)?),
    )?;
    assert_eq!(code(&output), 3);
    assert!(output.stdout.bytes.is_empty());
    assert!(!rig.state.join("fabric/fabric.sqlite3").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_replays_a_home_mutation_once_with_the_original_actor() -> Result {
    let mut rig = TestRig::new()?;
    let moved = rig.home.join("repo");
    std::fs::rename(&rig.project, &moved)?;
    rig.project = moved;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = rig.project.clone().into();
        }
    }
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let host = &identity["data"]["host"];
    let key = &identity["data"]["federation"]["key"];
    assert!(key.is_string());
    let request = json!({"version":1,"target_host":host,"source_host":host,"agent":"worker","project":{"key":key,"common_dir":"repo/.git"},"action":{"action":"coordination","input":{"domain":"events","input":{"op":"note","note":"remote note","dedup_key":null}},"operation_id":"note-once"}});
    let input = serde_json::to_vec(&request)?;
    let args: Vec<OsString> = vec![
        "--state-dir".into(),
        rig.state.clone().into(),
        "fabric".into(),
        "peer".into(),
    ];
    let binary = Path::new(env!("CARGO_BIN_EXE_provenant"));
    let first = rig.execute_os(binary, &args, Some(&input))?;
    assert_eq!(
        code(&first),
        0,
        "{}",
        String::from_utf8_lossy(&first.stderr.bytes)
    );
    let second = rig.execute_os(binary, &args, Some(&input))?;
    assert_eq!(first.stdout.bytes, second.stdout.bytes);
    let response: Value = serde_json::from_slice(&first.stdout.bytes)?;
    assert_eq!(response["result"]["result"], "coordination", "{response}");
    let events = rig.command(&["fabric", "events", "read", "--after", "0", "--json"])?;
    let events: Value = serde_json::from_slice(&events.stdout.bytes)?;
    let items = events["data"]["page"]["items"].as_array().ok_or("events")?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["record"]["detail"], "remote note");
    rig.assert_legacy_unchanged()
}

#[test]
fn mcp_client_default_selects_its_own_inbox_and_run_binding_is_honest() -> Result {
    let mut rig = TestRig::new()?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_identity","arguments":{"op":"inspect"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"list","limit":10}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fabric_events","arguments":{"op":"read","limit":10,"after":0,"wait_ms":null}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fabric_events","arguments":{"op":"read","limit":10,"after":0,"wait_ms":1000}}}),
        ],
    )?;
    let identity =
        &replies.iter().find(|r| r["id"] == 1).ok_or("identity")?["result"]["structuredContent"];
    assert_eq!(identity["inbox"]["agent"], "codex");
    let run = &replies.iter().find(|r| r["id"] == 2).ok_or("run")?["result"];
    assert_ne!(run["isError"], true, "{run}");
    assert_eq!(run["structuredContent"]["page"]["items"], json!([]));
    let events =
        &replies.iter().find(|r| r["id"] == 3).ok_or("events")?["result"]["structuredContent"];
    assert_eq!(events["unknown_hosts"], json!([]));
    assert_eq!(events["run_events_pending"], false);
    let waited =
        &replies.iter().find(|r| r["id"] == 4).ok_or("event wait")?["result"]["structuredContent"];
    assert_eq!(waited["timed_out"], true);
    assert_eq!(waited["cursor"], 0);
    rig.assert_legacy_unchanged()
}

#[test]
fn graph_run_falls_back_across_routes_refused_before_launch() -> Result {
    let rig = TestRig::new()?;
    std::fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"edge","nodes":[{"id":"one","prompt":"work","access":"read_only","routes":[{"adapter":"fixture","model":"one"},{"adapter":"fixture","model":"two"}]}]}),
        )?,
    )?;
    let run = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&run),
        0,
        "{}",
        String::from_utf8_lossy(&run.stderr.bytes)
    );
    let run: Value = serde_json::from_slice(&run.stdout.bytes)?;
    assert_eq!(run["data"]["status"], "complete");
    let node = &run["data"]["checkpoint"]["nodes"][0];
    assert_eq!(node["state"], "failed");
    // Neither fixture route is configured, so each is refused before launch and
    // the graph moves on to the next route rather than claiming a task failure.
    let attempts = node["attempts"].as_array().ok_or("attempts")?;
    assert_eq!(attempts.len(), 2);
    for attempt in attempts {
        assert_eq!(attempt["state"], "terminal");
        assert_eq!(attempt["launched"], false);
        assert_eq!(attempt["outcome"], "adapter_unavailable");
        assert!(attempt["attempt_ref"].as_str().is_some());
    }
    // Every harness reads the same persisted checkpoint.
    let frames = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":"status","method":"tools/call","params":{"name":"fabric_graph","arguments":{"op":"status","id":"edge"}}}),
            json!({"jsonrpc":"2.0","id":"resume","method":"tools/call","params":{"name":"fabric_graph","arguments":{"op":"resume","document":"graph.json"}}}),
        ],
    )?;
    for op in ["status", "resume"] {
        let result = &frames
            .iter()
            .find(|frame| frame["id"] == op)
            .ok_or("graph reply")?["result"];
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(result["structuredContent"]["status"], "complete");
        assert_eq!(
            result["structuredContent"]["checkpoint"]["nodes"][0]["attempts"],
            node["attempts"]
        );
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn run_status_and_wait_read_the_owner_store_and_event_pull_deduplicates() -> Result {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, HostId, OperationId, ProjectId, RootInputs,
        RunId, TaskId, WaitBudget,
    };
    use provenant_records::{
        CanonicalV1,
        run::{CanonicalRequest, RequestedRoute, TaskRequest},
    };
    use provenant_supervisor::{clock::SystemClock, fs::LocalFs};
    let rig = TestRig::new()?;
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let actor = ActorId::parse(identity["data"]["inbox"]["actor"].as_str().ok_or("actor")?)?;
    let mut roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let state = roots.resolve(&LocalFs)?.state;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 10)?,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: state,
        project_id: project,
        actor_id: actor,
        read_prompts: false,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    let tasks = [
        TaskId::from_bytes([92; 16]),
        TaskId::from_bytes([93; 16]),
        TaskId::from_bytes([94; 16]),
    ];
    let attempts = [
        AttemptId::from_bytes([91; 16]),
        AttemptId::from_bytes([95; 16]),
        AttemptId::from_bytes([97; 16]),
    ];
    let attempt = attempts[0];
    store.create(
        &provenant_run::PreparedRun {
            run_id: RunId::from_bytes([96; 16]),
            operation_id: OperationId::parse("edge-read")?,
            created_at: clock.wall_now()?,
            request: CanonicalRequest {
                schema_version: 1,
                tasks: tasks
                    .iter()
                    .map(|task_id| TaskRequest {
                        writer: None,
                        profile: provenant_records::run::HarnessProfile::default(),
                        task_id: *task_id,
                        prompt_digest: CanonicalV1::file_digest(b"prompt"),
                        route: RequestedRoute::Model {
                            provider: "fixture".into(),
                            model: "fixture".into(),
                            effort: None,
                        },
                        account: None,
                        fallback: vec![],
                        authority_digest: Digest::from_bytes([94; 32]),
                        provider_timeout_seconds: 60,
                        delivery_id: None,
                    })
                    .collect(),
                concurrency: 1,
                parent: None,
                lane_depth: Some(1),
                parent_attempt: None,
            },
            attempts: tasks
                .iter()
                .zip(attempts)
                .map(|(task_id, attempt_id)| provenant_run::NewAttempt {
                    attempt_id,
                    task_id: *task_id,
                    prompt: "prompt".into(),
                })
                .collect(),
        },
        &call,
    )?;
    drop(store);
    let first = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"list","limit":1}}}),
        ],
    )?;
    let first = &first
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("first page")?["result"]["structuredContent"]["page"];
    assert_eq!(first["items"][0]["attempt_id"], attempts[0].to_string());
    assert_eq!(first["truncated"], true);
    let cursor = first["next_after"].clone();
    let local_host = HostId::parse(cursor["host"].as_str().ok_or("cursor host")?)?;
    let mut remote_host_bytes = *local_host.as_bytes();
    remote_host_bytes[0] ^= 1;
    let mut remote_cursor = cursor.clone();
    remote_cursor["host"] = json!(HostId::from_bytes(remote_host_bytes).to_string());
    let remote = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"list","after":remote_cursor,"limit":1}}}),
        ],
    )?;
    let remote = &remote
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("remote cursor")?["result"];
    assert_eq!(remote["isError"], true);
    assert_eq!(remote["structuredContent"]["code"], "owner_unavailable");
    let second = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"list","after":cursor,"limit":1}}}),
        ],
    )?;
    let second = &second
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("second page")?["result"]["structuredContent"]["page"];
    assert_eq!(second["items"][0]["attempt_id"], attempts[1].to_string());
    assert_eq!(second["truncated"], true);
    let cursor = second["next_after"].clone();
    let third = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"list","after":cursor,"limit":1}}}),
        ],
    )?;
    let third = &third
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("third page")?["result"]["structuredContent"]["page"];
    assert_eq!(third["items"][0]["attempt_id"], attempts[2].to_string());
    assert_eq!(third["next_after"], Value::Null);
    let target = json!({"host":null,"attempt":attempt.to_string()});
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"status","target":target}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"wait","targets":[target],"condition":"all","wait_ms":1000}}}),
        ],
    )?;
    let status =
        &replies.iter().find(|r| r["id"] == 1).ok_or("status")?["result"]["structuredContent"];
    assert_eq!(status["attempt"]["attempt_id"], attempt.to_string());
    let wait = &replies.iter().find(|r| r["id"] == 2).ok_or("wait")?["result"]["structuredContent"];
    assert_eq!(wait["timed_out"], true);
    assert_eq!(wait["satisfied"], false);
    let first = rig.command(&["fabric", "events", "read", "--after", "0", "--json"])?;
    let second = rig.command(&["fabric", "events", "read", "--after", "0", "--json"])?;
    assert_eq!(code(&first), 0);
    assert_eq!(code(&second), 0);
    let first: Value = serde_json::from_slice(&first.stdout.bytes)?;
    let second: Value = serde_json::from_slice(&second.stdout.bytes)?;
    assert_eq!(
        first["data"]["page"]["items"],
        second["data"]["page"]["items"]
    );
    assert!(
        first["data"]["page"]["items"]
            .as_array()
            .ok_or("events")?
            .iter()
            .any(|event| event["record"]["kind"] == "run_changed")
    );
    // A cancel operation is bound to its target: a replay is answered, the
    // same ID for another target is a conflict.
    let other = json!({"host":null,"attempt":"00000000000000000000000000000001"});
    let cancel = |id: u64, target: &Value| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"cancel","target":target,"operation_id":"stop"}}});
    let replies = mcp(
        &rig,
        &[cancel(1, &target), cancel(2, &target), cancel(3, &other)],
    )?;
    for id in [1, 2] {
        let reply = &replies.iter().find(|r| r["id"] == id).ok_or("cancel")?["result"];
        assert_ne!(reply["isError"], true, "{reply}");
        assert_eq!(
            reply["structuredContent"]["attempt"]["state"]["outcome"], "cancelled",
            "{reply}"
        );
    }
    let conflict = &replies.iter().find(|r| r["id"] == 3).ok_or("conflict")?["result"];
    assert_eq!(
        conflict["structuredContent"]["code"], "conflict",
        "{conflict}"
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn mcp_refuses_nested_submission_above_max_depth_before_placement() -> Result {
    let mut rig = TestRig::new()?;
    // The host binds the project before a read-only lane looks it up.
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    rig.env.extend([
        (provenant_run::LANE_DEPTH_ENV.into(), "2".into()),
        (
            provenant_run::PARENT_ATTEMPT_ENV.into(),
            "11111111111111111111111111111111".into(),
        ),
    ]);
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_submit","arguments":{"op":"submission","operation_id":"nested-depth","tasks":[{"prompt":"read only","route":{"adapter":"codex","model":"gpt-6.1-sol"},"timeout_ms":1000}],"concurrency":1}}}),
        ],
    )?;
    let result = &replies.iter().find(|r| r["id"] == 1).ok_or("submission")?["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["code"], "lane_depth",
        "{result}"
    );
    assert!(!rig.state.join("runs").exists());
    rig.assert_legacy_unchanged()
}

// Providers remain disabled. Submission still seals policy and admission tickets,
// so this fixture exercises the real Fabric-to-Run path without a provider run.
fn submitted_policy(
    rig: &TestRig,
    operation: &str,
    concurrency: Option<u64>,
    knobs: &[&str],
    route: Value,
) -> Result<(provenant_records::run::CanonicalRequest, Value)> {
    let task = json!({"prompt":"fixture", "route":route,"timeout_ms":60000});
    let mut arguments = json!({"op":"submission","operation_id":operation,"tasks":[task.clone(),task.clone(),task.clone(),task],"knobs":knobs});
    if let Some(n) = concurrency {
        arguments["concurrency"] = n.into();
    }
    let replies = mcp(
        rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_submit","arguments":arguments}}),
        ],
    )?;
    let result = &replies.iter().find(|r| r["id"] == 1).ok_or("submission")?["result"];
    assert_ne!(result["isError"], true, "{result}");
    let target = result["structuredContent"]["targets"][0].clone();
    let replies = mcp(
        rig,
        &[
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"status","target":target}}}),
        ],
    )?;
    let attempt = replies.iter().find(|r| r["id"] == 2).ok_or("status")?["result"]["structuredContent"]["attempt"].clone();
    let run = attempt["run_id"].as_str().ok_or("run ID")?;
    let request = serde_json::from_slice(&std::fs::read(
        rig.state.join("runs").join(run).join("request.json"),
    )?)?;
    Ok((request, attempt))
}

#[test]
fn submission_preserves_explicit_named_route_tier() -> Result {
    let rig = TestRig::new()?;
    let (request, _) = submitted_policy(
        &rig,
        "named-tier",
        None,
        &[],
        json!({"route":"senior","task_class":"mechanical"}),
    )?;
    assert_eq!(
        request.tasks[0].route,
        provenant_records::run::RequestedRoute::Named {
            name: "senior".into(),
            effort: None,
        }
    );
    assert_eq!(
        request.tasks[0].profile.task_class.as_deref(),
        Some("mechanical")
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn submission_does_not_advertise_unsupported_inputs() -> Result {
    let rig = TestRig::new()?;
    let replies = mcp(
        &rig,
        &[json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})],
    )?;
    let tools = replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("tools")?;
    let submit = tools["result"]["tools"]
        .as_array()
        .ok_or("tool list")?
        .iter()
        .find(|tool| tool["name"] == "fabric_submit")
        .ok_or("fabric_submit")?;
    assert!(!submit["inputSchema"].to_string().contains("inputs"));

    let reply = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_submit","arguments":{"op":"submission","operation_id":"unsupported-inputs","tasks":[{"prompt":"fixture","inputs":[],"route":{"route":"senior"},"timeout_ms":60000}]}}}),
        ],
    )?;
    let call = reply.iter().find(|reply| reply["id"] == 2).ok_or("call")?;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert_eq!(call["result"]["structuredContent"]["code"], "invalid_input");
    rig.assert_legacy_unchanged()
}

#[test]
fn concurrency_knob_controls_admission_unless_dispatch_is_explicit() -> Result {
    let rig = TestRig::new()?;
    std::fs::DirBuilder::new().mode(0o700).create(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\nprofile='rigorous'\n[knobs]\n'lanes.max_concurrency'=3\n",
    )?;
    for (operation, explicit, knobs, expected) in [
        ("config-capacity", None, vec![], 3),
        (
            "dispatch-capacity",
            None,
            vec![
                "lanes.max_concurrency=2",
                "effort.shift=-1",
                "routing.pool_mode=rotate",
            ],
            2,
        ),
        (
            "explicit-capacity",
            Some(1),
            vec!["lanes.max_concurrency=2"],
            1,
        ),
    ] {
        let (request, attempt) = submitted_policy(
            &rig,
            operation,
            explicit,
            &knobs,
            json!({"model":"codex/fixture","task_class":"mechanical"}),
        )?;
        assert_eq!(request.concurrency, expected);
        let task = &request.tasks[0];
        assert_eq!(serde_json::to_value(&task.profile)?, attempt["profile"]);
        assert_eq!(task.profile.overrides, knobs);
        assert_eq!(task.profile.task_class.as_deref(), Some("mechanical"));
        assert_eq!(
            task.profile.knobs["effort.shift"],
            if operation == "dispatch-capacity" {
                -1
            } else {
                0
            }
        );
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn silence_knob_controls_capture_plan_observations() -> Result {
    let rig = TestRig::new()?;
    std::fs::DirBuilder::new().mode(0o700).create(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[knobs]\n'lanes.silence_seconds'=2\n",
    )?;
    for (operation, knobs, threshold) in [
        ("config-silence", vec![], 2000),
        ("dispatch-silence", vec!["lanes.silence_seconds=1"], 1000),
    ] {
        let (request, _) = submitted_policy(
            &rig,
            operation,
            None,
            &knobs,
            json!({"model":"codex/fixture","task_class":"mechanical"}),
        )?;
        let plan = provenant_run::capture_plan(&request.tasks[0]);
        assert_eq!(plan.silence_ms, threshold);
        assert!(!plan.silent(threshold - 1, Some(0)));
        assert!(plan.silent(threshold, Some(0)));
        assert!(!plan.silent(threshold, Some(threshold)));
    }
    rig.assert_legacy_unchanged()
}
