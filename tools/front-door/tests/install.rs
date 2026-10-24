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
        "#!/bin/sh\ncase \"$1\" in\n--version) printf '%s\\n' '{version_json}' ;;\n--help) printf '%s\\n' '{help_json}' ;;\n*) exit 2 ;;\nesac\n"
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
    let preview: serde_json::Value = serde_json::from_slice(
        dry.stderr
            .bytes
            .split(|b| *b == b'\n')
            .next()
            .ok_or("preview")?,
    )?;
    assert_eq!(preview["kind"], "actions");
    fs::create_dir_all(&rig.state)?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.fabric]\nclient_label=\"install-fixture\"\n",
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
        String::from_utf8_lossy(&checked.stderr.bytes)
    );
    let reapplied = rig.execute_os(binary, &args(&stage, false), None)?;
    assert_eq!(code(&reapplied), 0);
    let result: serde_json::Value = serde_json::from_slice(&reapplied.stdout.bytes)?;
    assert_eq!(result["data"]["outcome"]["actions"], serde_json::json!([]));
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
        format!("unmanaged destination conflicts: {}", source.display())
    );
    assert_eq!(
        fs::read_to_string(sensitive.join("release.json"))?,
        "private manifest sentinel"
    );
    assert!(!rig.state.exists());
    let old = legacy.join(".git/hooks/post-merge");
    symlink(legacy.join("scripts/hooks/post-merge"), &old)?;
    let mut args = args(&stage, false);
    args.extend(["--legacy-checkout".into(), legacy.clone().into_os_string()]);
    let installed = rig.execute_os(Path::new(env!("CARGO_BIN_EXE_provenant")), &args, None)?;
    assert_eq!(
        code(&installed),
        0,
        "{}",
        String::from_utf8_lossy(&installed.stderr.bytes)
    );
    assert!(fs::symlink_metadata(&old).is_err());
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
        legacy.join(".git/hooks")
    );
    rig.assert_legacy_unchanged()
}
