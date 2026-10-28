//! Workflow leaves through the isolated binary: receipts, review coverage, sessions.
mod support;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn call(rig: &TestRig, args: &[&str], expected: i64) -> Result<Value> {
    let mut args = args.to_vec();
    args.push("--json");
    let output = rig.command(&args)?;
    assert_eq!(
        code(&output),
        expected,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}
fn request(rig: &TestRig, name: &str, value: Value) -> Result<String> {
    let path = rig.root.join(name);
    std::fs::write(&path, serde_json::to_vec(&value)?)?;
    Ok(path.to_string_lossy().into_owned())
}
fn receipt_request(critical: &str, decision: Value) -> Value {
    let digest = format!("sha256:{}", "0".repeat(64));
    json!({
        "subject": {"kind": "document", "path": {"encoding": "utf8", "path": "docs/x.md"}, "digest": digest},
        "authors": [{"kind": "user", "actor_id": "01".repeat(16)}],
        "risk": {
            "factors": {
                "blast_radius": "local", "reversibility": "easy", "data_sensitivity": "public",
                "migration": "none", "oracle_quality": "strong", "external_effects": "none",
                "critical_surface": critical
            },
            "tier": if critical == "none" { "routine" } else { "substantial" }
        },
        "decision": decision
    })
}
const DELIVERY: &str = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";

fn state_snapshot(root: &std::path::Path) -> Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
    let mut snapshot = Vec::new();
    if root.exists() {
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if path.is_dir() {
                snapshot.push((path.clone(), Vec::new()));
                snapshot.extend(state_snapshot(&path)?);
            } else {
                snapshot.push((path.clone(), std::fs::read(path)?));
            }
        }
    }
    snapshot.sort();
    Ok(snapshot)
}

#[test]
fn session_reads_allocate_nothing() -> Result {
    let rig = TestRig::new()?;
    let before = state_snapshot(&rig.state)?;
    for args in [
        vec!["workflow", "session", "show", "absent"],
        vec!["workflow", "session", "show", "absent", "--revisions"],
        vec!["workflow", "session", "show", "absent", "--rev", "1"],
        vec!["workflow", "session", "diff", "absent", "1"],
    ] {
        call(&rig, &args, 1)?;
        assert_eq!(state_snapshot(&rig.state)?, before);
    }
    std::fs::remove_dir_all(&rig.state)?;
    call(&rig, &["workflow", "session", "show", "absent"], 1)?;
    assert!(!rig.state.exists());
    Ok(())
}

#[test]
fn chair_log_cli_replay_and_pages() -> Result {
    let rig = TestRig::new()?;
    let first = request(
        &rig,
        "log.json",
        json!({"kind":"progress", "markdown":"First update", "channel":"chat"}),
    )?;
    let append = |file: &str, operation: &str| {
        call(
            &rig,
            &[
                "chair",
                "log",
                "append",
                "--session",
                "chair",
                "--file",
                file,
                "--operation-id",
                operation,
            ],
            0,
        )
    };
    let original = append(&first, "first")?;
    assert_eq!(append(&first, "first")?["data"], original["data"]);
    let correction = request(
        &rig,
        "correction.json",
        json!({"kind":"correction", "markdown":"Corrected", "channel":"chat", "correction_of":1}),
    )?;
    append(&correction, "correction")?;
    call(
        &rig,
        &[
            "chair",
            "log",
            "append",
            "--session",
            "chair",
            "--file",
            &correction,
            "--operation-id",
            "first",
        ],
        1,
    )?;
    let page = call(
        &rig,
        &["chair", "log", "list", "--session", "chair", "--limit", "1"],
        0,
    )?;
    assert_eq!(page["data"]["entries"][0]["markdown"], "First update");
    assert_eq!(page["data"]["next_cursor"], 1);
    let next = call(
        &rig,
        &["chair", "log", "list", "--session", "chair", "--after", "1"],
        0,
    )?;
    assert_eq!(next["data"]["entries"][0]["correction_of"], 1);
    assert!(next["data"]["next_cursor"].is_null());
    Ok(())
}

#[test]
fn chair_log_cli_retention_replay_and_lane_refusal() -> Result {
    let mut rig = TestRig::new()?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[retention]\nchair_log_entries=1\n",
    )?;
    let first = request(
        &rig,
        "first.json",
        json!({"kind":"progress", "markdown":"First", "channel":"chat"}),
    )?;
    let second = request(
        &rig,
        "second.json",
        json!({"kind":"progress", "markdown":"Second", "channel":"chat"}),
    )?;
    let append = |rig: &TestRig, file: &str, operation: &str, expected| {
        call(
            rig,
            &[
                "chair",
                "log",
                "append",
                "--session",
                "chair",
                "--file",
                file,
                "--operation-id",
                operation,
            ],
            expected,
        )
    };
    let original = append(&rig, &first, "first", 0)?;
    append(&rig, &second, "second", 0)?;
    let page = call(&rig, &["chair", "log", "list", "--session", "chair"], 0)?;
    assert_eq!(
        page["data"]["entries"].as_array().ok_or("entries")?.len(),
        1
    );
    assert_eq!(page["data"]["entries"][0]["markdown"], "Second");
    assert_eq!(page["data"]["total_pruned"], 1);
    assert_eq!(append(&rig, &first, "first", 0)?["data"], original["data"]);
    rig.env.push(("PROVENANT_LANE_DEPTH".into(), "1".into()));
    let refused = append(&rig, &first, "lane", 1)?;
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("lane cannot append")
    );
    Ok(())
}

#[test]
fn session_show_honours_project_and_state_dir_without_allocating() -> Result {
    let rig = TestRig::new()?;
    let other = rig.root.join("other");
    std::fs::create_dir(&other)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    assert_eq!(
        code(&rig.execute(&git, &["init", other.to_str().ok_or("project")?], None)?),
        0
    );
    let file = rig.root.join("checkpoint.md");
    std::fs::write(&file, "selected project\n")?;
    call(
        &rig,
        &[
            "--project",
            &other.to_string_lossy(),
            "workflow",
            "session",
            "save",
            "selected",
            "--file",
            &file.to_string_lossy(),
        ],
        0,
    )?;
    let before = state_snapshot(&rig.state)?;
    for mode in [vec![], vec!["--revisions"], vec!["--rev", "1"]] {
        let mut args = vec![
            "--project",
            other.to_str().ok_or("project")?,
            "workflow",
            "session",
            "show",
            "selected",
        ];
        args.extend(mode);
        let shown = call(&rig, &args, 0)?;
        assert_eq!(shown["status"], "ok");
        assert_eq!(state_snapshot(&rig.state)?, before);
    }
    // TestRig::command always supplies an explicit --state-dir. A different
    // directory must not fall back to that store or allocate a new one.
    let absent_state = rig.root.join("absent-state");
    let output = rig.execute(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--state-dir",
            absent_state.to_str().ok_or("state")?,
            "--project",
            other.to_str().ok_or("project")?,
            "workflow",
            "session",
            "show",
            "selected",
            "--json",
        ],
        None,
    )?;
    assert_eq!(code(&output), 1);
    assert!(!absent_state.exists());
    Ok(())
}

#[test]
fn a_lane_cannot_register_or_overwrite_a_chair() -> Result {
    let mut rig = TestRig::new()?;
    let file = rig.root.join("checkpoint.md");
    std::fs::write(&file, "chair\n")?;
    call(
        &rig,
        &[
            "workflow",
            "session",
            "save",
            "chair",
            "--file",
            &file.to_string_lossy(),
        ],
        0,
    )?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    rig.env
        .push(("PROVENANT_AGENT_LABEL".into(), "lane-attempt".into()));
    rig.env.push(("PROVENANT_LANE_DEPTH".into(), "1".into()));
    let before = state_snapshot(&rig.state.join("workflow"))?;
    for session in ["chair", "lane"] {
        let refused = call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                session,
                "--file",
                &file.to_string_lossy(),
                "--rebind",
            ],
            1,
        )?;
        assert_eq!(refused["error"]["code"], "IN_LANE");
        assert_eq!(state_snapshot(&rig.state.join("workflow"))?, before);
    }
    rig.env
        .retain(|(key, _)| key != "PROVENANT_AGENT_LABEL" && key != "PROVENANT_LANE_DEPTH");
    rig.env
        .push(("PROVENANT_AGENT_LABEL".into(), "fixture".into()));
    call(
        &rig,
        &[
            "workflow",
            "session",
            "save",
            "chair",
            "--file",
            &file.to_string_lossy(),
        ],
        0,
    )?;
    Ok(())
}

#[test]
fn conflicting_markers_fall_back_to_the_configured_client() -> Result {
    let mut rig = TestRig::new()?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nclient_label='terminal'\n",
    )?;
    let identity = call(&rig, &["fabric", "identity"], 0)?["data"].clone();
    rig.env.push(("CODEX_THREAD_ID".into(), "nested".into()));
    rig.env.push(("CLAUDECODE".into(), "1".into()));
    let file = rig.root.join("checkpoint.md");
    std::fs::write(&file, "notes\n")?;
    call(
        &rig,
        &[
            "workflow",
            "session",
            "save",
            "conflict",
            "--file",
            file.to_str().ok_or("file")?,
        ],
        0,
    )?;
    let project = identity["project"].as_str().ok_or("project")?;
    let discovery: Value = serde_json::from_slice(&std::fs::read(rig.state.join(format!(
        "workflow/projects/{project}/sessions/conflict/discovery.json"
    )))?)?;
    assert_eq!(discovery["chair_actor"], identity["inbox"]["actor"]);
    let mut mcp = support::mcp::McpClient::default();
    mcp.initialise("2025-11-25")?;
    mcp.initialised()?;
    let id = mcp.call("fabric_context", json!({"op":"inspect"}))?;
    let replies = mcp.eof(
        &rig,
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["fabric", "mcp"],
    )?;
    let mcp_identity = &replies.get(&id).ok_or("identity")?["result"]["structuredContent"];
    assert_eq!(mcp_identity["inbox"], identity["inbox"]);
    assert_eq!(mcp_identity["client"], "terminal");
    Ok(())
}

#[test]
fn session_save_discovery_restart() -> Result {
    let mut rig = TestRig::new()?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    let mut actors = Vec::new();
    for (client, marker, value) in [
        ("codex", "CODEX_THREAD_ID", "chair-fixture"),
        ("claude", "CLAUDECODE", "1"),
    ] {
        rig.env.retain(|(key, _)| {
            key != "PROVENANT_AGENT_LABEL" && key != "CODEX_THREAD_ID" && key != "CLAUDECODE"
        });
        rig.env.push((marker.into(), value.into()));
        // Explicit clients win even when nested harness markers conflict.
        let other_marker = if client == "codex" {
            "CLAUDECODE"
        } else {
            "CODEX_THREAD_ID"
        };
        rig.env.push((other_marker.into(), "1".into()));
        let session = format!("{client}-chair");
        let hook_session = format!("{client}-hook-first");
        let mut mcp = support::mcp::McpClient::default();
        mcp.initialise("2025-11-25")?;
        mcp.initialised()?;
        let id = mcp.call("fabric_context", json!({"op":"inspect"}))?;
        let replies = mcp.eof(
            &rig,
            std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["fabric", "mcp", "--client", client],
        )?;
        let identity = &replies.get(&id).ok_or("MCP identity")?["result"]["structuredContent"];
        let file = rig.root.join("checkpoint.md");
        std::fs::write(&file, "# Goal\nExact chair identity.\n")?;
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                session.as_str(),
                "--file",
                &file.to_string_lossy(),
                "--client",
                client,
            ],
            0,
        )?;
        rig.env.retain(|(key, _)| key != other_marker);
        let mut inferred = support::mcp::McpClient::default();
        inferred.initialise("2025-11-25")?;
        inferred.initialised()?;
        let inferred_id = inferred.call("fabric_context", json!({"op":"inspect"}))?;
        let inferred_replies = inferred.eof(
            &rig,
            std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
            &["fabric", "mcp"],
        )?;
        let inferred_identity = &inferred_replies
            .get(&inferred_id)
            .ok_or("inferred identity")?["result"]["structuredContent"];
        assert_eq!(inferred_identity["inbox"], identity["inbox"]);
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                session.as_str(),
                "--file",
                &file.to_string_lossy(),
            ],
            0,
        )?;
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                hook_session.as_str(),
                "--file",
                &file.to_string_lossy(),
            ],
            0,
        )?;
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                hook_session.as_str(),
                "--file",
                &file.to_string_lossy(),
                "--client",
                client,
            ],
            0,
        )?;
        let project = identity["project"].as_str().ok_or("project identity")?;
        let manifest_path = rig.state.join(format!(
            "workflow/projects/{project}/sessions/{session}/revisions.json"
        ));
        let manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        let discovery_path = manifest_path.with_file_name("discovery.json");
        let discovery: Value = serde_json::from_slice(&std::fs::read(&discovery_path)?)?;
        assert!(manifest.get("discovery").is_none());
        assert_eq!(discovery["schema_version"], 1);
        assert_eq!(discovery["host"], identity["host"]);
        assert_eq!(discovery["project_id"], identity["project"]);
        assert_eq!(discovery["chair_actor"], identity["inbox"]["actor"]);
        assert_eq!(discovery["host_session_id"], session.as_str());
        assert_eq!(discovery["checkpoint"], manifest["revisions"][0]);
        actors.push(discovery["chair_actor"].clone());
        // Each CLI call is a fresh process. Reads leave every state file unchanged.
        let before = state_snapshot(&rig.state)?;
        let shown = call(&rig, &["workflow", "session", "show", session.as_str()], 0)?;
        assert_eq!(shown["data"]["markdown"], "# Goal\nExact chair identity.\n");
        call(
            &rig,
            &[
                "workflow",
                "session",
                "show",
                session.as_str(),
                "--revisions",
            ],
            0,
        )?;
        call(
            &rig,
            &[
                "workflow",
                "session",
                "show",
                session.as_str(),
                "--rev",
                "1",
            ],
            0,
        )?;
        call(
            &rig,
            &["workflow", "session", "diff", session.as_str(), "1"],
            0,
        )?;
        assert_eq!(state_snapshot(&rig.state)?, before);
        rig.env
            .push(("PROVENANT_AGENT_LABEL".into(), "changed-chair".into()));
        let refused = call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                session.as_str(),
                "--file",
                &file.to_string_lossy(),
                "--client",
                "claude",
            ],
            1,
        )?;
        assert_eq!(refused["status"], "refused");
        assert_eq!(refused["error"]["code"], "IDENTITY");
        // Only the attempted write's demand telemetry may change on refusal.
        let unchanged: Value = serde_json::from_slice(&std::fs::read(&discovery_path)?)?;
        assert_eq!(unchanged, discovery);
        assert!(
            refused["error"]["message"]
                .as_str()
                .ok_or("message")?
                .contains("--rebind")
        );
        let rebound = call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                session.as_str(),
                "--file",
                &file.to_string_lossy(),
                "--rebind",
            ],
            0,
        )?;
        assert_eq!(rebound["data"]["warnings"], json!([]));
        let rebound: Value = serde_json::from_slice(&std::fs::read(&discovery_path)?)?;
        assert_ne!(rebound["chair_actor"], discovery["chair_actor"]);
        assert_eq!(
            rebound["rebound_from"][0]["actor"],
            discovery["chair_actor"]
        );
        assert_eq!(rebound["rebound_from"][0]["host"], discovery["host"]);
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                session.as_str(),
                "--file",
                &file.to_string_lossy(),
            ],
            0,
        )?;

        // A legacy manifest has no registration; showing it must not register it.
        std::fs::remove_file(discovery_path)?;
        let before = state_snapshot(&rig.state)?;
        call(&rig, &["workflow", "session", "show", session.as_str()], 0)?;
        assert_eq!(state_snapshot(&rig.state)?, before);
    }
    assert_ne!(actors[0], actors[1]);
    Ok(())
}

#[test]
fn session_rebind_recovers_an_old_agent_binding() -> Result {
    let mut rig = TestRig::new()?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    let file = rig.root.join("notes.md");
    std::fs::write(&file, "notes")?;
    let base = [
        "workflow",
        "session",
        "save",
        "old-agent",
        "--file",
        file.to_str().ok_or("file")?,
    ];
    call(&rig, &base, 0)?;
    let mut client = base.to_vec();
    client.extend(["--client", "codex"]);
    let refused = call(&rig, &client, 1)?;
    assert_eq!(refused["error"]["code"], "IDENTITY");
    client.push("--rebind");
    call(&rig, &client, 0)?;
    let mut mcp = support::mcp::McpClient::default();
    mcp.initialise("2025-11-25")?;
    mcp.initialised()?;
    let id = mcp.call("fabric_context", json!({"op":"inspect"}))?;
    let replies = mcp.eof(
        &rig,
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &["fabric", "mcp", "--client", "codex"],
    )?;
    let identity = &replies.get(&id).ok_or("identity")?["result"]["structuredContent"];
    let project = identity["project"].as_str().ok_or("project")?;
    let discovery: Value = serde_json::from_slice(&std::fs::read(rig.state.join(format!(
        "workflow/projects/{project}/sessions/old-agent/discovery.json"
    )))?)?;
    assert_eq!(discovery["chair_actor"], identity["inbox"]["actor"]);
    assert_eq!(
        discovery["rebound_from"][0]["actor"],
        serde_json::to_value(
            provenant_fabric_runtime::identity::Address::new(
                serde_json::from_value(identity["host"].clone())?,
                "agent"
            )?
            .actor
        )?
    );
    Ok(())
}

#[test]
fn long_checkpoints_save_up_to_the_documented_bound() -> Result {
    let rig = TestRig::new()?;
    let file = rig.root.join("long.md");
    let save = |bytes: usize, expected: i64| -> Result<Value> {
        std::fs::write(&file, "x".repeat(bytes - 1) + "\n")?;
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                "s-long",
                "--file",
                &file.to_string_lossy(),
            ],
            expected,
        )
    };
    // Past the old 32 KiB cap, up to and including 256 KiB.
    let saved = save(200 * 1024, 0)?;
    assert_eq!(saved["data"]["saved_bytes"], 200 * 1024);
    assert_eq!(saved["data"]["limit_bytes"], 256 * 1024);
    save(256 * 1024, 0)?;
    let shown = call(&rig, &["workflow", "session", "show", "s-long"], 0)?;
    assert_eq!(
        shown["data"]["markdown"].as_str().map(str::len),
        Some(256 * 1024)
    );
    // One byte over is refused and leaves the saved checkpoint intact.
    save(256 * 1024 + 1, 2)?;
    let again = call(&rig, &["workflow", "session", "show", "s-long"], 0)?;
    assert_eq!(again["data"]["markdown"], shown["data"]["markdown"]);
    Ok(())
}

#[test]
fn session_checkpoints_round_trip_and_reject_unsafe_keys() -> Result {
    let rig = TestRig::new()?;
    let file = rig.root.join("checkpoint.md");
    std::fs::write(&file, "# Goal\nship it\n")?;
    let file = file.to_string_lossy().into_owned();
    call(
        &rig,
        &["workflow", "session", "save", "s-1", "--file", &file],
        0,
    )?;
    let shown = call(&rig, &["workflow", "session", "show", "s-1"], 0)?;
    assert_eq!(shown["data"]["markdown"], "# Goal\nship it\n");
    // Human output is terse: one line per record, never pretty JSON.
    let human = rig.command(&["workflow", "session", "show", "s-1"])?;
    assert_eq!(code(&human), 0);
    let text = String::from_utf8_lossy(&human.stdout.bytes);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.starts_with("markdown=\"# Goal\\nship it\\n\" modified_at="),
        "{text}"
    );
    call(&rig, &["workflow", "session", "show", "s-2"], 1)?;
    call(
        &rig,
        &["workflow", "session", "save", "../x", "--file", &file],
        2,
    )?;
    Ok(())
}

#[test]
fn session_checkpoints_keep_bounded_revisions_and_show_what_changed() -> Result {
    let rig = TestRig::new()?;
    let file = rig.root.join("checkpoint.md");
    let save = |text: &str| -> Result<Value> {
        std::fs::write(&file, text)?;
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                "s-rev",
                "--file",
                &file.to_string_lossy(),
            ],
            0,
        )
    };
    let first = save("# Goal\nship it\n")?;
    assert_eq!(first["data"]["revision"], 1);
    assert_eq!(first["data"]["parent"], Value::Null);
    assert_eq!(first["data"]["keep"], 8);
    let second = save("# Goal\nship it fast\nnext: review\n")?;
    assert_eq!(second["data"]["revision"], 2);
    assert_eq!(second["data"]["parent"], 1);
    assert_eq!(second["data"]["retained"], 2);
    // The earlier checkpoint is still there, whole, and so is the current one.
    let old = call(
        &rig,
        &["workflow", "session", "show", "s-rev", "--rev", "1"],
        0,
    )?;
    assert_eq!(old["data"]["markdown"], "# Goal\nship it\n");
    assert_eq!(old["data"]["revision"]["revision"], 1);
    assert!(
        old["data"]["revision"]["sha256"]
            .as_str()
            .is_some_and(|h| h.starts_with("sha256:"))
    );
    let current = call(&rig, &["workflow", "session", "show", "s-rev"], 0)?;
    assert_eq!(current["data"]["revision"], 2);
    // Plain text: the revision as written, the list and the diff.
    let text = rig.command(&["workflow", "session", "show", "s-rev", "--rev", "1"])?;
    assert_eq!(String::from_utf8(text.stdout.bytes)?, "# Goal\nship it\n");
    let list = rig.command(&["workflow", "session", "show", "s-rev", "--revisions"])?;
    let list = String::from_utf8(list.stdout.bytes)?;
    assert_eq!(list.lines().count(), 2, "{list}");
    assert!(list.starts_with("1 parent=none sha256:"), "{list}");
    assert!(
        list.lines()
            .nth(1)
            .is_some_and(|l| l.starts_with("2 parent=1 ")),
        "{list}"
    );
    let diff = rig.command(&["workflow", "session", "diff", "s-rev", "1"])?;
    assert_eq!(
        String::from_utf8(diff.stdout.bytes)?,
        "--- revision 1\n+++ revision 2 (current)\n@@ -1,2 +1,3 @@\n # Goal\n-ship it\n+ship it fast\n+next: review\n"
    );
    let same = rig.command(&["workflow", "session", "diff", "s-rev", "2"])?;
    assert_eq!(String::from_utf8(same.stdout.bytes)?, "no differences\n");
    // Unknown revisions are not found, and the session is bounded by retention.
    call(
        &rig,
        &["workflow", "session", "show", "s-rev", "--rev", "9"],
        1,
    )?;
    call(&rig, &["workflow", "session", "diff", "s-rev", "9"], 1)?;
    call(&rig, &["workflow", "session", "diff", "s-rev", "0"], 2)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[retention]\nsession_revisions=2\n",
    )?;
    let third = save("three\n")?;
    assert_eq!(
        (
            third["data"]["revision"].clone(),
            third["data"]["keep"].clone()
        ),
        (json!(3), json!(2))
    );
    assert_eq!(third["data"]["retained"], 2);
    call(
        &rig,
        &["workflow", "session", "show", "s-rev", "--rev", "1"],
        1,
    )?;
    let kept = call(
        &rig,
        &["workflow", "session", "show", "s-rev", "--rev", "2"],
        0,
    )?;
    assert_eq!(
        kept["data"]["markdown"],
        "# Goal\nship it fast\nnext: review\n"
    );
    Ok(())
}

#[test]
fn routine_receipt_saves_conflicts_and_accepts_without_reviews() -> Result {
    let rig = TestRig::new()?;
    let pending = request(
        &rig,
        "r1.json",
        receipt_request("none", json!({"status": "pending"})),
    )?;
    let saved = call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "0",
            "--request",
            &pending,
        ],
        0,
    )?;
    assert_eq!(saved["data"]["receipt"]["revision"], 1);
    call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "0",
            "--request",
            &pending,
        ],
        1,
    )?;
    let accept = request(
        &rig,
        "r2.json",
        receipt_request(
            "none",
            json!({"status": "accepted", "reason": "checks green"}),
        ),
    )?;
    let saved = call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "1",
            "--request",
            &accept,
        ],
        0,
    )?;
    assert_eq!(saved["data"]["receipt"]["decision"]["status"], "accepted");
    let shown = call(&rig, &["workflow", "receipt", "show", DELIVERY], 0)?;
    assert_eq!(shown["data"]["revision"], 2);
    Ok(())
}

#[test]
fn substantial_acceptance_is_refused_until_reviews_cover_it() -> Result {
    let rig = TestRig::new()?;
    let pending = request(
        &rig,
        "s1.json",
        receipt_request("public_contract", json!({"status": "pending"})),
    )?;
    call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "0",
            "--request",
            &pending,
        ],
        0,
    )?;
    let accept = request(
        &rig,
        "s2.json",
        receipt_request(
            "public_contract",
            json!({"status": "accepted", "reason": "looks fine"}),
        ),
    )?;
    let refused = call(
        &rig,
        &[
            "workflow",
            "receipt",
            "save",
            DELIVERY,
            "--expected-revision",
            "1",
            "--request",
            &accept,
        ],
        1,
    )?;
    assert_eq!(refused["error"]["code"], "REVIEW_INCOMPLETE");
    assert_eq!(refused["data"]["status"], "incomplete");
    let review = request(&rig, "s3.json", json!({"delivery_id": DELIVERY}))?;
    let report = call(&rig, &["workflow", "review", "--request", &review], 1)?;
    assert_eq!(report["data"]["status"], "incomplete");
    assert_eq!(report["data"]["required"], 1);
    Ok(())
}
