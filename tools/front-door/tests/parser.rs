mod support;
use support::{Result, TestRig, code};

#[test]
fn terminal_status_sees_graphs_submitted_under_harness_markers() -> Result {
    let mut rig = TestRig::new()?;
    rig.env.retain(|(key, _)| key != "PROVENANT_AGENT_LABEL");
    for (marker, value, graph) in [
        ("CLAUDECODE", "1", "claude-marker"),
        ("CODEX_THREAD_ID", "chair-fixture", "codex-marker"),
    ] {
        rig.env.push((marker.into(), value.into()));
        let document = rig.root.join("graph.json");
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1,"graph_id":graph,"nodes":[{
                    "id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}
                }]
            }))?,
        )?;
        let out = rig.command(&[
            "run",
            "--file",
            document.to_str().ok_or("document path")?,
            "--json",
        ])?;
        assert_eq!(code(&out), 0, "{out:?}");
        let submitted: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(submitted["data"]["checkpoint"]["graph_id"], graph);
        rig.env.retain(|(key, _)| key != marker);
        let out = rig.command(&["status", graph, "--json"])?;
        assert_eq!(code(&out), 0, "{out:?}");
        let status: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        let rows = status["data"]["rows"].as_array().ok_or("status rows")?;
        assert!(
            rows.iter()
                .any(|row| row["kind"] == "graph" && row["graph_id"] == graph)
        );
        assert!(
            rows.iter()
                .any(|row| row["kind"] == "node" && row["node_id"] == "accept")
        );
        rig.env.push((marker.into(), value.into()));
        let out = rig.command(&["fabric", "identity", "--json"])?;
        assert_eq!(code(&out), 0);
        let identity: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(identity["data"]["client"], "agent");
        assert_eq!(identity["data"]["inbox"]["agent"], "agent");
        rig.env.retain(|(key, _)| key != marker);
    }
    Ok(())
}

#[test]
fn worktree_creation_refuses_colliding_branch_names() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "initial"], None)?;
    assert_eq!(code(&commit), 0);
    let branch = rig.execute(&git, &["branch", "Existing"], None)?;
    assert_eq!(code(&branch), 0);

    let explicit = rig.command(&["worktree", "create", "existing", "--base", "HEAD"])?;
    assert_ne!(code(&explicit), 0);
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&explicit.stdout.bytes),
        String::from_utf8_lossy(&explicit.stderr.bytes)
    );
    assert!(message.contains("collides"), "{message}");

    assert!(!rig.project.join(".worktrees/existing").exists());
    Ok(())
}

#[test]
fn invalid_json_is_an_envelope() -> Result {
    let rig = TestRig::new()?;
    let invalid = rig.command(&["land", "2199", "--wait=0", "--json"])?;
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
fn help_and_version_need_no_roots_and_catalogue_help_drives_every_leaf() -> Result {
    let mut rig = TestRig::new()?.without_state()?;
    rig.env.retain(|(key, _)| {
        key != "HOME" && key != "PROVENANT_ROOT" && key != "PROVENANT_STATE_DIR"
    });
    for args in [&["help", "--json"][..], &["help", "--all", "--json"][..]] {
        let help = rig.command(args)?;
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
    let executable = release.join("bin/provenant");
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
    let out = rig.with_process_spawn_lock(|| {
        fs::copy(env!("CARGO_BIN_EXE_provenant"), &executable)?;
        rig.execute_os_locked(&executable, &args, None)
    })?;
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
    let compact = rig.execute(&executable, &["root"], None)?;
    assert_eq!(code(&compact), 0);
    let text = String::from_utf8_lossy(&compact.stdout.bytes);
    assert!(
        !text.contains("next:"),
        "no owner next, no invented one: {text}"
    );
    assert!(compact.stdout.bytes.len() <= 2048);
    let human = rig.execute(&executable, &["root", "--full"], None)?;
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
    let rig = TestRig::new()?;
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
    let fake = rig.project.join("fake");
    fs::create_dir_all(fake.join("scripts"))?;
    fs::write(fake.join(".git"), "not a Git worktree file")?;
    fs::write(fake.join("GLOBAL-AGENTS.md"), "# fixture")?;
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
    let leaves: Vec<Vec<&str>> = vec![
        vec!["root"],
        vec!["config", "show"],
        vec!["fabric", "mcp"],
        vec!["install", "--source", "/fixture", "--harness", "claude"],
        vec!["uninstall", "--harness", "claude"],
        vec!["route", "list"],
        vec!["route", "resolve", "--model", "codex/id"],
        vec!["status"],
        vec!["wait", "fix-login"],
        vec!["output", "fix-login/review#1"],
        vec!["stop", "fix-login"],
        vec!["retry", "fix-login/review#1"],
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
                        | &"status"
                        | &"wait"
                        | &"output"
                        | &"stop"
                        | &"retry"
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
fn config_show_reads_typed_defaults_warns_on_unknown_keys_and_restricts_authority_input_without_writes()
-> Result {
    let rig = TestRig::new()?.without_state()?;
    let defaults = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&defaults), 0);
    let value: serde_json::Value = serde_json::from_slice(&defaults.stdout.bytes)?;
    assert_eq!(value["data"]["profile"], "balanced");
    assert!(value["data"]["domains"]["quota"].is_object());
    let human = rig.command(&["config", "show", "--full"])?;
    assert_eq!(code(&human), 0);
    let text = String::from_utf8_lossy(&human.stdout.bytes);
    for field in ["profile=balanced", "schema_version=1"] {
        assert!(
            text.split_whitespace().any(|value| value == field),
            "{text}"
        );
    }
    let removed = rig.command(&["config", "show", "--task-class", "mechanical", "--json"])?;
    assert_eq!(code(&removed), 2);
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
    // An unknown key warns by name without echoing its value.
    let unknown = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&unknown), 0);
    let stderr = String::from_utf8_lossy(&unknown.stderr.bytes);
    assert!(
        stderr.contains("unknown: ignored invalid configuration (unknown_key)"),
        "{stderr}"
    );
    for stream in [&unknown.stdout.bytes, &unknown.stderr.bytes] {
        assert!(!String::from_utf8_lossy(stream).contains("private-marker"));
    }
    // A malformed authority knob warns and takes its restrictive value, so one
    // typo never stops the command or widens authority.
    let malformed = "schema_version=1\n[knobs]\n'lanes.publish'='private-marker'\n";
    std::fs::write(rig.state.join("config.toml"), malformed)?;
    let invalid = rig.command(&["config", "show", "--json"])?;
    assert_eq!(
        code(&invalid),
        0,
        "{}",
        String::from_utf8_lossy(&invalid.stdout.bytes)
    );
    let shown: serde_json::Value = serde_json::from_slice(&invalid.stdout.bytes)?;
    assert_eq!(shown["data"]["knobs"]["lanes.publish"]["value"], false);
    let stderr = String::from_utf8_lossy(&invalid.stderr.bytes);
    assert!(stderr.contains("lanes.publish"), "{stderr}");
    for stream in [&invalid.stdout.bytes, &invalid.stderr.bytes] {
        assert!(!String::from_utf8_lossy(stream).contains("private-marker"));
    }
    assert_eq!(
        std::fs::read_to_string(rig.state.join("config.toml"))?,
        malformed
    );
    // A misspelt knob name warns once, naming the nearest knob, and neither
    // `config show` nor `status` refuses.
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n\n[knobs]\n\"lanes.max_concurency\" = 4\n",
    )?;
    // Fabric requires a private state directory; that is unrelated to the typo.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&rig.state, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut stderr = String::new();
    for args in [&["config", "show", "--json"][..], &["status", "--json"][..]] {
        let output = rig.command(args)?;
        assert_eq!(
            code(&output),
            0,
            "{args:?} {}",
            String::from_utf8_lossy(&output.stdout.bytes)
        );
        stderr.push_str(&String::from_utf8_lossy(&output.stderr.bytes));
    }
    assert_eq!(
        stderr.matches("lanes.max_concurency").count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains("lanes.max_concurrency"), "{stderr}");
    assert!(!stderr.contains("PathWire"), "{stderr}");
    rig.assert_legacy_unchanged()
}

#[test]
fn config_commands_report_graph_checkpoint_retention_and_source() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let show = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&show), 0);
    let show: serde_json::Value = serde_json::from_slice(&show.stdout.bytes)?;
    assert_eq!(show["data"]["gc"]["graph_checkpoint_days"], 90);
    assert_eq!(
        show["data"]["origins"]["gc.graph_checkpoint_days"]["kind"],
        "compiled"
    );
    let schema = rig.command(&["config", "schema", "--json"])?;
    assert_eq!(code(&schema), 0);
    let schema: serde_json::Value = serde_json::from_slice(&schema.stdout.bytes)?;
    let compact = schema["data"]["fields"]
        .as_array()
        .ok_or("compact fields")?;
    assert!(compact.len() <= 40);
    assert!(
        !compact
            .iter()
            .any(|field| field["path"] == "config.history_max_bytes")
    );
    let all = rig.command(&["config", "schema", "--all", "--json"])?;
    assert_eq!(code(&all), 0);
    let all: serde_json::Value = serde_json::from_slice(&all.stdout.bytes)?;
    let advanced = all["data"]["fields"].as_array().ok_or("advanced fields")?;
    assert!(advanced.len() > compact.len());
    let registry: serde_json::Value =
        serde_json::from_str(include_str!("../../../config/harness-knobs.json"))?;
    let documented_providers = registry["provider_updates"]["provider_ids"]
        .as_array()
        .ok_or("documented providers")?;
    for adapter in provenant_records::ADAPTER_SPECS {
        let documented = documented_providers
            .iter()
            .any(|provider| provider.as_str() == Some(adapter.name));
        for setting in ["update", "runtime_deadline_seconds"] {
            let path = format!("providers.{}.{setting}", adapter.name);
            assert_eq!(
                compact.iter().any(|field| field["path"] == path),
                documented,
                "compact provider setting {path}"
            );
            assert!(
                advanced.iter().any(|field| field["path"] == path),
                "advanced provider setting {path}"
            );
        }
    }
    assert!(
        advanced
            .iter()
            .any(|field| field["path"] == "config.history_max_bytes")
    );
    let field = schema["data"]["fields"]
        .as_array()
        .ok_or("schema fields")?
        .iter()
        .find(|field| field["path"] == "gc.graph_checkpoint_days")
        .ok_or("retention field")?;
    assert_eq!(field["default"], 90);
    assert_eq!(field["value"], 90);
    assert_eq!(field["source"], "default");
    std::fs::create_dir_all(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\ngraph_checkpoint_days=30\n",
    )?;
    let show = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&show), 0);
    let show: serde_json::Value = serde_json::from_slice(&show.stdout.bytes)?;
    assert_eq!(show["data"]["gc"]["graph_checkpoint_days"], 30);
    assert_eq!(
        show["data"]["origins"]["gc.graph_checkpoint_days"]["kind"],
        "user"
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn config_check_and_doctor_report_pool_effort_warnings_as_advice() -> Result {
    let rig = TestRig::new()?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.routing.pools]\ntrial=[{model='codex/gpt-6-luna',effort='retired',efforts=['low','retired']}]\n",
    )?;

    let checked = rig.command(&["check", "config", "--json"])?;
    assert_eq!(
        code(&checked),
        0,
        "{}",
        String::from_utf8_lossy(&checked.stdout.bytes)
    );
    let checked: serde_json::Value = serde_json::from_slice(&checked.stdout.bytes)?;
    assert_eq!(checked["status"], "ok");
    assert_eq!(checked["data"]["count"], 0);
    let warning = checked["data"]["warnings"][0].as_str().ok_or("warning")?;
    assert!(warning.contains("pool trial"), "{warning}");
    assert!(warning.contains("effort retired"), "{warning}");
    assert!(warning.contains("codex/gpt-6-luna"), "{warning}");
    assert!(warning.contains("pool source: config.toml"), "{warning}");
    assert!(warning.contains("model source: config.toml"), "{warning}");
    assert!(warning.contains("effort source: config.toml"), "{warning}");
    assert!(warning.contains("efforts source: config.toml"), "{warning}");

    let doctor = rig.command(&["hosts", "doctor", "--json"])?;
    assert_eq!(
        code(&doctor),
        0,
        "{}",
        String::from_utf8_lossy(&doctor.stdout.bytes)
    );
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout.bytes)?;
    assert_eq!(doctor["status"], "ok");
    assert_eq!(doctor["data"]["warnings"][0], warning);
    rig.assert_legacy_unchanged()
}

#[test]
fn hosts_doctor_reports_unobserved_installed_adapters_without_running_them() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("status-bin");
    fs::create_dir(&bin)?;
    for name in ["agy", "opencode"] {
        let executable = bin.join(name);
        fs::write(
            &executable,
            b"#!/bin/sh\n: > \"$HOME/provider-ran\"\nexit 1\n",
        )?;
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700))?;
    }
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
    let rows = value["data"]["unobserved_adapters"]
        .as_array()
        .ok_or("missing adapters")?;
    for name in ["agy", "opencode"] {
        assert!(rows.iter().any(|row| row == name), "{value}");
    }
    assert!(!rig.home.join("provider-ran").exists());
    Ok(())
}

#[test]
fn hosts_doctor_uses_non_billing_status_and_preserves_identity_selection() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = TestRig::new()?.without_state()?;
    let bin = rig.root.join("status-bin");
    fs::create_dir(&bin)?;
    let claude = bin.join("claude");
    fs::write(&claude, b"#!/bin/sh\npwd -P >> \"$HOME/doctor-cwd\"\n[ \"$1 $2 $3\" = 'auth status --json' ] || exit 90\ncase \"$CLAUDE_CONFIG_DIR\" in\n  */.claude-pro) printf '{\"loggedIn\":false}'; exit 1;;\n  *) printf '{\"loggedIn\":true,\"email\":\"private-marker\"}';;\nesac\n")?;
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
    assert_eq!(
        fs::read_to_string(rig.home.join("doctor-cwd"))?,
        "/\n/\n/\n"
    );
    assert!(!rig.state.exists());
    assert_eq!(
        code(&rig.command(&["hosts", "doctor", "bad;host", "--json"])?),
        2
    );
    rig.assert_legacy_unchanged()
}
