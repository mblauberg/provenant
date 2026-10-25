mod support;
use support::{Result, TestRig, code};
#[test]
fn invalid_json_is_an_envelope() -> Result {
    let rig = TestRig::new()?;
    let invalid = rig.command(&["lanes", "--wait", "0", "--json"])?;
    assert_eq!(code(&invalid), 2);
    let value: serde_json::Value = serde_json::from_slice(&invalid.stdout.bytes)?;
    assert_eq!(value["status"], "invalid");
    assert!(
        !value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Usage:")
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn run_leaves_accept_control_and_report_work_refused_per_task() -> Result {
    let rig = TestRig::new()?;
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_provenant"));
    let state = rig.state.to_string_lossy().into_owned();
    let empty = rig.command(&["lanes", "--json"])?;
    assert_eq!(code(&empty), 0);
    let value: serde_json::Value = serde_json::from_slice(&empty.stdout.bytes)?;
    assert_eq!(value["data"]["lanes"], serde_json::json!([]));
    let args = [
        "--state-dir",
        &state,
        "dispatch",
        "--prompt-stdin",
        "--model",
        "codex/id",
        "--operation-id",
        "fixture",
        "--json",
    ];
    let out = rig.execute(binary, &args, Some(b"hello"))?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let lane = &value["data"]["lanes"][0];
    assert_eq!(lane["state"], "rejected");
    assert_eq!(lane["account"], "unknown");
    let manifest = rig.root.join("accounts.json");
    std::fs::write(&manifest, br#"{"schema_version":1,"tasks":[{"prompt":"fixture","model":"codex/id","account":"fixture-account"}]}"#)?;
    let placed = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest path")?,
        "--operation-id",
        "account-override",
        "--json",
    ])?;
    assert_eq!(code(&placed), 0);
    let placed: serde_json::Value = serde_json::from_slice(&placed.stdout.bytes)?;
    let run = placed["data"]["run_id"].as_str().ok_or("run")?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(
        rig.state.join("runs").join(run).join("request.json"),
    )?)?;
    assert_eq!(saved["tasks"][0]["account"], "fixture-account");
    // --account pins the same way a manifest task does, and an unknown one is
    // rejected at launch with the manifest path's outcome.
    let pinned = rig.execute(
        binary,
        &[
            "--state-dir",
            &state,
            "dispatch",
            "--prompt-stdin",
            "--model",
            "codex/id",
            "--account",
            "fixture-account",
            "--operation-id",
            "account-flag",
            "--json",
        ],
        Some(b"hello"),
    )?;
    assert_eq!(code(&pinned), 0);
    let pinned: serde_json::Value = serde_json::from_slice(&pinned.stdout.bytes)?;
    let run = pinned["data"]["run_id"].as_str().ok_or("run")?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(
        rig.state.join("runs").join(run).join("request.json"),
    )?)?;
    assert_eq!(saved["tasks"][0]["account"], "fixture-account");
    assert_eq!(pinned["data"]["lanes"][0]["state"], "rejected", "{pinned}");
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: serde_json::Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let reason = |run: &str| {
        lanes["data"]["lanes"]
            .as_array()
            .and_then(|all| all.iter().find(|lane| lane["run_id"] == run))
            .and_then(|lane| lane["reason"].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    assert!(
        reason(run).contains("account fixture-account is not registered"),
        "{lanes}"
    );
    // The same pin reaches a writer lane beside the access flags.
    let project = rig.project.to_str().ok_or("project")?;
    let commit = rig.execute(
        std::path::Path::new("/usr/bin/git"),
        &[
            "-C",
            project,
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "fixture",
        ],
        None,
    )?;
    assert_eq!(
        code(&commit),
        0,
        "{}",
        String::from_utf8_lossy(&commit.stderr.bytes)
    );
    let created = rig.command(&["worktree", "create", "fixture", "--json"])?;
    assert_eq!(
        code(&created),
        0,
        "{}",
        String::from_utf8_lossy(&created.stdout.bytes)
    );
    let worktree = rig.project.join(".worktrees/fixture");
    let writer = rig.execute(
        binary,
        &[
            "--state-dir",
            &state,
            "dispatch",
            "--prompt-stdin",
            "--model",
            "codex/id",
            "--mode",
            "worktree-write",
            "--worktree",
            worktree.to_str().ok_or("worktree")?,
            "--account",
            "fixture-account",
            "--operation-id",
            "account-writer",
            "--json",
        ],
        Some(b"hello"),
    )?;
    assert_eq!(
        code(&writer),
        0,
        "{}",
        String::from_utf8_lossy(&writer.stdout.bytes)
    );
    let writer: serde_json::Value = serde_json::from_slice(&writer.stdout.bytes)?;
    let run = writer["data"]["run_id"].as_str().ok_or("run")?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(
        rig.state.join("runs").join(run).join("request.json"),
    )?)?;
    assert_eq!(saved["tasks"][0]["account"], "fixture-account");
    assert!(!saved["tasks"][0]["writer"].is_null(), "{saved}");
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: serde_json::Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let writer_reason = lanes["data"]["lanes"]
        .as_array()
        .and_then(|all| all.iter().find(|lane| lane["run_id"] == run))
        .and_then(|lane| lane["reason"].as_str())
        .unwrap_or_default();
    assert!(
        writer_reason.contains("account fixture-account is not registered"),
        "{lanes}"
    );
    // Replaying the operation returns the same run.
    let again: serde_json::Value =
        serde_json::from_slice(&rig.execute(binary, &args, Some(b"hello"))?.stdout.bytes)?;
    assert_eq!(again["data"]["run_id"], value["data"]["run_id"]);
    // Secret-bearing input is refused before any record and never echoed.
    let secret = format!("use ghp_{}", "a1".repeat(18));
    let mut refused = args;
    refused[7] = "secret";
    let out = rig.execute(binary, &refused, Some(secret.as_bytes()))?;
    let text = String::from_utf8_lossy(&out.stdout.bytes);
    assert!(
        text.contains("SECRET_INPUT") && !text.contains(&secret[4..]),
        "{text}"
    );
    // A worktree is never silently run read-only (#1310); a missing one says so (#1309).
    for (mode, worktree, expected) in [
        ("read-only", ".", "INVALID"),
        ("worktree-write", "missing-worktree", "WORKTREE_MISSING"),
    ] {
        let mut flags = args.to_vec();
        flags.extend(["--mode", mode, "--worktree", worktree]);
        let out = rig.execute(binary, &flags, Some(b"hello"))?;
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(value["error"]["code"], expected, "{value}");
    }
    let attempt = format!(
        "{}/{}/{}",
        lane["run_id"].as_str().ok_or("run")?,
        lane["task_id"].as_str().ok_or("task")?,
        lane["attempt_id"].as_str().ok_or("attempt")?
    );
    let prompt = rig.command(&["output", &attempt, "--part", "prompt", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&prompt.stdout.bytes)?;
    assert_eq!(value["data"]["text"], "hello");

    let cancel = rig.command(&["cancel", &attempt, "--operation-id", "stop", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&cancel.stdout.bytes)?;
    assert_eq!(
        value["data"]["receipts"][0]["receipt"]["state"],
        "already_terminal"
    );
    // Providers expose no sessions yet, so resume refuses before it spends
    // the attempt's one continuation.
    let resume = [
        "--state-dir",
        &state,
        "resume",
        &attempt,
        "--prompt-stdin",
        "--operation-id",
        "again",
        "--json",
    ];
    let out = rig.execute(binary, &resume, Some(b"more"))?;
    assert_eq!(code(&out), 1);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(value["error"]["code"], "UNSUPPORTED");
    let reconcile = rig.command(&["reconcile", "--json"])?;
    assert_eq!(code(&reconcile), 0);
    let value: serde_json::Value = serde_json::from_slice(&reconcile.stdout.bytes)?;
    assert_eq!(value["data"]["runs"], serde_json::json!([]));
    // Prices ship as a reviewed product asset beside the binary.
    std::fs::create_dir_all(rig.project.join("config"))?;
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/telemetry-prices.toml"
        ),
        rig.project.join("config/telemetry-prices.toml"),
    )?;
    let stats = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(
        code(&stats),
        0,
        "{}",
        String::from_utf8_lossy(&stats.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&stats.stdout.bytes)?;
    assert_eq!(value["data"]["totals"]["attempts"], 4);
    rig.assert_legacy_unchanged()
}

#[test]
fn help_and_version_need_no_roots_and_catalogue_help_drives_every_leaf() -> Result {
    let mut rig = TestRig::new()?;
    rig.env.retain(|(key, _)| {
        key != "HOME" && key != "PROVENANT_ROOT" && key != "PROVENANT_STATE_DIR"
    });
    let help = rig.command(&["help", "--json"])?;
    assert_eq!(code(&help), 0);
    let catalog: serde_json::Value = serde_json::from_slice(&help.stdout.bytes)?;
    for command in catalog["data"]["commands"]
        .as_array()
        .ok_or("missing catalogue")?
    {
        let name = command["command"].as_str().ok_or("missing command")?;
        let mut argv: Vec<_> = name.split_whitespace().skip(1).collect();
        argv.push("--help");
        let out = rig.command(&argv)?;
        assert_eq!(
            code(&out),
            0,
            "{name}: {}",
            String::from_utf8_lossy(&out.stdout.bytes)
        );
    }
    let version = rig.command(&["--json", "--version"])?;
    assert_eq!(code(&version), 0);
    let value: serde_json::Value = serde_json::from_slice(&version.stdout.bytes)?;
    assert!(value["data"]["source_digest"].is_string());
    let empty = rig.command(&[])?;
    assert_eq!(code(&empty), 2);
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}
#[test]
fn root_uses_release_location_and_explicit_state_without_creating_it() -> Result {
    use std::fs;
    let mut rig = TestRig::new()?;
    let release = rig.root.join("rélease");
    fs::create_dir_all(release.join("bin"))?;
    fs::copy(
        env!("CARGO_BIN_EXE_provenant"),
        release.join("bin/provenant"),
    )?;
    fs::write(release.join("release.json"), "{}")?;
    rig.env
        .push(("PROVENANT_ROOT".into(), "/intentionally/missing".into()));
    // Environment keys are unique in real processes; replace the fixture value.
    rig.env
        .retain(|(key, value)| key != "PROVENANT_ROOT" || value == "/intentionally/missing");
    let state = rig.root.join("explicit-state");
    let args = [
        std::ffi::OsString::from("--state-dir"),
        state.clone().into_os_string(),
        "root".into(),
        "--json".into(),
    ];
    let out = rig.execute_os(&release.join("bin/provenant"), &args, None)?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let assets: provenant_records::AbsolutePath =
        serde_json::from_value(value["data"]["assets"].clone())?;
    let selected: provenant_records::AbsolutePath =
        serde_json::from_value(value["data"]["state_directory"].clone())?;
    assert_eq!(assets.to_path_buf()?, fs::canonicalize(&release)?);
    assert_eq!(selected.to_path_buf()?, state);
    assert!(!state.exists());
    let human = rig.execute(&release.join("bin/provenant"), &["root"], None)?;
    assert_eq!(code(&human), 0);
    assert_eq!(
        human.stdout.bytes,
        format!("{}\n", release.display()).as_bytes()
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn state_refuses_legacy_aliases_and_checks_select_the_caller_checkout() -> Result {
    use std::{fs, os::unix::fs::symlink};
    let mut rig = TestRig::new()?;
    let alias = rig.root.join("legacy-alias");
    symlink(rig.home.join(".agents"), &alias)?;
    let out = rig.execute_os(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "root".into(),
            "--state-dir".into(),
            alias.into_os_string(),
            "--json".into(),
        ],
        None,
    )?;
    assert_eq!(code(&out), 2);
    let invalid = rig.command(&["check", "harness", "--json"])?;
    assert_eq!(code(&invalid), 0);
    rig.env.push(("GIT_DIR".into(), "/invalid/redirect".into()));
    let check = rig.command(&["check", "harness", "--json"])?;
    assert_eq!(code(&check), 0);
    let value: serde_json::Value = serde_json::from_slice(&check.stdout.bytes)?;
    assert_eq!(value["command"], "check harness");
    let fake = rig.project.join("fake");
    fs::create_dir_all(fake.join("scripts"))?;
    fs::write(fake.join(".git"), "not a Git worktree file")?;
    fs::write(fake.join("HARNESS.md"), "# fixture")?;
    let invalid = rig.execute_os(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--project".into(),
            fake.clone().into_os_string(),
            "check".into(),
            "harness".into(),
            "--json".into(),
        ],
        None,
    )?;
    assert_eq!(code(&invalid), 2);
    let invalid_project = rig.execute_os(
        std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "--project".into(),
            fake.into_os_string(),
            "root".into(),
            "--json".into(),
        ],
        None,
    )?;
    assert_eq!(code(&invalid_project), 2);
    let value: serde_json::Value = serde_json::from_slice(&invalid_project.stdout.bytes)?;
    assert_eq!(value["command"], "root");
    let hook = rig.command(&["hook", "claude", "pre-tool-use", "--json"])?;
    assert_eq!(code(&hook), 2);
    let mcp = rig.command(&["fabric", "mcp", "--json"])?;
    assert_eq!(code(&mcp), 2);
    rig.assert_legacy_unchanged()
}

#[test]
fn ordinary_leaves_validate_roots_and_unbuilt_owners_refuse() -> Result {
    let rig = TestRig::new()?;
    let attempt = format!(
        "{}/{}/{}",
        "11".repeat(16),
        "22".repeat(16),
        "33".repeat(16)
    );
    let leaves: Vec<Vec<&str>> = vec![
        vec!["root"],
        vec!["config", "show"],
        vec!["fabric", "mcp"],
        vec!["install", "--source", "/fixture", "--harness", "claude"],
        vec!["uninstall", "--harness", "claude"],
        vec!["route", "list"],
        vec!["route", "resolve", "--model", "codex/id"],
        vec![
            "dispatch",
            "--prompt-stdin",
            "--model",
            "codex/id",
            "--operation-id",
            "fixture",
        ],
        vec!["lanes"],
        vec!["output", &attempt],
        vec!["cancel", &attempt, "--operation-id", "fixture"],
        vec![
            "resume",
            &attempt,
            "--prompt-stdin",
            "--operation-id",
            "fixture",
        ],
        vec!["reconcile"],
        vec!["telemetry", "stats"],
        vec!["quota", "show"],
        vec!["quota", "tick"],
        vec!["worktree", "create", "fixture"],
        vec!["worktree", "list"],
        vec!["worktree", "remove", "fixture"],
    ];
    for leaf in leaves {
        for (flag, path) in [
            ("--project", rig.root.join("missing")),
            ("--state-dir", rig.home.join(".agents")),
        ] {
            let mut args: Vec<_> = leaf.iter().map(std::ffi::OsString::from).collect();
            args.extend([flag.into(), path.into_os_string()]);
            let out = rig.execute_os(
                std::path::Path::new(env!("CARGO_BIN_EXE_provenant")),
                &args,
                None,
            )?;
            assert_eq!(code(&out), 2, "{leaf:?} {flag}");
        }
        if leaf == ["root"]
            || matches!(
                leaf.first(),
                Some(
                    &"quota"
                        | &"route"
                        | &"worktree"
                        | &"dispatch"
                        | &"lanes"
                        | &"output"
                        | &"cancel"
                        | &"resume"
                        | &"reconcile"
                        | &"telemetry"
                )
            )
        {
            continue;
        }
        let out = rig.command(&leaf)?;
        let expected = match leaf.first().copied() {
            Some("fabric") if leaf.get(1) == Some(&"mcp") => 0,
            Some("config") => 0,
            Some("install") => 1,
            Some("uninstall") => 0,
            Some("check") if leaf.get(1) == Some(&"install") => 1,
            _ => 4,
        };
        assert_eq!(
            code(&out),
            expected,
            "{leaf:?}: {}",
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        if expected == 4 || leaf == ["fabric", "mcp"] {
            assert!(out.stdout.bytes.is_empty());
        }
    }
    let internal = rig.command(&[
        "internal",
        "supervise",
        "2147483647",
        "2147483647",
        "2147483647",
        "2147483647",
        "2147483647",
    ])?;
    assert_eq!(code(&internal), 3); // Reaches supervisor's typed invalid-handshake error.
    rig.assert_legacy_unchanged()
}

#[test]
fn config_show_reads_typed_defaults_and_refuses_invalid_input_without_writes() -> Result {
    let rig = TestRig::new()?;
    let defaults = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&defaults), 0);
    let value: serde_json::Value = serde_json::from_slice(&defaults.stdout.bytes)?;
    assert_eq!(value["data"]["profile"], "balanced");
    assert!(value["data"]["domains"]["quota"].is_object());
    assert!(!rig.state.exists());
    std::fs::create_dir_all(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\nprofile='economy'\n",
    )?;
    let selected = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&selected), 0);
    let value: serde_json::Value = serde_json::from_slice(&selected.stdout.bytes)?;
    assert_eq!(value["data"]["profile"], "economy");
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\nunknown='private-marker'\n",
    )?;
    let invalid = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&invalid), 2);
    assert!(!String::from_utf8_lossy(&invalid.stdout.bytes).contains("private-marker"));
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_uses_non_billing_status_and_preserves_identity_selection() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("status-bin");
    fs::create_dir(&bin)?;
    let claude = bin.join("claude");
    fs::write(&claude, b"#!/bin/sh\n[ \"$1 $2 $3\" = 'auth status --json' ] || exit 90\ncase \"$CLAUDE_CONFIG_DIR\" in\n  */.claude-pro) printf '{\"loggedIn\":false}'; exit 1;;\n  *) printf '{\"loggedIn\":true,\"email\":\"private-marker\"}';;\nesac\n")?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    fs::create_dir(rig.home.join(".claude-default"))?;
    let identity = rig.home.join(".claude-pro");
    fs::create_dir(&identity)?;
    // Only file metadata is needed. Doctor must not decode or return this value.
    fs::write(
        identity.join(".credentials.json"),
        b"private-credential-marker",
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push(("PATH".into(), bin.into_os_string()));
    let out = rig.command(&["hosts", "doctor", "--json"])?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let rows = value["data"]["claude"]
        .as_array()
        .ok_or("missing identities")?;
    assert!(rows.iter().any(|r| r["label"] == "default (~/.claude)"));
    assert!(rows.iter().any(|r| r["label"] == "default"));
    assert!(
        rows.iter()
            .any(|r| r["label"] == "pro" && r["signin"]["state"] == "credential_store_present")
    );
    assert!(!String::from_utf8_lossy(&out.stdout.bytes).contains("private-marker"));
    assert!(!String::from_utf8_lossy(&out.stdout.bytes).contains("private-credential-marker"));
    assert!(!rig.state.exists());
    assert_eq!(
        code(&rig.command(&["hosts", "doctor", "bad;host", "--json"])?),
        2
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_remote_uses_fixed_command_and_rejects_incomplete_responses() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("ssh-bin");
    fs::create_dir(&bin)?;
    let ssh = bin.join("ssh");
    fs::write(&ssh, b"#!/bin/sh\n[ \"$6\" = -- ] && [ \"$7\" = workshop ] && [ \"$8\" = 'exec ~/.local/bin/provenant hosts doctor --json' ] || exit 91\n[ \"$SSH_AUTH_SOCK\" = /fixture/agent.sock ] || exit 92\nprintf '{\"command\":\"hosts doctor\",\"status\":\"ok\",\"data\":{\"claude\":[{\"label\":\"pro\",\"signin\":{\"state\":\"keychain_unavailable\",\"token_validity\":\"unverified\"}}]}}'\n")?;
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700))?;
    rig.env
        .push(("SSH_AUTH_SOCK".into(), "/fixture/agent.sock".into()));
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push(("PATH".into(), bin.into_os_string()));
    let out = rig.command(&["hosts", "doctor", "workshop", "--json"])?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(
        value["data"]["claude"][0]["signin"]["state"],
        "keychain_unavailable"
    );
    fs::write(&ssh, b"#!/bin/sh\nprintf '{\"command\":\"hosts doctor\"'\n")?;
    assert_eq!(
        code(&rig.command(&["hosts", "doctor", "workshop", "--json"])?),
        4
    );
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}
