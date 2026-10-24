//! The real Rust command edge runs without Python, Node or ambient state.
mod support;
use serde_json::{Value, json};
use std::{ffi::OsString, path::Path};
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
    assert_eq!(run["isError"], true);
    assert_eq!(run["structuredContent"]["code"], "owner_unavailable");
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
        ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, ProjectId, RootInputs, RunId,
        TaskId, WaitBudget,
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
    let attempt = AttemptId::from_bytes([91; 16]);
    let task = TaskId::from_bytes([92; 16]);
    store.create(
        &provenant_run::PreparedRun {
            run_id: RunId::from_bytes([93; 16]),
            operation_id: OperationId::parse("edge-read")?,
            created_at: clock.wall_now()?,
            request: CanonicalRequest {
                schema_version: 1,
                tasks: vec![TaskRequest {
                    writer: None,
                    task_id: task,
                    prompt_digest: CanonicalV1::file_digest(b"prompt"),
                    route: RequestedRoute::Model {
                        provider: "fixture".into(),
                        model: "fixture".into(),
                        effort: None,
                    },
                    fallback: vec![],
                    authority_digest: Digest::from_bytes([94; 32]),
                    provider_timeout_seconds: 60,
                    delivery_id: None,
                }],
                concurrency: 1,
                parent: None,
            },
            attempts: vec![provenant_run::NewAttempt {
                attempt_id: attempt,
                task_id: task,
                prompt: "prompt".into(),
            }],
        },
        &call,
    )?;
    drop(store);
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
