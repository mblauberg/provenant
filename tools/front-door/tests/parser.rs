mod support;
use support::{Result, TestRig, code};
#[test]
fn invalid_json_is_an_envelope_and_unbuilt_owners_never_succeed() -> Result {
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
    let unavailable = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&unavailable), 4);
    let value: serde_json::Value = serde_json::from_slice(&unavailable.stdout.bytes)?;
    assert_eq!(value["error"]["code"], "OWNER_UNAVAILABLE");
    rig.assert_legacy_unchanged()
}

#[test]
fn run_leaves_accept_work_and_refuse_unactivated_providers_per_task() -> Result {
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
    // Replaying the operation returns the same run.
    let again: serde_json::Value =
        serde_json::from_slice(&rig.execute(binary, &args, Some(b"hello"))?.stdout.bytes)?;
    assert_eq!(again["data"]["run_id"], value["data"]["run_id"]);
    let attempt = format!(
        "{}/{}/{}",
        lane["run_id"].as_str().ok_or("run")?,
        lane["task_id"].as_str().ok_or("task")?,
        lane["attempt_id"].as_str().ok_or("attempt")?
    );
    let prompt = rig.command(&["output", &attempt, "--part", "prompt", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&prompt.stdout.bytes)?;
    assert_eq!(value["data"]["text"], "hello");
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
                Some(&"quota" | &"route" | &"worktree" | &"dispatch" | &"lanes" | &"output")
            )
        {
            continue;
        }
        let out = rig.command(&leaf)?;
        let expected = match leaf.first().copied() {
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
        if expected == 4 {
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
