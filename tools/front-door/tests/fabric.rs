//! End-to-end Fabric commands use the isolated binary rig, never real state.
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
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value.as_object().ok_or("object")?.len(), 5);
    Ok(value)
}
#[test]
fn identity_mail_tasks_and_events_use_durable_scoped_receipts() -> Result {
    let rig = TestRig::new()?;
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
    rig.assert_legacy_unchanged()
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
    rig.assert_legacy_unchanged()
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
        vec!["fabric", "events", "read", "--wait", "14401"],
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
    rig.assert_legacy_unchanged()
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
    for bad in ["lease_seconds=3601", "page_rows=0"] {
        std::fs::write(
            rig.state.join("config.toml"),
            format!("schema_version=1\n[domains.fabric.limits]\n{bad}\n"),
        )?;
        assert_eq!(call(&rig, &["fabric", "identity"], 2)?["status"], "invalid");
    }
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
    rig.assert_legacy_unchanged()
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
    rig.assert_legacy_unchanged()
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
    rig.assert_legacy_unchanged()
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
fn fabric_uses_the_binary_product_root_protection() -> Result {
    let mut rig = TestRig::new()?;
    let product = rig.root.join("product");
    std::fs::create_dir(&product)?;
    rig.state = product.join("state");
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = product.clone().into_os_string();
        }
    }
    assert_eq!(call(&rig, &["fabric", "identity"], 2)?["status"], "invalid");
    assert!(!rig.state.exists());
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
