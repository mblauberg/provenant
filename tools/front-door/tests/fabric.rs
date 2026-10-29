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
fn token(proof: &Value) -> Result<String> {
    let token = proof["token"].as_str().ok_or("token string")?;
    assert_eq!(token.len(), 32, "{token}");
    assert!(
        token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{token}"
    );
    Ok(token.to_owned())
}

#[test]
fn events_read_reports_upgrade_refused_for_legacy_import_data() -> Result {
    let rig = TestRig::new()?;
    call(
        &rig,
        &[
            "fabric",
            "events",
            "note",
            "--note",
            "seed store",
            "--operation-id",
            "seed-1",
        ],
        0,
    )?;
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
    let result = call(&rig, &["fabric", "events", "read"], 1)?;
    assert_eq!(result["error"]["code"], "upgrade_refused");
    Ok(())
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
    let create = [
        "fabric",
        "task",
        "create",
        "--objective",
        "review",
        "--operation-id",
        "task-1",
    ];
    let task = call(&rig, &create, 0)?;
    assert_eq!(task, call(&rig, &create, 0)?);
    let id = task["data"]["task"]["id"].as_str().ok_or("task ID")?;
    let claim = call(
        &rig,
        &["fabric", "task", "claim", id, "--operation-id", "claim-1"],
        0,
    )?;
    let proof = &claim["data"]["proof"];
    let generation = proof["generation"].to_string();
    let token = token(proof)?;
    call(
        &rig,
        &[
            "fabric",
            "task",
            "finish",
            id,
            "--generation",
            &generation,
            "--token",
            &token,
            "--operation-id",
            "finish-1",
        ],
        0,
    )?;
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
    let events = call(&rig, &["fabric", "events", "read", "--after", "0"], 0)?;
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
fn ownership_and_expired_fences_are_refused() -> Result {
    let mut rig = TestRig::new()?;
    let acquired = call(
        &rig,
        &[
            "fabric",
            "claim",
            "acquire",
            "--issue",
            "PR-1",
            "--lease-seconds",
            "1",
            "--operation-id",
            "work-1",
        ],
        0,
    )?;
    let proof = &acquired["data"]["proof"];
    let id = proof["id"].as_str().ok_or("claim ID")?;
    let generation = proof["generation"].to_string();
    let token = token(proof)?;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_AGENT_LABEL" {
            *value = "other".into();
        }
    }
    let args = [
        "fabric",
        "claim",
        "verify",
        id,
        "--generation",
        &generation,
        "--token",
        &token,
    ];
    let refusal = call(&rig, &args, 1)?;
    assert_eq!(refusal["error"]["code"], "stale_fence");
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_AGENT_LABEL" {
            *value = "fixture".into();
        }
    }
    // A real bounded observation consumes the one-second lease; no test sleeps.
    let cursor = call(&rig, &["fabric", "events", "read"], 0)?["data"]["cursor"].to_string();
    call(
        &rig,
        &[
            "fabric", "events", "read", "--after", &cursor, "--wait", "1",
        ],
        124,
    )?;
    assert_eq!(call(&rig, &args, 1)?["error"]["code"], "stale_fence");
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
        vec![
            "fabric",
            "landing",
            "wait",
            "--reference",
            "refs/heads/main",
            "--generation",
            "1",
            "--wait",
            "0",
        ],
    ] {
        assert_eq!(call(&rig, &args, 2)?["status"], "invalid");
        assert!(!rig.state.exists());
    }
    let help = call(&rig, &["help", "fabric"], 0)?;
    let commands = help["data"]["commands"].as_array().ok_or("catalogue")?;
    assert!(
        commands
            .iter()
            .any(|v| v["command"] == "provenant fabric landing wait")
    );
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}
#[test]
fn landing_fence_reads_secret_from_environment_without_a_token_argument() -> Result {
    let mut rig = TestRig::new()?;
    let acquired = call(
        &rig,
        &[
            "fabric",
            "landing",
            "acquire",
            "--reference",
            "refs/heads/main",
            "--expected",
            &"a".repeat(40),
            "--candidate",
            &"b".repeat(40),
            "--operation-id",
            "env-land",
        ],
        0,
    )?;
    let proof = &acquired["data"]["proof"];
    let secret = token(proof)?;
    rig.env
        .push(("PROVENANT_LANDING_TOKEN".into(), secret.clone().into()));
    call(
        &rig,
        &[
            "fabric",
            "landing",
            "verify",
            "--reference",
            "refs/heads/main",
            "--id",
            proof["id"].as_str().ok_or("claim")?,
            "--generation",
            &proof["generation"].to_string(),
        ],
        0,
    )?;
    let rejected = call(
        &rig,
        &[
            "fabric",
            "landing",
            "verify",
            "--reference",
            "refs/heads/main",
            "--id",
            proof["id"].as_str().ok_or("claim")?,
            "--generation",
            &proof["generation"].to_string(),
            "--token",
            &secret,
        ],
        2,
    )?;
    assert!(!serde_json::to_string(&rejected)?.contains(&secret));
    Ok(())
}

#[test]
fn landing_observation_never_acquires_and_release_is_replayable() -> Result {
    let mut rig = TestRig::new()?;
    let acquired = call(
        &rig,
        &[
            "fabric",
            "landing",
            "acquire",
            "--reference",
            "refs/heads/main",
            "--expected",
            &"a".repeat(40),
            "--candidate",
            &"b".repeat(40),
            "--operation-id",
            "land-1",
        ],
        0,
    )?;
    let proof = &acquired["data"]["proof"];
    let id = proof["id"].as_str().ok_or("claim ID")?;
    let generation = proof["generation"].to_string();
    let token = token(proof)?;
    rig.env
        .push(("PROVENANT_LANDING_TOKEN".into(), token.into()));
    let released = [
        "fabric",
        "landing",
        "release",
        "--reference",
        "refs/heads/main",
        "--id",
        id,
        "--generation",
        &generation,
        "--operation-id",
        "release-1",
    ];
    assert_eq!(call(&rig, &released, 0)?, call(&rig, &released, 0)?);
    let waited = call(
        &rig,
        &[
            "fabric",
            "landing",
            "wait",
            "--reference",
            "refs/heads/main",
            "--generation",
            &generation,
            "--wait",
            "14400",
        ],
        0,
    )?;
    assert_eq!(waited["data"]["timed_out"], false);
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
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
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
fn malformed_fence_tokens_never_reach_either_error_renderer() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let secret = "0123456789abcdef0123456789abcdefx";
    let args = [
        "fabric",
        "claim",
        "verify",
        "00000000000000000000000000000001",
        "--generation",
        "1",
        "--token",
        secret,
    ];
    let value = call(&rig, &args, 2)?;
    assert!(!serde_json::to_string(&value)?.contains(secret));
    let human = rig.command(&args)?;
    assert_eq!(code(&human), 2);
    assert!(!String::from_utf8_lossy(&human.stderr.bytes).contains(secret));
    assert!(!String::from_utf8_lossy(&human.stdout.bytes).contains(secret));
    assert!(!rig.state.exists());
    Ok(())
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
fn positional_fence_tokens_never_reach_either_error_renderer() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let secret = "0123456789abcdef0123456789abcdef";
    for option in ["--generation", "--unknown"] {
        let args = [
            "fabric",
            "claim",
            "verify",
            "00000000000000000000000000000001",
            secret,
            option,
            "1",
            "--token",
            secret,
        ];
        let value = call(&rig, &args, 2)?;
        assert!(!serde_json::to_string(&value)?.contains(secret));
        let human = rig.command(&args)?;
        assert_eq!(code(&human), 2);
        assert!(!String::from_utf8_lossy(&human.stderr.bytes).contains(secret));
        assert!(!String::from_utf8_lossy(&human.stdout.bytes).contains(secret));
    }
    assert!(!rig.state.exists());
    Ok(())
}

#[test]
fn embedded_and_uppercase_fence_tokens_never_reach_either_error_renderer() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let lower = "0123456789abcdef0123456789abcdef";
    let upper = lower.to_ascii_uppercase();
    for (stray, secret) in [
        (format!("x{lower}y"), lower),
        (format!("{lower}0"), lower),
        (upper.clone(), upper.as_str()),
        (format!("--tokn={lower}"), lower),
    ] {
        let args = [
            "fabric",
            "claim",
            "verify",
            "00000000000000000000000000000001",
            stray.as_str(),
            "--generation",
            "1",
        ];
        let value = call(&rig, &args, 2)?;
        assert!(!serde_json::to_string(&value)?.contains(secret), "{stray}");
        let human = rig.command(&args)?;
        assert_eq!(code(&human), 2);
        assert!(!String::from_utf8_lossy(&human.stderr.bytes).contains(secret));
        assert!(!String::from_utf8_lossy(&human.stdout.bytes).contains(secret));
    }
    assert!(!rig.state.exists());
    Ok(())
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
    let human = rig.command(&["hosts", "--full"])?;
    let text = String::from_utf8(human.stdout.bytes.clone())?;
    assert_eq!(text.lines().count(), rows.len());
    assert!(text.contains("available=true") && !text.contains('{'));
    let removed = rig.command(&["fabric", "host", "list"])?;
    assert_eq!(code(&removed), 2);
    Ok(())
}
