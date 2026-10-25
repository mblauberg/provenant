//! The real Rust command edge runs without Python, Node or ambient state.
mod support;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
};
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

fn federated_identity(rig: &mut TestRig) -> Result<Value> {
    let moved = rig.home.join("repo");
    std::fs::rename(&rig.project, &moved)?;
    rig.project = moved;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = rig.project.clone().into();
        }
    }
    let output = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&output), 0);
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

fn next_host(host: provenant_core::HostId) -> Result<provenant_core::HostId> {
    let mut bytes = *host.as_bytes();
    for index in (0..bytes.len()).rev() {
        if bytes[index] < u8::MAX {
            bytes[index] += 1;
            bytes[index + 1..].fill(0);
            return Ok(provenant_core::HostId::from_bytes(bytes));
        }
    }
    Err("test host ID has no successor".into())
}

#[test]
fn hosts_probe_only_the_requested_page() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let local = provenant_core::HostId::parse(identity["data"]["host"].as_str().ok_or("host")?)?;
    let project_key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let first = next_host(local)?;
    let second = next_host(first)?;
    let ssh = rig.root.join("ssh");
    let calls = rig.root.join("ssh-calls");
    let response = rig.root.join("ssh-response.json");
    std::fs::write(
        &response,
        serde_json::to_vec(&json!({
            "version": 1,
            "host": first.to_string(),
            "project": project_key,
            "result": {"result":"health","data":{
                "host":first.to_string(),"available":true,"execution":true
            }}
        }))?,
    )?;
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nrequest=$(cat)\nprintf '%s\\n' \"$request\" >> '{}'\ncat '{}'\n",
            calls.display(),
            response.display()
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric]\nssh_executable={:?}\n\
             [domains.fabric.peers.first]\nhost='{}'\ndestination='127.0.0.1'\n\
             [domains.fabric.peers.second]\nhost='{}'\ndestination='127.0.0.1'\n",
            ssh.to_string_lossy(),
            first,
            second
        ),
    )?;
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"fabric_hosts","arguments":{"op":"list","after":local.to_string(),"limit":1}
            }}),
        ],
    )?;
    let hosts = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("hosts")?["result"]["structuredContent"];
    assert_eq!(hosts.as_array().ok_or("host list")?.len(), 1);
    assert_eq!(hosts[0]["host"], first.to_string());
    assert_eq!(hosts[0]["available"], true);
    let calls = std::fs::read_to_string(calls)?;
    let calls = calls
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["target_host"], first.to_string());
    rig.assert_legacy_unchanged()
}

#[test]
fn remote_landing_push_preserves_definitive_store_refusal() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let request = json!({
        "version": 1,
        "target_host": host,
        "source_host": host,
        "agent": "fixture",
        "project": {"key": key, "common_dir": "repo/.git"},
        "action": {"action":"landing_push","proof":{
            "reference":"refs/heads/main",
            "id":"11111111111111111111111111111111",
            "generation":1,
            "token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]
        },"operation_id":"landing-refusal","reconcile":false}
    });
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
        ],
        Some(&serde_json::to_vec(&request)?),
    )?;
    assert_eq!(code(&output), 0);
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(response["result"]["result"], "error");
    assert_eq!(response["result"]["data"]["code"], "stale_fence");
    assert_ne!(response["result"]["data"]["code"], "launch_unknown");
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_run_statuses_refuses_more_than_one_page() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let targets = (0..101)
        .map(|index| json!({"host":host,"attempt":format!("{index:032x}")}))
        .collect::<Vec<_>>();
    let request = json!({
        "version": 1,
        "target_host": host,
        "source_host": host,
        "agent": "fixture",
        "project": {"key": key, "common_dir": "repo/.git"},
        "action": {"action":"run_statuses","targets":targets}
    });
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
        ],
        Some(&serde_json::to_vec(&request)?),
    )?;
    assert_eq!(code(&output), 0);
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(response["result"]["result"], "error");
    assert_eq!(response["result"]["data"]["code"], "invalid_input");
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_wait_refuses_remote_fanout() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let local = provenant_core::HostId::parse(identity["data"]["host"].as_str().ok_or("host")?)?;
    let remote = next_host(local)?;
    let request = json!({
        "version": 1,
        "target_host": local.to_string(),
        "source_host": local.to_string(),
        "agent": "fixture",
        "project": {
            "key": identity["data"]["federation"]["key"],
            "common_dir": "repo/.git"
        },
        "action": {"action":"run","input":{
            "op":"wait",
            "targets":[{"host":remote.to_string(),"attempt":"11111111111111111111111111111111"}],
            "condition":"all",
            "wait_ms":1000
        }}
    });
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
        ],
        Some(&serde_json::to_vec(&request)?),
    )?;
    assert_eq!(code(&output), 0);
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(response["result"]["result"], "error");
    assert_eq!(response["result"]["data"]["code"], "confinement_refused");
    rig.assert_legacy_unchanged()
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
fn peer_frames_a_local_run_event_failure() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = &identity["data"]["host"];
    let key = &identity["data"]["federation"]["key"];
    std::fs::write(rig.state.join("run.sqlite3"), b"invalid SQLite database")?;
    std::fs::set_permissions(
        rig.state.join("run.sqlite3"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    let request = json!({
        "version": 1, "target_host": host, "source_host": host, "agent": "worker",
        "project": {"key": key, "common_dir": "repo/.git"},
        "action": {"action": "coordination", "operation_id": null,
            "input": {"domain": "events", "input": {
                "op": "read", "after": 0, "limit": 10, "wait_ms": null
            }}
        }
    });
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
        ],
        Some(&serde_json::to_vec(&request)?),
    )?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let response: provenant_fabric_runtime::hosts::PeerResponse =
        serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(response.version, 1);
    assert_eq!(serde_json::to_value(response.host)?, *host);
    assert_eq!(serde_json::to_value(response.project)?, *key);
    assert!(matches!(
        response.result,
        provenant_fabric_runtime::hosts::PeerResult::Error { code, operation_id }
            if code == "io" && operation_id.is_none()
    ));
    rig.assert_legacy_unchanged()
}

#[test]
fn local_run_event_failure_is_an_error_instead_of_unknown_host() -> Result {
    let rig = TestRig::new()?;
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    std::fs::write(rig.state.join("run.sqlite3"), b"invalid SQLite database")?;
    std::fs::set_permissions(
        rig.state.join("run.sqlite3"),
        std::fs::Permissions::from_mode(0o600),
    )?;

    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"fabric_events","arguments":{"op":"read","limit":10,"after":0}
            }}),
        ],
    )?;
    let result = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("events")?["result"];
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["code"], "io", "{result}");

    let output = rig.command(&["fabric", "events", "read", "--after", "0", "--json"])?;
    assert_ne!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
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
fn graph_cancel_recovers_an_attempt_missing_its_checkpoint_reference() -> Result {
    use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
    use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphJournal};
    use provenant_supervisor::{clock::SystemClock, fs::LocalFs};
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    std::fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"lost","nodes":[{"id":"one","prompt":"work","access":"read_only","routes":[{"adapter":"fixture","model":"one"}]}]}),
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
    assert_eq!(code(&run), 0);
    // Model a crash after acceptance but before the reference was saved.
    let mut roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let state = roots.resolve(&LocalFs)?.state;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    {
        let mut journal = FileJournal::open(
            &state,
            project,
            "lost",
            &LocalFs,
            WaitBudget::cli(clock.mono_now(), 10)?,
            &cancel,
            &clock,
        )?;
        let mut checkpoint = journal.load("lost")?.ok_or("checkpoint")?;
        let attempt = checkpoint.nodes[0].attempts.last_mut().ok_or("attempt")?;
        attempt.state = AttemptState::Running;
        attempt.attempt_ref = None;
        journal.save(&checkpoint)?;
    }
    let cancelled = rig.command(&["fabric", "graph", "cancel", "lost", "--json"])?;
    assert_eq!(
        code(&cancelled),
        0,
        "{}",
        String::from_utf8_lossy(&cancelled.stdout.bytes)
    );
    let cancelled: Value = serde_json::from_slice(&cancelled.stdout.bytes)?;
    assert_eq!(cancelled["data"]["status"], "cancelled");
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
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
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
                        deliver: None,
                        access: Default::default(),
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
    let cancel_request = |id: u64, target: &Value| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"cancel","target":target,"operation_id":"stop"}}});
    let bound = mcp(&rig, &[cancel_request(1, &target)])?;
    let bound = &bound.iter().find(|r| r["id"] == 1).ok_or("cancel")?["result"];
    assert_ne!(bound["isError"], true, "{bound}");
    assert_eq!(
        bound["structuredContent"]["attempt"]["state"]["outcome"], "cancelled",
        "{bound}"
    );
    let replies = mcp(
        &rig,
        &[cancel_request(2, &target), cancel_request(3, &other)],
    )?;
    let replay = &replies.iter().find(|r| r["id"] == 2).ok_or("replay")?["result"];
    assert_ne!(replay["isError"], true, "{replay}");
    assert_eq!(
        replay["structuredContent"]["attempt"]["state"]["outcome"], "cancelled",
        "{replay}"
    );
    let conflict = &replies.iter().find(|r| r["id"] == 3).ok_or("conflict")?["result"];
    assert_eq!(
        conflict["structuredContent"]["code"], "conflict",
        "{conflict}"
    );

    let remote_host = next_host(local_host)?;
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 10)?,
        cancel: &cancel,
    };
    let records = {
        let store = provenant_run::RunStore::open(&context, &call)?;
        attempts[..2]
            .iter()
            .map(|attempt| store.record(*attempt, &call))
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let project_key_value: provenant_fabric_runtime::identity::CrossHostProjectKey =
        serde_json::from_value(json!(identity["data"]["federation"]["key"]))?;
    let ingress_request = json!({
        "version": 1,
        "target_host": local_host.to_string(),
        "source_host": local_host.to_string(),
        "agent": "fixture",
        "project": {"key": project_key_value, "common_dir": "repo/.git"},
        "action": {"action":"run_statuses","targets":attempts[..2]
            .iter()
            .map(|attempt| json!({"host":local_host.to_string(),"attempt":attempt.to_string()}))
            .collect::<Vec<_>>()}
    });
    let ingress = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
        ],
        Some(&serde_json::to_vec(&ingress_request)?),
    )?;
    assert_eq!(code(&ingress), 0);
    let ingress: Value = serde_json::from_slice(&ingress.stdout.bytes)?;
    assert_eq!(ingress["result"]["result"], "run_statuses");
    assert_eq!(
        ingress["result"]["data"]
            .as_array()
            .ok_or("statuses")?
            .len(),
        2
    );

    let project_key: provenant_fabric_runtime::identity::CrossHostProjectKey =
        serde_json::from_value(json!(identity["data"]["federation"]["key"]))?;
    let response = provenant_fabric_runtime::hosts::PeerResponse {
        version: 1,
        host: remote_host,
        project: project_key,
        result: provenant_fabric_runtime::hosts::PeerResult::RunStatuses(records),
    };
    let response_path = rig.root.join("ssh-response.json");
    let requests_path = rig.root.join("ssh-requests.jsonl");
    let ssh = rig.root.join("ssh");
    std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nrequest=$(cat)\nprintf '%s\\n' \"$request\" >> '{}'\ncat '{}'\n",
            requests_path.display(),
            response_path.display()
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric]\nssh_executable={:?}\n\
             [domains.fabric.peers.fixture]\nhost='{}'\ndestination='fixture'\n",
            ssh.to_string_lossy(),
            remote_host
        ),
    )?;
    let remote_targets = attempts[..2]
        .iter()
        .map(|attempt| json!({"host":remote_host.to_string(),"attempt":attempt.to_string()}))
        .collect::<Vec<_>>();
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"fabric_run","arguments":{"op":"wait","targets":remote_targets,"condition":"all","wait_ms":1000}
            }}),
        ],
    )?;
    let wait_result = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("remote wait")?["result"];
    assert_ne!(wait_result["isError"], true, "{wait_result}");
    let wait = &wait_result["structuredContent"];
    assert_eq!(wait["timed_out"], true, "{wait}");
    assert_eq!(
        wait["attempts"].as_array().ok_or("attempts")?.len(),
        2,
        "{wait}"
    );
    let requests = std::fs::read_to_string(requests_path)?;
    let requests = requests
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(!requests.is_empty());
    assert!(requests.iter().all(|request| {
        request["action"]["targets"]
            .as_array()
            .is_some_and(|targets| targets.len() == 2)
    }));

    let event_limit = {
        let store = provenant_run::RunStore::open(&context, &call)?;
        let events = store.events(0, 64, None, &call)?;
        assert!(events.len() < 64);
        u16::try_from(events.len())?
    };
    assert!((1..=64).contains(&event_limit));
    let run_events_request = json!({
        "version": 1,
        "target_host": local_host.to_string(),
        "source_host": local_host.to_string(),
        "agent": "fixture",
        "project": {"key": identity["data"]["federation"]["key"], "common_dir": "repo/.git"},
        "action": {"action":"run_events","after":0,"limit":event_limit}
    });
    let run_events = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
        ],
        Some(&serde_json::to_vec(&run_events_request)?),
    )?;
    assert_eq!(code(&run_events), 0);
    let run_events: Value = serde_json::from_slice(&run_events.stdout.bytes)?;
    assert_eq!(
        run_events["result"]["data"]["items"]
            .as_array()
            .ok_or("run events")?
            .len(),
        usize::from(event_limit)
    );
    assert_eq!(run_events["result"]["data"]["truncated"], false);
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

/// A loopback peer: the fake SSH executable runs this build's real
/// `fabric peer` against a second state directory, so the placement record
/// and every forwarded call cross the actual peer protocol.
fn loopback_peer(rig: &mut TestRig) -> Result<provenant_core::HostId> {
    let identity = federated_identity(rig)?;
    let local = identity["data"]["host"].as_str().ok_or("host")?.to_owned();
    let peer_state = rig.root.join("peer-state");
    std::fs::DirBuilder::new().mode(0o700).create(&peer_state)?;
    std::fs::write(
        peer_state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric.peers.laptop]\nhost='{local}'\ndestination='laptop'\n"
        ),
    )?;
    let binary = Path::new(env!("CARGO_BIN_EXE_provenant"));
    let peer_identity = rig.execute_os(
        binary,
        &[
            "--state-dir".into(),
            peer_state.clone().into(),
            "fabric".into(),
            "identity".into(),
            "--json".into(),
        ],
        None,
    )?;
    assert_eq!(code(&peer_identity), 0);
    let peer_identity: Value = serde_json::from_slice(&peer_identity.stdout.bytes)?;
    let peer =
        provenant_core::HostId::parse(peer_identity["data"]["host"].as_str().ok_or("peer host")?)?;
    assert_ne!(peer.to_string(), local);
    let mut script = String::from("#!/bin/sh\n");
    for (key, value) in &rig.env {
        if key == "PROVENANT_STATE_DIR" {
            continue;
        }
        script.push_str(&format!(
            "export {}='{}'\n",
            key.to_string_lossy(),
            value.to_string_lossy()
        ));
    }
    script.push_str(&format!(
        "exec '{}' --state-dir '{}' fabric peer 2>>'{}'\n",
        binary.display(),
        peer_state.display(),
        rig.root.join("peer-stderr").display()
    ));
    let ssh = rig.root.join("ssh");
    std::fs::write(&ssh, script)?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric]\nssh_executable={:?}\n\
             [domains.fabric.peers.workshop]\nhost='{peer}'\ndestination='workshop'\n",
            ssh.to_string_lossy(),
        ),
    )?;
    Ok(peer)
}

fn json_command(rig: &TestRig, args: &[&str]) -> Result<(i64, Value)> {
    let output = rig.command(args)?;
    let value = serde_json::from_slice(&output.stdout.bytes).map_err(|error| {
        format!(
            "{error}: {} {}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        )
    })?;
    Ok((code(&output), value))
}

#[test]
fn read_only_dispatch_places_on_a_peer_and_forwards_by_id() -> Result {
    let mut rig = TestRig::new()?;
    // A provider the peer can launch: it answers the version probe, then
    // writes a known stderr line. The production read-only profile anchors
    // the host Rust runtime roots.
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        std::fs::create_dir_all(rig.home.join(path))?;
    }
    std::fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let bin = rig.root.join("provider/bin");
    std::fs::create_dir_all(&bin)?;
    std::fs::write(
        bin.join("codex"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'codex-cli 0.159.0'; exit 0; fi\n\
         cat >/dev/null\necho 'peer stderr fixture' >&2\n",
    )?;
    std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(
        rig.root.join("codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-one"}}"#,
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let peer = loopback_peer(&mut rig)?;
    let operation = "remote-dispatch-fixture";
    let prompt = rig.root.join("prompt.txt");
    std::fs::write(&prompt, "reply OK")?;
    let prompt = prompt.to_str().ok_or("prompt path")?.to_owned();
    let dispatch = [
        "dispatch",
        "--host",
        "workshop",
        "--mode",
        "read-only",
        "--model",
        "codex/gpt-6-luna",
        "--effort",
        "low",
        "--operation-id",
        operation,
        "--prompt-file",
        &prompt,
        "--wait",
        "30",
        "--json",
    ];
    let (_, placed) = json_command(&rig, &dispatch)?;
    assert_eq!(
        placed["data"]["host"],
        peer.to_string(),
        "{placed} {}",
        std::fs::read_to_string(rig.root.join("peer-stderr")).unwrap_or_default()
    );
    assert_eq!(placed["data"]["operation_id"], operation);
    assert_eq!(placed["data"]["condition_met"], true, "{placed}");
    let run = placed["data"]["run_id"]
        .as_str()
        .ok_or("run id")?
        .to_owned();
    let lane = &placed["data"]["lanes"][0];
    let attempt = lane["attempt_id"].as_str().ok_or("attempt")?.to_owned();
    assert_eq!(lane["host"], peer.to_string());
    assert_eq!(lane["result_path"], Value::Null);
    // The placement is the only local record; the run lives on the peer.
    let placement: Value = serde_json::from_slice(&std::fs::read(
        rig.state.join(format!("fabric/remote-runs/{run}.json")),
    )?)?;
    assert_eq!(placement["host"], peer.to_string());
    assert_eq!(placement["attempts"][0]["attempt_id"], attempt);
    // Replaying the same operation recovers the same receipt.
    let (_, replayed) = json_command(&rig, &dispatch)?;
    assert_eq!(replayed["data"]["run_id"], run.as_str(), "{replayed}");

    let (status, lanes) = json_command(&rig, &["lanes", "--id", &run, "--json"])?;
    assert_eq!(status, 0, "{lanes}");
    assert_eq!(lanes["data"]["lanes"][0]["attempt_id"], attempt.as_str());
    assert_eq!(lanes["data"]["lanes"][0]["state"], lane["state"]);
    let (status, waited) = json_command(&rig, &["lanes", "--id", &run, "--wait", "5", "--json"])?;
    assert_eq!(status, 0, "{waited}");
    assert_eq!(waited["data"]["condition_met"], true);

    let task = lane["task_id"].as_str().ok_or("task")?;
    let target = format!("{run}/{task}/{attempt}");
    let (status, stderr) = json_command(&rig, &["output", &target, "--part", "stderr", "--json"])?;
    if cfg!(target_os = "macos") {
        assert_eq!(status, 0, "{stderr} {placed}");
        assert_eq!(stderr["data"]["host"], peer.to_string());
        assert_eq!(stderr["data"]["offset"], 0);
        assert!(
            stderr["data"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("peer stderr fixture")),
            "{stderr}"
        );
    } else {
        // Confined launch is macOS-only, so the peer rejects before any
        // output exists; the read is still the peer's, not a local miss.
        assert_eq!(lane["state"], "rejected", "{placed}");
        assert_eq!(stderr["error"]["code"], "PEER", "{stderr}");
    }
    let (status, tail) = json_command(&rig, &["output", &target, "--tail", "5", "--json"])?;
    assert_ne!(status, 0);
    assert_eq!(tail["error"]["code"], "UNSUPPORTED", "{tail}");

    let (status, cancelled) = json_command(
        &rig,
        &[
            "cancel",
            &run,
            "--operation-id",
            "remote-cancel-fixture",
            "--json",
        ],
    )?;
    assert_eq!(status, 0, "{cancelled}");
    assert_eq!(
        cancelled["data"]["receipts"][0]["attempt_id"],
        attempt.as_str()
    );
    assert_eq!(cancelled["data"]["host"], peer.to_string());
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_dispatch_refuses_misplaced_writers_and_unknown_hosts_before_contact() -> Result {
    let mut rig = TestRig::new()?;
    loopback_peer(&mut rig)?;
    std::fs::write(rig.root.join("ssh"), "#!/bin/sh\nexit 97\n")?;
    let prompt = rig.root.join("prompt.txt");
    std::fs::write(&prompt, "x")?;
    let prompt = prompt.to_str().ok_or("prompt path")?;
    let worktree = rig.project.to_str().ok_or("worktree path")?.to_owned();
    let worktree = worktree.as_str();
    let writer = |host: &str, operation: &str, place: &[&str]| -> Result<(i64, Value)> {
        let mut args = vec![
            "dispatch",
            "--host",
            host,
            "--mode",
            "worktree-write",
            "--model",
            "codex/gpt-6-luna",
            "--operation-id",
            operation,
            "--prompt-file",
            prompt,
            "--json",
        ];
        args.extend_from_slice(place);
        json_command(&rig, &args)
    };
    // --worktree is a path on this host, so a peer writer cannot use it.
    let (status, refused) = writer("workshop", "remote-writer", &["--worktree", worktree])?;
    assert_ne!(status, 0);
    assert_eq!(
        refused["error"]["code"], "WORKTREE_NOT_APPLICABLE",
        "{refused}"
    );
    // The local host's own ID is local placement, not a peer: the writer is
    // then judged by its worktree (the primary checkout is refused), and a
    // peer's --branch does not apply.
    let output = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let local = identity["data"]["host"].as_str().ok_or("host")?;
    for (host, operation) in [(local, "local-writer"), ("local", "local-alias-writer")] {
        let (status, local_writer) = writer(host, operation, &["--worktree", worktree])?;
        assert_ne!(status, 0);
        assert_ne!(
            local_writer["error"]["code"], "WORKTREE_NOT_APPLICABLE",
            "{local_writer}"
        );
    }
    let (status, local_branch) = writer(
        "local",
        "local-branch-writer",
        &["--branch", "peer/x", "--base", "main"],
    )?;
    assert_ne!(status, 0);
    assert_eq!(local_branch["error"]["code"], "INVALID", "{local_branch}");
    // An unrecognised name is unknown, not a refused peer.
    let (status, unknown_writer) = writer(
        "laptop",
        "unknown-writer",
        &["--branch", "peer/x", "--base", "main"],
    )?;
    assert_ne!(status, 0);
    assert_eq!(
        unknown_writer["error"]["code"], "UNKNOWN_HOST",
        "{unknown_writer}"
    );
    let (status, unknown) = json_command(
        &rig,
        &[
            "dispatch",
            "--host",
            "nowhere",
            "--model",
            "codex/gpt-6-luna",
            "--operation-id",
            "remote-unknown",
            "--prompt-file",
            prompt,
            "--json",
        ],
    )?;
    assert_ne!(status, 0);
    assert_eq!(unknown["error"]["code"], "UNKNOWN_HOST", "{unknown}");
    rig.assert_legacy_unchanged()
}

fn git(rig: &TestRig, cwd: &Path, args: &[&str]) -> Result<String> {
    let mut argv = vec!["-C", cwd.to_str().ok_or("git cwd")?];
    argv.extend_from_slice(args);
    let output = rig.execute(Path::new("/usr/bin/git"), &argv, None)?;
    assert_eq!(
        code(&output),
        0,
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
}

#[test]
fn peer_writer_creates_its_worktree_from_a_pushed_base() -> Result {
    let mut rig = TestRig::new()?;
    let peer = loopback_peer(&mut rig)?;
    let project = rig.project.clone();
    let commit = [
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
    ];
    git(
        &rig,
        &project,
        &[
            &commit[..],
            &["commit", "-q", "--allow-empty", "-m", "base"],
        ]
        .concat(),
    )?;
    let base = git(&rig, &project, &["rev-parse", "HEAD"])?;
    let remote = rig.root.join("remote.git");
    git(
        &rig,
        &rig.root,
        &["init", "-q", "--bare", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &rig,
        &project,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &rig,
        &project,
        &["push", "-q", "origin", "HEAD:refs/heads/main"],
    )?;
    // A commit only this clone has is not a base the peer can share.
    git(
        &rig,
        &project,
        &[
            &commit[..],
            &["commit", "-q", "--allow-empty", "-m", "local"],
        ]
        .concat(),
    )?;
    let unpushed = git(&rig, &project, &["rev-parse", "HEAD"])?;
    let prompt = rig.root.join("prompt.txt");
    std::fs::write(&prompt, "commit nothing")?;
    let prompt = prompt.to_str().ok_or("prompt path")?.to_owned();
    let dispatch = |operation: &str, branch: &str, base: &str| {
        json_command(
            &rig,
            &[
                "dispatch",
                "--host",
                "workshop",
                "--mode",
                "worktree-write",
                "--branch",
                branch,
                "--base",
                base,
                "--model",
                "codex/gpt-6-luna",
                "--operation-id",
                operation,
                "--prompt-file",
                &prompt,
                "--json",
            ],
        )
    };
    let (status, placed) = dispatch("peer-writer", "peer/writer", "origin/main")?;
    assert_eq!(
        status,
        0,
        "{placed} {}",
        std::fs::read_to_string(rig.root.join("peer-stderr")).unwrap_or_default()
    );
    let worktree = &placed["data"]["worktree"];
    assert_eq!(worktree["host"], peer.to_string(), "{placed}");
    assert_eq!(worktree["branch"], "peer/writer");
    let path = Path::new(worktree["path"].as_str().ok_or("worktree path")?);
    assert_eq!(git(&rig, path, &["rev-parse", "HEAD"])?, base);
    let run = placed["data"]["run_id"]
        .as_str()
        .ok_or("run id")?
        .to_owned();
    // Replaying the operation recovers the receipt and its worktree.
    let (_, replayed) = dispatch("peer-writer", "peer/writer", "origin/main")?;
    assert_eq!(replayed["data"]["run_id"], run.as_str(), "{replayed}");
    assert_eq!(replayed["data"]["worktree"], *worktree);
    let (status, lanes) = json_command(&rig, &["lanes", "--id", &run, "--json"])?;
    assert_eq!(status, 0, "{lanes}");
    assert_eq!(
        lanes["data"]["lanes"][0]["attempt_id"],
        placed["data"]["lanes"][0]["attempt_id"]
    );

    // A retry whose first create left no run record adopts the worktree
    // while it is still clean at the base; a unique commit makes it taken.
    let (status, adopted) = dispatch("peer-writer-retry", "peer/writer", "origin/main")?;
    assert_eq!(status, 0, "{adopted}");
    assert_ne!(adopted["data"]["run_id"], run.as_str());
    assert_eq!(adopted["data"]["worktree"], *worktree);
    git(
        &rig,
        path,
        &[
            &commit[..],
            &["commit", "-q", "--allow-empty", "-m", "work"],
        ]
        .concat(),
    )?;
    let (status, taken) = dispatch("peer-writer-again", "peer/writer", "origin/main")?;
    assert_ne!(status, 0);
    assert_eq!(taken["error"]["code"], "BRANCH_EXISTS", "{taken}");
    let (status, unreachable) = dispatch("peer-writer-local", "peer/local", &unpushed)?;
    assert_ne!(status, 0);
    assert_eq!(
        unreachable["error"]["code"], "BASE_UNREACHABLE",
        "{unreachable}"
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn mcp_submission_places_read_only_work_on_a_named_peer() -> Result {
    let mut rig = TestRig::new()?;
    let peer = loopback_peer(&mut rig)?;
    let task = json!({"prompt":"fixture","route":{"model":"codex/gpt-6-luna"},"timeout_ms":60000});
    let mut writer = task.clone();
    writer["access"] = "worktree_write".into();
    writer["worktree"] = rig.project.to_string_lossy().into_owned().into();
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_submit","arguments":{"op":"submission","operation_id":"mcp-peer","tasks":[task],"host":peer.to_string()}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_submit","arguments":{"op":"submission","operation_id":"mcp-peer-writer","tasks":[writer],"host":peer.to_string()}}}),
        ],
    )?;
    let placed = &replies.iter().find(|r| r["id"] == 1).ok_or("submission")?["result"];
    assert_ne!(placed["isError"], true, "{placed}");
    let target = placed["structuredContent"]["targets"][0].clone();
    assert_eq!(target["host"], peer.to_string(), "{placed}");
    // The run lives only in the peer's store.
    assert!(!rig.state.join("runs").exists());
    assert!(rig.root.join("peer-state/runs").exists());
    let writer = &replies.iter().find(|r| r["id"] == 2).ok_or("writer")?["result"];
    assert_eq!(writer["isError"], true, "{writer}");
    assert_eq!(writer["structuredContent"]["code"], "owner_unavailable");

    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"status","target":target}}}),
        ],
    )?;
    let status = &replies.iter().find(|r| r["id"] == 3).ok_or("status")?["result"];
    assert_ne!(status["isError"], true, "{status}");
    assert_eq!(
        status["structuredContent"]["attempt"]["attempt_id"],
        target["attempt"]
    );
    rig.assert_legacy_unchanged()
}
