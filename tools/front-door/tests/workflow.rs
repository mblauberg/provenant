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
fn session_save_discovery_restart() -> Result {
    let rig = TestRig::new()?;
    let identity = call(&rig, &["fabric", "identity"], 0)?;
    let identity = &identity["data"];
    let file = rig.root.join("checkpoint.md");
    std::fs::write(&file, "# Goal\nExact chair identity.\n")?;
    call(
        &rig,
        &[
            "workflow",
            "session",
            "save",
            "chair-1",
            "--file",
            &file.to_string_lossy(),
        ],
        0,
    )?;
    let project = identity["project"].as_str().ok_or("project identity")?;
    let manifest_path = rig.state.join(format!(
        "workflow/projects/{project}/sessions/chair-1/revisions.json"
    ));
    let manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    let discovery = &manifest["discovery"];
    assert_eq!(discovery["schema_version"], 1);
    assert_eq!(discovery["host"], identity["host"]);
    assert_eq!(discovery["project_id"], identity["project"]);
    assert_eq!(discovery["chair_actor"], identity["inbox"]["actor"]);
    assert_eq!(discovery["host_session_id"], "chair-1");
    assert_eq!(discovery["checkpoint"], manifest["revisions"][0]);
    // Each CLI call is a fresh process. Reads leave every state file unchanged.
    let before = state_snapshot(&rig.state)?;
    let shown = call(&rig, &["workflow", "session", "show", "chair-1"], 0)?;
    assert_eq!(shown["data"]["markdown"], "# Goal\nExact chair identity.\n");
    call(
        &rig,
        &["workflow", "session", "show", "chair-1", "--revisions"],
        0,
    )?;
    call(
        &rig,
        &["workflow", "session", "show", "chair-1", "--rev", "1"],
        0,
    )?;
    call(&rig, &["workflow", "session", "diff", "chair-1", "1"], 0)?;
    assert_eq!(state_snapshot(&rig.state)?, before);
    // A legacy manifest has no registration; showing it must not register it.
    let mut legacy = manifest;
    legacy
        .as_object_mut()
        .ok_or("manifest")?
        .remove("discovery");
    std::fs::write(manifest_path, serde_json::to_vec(&legacy)?)?;
    let before = state_snapshot(&rig.state)?;
    call(&rig, &["workflow", "session", "show", "chair-1"], 0)?;
    assert_eq!(state_snapshot(&rig.state)?, before);
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
