//! End-to-end Fabric commands use the isolated binary rig, never real state.
use crate::support;
use serde_json::{Value, json};
use support::{Result, TestRig, call, code};
#[test]
fn identity_mail_tasks_and_events_use_durable_scoped_receipts() -> Result {
    let rig = support::rig()?;
    let identity = call(&rig, &["fabric", "identity"], 0)?;
    assert_eq!(identity["data"]["inbox"]["agent"], "fixture");
    assert_eq!(
        identity["data"]["project"],
        call(&rig, &["fabric", "identity"], 0)?["data"]["project"]
    );
    let send = [
        "fabric",
        "mail",
        "send",
        "--to",
        "fixture",
        "--body",
        "review this",
        "--operation-id",
        "mail-1",
    ];
    let first = call(&rig, &send, 0)?;
    assert_eq!(first, call(&rig, &send, 0)?);
    let mail = call(&rig, &["fabric", "mail", "read", "--after", "0"], 0)?;
    assert_eq!(
        mail["data"]["page"]["items"]
            .as_array()
            .ok_or("mail")?
            .len(),
        1
    );
    let cursor = mail["data"]["cursor"].to_string();
    call(
        &rig,
        &[
            "fabric",
            "mail",
            "mark-read",
            &cursor,
            "--operation-id",
            "mark-1",
        ],
        0,
    )?;
    assert_eq!(
        call(&rig, &["fabric", "mail", "read"], 0)?["data"]["page"]["items"],
        json!([])
    );
    let note = [
        "fabric",
        "events",
        "note",
        "--note",
        "review done",
        "--operation-id",
        "event-1",
    ];
    assert_eq!(call(&rig, &note, 0)?, call(&rig, &note, 0)?);
    assert!(!rig.state.join("run.sqlite3").exists());
    let events = call(&rig, &["fabric", "events", "read", "--after", "0"], 0)?;
    assert!(!rig.state.join("run.sqlite3").exists());
    assert!(
        !events["data"]["page"]["items"]
            .as_array()
            .ok_or("events")?
            .is_empty()
    );
    let conflict = call(
        &rig,
        &[
            "fabric",
            "task",
            "create",
            "--objective",
            "different",
            "--operation-id",
            "task-1",
        ],
        2,
    )?;
    assert_eq!(conflict["status"], "invalid");
    rig.assert_v1_root_untouched()
}
#[test]
fn custody_claim_adoption_and_release_share_the_mine_view() -> Result {
    let mut rig = TestRig::new()?;
    let acquired = call(
        &rig,
        &[
            "fabric",
            "claim",
            "acquire",
            "issue:3078",
            "--operation-id",
            "work-1",
        ],
        0,
    )?;
    assert_eq!(
        acquired["data"]["change"]["record"]["state"]["state"],
        "active"
    );
    let mine = call(&rig, &["status", "--mine"], 0)?;
    assert!(
        mine["data"]["items"]
            .as_array()
            .ok_or("custody items")?
            .iter()
            .any(|item| item["record"]["subject"]["id"] == "3078")
    );
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_AGENT_LABEL" {
            *value = "other".into();
        }
    }
    // Advisory custody remains inspectable and adoptable by another live chair.
    let inspected = call(&rig, &["fabric", "claim", "verify", "issue:3078"], 0)?;
    let adopted = call(&rig, &["adopt", "issue:3078"], 0)?;
    assert_eq!(adopted["data"]["record"]["origin"], "adopted");
    assert_ne!(
        inspected["data"]["record"]["holder"],
        adopted["data"]["record"]["holder"]
    );
    let released = call(&rig, &["release", "issue:3078"], 0)?;
    assert_eq!(released["data"]["record"]["state"]["state"], "released");
    let mine = call(&rig, &["status", "--mine"], 0)?;
    assert!(
        !mine["data"]["items"]
            .as_array()
            .ok_or("custody items")?
            .iter()
            .any(|item| item["record"]["subject"]["id"] == "3078")
    );
    let ledger = call(&rig, &["fabric", "claim", "list"], 0)?;
    assert!(
        ledger["data"]["page"]["items"]
            .as_array()
            .ok_or("ledger items")?
            .iter()
            .any(|item| item["subject"]["id"] == "3078" && item["state"]["state"] == "released")
    );
    rig.assert_v1_root_untouched()
}

#[test]
fn chair_custody_matches_checkpoint_identity_for_harnesses_and_overrides() -> Result {
    let mut rig = TestRig::new()?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nclient_label='terminal'\n",
    )?;
    let checkpoint = rig.root.join("checkpoint.md");
    std::fs::write(&checkpoint, "# Goal\nChair identity fixture.\n")?;
    let identity = call(&rig, &["fabric", "identity"], 0)?["data"].clone();
    let project = identity["project"].as_str().ok_or("project identity")?;

    // Spec: docs/specs/fabric.md#identity-and-store
    let cases = [
        (
            "codex",
            Some(("CODEX_THREAD_ID", "chair-fixture")),
            None,
            None,
        ),
        ("claude", Some(("CLAUDECODE", "1")), None, None),
        (
            "codex",
            Some(("CODEX_THREAD_ID", "chair-fixture")),
            Some("named-chair"),
            None,
        ),
        ("terminal", None, None, None),
        (
            "codex",
            Some(("CODEX_THREAD_ID", "chair-fixture")),
            None,
            Some("explicit-session"),
        ),
    ];
    for (index, (client, marker, agent, session_override)) in cases.into_iter().enumerate() {
        rig.env.retain(|(key, _)| {
            ![
                "PROVENANT_AGENT_LABEL",
                "PROVENANT_SESSION_ID",
                "CODEX_THREAD_ID",
                "CLAUDECODE",
                "CLAUDE_CODE_SESSION_ID",
                "CLAUDE_SESSION_ID",
            ]
            .iter()
            .any(|name| key == name)
        });
        if let Some((name, value)) = marker {
            rig.env.push((name.into(), value.into()));
        }
        if let Some(agent) = agent {
            rig.env.push(("PROVENANT_AGENT_LABEL".into(), agent.into()));
        }

        let session = format!("{client}-chair-{index}");
        call(
            &rig,
            &[
                "workflow",
                "session",
                "save",
                &session,
                "--file",
                checkpoint.to_str().ok_or("checkpoint path")?,
            ],
            0,
        )?;
        let discovery: Value = serde_json::from_slice(&std::fs::read(rig.state.join(format!(
            "workflow/projects/{project}/sessions/{session}/discovery.json"
        )))?)?;
        let identity = call(&rig, &["fabric", "identity"], 0)?["data"].clone();
        assert_eq!(identity["client"], client);
        assert_eq!(identity["inbox"]["agent"], agent.unwrap_or(client));
        assert_eq!(identity["inbox"]["actor"], discovery["chair_actor"]);
        if let Some(session_override) = session_override {
            rig.env
                .push(("PROVENANT_SESSION_ID".into(), session_override.into()));
        }

        let subject_id = (40_000 + index).to_string();
        let subject = format!("issue:{subject_id}");
        let operation = format!("chair-identity-{index}");
        let acquired = call(
            &rig,
            &[
                "fabric",
                "claim",
                "acquire",
                &subject,
                "--operation-id",
                &operation,
            ],
            0,
        )?;
        let holder = &acquired["data"]["change"]["record"]["holder"]["chair"];
        let expected_session = session_override.unwrap_or(&session);
        assert_eq!(holder["actor"], discovery["chair_actor"]);
        assert_eq!(holder["session"], expected_session);

        let mine = call(&rig, &["status", "--mine", "--full"], 0)?;
        let item = mine["data"]["items"]
            .as_array()
            .ok_or("custody items")?
            .iter()
            .find(|item| item["record"]["subject"]["id"] == subject_id)
            .ok_or("current chair custody missing from status")?;
        assert!(item["mine"].as_bool().ok_or("mine status")?);
        assert_eq!(
            item["record"]["holder"]["chair"]["actor"],
            discovery["chair_actor"]
        );
        assert_eq!(
            item["record"]["holder"]["chair"]["session"],
            expected_session
        );
        assert_eq!(item["record"]["subject"]["kind"], "issue");
    }
    rig.assert_v1_root_untouched()
}

#[test]
fn malformed_operations_and_waits_fail_before_state_creation() -> Result {
    let rig = TestRig::new()?.without_state()?;
    for args in [
        vec![
            "fabric",
            "events",
            "note",
            "--note",
            "x",
            "--operation-id",
            "bad/id",
        ],
        // Oversized observation waits clamp to the supported maximum; use
        // malformed syntax to verify this path refuses before state creation.
        vec!["fabric", "events", "read", "--wait", "not-a-number"],
    ] {
        assert_eq!(call(&rig, &args, 2)?["status"], "invalid");
        assert!(!rig.state.exists());
    }
    let help = call(&rig, &["help", "fabric"], 0)?;
    let commands = help["data"]["commands"].as_array().ok_or("catalogue")?;
    assert!(
        commands
            .iter()
            .any(|v| v["command"] == "provenant fabric claim list")
    );
    assert!(!rig.state.exists());
    rig.assert_v1_root_untouched()
}
#[test]
fn domain_config_accepts_typed_peers_and_only_lower_limits() -> Result {
    let rig = TestRig::new()?;
    std::fs::create_dir_all(&rig.state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&rig.state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.fabric.peers.workshop]\nhost='00000000000000000000000000000001'\ndestination='user@workshop'\n",
    )?;
    call(&rig, &["fabric", "identity"], 0)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.fabric.limits]\npage_rows=0\n",
    )?;
    assert_eq!(call(&rig, &["fabric", "identity"], 2)?["status"], "invalid");
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.fabric.limits]\nbody_bytes=4\n",
    )?;
    call(
        &rig,
        &[
            "fabric",
            "mail",
            "send",
            "--to",
            "fixture",
            "--body",
            "12345",
            "--operation-id",
            "limit-1",
        ],
        2,
    )?;
    assert!(
        !rig.state.join("fabric/fabric.sqlite3").exists()
            || call(&rig, &["fabric", "mail", "read"], 0)?["data"]["page"]["items"] == json!([])
    );
    rig.assert_v1_root_untouched()
}

#[test]
fn linked_worktrees_share_projects_and_ignore_git_redirects() -> Result {
    let mut rig = TestRig::new()?;
    let output = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        &["commit", "--allow-empty", "-qm", "fixture"],
        None,
    )?;
    assert_eq!(code(&output), 0);
    let linked = rig.home.join("linked");
    let output = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().ok_or("linked path")?,
        ],
        None,
    )?;
    assert_eq!(code(&output), 0);
    let primary = call(&rig, &["fabric", "identity"], 0)?;
    rig.env
        .push(("GIT_DIR".into(), "/nonexistent/poison".into()));
    rig.env
        .push(("GIT_COMMON_DIR".into(), "/nonexistent/poison".into()));
    let worker = call(
        &rig,
        &[
            "fabric",
            "identity",
            "--project",
            linked.to_str().ok_or("linked path")?,
        ],
        0,
    )?;
    assert_eq!(primary["data"]["project"], worker["data"]["project"]);
    assert_eq!(
        worker["data"]["cwd"],
        serde_json::to_value(provenant_records::AbsolutePath::new(
            &std::fs::canonicalize(&linked)?
        )?)?
    );
    assert_eq!(primary["data"]["inbox"], worker["data"]["inbox"]);
    // Non-Git cwd uses config's canonical directory binding and stays local.
    std::fs::rename(
        rig.project.join(".git"),
        rig.project.join("git-fixture-backup"),
    )?;
    let non_git = call(&rig, &["fabric", "identity"], 0)?;
    assert_ne!(primary["data"]["project"], non_git["data"]["project"]);
    assert_eq!(non_git["data"]["federation"], Value::Null);
    rig.assert_v1_root_untouched()
}

#[test]
fn non_git_directories_beneath_home_stay_local_and_remote_homes_refuse_local_writes() -> Result {
    let mut rig = TestRig::new()?;
    rig.project = rig.home.join("workspace");
    std::fs::create_dir(&rig.project)?;
    let local = call(&rig, &["fabric", "identity"], 0)?;
    assert_eq!(local["data"]["federation"], Value::Null);
    let output = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        &["init", "-q"],
        None,
    )?;
    assert_eq!(code(&output), 0);
    let git = call(&rig, &["fabric", "identity"], 0)?;
    let key = git["data"]["federation"]["key"]
        .as_str()
        .ok_or("federation key")?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric.home_hosts]\n'{key}'='00000000000000000000000000000001'\n"
        ),
    )?;
    let refused = call(
        &rig,
        &[
            "fabric",
            "events",
            "note",
            "--note",
            "local writes refused",
            "--operation-id",
            "remote-1",
        ],
        3,
    )?;
    assert_eq!(refused["error"]["code"], "home_unavailable");
    assert!(!rig.state.join("fabric/fabric.sqlite3").exists());
    rig.assert_v1_root_untouched()
}

#[test]
fn empty_environment_label_falls_through_to_config() -> Result {
    let mut rig = TestRig::new()?;
    call(&rig, &["fabric", "identity"], 0)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.fabric]\nagent_label='configured'\n",
    )?;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_AGENT_LABEL" {
            *value = "".into();
        }
    }
    assert_eq!(
        call(&rig, &["fabric", "identity"], 0)?["data"]["inbox"]["agent"],
        "configured"
    );
    Ok(())
}

#[test]
fn fabric_never_creates_ancestors_of_the_state_root() -> Result {
    let mut rig = TestRig::new()?;
    let parent = rig.root.join("missing-parent");
    rig.state = parent.join("state");
    let result = call(&rig, &["fabric", "identity"], 3)?;
    assert_eq!(result["error"]["code"], "io");
    assert!(!parent.exists());
    Ok(())
}

#[test]
fn fabric_identity_allows_state_under_product_checkout() -> Result {
    let mut rig = TestRig::new()?;
    let product = rig.root.join("product");
    std::fs::create_dir(&product)?;
    rig.state = product.join("state");
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = product.clone().into_os_string();
        }
    }
    let identity = call(&rig, &["fabric", "identity"], 0)?;
    assert_eq!(identity["status"], "ok");
    Ok(())
}
#[test]
fn hosts_lists_readiness_and_fabric_host_is_gone() -> Result {
    let rig = TestRig::new()?;
    let hosts = call(&rig, &["hosts"], 0)?;
    assert_eq!(hosts["command"], "hosts");
    let rows = hosts["data"].as_array().ok_or("hosts")?;
    assert!(rows.iter().any(|row| row["available"] == true));
    let removed = rig.command(&["fabric", "host", "list"])?;
    assert_eq!(code(&removed), 2);
    Ok(())
}
