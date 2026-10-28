//! The real Rust command edge runs without Python, Node or ambient state.
mod support;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
};
use support::{Result, TestRig, code};

/// Read the stable code from structured errors or the text-only edge.
fn text_error_code(result: &Value) -> &str {
    if let Some(code) = result["structuredContent"]["code"].as_str() {
        return code;
    }
    result["content"][0]["text"]
        .as_str()
        .and_then(|text| text.split_once(':'))
        .map_or("", |(code, _)| code)
}

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

fn configure_fixture_peer(rig: &TestRig, host: provenant_core::HostId) -> Result {
    let config = rig.state.join("config.toml");
    let mut doc: toml_edit::DocumentMut = if config.exists() {
        std::fs::read_to_string(&config)?.parse()?
    } else {
        "schema_version=1\n".parse()?
    };
    doc["domains"]["fabric"]["peers"]["fixture"]["host"] = toml_edit::value(host.to_string());
    doc["domains"]["fabric"]["peers"]["fixture"]["destination"] = toml_edit::value("fixture");
    std::fs::write(config, doc.to_string())?;
    Ok(())
}

#[test]
fn hosts_probe_only_the_requested_page() -> Result {
    let mut rig = TestRig::in_directory(&std::env::temp_dir())?;
    let identity = federated_identity(&mut rig)?;
    let cli = rig.command(&["hosts", "--json"])?;
    assert_eq!(code(&cli), 0);
    let cli: Value = serde_json::from_slice(&cli.stdout.bytes)?;
    assert_eq!(cli["data"].as_array().ok_or("CLI host list")?.len(), 1);
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
            "version": 1, "protocol":5,
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
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n\
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
                "name":"fabric_context","arguments":{"op":"hosts","after":local.to_string(),"limit":1}
            }}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"fabric_context","arguments":{"op":"hosts","after":second.to_string(),"limit":1}
            }}),
        ],
    )?;
    let hosts = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("hosts")?["result"]["structuredContent"]["hosts"];
    assert_eq!(hosts.as_array().ok_or("host list")?.len(), 1);
    assert_eq!(hosts[0]["host"], first.to_string());
    assert_eq!(hosts[0]["available"], true, "{hosts}");
    assert_eq!(hosts[0]["protocol"], 5);
    assert!(hosts[0].get("reason").is_none());
    assert_eq!(
        replies
            .iter()
            .find(|reply| reply["id"] == 2)
            .ok_or("empty hosts")?["result"]["structuredContent"],
        json!({"hosts":[]})
    );
    let calls = std::fs::read_to_string(calls)?;
    let calls = calls
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["target_host"], first.to_string());
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                "name":"fabric_context","arguments":{"op":"hosts","after":local.to_string(),"limit":2}
            }}),
        ],
    )?;
    let hosts = &replies
        .iter()
        .find(|reply| reply["id"] == 3)
        .ok_or("hosts")?["result"]["structuredContent"]["hosts"];
    assert_eq!(hosts[0]["available"], true);
    // The second peer cannot provide a valid reply for its host identity.
    assert_eq!(hosts[1]["host"], second.to_string());
    assert_eq!(hosts[1]["available"], false);
    assert!(hosts[1].get("protocol").is_none());
    // An unreachable peer also has no observed protocol.
    std::fs::write(&ssh, "#!/bin/sh\ncat >/dev/null\nexit 255\n")?;
    let (status, output) = json_command(&rig, &["hosts", "--json"])?;
    assert_eq!(status, 0, "{output}");
    for host in output["data"].as_array().ok_or("CLI host list")? {
        if host["host"] != local.to_string() {
            assert_eq!(host["available"], false);
            assert!(host.get("protocol").is_none());
        }
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn home_events_read_reports_upgrade_refused_for_legacy_import_data() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let seeded = rig.command(&[
        "fabric",
        "events",
        "note",
        "--note",
        "seed store",
        "--operation-id",
        "seed-1",
    ])?;
    assert_eq!(code(&seeded), 0);
    let db = rusqlite::Connection::open(rig.state.join("fabric/fabric.sqlite3"))?;
    db.execute_batch(
        "CREATE TABLE imports(source TEXT PRIMARY KEY, project TEXT NOT NULL, \
         legacy_project TEXT NOT NULL, imported_at INTEGER NOT NULL, \
         tasks INTEGER NOT NULL, messages INTEGER NOT NULL, \
         skipped_rows BLOB NOT NULL DEFAULT X'7B7D') STRICT; \
         INSERT INTO imports VALUES('/old/fabric.db','p','lp',0,1,0,X'7B7D'); \
         PRAGMA user_version=6;",
    )?;
    drop(db);
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let source = next_host(host.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let request = json!({
        "version": 1, "protocol":5,
        "target_host": host,
        "source_host": source,
        "agent": "fixture",
        "project": {"key": key, "common_dir": "repo/.git"},
        "action": {"action":"coordination","input":{
            "domain":"events","input":{"op":"read","after":null,"limit":10,"wait_seconds":null}
        },"operation_id":null}
    });
    // The home pulls events before opening coordination, unlike the CLI read.
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
            "--in-process".into(),
        ],
        Some(&serde_json::to_vec(&request)?),
    )?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        response["result"],
        json!({"result":"error","data":{"error":{"code":"upgrade_refused"}}})
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn remote_landing_push_preserves_definitive_store_refusal() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let source = next_host(host.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let request = json!({
        "version": 1, "protocol":5,
        "target_host": host,
        "source_host": source,
        "agent": "fixture",
        "project": {"key": key, "common_dir": "repo/.git"},
        "action": {"action":"landing_push","proof":{
            "reference":"refs/heads/main",
            "id":"11111111111111111111111111111111",
            "generation":1,
            "token":"00000000000000000000000000000000"
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
    assert_eq!(response["result"]["data"]["error"]["code"], "stale_fence");
    assert_ne!(
        response["result"]["data"]["error"]["code"],
        "launch_unknown"
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_run_statuses_refuses_more_than_one_page() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let source = next_host(host.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let targets = (0..101)
        .map(|index| json!({"host":host,"attempt":format!("{index:032x}")}))
        .collect::<Vec<_>>();
    let request = json!({
        "version": 1, "protocol":5,
        "target_host": host,
        "source_host": source,
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
    assert_eq!(response["result"]["data"]["error"]["code"], "invalid_input");
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_wait_refuses_remote_fanout() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let local = provenant_core::HostId::parse(identity["data"]["host"].as_str().ok_or("host")?)?;
    let remote = next_host(local)?;
    configure_fixture_peer(&rig, remote)?;
    let request = json!({
        "version": 1, "protocol":5,
        "target_host": local.to_string(),
        "source_host": remote.to_string(),
        "agent": "fixture",
        "project": {
            "key": identity["data"]["federation"]["key"],
            "common_dir": "repo/.git"
        },
        "action": {"action":"run","input":{
            "op":"wait",
            "targets":[{"host":remote.to_string(),"attempt":"11111111111111111111111111111111"}],
            "condition":"all",
            "wait_seconds":1
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
    assert_eq!(
        response["result"]["data"]["error"]["code"],
        "confinement_refused"
    );
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
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fabric_context","arguments":{"op":"inspect"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fabric_mail","arguments":{"op":"send","recipients":["11111111111111111111111111111111"],"kind":"note","body":"Rust transport","artifacts":[],"reply_to":null,"task":null}}}),
        ],
    )?;
    assert_eq!(
        replies.iter().find(|r| r["id"] == 1).ok_or("initialise")?["result"]["protocolVersion"],
        "2025-11-25"
    );
    let list = &replies.iter().find(|r| r["id"] == 2).ok_or("catalogue")?["result"]["tools"];
    let names: Vec<_> = list
        .as_array()
        .ok_or("tools")?
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "fabric_context",
            "fabric_mail",
            "fabric_run",
            "fabric_output",
            "fabric_discover"
        ]
    );
    let identity =
        &replies.iter().find(|r| r["id"] == 3).ok_or("identity")?["result"]["structuredContent"];
    assert_eq!(identity["client"], "codex");
    assert_eq!(identity["server_version"], env!("CARGO_PKG_VERSION"));
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
fn peer_ingress_refuses_protocol_before_decoding_or_running_mutations() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    for protocol in [1, 4, 6] {
        for action in [
            json!({"action":"coordination","input":{
                "domain":"events","input":{"op":"note","note":"must not run","dedup_key":null}
            },"operation_id":"wrong-version"}),
            json!({"action":"future_unknown_action"}),
        ] {
            let input = json!({
                "version":1,"protocol":protocol,
                "target_host":identity["data"]["host"],
                "source_host":"22222222222222222222222222222222",
                "agent":"fixture",
                "project":{"key":identity["data"]["federation"]["key"],"common_dir":"repo/.git"},
                "action":action,
            });
            let output = rig.execute_os(
                Path::new(env!("CARGO_BIN_EXE_provenant")),
                &["fabric".into(), "peer".into()],
                Some(&serde_json::to_vec(&input)?),
            )?;
            assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
            let reply: Value = serde_json::from_slice(&output.stdout.bytes)?;
            assert_eq!(reply["protocol"], 5);
            assert_eq!(reply["result"]["data"]["error"]["code"], "peer_version");
            assert!(!rig.state.join("fabric/fabric.sqlite3").exists());
        }
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_ingress_rejects_wrong_host_before_coordination_mutation() -> Result {
    let rig = TestRig::new()?;
    let input = json!({"version":1,"protocol":5,"target_host":"11111111111111111111111111111111","source_host":"22222222222222222222222222222222","agent":"fixture","project":{"key":"not-a-project","common_dir":"../escape"},"action":{"action":"health"}});
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
fn peer_ingress_refuses_the_target_host_as_source() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    configure_fixture_peer(&rig, host.parse()?)?;
    let request = json!({
        "version": 1, "protocol":5,
        "target_host": host,
        "source_host": host,
        "agent": "fixture",
        "project": {"key": identity["data"]["federation"]["key"], "common_dir": "repo/.git"},
        "action": {"action":"coordination","input":{
            "domain":"mail","input":{"op":"read","after":null,"limit":10}
        },"operation_id":null}
    });
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
            "--in-process".into(),
        ],
        Some(&serde_json::to_vec(&request)?),
    )?;
    // Validation failures exit before a PeerResponse is framed.
    assert_eq!(code(&output), 3);
    assert!(output.stdout.bytes.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr.bytes)
            .contains("peer source host must be a configured peer other than this host")
    );
    assert!(!rig.state.join("fabric/fabric.sqlite3").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_ingress_refuses_a_trace_naming_another_host_than_the_sender() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = &identity["data"]["host"];
    let source = next_host(host.as_str().ok_or("host")?.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let key = &identity["data"]["federation"]["key"];
    let mut request = json!({
        "version": 1, "protocol":5, "target_host": host, "source_host": source, "agent": "worker",
        "project": {"key": key, "common_dir": "repo/.git"},
        "trace": {"host": host},
        "action": {"action": "health"},
    });
    let args: Vec<OsString> = vec![
        "--state-dir".into(),
        rig.state.clone().into(),
        "fabric".into(),
        "peer".into(),
    ];
    let binary = Path::new(env!("CARGO_BIN_EXE_provenant"));
    let refused = rig.execute_os(binary, &args, Some(&serde_json::to_vec(&request)?))?;
    assert_ne!(code(&refused), 0);
    assert!(refused.stdout.bytes.is_empty());
    // The same request traced to its sender is served.
    request["trace"] = json!({"host": source});
    let served = rig.execute_os(binary, &args, Some(&serde_json::to_vec(&request)?))?;
    assert_eq!(code(&served), 0, "{:?}", served.stderr.bytes);
    let reply: Value = serde_json::from_slice(&served.stdout.bytes)?;
    assert_eq!(reply["result"]["result"], "health", "{reply}");
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
    let source = next_host(host.as_str().ok_or("host")?.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let request = json!({"version":1,"protocol":5,"target_host":host,"source_host":source,"agent":"worker","project":{"key":key,"common_dir":"repo/.git"},"action":{"action":"coordination","input":{"domain":"events","input":{"op":"note","note":"remote note","dedup_key":null}},"operation_id":"note-once"}});
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
    let source = next_host(host.as_str().ok_or("host")?.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let key = &identity["data"]["federation"]["key"];
    std::fs::write(rig.state.join("run.sqlite3"), b"invalid SQLite database")?;
    std::fs::set_permissions(
        rig.state.join("run.sqlite3"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    let request = json!({
        "version": 1, "protocol":5, "target_host": host, "source_host": source, "agent": "worker",
        "project": {"key": key, "common_dir": "repo/.git"},
        "action": {"action": "coordination", "operation_id": null,
            "input": {"domain": "events", "input": {
                "op": "read", "after": 0, "limit": 10, "wait_seconds": null
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
        provenant_fabric_runtime::hosts::PeerResult::Error {
            error: provenant_fabric_runtime::coordination::facade::FacadeError::Io { .. }
        }
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
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_context","arguments":{"op":"inspect"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"list","limit":10}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fabric_events","arguments":{"op":"read","limit":10,"after":0,"wait_seconds":null}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fabric_events","arguments":{"op":"read","limit":10,"after":0,"wait_seconds":1}}}),
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
    let cleanup = rig.process_cleanup();
    std::fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"edge","nodes":[{"id":"one","prompt":"work","access":"read_only","routes":[{"adapter":"fixture","model":"one","account":"work"},{"adapter":"fixture","model":"two","account":"pro"}]}]}),
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
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let context = provenant_run::ExecutionContext {
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
    use provenant_core::Clock;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 10)?,
        cancel: &cancel,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    for (attempt, account) in attempts.iter().zip(["work", "pro"]) {
        assert_eq!(attempt["state"], "terminal");
        assert_eq!(attempt["launched"], false);
        assert_eq!(attempt["outcome"], "adapter_unavailable");
        assert!(attempt["attempt_ref"].as_str().is_some());
        let (_, id) = attempt["attempt_ref"]
            .as_str()
            .ok_or("reference")?
            .split_once('/')
            .ok_or("reference")?;
        let record = store.record(id.parse()?, &call)?;
        assert_eq!(
            store.request(record.run_id, &call)?.tasks[0]
                .account
                .as_deref(),
            Some(account)
        );
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
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn graph_resume_clears_a_recorded_advance_failure() -> Result {
    let rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    std::fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"stuck","nodes":[{"id":"one","prompt":"work","access":"read_only","route":{"adapter":"fixture","model":"one"}}]}),
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
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = identity["data"]["project"].as_str().ok_or("project")?;
    // The guardian writes this beside the checkpoint when its advance fails;
    // `dispatch_provider` covers a real failure, this seeds one to clear it.
    let marker = rig
        .state
        .join("fabric/graphs")
        .join(project)
        .join("graph-stuck.advance-error");
    std::fs::write(&marker, "journal lock timed out")?;
    let status = rig.command(&["fabric", "graph", "status", "stuck", "--json"])?;
    assert_eq!(code(&status), 0);
    let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
    assert_eq!(status["data"]["advance_error"], "journal lock timed out");
    // A successful manual advance supersedes the recorded failure.
    let resume = rig.command(&[
        "fabric",
        "graph",
        "resume",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&resume),
        0,
        "{}",
        String::from_utf8_lossy(&resume.stderr.bytes)
    );
    assert!(!marker.exists());
    let status = rig.command(&["fabric", "graph", "status", "stuck", "--json"])?;
    let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
    assert!(status["data"].get("advance_error").is_none(), "{status}");
    cleanup.disarm();
    Ok(())
}

#[test]
fn graph_cancel_recovers_an_attempt_missing_its_checkpoint_reference() -> Result {
    use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
    use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphJournal};
    use provenant_os::clock::SystemClock;

    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
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
    let state = roots.resolve(&provenant_os::fs::SystemPathResolver)?.state;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    {
        let mut journal = FileJournal::open(
            &state,
            project,
            "lost",
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
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn run_status_and_wait_read_the_owner_store_and_event_pull_deduplicates() -> Result {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, HostId, OperationId, ProjectId, RootInputs,
        RunId, TaskId, WaitBudget,
    };
    use provenant_os::clock::SystemClock;
    use provenant_records::{
        CanonicalV1,
        run::{CanonicalRequest, RequestedRoute, TaskRequest},
    };

    let mut rig = TestRig::in_directory(Path::new(env!("CARGO_MANIFEST_DIR")))?;
    let identity = federated_identity(&mut rig)?;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let actor = ActorId::parse(identity["data"]["inbox"]["actor"].as_str().ok_or("actor")?)?;
    let mut roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let state = roots.resolve(&provenant_os::fs::SystemPathResolver)?.state;
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
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: tasks
                    .iter()
                    .map(|task_id| TaskRequest {
                        title: None,
                        base: None,
                        base_ref: None,
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
    let output = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_output","arguments":{"op":"read","target":{"attempt":attempt},"part":"prompt","offset":0,"max_bytes":65536}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_output","arguments":{"op":"read","target":{"attempt":attempt},"part":"events","offset":0,"max_bytes":65536}}}),
        ],
    )?;
    let prompt = &output
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("prompt output")?["result"];
    assert_eq!(prompt["structuredContent"]["bytes"]["data"], "prompt");
    let events = &output
        .iter()
        .find(|reply| reply["id"] == 2)
        .ok_or("events output")?["result"];
    let events: Value = serde_json::from_str(
        events["structuredContent"]["bytes"]["data"]
            .as_str()
            .ok_or("event bytes")?,
    )?;
    assert!(!events.as_array().ok_or("event array")?.is_empty());
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
    // Pages run newest first.
    assert_eq!(first["items"][0]["attempt_id"], attempts[2].to_string());
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
    assert_eq!(third["items"][0]["attempt_id"], attempts[0].to_string());
    assert_eq!(third["next_after"], Value::Null);
    let target = json!({"host":null,"attempt":attempt.to_string()});
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"status","target":target}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"wait","targets":[target],"condition":"all","wait_seconds":1}}}),
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
    // same ID for another target is an operation conflict.
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
        conflict["structuredContent"]["code"], "operation_conflict",
        "{conflict}"
    );

    let remote_host = next_host(local_host)?;
    configure_fixture_peer(&rig, remote_host)?;
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
        "version": 1, "protocol":5,
        "target_host": local_host.to_string(),
        "source_host": remote_host.to_string(),
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

    // The peer derives a queued lane's class and wait itself, so a lane listed
    // from another host reads like a local one.
    let mut waits_request = ingress_request.clone();
    waits_request["action"] = json!({"action":"lane_waits","targets":attempts[1..3]
        .iter()
        .map(|attempt| json!({"host":local_host.to_string(),"attempt":attempt.to_string()}))
        .collect::<Vec<_>>()});
    let waits = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.state.clone().into(),
            "fabric".into(),
            "peer".into(),
        ],
        Some(&serde_json::to_vec(&waits_request)?),
    )?;
    assert_eq!(code(&waits), 0);
    let waits: Value = serde_json::from_slice(&waits.stdout.bytes)?;
    assert_eq!(waits["result"]["result"], "lane_waits", "{waits}");
    let waits = waits["result"]["data"].as_array().ok_or("waits")?;
    assert_eq!(waits.len(), 2, "{waits:?}");
    for wait in waits {
        assert_eq!(wait["lane_class"], "read_only", "{wait}");
        assert_eq!(
            wait["waiting_for"], "launch: not yet handed to a guardian",
            "{wait}"
        );
    }

    let project_key: provenant_fabric_runtime::identity::CrossHostProjectKey =
        serde_json::from_value(json!(identity["data"]["federation"]["key"]))?;
    let mut status_record = records[0].clone();
    status_record.provider_ms = Some(1234);
    let wire_status = provenant_fabric_runtime::runs::RunResult::Status {
        attempt: status_record.clone(),
    };
    let wire = serde_json::to_value(&wire_status)?;
    assert_eq!(wire["attempt"]["provider_seconds"], 1.234);
    assert!(wire["attempt"].get("provider_ms").is_none());
    assert_eq!(
        serde_json::from_value::<provenant_fabric_runtime::runs::RunResult>(wire)?,
        wire_status
    );
    let mut retired = serde_json::to_value(&wire_status)?;
    retired["attempt"]["provider_ms"] = json!(1234);
    assert!(serde_json::from_value::<provenant_fabric_runtime::runs::RunResult>(retired).is_err());
    assert_eq!(serde_json::to_value(&status_record)?["provider_ms"], 1234);
    let mut response = provenant_fabric_runtime::hosts::PeerResponse {
        version: 1,
        protocol: provenant_fabric_runtime::hosts::PROTOCOL,
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
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n\
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
                "name":"fabric_run","arguments":{"op":"wait","targets":remote_targets,"condition":"all","wait_seconds":1}
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

    response.result = provenant_fabric_runtime::hosts::PeerResult::Run(Box::new(
        provenant_fabric_runtime::runs::RunResult::Status {
            attempt: status_record.clone(),
        },
    ));
    std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
    let replies = mcp(&rig, &attempts[..2].iter().enumerate().map(|(index, attempt)| json!({
        "jsonrpc":"2.0","id":index+1,"method":"tools/call","params":{
            "name":"fabric_run","arguments":{"op":"status","target":{"host":remote_host.to_string(),"attempt":attempt.to_string()}}
        }
    })).collect::<Vec<_>>())?;
    let matching = &replies.iter().find(|r| r["id"] == 1).ok_or("matching")?["result"];
    assert_ne!(matching["isError"], true, "{matching}");
    let mismatch = &replies.iter().find(|r| r["id"] == 2).ok_or("mismatch")?["result"];
    assert_eq!(
        mismatch["structuredContent"]["code"], "state_corrupt",
        "{mismatch}"
    );

    for (returned, expected) in [
        ("local-stop", "launch_unknown"),
        ("unrelated", "state_corrupt"),
    ] {
        response.result = provenant_fabric_runtime::hosts::PeerResult::Error {
            error: provenant_fabric_runtime::coordination::facade::FacadeError::LaunchUnknown {
                operation_id: OperationId::parse(returned)?,
            },
        };
        std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
        let replies = mcp(
            &rig,
            &[json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"fabric_run","arguments":{"op":"cancel","operation_id":"local-stop","target":{"host":remote_host.to_string(),"attempt":attempt.to_string()}}
                }
            })],
        )?;
        let result = &replies.iter().find(|r| r["id"] == 1).ok_or("error")?["result"];
        assert_eq!(result["structuredContent"]["code"], expected, "{result}");
        assert_eq!(result["structuredContent"]["operation_id"], "local-stop");
    }

    // A well-formed run reply must also match the requested operation and target.
    use provenant_fabric_runtime::{hosts::PeerResult, runs::RunResult};
    let mut other_attempt = status_record.clone();
    other_attempt.attempt_id = attempts[1];
    let mut resumed = other_attempt.clone();
    resumed.operation_id = OperationId::parse("local-resume")?;
    for (op, result, expected) in [
        (
            "cancel",
            RunResult::Cancel {
                attempt: other_attempt,
            },
            Some("state_corrupt"),
        ),
        (
            "cancel",
            RunResult::Status {
                attempt: status_record.clone(),
            },
            Some("state_corrupt"),
        ),
        (
            "cancel",
            RunResult::Cancel {
                attempt: status_record.clone(),
            },
            None,
        ),
        (
            "resume",
            RunResult::Status {
                attempt: resumed.clone(),
            },
            Some("state_corrupt"),
        ),
        (
            "resume",
            RunResult::Resume {
                attempt: status_record.clone(),
            },
            Some("state_corrupt"),
        ),
        ("resume", RunResult::Resume { attempt: resumed }, None),
    ] {
        response.result = PeerResult::Run(Box::new(result));
        std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
        let mut arguments = json!({"op":op,"operation_id":format!("local-{op}"),"target":{"host":remote_host.to_string(),"attempt":attempt.to_string()}});
        if op == "resume" {
            arguments["prompt"] = json!("continue");
            arguments["expected_generation"] = json!(0);
        }
        let replies = mcp(
            &rig,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":arguments}}),
            ],
        )?;
        let result = &replies.iter().find(|r| r["id"] == 1).ok_or("run reply")?["result"];
        if let Some(expected) = expected {
            assert_eq!(
                result["structuredContent"]["code"], expected,
                "{op}: {result}"
            );
        } else {
            assert_ne!(result["isError"], true, "{op}: {result}");
        }
    }

    use provenant_fabric_runtime::{
        coordination::facade::AttemptTarget,
        runs::{PeerWorktree, SubmissionReceipt},
    };
    let receipt = SubmissionReceipt {
        operation_id: OperationId::parse("verified-submit")?,
        targets: vec![AttemptTarget {
            host: Some(remote_host),
            attempt,
        }],
        worktree: None,
    };
    let mut wrong_id = receipt.clone();
    wrong_id.operation_id = OperationId::parse("unrelated")?;
    let mut unexpected_writer = receipt.clone();
    unexpected_writer.worktree = Some(PeerWorktree {
        path: "peer/worktree".into(),
        branch: "peer/writer".into(),
    });
    let mut empty = receipt.clone();
    empty.targets.clear();
    for (returned, corrupt) in [
        (wrong_id, true),
        (unexpected_writer, true),
        (empty, true),
        (receipt, false),
    ] {
        response.result = PeerResult::Submitted(returned);
        std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
        let replies = mcp(
            &rig,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"submit","operation_id":"verified-submit","host":remote_host.to_string(),"tasks":[{"prompt":"fixture","route":{"kind":"model","provider":"codex","model":"gpt-6-luna"},"timeout_seconds":1}]}}}),
            ],
        )?;
        let result = &replies
            .iter()
            .find(|r| r["id"] == 1)
            .ok_or("submit reply")?["result"];
        if corrupt {
            assert_eq!(text_error_code(result), "state_corrupt", "{result}");
        } else {
            assert_ne!(result["isError"], true, "{result}");
        }
    }

    use provenant_fabric_runtime::coordination::facade::FacadeError;
    for (error, expected_operation) in [
        (
            FacadeError::Io {
                operation: "submit: access database".into(),
                path: None,
            },
            "submit: access database",
        ),
        (
            FacadeError::Io {
                operation: "spawn peer".into(),
                path: None,
            },
            "submit: spawn peer",
        ),
    ] {
        response.result = PeerResult::Error { error };
        std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
        let replies = mcp(
            &rig,
            &[json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "fabric_run",
                    "arguments": {
                        "op": "submit",
                        "operation_id": "verified-submit-scope",
                        "host": remote_host.to_string(),
                        "tasks": [{
                            "prompt": "fixture",
                            "route": {"kind": "model", "provider": "codex", "model": "gpt-6-luna"},
                            "timeout_seconds": 1
                        }]
                    }
                }
            })],
        )?;
        let result = &replies
            .iter()
            .find(|r| r["id"] == 1)
            .ok_or("submit reply")?["result"];
        assert_eq!(result["isError"], true, "{result}");
        let structured = &result["structuredContent"];
        assert_eq!(structured["code"], "io", "{result}");
        assert_eq!(structured["operation"], expected_operation, "{result}");
    }

    // Return valid status records after admission, so a wrong-branch receipt
    // cannot pass this test by failing an unrelated later status request.
    let status_path = rig.root.join("ssh-status.json");
    response.result = PeerResult::RunStatuses(vec![status_record.clone()]);
    std::fs::write(&status_path, serde_json::to_vec(&response)?)?;
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nrequest=$(cat)\ncase \"$request\" in *'\"action\":\"run_statuses\"'*) cat '{}' ;; *) cat '{}' ;; esac\n",
            status_path.display(),
            response_path.display(),
        ),
    )?;
    let prompt = rig.root.join("verified-prompt.txt");
    std::fs::write(&prompt, "fixture")?;
    for (branch, expected) in [
        ("peer/unrelated", Some("STATE_CORRUPT")),
        ("peer/expected", None),
    ] {
        response.result = PeerResult::Submitted(SubmissionReceipt {
            operation_id: OperationId::parse("verified-dispatch")?,
            targets: vec![AttemptTarget {
                host: Some(remote_host),
                attempt,
            }],
            worktree: Some(PeerWorktree {
                path: "peer/worktree".into(),
                branch: branch.into(),
            }),
        });
        std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
        let (status, result) = json_command(
            &rig,
            &[
                "dispatch",
                "--host",
                "fixture",
                "--mode",
                "worktree-write",
                "--branch",
                "peer/expected",
                "--base",
                "origin/main",
                "--model",
                "codex/gpt-6-luna",
                "--operation-id",
                "verified-dispatch",
                "--prompt-file",
                prompt.to_str().ok_or("prompt")?,
                "--json",
            ],
        )?;
        if let Some(expected) = expected {
            assert_ne!(status, 0, "{result}");
            assert_eq!(result["error"]["code"], expected, "{result}");
        } else {
            assert_eq!(status, 0, "{result}");
            assert_eq!(result["data"]["worktree"]["branch"], "peer/expected");
        }
    }

    let event_limit = {
        let store = provenant_run::RunStore::open(&context, &call)?;
        let events = store.events(0, 64, None, &call)?;
        assert!(events.len() < 64);
        u16::try_from(events.len())?
    };
    assert!((1..=64).contains(&event_limit));
    let run_events_request = json!({
        "version": 1, "protocol":5,
        "target_host": local_host.to_string(),
        "source_host": remote_host.to_string(),
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

    // Landing uses the frozen local ID for uncertainty and rejects IDs from
    // another request, including successful receipts and generic refusals.
    let config_path = rig.state.join("config.toml");
    let mut config = std::fs::read_to_string(&config_path)?;
    config.push_str(&format!(
        "\n[domains.fabric.home_hosts]\n'{}'='{}'\n",
        identity["data"]["federation"]["key"]
            .as_str()
            .ok_or("project key")?,
        remote_host
    ));
    std::fs::write(config_path, config)?;
    rig.env.push((
        "PROVENANT_LANDING_TOKEN".into(),
        "00000000000000000000000000000000".into(),
    ));
    let push = provenant_fabric_store::PushOperation {
        header: provenant_records::RecordHeader::V1,
        id: OperationId::parse("landing-local")?,
        digest: Digest::from_bytes([1; 32]),
        request: provenant_fabric_store::PushRequest {
            header: provenant_records::RecordHeader::V1,
            project,
            claim: provenant_core::ClaimId::from_bytes([44; 16]),
            actor,
            generation: 1,
            reference: "refs/heads/main".into(),
            expected: provenant_core::GitOid::parse("1111111111111111111111111111111111111111")?,
            candidate: provenant_core::GitOid::parse("2222222222222222222222222222222222222222")?,
            remote: "origin".into(),
        },
        phase: provenant_fabric_store::PushPhase::Committed,
        observed_remote: None,
        updated_at: clock.wall_now()?,
    };
    let mut wrong_push = push.clone();
    wrong_push.id = OperationId::parse("unrelated")?;
    let replies = [
        (PeerResult::Push(push), None),
        (PeerResult::Push(wrong_push), Some("state_corrupt")),
        (
            PeerResult::Error {
                error: provenant_fabric_runtime::coordination::facade::FacadeError::Busy,
            },
            Some("QUEUE_FULL"),
        ),
        (
            PeerResult::Error {
                error: provenant_fabric_runtime::coordination::facade::FacadeError::StaleFence,
            },
            Some("landing_refused"),
        ),
        (
            PeerResult::Error {
                error: provenant_fabric_runtime::coordination::facade::FacadeError::LaunchUnknown {
                    operation_id: OperationId::parse("unrelated")?,
                },
            },
            Some("state_corrupt"),
        ),
        (
            PeerResult::Error {
                error: provenant_fabric_runtime::coordination::facade::FacadeError::LaunchUnknown {
                    operation_id: OperationId::parse("landing-local")?,
                },
            },
            Some("launch_unknown"),
        ),
    ];
    for (reply, expected) in replies {
        response.result = reply;
        std::fs::write(&response_path, serde_json::to_vec(&response)?)?;
        for op in ["push", "reconcile"] {
            let (status, result) = json_command(
                &rig,
                &[
                    "fabric",
                    "landing",
                    op,
                    "--reference",
                    "refs/heads/main",
                    "--id",
                    &provenant_core::ClaimId::from_bytes([44; 16]).to_string(),
                    "--generation",
                    "1",
                    "--operation-id",
                    "landing-local",
                    "--json",
                ],
            )?;
            if let Some(expected) = expected {
                assert_ne!(status, 0, "{op}: {result}");
                assert_eq!(result["error"]["code"], expected, "{op}: {result}");
                if expected == "launch_unknown" {
                    assert_eq!(result["data"]["operation_id"], "landing-local");
                }
                if expected == "QUEUE_FULL" {
                    assert_eq!(result["status"], "refused", "{op}: {result}");
                    assert_eq!(result["data"]["operation_id"], "landing-local");
                    let repair = result["error"]["repair"].as_str().ok_or("repair")?;
                    assert!(repair.contains("same fence and operation id"), "{repair}");
                    assert!(repair.contains(&format!("fabric landing {op}")), "{repair}");
                }
            } else {
                assert_eq!(status, 0, "{op}: {result}");
            }
        }
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn lane_graph_status_does_not_create_shared_graph_store() -> Result {
    let mut rig = TestRig::new()?;
    assert_eq!(code(&rig.command(&["fabric", "identity", "--json"])?), 0);
    rig.env
        .push((provenant_run::LANE_DEPTH_ENV.into(), "1".into()));
    let output = rig.command(&["fabric", "graph", "status", "missing", "--json"])?;
    assert_ne!(code(&output), 0);
    let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(result["error"]["code"], "not_found", "{result}");
    assert!(!rig.state.join("fabric/graphs").exists());
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_graph","arguments":{"op":"status","id":"missing"}}}),
        ],
    )?;
    let result = &replies.iter().find(|r| r["id"] == 1).ok_or("status")?["result"];
    assert_eq!(result["structuredContent"]["code"], "not_found", "{result}");
    assert!(!rig.state.join("fabric/graphs").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn lane_cli_cancel_refuses_peer_placement() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = next_host(identity["data"]["host"].as_str().ok_or("host")?.parse()?)?;
    let run = "11111111111111111111111111111111";
    let directory = rig.state.join("fabric/remote-runs");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    std::fs::write(
        directory.join(format!("{run}.json")),
        serde_json::to_vec(&json!({
            "schema_version":1, "host":host, "project":identity["data"]["federation"]["key"],
            "operation_id":"placement", "run_id":run, "attempts":[],
        }))?,
    )?;
    rig.env
        .push((provenant_run::LANE_DEPTH_ENV.into(), "1".into()));
    let output = rig.command(&["cancel", run, "--operation-id", "lane-cancel", "--json"])?;
    assert_ne!(code(&output), 0);
    let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        result["error"]["code"], "lane_coordination_refused",
        "{result}"
    );
    assert_eq!(result["status"], "refused", "{result}");
    rig.assert_legacy_unchanged()
}

#[test]
fn lane_mcp_refuses_coordination_but_allows_reads_and_child_submission() -> Result {
    for (depth, child_store) in [(Some("1"), true), (None, true), (Some("1"), false)] {
        let mut rig = TestRig::in_directory(&std::env::temp_dir())?;
        let cleanup = rig.process_cleanup();
        let identity = rig.command(&["fabric", "identity", "--json"])?;
        assert_eq!(code(&identity), 0);
        let host_identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
        let host = host_identity["data"]["host"].as_str().ok_or("host")?;
        let peer = next_host(host.parse()?)?;
        assert_eq!(
            code(&rig.command(&["fabric", "events", "read", "--json"])?),
            0
        );
        let database = rig.state.join("fabric/fabric.sqlite3");
        let cursor = || -> Result<i64> {
            Ok(rusqlite::Connection::open(&database)?.query_row(
                "SELECT COALESCE(SUM(sequence), 0) FROM run_event_cursors",
                [],
                |row| row.get(0),
            )?)
        };
        let before = cursor()?;
        let parent = "11111111111111111111111111111111";
        let child_state = rig.state.join("children").join(parent);
        std::fs::create_dir_all(&child_state)?;
        std::fs::set_permissions(
            rig.state.join("children"),
            std::fs::Permissions::from_mode(0o700),
        )?;
        std::fs::set_permissions(&child_state, std::fs::Permissions::from_mode(0o700))?;
        for (key, value) in &mut rig.env {
            if key == "PROVENANT_AGENT_LABEL" {
                *value = format!("lane-{parent}").into();
            }
        }
        rig.env
            .push((provenant_run::PARENT_ATTEMPT_ENV.into(), parent.into()));
        if child_store {
            rig.env.push((
                provenant_core::RUN_STATE_DIRECTORY_ENV.into(),
                child_state.clone().into(),
            ));
        }
        if let Some(depth) = depth {
            rig.env
                .push((provenant_run::LANE_DEPTH_ENV.into(), depth.into()));
        }
        let calls = [
            (
                "fabric_run",
                json!({"op":"cancel","target":{"host":peer,"attempt":parent},"operation_id":"lane-peer-cancel"}),
            ),
            (
                "fabric_run",
                json!({"op":"resume","target":{"host":peer,"attempt":parent},"operation_id":"lane-peer-resume","prompt":"continue","expected_generation":0}),
            ),
            (
                "fabric_graph",
                json!({"op":"run","document":"missing.json"}),
            ),
            ("fabric_graph", json!({"op":"cancel","id":"g-lane"})),
            (
                "fabric_graph",
                json!({"op":"resume","document":"missing.json"}),
            ),
            (
                "fabric_run",
                json!({"op":"submit","operation_id":"lane-host","host":host,"tasks":[{"prompt":"read only","route":{"kind":"model","provider":"codex","model":"gpt-6.1-sol"},"timeout_seconds":1}]}),
            ),
            (
                "fabric_mail",
                json!({"op":"send","recipients":["11111111111111111111111111111111"],"kind":"note","body":"injected","artifacts":[],"operation_id":"lane-mail"}),
            ),
            (
                "fabric_mail",
                json!({"op":"mark_read","through":1,"operation_id":"lane-read"}),
            ),
            (
                "fabric_task",
                json!({"op":"create","objective":"injected","operation_id":"lane-task"}),
            ),
            (
                "fabric_task",
                json!({"op":"update","proof":{"task":parent,"generation":1,"token":"00000000000000000000000000000000"},"objective":"injected","operation_id":"lane-task-update"}),
            ),
            (
                "fabric_lease",
                json!({"op":"claim_acquire","scope":{"kind":"issue","key":"lane"},"lease_seconds":1,"operation_id":"lane-claim"}),
            ),
            (
                "fabric_lease",
                json!({"op":"landing_acquire","reference":"refs/heads/main","expected":"1111111111111111111111111111111111111111","candidate":"2222222222222222222222222222222222222222","lease_seconds":1,"operation_id":"lane-land"}),
            ),
            (
                "fabric_events",
                json!({"op":"note","note":"injected","operation_id":"lane-note"}),
            ),
        ];
        let mut frames = calls.iter().enumerate().map(|(id, (tool, arguments))| {
            json!({"jsonrpc":"2.0","id":id+1,"method":"tools/call","params":{"name":tool,"arguments":arguments}})
        }).collect::<Vec<_>>();
        frames.extend([
            json!({"jsonrpc":"2.0","id":100,"method":"tools/call","params":{"name":"fabric_mail","arguments":{"op":"read","limit":10}}}),
            json!({"jsonrpc":"2.0","id":102,"method":"tools/call","params":{"name":"fabric_context","arguments":{"op":"inspect"}}}),
            json!({"jsonrpc":"2.0","id":103,"method":"tools/call","params":{"name":"fabric_events","arguments":{"op":"read","limit":10}}}),
            json!({"jsonrpc":"2.0","id":104,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"cancel","target":{"host":host,"attempt":parent},"operation_id":"lane-local-cancel"}}}),
            json!({"jsonrpc":"2.0","id":105,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"resume","target":{"host":host,"attempt":parent},"operation_id":"lane-local-resume","prompt":"continue","expected_generation":0}}}),
            json!({"jsonrpc":"2.0","id":106,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"resume","target":{"attempt":parent},"operation_id":"lane-default-resume","prompt":"continue","expected_generation":0}}}),
        ]);
        if depth.is_some() && child_store {
            frames.push(json!({"jsonrpc":"2.0","id":101,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"submit","operation_id":"lane-child","wait_seconds":1,"tasks":[{"prompt":"read only","route":{"kind":"model","provider":"codex","model":"gpt-6.1-sol"},"timeout_seconds":1}],"concurrency":1}}}));
        }
        let replies = mcp(&rig, &frames)?;
        for id in 1..=calls.len() {
            let result = &replies.iter().find(|r| r["id"] == id).ok_or("mutation")?["result"];
            assert_eq!(result["isError"], true, "{result}");
            if calls[id - 1].1["op"] == "submit" {
                assert_eq!(
                    text_error_code(result),
                    "lane_coordination_refused",
                    "{result}"
                );
            } else {
                assert_eq!(
                    result["structuredContent"]["code"], "lane_coordination_refused",
                    "{result}"
                );
            }
        }
        for id in [100, 102, 103]
            .into_iter()
            .chain((depth.is_some() && child_store).then_some(101))
        {
            let result = &replies.iter().find(|r| r["id"] == id).ok_or("allowed")?["result"];
            assert_ne!(result["isError"], true, "{result}");
        }
        let identity = &replies.iter().find(|r| r["id"] == 102).ok_or("identity")?["result"]["structuredContent"];
        assert_eq!(identity["inbox"]["agent"], format!("lane-{parent}"));
        let events = &replies.iter().find(|r| r["id"] == 103).ok_or("events")?["result"]["structuredContent"];
        assert!(
            events["unknown_hosts"]
                .as_array()
                .ok_or("coverage")?
                .contains(&json!(host)),
            "{events}"
        );
        // Explicit local hosts in child cancel receipts still reach the local owner.
        let result = &replies
            .iter()
            .find(|r| r["id"] == 104)
            .ok_or("local cancel")?["result"];
        assert_ne!(
            result["structuredContent"]["code"], "lane_coordination_refused",
            "{result}"
        );
        for id in [105, 106] {
            let result = &replies
                .iter()
                .find(|r| r["id"] == id)
                .ok_or("local resume")?["result"];
            assert_eq!(result["isError"], true, "{result}");
            assert_eq!(
                result["structuredContent"]["code"], "continuation_unsupported",
                "{result}"
            );
        }
        if depth.is_some() && child_store {
            assert!(child_state.join("runs").is_dir());
        }
        assert!(!rig.state.join("runs").exists());
        let output = rig.command(&[
            "fabric",
            "task",
            "create",
            "--objective",
            "injected",
            "--operation-id",
            "lane-cli-task",
            "--json",
        ])?;
        assert_ne!(code(&output), 0);
        let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(
            result["error"]["code"], "lane_coordination_refused",
            "{result}"
        );
        let prompt = rig.root.join("lane-prompt.txt");
        std::fs::write(&prompt, "read only")?;
        for args in [
            vec!["fabric", "graph", "cancel", "g-lane", "--json"],
            vec![
                "dispatch",
                "--host",
                "workshop",
                "--operation-id",
                "lane-dispatch",
                "--mode",
                "read-only",
                "--model",
                "codex/gpt-6-luna",
                "--prompt-file",
                prompt.to_str().ok_or("prompt")?,
                "--json",
            ],
            vec![
                "dispatch",
                "--host",
                "local",
                "--operation-id",
                "lane-local-dispatch",
                "--mode",
                "read-only",
                "--model",
                "codex/gpt-6-luna",
                "--prompt-file",
                prompt.to_str().ok_or("prompt")?,
                "--json",
            ],
            vec![
                "workflow",
                "session",
                "handoff",
                "--to",
                host,
                "--account",
                "fixture",
                "--json",
            ],
        ] {
            let output = rig.command(&args)?;
            assert_ne!(code(&output), 0, "{args:?}");
            let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
            assert_eq!(
                result["error"]["code"], "lane_coordination_refused",
                "{args:?}: {result}"
            );
            assert_eq!(result["status"], "refused", "{args:?}: {result}");
        }
        let target = format!("{parent}/{parent}/{parent}");
        let output = rig.command(&[
            "resume",
            &target,
            "--operation-id",
            "lane-cli-resume",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--json",
        ])?;
        assert_ne!(code(&output), 0);
        let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(result["error"]["code"], "UNSUPPORTED", "{result}");
        assert_eq!(result["status"], "refused", "{result}");
        assert!(!rig.state.join("runs").exists());
        assert!(!rig.state.join("fabric/graphs").exists());
        let output = rig.command(&["fabric", "events", "read", "--json"])?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let events: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert!(
            events["data"]["unknown_hosts"]
                .as_array()
                .ok_or("coverage")?
                .contains(&json!(host)),
            "{events}"
        );
        assert_eq!(
            cursor()?,
            before,
            "lane read advanced the host run-event cursor"
        );
        // A peer frame names its own agent, so lanes may not serve one. The
        // forwarding path must refuse before it connects to the agent socket.
        let agent = rig.state.join("peer-agent");
        std::fs::create_dir_all(&agent)?;
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700))?;
        let listener = std::os::unix::net::UnixListener::bind(agent.join("peer.sock"))
            .map_err(|error| format!("bind lane peer test socket: {error}"))?;
        listener.set_nonblocking(true)?;
        for args in [
            &["fabric", "peer", "--in-process"][..],
            &["fabric", "peer"],
            &["fabric", "peer-agent"],
        ] {
            let output = rig.command(args)?;
            assert_eq!(code(&output), 3, "{args:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr.bytes)
                    .contains("coordination mutations are not allowed inside a lane"),
                "{args:?}"
            );
        }
        assert_eq!(
            listener.accept().map(|_| ()).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::WouldBlock)
        );
        drop(listener);
        std::fs::remove_dir_all(&agent)?;
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
    Ok(())
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
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"submit","operation_id":"nested-depth","tasks":[{"prompt":"read only","route":{"kind":"model","provider":"codex","model":"gpt-6.1-sol"},"timeout_seconds":1}],"concurrency":1}}}),
        ],
    )?;
    let result = &replies.iter().find(|r| r["id"] == 1).ok_or("submission")?["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(text_error_code(result), "lane_depth", "{result}");
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
    let task =
        json!({"prompt":"fixture", "title":"Review fixture", "route":route,"timeout_seconds":60});
    let mut arguments = json!({"op":"submit","operation_id":operation,"wait_seconds":1,"tasks":[task.clone(),task.clone(),task.clone(),task],"knobs":knobs});
    if let Some(n) = concurrency {
        arguments["concurrency"] = n.into();
    }
    let replies = mcp(
        rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":arguments}}),
        ],
    )?;
    let result = &replies.iter().find(|r| r["id"] == 1).ok_or("submission")?["result"];
    assert_ne!(result["isError"], true, "{result}");
    let target = result["structuredContent"]["receipt"]["targets"][0].clone();
    let replies = mcp(
        rig,
        &[
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"status","target":target,"detail":true}}}),
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
    let cleanup = rig.process_cleanup();
    let (request, attempt) = submitted_policy(
        &rig,
        "named-tier",
        None,
        &[],
        json!({"kind":"named","name":"senior"}),
    )?;
    assert_eq!(request.tasks[0].title.as_deref(), Some("Review fixture"));
    assert_eq!(attempt["title"], "Review fixture");
    assert_eq!(
        request.tasks[0].route,
        provenant_records::run::RequestedRoute::Named {
            name: "senior".into(),
            effort: None,
        }
    );
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn mcp_submission_stamps_the_local_host_on_its_trace() -> Result {
    let rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let (_, attempt) = submitted_policy(
        &rig,
        "trace-host",
        None,
        &[],
        json!({"kind":"named","name":"senior"}),
    )?;
    assert_eq!(attempt["trace"]["host"], identity["data"]["host"]);
    assert_eq!(attempt["trace"]["attempt"], attempt["attempt_id"]);
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
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
        .find(|tool| tool["name"] == "fabric_run")
        .ok_or("fabric_run")?;
    assert!(!submit["inputSchema"].to_string().contains("inputs"));
    assert!(!submit["inputSchema"].to_string().contains("task_class"));

    let reply = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"submit","operation_id":"unsupported-inputs","tasks":[{"prompt":"fixture","inputs":[],"route":{"kind":"named","name":"senior"},"timeout_seconds":60}]}}}),
        ],
    )?;
    let call = reply.iter().find(|reply| reply["id"] == 2).ok_or("call")?;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert_eq!(text_error_code(&call["result"]), "invalid_input");

    let reply = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"submit","operation_id":"no-task-class","tasks":[{"prompt":"fixture","route":{"kind":"named","name":"senior"},"task_class":"mechanical","timeout_seconds":60}]}}}),
        ],
    )?;
    let call = reply.iter().find(|reply| reply["id"] == 3).ok_or("call")?;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert_eq!(text_error_code(&call["result"]), "invalid_input");
    rig.assert_legacy_unchanged()
}

#[test]
fn concurrency_knob_controls_admission_unless_dispatch_is_explicit() -> Result {
    let rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    std::fs::create_dir_all(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\nprofile='rigorous'\n[gc]\nfree_floor='1B'\n[knobs]\n'lanes.max_concurrency'=3\n",
    )?;
    for (operation, explicit, knobs, expected) in [
        ("config-capacity", None, vec![], 3),
        (
            "dispatch-capacity",
            None,
            vec!["lanes.max_concurrency=2", "review.extra_reviewers=2"],
            2,
        ),
        (
            "explicit-capacity",
            Some(1),
            vec!["lanes.max_concurrency=2"],
            1,
        ),
        // Four tasks and an explicit 16: above the task count, still accepted.
        ("wide-capacity", Some(16), vec![], 16),
    ] {
        let (request, attempt) = submitted_policy(
            &rig,
            operation,
            explicit,
            &knobs,
            json!({"kind":"model","provider":"codex","model":"fixture"}),
        )?;
        assert_eq!(request.concurrency, expected);
        let task = &request.tasks[0];
        assert_eq!(serde_json::to_value(&task.profile)?, attempt["profile"]);
        assert_eq!(task.profile.overrides, knobs);
        assert_eq!(
            task.profile.knobs["review.extra_reviewers"],
            if operation == "dispatch-capacity" {
                2
            } else {
                1
            }
        );
    }
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn silence_knob_controls_capture_plan_observations() -> Result {
    let rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    std::fs::create_dir_all(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[knobs]\n'lanes.silence_seconds'=2\n",
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
            json!({"kind":"model","provider":"codex","model":"fixture"}),
        )?;
        let plan = provenant_run::capture_plan(&request.tasks[0]);
        assert_eq!(plan.silence_ms, threshold);
        assert!(!plan.silent(threshold - 1, Some(0)));
        assert!(plan.silent(threshold, Some(0)));
        assert!(!plan.silent(threshold, Some(threshold)));
    }
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
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
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric.peers.laptop]\nhost='{local}'\ndestination='laptop'\n"
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
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n\
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
fn writer_disk_refusal_keeps_its_reason_through_graph_mcp_and_dispatch() -> Result {
    let rig = TestRig::new()?;
    let commit = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "Initial",
        ],
        None,
    )?;
    assert_eq!(
        code(&commit),
        0,
        "{}",
        String::from_utf8_lossy(&commit.stderr.bytes)
    );
    let (status, created) = json_command(
        &rig,
        &["worktree", "create", "low-disk", "--base", "HEAD", "--json"],
    )?;
    assert_eq!(status, 0, "{created}");
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='18446744073709551615B'\n",
    )?;
    let worktree = rig.project.join(".worktrees/low-disk");
    let worktree = worktree.to_str().ok_or("worktree path")?;
    let route = json!({"kind":"model","provider":"codex","model":"gpt-6-luna"});
    std::fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({"schema_version":1,"graph_id":"low-disk","nodes":[
            {"id":"write","prompt":"fixture","access":"worktree_write","worktree":worktree,
             "route":{"adapter":"codex","model":"codex/gpt-6-luna","effort":"low"}}
        ]}))?,
    )?;
    let reason = |message: &Value| -> Result {
        let message = message.as_str().ok_or("refusal reason")?;
        assert!(message.contains("insufficient disk space on "), "{message}");
        assert!(
            message.contains(worktree)
                || message.contains(rig.state.to_str().ok_or("state")?)
                || message.contains(rig.home.to_str().ok_or("home")?),
            "{message}"
        );
        let (_, available) = message.rsplit_once(": ").ok_or("available bytes")?;
        let (bytes, _) = available
            .split_once(" bytes available, floor ")
            .ok_or("floor")?;
        assert!(bytes.parse::<u64>()? < u64::MAX, "{message}");
        assert!(
            message.contains("floor 18446744073709551615 bytes"),
            "{message}"
        );
        assert!(message.contains("provenant gc"), "{message}");
        Ok(())
    };
    let (status, graph) = json_command(
        &rig,
        &[
            "fabric",
            "graph",
            "run",
            "--document",
            "graph.json",
            "--json",
        ],
    )?;
    assert_eq!(status, 1, "{graph}");
    assert_eq!(graph["status"], "refused", "{graph}");
    assert_eq!(graph["error"]["code"], "dispatch_refused", "{graph}");
    reason(&graph["error"]["message"])?;

    // Submission is exposed as fabric_run op=submit in the current MCP schema.
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"fabric_run","arguments":{"op":"submit","operation_id":"low-disk-mcp","tasks":[
                    {"prompt":"fixture","route":route,"access":"worktree_write","worktree":worktree,"timeout_seconds":60}
                ]}
            }}),
        ],
    )?;
    let result = &replies.iter().find(|r| r["id"] == 1).ok_or("MCP reply")?["result"];
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(
        result["structuredContent"]["code"], "dispatch_refused",
        "{result}"
    );
    reason(&result["structuredContent"]["message"])?;

    let prompt = rig.root.join("prompt.txt");
    std::fs::write(&prompt, "fixture")?;
    let (status, dispatch) = json_command(
        &rig,
        &[
            "dispatch",
            "--model",
            "codex/gpt-6-luna",
            "--mode",
            "worktree-write",
            "--worktree",
            worktree,
            "--operation-id",
            "low-disk-cli",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--json",
        ],
    )?;
    assert_eq!(status, 1, "{dispatch}");
    assert_eq!(dispatch["status"], "refused", "{dispatch}");
    assert_eq!(dispatch["error"]["code"], "REFUSED", "{dispatch}");
    reason(&dispatch["error"]["message"])?;
    assert!(!rig.state.join("runs").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_refusal_codes_and_protocol_mismatches_reach_cli_and_mcp() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let local = provenant_core::HostId::parse(identity["data"]["host"].as_str().ok_or("host")?)?;
    let host = next_host(local)?;
    let project = &identity["data"]["federation"]["key"];
    let ssh = rig.root.join("ssh");
    let refusal = rig.root.join("refusal.json");
    let calls = rig.root.join("ssh-calls");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nrequest=$(cat)\nprintf '%s\\n' \"$request\" >> '{}'\ncat '{}'\n",
            calls.display(),
            refusal.display(),
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n\
         [domains.fabric.peers.workshop]\nhost='{host}'\ndestination='workshop'\n",
            ssh.to_string_lossy(),
        ),
    )?;
    let frame = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name":"fabric_run", "arguments":{"op":"status","target":{
            "host":host.to_string(), "attempt":"11111111111111111111111111111111",
        }},
    }});
    for (wire, cli) in [
        ("not_terminal", "NONTERMINAL"),
        ("operation_conflict", "OPERATION_CONFLICT"),
        ("owner_unavailable", "OWNER_UNAVAILABLE"),
    ] {
        std::fs::write(
            &refusal,
            serde_json::to_vec(&json!({
                "version":1,"protocol":5, "host":host.to_string(), "project":project,
                "result":{"result":"error","data":{"error":{"code":wire}}},
            }))?,
        )?;
        let (status, output) = json_command(&rig, &["lanes", "--host", "workshop", "--json"])?;
        assert_eq!(status, 1, "{output}");
        assert_eq!(output["error"]["code"], cli, "{output}");
        let replies = mcp(&rig, std::slice::from_ref(&frame))?;
        let result = &replies.iter().find(|r| r["id"] == 1).ok_or("MCP reply")?["result"];
        assert_eq!(result["structuredContent"]["code"], wire, "{result}");
    }
    // The released v1 health response, without protocol in either place.
    std::fs::write(
        &refusal,
        serde_json::to_vec(&json!({
            "version":1, "host":host.to_string(), "project":project,
            "result":{"result":"health","data":{
                "host":host.to_string(), "available":true, "execution":true,
            }},
        }))?,
    )?;
    std::fs::write(&calls, "")?;
    let expected = "peer workshop speaks protocol 1, this host speaks 5; upgrade both";
    let (status, output) = json_command(&rig, &["lanes", "--host", "workshop", "--json"])?;
    assert_eq!(status, 1, "{output}");
    assert_eq!(output["error"]["code"], "PEER_VERSION", "{output}");
    assert!(
        output["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(expected)),
        "{output}"
    );
    let hosts = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"fabric_context", "arguments":{"op":"hosts","limit":10},
    }});
    let replies = mcp(&rig, &[frame, hosts])?;
    let result = &replies.iter().find(|r| r["id"] == 1).ok_or("MCP reply")?["result"];
    assert_eq!(
        result["structuredContent"]["code"], "peer_version",
        "{result}"
    );
    assert!(
        result["content"][0]["text"]
            .as_str()
            .is_some_and(|m| m.contains(expected)),
        "{result}"
    );
    let result = &replies
        .iter()
        .find(|r| r["id"] == 2)
        .ok_or("MCP hosts reply")?["result"];
    let hosts = result["structuredContent"]["hosts"]
        .as_array()
        .ok_or("host list")?;
    assert_eq!(hosts.len(), 2, "{result}");
    let remote = hosts
        .iter()
        .find(|entry| entry["host"] == host.to_string())
        .ok_or("remote")?;
    assert_eq!(remote["available"], false);
    assert_eq!(remote["protocol"], 1);
    assert_eq!(remote["reason"], "peer_version");
    let local_health = hosts
        .iter()
        .find(|entry| entry["host"] == local.to_string())
        .ok_or("local")?;
    assert_eq!(local_health["available"], true);
    assert_eq!(local_health["protocol"], 5);
    let calls = std::fs::read_to_string(calls)?;
    assert_eq!(calls.lines().count(), 3);
    let actions = calls
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(actions[0]["action"]["action"], "lanes");
    // MCP workers run the two tool calls concurrently, so their order varies.
    let mut concurrent = [&actions[1], &actions[2]].map(|call| call["action"]["action"].clone());
    concurrent.sort_by_key(|action| action.to_string());
    assert_eq!(concurrent, ["health", "run"]);
    for call in actions {
        assert_eq!(call["protocol"], 5);
    }
    let (status, output) = json_command(&rig, &["hosts", "--json"])?;
    assert_eq!(status, 0, "{output}");
    let remote = output["data"]
        .as_array()
        .ok_or("CLI host list")?
        .iter()
        .find(|entry| entry["host"] == host.to_string())
        .ok_or("remote")?;
    assert_eq!(remote["available"], false);
    assert_eq!(remote["protocol"], 1);
    assert_eq!(remote["reason"], "peer_version");
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_terminal_missing_output_is_not_found() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    loopback_peer(&mut rig)?;
    let prompt = rig.root.join("prompt.txt");
    std::fs::write(&prompt, "reply OK")?;
    // No provider executable is on the peer's isolated PATH.
    let (_, placed) = json_command(
        &rig,
        &[
            "dispatch",
            "--host",
            "workshop",
            "--mode",
            "read-only",
            "--model",
            "codex/gpt-6-luna",
            "--operation-id",
            "remote-missing-output",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--wait",
            "30",
            "--json",
        ],
    )?;
    let lane = &placed["data"]["lanes"][0];
    assert_eq!(lane["state"], "rejected", "{placed}");
    let target = format!(
        "{}/{}/{}",
        placed["data"]["run_id"].as_str().ok_or("run")?,
        lane["task_id"].as_str().ok_or("task")?,
        lane["attempt_id"].as_str().ok_or("attempt")?
    );
    let (status, output) = json_command(&rig, &["output", &target, "--part", "stderr", "--json"])?;
    assert_ne!(status, 0, "{output}");
    assert_eq!(output["error"]["code"], "NOT_FOUND", "{output}");
    assert!(
        output["error"]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("peer:")),
        "{output}"
    );
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn peer_dispatch_refuses_an_oversized_frame_before_contact() -> Result {
    let mut rig = TestRig::new()?;
    loopback_peer(&mut rig)?;
    let prompt = rig.root.join("prompt.txt");
    // The prompt fits the launch limit, but JSON escaping exceeds the frame cap.
    std::fs::write(&prompt, "\"".repeat(600 * 1024))?;
    for _ in 0..2 {
        let (status, refused) = json_command(
            &rig,
            &[
                "dispatch",
                "--host",
                "workshop",
                "--mode",
                "read-only",
                "--model",
                "agy/gemini-3.8-flash",
                "--operation-id",
                "peer-frame-limit",
                "--prompt-file",
                prompt.to_str().ok_or("prompt path")?,
                "--json",
            ],
        )?;
        assert_ne!(status, 0);
        assert_eq!(refused["error"]["code"], "INVALID", "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("1 MiB")),
            "{refused}"
        );
    }
    assert!(!rig.root.join("peer-stderr").exists());
    assert!(!rig.root.join("peer-state/runs").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_dispatch_launches_a_large_argv_prompt() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    let backend = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    );
    if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some()
        && let Err(error) = &backend
    {
        return Err(error.to_string().into());
    }
    let bin = rig.root.join("provider/bin");
    std::fs::create_dir_all(&bin)?;
    std::fs::write(
        bin.join("agy"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'agy 1.0.0'; exit 0; fi\n\
         while [ \"$#\" -gt 0 ] && [ \"$1\" != --print ]; do shift; done\n\
         [ \"$#\" -gt 1 ] || exit 2\nshift\n\
         task=${1#Read the file at }\n\
         task=${task% first and treat its full contents as your task instructions.}\n\
         [ \"$(wc -c <\"$task\")\" -gt 119808 ] || exit 3\n\
         echo launched >>\"$HOME/launch-count\"\necho 'peer large prompt fixture' >&2\n",
    )?;
    std::fs::set_permissions(bin.join("agy"), std::fs::Permissions::from_mode(0o755))?;
    std::fs::create_dir_all(rig.home.join("Library/Keychains"))?;
    std::fs::write(
        rig.home.join("Library/Keychains/login.keychain-db"),
        "fixture",
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let peer = loopback_peer(&mut rig)?;
    let prompt = rig.root.join("prompt.txt");
    let text = "x".repeat(117 * 1024);
    std::fs::write(&prompt, &text)?;
    let dispatch = [
        "dispatch",
        "--host",
        "workshop",
        "--mode",
        "read-only",
        "--model",
        "agy/gemini-3.8-flash",
        "--effort",
        "high",
        "--operation-id",
        "peer-large-prompt",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
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
    let lane = &placed["data"]["lanes"][0];
    match &backend {
        Ok(_) => {
            assert_ne!(lane["state"], "rejected", "{placed}");
            assert_eq!(lane["exit_code"], 0, "{placed}");
        }
        Err(error) => {
            // Unsupported or nested sandboxes still create a definite receipt.
            assert_eq!(lane["state"], "rejected", "{placed}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains(&error.to_string())),
                "{placed}"
            );
        }
    }
    let run = placed["data"]["run_id"].as_str().ok_or("run")?;
    let attempt = lane["attempt_id"].as_str().ok_or("attempt")?;
    let sealed = std::fs::read_to_string(rig.root.join(format!(
        "peer-state/runs/{run}/attempts/{attempt}/prompt.md"
    )))?;
    assert!(sealed.ends_with(&text));
    let (_, replayed) = json_command(&rig, &dispatch)?;
    assert_eq!(replayed["data"]["run_id"], run, "{replayed}");
    assert_eq!(
        replayed["data"]["lanes"][0]["attempt_id"], attempt,
        "{replayed}"
    );
    assert_eq!(
        replayed["data"]["lanes"][0]["state"], lane["state"],
        "{replayed}"
    );
    assert_eq!(
        replayed["data"]["lanes"][0]["reason"], lane["reason"],
        "{replayed}"
    );
    if backend.is_ok() {
        let launches = rig.root.join(format!(
            "peer-state/runs/{run}/attempts/{attempt}/provider-state/candidate-0/launch-count"
        ));
        assert_eq!(std::fs::read_to_string(launches)?, "launched\n");
    }
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn read_only_dispatch_places_on_a_peer_and_forwards_by_id() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    let backend = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    );
    if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some()
        && let Err(error) = &backend
    {
        return Err(error.to_string().into());
    }
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
    // A named attempt counts whenever it settled; a run would count only
    // settlements after the wait starts.
    let task = lane["task_id"].as_str().ok_or("task")?;
    let target = format!("{run}/{task}/{attempt}");
    let (status, waited) =
        json_command(&rig, &["lanes", "--id", &target, "--wait", "5", "--json"])?;
    assert_eq!(status, 0, "{waited}");
    assert_eq!(waited["data"]["condition_met"], true);

    let peer_text = peer.to_string();
    let (status, stderr) = json_command(&rig, &["output", &target, "--part", "stderr", "--json"])?;
    if backend.is_ok() {
        assert_eq!(status, 0, "{stderr} {placed}");
        assert_eq!(stderr["data"]["host"], peer.to_string());
        assert_eq!(stderr["data"]["offset"], 0);
        assert!(
            stderr["data"]["bytes"]["data"]
                .as_str()
                .is_some_and(|text| text.contains("peer stderr fixture")),
            "{stderr}"
        );
    } else {
        // Unsupported or nested sandboxes reject before captured output
        // exists. The peer reports the terminal attempt's missing output.
        assert_eq!(lane["state"], "rejected", "{placed}");
        let error = backend.as_ref().err().ok_or("backend error")?;
        assert!(
            lane["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(&error.to_string())),
            "{placed}"
        );
        assert_eq!(stderr["error"]["code"], "NOT_FOUND", "{stderr}");
        assert!(
            stderr["error"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("peer:")),
            "{stderr}"
        );
    }
    let status_of_stderr = status;
    let (status, tail) = json_command(&rig, &["output", &target, "--tail", "5", "--json"])?;
    assert_ne!(status, 0);
    assert_eq!(tail["error"]["code"], "UNSUPPORTED", "{tail}");
    // The peer's own attempt is reachable without a placement: `--host`, and
    // the `HOST/ATTEMPT` form of a graph `attempt_ref` (#1837).
    let by_host = format!("{peer}/{attempt}");
    for words in [
        vec!["output", &attempt, "--host", "workshop"],
        vec!["output", &attempt, "--host", &peer_text],
        vec!["output", &by_host],
    ] {
        let mut words = words;
        words.extend(["--part", "stderr", "--json"]);
        let (direct_status, direct) = json_command(&rig, &words)?;
        assert_eq!(direct_status, status_of_stderr, "{words:?} {direct}");
        if backend.is_ok() {
            assert_eq!(direct["data"]["host"], peer_text, "{words:?} {direct}");
            assert_eq!(direct["data"]["bytes"], stderr["data"]["bytes"], "{direct}");
        } else {
            assert_eq!(direct["error"]["code"], "NOT_FOUND", "{words:?} {direct}");
        }
    }

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
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
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
    let executable = provenant_os::git::git_executable().ok_or("git executable")?;
    let output = rig.execute(&executable, &argv, None)?;
    assert_eq!(
        code(&output),
        0,
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
}

/// A Git registration and an advisory claim do not grant writer custody.
#[test]
fn submit_git_worktree_names_the_missing_provenant_registration() -> Result {
    let rig = TestRig::new()?;
    git(
        &rig,
        &rig.project,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    )?;
    let worktree = rig.project.join(".worktrees/git-writer");
    git(
        &rig,
        &rig.project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "git-writer",
            worktree.to_str().ok_or("worktree")?,
        ],
    )?;
    let claim = rig.command(&[
        "fabric",
        "claim",
        "acquire",
        "--worktree",
        worktree.to_str().ok_or("worktree")?,
        "--operation-id",
        "writer-claim",
        "--json",
    ])?;
    assert_eq!(
        code(&claim),
        0,
        "{}",
        String::from_utf8_lossy(&claim.stderr.bytes)
    );
    for deliver in [None, Some(json!({"remote":"origin","branch":"git-writer"}))] {
        let mut task = json!({
            "prompt":"commit the fix", "access":"worktree_write", "worktree":worktree,
            "route":{"kind":"model","provider":"codex","model":"gpt-6.1-sol","effort":"high"},
            "timeout_seconds":60,
        });
        if let Some(deliver) = deliver {
            task["deliver"] = deliver;
        }
        let replies = mcp(
            &rig,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"fabric_run","arguments":{"op":"submit","operation_id":"git-writer","tasks":[task]}
                }}),
            ],
        )?;
        let result = &replies
            .iter()
            .find(|reply| reply["id"] == 1)
            .ok_or("submit reply")?["result"];
        assert_eq!(result["isError"], true, "{result}");
        assert_eq!(
            result["structuredContent"]["code"], "worktree_unregistered",
            "{result}"
        );
        assert_eq!(
            result["structuredContent"]["path"],
            worktree.to_str().ok_or("worktree")?
        );
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn submit_writer_io_names_the_failed_operation_and_path() -> Result {
    let rig = TestRig::new()?;
    git(
        &rig,
        &rig.project,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    )?;
    let worktree = rig.project.join(".worktrees/git-writer");
    git(
        &rig,
        &rig.project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "git-writer",
            worktree.to_str().ok_or("worktree")?,
        ],
    )?;
    assert_eq!(code(&rig.command(&["fabric", "identity", "--json"])?), 0);
    let submit = || -> Result<Value> {
        let replies = mcp(
            &rig,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"fabric_run","arguments":{"op":"submit","operation_id":"writer-io","tasks":[{
                        "prompt":"commit the fix", "access":"worktree_write", "worktree":worktree,
                        "route":{"kind":"model","provider":"codex","model":"gpt-6.1-sol"},"timeout_seconds":60
                    }]}
                }}),
            ],
        )?;
        Ok(replies
            .iter()
            .find(|reply| reply["id"] == 1)
            .ok_or("submit reply")?["result"]
            .clone())
    };

    let gitfile = std::fs::read_to_string(worktree.join(".git"))?;
    let private = Path::new(gitfile.trim().strip_prefix("gitdir: ").ok_or("gitdir")?);
    let head = private.join("HEAD");
    let original_head = std::fs::read(&head)?;
    for (contents, operation) in [
        (None, "submit: openat read"),
        (Some(vec![0xff]), "submit: decode writer HEAD"),
        (
            Some(b"ref: refs/heads/../invalid\n".to_vec()),
            "submit: parse writer branch",
        ),
    ] {
        if let Some(contents) = contents {
            std::fs::write(&head, contents)?;
        } else {
            std::fs::remove_file(&head)?;
        }
        let result = submit()?;
        let error = &result["structuredContent"];
        assert_eq!(error["code"], "io", "{result}");
        assert_eq!(error["operation"], operation, "{result}");
        assert_eq!(error["path"], head.to_string_lossy().as_ref(), "{result}");
    }
    std::fs::write(&head, original_head)?;
    let blocked = rig.state.join("worktrees");
    std::fs::write(&blocked, "not a directory")?;
    let result = submit()?;
    let error = &result["structuredContent"];
    assert_eq!(error["code"], "io", "{result}");
    assert_eq!(error["operation"], "submit: openat directory", "{result}");
    assert_eq!(
        error["path"],
        blocked.to_string_lossy().as_ref(),
        "{result}"
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn submit_coordination_io_names_the_failed_operation_and_path() -> Result {
    let rig = TestRig::new()?;
    assert_eq!(code(&rig.command(&["fabric", "identity", "--json"])?), 0);
    let blocked = rig.state.join("fabric");
    std::fs::write(&blocked, "not a directory")?;
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"fabric_run","arguments":{"op":"submit","operation_id":"store-io","tasks":[{
                    "prompt":"read the code", "route":{"kind":"model","provider":"codex","model":"gpt-6.1-sol"},
                    "timeout_seconds":60
                }]}
            }}),
        ],
    )?;
    let result = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("submit reply")?["result"];
    let error = &result["structuredContent"];
    assert_eq!(error["code"], "io", "{result}");
    assert_eq!(
        error["operation"], "open coordination store: openat directory",
        "{result}"
    );
    assert_eq!(
        error["path"],
        blocked.to_string_lossy().as_ref(),
        "{result}"
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_writer_reports_delivery_prepare_failure() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        std::fs::create_dir_all(rig.home.join(path))?;
    }
    std::fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let bin = rig.root.join("provider/bin");
    std::fs::create_dir_all(&bin)?;
    std::fs::write(bin.join("codex"), "#!/bin/sh\necho 'codex-cli 0.159.0'\n")?;
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
    git(
        &rig,
        &rig.project,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    )?;
    let remote = rig.root.join("remote.git");
    git(
        &rig,
        &rig.root,
        &["init", "-q", "--bare", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &rig,
        &rig.project,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &rig,
        &rig.project,
        &["push", "-q", "origin", "HEAD:refs/heads/main"],
    )?;
    let missing = rig
        .root
        .join("long-parent-".repeat(18))
        .join("missing-helper-gh");
    git(
        &rig,
        &rig.project,
        &[
            "config",
            "credential.helper",
            &format!("!{} auth git-credential", missing.display()),
        ],
    )?;
    let prompt = rig.root.join("prompt.txt");
    std::fs::write(&prompt, "commit nothing")?;
    let (_, placed) = json_command(
        &rig,
        &[
            "dispatch",
            "--host",
            "workshop",
            "--mode",
            "worktree-write",
            "--branch",
            "peer/prepare-failure",
            "--base",
            "origin/main",
            "--model",
            "codex/gpt-6-luna",
            "--operation-id",
            "peer-prepare-failure",
            "--deliver-remote",
            "origin",
            "--deliver-branch",
            "published/peer",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--json",
        ],
    )?;
    let lane = &placed["data"]["lanes"][0];
    assert_eq!(lane["host"], peer.to_string(), "{placed}");
    assert_eq!(lane["state"], "rejected", "{placed}");
    let reason = lane["reason"].as_str().ok_or("peer refusal reason")?;
    assert!(reason.contains("Prepare (credential helper)"), "{placed}");
    assert!(reason.contains("missing-helper-gh"), "{placed}");
    assert!(!reason.contains("receipt"), "{placed}");
    assert_eq!(lane["delivery"], Value::Null, "{placed}");
    let run = placed["data"]["run_id"].as_str().ok_or("run")?;
    let (status, lanes) = json_command(&rig, &["lanes", "--id", run, "--json"])?;
    assert_eq!(status, 0, "{lanes}");
    assert_eq!(lanes["data"]["lanes"][0]["reason"], reason, "{lanes}");
    let attempt = lane["attempt_id"].as_str().ok_or("attempt")?;
    let directory = rig
        .root
        .join(format!("peer-state/runs/{run}/attempts/{attempt}"));
    assert!(!directory.join("delivery.json").exists());
    assert!(!directory.join("delivery-started.json").exists());
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn peer_writer_creates_its_worktree_from_a_pushed_base() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
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
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn mcp_submission_places_read_only_work_on_a_named_peer() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let peer = loopback_peer(&mut rig)?;
    let task = json!({"prompt":"fixture","route":{"kind":"model","provider":"codex","model":"gpt-6-luna"},"timeout_seconds":60});
    let mut writer = task.clone();
    writer["access"] = "worktree_write".into();
    writer["worktree"] = rig.project.to_string_lossy().into_owned().into();
    let replies = mcp(
        &rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"submit","operation_id":"mcp-peer","tasks":[task],"host":peer.to_string()}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fabric_run","arguments":{"op":"submit","operation_id":"mcp-peer-writer","tasks":[writer],"host":peer.to_string()}}}),
        ],
    )?;
    let placed = &replies.iter().find(|r| r["id"] == 1).ok_or("submission")?["result"];
    assert_ne!(placed["isError"], true, "{placed}");
    let target = placed["structuredContent"]["receipt"]["targets"][0].clone();
    assert_eq!(target["host"], peer.to_string(), "{placed}");
    // The run lives only in the peer's store.
    assert!(!rig.state.join("runs").exists());
    assert!(rig.root.join("peer-state/runs").exists());
    let writer = &replies.iter().find(|r| r["id"] == 2).ok_or("writer")?["result"];
    assert_eq!(writer["isError"], true, "{writer}");
    assert_eq!(text_error_code(writer), "owner_unavailable");

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
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

/// Seed one queued attempt with `prompt` in the owner store, optionally
/// cancelled so it carries two events. Returns the attempt and its events.
fn seed_attempt(
    rig: &mut TestRig,
    prompt: &str,
    cancel_it: bool,
) -> Result<(
    provenant_core::AttemptId,
    Vec<provenant_records::run::RunEvent>,
)> {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, ProjectId, RootInputs, RunId,
        TaskId, WaitBudget,
    };
    use provenant_os::clock::SystemClock;
    use provenant_records::{
        CanonicalV1,
        run::{CanonicalRequest, RequestedRoute, TaskRequest},
    };

    let identity = federated_identity(rig)?;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let actor = ActorId::parse(identity["data"]["inbox"]["actor"].as_str().ok_or("actor")?)?;
    let mut roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let state = roots.resolve(&provenant_os::fs::SystemPathResolver)?.state;
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
    let task_id = TaskId::from_bytes([92; 16]);
    let attempt_id = AttemptId::from_bytes([91; 16]);
    store.create(
        &provenant_run::PreparedRun {
            run_id: RunId::from_bytes([96; 16]),
            operation_id: OperationId::parse("page-seed")?,
            created_at: clock.wall_now()?,
            request: CanonicalRequest {
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: vec![TaskRequest {
                    title: None,
                    base: None,
                    base_ref: None,
                    writer: None,
                    profile: provenant_records::run::HarnessProfile::default(),
                    task_id,
                    prompt_digest: CanonicalV1::file_digest(prompt.as_bytes()),
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
                }],
                concurrency: 1,
                parent: None,
                lane_depth: Some(1),
                parent_attempt: None,
            },
            attempts: vec![provenant_run::NewAttempt {
                attempt_id,
                task_id,
                prompt: prompt.into(),
            }],
        },
        &call,
    )?;
    if cancel_it {
        store.cancel(attempt_id, "page fixture", &call)?;
    }
    let events = store.events(0, 64, Some(attempt_id), &call)?;
    Ok((attempt_id, events))
}

fn read_output(
    rig: &TestRig,
    attempt: provenant_core::AttemptId,
    part: &str,
    offset: u64,
    max_bytes: u64,
) -> Result<Value> {
    let replies = mcp(
        rig,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_output","arguments":{"op":"read","target":{"attempt":attempt.to_string()},"part":part,"offset":offset,"max_bytes":max_bytes}}}),
        ],
    )?;
    Ok(replies
        .into_iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("output reply")?["result"]
        .clone())
}

#[test]
fn trimmed_event_page_continues_from_the_last_kept_event() -> Result {
    let mut rig = TestRig::in_directory(Path::new(env!("CARGO_MANIFEST_DIR")))?;
    let (attempt, events) = seed_attempt(&mut rig, "prompt", true)?;
    assert_eq!(events.len(), 2);
    let first_only = serde_json::to_vec(&events[..1])?.len() as u64;
    let both = serde_json::to_vec(&events)?.len() as u64;
    // The separator byte means two events need more than twice one.
    assert!(both > first_only + 1);

    // Room for the first event only: the page is trimmed, not exhausted.
    let page = read_output(&rig, attempt, "events", 0, both - 1)?;
    assert_ne!(page["isError"], true, "{page}");
    let page = &page["structuredContent"];
    assert_eq!(page["eof"], false);
    assert_eq!(page["next_offset"], events[0].sequence);
    let kept: Vec<Value> = serde_json::from_str(page["bytes"]["data"].as_str().ok_or("bytes")?)?;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["sequence"], events[0].sequence);

    // The next read starts after the kept event and returns the rest.
    let rest = read_output(&rig, attempt, "events", events[0].sequence, 65536)?;
    let rest = &rest["structuredContent"];
    assert_eq!(rest["eof"], true);
    assert_eq!(rest["next_offset"], events[1].sequence);
    let rest: Vec<Value> = serde_json::from_str(rest["bytes"]["data"].as_str().ok_or("bytes")?)?;
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0]["sequence"], events[1].sequence);

    // A limit below one event refuses rather than returning an empty page.
    let refused = read_output(&rig, attempt, "events", 0, first_only - 1)?;
    assert_eq!(refused["isError"], true);
    assert_eq!(text_error_code(&refused), "output_too_large");
    Ok(())
}

#[test]
fn output_page_ending_inside_a_character_stops_before_it() -> Result {
    let mut rig = TestRig::in_directory(Path::new(env!("CARGO_MANIFEST_DIR")))?;
    // 'a' is one byte and the sun is three, so two bytes split the sun.
    let (attempt, _) = seed_attempt(&mut rig, "a\u{2600}b", false)?;
    let page = read_output(&rig, attempt, "prompt", 0, 2)?;
    assert_ne!(page["isError"], true, "{page}");
    let page = &page["structuredContent"];
    assert_eq!(page["bytes"]["encoding"], "utf8");
    assert_eq!(page["bytes"]["data"], "a");
    assert_eq!(page["next_offset"], 1);
    assert_eq!(page["eof"], false);

    let next = read_output(&rig, attempt, "prompt", 1, 3)?;
    let next = &next["structuredContent"];
    assert_eq!(next["bytes"]["encoding"], "utf8");
    assert_eq!(next["bytes"]["data"], "\u{2600}");
    assert_eq!(next["next_offset"], 4);
    Ok(())
}
