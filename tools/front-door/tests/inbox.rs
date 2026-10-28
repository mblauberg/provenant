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
            &json!({"kind":kind,"title":"Repair choice","body":"Choose how to repair the check.","blocking":kind=="decision","options":[{"id":"repair","label":"Repair","description":"Repair the failing check"}],"recommended_option_id":"repair","proceeding_with":if kind=="assumption" {json!("Repair the failing check")}else{Value::Null}}),
        )?,
    )?;
    Ok(path.to_str().ok_or("draft path")?.into())
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
    let remote = call(&rig, &["inbox", "--host", "remote"], None, 1)?;
    assert_eq!(remote["error"]["code"], "unsupported_owner");
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
            .contains("--operation-id")
    );
    Ok(())
}
