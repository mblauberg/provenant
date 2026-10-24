mod support;
use provenant_config::{Release, ReleaseSmoke};
use provenant_core::CancelToken;
use provenant_records::{CommandResult, Outcome, SourceInput, VersionInfo, source_digest};
use provenant_supervisor::fs::LocalFs;
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use support::{Result, TestRig, code};

struct FixtureSmoke(VersionInfo);
impl ReleaseSmoke for FixtureSmoke {
    fn check(&self, _: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        Ok(self.0.clone())
    }
}
fn staged(rig: &TestRig) -> Result<PathBuf> {
    let stage = rig.root.join("staged");
    fs::create_dir_all(stage.join("bin"))?;
    fs::create_dir_all(stage.join("skills/example"))?;
    let skill = b"---\nname: example\ndescription: fixture\n---\n";
    let mut asset_bytes = vec![0];
    asset_bytes.extend(skill);
    let version = VersionInfo {
        source_digest: source_digest(&[SourceInput {
            name: "fixture.rs",
            bytes: b"synthetic release",
        }])?,
        asset_digest: source_digest(&[SourceInput {
            name: "skills/example/SKILL.md",
            bytes: &asset_bytes,
        }])?,
        target: env!("PROVENANT_TARGET_TRIPLE").into(),
        toolchain: "fixture-toolchain".into(),
        record_schema_version: 1,
        config_schema_version: 1,
    };
    let mut data = serde_json::to_value(&version)?;
    data.as_object_mut()
        .ok_or("version object")?
        .insert("package_version".into(), env!("CARGO_PKG_VERSION").into());
    let version_json = serde_json::to_string(&CommandResult::new("version", Outcome::Ok, data))?;
    let help_json = serde_json::to_string(&CommandResult::new(
        "help",
        Outcome::Ok,
        serde_json::json!({"commands":[]}),
    ))?;
    // A fake release executable, never a provider or a real harness. Its two
    // stdout envelopes exercise the actual bounded production smoke path.
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n--version) printf '%s\\n' '{version_json}' ;;\n--help|help) printf '%s\\n' '{help_json}' ;;\n*) exit 2 ;;\nesac\n"
    );
    let binary = stage.join("bin/provenant");
    fs::write(&binary, script)?;
    fs::set_permissions(binary, fs::Permissions::from_mode(0o755))?;
    let path = stage.join("skills/example/SKILL.md");
    fs::write(&path, skill)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    provenant_config::package_release(
        &stage,
        &version,
        &LocalFs,
        &FixtureSmoke(version.clone()),
        &CancelToken::new(),
    )?;
    Ok(stage)
}
fn args(stage: &Path, dry: bool) -> Vec<std::ffi::OsString> {
    let mut args = vec![
        "install".into(),
        "--source".into(),
        stage.into(),
        "--harness".into(),
        "codex".into(),
        "--json".into(),
    ];
    if dry {
        args.push("--dry-run".into());
    }
    args
}
#[test]
fn check_install_source_failures_keep_the_typed_outcome() -> Result {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let missing = rig.root.join("missing-release");
    fs::write(stage.join("release.json"), "{invalid")?;
    for (source, expected_status, expected_code) in [
        (&missing, "refused", "INSTALL_REFUSED"),
        (&stage, "invalid", "INSTALL_INVALID"),
    ] {
        let output = rig.execute_os(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "check".into(),
                "install".into(),
                "--source".into(),
                source.into(),
                "--json".into(),
            ],
            None,
        )?;
        let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(value["status"], expected_status);
        assert_eq!(value["error"]["code"], expected_code);
        assert_ne!(code(&output), 0);
    }
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn install_resolves_source_through_symlinked_temp_ancestor() -> Result {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let alias = rig.root.join("source-alias");
    std::os::unix::fs::symlink(&rig.root, &alias)?;
    let aliased_stage = alias.join(stage.file_name().ok_or("stage name")?);
    let binary = Path::new(env!("CARGO_BIN_EXE_provenant"));
    let result = rig.execute_os(binary, &args(&aliased_stage, true), None)?;
    assert_eq!(
        code(&result),
        0,
        "{}",
        String::from_utf8_lossy(&result.stderr.bytes)
    );
    Ok(())
}

#[test]
fn real_install_check_uninstall_use_only_the_isolated_home() -> Result {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let binary = Path::new(env!("CARGO_BIN_EXE_provenant"));
    let dry = rig.execute_os(binary, &args(&stage, true), None)?;
    assert_eq!(
        code(&dry),
        0,
        "{}",
        String::from_utf8_lossy(&dry.stderr.bytes)
    );
    assert!(!rig.state.exists());
    assert!(dry.stderr.bytes.is_empty());
    let preview: serde_json::Value = serde_json::from_slice(&dry.stdout.bytes)?;
    let action_count = preview["data"]["outcome"]["actions"]
        .as_array()
        .ok_or("JSON action list")?
        .len();
    assert!(action_count > 0);
    let mut human_args = args(&stage, true);
    human_args.retain(|arg| arg != "--json");
    let human = rig.execute_os(binary, &human_args, None)?;
    assert_eq!(code(&human), 0);
    let human_stdout = String::from_utf8_lossy(&human.stdout.bytes);
    assert!(human_stdout.contains("links"));
    assert!(human_stdout.contains("hooks"));
    assert!(human_stdout.contains("MCP"));
    assert!(human_stdout.contains("findings"));
    assert!(!human_stdout.trim_start().starts_with('{'));
    assert!(!String::from_utf8_lossy(&human.stderr.bytes).contains("\"kind\":\"actions\""));
    fs::create_dir_all(&rig.state)?;
    fs::write(
        rig.state.join("config.toml"),
        // Sections other commands write must not block installation.
        "schema_version=1\n[domains.fabric]\nclient_label=\"install-fixture\"\n[domains.accounts]\n",
    )?;
    let installed = rig.execute_os(binary, &args(&stage, false), None)?;
    assert_eq!(
        code(&installed),
        0,
        "{}",
        String::from_utf8_lossy(&installed.stderr.bytes)
    );
    let checked = rig.command(&["check", "install", "--json"])?;
    assert_eq!(
        code(&checked),
        0,
        "{}",
        String::from_utf8_lossy(&checked.stdout.bytes)
    );
    let reapplied = rig.execute_os(binary, &args(&stage, false), None)?;
    assert_eq!(code(&reapplied), 0);
    let result: serde_json::Value = serde_json::from_slice(&reapplied.stdout.bytes)?;
    assert_eq!(result["data"]["outcome"]["actions"], serde_json::json!([]));
    let mut human_reapply_args = args(&stage, false);
    human_reapply_args.retain(|arg| arg != "--json");
    let human_reapply = rig.execute_os(binary, &human_reapply_args, None)?;
    assert_eq!(code(&human_reapply), 0);
    let human_reapply = String::from_utf8_lossy(&human_reapply.stdout.bytes);
    assert!(human_reapply.contains("0 changed hooks"));
    assert!(human_reapply.contains("0 changed MCP"));
    let config = rig.root.join("codex/config.toml");
    let text = fs::read_to_string(&config)?;
    assert!(text.contains("--state-dir"));
    let removed = rig.command(&["uninstall", "--harness", "codex", "--json"])?;
    assert_eq!(
        code(&removed),
        0,
        "{}",
        String::from_utf8_lossy(&removed.stderr.bytes)
    );
    assert!(!rig.root.join("codex/skills/example").exists());
    assert!(rig.home.join(".local/bin/provenant").is_symlink());
    let config: serde_json::Value = toml_edit::de::from_str(&fs::read_to_string(config)?)?;
    assert!(
        config
            .get("mcp_servers")
            .and_then(|v| v.get("fabric"))
            .is_none()
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn legacy_git_hook_location_comes_from_the_explicit_checkout() -> Result {
    assert_legacy_git_hook_location(false)?;
    assert_legacy_git_hook_location(true)
}

fn assert_legacy_git_hook_location(custom_hooks: bool) -> Result {
    use std::os::unix::fs::symlink;
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let legacy = rig.root.join("legacy-checkout");
    fs::create_dir(&legacy)?;
    let git = Path::new("/usr/bin/git");
    let init = rig.execute_os(
        git,
        &[
            "-C".into(),
            legacy.clone().into_os_string(),
            "init".into(),
            "-q".into(),
        ],
        None,
    )?;
    assert_eq!(code(&init), 0);
    let hooks = if custom_hooks {
        let hooks = legacy.join("custom-hooks");
        fs::create_dir(&hooks)?;
        let configured = rig.execute_os(
            git,
            &[
                "-C".into(),
                legacy.clone().into_os_string(),
                "config".into(),
                "core.hooksPath".into(),
                hooks.clone().into_os_string(),
            ],
            None,
        )?;
        assert_eq!(code(&configured), 0);
        hooks
    } else {
        legacy.join(".git/hooks")
    };
    let source_alias = rig.root.join("legacy-source-alias");
    symlink(&legacy, &source_alias)?;
    let sensitive = legacy.join("sensitive-release");
    fs::create_dir(&sensitive)?;
    fs::write(sensitive.join("release.json"), "private manifest sentinel")?;
    let source = source_alias.join("sensitive-release");
    let mut denied_args = args(&source, true);
    denied_args.extend(["--legacy-checkout".into(), legacy.clone().into_os_string()]);
    let denied = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &denied_args,
        None,
    )?;
    assert_eq!(code(&denied), 1);
    let denied: serde_json::Value = serde_json::from_slice(&denied.stdout.bytes)?;
    assert_eq!(
        denied["error"]["message"],
        format!(
            "unmanaged destination conflicts: {}",
            source.canonicalize()?.display()
        )
    );
    assert_eq!(
        fs::read_to_string(sensitive.join("release.json"))?,
        "private manifest sentinel"
    );
    assert!(!rig.state.exists());
    let old = hooks.join("post-merge");
    symlink(legacy.join("scripts/hooks/post-merge"), &old)?;
    let retained = hooks.join("pre-push");
    symlink(legacy.join("scripts/hooks/pre-push"), &retained)?;
    let project_hook = rig.project.join(".git/hooks/post-merge");
    symlink(legacy.join("scripts/hooks/post-merge"), &project_hook)?;
    let mut args = args(&stage, false);
    args.extend([
        "--legacy-checkout".into(),
        source_alias.into_os_string(),
        "--project".into(),
        rig.project.clone().into_os_string(),
    ]);
    let installed = rig.execute_os(Path::new(env!("CARGO_BIN_EXE_provenant")), &args, None)?;
    assert_eq!(
        code(&installed),
        0,
        "{}",
        String::from_utf8_lossy(&installed.stderr.bytes)
    );
    assert!(fs::symlink_metadata(&old).is_err());
    assert_eq!(
        fs::read_link(&retained)?,
        legacy.join("scripts/hooks/pre-push")
    );
    assert_eq!(
        fs::read_link(&project_hook)?,
        legacy.join("scripts/hooks/post-merge")
    );
    let installed_data: serde_json::Value = serde_json::from_slice(&installed.stdout.bytes)?;
    assert!(
        installed_data["data"]["outcome"]["findings"]
            .as_array()
            .ok_or("findings")?
            .iter()
            .any(|v| v["rule"] == "legacy_entry")
    );
    let checked = rig.command(&["check", "install", "--json"])?;
    assert_eq!(
        code(&checked),
        1,
        "{}",
        String::from_utf8_lossy(&checked.stdout.bytes)
    );
    let receipt: provenant_records::InstallReceipt = provenant_records::decode_v1(
        fs::File::open(rig.state.join("install/receipt.json"))?,
        provenant_records::Limits::CONFIG,
    )?;
    assert_eq!(
        receipt
            .legacy
            .ok_or("legacy receipt")?
            .git_hooks_dir
            .ok_or("git hooks")?
            .to_path_buf()?,
        hooks
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn recorded_legacy_hooks_replace_in_place_and_keep_herdr_during_dry_run() -> Result {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let legacy = rig.home.join("Repos/provenant");
    fs::create_dir_all(legacy.join(".git/hooks"))?;
    let init = rig.execute(
        Path::new("/usr/bin/git"),
        &["init", "-q", legacy.to_str().ok_or("legacy path")?],
        None,
    )?;
    assert_eq!(code(&init), 0);
    std::os::unix::fs::symlink(
        legacy.join("scripts/hooks/pre-push"),
        legacy.join(".git/hooks/pre-push"),
    )?;
    std::os::unix::fs::symlink(
        legacy.join("scripts/hooks/post-merge"),
        legacy.join(".git/hooks/post-merge"),
    )?;
    let settings = rig.root.join("claude/settings.json");
    let fixture = include_str!("fixtures/external/hooks/claude/legacy-settings.json")
        .replace("{home}", rig.home.to_str().ok_or("home")?)
        .replace("{legacy}", legacy.to_str().ok_or("legacy")?);
    fs::write(&settings, &fixture)?;
    let herdr_before: serde_json::Value = serde_json::from_str(&fixture)?;
    let arguments = [
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--harness",
        "claude",
        "--hooks",
        "on",
        "--legacy-checkout",
        legacy.to_str().ok_or("legacy")?,
        "--dry-run",
        "--json",
    ];
    let output = rig.command(&arguments)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    assert!(output.stderr.bytes.is_empty());
    let preview = String::from_utf8(output.stdout.bytes.clone())?;
    assert!(preview.contains("hook") && preview.contains("compact"));
    assert!(preview.contains("utility_output_hook.py"));
    assert_eq!(fs::read_to_string(&settings)?, fixture);
    assert!(!rig.state.exists());
    // Apply only to this disposable home, then check order and foreign bytes.
    let apply: Vec<_> = arguments
        .into_iter()
        .filter(|a| *a != "--dry-run")
        .collect();
    let output = rig.command(&apply)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let git_hook = fs::read_to_string(legacy.join(".git/hooks/pre-push"))?;
    assert!(
        git_hook.starts_with("#!/bin/sh\nexec '")
            && git_hook.contains("'hook' 'git' 'pre-push' \"$@\"")
    );
    assert!(!legacy.join(".git/hooks/post-merge").exists());
    let installed = fs::read_to_string(&settings)?;
    let value: serde_json::Value = serde_json::from_str(&installed)?;
    assert_eq!(
        value["hooks"]["SessionStart"][0],
        herdr_before["hooks"]["SessionStart"][0]
    );
    assert_eq!(value["hooks"]["SessionStart"][1]["matcher"], "compact");
    assert!(
        value["hooks"]["SessionStart"][1]["hooks"][0]["command"]
            .as_str()
            .ok_or("command")?
            .contains("'hook' 'claude' 'compact'")
    );
    assert_eq!(value["hooks"]["PreCompact"][0]["hooks"][0]["timeout"], 5);
    assert!(
        value["hooks"]["PostToolUse"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let reapplied = rig.command(&apply)?;
    assert_eq!(
        code(&reapplied),
        0,
        "{}",
        String::from_utf8_lossy(&reapplied.stdout.bytes)
    );
    let data: serde_json::Value = serde_json::from_slice(&reapplied.stdout.bytes)?;
    assert_eq!(data["data"]["outcome"]["actions"], serde_json::json!([]));
    let checked = rig.command(&["check", "install", "--json"])?;
    assert_eq!(
        code(&checked),
        0,
        "{}",
        String::from_utf8_lossy(&checked.stdout.bytes)
    );
    let removed = rig.command(&["uninstall", "--harness", "claude", "--json"])?;
    assert_eq!(
        code(&removed),
        0,
        "{}",
        String::from_utf8_lossy(&removed.stdout.bytes)
    );
    assert!(!legacy.join(".git/hooks/pre-push").exists());
    let retained: serde_json::Value = serde_json::from_slice(&fs::read(&settings)?)?;
    assert_eq!(
        retained["hooks"]["SessionStart"][0],
        herdr_before["hooks"]["SessionStart"][0]
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn repository_hooks_path_cannot_authorise_writes_outside_the_selected_checkout() -> Result {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let legacy = rig.root.join("legacy");
    let outside = rig.root.join("outside");
    fs::create_dir(&outside)?;
    let init = rig.execute(
        Path::new("/usr/bin/git"),
        &["init", "-q", legacy.to_str().ok_or("legacy")?],
        None,
    )?;
    assert_eq!(code(&init), 0);
    let config = rig.execute(
        Path::new("/usr/bin/git"),
        &[
            "-C",
            legacy.to_str().ok_or("legacy")?,
            "config",
            "core.hooksPath",
            outside.to_str().ok_or("outside")?,
        ],
        None,
    )?;
    assert_eq!(code(&config), 0);
    let target = legacy.join("scripts/hooks/pre-push");
    let foreign = outside.join("pre-push");
    std::os::unix::fs::symlink(&target, &foreign)?;
    let output = rig.command(&[
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--harness",
        "claude",
        "--hooks",
        "on",
        "--legacy-checkout",
        legacy.to_str().ok_or("legacy")?,
        "--json",
    ])?;
    assert_ne!(code(&output), 0);
    assert_eq!(fs::read_link(&foreign)?, target);
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}
