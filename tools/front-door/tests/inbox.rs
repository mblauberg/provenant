//! Human requests through the app-independent installed command and MCP edges.
mod support;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn call(rig: &TestRig, args: &[&str], stdin: Option<&[u8]>, expected: i64) -> Result<Value> {
    let mut words = vec![
        "--state-dir",
        rig.state.to_str().ok_or("state path")?,
        "--json",
    ];
    words.extend_from_slice(args);
    let output = rig.execute(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &words,
        stdin,
    )?;
    assert_eq!(
        code(&output),
        expected,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}
fn draft(rig: &TestRig, kind: &str) -> Result<String> {
    let path = rig.root.join(format!("{kind}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec(
            &json!({"work":{"run_id":"00000000000000000000000000000001","task_id":"00000000000000000000000000000002"},"kind":kind,"title":"Repair choice","body":"Choose how to repair the check.","blocking":kind=="decision","options":[{"id":"repair","label":"Repair","description":"Repair the failing check"}],"recommended_option_id":"repair","proceeding_with":if kind=="assumption" {json!("Repair the failing check")}else{Value::Null}}),
        )?,
    )?;
    Ok(path.to_str().ok_or("draft path")?.into())
}

#[test]
fn viewer_cli_isolation_and_render_race() -> Result {
    let mut rig = TestRig::new()?;
    let file = draft(&rig, "decision")?;
    let created = call(
        &rig,
        &[
            "ask",
            "--file",
            &file,
            "--session",
            "chair-a",
            "--operation-id",
            "viewer-ask",
        ],
        None,
        0,
    )?;
    let id = created["data"]["request_id"].as_str().ok_or("request ID")?;
    let args = [
        "chair",
        "viewer",
        "set-request",
        "console",
        id,
        "--seen-revision",
        "1",
        "--snoozed-until",
        "2033-05-18T03:33:20.000000Z",
        "--dismissed",
    ];
    call(&rig, &args, None, 0)?;
    let shown = call(
        &rig,
        &["chair", "viewer", "request", "console", id],
        None,
        0,
    )?;
    assert_eq!(shown["data"]["view"]["dismissed"], true);
    assert_eq!(shown["data"]["request"]["state"], "open");
    assert_eq!(shown["data"]["request"]["request"]["blocking"], true);
    assert_eq!(
        call(&rig, &["chair", "viewer", "request", "phone", id], None, 0)?["data"]["view"]["dismissed"],
        false
    );
    rig.env.push((
        "PROVENANT_RUN_STATE_DIR".into(),
        rig.state.clone().into_os_string(),
    ));
    assert_eq!(call(&rig, &args, None, 1)?["error"]["code"], "VIEWER_LANE");
    rig.env.pop();

    let checkpoint = rig.root.join("viewer-checkpoint.md");
    let path = checkpoint.to_str().ok_or("checkpoint path")?;
    let save = |revision: u64, now: &str| -> Result {
        std::fs::write(
            &checkpoint,
            format!(
                "---\nprovenant_summary:\n  schema_version: 1\n  revision: {revision}\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"{now}\"\n  next: \"Next\"\n  needs_you: []\n  risk: \"None\"\n---\nBody\n"
            ),
        )?;
        call(
            &rig,
            &["workflow", "session", "save", "chair-a", "--file", path],
            None,
            0,
        )?;
        Ok(())
    };
    save(1, "First")?;
    let first = call(
        &rig,
        &["chair", "viewer", "summary", "console", "chair-a"],
        None,
        0,
    )?;
    assert_eq!(first["data"]["to"], 1);
    assert_eq!(first["data"]["coverage"], "available");
    assert_eq!(first["data"]["summary"]["now"], "First");
    assert!(first["data"]["baseline"].is_null());
    save(2, "Second")?;
    call(
        &rig,
        &[
            "chair",
            "viewer",
            "rendered",
            "console",
            "chair-a",
            "--revision",
            "1",
        ],
        None,
        0,
    )?;
    let diff = call(
        &rig,
        &["chair", "viewer", "summary", "console", "chair-a"],
        None,
        0,
    )?;
    assert_eq!(diff["data"]["to"], 2);
    assert_eq!(diff["data"]["diff"]["to"], 2);
    assert_eq!(
        diff["data"]["diff"]["now"],
        json!({"from":"First","to":"Second"})
    );
    call(
        &rig,
        &[
            "chair",
            "viewer",
            "rendered",
            "console",
            "chair-a",
            "--revision",
            "2",
        ],
        None,
        0,
    )?;
    assert_eq!(
        call(
            &rig,
            &[
                "chair",
                "viewer",
                "rendered",
                "console",
                "chair-a",
                "--revision",
                "1"
            ],
            None,
            1
        )?["error"]["code"],
        "VIEWER_CONFLICT"
    );
    assert_eq!(
        call(
            &rig,
            &[
                "chair", "viewer", "summary", "console", "chair-a", "--to", "1"
            ],
            None,
            1
        )?["error"]["code"],
        "VIEWER_CONFLICT"
    );
    std::fs::write(&checkpoint, "No summary")?;
    call(
        &rig,
        &["workflow", "session", "save", "chair-a", "--file", path],
        None,
        0,
    )?;
    let missing = call(
        &rig,
        &["chair", "viewer", "summary", "console", "chair-a"],
        None,
        0,
    )?;
    assert_eq!(missing["data"]["coverage"], "unavailable");
    assert_eq!(missing["data"]["diff"]["unavailable"], json!([2]));
    let missing_to = call(
        &rig,
        &[
            "chair", "viewer", "summary", "console", "chair-a", "--to", "5",
        ],
        None,
        0,
    )?;
    assert_eq!(missing_to["data"]["to"], 5);
    assert_eq!(missing_to["data"]["diff"]["to"], 5);
    let projects = rig.state.join("workflow/projects");
    for entry in std::fs::read_dir(projects)? {
        let session = entry?.path().join("sessions/chair-a");
        if session.exists() {
            std::fs::remove_dir_all(session)?;
        }
    }
    let missing = call(
        &rig,
        &["chair", "viewer", "summary", "console", "chair-a"],
        None,
        0,
    )?;
    assert_eq!(missing["data"]["diff"]["unavailable"], json!([2]));
    save(1, "Restarted")?;
    let restarted = call(
        &rig,
        &["chair", "viewer", "summary", "console", "chair-a"],
        None,
        0,
    )?;
    assert_eq!(restarted["data"]["coverage"], "baseline_ahead");
    assert_eq!(restarted["data"]["summary"]["now"], "Restarted");
    assert_eq!(
        call(
            &rig,
            &["chair", "viewer", "summary", "console", "bad id!"],
            None,
            1
        )?["error"]["code"],
        "VIEWER_INVALID"
    );
    let database = rusqlite::Connection::open(rig.state.join("fabric/fabric.sqlite3"))?;
    database.pragma_update(None, "user_version", 12)?;
    let incompatible = call(
        &rig,
        &["chair", "viewer", "summary", "console", "chair-a"],
        None,
        1,
    )?;
    assert_eq!(incompatible["error"]["code"], "VIEWER_INCOMPATIBLE");
    assert!(incompatible["error"]["repair"].is_string());
    database.pragma_update(None, "user_version", 13)?;
    call(
        &rig,
        &["chair", "viewer", "forget", "console", "chair-a"],
        None,
        0,
    )?;
    Ok(())
}

#[test]
fn inbox_cli_posts_lists_and_resolves_all_kinds_without_apps() -> Result {
    let rig = TestRig::new()?;
    for kind in ["decision", "assumption", "question"] {
        let file = draft(&rig, kind)?;
        let operation = format!("ask-{kind}");
        let created = call(
            &rig,
            &[
                "ask",
                "--file",
                &file,
                "--session",
                "chair-a",
                "--operation-id",
                &operation,
            ],
            None,
            0,
        )?;
        let id = created["data"]["request_id"].as_str().ok_or("request ID")?;
        assert_eq!(created["data"]["revision"], 1);
        let repeated = call(
            &rig,
            &[
                "ask",
                "--file",
                &file,
                "--session",
                "chair-a",
                "--operation-id",
                &operation,
            ],
            None,
            0,
        )?;
        assert_eq!(created, repeated);
        let listed = call(&rig, &["inbox", "--session", "chair-a"], None, 0)?;
        assert!(
            listed["data"]["items"]
                .as_array()
                .ok_or("items")?
                .iter()
                .any(|item| item["id"] == id)
        );
        let item = listed["data"]["items"]
            .as_array()
            .ok_or("items")?
            .iter()
            .find(|item| item["id"] == id)
            .ok_or("created request")?;
        assert_eq!(
            item["request"]["work"]["task_id"],
            "00000000000000000000000000000002"
        );
        assert_eq!(item["work_coverage"], "unindexed");
        let action = match kind {
            "assumption" => "confirm",
            "question" => "reply",
            _ => "choose",
        };
        let operation = format!("answer-{kind}");
        let mut args = vec![
            "answer",
            id,
            "--action",
            action,
            "--expect-revision",
            "1",
            "--channel",
            "cli",
            "--operation-id",
            &operation,
        ];
        let stdin = match kind {
            "decision" => {
                args.extend(["--option", "repair"]);
                None
            }
            "question" => {
                args.push("--text-stdin");
                Some(b"  exact answer\n".as_slice())
            }
            _ => None,
        };
        let answered = call(&rig, &args, stdin, 0)?;
        assert_eq!(answered["data"]["revision"], 2);
        assert_eq!(answered["data"]["delivery"]["transport"], "fabric_mail");
        assert_eq!(answered["data"]["delivery"]["state"], "queued");
        assert_eq!(answered, call(&rig, &args, stdin, 0)?);
        let shown = call(&rig, &["inbox", id], None, 0)?;
        assert_eq!(shown["data"]["item"]["answer"]["request_revision"], 1);
        if kind == "question" {
            assert_eq!(shown["data"]["item"]["answer"]["text"], "  exact answer\n");
        }
        let answer_id = answered["data"]["answer_id"].as_str().ok_or("answer ID")?;
        let operation = format!("ack-{kind}");
        let ack = call(
            &rig,
            &[
                "inbox",
                "acknowledge",
                id,
                "--answer",
                answer_id,
                "--session",
                "chair-a",
                "--operation-id",
                &operation,
            ],
            None,
            0,
        )?;
        assert_eq!(ack["data"]["delivery"]["state"], "acknowledged");
    }
    assert!(
        call(&rig, &["inbox"], None, 0)?["data"]["items"]
            .as_array()
            .ok_or("items")?
            .is_empty()
    );
    assert_eq!(
        call(&rig, &["inbox", "--state", "all"], None, 0)?["data"]["items"]
            .as_array()
            .ok_or("items")?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn inbox_cli_refuses_forged_ownership_and_closes_without_approval() -> Result {
    let rig = TestRig::new()?;
    let file = draft(&rig, "decision")?;
    let mut forged: Value = serde_json::from_slice(&std::fs::read(&file)?)?;
    forged["owner_host"] = json!("01".repeat(16));
    std::fs::write(&file, serde_json::to_vec(&forged)?)?;
    let result = call(
        &rig,
        &["ask", "--file", &file, "--session", "chair-a"],
        None,
        2,
    )?;
    assert_eq!(result["error"]["code"], "invalid_input");
    let file = draft(&rig, "decision")?;
    let created = call(
        &rig,
        &["ask", "--file", &file, "--session", "chair-a"],
        None,
        0,
    )?;
    let id = created["data"]["request_id"].as_str().ok_or("request ID")?;
    let args = [
        "inbox",
        "close",
        id,
        "--state",
        "withdrawn",
        "--reason",
        "No longer needed",
        "--expect-revision",
        "1",
        "--operation-id",
        "withdraw",
    ];
    let closed = call(&rig, &args, None, 0)?;
    assert_eq!(closed["data"]["state"], "withdrawn");
    assert!(closed["data"]["answer_id"].is_null());
    assert_eq!(closed, call(&rig, &args, None, 0)?);
    Ok(())
}

#[test]
fn inbox_explicit_local_and_unconfigured_owner() -> Result {
    let mut rig = TestRig::new()?;
    // The initial project is outside HOME and has no federation binding.
    let explicit = call(&rig, &["inbox", "--host", "local"], None, 0)?;
    let implicit = call(&rig, &["inbox"], None, 0)?;
    assert_eq!(explicit["data"], implicit["data"]);

    let moved = rig.home.join("repo");
    std::fs::rename(&rig.project, &moved)?;
    rig.project = moved;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = rig.project.clone().into();
        }
    }
    call(&rig, &["inbox"], None, 0)?;
    let remote = call(&rig, &["inbox", "--host", "remote"], None, 3)?;
    assert_eq!(remote["error"]["code"], "owner_unavailable");
    let request = "01".repeat(16);
    let answer = call(
        &rig,
        &[
            "answer",
            &request,
            "--host",
            "remote",
            "--action",
            "choose",
            "--option",
            "repair",
            "--expect-revision",
            "1",
            "--channel",
            "cli",
            "--operation-id",
            "retry-answer",
        ],
        None,
        3,
    )?;
    assert_eq!(answer["error"]["code"], "owner_unavailable");
    assert!(
        answer["error"]["message"]
            .as_str()
            .ok_or("message")?
            .contains("retry-answer")
    );
    assert!(
        answer["error"]["repair"]
            .as_str()
            .ok_or("repair")?
            .contains("--operation-id retry-answer")
    );
    let generated = call(
        &rig,
        &[
            "answer",
            &request,
            "--host",
            "remote",
            "--action",
            "choose",
            "--option",
            "repair",
            "--expect-revision",
            "1",
            "--channel",
            "cli",
        ],
        None,
        3,
    )?;
    let repair = generated["error"]["repair"].as_str().ok_or("repair")?;
    let operation = repair
        .split_once("--operation-id ")
        .ok_or("operation ID in repair")?
        .1
        .split(';')
        .next()
        .ok_or("operation ID")?;
    assert!(!operation.is_empty());
    assert!(
        generated["error"]["message"]
            .as_str()
            .ok_or("message")?
            .contains(operation)
    );
    let id = format!("{}:{}", "09".repeat(16), "01".repeat(16));
    let mismatch = call(&rig, &["inbox", &id, "--host", "local"], None, 2)?;
    assert_eq!(mismatch["error"]["code"], "invalid_input");
    Ok(())
}

fn mcp_call(rig: &TestRig, name: &str, arguments: Value) -> Result<Value> {
    let frames = [
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    ];
    let mut stdin = Vec::new();
    for frame in frames {
        stdin.extend(serde_json::to_vec(&frame)?);
        stdin.push(b'\n');
    }
    let args = [
        "--state-dir",
        rig.state.to_str().ok_or("state")?,
        "fabric",
        "mcp",
        "--client",
        "codex",
    ];
    let output = rig.execute(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &args,
        Some(&stdin),
    )?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let frames = output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    Ok(frames
        .into_iter()
        .find(|frame| frame["id"] == 1)
        .ok_or("MCP reply")?["result"]
        .clone())
}

#[test]
fn inbox_mcp_remote_home_lists_locally_when_configured() -> Result {
    for with_peer in [false, true] {
        let mut rig = TestRig::new()?;
        let moved = rig.home.join("repo");
        std::fs::rename(&rig.project, &moved)?;
        rig.project = moved;
        for (key, value) in &mut rig.env {
            if key == "PROVENANT_ROOT" {
                *value = rig.project.clone().into();
            }
        }
        let identity = call(&rig, &["fabric", "identity"], None, 0)?;
        let key = identity["data"]["federation"]["key"]
            .as_str()
            .ok_or("project key")?;
        let remote = "09".repeat(16);
        assert_ne!(identity["data"]["host"], remote);
        let file = rig.state.join("config.toml");
        let mut config: toml_edit::DocumentMut = std::fs::read_to_string(&file)?.parse()?;
        config["domains"]["fabric"]["home_hosts"][key] = toml_edit::value(remote.clone());
        if with_peer {
            config["domains"]["fabric"]["peers"]["fixture"]["host"] = toml_edit::value(remote);
            config["domains"]["fabric"]["peers"]["fixture"]["destination"] =
                toml_edit::value("fixture");
        }
        std::fs::write(file, config.to_string())?;
        assert!(!rig.state.join("fabric").exists());
        let reply = mcp_call(&rig, "fabric_inbox", json!({"op":"list"}))?;
        if with_peer {
            assert_eq!(reply["structuredContent"]["result"], "listed", "{reply}");
            assert_eq!(reply["structuredContent"]["items"], json!([]));
            assert!(
                reply["structuredContent"]["coverage"]
                    .as_str()
                    .ok_or("coverage")?
                    .contains("local_only")
            );
        } else {
            assert_eq!(
                reply["structuredContent"]["code"], "home_unavailable",
                "{reply}"
            );
            assert!(!rig.state.join("fabric").exists());
        }
    }
    Ok(())
}

#[test]
fn inbox_close_uses_the_same_configured_body_limit_on_both_edges() -> Result {
    let rig = TestRig::new()?;
    let file = rig.state.join("config.toml");
    let mut config: toml_edit::DocumentMut = std::fs::read_to_string(&file)?.parse()?;
    config["domains"]["fabric"]["limits"]["body_bytes"] = toml_edit::value(64);
    std::fs::write(file, config.to_string())?;
    let file = draft(&rig, "question")?;
    let created = call(
        &rig,
        &["ask", "--file", &file, "--session", "chair-a"],
        None,
        0,
    )?;
    let id = created["data"]["request_id"].as_str().ok_or("request ID")?;
    let reason = "x".repeat(65);
    let cli = call(
        &rig,
        &[
            "inbox",
            "close",
            id,
            "--state",
            "withdrawn",
            "--reason",
            &reason,
            "--expect-revision",
            "1",
        ],
        None,
        2,
    )?;
    assert_eq!(cli["error"]["code"], "invalid_input");
    let mcp = mcp_call(
        &rig,
        "fabric_inbox",
        json!({"op":"close","id":id,"state":"withdrawn","reason":reason,"expect_revision":1}),
    )?;
    assert_eq!(mcp["structuredContent"]["code"], "invalid_input");
    let current = call(&rig, &["inbox", id], None, 0)?;
    assert_eq!(current["data"]["item"]["request"]["revision"], 1);
    assert_eq!(current["data"]["item"]["request"]["state"], "open");
    Ok(())
}

#[test]
fn inbox_mcp_and_cli_share_the_same_authoritative_records() -> Result {
    let rig = TestRig::new()?;
    let file = draft(&rig, "question")?;
    let request: Value = serde_json::from_slice(&std::fs::read(file)?)?;
    let asked = mcp_call(
        &rig,
        "fabric_inbox",
        json!({"op":"ask","session":"chair-a","request":request,"operation_id":"mcp-ask"}),
    )?;
    assert_ne!(asked["isError"], true, "{asked}");
    let id = asked["structuredContent"]["request_id"]
        .as_str()
        .ok_or("request ID")?;
    let cli = call(&rig, &["inbox", id], None, 0)?;
    assert_eq!(
        cli["data"]["item"]["request"]["chair_session_id"],
        "chair-a"
    );
    let reply = mcp_call(
        &rig,
        "fabric_inbox",
        json!({"op":"answer","id":id,"action":"reply","text":"  yes\n","expect_revision":1,"channel":"chat","operation_id":"mcp-answer"}),
    )?;
    assert_ne!(reply["isError"], true, "{reply}");
    let cli = call(&rig, &["inbox", id], None, 0)?;
    assert_eq!(cli["data"]["item"]["answer"]["text"], "  yes\n");
    assert_eq!(cli["data"]["item"]["answer"]["channel"], "chat");
    let conflict = mcp_call(
        &rig,
        "fabric_inbox",
        json!({"op":"answer","id":id,"action":"reply","text":"different","expect_revision":1,"channel":"chat","operation_id":"mcp-answer"}),
    )?;
    assert_eq!(conflict["isError"], true);
    assert_eq!(conflict["structuredContent"]["code"], "operation_conflict");
    assert_eq!(conflict["structuredContent"]["operation_id"], "mcp-answer");
    let stale = call(
        &rig,
        &[
            "answer",
            id,
            "--action",
            "reply",
            "--text-stdin",
            "--expect-revision",
            "1",
            "--channel",
            "cli",
        ],
        Some(b"another answer"),
        1,
    )?;
    assert_eq!(stale["error"]["code"], "conflict");
    assert!(
        stale["error"]["repair"]
            .as_str()
            .ok_or("repair")?
            .contains("inbox --state all")
    );
    Ok(())
}

#[test]
fn inbox_cli_accepts_composed_text_files_and_rejects_oversized_piped_answers() -> Result {
    let rig = TestRig::new()?;
    let file = draft(&rig, "question")?;
    let created = call(
        &rig,
        &["ask", "--file", &file, "--session", "chair-a"],
        None,
        0,
    )?;
    let id = created["data"]["request_id"].as_str().ok_or("request ID")?;
    let text = rig.root.join("answer.md");
    std::fs::write(&text, "  composed without a deadline\n")?;
    let answered = call(
        &rig,
        &[
            "answer",
            id,
            "--action",
            "reply",
            "--text-file",
            text.to_str().ok_or("text path")?,
            "--expect-revision",
            "1",
            "--channel",
            "cli",
        ],
        None,
        0,
    )?;
    assert_eq!(answered["data"]["state"], "answered");
    let shown = call(&rig, &["inbox", id], None, 0)?;
    assert_eq!(
        shown["data"]["item"]["answer"]["text"],
        "  composed without a deadline\n"
    );
    let oversized = vec![b'x'; 4097];
    let result = call(
        &rig,
        &[
            "answer",
            id,
            "--action",
            "reply",
            "--text-stdin",
            "--expect-revision",
            "1",
            "--channel",
            "cli",
        ],
        Some(&oversized),
        2,
    )?;
    assert_eq!(result["error"]["code"], "invalid_input");
    Ok(())
}

/// Trusted preparation seam: Run seals the attempt, then handoff is refused.
struct Indexed(provenant_records::AbsolutePath);
impl provenant_run::Providers for Indexed {
    fn prepare(
        &self,
        _: &provenant_run::TaskRequest,
        _: &str,
        _: &provenant_run::LaunchPaths,
        _: Option<&provenant_run::AttemptRecord>,
    ) -> std::result::Result<provenant_run::PreparedLaunch, String> {
        Ok(provenant_run::PreparedLaunch {
            provider_lease: None,
            argv: vec!["fixture".into()],
            environment: vec![],
            stdin: vec![],
            secret_input: None,
            cwd: "/".into(),
            skipped_routes: vec![],
            confinement: None,
            route: provenant_run::RouteRecord {
                provider: Some("fixture".into()),
                model: Some("fixture/model".into()),
                ..Default::default()
            },
            repository: Some(self.0.clone()),
            observed_head: Some(
                provenant_core::GitOid::parse(&"a".repeat(40)).map_err(|e| e.to_string())?,
            ),
        })
    }
    fn decode(
        &self,
        _: &provenant_run::RouteRecord,
        _: &[u8],
        _: &provenant_supervisor::custody::ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<provenant_run::Decoded, String> {
        unreachable!("no provider runs")
    }
}

#[test]
fn request_correlation_edges_verify_host_seals_and_refuse_mismatches() -> Result {
    use provenant_core::{ActorId, CancelToken, OperationId, ProjectId, TaskId};
    use provenant_run::{
        DispatchRequest, DispatchTask, ExecutionContext, Request, RequestedRoute, RunStore, Service,
    };
    let rig = TestRig::new()?;
    let identity = call(&rig, &["fabric", "identity"], None, 0)?;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let mut roots = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    roots.state_dir = Some(rig.state.clone());
    let context = ExecutionContext {
        state_root: roots.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: project,
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let store = RunStore::open(&context, &request)?;
    let task = TaskId::from_bytes([3; 16]);
    let run = Service::new(&store).dispatch(
        DispatchRequest {
            chair: None,
            operation_id: OperationId::parse("correlation-fixture")?,
            concurrency: 1,
            lane_depth: 1,
            parent_attempt: None,
            tasks: vec![DispatchTask {
                context: None,
                build_heavy: false,
                title: None,
                task_id: task,
                prompt: "fixture".into(),
                base: None,
                pinned_base: None,
                head: None,
                route: RequestedRoute::Named {
                    extra: Default::default(),
                    name: "senior".into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
                profile: Default::default(),
                timeout_seconds: 60,
                delivery_id: None,
                deliver: None,
                worktree: None,
                access: Default::default(),
            }],
        },
        &rig.project,
        &request,
    )?;
    let repository = identity["data"]["common_dir"].clone();
    let outcomes = store.start(
        run.run_id,
        &Indexed(serde_json::from_value(repository.clone())?),
        &|_, _| Err(provenant_run::RunError::Validation("fixture handoff")),
        &request,
    )?;
    assert!(
        matches!(outcomes.as_slice(), [provenant_run::Started::Refused { reason, .. }] if reason.contains("fixture handoff"))
    );
    let draft = json!({"kind":"question","title":"Correlation","body":"Which work?",
        "work":{"run_id":run.run_id,"task_id":task,"repository":repository,"head":"a".repeat(40)}});
    let path = rig.root.join("linked.json");
    let path_str = path.to_str().ok_or("draft")?;
    std::fs::write(&path, serde_json::to_vec(&draft)?)?;
    let cli = call(
        &rig,
        &["ask", "--file", path_str, "--session", "chair-a"],
        None,
        0,
    )?;
    let mcp = mcp_call(
        &rig,
        "fabric_inbox",
        json!({"op":"ask","session":"chair-a","request":draft}),
    )?;
    for receipt in [&cli["data"], &mcp["structuredContent"]] {
        assert_eq!(receipt["work_coverage"], "verified");
        let id = receipt["request_id"].as_str().ok_or("request ID")?;
        let shown = call(&rig, &["inbox", id], None, 0)?;
        assert_eq!(shown["data"]["item"]["request"]["schema_version"], 2);
    }
    for unqualified in [false, true] {
        for field in ["repository", "head"] {
            let mut bad = draft.clone();
            if unqualified {
                bad["work"].as_object_mut().ok_or("work")?.remove("task_id");
            }
            bad["work"][field] = if field == "head" {
                json!("b".repeat(40))
            } else {
                serde_json::to_value(provenant_records::AbsolutePath::new(std::path::Path::new(
                    "/foreign/.git",
                ))?)?
            };
            std::fs::write(&path, serde_json::to_vec(&bad)?)?;
            assert_eq!(
                call(
                    &rig,
                    &["ask", "--file", path_str, "--session", "chair-a"],
                    None,
                    2
                )?["error"]["code"],
                "work_mismatch"
            );
            assert_eq!(
                mcp_call(
                    &rig,
                    "fabric_inbox",
                    json!({"op":"ask","session":"chair-a","request":bad})
                )?["structuredContent"]["code"],
                "work_mismatch"
            );
        }
    }
    let db = rusqlite::Connection::open(rig.state.join("fabric/fabric.sqlite3"))?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM human_requests", [], |r| r
            .get::<_, i64>(0))?,
        2
    );
    let index = rusqlite::Connection::open(rig.state.join(provenant_run::WORK_INDEX_FILE))?;
    index.pragma_update(None, "user_version", 2)?;
    std::fs::write(&path, serde_json::to_vec(&draft)?)?;
    let failed = call(
        &rig,
        &["ask", "--file", path_str, "--session", "chair-a"],
        None,
        3,
    )?;
    assert_eq!(failed["error"]["code"], "work_lookup_failed");
    assert!(
        failed["error"]["message"]
            .as_str()
            .ok_or("message")?
            .contains("version")
    );
    Ok(())
}
