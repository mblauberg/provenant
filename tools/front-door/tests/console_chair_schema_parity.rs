//! Console and chair operations share their behaviour across CLI and MCP.
mod support;
use serde_json::{Value, json};
use std::{ffi::OsString, path::Path};
use support::{Result, TestRig, code};

fn cli(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let mut args = args.to_vec();
    args.push("--json");
    let output = rig.command(&args)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice::<Value>(&output.stdout.bytes)?["data"].clone())
}

fn mcp(rig: &TestRig, frames: &[Value]) -> Result<Vec<Value>> {
    let mut input = serde_json::to_vec(
        &json!({"jsonrpc":"2.0","id":9999,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
    )?;
    input.extend(b"\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n");
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
    output
        .stdout
        .bytes
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_slice(s).map_err(Into::into))
        .collect()
}

fn frame(id: u64, name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})
}

fn invoke(rig: &TestRig, name: &str, args: Value) -> Result<Value> {
    let replies = mcp(rig, &[frame(1, name, args)])?;
    let result = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("MCP reply")?["result"];
    assert_ne!(result["isError"], true, "{result}");
    Ok(result["structuredContent"].clone())
}

fn copy_tree(from: &Path, to: &Path) -> Result {
    std::fs::create_dir_all(to)?;
    std::fs::set_permissions(to, std::fs::metadata(from)?.permissions())?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[test]
fn console_chair_schema_parity() -> Result {
    let rig = TestRig::new()?;
    let checkpoint = rig.root.join("checkpoint.md");
    std::fs::write(&checkpoint, "# Checkpoint\nOriginal.\n")?;
    let checkpoint = checkpoint.to_str().ok_or("checkpoint path")?;
    cli(
        &rig,
        &["workflow", "session", "save", "chair", "--file", checkpoint],
    )?;
    let patch = json!({"now":"Working", "needs_you":[]});
    let patch_path = rig.root.join("patch.json");
    std::fs::write(&patch_path, serde_json::to_vec(&patch)?)?;
    let patch_path = patch_path.to_str().ok_or("patch path")?;

    // Compare writes from the same pre-operation state. Summary timestamps are
    // produced at commit time and therefore cannot be identical across calls.
    let backup = rig.root.join("backup");
    copy_tree(&rig.state, &backup)?;
    let mut expected = cli(
        &rig,
        &[
            "chair",
            "summary",
            "update",
            "chair",
            "--file",
            patch_path,
            "--expect-revision",
            "0",
            "--operation-id",
            "parity-summary",
        ],
    )?;
    std::fs::remove_dir_all(&rig.state)?;
    copy_tree(&backup, &rig.state)?;
    let mut actual = invoke(
        &rig,
        "fabric_chair",
        json!({"op":"summary_update","session":"chair","patch":patch,"expect_revision":0,"operation_id":"parity-summary"}),
    )?;
    assert!(expected["summary"]["updated_at"].is_string());
    assert!(actual["summary"]["updated_at"].is_string());
    expected["summary"]["updated_at"] = Value::Null;
    actual["summary"]["updated_at"] = Value::Null;
    assert_eq!(actual, expected);
    assert_eq!(actual["replayed"], false);

    // A retry through either surface replays the receipt; a different request
    // under the same id is refused.
    let mut replay = invoke(
        &rig,
        "fabric_chair",
        json!({"op":"summary_update","session":"chair","patch":patch,"expect_revision":0,"operation_id":"parity-summary"}),
    )?;
    assert_eq!(replay["replayed"], true);
    replay["replayed"] = Value::Bool(false);
    replay["summary"]["updated_at"] = Value::Null;
    assert_eq!(replay, actual);
    let conflict = rig.command(&[
        "chair",
        "summary",
        "update",
        "chair",
        "--file",
        patch_path,
        "--expect-revision",
        "1",
        "--operation-id",
        "parity-summary",
        "--json",
    ])?;
    assert_ne!(code(&conflict), 0);

    let stale = rig.command(&[
        "chair",
        "summary",
        "update",
        "chair",
        "--file",
        patch_path,
        "--expect-revision",
        "0",
        "--json",
    ])?;
    assert_ne!(code(&stale), 0);
    let cli_error: Value = serde_json::from_slice(&stale.stdout.bytes)?;
    let replies = mcp(
        &rig,
        &[frame(
            1,
            "fabric_chair",
            json!({
                "op":"summary_update", "session":"chair", "patch":patch, "expect_revision":0
            }),
        )],
    )?;
    let result = &replies
        .iter()
        .find(|reply| reply["id"] == 1)
        .ok_or("MCP reply")?["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["code"],
        cli_error["error"]["code"]
    );
    assert_eq!(
        result["structuredContent"]["current_revision"],
        cli_error["data"]["current_revision"]
    );

    use provenant_core::Clock;
    let issued = provenant_os::clock::SystemClock::new()
        .wall_now()?
        .as_timestamp()
        .as_second();
    let issued_text = issued.to_string();
    let entry = json!({"kind":"progress","markdown":"First update","channel":"chat"});
    let entry_path = rig.root.join("entry.json");
    std::fs::write(&entry_path, serde_json::to_vec(&entry)?)?;
    let entry_path = entry_path.to_str().ok_or("entry path")?;
    let appended = cli(
        &rig,
        &[
            "chair",
            "log",
            "append",
            "--session",
            "chair",
            "--file",
            entry_path,
            "--operation-id",
            "parity-log",
            "--issued-at",
            &issued_text,
        ],
    )?;
    assert_eq!(
        invoke(
            &rig,
            "fabric_chair",
            json!({"op":"log_append","session":"chair","entry":entry,"operation_id":"parity-log","issued_at":issued})
        )?,
        appended
    );

    let generated = invoke(
        &rig,
        "fabric_chair",
        json!({
            "op":"log_append", "session":"chair", "entry":entry
        }),
    )?;
    let retry = invoke(
        &rig,
        "fabric_chair",
        json!({
            "op":"log_append", "session":"chair", "entry":entry,
            "operation_id":generated["operation_id"]
        }),
    )?;
    assert_eq!(retry, generated);

    let cases: &[(&str, Value, &[&str])] = &[
        (
            "fabric_console",
            json!({"op":"projects"}),
            &["console", "projects"],
        ),
        (
            "fabric_console",
            json!({"op":"sessions","session":"chair","limit":1}),
            &["console", "sessions", "chair", "--limit", "1"],
        ),
        (
            "fabric_console",
            json!({"op":"board","all":true,"limit":1}),
            &["console", "board", "--all", "--limit", "1"],
        ),
        (
            "fabric_console",
            json!({"op":"tree","max_depth":2,"max_nodes":1,"detail":true}),
            &[
                "console",
                "tree",
                "--max-depth",
                "2",
                "--max-nodes",
                "1",
                "--detail",
            ],
        ),
        (
            "fabric_console",
            json!({"op":"overview"}),
            &["console", "overview"],
        ),
        (
            "fabric_chair",
            json!({"op":"summary_show","session":"chair"}),
            &["chair", "summary", "show", "chair"],
        ),
        (
            "fabric_chair",
            json!({"op":"summary_diff","session":"chair","from":1,"to":1}),
            &[
                "chair", "summary", "diff", "chair", "--from", "1", "--to", "1",
            ],
        ),
        (
            "fabric_chair",
            json!({"op":"log_list","session":"chair","limit":1}),
            &["chair", "log", "list", "--session", "chair", "--limit", "1"],
        ),
    ];
    for (tool, args, words) in cases {
        let mut actual = invoke(&rig, tool, args.clone())?;
        let mut expected = cli(&rig, words)?;
        if args["op"] == "board" {
            assert!(actual["observed_at"].is_string());
            assert!(expected["observed_at"].is_string());
            actual["observed_at"] = Value::Null;
            expected["observed_at"] = Value::Null;
        }
        assert_eq!(actual, expected, "{tool} {args}");
    }
    let mut frames = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
        frame(2, "fabric_discover", json!({})),
    ];
    for (index, (tool, args, _)) in cases.iter().enumerate() {
        let mut invalid = args.clone();
        invalid["unexpected"] = json!(true);
        frames.push(frame(index as u64 + 3, tool, invalid));
    }
    for args in [
        json!({"op":"summary_update","session":"chair","patch":{},"expect_revision":1,"unexpected":true}),
        json!({"op":"log_append","session":"chair","entry":{},"unexpected":true}),
    ] {
        frames.push(frame(frames.len() as u64 + 1, "fabric_chair", args));
    }
    let replies = mcp(&rig, &frames)?;
    let listed = &replies.iter().find(|r| r["id"] == 1).ok_or("list")?["result"]["tools"];
    let discovered = &replies.iter().find(|r| r["id"] == 2).ok_or("discover")?["result"]["structuredContent"]
        ["tools"];
    for name in ["fabric_console", "fabric_chair"] {
        assert!(
            !listed
                .as_array()
                .ok_or("tools")?
                .iter()
                .any(|t| t["name"] == name)
        );
        assert!(
            discovered
                .as_array()
                .ok_or("discovered tools")?
                .iter()
                .any(|t| t["name"] == name)
        );
    }
    for reply in replies
        .iter()
        .filter(|r| r["id"].as_u64().is_some_and(|id| (3..9999).contains(&id)))
    {
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(
            reply["result"]["structuredContent"]["code"], "invalid_input",
            "{reply}"
        );
    }
    Ok(())
}

#[test]
fn mcp_inbox_owner_routing() -> Result {
    let mut rig = TestRig::new()?;
    for (key, value) in &mut rig.env {
        if key == "HOME" {
            *value = rig.root.clone().into();
        }
    }
    let draft = rig.root.join("request.json");
    std::fs::write(
        &draft,
        serde_json::to_vec(
            &json!({"kind":"question","title":"Question","body":"Please reply","blocking":false,"options":[],"recommended_option_id":null,"proceeding_with":null}),
        )?,
    )?;
    let created = cli(
        &rig,
        &[
            "ask",
            "--file",
            draft.to_str().ok_or("draft path")?,
            "--session",
            "chair",
            "--operation-id",
            "ask-parity",
        ],
    )?;
    let id = created["request_id"].as_str().ok_or("request ID")?;
    for args in [json!({"op":"list","host":"local"}), json!({"op":"list"})] {
        assert_eq!(
            invoke(&rig, "fabric_inbox", args)?,
            cli(&rig, &["inbox", "--host", "local"])?
        );
    }
    assert_eq!(
        invoke(
            &rig,
            "fabric_inbox",
            json!({"op":"list","host":"local","id":id})
        )?,
        cli(&rig, &["inbox", id, "--host", "local"])?
    );
    let reply = rig.root.join("reply.txt");
    std::fs::write(&reply, "Yes")?;
    let answered = cli(
        &rig,
        &[
            "answer",
            id,
            "--host",
            "local",
            "--action",
            "reply",
            "--text-file",
            reply.to_str().ok_or("reply path")?,
            "--expect-revision",
            "1",
            "--channel",
            "cli",
            "--operation-id",
            "answer-parity",
        ],
    )?;
    assert_eq!(
        invoke(
            &rig,
            "fabric_inbox",
            json!({"op":"answer","host":"local","id":id,"action":"reply","text":"Yes","expect_revision":1,"channel":"cli","operation_id":"answer-parity"})
        )?,
        answered
    );
    let answer_id = answered["answer_id"].as_str().ok_or("answer ID")?;

    let cases = [
        (
            json!({"op":"list","host":"missing"}),
            vec!["inbox", "--host", "missing"],
            "owner_unavailable",
        ),
        (
            json!({"op":"close","host":"missing","id":id,"state":"withdrawn","reason":"closed","expect_revision":1,"operation_id":"close-parity"}),
            vec![
                "inbox",
                "close",
                id,
                "--host",
                "missing",
                "--state",
                "withdrawn",
                "--reason",
                "closed",
                "--expect-revision",
                "1",
                "--operation-id",
                "close-parity",
            ],
            "owner_local_only",
        ),
        (
            json!({"op":"acknowledge","host":"missing","id":id,"answer_id":answer_id,"session":"chair","operation_id":"ack-parity"}),
            vec![
                "inbox",
                "acknowledge",
                id,
                "--host",
                "missing",
                "--answer",
                answer_id,
                "--session",
                "chair",
                "--operation-id",
                "ack-parity",
            ],
            "owner_local_only",
        ),
    ];
    for (args, words, expected) in cases {
        let mut words = words;
        words.push("--json");
        let output = rig.command(&words)?;
        assert_ne!(code(&output), 0);
        let cli_error: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(cli_error["error"]["code"], expected, "{cli_error}");
        let replies = mcp(&rig, &[frame(1, "fabric_inbox", args)])?;
        let result = &replies.iter().find(|r| r["id"] == 1).ok_or("MCP reply")?["result"];
        assert_eq!(result["isError"], true, "{result}");
        assert_eq!(result["structuredContent"]["code"], expected, "{result}");
    }
    Ok(())
}
