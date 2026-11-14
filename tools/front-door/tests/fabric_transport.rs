//! The real Rust command edge runs without Python, Node or ambient state.
mod support;
use provenant_fabric_runtime::hosts::{
    CONSOLE_DETAIL_PROTOCOL, CONSOLE_INBOX_PROTOCOL, DOCTOR_PROTOCOL, PROTOCOL,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
};
use support::{Result, TestRig, code};

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
fn peer_health_advertises_graph_submission() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let source = next_host(host.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let request = json!({
        "version": 1, "protocol": PROTOCOL,
        "target_host": host, "source_host": source, "agent": "fixture",
        "project": {
            "key": identity["data"]["federation"]["key"],
            "common_dir": "repo/.git"
        },
        "action": {"action": "health"}
    });
    // Read the real peer response directly: dispatch's short health-probe
    // deadline may expire and continue without observing the advertisement.
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["fabric".into(), "peer".into()],
        Some(&serde_json::to_vec(&request)?),
    )?;
    assert_eq!(code(&output), 0, "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(response["result"]["result"], "health", "{response}");
    let health = &response["result"]["data"];
    assert_eq!(health["host"], host, "{health}");
    assert_eq!(health["available"], true, "{health}");
    assert_eq!(health["execution"], true, "{health}");
    assert_eq!(health["run_capabilities"]["submit"], true, "{health}");
    assert!(!rig.state.join("attempts").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_doctor_returns_the_local_report() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let source = next_host(host.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let request = json!({
        "version": 1, "protocol":DOCTOR_PROTOCOL,
        "target_host": host,
        "source_host": source,
        "agent": "fixture",
        "project": {"key": key, "common_dir": "repo/.git"},
        "action": {"action":"doctor"}
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
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(response["result"]["result"], "doctor", "{response}");
    let report = &response["result"]["data"];
    assert!(report["claude"].is_array(), "{report}");
    assert!(report["peer_agent"].is_string(), "{report}");
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_uses_the_configured_peer_transport() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let local = identity["data"]["host"].as_str().ok_or("host")?;
    let host = next_host(local.parse()?)?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let ssh = rig.root.join("ssh");
    let args = rig.root.join("ssh-args");
    let request = rig.root.join("ssh-request");
    let response = rig.root.join("ssh-response");
    std::fs::write(
        &response,
        serde_json::to_vec(&json!({
            "version": 1, "protocol":DOCTOR_PROTOCOL,
            "host": host.to_string(),
            "project": key,
            "result": {"result":"doctor","data":{
                "claude":[{"label":"default (~/.claude)","signin":{
                    "state":"signed_out","token_validity":"unverified"
                }}],
                "peer_agent":"running","login_accounts":[],
                "managed":[],
                "warnings":[]
            }}
        }))?,
    )?;
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\ncat '{}'\n",
            args.display(),
            request.display(),
            response.display()
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n\
             [domains.fabric.peers.workshop]\nhost='{}'\ndestination='workshop-fabric'\n\
             remote_executable='/opt/provenant'\n",
            ssh.to_string_lossy(),
            host
        ),
    )?;
    let (status, output) = json_command(&rig, &["hosts", "doctor", "workshop", "--json"])?;
    assert_eq!(status, 0, "{output}");
    assert_eq!(
        output["data"]["claude"][0]["label"], "default (~/.claude)",
        "{output}"
    );
    let args = std::fs::read_to_string(args)?;
    assert!(args.lines().any(|arg| arg == "workshop-fabric"), "{args}");
    assert!(
        args.lines().any(|arg| arg == "/opt/provenant fabric peer"),
        "{args}"
    );
    let request: Value = serde_json::from_slice(&std::fs::read(request)?)?;
    assert_eq!(request["action"], json!({"action": "doctor"}), "{request}");
    assert_eq!(request["protocol"], DOCTOR_PROTOCOL, "{request}");
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_unknown_peer_refuses_with_unknown_host() -> Result {
    let mut rig = TestRig::new()?;
    federated_identity(&mut rig)?;
    let (status, output) = json_command(&rig, &["hosts", "doctor", "nowhere", "--json"])?;
    assert_ne!(status, 0);
    assert_eq!(output["error"]["code"], "UNKNOWN_HOST", "{output}");
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_rejects_untrusted_peer_reports() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let local = identity["data"]["host"].as_str().ok_or("host")?;
    let host = next_host(local.parse()?)?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let ssh = rig.root.join("ssh");
    let response_path = rig.root.join("ssh-response");
    let good = json!({
        "claude":[{"label":"default","signin":{"state":"signed_out","token_validity":"unverified"}}],
        "peer_agent":"running","login_accounts":[],"managed":[],"warnings":[]
    });
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\ncat >/dev/null\ncat '{}'\n",
            response_path.display()
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n
             [domains.fabric.peers.workshop]\nhost='{}'\ndestination='workshop-fabric'\n",
            ssh.to_string_lossy(),
            host
        ),
    )?;

    let invalid_replies = [
        json!({"result":"doctor","data":{
            "claude":[{"label":"default","signin":{"state":"signed_out","token_validity":"valid"}}],
            "peer_agent":"running","login_accounts":[],"managed":[],"warnings":[]
        }}),
        {
            let mut report = good.clone();
            report["claude"][0]["label"] = json!("bad\u{0007}label");
            json!({"result":"doctor","data":report})
        },
        {
            let mut report = good.clone();
            report["claude"] = json!(vec![report["claude"][0].clone(); 129]);
            json!({"result":"doctor","data":report})
        },
        json!({"result":"health","data":{"host":host,"available":true,"execution":true}}),
    ];
    for result in invalid_replies {
        let envelope = json!({
            "version":1,"protocol":DOCTOR_PROTOCOL,"host":host,"project":key,"result":result
        });
        std::fs::write(&response_path, serde_json::to_vec(&envelope)?)?;
        let (status, output) = json_command(&rig, &["hosts", "doctor", "workshop", "--json"])?;
        assert_ne!(status, 0, "{output}");
        assert_eq!(output["error"]["code"], "PEER", "{output}");
    }
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
        "version": 1, "protocol":PROTOCOL,
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
fn peer_console_inbox_is_read_only_and_refuses_an_oversized_page() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    let host = identity["data"]["host"].as_str().ok_or("host")?;
    let source = next_host(host.parse()?)?;
    configure_fixture_peer(&rig, source)?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    let ask = |protocol: u64, limit: u64| -> Result<Value> {
        let request = json!({
            "version": 1, "protocol":protocol,
            "target_host": host,
            "source_host": source,
            "agent": "fixture",
            "project": {"key": key, "common_dir": "repo/.git"},
            "action": {"action":"console_inbox","state":"open","limit":limit}
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
        Ok(serde_json::from_slice(&output.stdout.bytes)?)
    };
    let oversized = ask(CONSOLE_INBOX_PROTOCOL, 101)?;
    assert_eq!(oversized["result"]["result"], "error");
    assert_eq!(
        oversized["result"]["data"]["error"]["code"],
        "invalid_input"
    );
    // A peer on the previous feature fence is refused before the action decodes.
    let old = ask(CONSOLE_DETAIL_PROTOCOL, 50)?;
    assert_eq!(old["result"]["data"]["error"]["code"], "peer_version");
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
        "version": 1, "protocol":PROTOCOL,
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
fn peer_ingress_refuses_protocol_before_decoding_or_running_mutations() -> Result {
    let mut rig = TestRig::new()?;
    let identity = federated_identity(&mut rig)?;
    for protocol in [1, PROTOCOL - 1, PROTOCOL + 1] {
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
            assert_eq!(reply["protocol"], PROTOCOL);
            assert_eq!(reply["result"]["data"]["error"]["code"], "peer_version");
            assert!(!rig.state.join("fabric/fabric.sqlite3").exists());
        }
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_ingress_rejects_wrong_host_before_coordination_mutation() -> Result {
    let rig = TestRig::new()?;
    let input = json!({"version":1,"protocol":PROTOCOL,"target_host":"11111111111111111111111111111111","source_host":"22222222222222222222222222222222","agent":"fixture","project":{"key":"not-a-project","common_dir":"../escape"},"action":{"action":"health"}});
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
        "version": 1, "protocol":PROTOCOL,
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
        "version": 1, "protocol":PROTOCOL, "target_host": host, "source_host": source, "agent": "worker",
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
    let request = json!({"version":1,"protocol":PROTOCOL,"target_host":host,"source_host":source,"agent":"worker","project":{"key":key,"common_dir":"repo/.git"},"action":{"action":"coordination","input":{"domain":"events","input":{"op":"note","note":"remote note","dedup_key":null}},"operation_id":"note-once"}});
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
        "version": 1, "protocol":PROTOCOL, "target_host": host, "source_host": source, "agent": "worker",
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
            data: provenant_fabric_runtime::hosts::PeerResultError {
                error: provenant_fabric_runtime::coordination::facade::FacadeError::Io { .. },
                ..
            },
            ..
        }
    ));
    rig.assert_legacy_unchanged()
}

#[test]
fn graph_persists_auth_and_usage_refusals_before_fallback() -> Result {
    for scenario in ["authority", "refresh", "quota"] {
        let mut rig = TestRig::new()?;
        let cleanup = rig.process_cleanup();
        let bin = rig.root.join("bin");
        std::fs::create_dir(&bin)?;
        let executable = bin.join("claude");
        std::fs::write(
            &executable,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '2.1.0'; else echo '{\"loggedIn\":true,\"email\":\"fixture@example.test\"}'; fi\n",
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        rig.env.retain(|(key, _)| key != "PATH");
        rig.env.push((
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ));
        let directory = rig.home.join(".claude-pro");
        std::fs::create_dir(&directory)?;
        let credential = directory.join(".credentials.json");
        if scenario == "authority" {
            std::fs::create_dir(&credential)?;
        } else {
            std::fs::write(&credential, b"{}")?;
        }
        let key = provenant_providers::identity::fingerprint("fixture@example.test")
            .ok_or("account key")?;
        support::register_account(&rig, "claude", "pro", &key, &directory)?;
        if scenario == "quota" {
            #[allow(
                clippy::disallowed_methods,
                reason = "the quota record must be fresh against the CLI's live clock"
            )]
            let now = i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
            )?;
            let scope = provenant_quota::AccountScope {
                adapter: "claude".into(),
                account_key: key.clone(),
                label: "pro".into(),
            };
            let mut record = provenant_quota::AccountRecord::empty(scope.clone());
            record.snapshot = Some(provenant_quota::QuotaSnapshot {
                schema_version: 1,
                account: scope,
                revision: 0,
                status: provenant_quota::QuotaStatus::Exhausted,
                observed_unix_seconds: now,
                source_ref: "fixture".into(),
                authority_ref: None,
                windows: vec![provenant_quota::QuotaWindow {
                    pool: "five_hour".into(),
                    window_seconds: Some(18_000),
                    used_basis_points: 10_000,
                    resets_unix_seconds: Some(now + 9000),
                }],
                details: None,
            });
            let quota = rig.state.join("quota").join(&key);
            std::fs::create_dir_all(&quota)?;
            std::fs::set_permissions(
                rig.state.join("quota"),
                std::fs::Permissions::from_mode(0o700),
            )?;
            std::fs::set_permissions(&quota, std::fs::Permissions::from_mode(0o700))?;
            let path = quota.join("claude.json");
            std::fs::write(&path, serde_json::to_vec(&record)?)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::write(
            rig.project.join("graph.json"),
            serde_json::to_vec(&json!({
                "schema_version":1,"graph_id":"typed-refusal","nodes":[{"id":"one","prompt":"work","access":"read_only","routes":[
                    {"adapter":"claude","model":"claude/claude-sonnet-5-5","account":"pro"},
                    {"adapter":"fixture","model":"two","account":"pro"}
                ]}]
            }))?,
        )?;
        let run = rig.command(&["run", "--file", "graph.json", "--json"])?;
        assert_eq!(
            code(&run),
            1,
            "{scenario}: stdout={} stderr={}",
            String::from_utf8_lossy(&run.stdout.bytes),
            String::from_utf8_lossy(&run.stderr.bytes)
        );
        let run: Value = serde_json::from_slice(&run.stdout.bytes)?;
        assert_eq!(run["status"], "failed", "{run}");
        assert_eq!(run["data"]["outcome"], "failed", "{run}");
        let node = &run["data"]["checkpoint"]["nodes"][0];
        let attempts = node["attempts"].as_array().ok_or("attempts")?;
        assert_eq!(attempts.len(), 2, "{scenario}: {run}");
        let expected = if scenario == "quota" {
            "usage_limited"
        } else {
            "auth_required"
        };
        assert_eq!(attempts[0]["outcome"], expected, "{scenario}: {run}");
        assert_eq!(attempts[0]["launched"], false);
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
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = provenant_run::RunStore::open(&context, &call)?;
        let (_, id) = attempts[0]["attempt_ref"]
            .as_str()
            .ok_or("reference")?
            .split_once('/')
            .ok_or("reference")?;
        let record = store.record(id.parse()?, &call)?;
        let failure = if scenario == "quota" {
            provenant_records::run::PreparationFailure::UsageLimited
        } else {
            provenant_records::run::PreparationFailure::AuthRequired
        };
        assert_eq!(
            record.preparation_failure,
            Some(failure),
            "{scenario}: {record:?}"
        );
        let reason = match &record.state {
            provenant_run::AttemptState::Terminal {
                reason: Some(reason),
                ..
            } => reason,
            _ => return Err("missing refusal reason".into()),
        };
        let skipped = record
            .skipped_routes
            .iter()
            .find(|route| route.route.starts_with("claude/"))
            .ok_or("missing Claude preparation refusal")?;
        if scenario == "quota" {
            assert_eq!(skipped.kind, provenant_records::run::SkipKind::Unavailable);
            assert_eq!(
                skipped.cause,
                Some(provenant_records::RouteExclusionReason::Exhausted)
            );
        } else {
            assert_eq!(skipped.kind, provenant_records::run::SkipKind::AuthRequired);
            assert_eq!(
                skipped.cause, None,
                "unclassified source errors add no invented account cause"
            );
        }
        let diagnostic = skipped.diagnostic();
        assert_eq!(diagnostic.code, "REFUSED");
        assert!(
            reason.contains(diagnostic.next.as_deref().ok_or("preparation action")?),
            "{reason}"
        );
        assert!(!reason.contains("single regular file"), "{reason}");
        assert!(!reason.contains("stored credential JSON"), "{reason}");
        assert!(
            !reason.contains(directory.to_str().ok_or("directory")?),
            "{reason}"
        );
        assert_eq!(record.provider_launched, Some(false));
        assert_eq!(
            record.terminal_evidence,
            Some(provenant_records::run::TerminalEvidence::PreLaunchRefusal)
        );
        cleanup.disarm();
    }
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
    let run = rig.command(&["run", "--file", "graph.json", "--json"])?;
    assert_eq!(code(&run), 1);
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
        checkpoint.nodes[0].state = provenant_fabric_runtime::graph::NodeState::Running;
        checkpoint.nodes[0].outputs = None;
        checkpoint.settlements = 0;
        let attempt = checkpoint.nodes[0].attempts.last_mut().ok_or("attempt")?;
        attempt.state = AttemptState::Running;
        attempt.attempt_ref = None;
        attempt.outcome = None;
        attempt.settled = 0;
        journal.save(&checkpoint)?;
    }
    let cancelled = rig.command(&["stop", "lost", "--json"])?;
    assert_eq!(
        code(&cancelled),
        0,
        "{}",
        String::from_utf8_lossy(&cancelled.stdout.bytes)
    );
    let cancelled: Value = serde_json::from_slice(&cancelled.stdout.bytes)?;
    assert!(
        cancelled["data"]["rows"]
            .as_array()
            .ok_or("status rows")?
            .iter()
            .any(|row| row["kind"] == "graph" && row["state"] == "cancelled"),
        "{cancelled}"
    );
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

// Providers remain disabled. The graph still retains the sealed attempt request.
fn submitted_policy(
    rig: &TestRig,
    operation: &str,
    route: Value,
) -> Result<(provenant_records::run::CanonicalRequest, Value)> {
    let route = if route["kind"] == "named" {
        json!({"alias":route["name"]})
    } else {
        json!({"adapter":route["provider"],"model":route["model"]})
    };
    std::fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({
            "schema_version":1,"graph_id":operation,"nodes":[{
                "id":"task","title":"Review fixture","prompt":"fixture","access":"read_only",
                "route":route,"deadline_seconds":60
            }]
        }))?,
    )?;
    let run = rig.command(&["run", "--file", "graph.json", "--json"])?;
    let run: Value = serde_json::from_slice(&run.stdout.bytes)?;
    let reference = run["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
        .as_str()
        .ok_or_else(|| format!("graph attempt reference: {run}"))?;
    let attempt_id = reference.rsplit_once('/').ok_or("qualified attempt")?.1;
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let mut roots = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let context = provenant_run::ExecutionContext {
        state_root: roots.resolve(&provenant_os::fs::SystemPathResolver)?.state,
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
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    let attempt = serde_json::to_value(store.record(attempt_id.parse()?, &call)?)?;
    let request = serde_json::from_slice(&std::fs::read(
        rig.state
            .join("attempts")
            .join(attempt_id)
            .join("request.json"),
    )?)?;
    Ok((request, attempt))
}

#[test]
fn submission_preserves_explicit_named_route_tier() -> Result {
    let rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let (request, attempt) =
        submitted_policy(&rig, "named-tier", json!({"kind":"named","name":"senior"}))?;
    assert_eq!(request.task.title.as_deref(), Some("Review fixture"));
    assert_eq!(attempt["title"], "Review fixture");
    assert_eq!(
        request.task.route,
        provenant_records::run::RequestedRoute::Named {
            extra: Default::default(),
            name: "senior".into(),
            effort: None,
        }
    );
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn submission_stamps_the_local_host_on_its_trace() -> Result {
    let rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let (_, attempt) =
        submitted_policy(&rig, "trace-host", json!({"kind":"named","name":"senior"}))?;
    assert_eq!(attempt["trace"]["host"], identity["data"]["host"]);
    assert_eq!(attempt["trace"]["attempt"], attempt["attempt_id"]);
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
    let (request, attempt) = submitted_policy(
        &rig,
        "config-silence",
        json!({"kind":"model","provider":"codex","model":"fixture"}),
    )?;
    let task = &request.task;
    assert_eq!(serde_json::to_value(&task.profile)?, attempt["profile"]);
    assert_eq!(task.profile.knobs["lanes.silence_seconds"], 2);
    let plan = provenant_run::capture_plan(&request.task, &Default::default());
    assert_eq!(plan.silence_ms, 2000);
    assert!(!plan.silent(1999, Some(0)));
    assert!(plan.silent(2000, Some(0)));
    assert!(!plan.silent(2000, Some(2000)));
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

/// `run ARGS --name NAME`, then `wait NAME`, answering the launch's own exit
/// and a `data.checkpoint` read after the wait. A replayed name reads the same
/// graph again, so a replay and its first launch compare equal.
fn launch_named_and_wait(
    rig: &TestRig,
    args: &[&str],
    name: &str,
    timeout: &str,
) -> Result<(i64, Value)> {
    let mut argv = vec!["run"];
    argv.extend_from_slice(args);
    argv.extend(["--name", name, "--json"]);
    let (status, launched) = json_command(rig, &argv)?;
    if launched["data"]["checkpoint"].is_null() {
        return Ok((status, launched));
    }
    let waited = rig.command(&["wait", name, "--timeout", timeout, "--json"])?;
    let _ = code(&waited);
    let checkpoint = rig.checkpoint(name)?;
    Ok((
        status,
        json!({"data": {"checkpoint": checkpoint}, "launch": launched}),
    ))
}

/// Run the host tick until `settled` holds for NAME's checkpoint, failing
/// with the last observation once `limit` passes. A wait only observes
/// (retry-owner.md); the tick advances due launch retries and observes peer
/// settlement for the origin graph.
fn tick_until(
    rig: &TestRig,
    name: &str,
    limit: std::time::Duration,
    settled: impl Fn(&Value) -> bool,
) -> Result<Value> {
    let observe = || -> std::result::Result<(Value, String), String> {
        let ticked = rig
            .command(&["tick", "--json"])
            .map_err(|error| error.to_string())?;
        let checkpoint = rig.checkpoint(name).map_err(|error| error.to_string())?;
        Ok((
            checkpoint,
            String::from_utf8_lossy(&ticked.stdout.bytes).into_owned(),
        ))
    };
    let (checkpoint, tick) = provenant_test_support::wait_for(name, limit, observe, |observed| {
        observed
            .as_ref()
            .map_or(true, |(checkpoint, _)| settled(checkpoint))
    })??;
    Ok(json!({"data": {"checkpoint": checkpoint}, "tick": tick}))
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

// Adapted from the pre-graph peer dispatch regressions. Placement and replay
// now travel through the graph attempt owner, then the existing peer protocol.
#[test]
fn peer_dispatch_recovers_receipt_when_probes_fail() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let peer = loopback_peer(&mut rig)?;
    let ssh = rig.root.join("ssh");
    let real = rig.root.join("ssh-real");
    std::fs::rename(&ssh, &real)?;
    let lost = rig.root.join("lost-submit");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nrequest=$(cat)\ncase \"$request\" in\n  *'\"action\":\"health\"'*) exit 1;;\n  *'\"action\":\"submit\"'*) if [ -f '{}' ]; then printf '%s\\n' \"$request\" | '{}' >/dev/null; rm '{}'; exit 1; fi;;\nesac\nprintf '%s\\n' \"$request\" | '{}'\n",
            lost.display(),
            real.display(),
            lost.display(),
            real.display()
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    let launch = |rig: &TestRig, name, wait| {
        let args = [
            "task",
            "fixture",
            "--host",
            "workshop",
            "--model",
            "codex/gpt-6-luna",
        ];
        if wait {
            launch_named_and_wait(rig, &args, name, "15s")
        } else {
            let mut argv = vec!["run"];
            argv.extend_from_slice(&args);
            argv.extend(["--name", name, "--json"]);
            json_command(rig, &argv)
        }
    };
    let (_, success) = launch(&rig, "probe-failure-success", true)?;
    let reference = success["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
        .as_str()
        .ok_or_else(|| format!("missing peer attempt: {success}"))?;
    assert!(reference.starts_with(&format!("{peer}/")), "{success}");
    let peer_attempts = rig.root.join("peer-state/attempts");
    let existing: std::collections::HashSet<_> = std::fs::read_dir(&peer_attempts)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    std::fs::write(&lost, "")?;
    // Observe the lost receipt before a waited submission advances its due retry.
    let (_, unknown) = launch(&rig, "probe-failure-retry", false)?;
    assert!(unknown.to_string().contains("LAUNCH_UNKNOWN"), "{unknown}");
    let after_loss: std::collections::HashSet<_> = std::fs::read_dir(&peer_attempts)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(after_loss.difference(&existing).count(), 1, "{unknown}");
    // A lost receipt must remain recoverable after unrelated legacy work appears
    // at the receiver. Replay observes its sealed operation rather than admitting
    // a new launch, and must leave that legacy work untouched.
    let legacy_directory = rig
        .root
        .join("peer-state/fabric/graphs/11111111111111111111111111111111");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&legacy_directory)?;
    let legacy_path = legacy_directory.join("graph-old-pending-fixture.json");
    let legacy = include_bytes!("fixtures/install/old-pending-checkpoint.json");
    std::fs::write(&legacy_path, legacy)?;
    std::fs::set_permissions(&legacy_path, std::fs::Permissions::from_mode(0o600))?;
    let origin_legacy_directory = rig
        .state
        .join("fabric/graphs/11111111111111111111111111111111");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&origin_legacy_directory)?;
    let origin_legacy_path = origin_legacy_directory.join("graph-old-pending-fixture.json");
    std::fs::write(&origin_legacy_path, legacy)?;
    std::fs::set_permissions(&origin_legacy_path, std::fs::Permissions::from_mode(0o600))?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    rig.env
        .push(("PROVENANT_AGENT_LABEL".into(), "replacement-runner".into()));
    // A replayed name starts nothing, so the host tick advances the due retry.
    let recovered = tick_until(
        &rig,
        "probe-failure-retry",
        std::time::Duration::from_secs(20),
        |checkpoint| checkpoint["nodes"][0]["attempts"][0]["attempt_ref"].is_string(),
    )?;
    let reference = recovered["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
        .as_str()
        .ok_or_else(|| format!("missing recovered peer attempt: {recovered}"))?;
    assert!(reference.starts_with(&format!("{peer}/")), "{recovered}");
    let after: std::collections::HashSet<_> = std::fs::read_dir(&peer_attempts)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(after.difference(&existing).count(), 1, "{recovered}");
    let (_, replay) = launch(&rig, "probe-failure-retry", true)?;
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"],
        reference
    );
    let replayed: std::collections::HashSet<_> = std::fs::read_dir(&peer_attempts)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(replayed, after);
    assert_eq!(std::fs::read(&legacy_path)?, legacy);
    assert_eq!(std::fs::read(&origin_legacy_path)?, legacy);
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn peer_dispatch_set_host_reaches_the_peer_attempt() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let peer = loopback_peer(&mut rig)?;
    let (_, placed) = launch_named_and_wait(
        &rig,
        &[
            "task",
            "fixture",
            "--set",
            "attempt.host=\"workshop\"",
            "--model",
            "codex/gpt-6-luna",
        ],
        "placed-peer",
        "15s",
    )?;
    let reference = placed["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
        .as_str()
        .ok_or_else(|| format!("missing peer attempt: {placed}"))?;
    assert!(reference.starts_with(&format!("{peer}/")), "{placed}");
    assert_eq!(
        std::fs::read_dir(rig.root.join("peer-state/attempts"))?.count(),
        1
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
    // Raise the node limit so this regression reaches the peer frame check.
    let config = rig.state.join("config.toml");
    let mut settings: toml_edit::DocumentMut = std::fs::read_to_string(&config)?.parse()?;
    settings["domains"]["fabric"]["graph"]["limits"]["node_prompt_bytes"] =
        toml_edit::value(1024 * 1024);
    std::fs::write(config, settings.to_string())?;
    // JSON escaping exceeds the peer frame cap although the brief itself fits.
    std::fs::write(&prompt, "\"".repeat(600 * 1024))?;
    // The second identical launch replays the refused graph (cli.md "Names and
    // replay": exit 0 with its recorded state); neither contacts the peer.
    for replay in [false, true] {
        let (status, refused) = json_command(
            &rig,
            &[
                "run",
                "task",
                "--host",
                "workshop",
                "--model",
                "agy/gemini-3.8-flash",
                "--name",
                "peer-frame-limit",
                "--brief-file",
                prompt.to_str().ok_or("prompt")?,
                "--json",
            ],
        )?;
        assert_eq!(status == 0, replay, "{refused}");
        assert_eq!(
            refused["data"]["checkpoint"]["graph_id"],
            "peer-frame-limit"
        );
        assert_eq!(
            refused["data"]["checkpoint"]["nodes"][0]["advance_error_code"], "launch_refused",
            "{refused}"
        );
        assert!(
            refused["data"]["advance_error"]
                .as_str()
                .is_some_and(|message| message.contains("[invalid_input]")),
            "{refused}"
        );
    }
    assert!(!rig.root.join("peer-stderr").exists());
    assert!(!rig.root.join("peer-state/attempts").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn peer_dispatch_refuses_misplaced_writers_and_unknown_hosts_before_contact() -> Result {
    let mut rig = TestRig::new()?;
    loopback_peer(&mut rig)?;
    let contacted = rig.root.join("contacted");
    std::fs::write(
        rig.root.join("ssh"),
        format!("#!/bin/sh\ntouch '{}'\nexit 97\n", contacted.display()),
    )?;
    let worktree = rig.project.to_str().ok_or("worktree")?;
    let (_, refused) = json_command(
        &rig,
        &[
            "run",
            "task",
            "fixture",
            "--host",
            "workshop",
            "--mode",
            "worktree-write",
            "--worktree",
            worktree,
            "--model",
            "codex/gpt-6-luna",
            "--json",
        ],
    )?;
    assert!(
        refused.to_string().contains("WORKTREE_NOT_APPLICABLE"),
        "{refused}"
    );
    for host in ["nowhere", "laptop"] {
        let (_, unknown) = json_command(
            &rig,
            &[
                "run",
                "task",
                "fixture",
                "--host",
                host,
                "--model",
                "codex/gpt-6-luna",
                "--json",
            ],
        )?;
        assert!(unknown.to_string().contains("UNKNOWN_HOST"), "{unknown}");
    }
    assert!(!contacted.exists());
    assert!(!rig.root.join("peer-state/attempts").exists());
    rig.assert_legacy_unchanged()
}

#[test]
#[allow(clippy::disallowed_methods)] // Exercise the real wait deadline.
fn peer_dispatch_wait_bounds_unanswered_submission() -> Result {
    let mut rig = TestRig::new()?;
    let host = loopback_peer(&mut rig)?;
    // Answer the read-only health preflight, then lose the mutating
    // submission's reply so the deadline exercises actual launch uncertainty.
    let identity = federated_identity(&mut rig)?;
    let health = rig.root.join("peer-health.json");
    std::fs::write(
        &health,
        serde_json::to_vec(&json!({
            "version": 1, "protocol": PROTOCOL, "host": host,
            "project": identity["data"]["federation"]["key"],
            "result": {"result":"health", "data":{
                "host":host, "available":true, "execution":true
            }}
        }))?,
    )?;
    let submitted = rig.root.join("submitted");
    std::fs::write(
        rig.root.join("ssh"),
        format!(
            "#!/bin/sh\nrequest=$(cat)\ncase \"$request\" in *'\"action\":\"submit\"'*) : > '{}'; exec /usr/bin/tail -f /dev/null ;; esac\ncat '{}'\n",
            submitted.display(),
            health.display()
        ),
    )?;
    std::fs::set_permissions(rig.root.join("ssh"), std::fs::Permissions::from_mode(0o700))?;
    // `run` has no wait flag, so the peer exchange bound is what ends an
    // unanswered submission.
    let config = rig.state.join("config.toml");
    let text = std::fs::read_to_string(&config)?;
    std::fs::write(
        &config,
        format!("{text}[domains.fabric.limits]\npeer_seconds=5\n"),
    )?;
    let start = std::time::Instant::now();
    let (_, launched) = json_command(
        &rig,
        &[
            "run",
            "task",
            "fixture",
            "--host",
            "workshop",
            "--model",
            "codex/gpt-6-luna",
            "--name",
            "held-submit",
            "--json",
        ],
    )?;
    let result = json!({"launch": launched});
    // Health and submission share the five-second peer bound; the rest is
    // process start on a loaded host.
    assert!(
        start.elapsed() < std::time::Duration::from_secs(20),
        "{result}"
    );
    assert!(
        submitted.exists(),
        "the mutating submission must reach the peer: {result}"
    );
    assert!(result.to_string().contains("LAUNCH_UNKNOWN"), "{result}");
    assert!(
        !result.to_string().contains("home_unavailable"),
        "a deadline is not an unavailable home: {result}"
    );
    assert!(!rig.root.join("peer-state/attempts").exists());
    Ok(())
}

#[test]
fn peer_dispatch_launches_a_large_argv_prompt() -> Result {
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    if let Err(error) = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    ) {
        provenant_test_support::skip_sandbox_dependent_test(error.to_string())?;
        return Ok(());
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
         echo 'peer large prompt fixture' >&2\n\
         printf '%s\\n' '{\"event\":\"init\",\"conversation_id\":\"fixture\",\"init\":{\"model\":\"gemini-3.8-flash\"}}'\n\
         printf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"fixture\",\"status\":\"SUCCESS\",\"response\":\"## Findings\\nlaunched\\n## Evidence\\nread the prompt file\",\"error\":null,\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}'\n",
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
    let args = [
        "task",
        "--host",
        "workshop",
        "--model",
        "agy/gemini-3.8-flash",
        "--effort",
        "high",
        "--brief-file",
        prompt.to_str().ok_or("prompt")?,
    ];
    let mut argv = vec!["run"];
    argv.extend_from_slice(&args);
    argv.extend(["--name", "peer-large-prompt", "--json"]);
    let (status, launched) = json_command(&rig, &argv)?;
    assert_eq!(status, 0, "{launched}");
    let placed = tick_until(
        &rig,
        "peer-large-prompt",
        std::time::Duration::from_secs(20),
        |checkpoint| checkpoint["nodes"][0]["state"] != "running",
    )?;
    let node = &placed["data"]["checkpoint"]["nodes"][0];
    let reference = node["attempts"][0]["attempt_ref"]
        .as_str()
        .ok_or_else(|| format!("missing peer attempt: {placed}"))?;
    let (host, attempt) = reference.split_once('/').ok_or("attempt reference")?;
    assert_eq!(host, peer.to_string());
    // Success needs the fixture's own check that the prompt file it was
    // pointed at holds the whole brief, so the brief did not travel in argv.
    assert_eq!(node["state"], "succeeded", "{placed}");
    // Format 2 keeps one directory per attempt (docs/specs/state.md).
    let attempts = rig.root.join("peer-state/attempts");
    let directory = attempts.join(attempt);
    assert!(std::fs::read_to_string(directory.join("prompt.md"))?.ends_with(&text));
    let launched = std::fs::read_dir(&attempts)?.count();
    // An identical named launch replays the settled graph and starts nothing.
    let (status, replay) = json_command(&rig, &argv)?;
    assert_eq!(status, 0, "{replay}");
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"],
        reference
    );
    assert_eq!(std::fs::read_dir(&attempts)?.count(), launched, "{replay}");
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

#[test]
fn inbox_all_projects_reads_and_answers_through_peers() -> Result {
    let mut rig = TestRig::new()?;
    let peer = loopback_peer(&mut rig)?;
    let (_, local_identity) = json_command(&rig, &["fabric", "identity", "--json"])?;
    let key = local_identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?
        .to_owned();
    let draft = rig.root.join("inbox-draft.json");
    std::fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "kind":"question", "title":"Question", "body":"Please reply", "blocking":false,
            "options":[], "recommended_option_id":null, "proceeding_with":null
        }))?,
    )?;
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir".into(),
            rig.root.join("peer-state").into(),
            "--json".into(),
            "ask".into(),
            "--file".into(),
            draft.into(),
            "--session".into(),
            "chair-a".into(),
            "--operation-id".into(),
            "peer-ask".into(),
        ],
        None,
    )?;
    assert_eq!(code(&output), 0);
    let asked: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let id = asked["data"]["request_id"].as_str().ok_or("request id")?;
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--json".into(),
            "ask".into(),
            "--file".into(),
            rig.root.join("inbox-draft.json").into(),
            "--session".into(),
            "chair-local".into(),
            "--operation-id".into(),
            "local-ask".into(),
        ],
        None,
    )?;
    assert_eq!(code(&output), 0);
    // Without --hosts the listing is exactly the local one.
    let (status, plain) = json_command(&rig, &["inbox", "--all-projects", "--json"])?;
    assert_eq!(status, 0, "{plain}");
    assert!(plain["data"].get("peers").is_none(), "{plain}");
    assert_eq!(plain["data"]["items"].as_array().ok_or("items")?.len(), 1);
    let (status, listed) = json_command(
        &rig,
        &["inbox", "--all-projects", "--hosts", "all", "--json"],
    )?;
    assert_eq!(status, 0, "{listed}");
    let data = &listed["data"];
    assert!(data["coverage"].is_string());
    assert!(
        data["items"]
            .as_array()
            .ok_or("items")?
            .iter()
            .any(|item| item["id"] == id && item["owner_host"] == peer.to_string())
    );
    assert_eq!(data["peers"][0]["state"], "complete", "{listed}");
    assert_eq!(data["peers"][0]["label"], "workshop");
    assert_eq!(data["peers"][0]["included"], 1);
    assert_eq!(data["items"].as_array().ok_or("items")?.len(), 2);
    let (status, unknown) = json_command(
        &rig,
        &["inbox", "--all-projects", "--hosts", "nowhere", "--json"],
    )?;
    assert_ne!(status, 0);
    assert_eq!(unknown["error"]["code"], "invalid_input");
    let reply_file = rig.root.join("reply.txt");
    std::fs::write(&reply_file, "Yes")?;
    let reply_path = reply_file.to_str().ok_or("reply path")?;
    let answer = |key: &str| {
        json_command(
            &rig,
            &[
                "answer",
                "--all-projects",
                "--host",
                "workshop",
                "--project-key",
                key,
                id,
                "--action",
                "reply",
                "--text-file",
                reply_path,
                "--expect-revision",
                "1",
                "--channel",
                "console",
                "--operation-id",
                "routed-answer",
                "--json",
            ],
        )
    };
    let (status, missing) = answer("no/such/key")?;
    assert_ne!(status, 0);
    assert_eq!(missing["error"]["code"], "no_project", "{missing}");
    let (status, answered) = answer(&key)?;
    assert_eq!(status, 0, "{answered}");
    assert_eq!(answered["data"]["operation_id"], "routed-answer");
    let (status, close) = json_command(
        &rig,
        &[
            "inbox",
            "close",
            "--all-projects",
            "--host",
            "workshop",
            id,
            "--state",
            "withdrawn",
            "--reason",
            "obsolete",
            "--expect-revision",
            "1",
            "--json",
        ],
    )?;
    assert_ne!(status, 0);
    assert_eq!(close["error"]["code"], "OWNER_LOCAL_ONLY");
    Ok(())
}

/// Ask once on this host, so the loopback peer or the local store has a row.
fn ask_inbox(rig: &TestRig, state: Option<&Path>, session: &str, operation: &str) -> Result {
    let draft = rig.root.join(format!("draft-{operation}.json"));
    std::fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "kind":"question", "title":"Question", "body":"Please reply", "blocking":false,
            "options":[], "recommended_option_id":null, "proceeding_with":null
        }))?,
    )?;
    let mut args: Vec<OsString> = Vec::new();
    if let Some(state) = state {
        args.extend(["--state-dir".into(), state.into()]);
    }
    args.extend([
        "--json".into(),
        "ask".into(),
        "--file".into(),
        draft.into(),
        "--session".into(),
        session.into(),
        "--operation-id".into(),
        operation.into(),
    ]);
    let output = rig.execute_os(Path::new(env!("CARGO_BIN_EXE_provenant")), &args, None)?;
    assert_eq!(code(&output), 0);
    Ok(())
}

#[test]
fn inbox_hosts_flags_are_validated_and_cursor_pages_skip_peers() -> Result {
    let mut rig = TestRig::new()?;
    let peer = loopback_peer(&mut rig)?;
    let (_, identity) = json_command(&rig, &["fabric", "identity", "--json"])?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?
        .to_owned();
    let local = identity["data"]["host"].as_str().ok_or("host")?.to_owned();
    ask_inbox(
        &rig,
        Some(&rig.root.join("peer-state")),
        "chair-a",
        "peer-ask",
    )?;
    ask_inbox(&rig, None, "chair-local", "local-ask-1")?;
    ask_inbox(&rig, None, "chair-local", "local-ask-2")?;
    let refused = |args: &[&str], needle: &str| -> Result {
        let (status, output) = json_command(&rig, args)?;
        assert_ne!(status, 0, "{output}");
        assert_eq!(output["error"]["code"], "invalid_input", "{output}");
        let message = output["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains(needle), "{output}");
        Ok(())
    };
    refused(
        &["inbox", "--hosts", "all", "--json"],
        "--hosts needs --all-projects",
    )?;
    refused(
        &["inbox", "--all-projects", "--project-key", &key, "--json"],
        "--project-key needs --all-projects and a remote --host",
    )?;
    refused(
        &[
            "inbox",
            "--all-projects",
            "--hosts",
            "all",
            "--session",
            "chair-local",
            "--json",
        ],
        "--session",
    )?;
    refused(
        &[
            "inbox",
            "--all-projects",
            "--host",
            "workshop",
            "--hosts",
            "all",
            "--json",
        ],
        "remote --host",
    )?;
    // A qualified ID naming another configured peer than --host is refused.
    let bench = next_host(peer)?;
    assert_ne!(bench.to_string(), local);
    let config = rig.state.join("config.toml");
    let original = std::fs::read_to_string(&config)?;
    let text = original.clone();
    std::fs::write(
        &config,
        format!("{text}[domains.fabric.peers.bench]\nhost='{bench}'\ndestination='bench'\n"),
    )?;
    let id = "1".repeat(32);
    refused(
        &[
            "inbox",
            "--all-projects",
            "--host",
            "workshop",
            "--project-key",
            &key,
            &format!("{bench}:{id}"),
            "--json",
        ],
        "request owner does not match --host",
    )?;
    // The same ID qualified by --host's own host passes the owner check; the
    // call then fails on the missing request, not on the mismatch.
    let (status, owned) = json_command(
        &rig,
        &[
            "inbox",
            "--all-projects",
            "--host",
            "workshop",
            "--project-key",
            &key,
            &format!("{peer}:{id}"),
            "--json",
        ],
    )?;
    assert_ne!(status, 0, "{owned}");
    assert!(
        !owned["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not match --host"),
        "{owned}"
    );
    std::fs::write(&config, original)?;
    let (status, first) = json_command(
        &rig,
        &[
            "inbox",
            "--all-projects",
            "--limit",
            "1",
            "--hosts",
            "all",
            "--json",
        ],
    )?;
    assert_eq!(status, 0, "{first}");
    assert_eq!(first["data"]["peers"][0]["state"], "complete", "{first}");
    let cursor = first["data"]["next_cursor"].as_str().ok_or("next cursor")?;
    // A cursor page validates the value but never reads the peer.
    refused(
        &[
            "inbox",
            "--all-projects",
            "--cursor",
            cursor,
            "--hosts",
            "nowhere",
            "--json",
        ],
        "unknown peer host",
    )?;
    let (status, next) = json_command(
        &rig,
        &[
            "inbox",
            "--all-projects",
            "--limit",
            "1",
            "--cursor",
            cursor,
            "--hosts",
            "all",
            "--json",
        ],
    )?;
    assert_eq!(status, 0, "{next}");
    assert!(next["data"].get("peers").is_none(), "{next}");
    assert!(
        next["data"]["items"]
            .as_array()
            .ok_or("items")?
            .iter()
            .all(|item| item["owner_host"] != peer.to_string()),
        "{next}"
    );
    Ok(())
}

#[test]
fn inbox_hosts_names_why_a_peer_gave_no_page() -> Result {
    let mut rig = TestRig::new()?;
    let peer = loopback_peer(&mut rig)?;
    let (_, identity) = json_command(&rig, &["fabric", "identity", "--json"])?;
    let key = identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?
        .to_owned();
    ask_inbox(&rig, None, "chair-local", "local-ask")?;
    let peer_state = rig.root.join("peer-state");
    ask_inbox(&rig, Some(&peer_state), "chair-a", "peer-ask")?;
    let peers = |rig: &TestRig| -> Result<Value> {
        let (status, listed) = json_command(
            rig,
            &["inbox", "--all-projects", "--hosts", "all", "--json"],
        )?;
        assert_eq!(status, 0, "{listed}");
        // Local rows always show, whatever the peer did.
        assert_eq!(listed["data"]["items"].as_array().ok_or("items")?.len(), 1);
        Ok(listed["data"]["peers"][0].clone())
    };
    // The peer refuses a page larger than its own page_rows.
    let config = peer_state.join("config.toml");
    let text = std::fs::read_to_string(&config)?;
    std::fs::write(
        &config,
        format!("{text}[domains.fabric.limits]\npage_rows=5\n"),
    )?;
    let refused = peers(&rig)?;
    assert_eq!(refused["state"], "partial", "{refused}");
    assert_eq!(refused["reason"], "limit_refused", "{refused}");
    std::fs::write(&config, text)?;
    // Replace the peer with one that does not know this project.
    let ssh = rig.root.join("ssh");
    let real = std::fs::read_to_string(&ssh)?;
    std::fs::write(
        &ssh,
        "#!/bin/sh\ncat >/dev/null\necho 'provenant: fabric peer: peer project is outside the selected home' >&2\nexit 3\n",
    )?;
    let unknown = peers(&rig)?;
    assert_eq!(unknown["state"], "partial", "{unknown}");
    assert_eq!(unknown["reason"], "project_unknown", "{unknown}");
    // An invalid-input refusal that a one-row page does not cure is not about
    // --limit, so it must not say so.
    let frame = rig.root.join("invalid.json");
    std::fs::write(
        &frame,
        serde_json::to_vec(&json!({
            "version":1,"protocol":CONSOLE_INBOX_PROTOCOL,"host":peer.to_string(),"project":key,
            "result":{"result":"error","data":{"error":{"code":"invalid_input"}}},
        }))?,
    )?;
    std::fs::write(
        &ssh,
        format!("#!/bin/sh\ncat >/dev/null\ncat '{}'\n", frame.display()),
    )?;
    let invalid = peers(&rig)?;
    assert_eq!(invalid["state"], "partial", "{invalid}");
    assert_eq!(invalid["reason"], "invalid_request", "{invalid}");
    // A peer that never answers inside peer_seconds stays unreachable.
    std::fs::write(&ssh, "#!/bin/sh\nsleep 20\n")?;
    let main = rig.state.join("config.toml");
    let text = std::fs::read_to_string(&main)?;
    std::fs::write(
        &main,
        format!("{text}[domains.fabric.limits]\npeer_seconds=1\n"),
    )?;
    let slow = peers(&rig)?;
    assert_eq!(slow["state"], "unreachable", "{slow}");
    assert_eq!(slow["reason"], "timeout", "{slow}");
    std::fs::write(&ssh, real)?;
    Ok(())
}

#[test]
fn remote_inbox_owner() -> Result {
    let mut rig = TestRig::new()?;
    let peer = loopback_peer(&mut rig)?;
    let (_, local_identity) = json_command(&rig, &["fabric", "identity", "--json"])?;
    let draft = rig.root.join("inbox-draft.json");
    std::fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "kind":"question", "title":"Question", "body":"Please reply", "blocking":false,
            "options":[], "recommended_option_id":null, "proceeding_with":null
        }))?,
    )?;
    let peer_state = rig.root.join("peer-state");
    let create = |operation: &str| -> Result<String> {
        let output = rig.execute_os(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "--state-dir".into(),
                peer_state.clone().into(),
                "--json".into(),
                "ask".into(),
                "--file".into(),
                draft.clone().into(),
                "--session".into(),
                "chair-a".into(),
                "--operation-id".into(),
                operation.into(),
            ],
            None,
        )?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
        Ok(value["data"]["request_id"]
            .as_str()
            .ok_or("request id")?
            .into())
    };
    let id = create("remote-ask")?;
    let other = create("remote-close-ask")?;
    let qualified = format!("{peer}:{id}");
    let (status, listed) = json_command(
        &rig,
        &["inbox", "--host", "workshop", "--state", "all", "--json"],
    )?;
    assert_eq!(status, 0, "{listed}");
    assert!(
        listed["data"]["items"]
            .as_array()
            .ok_or("items")?
            .iter()
            .any(|item| item["id"] == id && item["owner_host"] == peer.to_string())
    );
    let reply_file = rig.root.join("reply.txt");
    std::fs::write(&reply_file, "Yes")?;
    let reply_path = reply_file.to_str().ok_or("reply path")?;
    let args = [
        "answer",
        &qualified,
        "--action",
        "reply",
        "--text-file",
        reply_path,
        "--expect-revision",
        "1",
        "--channel",
        "console",
        "--operation-id",
        "remote-answer",
        "--json",
    ];
    let (status, answer) = json_command(&rig, &args)?;
    assert_eq!(status, 0, "{answer}");
    assert_eq!(answer["data"]["operation_id"], "remote-answer");
    let (status, replay) = json_command(&rig, &args)?;
    assert_eq!(status, 0, "{replay}");
    assert_eq!(answer["data"], replay["data"]);
    std::fs::write(&reply_file, "Different")?;
    let (status, conflict) = json_command(&rig, &args)?;
    assert_ne!(status, 0);
    assert_eq!(conflict["error"]["code"], "operation_conflict");
    let mut new_operation = args;
    new_operation[11] = "another-answer";
    let (status, conflict) = json_command(&rig, &new_operation)?;
    assert_ne!(status, 0);
    assert_eq!(conflict["error"]["code"], "conflict");
    let (_, before_answered) = json_command(&rig, &["inbox", &qualified, "--json"])?;
    let (_, before_open) = json_command(&rig, &["inbox", &format!("{peer}:{other}"), "--json"])?;
    // Bypass CLI routing and send forged mutations directly to the owner.
    let answer_id = answer["data"]["answer_id"].as_str().ok_or("answer id")?;
    let key = local_identity["data"]["federation"]["key"]
        .as_str()
        .ok_or("project key")?;
    for (input, expected) in [
        (
            json!({"op":"close","id":other,"state":"withdrawn","reason":"forged","expect_revision":1,"operation_id":"forged-close"}),
            "OWNER_LOCAL_ONLY",
        ),
        (
            json!({"op":"acknowledge","id":id,"answer_id":answer_id,"session":"chair-a","operation_id":"forged-ack"}),
            "OWNER_LOCAL_ONLY",
        ),
        (
            json!({"op":"ask","session":"chair-a","request":{"kind":"question","title":"Forged","body":"No","blocking":false,"options":[],"recommended_option_id":null,"proceeding_with":null},"operation_id":"forged-ask"}),
            "invalid_input",
        ),
        (
            json!({"op":"answer","id":other,"action":"reply","option_id":null,"text":"No ID","expect_revision":1,"channel":"console","operation_id":null}),
            "invalid_input",
        ),
    ] {
        let request = json!({
            "version":1, "protocol":provenant_fabric_runtime::hosts::INBOX_PROTOCOL,
            "target_host":peer.to_string(), "source_host":local_identity["data"]["host"],
            "agent":"fixture", "project":{"key":key,"common_dir":"repo/.git"},
            "action":{"action":"inbox","input":input}
        });
        let output = rig.execute_os(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "--state-dir".into(),
                peer_state.clone().into(),
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
        assert_eq!(response["result"]["data"]["code"], expected, "{response}");
    }
    let (status, close) = json_command(
        &rig,
        &[
            "inbox",
            "close",
            &other,
            "--host",
            "workshop",
            "--state",
            "withdrawn",
            "--reason",
            "obsolete",
            "--expect-revision",
            "1",
            "--operation-id",
            "remote-close",
            "--json",
        ],
    )?;
    assert_ne!(status, 0);
    assert_eq!(close["error"]["code"], "OWNER_LOCAL_ONLY");
    let (status, ack) = json_command(
        &rig,
        &[
            "inbox",
            "acknowledge",
            &qualified,
            "--answer",
            answer_id,
            "--session",
            "chair-a",
            "--operation-id",
            "remote-ack",
            "--json",
        ],
    )?;
    assert_ne!(status, 0);
    assert_eq!(ack["error"]["code"], "OWNER_LOCAL_ONLY");
    let (status, shown) = json_command(&rig, &["inbox", &qualified, "--json"])?;
    assert_eq!(status, 0, "{shown}");
    assert_eq!(
        shown["data"]["item"]["answer"]["source_host"],
        local_identity["data"]["host"]
    );
    assert_eq!(
        shown["data"]["item"]["answer"]["actor"],
        local_identity["data"]["inbox"]["actor"]
    );
    assert_eq!(shown["data"], before_answered["data"]);
    assert_eq!(shown["data"]["item"]["delivery"]["state"], "queued");
    let (status, untouched) = json_command(&rig, &["inbox", &format!("{peer}:{other}"), "--json"])?;
    assert_eq!(status, 0, "{untouched}");
    assert_eq!(untouched["data"], before_open["data"]);
    assert_eq!(untouched["data"]["item"]["request"]["state"], "open");
    assert!(!rig.state.join("fabric").exists());
    std::fs::write(rig.root.join("ssh"), "#!/bin/sh\nexit 1\n")?;
    let (status, offline) = json_command(&rig, &["inbox", "--host", "workshop", "--json"])?;
    assert_ne!(status, 0);
    assert_eq!(offline["error"]["code"], "owner_unavailable");
    assert!(!rig.state.join("fabric").exists());
    Ok(())
}

#[test]
fn graph_cancel_reconciles_a_remote_attempt_using_existing_peer_calls() -> Result {
    use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
    use provenant_fabric_runtime::graph::{FileJournal, GraphDocument, GraphExecution};
    use provenant_os::clock::SystemClock;

    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let peer = loopback_peer(&mut rig)?;
    let peer_state = rig.root.join("peer-state");
    let peer_identity = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
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
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let actor = identity["data"]["inbox"]["actor"].as_str().ok_or("actor")?;
    // A receiver owns its own graph; its WorkKey is not the origin's key.
    // Match the receiver's submission identity, rather than relying on the
    // legacy by-reference fallback when no receiver graph is found.
    let key = provenant_fabric_runtime::graph::AttemptKey {
        extra: Default::default(),
        graph_id: "remote-graph".into(),
        node_id: "one".into(),
        attempt: 1,
        route_index: 0,
    };
    let digest = provenant_records::CanonicalV1::record_digest("graph-attempt-operation", 1, &key)?;
    let submission = provenant_core::OperationId::parse(&format!(
        "graph-{}",
        provenant_core::hex(digest.as_bytes())
    ))?;
    let origin_actor: provenant_core::ActorId = actor.parse()?;
    let digest = provenant_records::CanonicalV1::record_digest(
        "peer-subgraph",
        1,
        &(origin_actor, submission),
    )?;
    let receiver_graph = format!("peer-{}", &provenant_core::hex(digest.as_bytes())[..40]);
    let context =
        serde_json::from_value(json!({"graph_id":receiver_graph,"node_id":"one","attempt":1}))?;
    let (attempt, _) = seed_attempt_in_state(
        &rig,
        &peer_identity,
        &peer_state,
        "fixture",
        false,
        Some(context),
    )?;
    let mut roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let state = roots.resolve(&provenant_os::fs::SystemPathResolver)?.state;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    let document: GraphDocument = serde_json::from_value(
        json!({"schema_version":1,"graph_id":"remote-graph","nodes":[{"id":"one","access":"read_only","prompt":"fixture","route":{"alias":"senior"}}]}),
    )?;
    struct AcceptedPeer {
        actor: String,
        reference: String,
    }
    impl provenant_fabric_runtime::graph::AttemptOwner for AcceptedPeer {
        type Error = std::io::Error;
        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn observe_or_start(
            &mut self,
            _key: &provenant_fabric_runtime::graph::AttemptKey,
            _node: provenant_fabric_runtime::graph::NodeInput<'_>,
            _route: &provenant_fabric_runtime::graph::GraphRoute,
            _access: provenant_fabric_runtime::graph::AccessMode,
            _worktree: Option<&str>,
            _replay: provenant_fabric_runtime::graph::Replay<'_>,
        ) -> std::io::Result<provenant_fabric_runtime::graph::AttemptObservation> {
            Ok(
                provenant_fabric_runtime::graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.reference.clone()),
                },
            )
        }
    }
    // Protocol 13 binds the receiver graph to the origin WorkKey. Seed that
    // relationship through the graph journal, not just an accepted attempt ID.
    let origin_key = provenant_records::WorkKey::new(project, "remote-graph", "one", 1)?;
    let peer_project = ProjectId::parse(
        peer_identity["data"]["project"]
            .as_str()
            .ok_or("peer project")?,
    )?;
    let peer_actor = peer_identity["data"]["inbox"]["actor"]
        .as_str()
        .ok_or("peer actor")?;
    let mut peer_roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    peer_roots.state_dir = Some(peer_state.clone());
    let receiver_state = peer_roots
        .resolve(&provenant_os::fs::SystemPathResolver)?
        .state;
    let receiver_document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":receiver_graph,
        "peer_origin_work_key":origin_key,
        "nodes":[{"id":"one","access":"read_only","prompt":"fixture","route":{"alias":"senior"}}]
    }))?;
    let mut receiver = FileJournal::open(
        &receiver_state,
        peer_project,
        &receiver_graph,
        budget,
        &cancel,
        &clock,
    )?;
    receiver.bind(GraphExecution {
        extra: [(
            "peer_origin_context".into(),
            json!({"graph_id":"remote-graph","node_id":"one"}),
        )]
        .into(),
        integration_base: None,
        review_base: None,
        directory: rig.project.clone(),
        document: receiver_document.clone(),
    })?;
    let receiver_seeded = provenant_fabric_runtime::graph::advance_with_budget(
        &receiver_document,
        &mut AcceptedPeer {
            actor: peer_actor.into(),
            reference: format!("{peer}/{attempt}"),
        },
        &mut receiver,
        &Default::default(),
        &Default::default(),
    )?;
    assert_eq!(
        receiver_seeded.checkpoint.nodes[0].state,
        provenant_fabric_runtime::graph::NodeState::Running
    );
    drop(receiver);
    let execution = FileJournal::execution(&receiver_state, peer_project, &receiver_graph)?
        .ok_or("receiver execution")?;
    assert_eq!(
        execution.document.extra["peer_origin_work_key"],
        json!(origin_key)
    );
    assert_eq!(
        execution.extra["peer_origin_context"],
        json!({"graph_id":"remote-graph","node_id":"one"})
    );
    assert_ne!(execution.document.graph_id, document.graph_id);
    let mut journal = FileJournal::open(&state, project, "remote-graph", budget, &cancel, &clock)?;
    journal.bind(GraphExecution {
        extra: Default::default(),
        integration_base: None,
        review_base: None,
        directory: rig.project.clone(),
        document: document.clone(),
    })?;
    let seeded = provenant_fabric_runtime::graph::advance_with_budget(
        &document,
        &mut AcceptedPeer {
            actor: actor.into(),
            reference: format!("{peer}/{attempt}"),
        },
        &mut journal,
        &Default::default(),
        &Default::default(),
    )?;
    assert_eq!(
        seeded.checkpoint.nodes[0].state,
        provenant_fabric_runtime::graph::NodeState::Running
    );
    drop(journal);
    let (status, result) = json_command(
        &rig,
        &["stop", "remote-graph", "--reason", "peer test", "--json"],
    )?;
    assert_eq!(status, 0, "{result}");
    let stopped =
        FileJournal::read_checkpoint(&state, project, "remote-graph")?.ok_or("checkpoint")?;
    assert_eq!(stopped.cancel_reason.as_deref(), Some("peer test"));
    let receiver_stopped =
        FileJournal::read_checkpoint(&receiver_state, peer_project, &receiver_graph)?
            .ok_or("receiver checkpoint")?;
    assert!(
        receiver_stopped.nodes.iter().all(|node| !matches!(
            node.state,
            provenant_fabric_runtime::graph::NodeState::Pending
                | provenant_fabric_runtime::graph::NodeState::Running
        )),
        "origin cancellation must settle the receiver subtree"
    );
    let receiver_attempt = receiver_stopped
        .nodes
        .iter()
        .find(|node| node.id == "one")
        .ok_or("receiver node")?
        .attempts
        .last()
        .ok_or("receiver attempt")?;
    // The checkpoint keeps launch history. Cancellation authority belongs to
    // the run store, which must seal the actual accepted attempt as cancelled.
    let receiver_context = provenant_run::ExecutionContext {
        state_root: receiver_state,
        project_id: peer_project,
        actor_id: peer_actor.parse()?,
        read_prompts: false,
    };
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = provenant_run::RunStore::open(&receiver_context, &call)?;
    assert!(matches!(
        store.record(attempt, &call)?.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Cancelled,
            ..
        }
    ));
    assert_eq!(
        receiver_attempt.attempt_ref.as_deref(),
        Some(format!("{peer}/{attempt}").as_str())
    );

    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

/// Seed one queued attempt with `prompt` in the owner store, optionally
/// cancelled so it carries two events. Returns the attempt and its events.
fn seed_attempt_in_state(
    rig: &TestRig,
    identity: &Value,
    state_dir: &Path,
    prompt: &str,
    cancel_it: bool,
    work_context: Option<provenant_records::run::WorkContext>,
) -> Result<(
    provenant_core::AttemptId,
    Vec<provenant_records::run::RunEvent>,
)> {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, ProjectId, RootInputs,
    };
    use provenant_os::clock::SystemClock;
    use provenant_records::{
        CanonicalV1,
        run::{CanonicalRequest, RequestedRoute, TaskRequest},
    };

    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let actor = ActorId::parse(identity["data"]["inbox"]["actor"].as_str().ok_or("actor")?)?;
    let mut roots = RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(state_dir.to_owned());
    let state = roots.resolve(&provenant_os::fs::SystemPathResolver)?.state;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: state,
        project_id: project,
        actor_id: actor,
        read_prompts: false,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    // A launch without a work key is refused, so an unlabelled seed gets one.
    let work_context = work_context.unwrap_or_else(|| provenant_records::run::WorkContext {
        graph_id: Some("page-seed-graph".into()),
        node_id: Some("node".into()),
        attempt: Some(1),
        ..Default::default()
    });
    let attempt_id = AttemptId::from_bytes([91; 16]);
    store.create(
        &provenant_run::PreparedRun {
            attempt_id,
            operation_id: OperationId::parse("page-seed")?,
            created_at: clock.wall_now()?,
            request: CanonicalRequest {
                extra: Default::default(),
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                task: TaskRequest {
                    context: Some(work_context),
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    source_head: None,
                    base_ref: None,
                    writer: None,
                    profile: provenant_records::run::HarnessProfile::default(),
                    prompt_digest: CanonicalV1::file_digest(prompt.as_bytes()),
                    route: RequestedRoute::Model {
                        extra: Default::default(),
                        provider: "fixture".into(),
                        model: "fixture".into(),
                        effort: None,
                    },
                    account: None,
                    authority_digest: Digest::from_bytes([94; 32]),
                    provider_timeout_seconds: 60,
                    delivery_id: None,
                    deliver: None,
                    access: Default::default(),
                },
                parent: None,
                lane_depth: Some(1),
                parent_attempt: None,
            },
            prompt: prompt.into(),
        },
        &call,
    )?;
    if cancel_it {
        store.cancel(attempt_id, "page fixture", &call)?;
    }
    let events = store.events(0, 64, Some(attempt_id), &call)?;
    Ok((attempt_id, events))
}

#[test]
fn run_lane_document_with_overrides_at_cli_edge() -> Result {
    let rig = TestRig::new()?;
    let document = rig.root.join("lane.toml");
    // A composite template receives both a named parameter and each item.
    std::fs::write(
        &document,
        r#"
nodes = [{ id = "inspect", use = "task", brief = "Inspect ${subject}: ${item}" }]
[parameters.subject]
type = "string"
"#,
    )?;
    let path = document.to_str().ok_or("document path")?;
    let output = rig.command(&[
        "run",
        "--file",
        path,
        "--set",
        // A string parameter takes the text as written (cli.md).
        "subject=code",
        "--over",
        "one",
        "--over",
        "two",
        "--explain",
        "--json",
    ])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let text = value["data"]["document"].to_string();
    assert!(text.contains("Inspect code: one"), "{text}");
    assert!(text.contains("Inspect code: two"), "{text}");
    Ok(())
}
