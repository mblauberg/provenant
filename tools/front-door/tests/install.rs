mod support;
// Exercise the binary-only tick rendering seam without registering a real service.
#[path = "../src/tick_agent.rs"]
#[allow(dead_code)]
mod tick_agent;
// Exercise the same reset/launchctl gate used by the install reporter, with
// only the process edge substituted; the state and receipt owners remain real.
#[path = "../src/state_reset.rs"]
#[allow(dead_code)]
mod state_reset;
use provenant_config::{Release, ReleaseSmoke};
use provenant_core::CancelToken;
use provenant_records::{CommandResult, Outcome, SourceInput, VersionInfo, source_digest};

use sha2::Digest as _;
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use support::{Result, TestRig, code};

/// Executables of the bundled app: the console, and the menu helper nested as a login-item app.
const APP_EXECUTABLES: [&str; 2] = ["Contents/MacOS/provenant-console", MENU_EXECUTABLE];
const MENU_EXECUTABLE: &str =
    "Contents/Library/LoginItems/Provenant Menu.app/Contents/MacOS/quota-menubar";

// Emulate the helper protocol for installer failure injection; the native
// RENAME_SWAP implementation is exercised by the Swift Menu tests.
const APP_FIXTURE: &str = "#!/bin/sh\nif [ \"$1\" = --swap-path ]; then\n  mv \"$2\" \"$2.swap\" && mv \"$3\" \"$2\" && mv \"$2.swap\" \"$3\" || exit $?\n  if [ -n \"${PROVENANT_TEST_SWAP_SIGNAL:-}\" ] && [ ! -f \"$PROVENANT_TEST_SWAP_SIGNAL\" ]; then\n    touch \"$PROVENANT_TEST_SWAP_SIGNAL\"; kill -TERM \"$PPID\"\n  fi\n  exit 0\nfi\nexit 0\n";

struct FixtureSmoke(VersionInfo);

impl ReleaseSmoke for FixtureSmoke {
    fn check(&self, _: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        Ok(self.0.clone())
    }
}
fn staged(rig: &TestRig) -> Result<PathBuf> {
    staged_named(rig, "staged", b"synthetic release")
}
fn staged_named(rig: &TestRig, name: &str, identity: &[u8]) -> Result<PathBuf> {
    staged_binary(rig, name, identity, None)
}

fn staged_binary(
    rig: &TestRig,
    name: &str,
    identity: &[u8],
    signed_binary: Option<&Path>,
) -> Result<PathBuf> {
    let stage = rig.root.join(name);
    fs::create_dir_all(stage.join("bin"))?;
    fs::create_dir_all(stage.join("skills/example"))?;
    let skill = b"---\nname: example\ndescription: fixture\n---\n";
    let mut asset_bytes = vec![0];
    asset_bytes.extend(skill);
    let installer = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh"))?;
    let mut installer_bytes = vec![0];
    installer_bytes.extend(&installer);
    let version = VersionInfo {
        extra: Default::default(),
        source_digest: source_digest(&[SourceInput {
            name: "fixture.rs",
            bytes: identity,
        }])?,
        asset_digest: source_digest(&[
            SourceInput {
                name: "skills/example/SKILL.md",
                bytes: &asset_bytes,
            },
            SourceInput {
                name: "scripts/install.sh",
                bytes: &installer_bytes,
            },
        ])?,
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
        "#!/bin/sh\ncase \"$1\" in\n--version) printf '%s\\n' '{version_json}' ;;\n--help|help) printf '%s\\n' '{help_json}' ;;\n--state-dir) exec '{}' \"$@\" ;;\n*) exit 2 ;;\nesac\n",
        env!("CARGO_BIN_EXE_provenant").replace('\'', "'\\''")
    );
    let binary = stage.join("bin/provenant");
    fs::write(&binary, script)?;
    if let Some(signed_binary) = signed_binary {
        fs::copy(signed_binary, &binary)?;
    }
    fs::set_permissions(binary, fs::Permissions::from_mode(0o755))?;
    let path = stage.join("skills/example/SKILL.md");
    fs::write(&path, skill)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    fs::create_dir_all(stage.join("scripts"))?;
    fs::write(stage.join("scripts/install.sh"), installer)?;
    fs::set_permissions(
        stage.join("scripts/install.sh"),
        fs::Permissions::from_mode(0o600),
    )?;
    provenant_config::package_release(
        &stage,
        &version,
        &FixtureSmoke(version.clone()),
        &CancelToken::new(),
    )?;
    Ok(stage)
}

#[test]
fn tick_upgrade_keeps_stable_program_path_and_valid_signature() -> Result {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    use provenant_core::{Clock, WaitBudget};
    struct Reporter;
    impl provenant_config::InstallReporter for Reporter {
        fn preview(&mut self, _: &[provenant_config::Action]) -> io::Result<()> {
            Ok(())
        }
    }
    let rig = TestRig::new()?;
    let roots = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone())
        .resolve(&provenant_os::fs::SystemPathResolver)?;
    let mut targets =
        provenant_config::Targets::new(roots, &rig.home, &[provenant_records::Harness::Codex]);
    let program = targets.bin_dir.join("provenant");
    targets.tick_agent = Some(tick_agent::render(
        &program,
        targets.roots.state.as_path(),
        "/usr/bin:/bin".as_ref(),
    )?);
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let context = provenant_config::InstallContext {
        clock: &clock,
        cancel: &cancel,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
    };
    // Signed Mach-O stand-ins exercise actual binary publication and replacement.
    // Config owns those writes; no launchctl operation or ambient user agent is invoked.
    for (name, source) in [
        ("first-signed", "/usr/bin/true"),
        ("next-signed", "/bin/echo"),
    ] {
        let stage = staged_binary(&rig, name, name.as_bytes(), Some(Path::new(source)))?;
        let release = Release::read(&stage, env!("PROVENANT_TARGET_TRIPLE"))?;
        let manifest = release.manifest();
        let smoke = FixtureSmoke(VersionInfo {
            extra: Default::default(),
            source_digest: manifest.source_digest,
            asset_digest: manifest.asset_digest,
            target: manifest.target.clone(),
            toolchain: manifest.toolchain.clone(),
            record_schema_version: 1,
            config_schema_version: 1,
        });
        provenant_config::plan_and_install(
            &release,
            &targets,
            &context,
            &mut Reporter,
            &smoke,
            false,
        )?;
        assert_eq!(fs::read(&program)?, fs::read(source)?);
        let plist = tick_agent::plist(&rig.home);
        let extracted = rig.execute_os(
            Path::new("/usr/bin/plutil"),
            &[
                "-extract".into(),
                "ProgramArguments.0".into(),
                "raw".into(),
                "-o".into(),
                "-".into(),
                plist.into(),
            ],
            None,
        )?;
        assert_eq!(code(&extracted), 0, "{extracted:?}");
        assert_eq!(
            String::from_utf8(extracted.stdout.bytes)?.trim(),
            program.to_str().ok_or("program path")?
        );
        let verified = rig.execute_os(
            Path::new("/usr/bin/codesign"),
            &["--verify".into(), program.clone().into()],
            None,
        )?;
        assert_eq!(code(&verified), 0, "{verified:?}");
    }
    Ok(())
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

fn inspect_installed(rig: &TestRig, stage: &Path) -> Result<provenant_config::InstallInspection> {
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.environment_state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let targets =
        provenant_config::Targets::new(roots, &rig.home, &[provenant_records::Harness::Claude]);
    let release = Release::read(stage, env!("PROVENANT_TARGET_TRIPLE"))?;
    let manifest = release.manifest();
    let smoke = FixtureSmoke(VersionInfo {
        extra: Default::default(),
        source_digest: manifest.source_digest,
        asset_digest: manifest.asset_digest,
        target: manifest.target.clone(),
        toolchain: manifest.toolchain.clone(),
        record_schema_version: 1,
        config_schema_version: 1,
    });
    Ok(provenant_config::inspect_install(
        &targets,
        Some(&release),
        env!("PROVENANT_TARGET_TRIPLE"),
        &smoke,
        &CancelToken::new(),
    )?)
}
#[test]
fn home_rename_with_flag_or_config_bin_preserves_old_paths() -> Result {
    use provenant_records::{InstallReceipt, Limits};
    use std::os::unix::fs::symlink;
    enum Canary {
        Link(PathBuf),
        File(Vec<u8>, fs::Permissions),
    }
    // Staged smoke executables must live under a lane's executable build root.
    let executable = std::env::current_exe()?;
    let fixture_root = executable.parent().ok_or("test executable parent")?;
    for (bin_setting, state_in_home) in [
        ("flag-external", true),
        ("flag-home", true),
        ("config-home", true),
        ("config-home", false),
    ] {
        let mut rig = TestRig::in_directory(fixture_root)?;
        if state_in_home {
            rig.state = rig.home.join(".local/share/provenant/v1");
        }
        rig.env.retain(|(key, _)| {
            !matches!(key.to_str(), Some("CODEX_HOME" | "CLAUDE_CONFIG_DIR"))
                && !(state_in_home && key == "PROVENANT_STATE_DIR")
        });
        let bin_dir = match bin_setting {
            "flag-external" => rig.root.join("global-bin"),
            "flag-home" => rig.home.join("bin"),
            _ => rig.home.join("tools/bin"),
        };
        let stage = staged(&rig)?;
        let mut install_args = args(&stage, false);
        if bin_setting == "config-home" {
            fs::create_dir_all(&rig.state)?;
            fs::write(
                rig.state.join("config.toml"),
                format!(
                    "schema_version=1\n[install]\nbin_dir={}\n",
                    toml_edit::Value::from(bin_dir.to_str().ok_or("bin dir")?)
                ),
            )?;
        } else {
            install_args.extend(["--bin-dir".into(), bin_dir.into_os_string()]);
        }
        enable_fabric_mcp(&rig)?;
        let binary = Path::new(env!("CARGO_BIN_EXE_provenant"));
        let installed = rig.execute_os(binary, &install_args, None)?;
        assert_eq!(
            code(&installed),
            0,
            "{bin_setting}: {}",
            String::from_utf8_lossy(&installed.stdout.bytes)
        );
        let receipt: InstallReceipt = provenant_records::decode_v1(
            fs::read(rig.state.join("install/receipt.json"))?.as_slice(),
            Limits::CONFIG,
        )?;
        let mut canaries = Vec::new();
        for entry in &receipt.entries {
            let path = entry.path.to_path_buf()?;
            if path.starts_with(&rig.home) {
                let metadata = fs::symlink_metadata(&path)?;
                let value = if metadata.file_type().is_symlink() {
                    Canary::Link(fs::read_link(&path)?)
                } else {
                    Canary::File(fs::read(&path)?, metadata.permissions())
                };
                canaries.push((path, value));
            }
        }
        let old_home = rig.home.clone();
        let moved_home = rig.root.join("home-moved");
        fs::rename(&old_home, &moved_home)?;
        for (path, value) in &canaries {
            fs::create_dir_all(path.parent().ok_or("canary parent")?)?;
            match value {
                Canary::Link(target) => symlink(target, path)?,
                Canary::File(bytes, mode) => {
                    fs::write(path, bytes)?;
                    fs::set_permissions(path, mode.clone())?;
                }
            }
        }
        let old_release_binary = old_home
            .join(".local/share/provenant/v1/releases")
            .join(receipt.source_digest.to_string())
            .join("bin/provenant");
        if state_in_home {
            fs::create_dir_all(old_release_binary.parent().ok_or("release parent")?)?;
            fs::write(&old_release_binary, b"old release canary\n")?;
        }
        rig.home = moved_home.clone();
        if state_in_home {
            rig.state = moved_home.join(".local/share/provenant/v1");
        }
        // Doctor receives unresolved HOME and does not load install config.
        let alias = rig.root.join("home-alias");
        symlink(&moved_home, &alias)?;
        for (key, value) in &mut rig.env {
            if key == "HOME" {
                *value = alias.clone().into_os_string();
            }
        }
        let old_mcp = old_home.join(".codex/config.toml");
        let mcp_bytes = fs::read(&old_mcp)?;
        fs::write(&old_mcp, b"invalid TOML\0")?;
        let checked = rig.execute(binary, &["check", "install", "--json"], None)?;
        let checked: serde_json::Value = serde_json::from_slice(&checked.stdout.bytes)?;
        assert!(checked["error"].is_null(), "{bin_setting}: {checked}");
        assert!(
            !checked["data"]["findings"]
                .as_array()
                .ok_or("check findings")?
                .iter()
                .any(|row| row["rule"] == "install.invalid"),
            "{bin_setting}: {checked}"
        );
        fs::write(&old_mcp, mcp_bytes)?;
        // Uninstall has no --bin-dir flag; the config path still names old HOME.
        let uninstall_args = if state_in_home {
            vec!["uninstall", "--harness", "codex", "--json"]
        } else {
            vec!["uninstall", "--harness", "codex"]
        };
        let removed = rig.execute(binary, &uninstall_args, None)?;
        assert_eq!(code(&removed), 0, "{bin_setting}: {removed:?}");
        if state_in_home {
            let removed: serde_json::Value = serde_json::from_slice(&removed.stdout.bytes)?;
            assert!(
                removed["data"]["outcome"]["findings"]
                    .as_array()
                    .ok_or("findings")?
                    .iter()
                    .any(|finding| finding["rule"] == "relocated_stale_entry")
            );
        } else {
            assert!(
                String::from_utf8_lossy(&removed.stdout.bytes)
                    .contains("Home moved; old paths left untouched")
            );
        }
        for (path, value) in &canaries {
            match value {
                Canary::Link(target) => assert_eq!(fs::read_link(path)?, *target),
                Canary::File(bytes, mode) => {
                    assert_eq!(fs::read(path)?, *bytes);
                    assert_eq!(fs::metadata(path)?.permissions().mode(), mode.mode());
                }
            }
        }
        if state_in_home {
            assert_eq!(fs::read(old_release_binary)?, b"old release canary\n");
        }
    }
    Ok(())
}

#[test]
fn check_install_source_failures_keep_the_typed_outcome() -> Result {
    let rig = TestRig::new()?.without_state()?;
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
fn managed_install_prunes_releases_beyond_the_two_previous() -> Result {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let binary = Path::new(env!("CARGO_BIN_EXE_provenant"));
    let first = rig.execute_os(binary, &args(&stage, false), None)?;
    assert_eq!(code(&first), 0, "{first:?}");
    let receipt: provenant_records::InstallReceipt = provenant_records::decode_v1(
        fs::read(rig.state.join("install/receipt.json"))?.as_slice(),
        provenant_records::Limits::CONFIG,
    )?;
    let releases = rig.state.join("releases");
    let older: Vec<_> = (1..=4).map(|n| format!("{n:064x}")).collect();
    for (index, name) in older.iter().enumerate() {
        let path = releases.join(name);
        fs::create_dir_all(&path)?;
        fs::write(path.join("release.json"), b"managed release")?;
        let file = fs::File::open(&path)?;
        file.set_times(fs::FileTimes::new().set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(1_700_000_000 - (index as u64 + 1) * 86_400),
        ))?;
    }

    let second = rig.execute_os(binary, &args(&stage, false), None)?;
    assert_eq!(code(&second), 0, "{second:?}");
    assert!(releases.join(receipt.source_digest.to_string()).exists());
    assert!(releases.join(&older[0]).exists());
    assert!(releases.join(&older[1]).exists());
    assert!(!releases.join(&older[2]).exists());
    assert!(!releases.join(&older[3]).exists());
    Ok(())
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
    let rig = TestRig::new()?.without_state()?;
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
    assert!(!human_stdout.contains("findings"));
    assert!(!human_stdout.trim_start().starts_with('{'));
    assert!(!String::from_utf8_lossy(&human.stderr.bytes).contains("\"kind\":\"actions\""));
    fs::create_dir_all(&rig.state)?;
    fs::write(
        rig.state.join("config.toml"),
        // Sections other commands write must not block installation.
        "schema_version=1\n[domains.fabric]\nclient_label=\"install-fixture\"\n[domains.accounts]\n[knobs]\n'fabric.mcp_server'=true\n",
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
    assert!(
        human_reapply.contains("0 changes (0 links, 0 hooks, 0 MCP)"),
        "{human_reapply}"
    );
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
fn external_claude_config_without_home_move_keeps_ownership() -> Result {
    let executable = std::env::current_exe()?;
    let rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    let first = staged(&rig)?;
    let second = staged_named(&rig, "new-release", b"new release")?;
    let config = rig.root.join("claude");
    assert!(!config.starts_with(&rig.home));
    enable_fabric_mcp(&rig)?;
    for (stage, hooks) in [(&first, true), (&second, false)] {
        let mut args = vec![
            "install",
            "--source",
            stage.to_str().ok_or("stage")?,
            "--harness",
            "claude",
            "--json",
        ];
        if hooks {
            args.extend(["--hooks", "on"]);
        }
        let installed = rig.command(&args)?;
        assert_eq!(
            code(&installed),
            0,
            "{}",
            String::from_utf8_lossy(&installed.stdout.bytes)
        );
        let checked = rig.command(&["check", "install", "--json"])?;
        let report: serde_json::Value = serde_json::from_slice(&checked.stdout.bytes)?;
        assert_eq!(code(&checked), 0, "{report}");
        assert_eq!(report["data"]["findings"], serde_json::json!([]));
        let receipt: provenant_records::InstallReceipt = provenant_records::decode_v1(
            fs::read(rig.state.join("install/receipt.json"))?.as_slice(),
            provenant_records::Limits::CONFIG,
        )?;
        for selector in ["skill", "json:/mcpServers/fabric", "hook:"] {
            assert!(receipt.entries.iter().any(|entry| {
                entry.harness == Some(provenant_records::Harness::Claude)
                    && entry.selector.starts_with(selector)
            }));
        }
        let mcp: serde_json::Value =
            serde_json::from_slice(&fs::read(config.join(".claude.json"))?)?;
        assert!(mcp["mcpServers"]["fabric"].is_object());
        assert!(config.join("skills/example").is_symlink());
        assert!(fs::read_to_string(config.join("settings.json"))?.contains("'hook' 'claude'"));
    }
    let removed = rig.command(&["uninstall", "--harness", "claude", "--json"])?;
    assert_eq!(code(&removed), 0, "{removed:?}");
    let report: serde_json::Value = serde_json::from_slice(&removed.stdout.bytes)?;
    assert_eq!(report["data"]["outcome"]["findings"], serde_json::json!([]));
    assert!(fs::symlink_metadata(config.join("skills/example")).is_err());
    let mcp: serde_json::Value = serde_json::from_slice(&fs::read(config.join(".claude.json"))?)?;
    assert!(mcp["mcpServers"]["fabric"].is_null());
    assert!(!fs::read_to_string(config.join("settings.json"))?.contains("'hook' 'claude'"));
    rig.assert_legacy_unchanged()
}

#[test]
fn hook_ownership_survives_upgrade_without_flags_and_explicit_off_removes_it() -> Result {
    let executable = std::env::current_exe()?;
    let fixture_root = executable.parent().ok_or("test executable parent")?;
    for (explicit_off, modified) in [(false, false), (true, false), (true, true)] {
        let rig = TestRig::in_directory(fixture_root)?;
        let first = staged(&rig)?;
        let second = staged_named(&rig, "new-release", b"new release")?;
        let output = rig.command(&[
            "install",
            "--source",
            first.to_str().ok_or("first")?,
            "--harness",
            "claude",
            "--hooks",
            "on",
            "--json",
        ])?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stdout.bytes)
        );
        let settings = rig.root.join("claude/settings.json");
        let before = fs::read(&settings)?;
        let output = rig.command(&[
            "install",
            "--source",
            second.to_str().ok_or("second")?,
            "--harness",
            "claude",
            "--json",
        ])?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stdout.bytes)
        );
        assert_eq!(fs::read(&settings)?, before);
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(rig.state.join("install/receipt.json"))?)?;
        assert!(
            receipt["entries"]
                .as_array()
                .ok_or("entries")?
                .iter()
                .any(|entry| entry["selector"]
                    .as_str()
                    .is_some_and(|selector| selector.starts_with("hook:")))
        );
        assert!(fs::read_to_string(&settings)?.contains("'hook' 'claude' 'pre-tool-use'"));
        if explicit_off {
            let moved = rig.root.join("new-bin");
            let output = rig.command(&[
                "install",
                "--source",
                second.to_str().ok_or("second")?,
                "--harness",
                "claude",
                "--bin-dir",
                moved.to_str().ok_or("bin")?,
                "--json",
            ])?;
            assert_eq!(
                code(&output),
                0,
                "{}",
                String::from_utf8_lossy(&output.stdout.bytes)
            );
            assert!(
                fs::read_to_string(&settings)?
                    .contains(moved.join("provenant").to_str().ok_or("binary")?)
            );
            if modified {
                let mut document: serde_json::Value =
                    serde_json::from_slice(&fs::read(&settings)?)?;
                document["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"] = serde_json::json!(123);
                fs::write(&settings, serde_json::to_vec(&document)?)?;
            }
            let output = rig.command(&[
                "install",
                "--source",
                second.to_str().ok_or("second")?,
                "--harness",
                "claude",
                "--hooks",
                "off",
                "--bin-dir",
                moved.to_str().ok_or("bin")?,
                "--json",
            ])?;
            assert_eq!(
                code(&output),
                0,
                "{}",
                String::from_utf8_lossy(&output.stdout.bytes)
            );
            if modified {
                let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
                assert!(
                    report["data"]["outcome"]["findings"]
                        .as_array()
                        .ok_or("findings")?
                        .iter()
                        .any(|finding| finding["rule"] == "relinquished_modified_entry")
                );
            }
        } else {
            let checked = inspect_installed(&rig, &second)?;
            assert!(checked.error.is_none());
            assert!(checked.findings.is_empty());
            let output = rig.command(&["uninstall", "--harness", "claude", "--json"])?;
            assert_eq!(
                code(&output),
                0,
                "{}",
                String::from_utf8_lossy(&output.stdout.bytes)
            );
        }
        let retained = fs::read_to_string(&settings)?;
        if modified {
            assert!(retained.contains("'hook' 'claude' 'pre-tool-use'"));
            assert!(retained.contains("123"));
            assert!(!retained.contains("'hook' 'claude' 'pre-compact'"));
        } else {
            assert!(!retained.contains("'hook' 'claude'"));
        }
        rig.assert_legacy_unchanged()?;
    }
    Ok(())
}

#[test]
fn unknown_install_flag_is_rejected_without_writes() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let stage = staged(&rig)?;
    let mut install_args = args(&stage, false);
    install_args.extend([
        "--unknown-install-option".into(),
        rig.project.clone().into_os_string(),
    ]);
    let output = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &install_args,
        None,
    )?;
    assert_ne!(code(&output), 0);
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(message.contains("--unknown-install-option"), "{message}");
    assert!(!rig.state.exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn bare_install_detects_clis_and_claude_directories_then_replays_receipt() -> Result {
    let mut rig = TestRig::new()?.without_state()?;
    rig.env
        .retain(|(key, _)| !matches!(key.to_str(), Some("CODEX_HOME" | "CLAUDE_CONFIG_DIR")));
    let path = rig.root.join("detected-bin");
    fs::create_dir(&path)?;
    for cli in ["claude", "codex", "cursor-agent", "agy", "opencode"] {
        let file = path.join(cli);
        fs::write(&file, "#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(file, fs::Permissions::from_mode(0o755))?;
    }
    for label in ["work", "personal"] {
        fs::create_dir(rig.home.join(format!(".claude-{label}")))?;
    }
    fs::write(rig.home.join(".claude-not-a-directory"), "ignore")?;
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = format!("{}:/usr/bin:/bin", path.display()).into();
        }
    }
    let first = staged(&rig)?;
    let second = staged_named(&rig, "new-release", b"upgrade")?;
    let install = |stage: &Path, dry: bool| -> Result<_> {
        let mut arguments = vec![
            "install".into(),
            "--source".into(),
            stage.into(),
            "--json".into(),
        ];
        arguments.extend(["--hooks".into(), "on".into()]);
        if dry {
            arguments.push("--dry-run".into());
        }
        rig.execute_os(Path::new(env!("CARGO_BIN_EXE_provenant")), &arguments, None)
    };
    let dry = install(&first, true)?;
    assert_eq!(code(&dry), 0, "{dry:?}");
    assert!(!rig.state.exists());
    let conflict = rig.home.join(".codex/config.toml");
    fs::create_dir_all(conflict.parent().ok_or("config parent")?)?;
    std::os::unix::fs::symlink(rig.home.join("foreign-config"), &conflict)?;
    let refused = install(&first, false)?;
    assert_ne!(code(&refused), 0);
    assert!(!rig.state.join("install/receipt.json").exists());
    fs::remove_file(conflict)?;
    let installed = install(&first, false)?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    let receipt_path = rig.state.join("install/receipt.json");
    let before: provenant_records::InstallReceipt = provenant_records::decode_v1(
        &*fs::read(&receipt_path)?,
        provenant_records::Limits::CONFIG,
    )?;
    assert_eq!(before.harnesses.len(), 5);
    assert_eq!(before.harness_targets.len(), 7);
    for label in ["work", "personal"] {
        let expected = rig.home.join(format!(".claude-{label}/skills"));
        assert!(
            before.harness_targets.iter().any(|target| target
                .skills_dir
                .to_path_buf()
                .ok()
                .as_deref()
                == Some(expected.as_path()))
        );
    }
    fs::create_dir(rig.home.join(".claude-added-later"))?;
    let upgraded = install(&second, false)?;
    assert_eq!(code(&upgraded), 0, "{upgraded:?}");
    let after: provenant_records::InstallReceipt =
        provenant_records::decode_v1(&*fs::read(receipt_path)?, provenant_records::Limits::CONFIG)?;
    assert_eq!(after.harness_targets, before.harness_targets);
    assert_ne!(after.source_digest, before.source_digest);
    assert!(!rig.home.join(".claude-added-later/skills").exists());
    let checked = rig.command(&["check", "install", "--json"])?;
    assert_eq!(code(&checked), 0, "{checked:?}");
    Ok(())
}

#[test]
fn bare_install_preserves_recorded_custom_paths() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let stage = staged(&rig)?;
    let bin = rig.root.join("custom-bin");
    let installed = rig.command(&[
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--harness",
        "codex",
        "--bin-dir",
        bin.to_str().ok_or("bin")?,
        "--json",
    ])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    let upgraded = rig.command(&[
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--json",
    ])?;
    assert_eq!(code(&upgraded), 0, "{upgraded:?}");
    let receipt: provenant_records::InstallReceipt = provenant_records::decode_v1(
        &*fs::read(rig.state.join("install/receipt.json"))?,
        provenant_records::Limits::CONFIG,
    )?;
    assert_eq!(receipt.bin_dir.to_path_buf()?, bin);
    assert_eq!(receipt.harness_targets.len(), 1);
    assert_eq!(
        receipt.harness_targets[0].skills_dir.to_path_buf()?,
        rig.root.join("codex/skills")
    );
    Ok(())
}
#[test]
fn bootstrap_truncated_pipe_runs_nothing() -> Result {
    let mut rig = TestRig::new()?;
    let temporary = rig.root.join("temporary");
    fs::create_dir(&temporary)?;
    rig.env.push(("TMPDIR".into(), temporary.clone().into()));
    let script =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh"))?;
    let truncated = script.split("instruction_count=0").next().ok_or("script")?;
    let _ = rig.execute_os(
        Path::new("/bin/sh"),
        &["-s".into(), "--".into(), "--yes".into()],
        Some(truncated.as_bytes()),
    )?;
    assert_eq!(fs::read_dir(temporary)?.count(), 0);
    Ok(())
}

#[test]
fn bootstrap_help_and_unsupported_hosts_need_no_download() -> Result {
    let mut rig = TestRig::new()?;
    let fake_bin = rig.root.join("fake-bin");
    fs::create_dir(&fake_bin)?;
    let uname = fake_bin.join("uname");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh");
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = format!("{}:/usr/bin:/bin", fake_bin.display()).into();
        }
    }
    for (system, architecture) in [("Darwin", "x86_64"), ("Linux", "aarch64")] {
        fs::write(
            &uname,
            format!(
                "#!/bin/sh\ncase \"$1\" in -s) echo {system} ;; -m) echo {architecture} ;; esac\n"
            ),
        )?;
        fs::set_permissions(&uname, fs::Permissions::from_mode(0o755))?;
        let help = rig.execute_os(
            Path::new("/bin/sh"),
            &[script.clone().into(), "--help".into()],
            None,
        )?;
        assert_eq!(code(&help), 0);
        assert!(String::from_utf8(help.stdout.bytes)?.contains("--upgrade"));
        let failed = rig.execute_os(
            Path::new("/bin/sh"),
            &[script.clone().into(), "--yes".into()],
            None,
        )?;
        assert_eq!(code(&failed), 4, "{failed:?}");
        assert_eq!(
            failed.stderr.bytes,
            b"only macOS arm64 release packages are published; use --source\n"
        );
    }
    Ok(())
}

/// Reports the published release platform so the download path runs on any CI host.
fn fake_supported_uname(fake_bin: &Path) -> Result {
    let uname = fake_bin.join("uname");
    fs::write(
        &uname,
        "#!/bin/sh\ncase \"$1\" in -s) echo Darwin ;; -m) echo arm64 ;; esac\n",
    )?;
    fs::set_permissions(&uname, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[test]
fn bootstrap_verifies_release_checksum() -> Result {
    let mut rig = TestRig::new()?;
    let fixtures = rig.root.join("downloads");
    let fake_bin = rig.root.join("fake-bin");
    let release = rig.root.join("package");
    fs::create_dir_all(&fixtures)?;
    fs::create_dir_all(&fake_bin)?;
    fs::create_dir_all(release.join("bin"))?;
    fs::create_dir_all(release.join("instructions"))?;
    fs::write(
        release.join("instructions/AGENTS.md"),
        "fixture instructions\n",
    )?;
    let recorded = rig.root.join("installed-args");
    let executable = release.join("bin/provenant");
    fs::write(
        &executable,
        format!(
            r#"#!/bin/sh
if IFS= read -r unexpected; then exit 91; fi
printf '%s\n' "$@" >> '{}'
if [ "${{PROVENANT_TEST_ACCOUNT_FAILURE:-}}" = 1 ]; then
    [ "${{CLAUDE_CONFIG_DIR##*/}}" != .claude-work ] || exit 17
    printf '%s\n' published > "$HOME/published-instructions"
    ln -s "$HOME/published-instructions" "$CLAUDE_CONFIG_DIR/CLAUDE.md"
    mkdir -p "$PROVENANT_STATE_DIR/install"
    printf '%s\n' published > "$PROVENANT_STATE_DIR/install/receipt.json"
fi
"#,
            recorded.display()
        ),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let archive = fixtures.join("package.tar.gz");
    let tar = rig.execute_os(
        Path::new("/usr/bin/tar"),
        &[
            "-czf".into(),
            archive.clone().into(),
            "-C".into(),
            release.into(),
            ".".into(),
        ],
        None,
    )?;
    assert_eq!(code(&tar), 0);
    let hash = rig.execute_os(
        Path::new("/usr/bin/shasum"),
        &["-a".into(), "256".into(), archive.clone().into()],
        None,
    )?;
    assert_eq!(code(&hash), 0);
    let digest = String::from_utf8(hash.stdout.bytes)?
        .split_whitespace()
        .next()
        .ok_or("hash")?
        .to_owned();
    fs::write(
        fixtures.join("checksum"),
        format!("{digest}  package.tar.gz\n"),
    )?;
    let app_fixture = fixtures.join("apps");
    for binary in APP_EXECUTABLES {
        let path = app_fixture.join("Provenant Console.app").join(binary);
        fs::create_dir_all(path.parent().ok_or("app executable parent")?)?;
        fs::write(&path, APP_FIXTURE)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let apps_archive = fixtures.join("provenant-apps-aarch64-apple-darwin.tar.gz");
    let tar = rig.execute_os(
        Path::new("/usr/bin/tar"),
        &[
            "-czf".into(),
            apps_archive.clone().into(),
            "-C".into(),
            app_fixture.into(),
            ".".into(),
        ],
        None,
    )?;
    assert_eq!(code(&tar), 0);
    let hash = rig.execute_os(
        Path::new("/usr/bin/shasum"),
        &["-a".into(), "256".into(), apps_archive.clone().into()],
        None,
    )?;
    let apps_digest = String::from_utf8(hash.stdout.bytes)?
        .split_whitespace()
        .next()
        .ok_or("apps archive hash")?
        .to_owned();
    fs::write(
        fixtures.join("apps-checksum"),
        format!("{apps_digest}  provenant-apps-aarch64-apple-darwin.tar.gz\n"),
    )?;
    let curl = fake_bin.join("curl");
    fs::write(
        &curl,
        format!(
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -w) format=$2; shift 2 ;;
    *) url=$1; shift ;;
  esac
done
case "$url" in
  */provenant-apps-aarch64-apple-darwin.tar.gz.sha256)
    [ "${{PROVENANT_TEST_APPS_AVAILABLE:-}}" = 1 ] || {{ printf '%s' 404; exit 0; }}
    cp '{}/apps-checksum' "$out" ;;
  */provenant-apps-aarch64-apple-darwin.tar.gz)
    [ "${{PROVENANT_TEST_APPS_AVAILABLE:-}}" = 1 ] || {{ printf '%s' 404; exit 0; }}
    cp '{}/provenant-apps-aarch64-apple-darwin.tar.gz' "$out"
    [ -z "${{format:-}}" ] || printf '%s' 200 ;;
  */provenant-aarch64-apple-darwin.tar.gz.sha256) cp '{}/checksum' "$out"; [ "${{format:-}}" != '%{{http_code}}' ] || printf '%s' 200 ;;
  */provenant-aarch64-apple-darwin.tar.gz) cp '{}/package.tar.gz' "$out"; [ "${{format:-}}" != '%{{http_code}}' ] || printf '%s' 200 ;;
  *) [ "${{format:-}}" != '%{{http_code}}' ] || printf '%s' 404 ;;
esac
"#,
            fixtures.display(),
            fixtures.display(),
            fixtures.display(),
            fixtures.display()
        ),
    )?;
    fs::set_permissions(curl, fs::Permissions::from_mode(0o755))?;
    fake_supported_uname(&fake_bin)?;
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = format!("{}:/usr/bin:/bin", fake_bin.display()).into();
        }
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../install.sh")
        .canonicalize()?;
    let args = [
        script.into_os_string(),
        "--yes".into(),
        "--tag".into(),
        "v1.2.3".into(),
    ];
    let result = rig.execute_os(Path::new("/bin/sh"), &args, None)?;
    assert_eq!(
        code(&result),
        0,
        "{}",
        String::from_utf8_lossy(&result.stderr.bytes)
    );
    let installed = fs::read_to_string(&recorded)?;
    assert!(installed.contains("install\n--source\n"));
    assert!(!installed.contains("--harness"));
    // A curl | sh invocation must not infer a checkout tag, and children must
    // not consume shell input after the final main call.
    let git = fake_bin.join("git");
    let git_called = rig.root.join("git-called");
    fs::write(
        &git,
        format!("#!/bin/sh\ntouch '{}'\nexit 92\n", git_called.display()),
    )?;
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755))?;
    let mut piped_script = fs::read(&args[0])?;
    piped_script.extend_from_slice(b"printf '%s\\n' PIPE_COMPLETE\n");
    let piped = rig.execute_os(
        Path::new("/bin/sh"),
        &["-s".into(), "--".into(), "--yes".into()],
        Some(&piped_script),
    )?;
    assert_eq!(code(&piped), 0, "{piped:?}");
    assert!(piped.stdout.bytes.ends_with(b"PIPE_COMPLETE\n"));
    assert!(!git_called.exists());
    fs::remove_file(&recorded)?;
    rig.env.push(("PROVENANT_INSTALL_TTY".into(), "1".into()));
    let codex = rig.root.join("codex/AGENTS.md");
    fs::write(&codex, "keep my instructions\n")?;
    let guided_args = [args[0].clone(), "--tag".into(), "v1.2.3".into()];
    let cancelled = rig.execute_os(Path::new("/bin/sh"), &guided_args, Some(b"n\ny\nn\nn\nn\n"))?;
    assert_eq!(code(&cancelled), 2);
    assert_eq!(fs::read_to_string(&codex)?, "keep my instructions\n");
    let guided = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(
        code(&guided),
        0,
        "{}",
        String::from_utf8_lossy(&guided.stderr.bytes)
    );
    assert_eq!(
        fs::read_to_string(&codex)?,
        "keep my instructions\n\nfixture instructions\n"
    );
    assert!(fs::read_to_string(&recorded)?.contains("--harness\ncodex\n"));
    fs::remove_file(&recorded)?;
    fs::create_dir_all(rig.state.join("install"))?;
    fs::write(rig.state.join("install/receipt.json"), "fixture receipt")?;
    let rerun = rig.execute_os(Path::new("/bin/sh"), &guided_args, None)?;
    assert_eq!(
        code(&rerun),
        0,
        "{}",
        String::from_utf8_lossy(&rerun.stderr.bytes)
    );
    assert_eq!(
        rerun.stdout.bytes,
        b"This release has no bundled app archive; upgrading Provenant without refreshing the app.\n"
    );
    assert!(!fs::read_to_string(&recorded)?.contains("--harness"));
    fs::remove_file(&recorded)?;
    fs::remove_file(rig.state.join("install/receipt.json"))?;
    let account = rig.home.join(".claude-work");
    fs::create_dir(&account)?;
    for (key, value) in &mut rig.env {
        if key == "CLAUDE_CONFIG_DIR" {
            *value = account.clone().into_os_string();
        }
    }
    let account_setup = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"y\nn\nn\nn\nn\nskip\nn\nn\n"),
    )?;
    assert_eq!(
        code(&account_setup),
        0,
        "{}",
        String::from_utf8_lossy(&account_setup.stderr.bytes)
    );
    let prompts = String::from_utf8(account_setup.stderr.bytes)?;
    assert_eq!(prompts.matches("CLAUDE.md: replace").count(), 1);
    fs::remove_file(&recorded)?;
    // Preserve the first install's published links and receipt if a later
    // account fails after replace selections.
    let primary = rig.root.join("claude");
    for (key, value) in &mut rig.env {
        if key == "CLAUDE_CONFIG_DIR" {
            *value = primary.clone().into_os_string();
        }
    }
    fs::write(primary.join("CLAUDE.md"), "original primary\n")?;
    fs::write(account.join("CLAUDE.md"), "original account\n")?;
    rig.env
        .push(("PROVENANT_TEST_ACCOUNT_FAILURE".into(), "1".into()));
    let failed_account = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"y\nn\nn\nn\nn\nreplace\nreplace\nn\nn\n"),
    )?;
    assert_eq!(code(&failed_account), 17, "{failed_account:?}");
    assert_eq!(
        fs::read_link(primary.join("CLAUDE.md"))?,
        rig.home.join("published-instructions")
    );
    assert_eq!(
        fs::read_to_string(rig.state.join("install/receipt.json"))?,
        "published\n"
    );
    assert!(!account.join("CLAUDE.md").exists());
    fs::remove_file(&recorded)?;
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_ACCOUNT_FAILURE");
    rig.env
        .push(("PROVENANT_TEST_APPS_AVAILABLE".into(), "1".into()));
    fs::remove_file(rig.state.join("install/receipt.json"))?;
    let installed_app = rig.home.join("Applications/Provenant Console.app");
    fs::create_dir_all(&installed_app)?;
    fs::write(installed_app.join("old-build"), "preserve this app")?;
    let instructions_before_app_failure = fs::read_to_string(&codex)?;
    let mv = fake_bin.join("mv");
    fs::write(
        &mv,
        format!(
            "#!/bin/sh\ncase \"$1\" in */.provenant-retained.*/*) if [ ! -f '{0}' ]; then touch '{0}'; exit 1; fi ;; esac\nexec /bin/mv \"$@\"\n",
            rig.root.join("direct-install-mv-failed-once").display()
        ),
    )?;
    fs::set_permissions(&mv, fs::Permissions::from_mode(0o755))?;
    let failed_app_swap = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&failed_app_swap), 4, "{failed_app_swap:?}");
    assert!(installed_app.join("old-build").exists());
    assert_eq!(
        fs::read_to_string(&codex)?,
        format!("{instructions_before_app_failure}\nfixture instructions\n")
    );
    assert!(fs::read_dir(rig.home.join("Applications"))?.all(|entry| {
        entry.is_ok_and(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".provenant-retained.")
        })
    }));
    fs::remove_file(&recorded)?;
    fs::remove_file(&mv)?;
    let signal_marker = rig.root.join("swap-signalled");
    rig.env.push((
        "PROVENANT_TEST_SWAP_SIGNAL".into(),
        signal_marker.into_os_string(),
    ));
    let interrupted = rig.execute_os(Path::new("/bin/sh"), &args, None)?;
    assert_eq!(code(&interrupted), 143, "{interrupted:?}");
    assert!(installed_app.join("old-build").exists());
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_SWAP_SIGNAL");
    let agent = rig
        .home
        .join("Library/LaunchAgents/dev.provenant.quota-menubar.plist");
    fs::create_dir_all(agent.parent().ok_or("LaunchAgent parent")?)?;
    fs::write(&agent, "fake LaunchAgent")?;
    let plutil = fake_bin.join("plutil");
    fs::write(&plutil, "#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(&plutil, fs::Permissions::from_mode(0o755))?;
    let launchctl = fake_bin.join("launchctl");
    fs::write(
        &launchctl,
        "#!/bin/sh\n[ \"$1\" != print ] || exit 19\nexit 0\n",
    )?;
    fs::set_permissions(&launchctl, fs::Permissions::from_mode(0o755))?;
    let failed_restart = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&failed_restart), 4, "{failed_restart:?}");
    assert!(installed_app.join("old-build").exists());
    assert!(fs::read_dir(rig.home.join("Applications"))?.all(|entry| {
        entry.is_ok_and(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".provenant-retained.")
        })
    }));
    let cp = fake_bin.join("cp");
    fs::write(
        &cp,
        "#!/bin/sh\ncase \"$3\" in */agent.plist.tmp) printf partial > \"$3\"; exit 1 ;; esac\nexec /bin/cp \"$@\"\n",
    )?;
    fs::set_permissions(&cp, fs::Permissions::from_mode(0o755))?;
    let failed_copy = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&failed_copy), 4, "{failed_copy:?}");
    assert_eq!(fs::read_to_string(&agent)?, "fake LaunchAgent");
    assert!(installed_app.join("old-build").exists());
    fs::write(
        &cp,
        "#!/bin/sh\ncase \"$2\" in */agent.plist) exit 1 ;; esac\nexec /bin/cp \"$@\"\n",
    )?;
    fs::set_permissions(&cp, fs::Permissions::from_mode(0o755))?;
    let failed_restore = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&failed_restore), 4, "{failed_restore:?}");
    assert!(!installed_app.join("old-build").exists());
    assert!(
        String::from_utf8_lossy(&failed_restore.stderr.bytes).contains("agent remains stopped")
    );
    let retained = fs::read_dir(rig.home.join("Applications"))?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".provenant-retained.")
        })
        .ok_or("retained staging directory")?
        .path();
    assert_eq!(
        fs::read_to_string(retained.join("agent.plist"))?,
        "fake LaunchAgent"
    );
    assert!(!agent.exists());
    fs::copy(retained.join("agent.plist"), &agent)?;
    fs::remove_file(cp)?;
    fs::remove_dir_all(&installed_app)?;
    fs::rename(retained.join("Provenant Console.app"), &installed_app)?;
    fs::remove_dir_all(retained)?;
    fs::write(
        &plutil,
        "#!/bin/sh\ncase \"$1\" in -replace) printf '%s' \"$4\" > \"$5\" ;; -extract) cat \"$6\" ;; esac\n",
    )?;
    fs::write(&launchctl, "#!/bin/sh\nexit 0\n")?;
    // A retained legacy bundle must not look like an active wrapper stage.
    for binary in [
        "Contents/MacOS/provenant-console",
        "Contents/MacOS/quota-menubar",
    ] {
        let path = installed_app.join(binary);
        fs::create_dir_all(path.parent().ok_or("legacy executable parent")?)?;
        fs::write(&path, APP_FIXTURE)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let rm = fake_bin.join("rm");
    fs::write(
        &rm,
        "#!/bin/sh\ncase \"$2\" in */.provenant-retained.*/Provenant\\ Console.app) exit 1 ;; esac\nexec /bin/rm \"$@\"\n",
    )?;
    fs::set_permissions(&rm, fs::Permissions::from_mode(0o755))?;
    let retained_backup = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&retained_backup), 0, "{retained_backup:?}");
    assert!(String::from_utf8_lossy(&retained_backup.stderr.bytes).contains("staging remains"));
    assert!(!installed_app.join("old-build").exists());
    assert!(fs::read_to_string(&agent)?.contains("quota-menubar"));
    let retained = fs::read_dir(rig.home.join("Applications"))?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".provenant-retained.")
        })
        .ok_or("retained staging directory")?
        .path();
    assert_eq!(
        fs::read_to_string(retained.join("agent.plist"))?,
        "fake LaunchAgent"
    );
    assert!(retained.join("Provenant Console.app/old-build").exists());
    fs::remove_file(rm)?;
    fs::write(&agent, "legacy agent")?;
    let next_upgrade = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&next_upgrade), 0, "{next_upgrade:?}");
    assert!(fs::read_to_string(&agent)?.contains("Contents/Library/LoginItems/Provenant Menu.app"));
    fs::remove_dir_all(retained)?;
    fs::remove_file(&recorded)?;
    // A failed install whose swap-back and plist quarantine both fail must still
    // report the retained recovery copies and leave the agent stopped.
    fs::write(&agent, "legacy agent")?;
    fs::write(installed_app.join("old-build"), "preserve this app")?;
    let restart_failed = rig.root.join("restart-failed");
    let booted_out = rig.root.join("booted-out");
    fs::write(
        &launchctl,
        format!(
            "#!/bin/sh\ncase \"$1\" in print) touch '{}'; exit 19 ;; bootout) touch '{}' ;; esac\nexit 0\n",
            restart_failed.display(),
            booted_out.display()
        ),
    )?;
    let mv = fake_bin.join("mv");
    let rm = fake_bin.join("rm");
    fs::write(
        &mv,
        format!(
            "#!/bin/sh\ncase \"$1\" in */.provenant-retained.*/*) [ ! -f '{}' ] || exit 1 ;; esac\ncase \"$2\" in */LaunchAgents/*.plist) exit 1 ;; esac\nexec /bin/mv \"$@\"\n",
            restart_failed.display()
        ),
    )?;
    fs::write(
        &rm,
        "#!/bin/sh\ncase \"$2\" in */LaunchAgents/*.plist) exit 1 ;; esac\nexec /bin/rm \"$@\"\n",
    )?;
    for tool in [&mv, &rm] {
        fs::set_permissions(tool, fs::Permissions::from_mode(0o755))?;
    }
    let unquarantined = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&unquarantined), 4, "{unquarantined:?}");
    let message = String::from_utf8_lossy(&unquarantined.stderr.bytes);
    let retained = fs::read_dir(rig.home.join("Applications"))?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".provenant-retained.")
        })
        .ok_or("retained staging directory")?
        .path();
    assert!(
        message.contains("Could not restore the previous Provenant app; agent remains stopped")
    );
    assert!(message.contains(&format!(
        "previous app {}",
        retained.join("Provenant Console.app").display()
    )));
    assert!(message.contains(&format!(
        "previous LaunchAgent plist {}",
        retained.join("agent.plist").display()
    )));
    assert!(message.contains(&format!("rm -f '{}'", agent.display())));
    assert!(booted_out.exists());
    assert_eq!(
        fs::read_to_string(retained.join("agent.plist"))?,
        "legacy agent"
    );
    assert!(retained.join("Provenant Console.app/old-build").exists());
    assert_eq!(fs::read_to_string(&agent)?, "legacy agent");
    // The remove hint precedes the restore instructions.
    assert!(
        message.find("remove it with: rm -f").ok_or("hint")?
            < message.find("Before bootstrapping").ok_or("restore")?
    );
    fs::remove_file(&rm)?;
    fs::write(&launchctl, "#!/bin/sh\nexit 0\n")?;
    fs::remove_dir_all(retained)?;
    fs::remove_file(&recorded)?;
    // If the plist backup copy fails after the swap and the swap-back fails too,
    // the live plist is the only copy and must survive even when it cannot be moved.
    fs::write(&agent, "only copy")?;
    fs::write(installed_app.join("old-build"), "preserve this app")?;
    let backup_failed = rig.root.join("backup-copy-failed");
    let cp = fake_bin.join("cp");
    fs::write(
        &cp,
        format!(
            "#!/bin/sh\ncase \"$3\" in */agent.plist.tmp) touch '{}'; exit 1 ;; esac\nexec /bin/cp \"$@\"\n",
            backup_failed.display()
        ),
    )?;
    fs::write(
        &mv,
        format!(
            "#!/bin/sh\ncase \"$1\" in */.provenant-retained.*/*) [ ! -f '{}' ] || exit 1 ;; esac\ncase \"$2\" in */LaunchAgents/*.plist) exit 1 ;; esac\nexec /bin/mv \"$@\"\n",
            backup_failed.display()
        ),
    )?;
    fs::set_permissions(&cp, fs::Permissions::from_mode(0o755))?;
    let no_backup = rig.execute_os(
        Path::new("/bin/sh"),
        &guided_args,
        Some(b"n\ny\nn\nn\nn\nn\nappend\n"),
    )?;
    assert_eq!(code(&no_backup), 4, "{no_backup:?}");
    let message = String::from_utf8_lossy(&no_backup.stderr.bytes);
    let retained = fs::read_dir(rig.home.join("Applications"))?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".provenant-retained.")
        })
        .ok_or("retained staging directory")?
        .path();
    assert!(message.contains("left in place because no backup of it exists"));
    assert!(!message.contains("previous LaunchAgent plist"));
    assert!(retained.join("Provenant Console.app/old-build").exists());
    assert_eq!(fs::read_to_string(&agent)?, "only copy");
    fs::remove_file(&mv)?;
    fs::remove_file(&cp)?;
    fs::remove_dir_all(retained)?;
    fs::remove_file(&recorded)?;
    fs::remove_file(agent)?;
    fs::remove_file(plutil)?;
    fs::remove_file(launchctl)?;
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_APPS_AVAILABLE");
    fs::write(
        fixtures.join("checksum"),
        format!("{}  package.tar.gz\n", "0".repeat(64)),
    )?;
    let result = rig.execute_os(Path::new("/bin/sh"), &args, None)?;
    assert_eq!(code(&result), 4);
    assert!(!recorded.exists());
    Ok(())
}

#[test]
fn bootstrap_source_build_requires_consent_and_cleans_its_target() -> Result {
    let mut rig = TestRig::new()?;
    let source = rig.root.join("source");
    fs::create_dir_all(source.join("tools/front-door"))?;
    fs::write(
        source.join("tools/front-door/rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.1\"\n",
    )?;
    let fake_bin = rig.root.join("fake-bin");
    fs::create_dir(&fake_bin)?;
    let record = rig.root.join("build-target");
    for (name, content) in [
        (
            "rustup",
            "#!/bin/sh\nif IFS= read -r unexpected; then exit 91; fi\n[ \"$1\" != run ]\n"
                .to_owned(),
        ),
        (
            "cargo",
            format!(
                r#"#!/bin/sh
[ "$CARGO_TARGET_DIR" = "$CARGO_BUILD_BUILD_DIR" ] || exit 90
if IFS= read -r unexpected; then exit 91; fi
printf '%s\n' "$CARGO_TARGET_DIR" > '{}'
[ "$1" = run ] || exit 0
shift
[ "$1" = --release ] || exit 92
shift
[ "$1" = --locked ] || exit 92
shift
[ "$1" = --bin ] || exit 92
shift
[ "$1" = package-release ] || exit 92
shift
[ "$1" = -- ] || exit 92
while [ "$1" != --out ]; do shift; done
mkdir -p "$2/bin"
printf '#!/bin/sh\nexit 0\n' > "$2/bin/provenant"
chmod +x "$2/bin/provenant"
"#,
                record.display()
            ),
        ),
    ] {
        let file = fake_bin.join(name);
        fs::write(&file, content)?;
        fs::set_permissions(file, fs::Permissions::from_mode(0o755))?;
    }
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = format!("{}:/usr/bin:/bin", fake_bin.display()).into();
        }
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh");
    let mut args = vec![
        script.into_os_string(),
        "--source".into(),
        source.into_os_string(),
    ];
    let declined = rig.execute_os(Path::new("/bin/sh"), &args, None)?;
    assert_eq!(code(&declined), 3);
    assert!(!record.exists());
    args.push("--yes".into());
    let mut input = fs::read(&args[0])?;
    input.extend_from_slice(b"printf '%s\\n' SOURCE_COMPLETE\n");
    let mut piped_args = vec!["-s".into(), "--".into()];
    piped_args.extend_from_slice(&args[1..]);
    let built = rig.execute_os(Path::new("/bin/sh"), &piped_args, Some(&input))?;
    assert_eq!(
        code(&built),
        0,
        "{}",
        String::from_utf8_lossy(&built.stderr.bytes)
    );
    let target = fs::read_to_string(record)?;
    assert_eq!(built.stdout.bytes, b"SOURCE_COMPLETE\n");
    assert!(
        !Path::new(target.trim())
            .parent()
            .ok_or("target parent")?
            .exists()
    );
    Ok(())
}

#[test]
fn upgrade_requires_a_receipt_before_download_or_build() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let result = rig.command(&["upgrade", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&result.stdout.bytes)?;
    assert_eq!(code(&result), 2, "{value}");
    assert_eq!(value["command"], "upgrade");
    assert_eq!(value["error"]["code"], "UPGRADE_RECEIPT");
    assert!(!rig.state.exists());
    Ok(())
}

#[test]
fn upgrade_pins_one_release_replays_settings_and_reports_peer_failure() -> Result {
    let mut rig = TestRig::new()?;
    let first = staged(&rig)?;
    let second = staged_named(&rig, "next-release", b"upgraded release")?;
    let installed = rig.command(&[
        "install",
        "--source",
        first.to_str().ok_or("stage")?,
        "--harness",
        "codex",
        "--json",
    ])?;
    assert_eq!(code(&installed), 0);
    let archive = rig.root.join("package.tar.gz");
    let tar = rig.execute_os(
        Path::new("/usr/bin/tar"),
        &[
            "-czf".into(),
            archive.clone().into(),
            "-C".into(),
            second.into(),
            ".".into(),
        ],
        None,
    )?;
    assert_eq!(code(&tar), 0);
    let hash = rig.execute_os(
        Path::new("/usr/bin/shasum"),
        &["-a".into(), "256".into(), archive.clone().into()],
        None,
    )?;
    let digest = String::from_utf8(hash.stdout.bytes)?
        .split_whitespace()
        .next()
        .ok_or("digest")?
        .to_owned();
    let apps = rig.root.join("apps");
    for binary in APP_EXECUTABLES {
        let path = apps.join("Provenant Console.app").join(binary);
        fs::create_dir_all(path.parent().ok_or("app executable parent")?)?;
        fs::write(&path, APP_FIXTURE)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let apps_archive = rig.root.join("apps.tar.gz");
    let tar = rig.execute_os(
        Path::new("/usr/bin/tar"),
        &[
            "-czf".into(),
            apps_archive.clone().into(),
            "-C".into(),
            apps.into(),
            ".".into(),
        ],
        None,
    )?;
    assert_eq!(code(&tar), 0);
    let hash = rig.execute_os(
        Path::new("/usr/bin/shasum"),
        &["-a".into(), "256".into(), apps_archive.clone().into()],
        None,
    )?;
    let apps_digest = String::from_utf8(hash.stdout.bytes)?
        .split_whitespace()
        .next()
        .ok_or("apps digest")?
        .to_owned();
    let invalid_apps_archive = rig.root.join("invalid-apps.tar.gz");
    fs::write(&invalid_apps_archive, b"not a gzip archive")?;
    let hash = rig.execute_os(
        Path::new("/usr/bin/shasum"),
        &[
            "-a".into(),
            "256".into(),
            invalid_apps_archive.clone().into(),
        ],
        None,
    )?;
    let invalid_apps_digest = String::from_utf8(hash.stdout.bytes)?
        .split_whitespace()
        .next()
        .ok_or("invalid archive digest")?
        .to_owned();
    let incomplete_apps = rig.root.join("incomplete-apps");
    let incomplete_binary =
        incomplete_apps.join("Provenant Console.app/Contents/MacOS/provenant-console");
    fs::create_dir_all(incomplete_binary.parent().ok_or("incomplete app parent")?)?;
    fs::write(&incomplete_binary, "#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(&incomplete_binary, fs::Permissions::from_mode(0o755))?;
    let incomplete_apps_archive = rig.root.join("incomplete-apps.tar.gz");
    let tar = rig.execute_os(
        Path::new("/usr/bin/tar"),
        &[
            "-czf".into(),
            incomplete_apps_archive.clone().into(),
            "-C".into(),
            incomplete_apps.into(),
            ".".into(),
        ],
        None,
    )?;
    assert_eq!(code(&tar), 0);
    let hash = rig.execute_os(
        Path::new("/usr/bin/shasum"),
        &[
            "-a".into(),
            "256".into(),
            incomplete_apps_archive.clone().into(),
        ],
        None,
    )?;
    let incomplete_apps_digest = String::from_utf8(hash.stdout.bytes)?
        .split_whitespace()
        .next()
        .ok_or("incomplete app digest")?
        .to_owned();
    let invalid_apps_archive_path = invalid_apps_archive.display().to_string();
    let incomplete_apps_archive_path = incomplete_apps_archive.display().to_string();
    let home = rig
        .env
        .iter()
        .find(|(key, _)| key == "HOME")
        .ok_or("HOME")?
        .1
        .clone();
    let home = PathBuf::from(home);
    let installed_app = home.join("Applications/Provenant Console.app");
    fs::create_dir_all(&installed_app)?;
    fs::write(installed_app.join("old-build"), "stale")?;
    let agent = home.join("Library/LaunchAgents/dev.provenant.quota-menubar.plist");
    fs::create_dir_all(agent.parent().ok_or("agent parent")?)?;
    if cfg!(target_os = "macos") {
        fs::write(
            &agent,
            r#"<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>dev.provenant.quota-menubar</string><key>ProgramArguments</key><array><string>/old/build/quota-menubar</string><string>--kept</string></array></dict></plist>"#,
        )?;
    }
    let fake_bin = rig.root.join("fake-bin");
    fs::create_dir(&fake_bin)?;
    let launch_log = rig.root.join("launchctl-log");
    let launchctl = fake_bin.join("launchctl");
    fs::write(
        &launchctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{0}'\nif [ \"$1\" = bootstrap ] && [ -n \"${{PROVENANT_TEST_BOOTSTRAP_FAIL:-}}\" ]; then exit 5; fi\nif [ \"$1\" = bootstrap ] && [ ! -f '{0}.bootstrapped' ]; then touch '{0}.bootstrapped'; exit 5; fi\n",
            launch_log.display(),
        ),
    )?;
    fs::set_permissions(launchctl, fs::Permissions::from_mode(0o755))?;
    let ssh = fake_bin.join("ssh");
    let ssh_args = rig.root.join("ssh-args");
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprintf '%s\n' \"$@\" > '{}'\ncat >/dev/null\ncase \"$*\" in *--preflight-only*) [ -z \"${{PROVENANT_TEST_PEER_REFUSE:-}}\" ] && exit 0 ;; esac\nexit 17\n",
            ssh_args.display()
        ),
    )?;
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755))?;
    fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric]\nssh_executable={:?}\n[domains.fabric.peers.worker]\nhost='{}'\ndestination='worker.example'\n",
            ssh.to_str().ok_or("ssh")?,
            provenant_core::HostId::from_bytes([1; 16])
        ),
    )?;
    let curl = fake_bin.join("curl");
    let curl_log = rig.root.join("curl-log");
    fs::write(
        &curl,
        format!(
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -w) format=$2; shift 2 ;;
    *) url=$1; shift ;;
  esac
done
printf '%s\n' "$url" >> '{2}'
case "$url" in
  */releases/latest) printf '%s\n' '{{"tag_name":"v9.8.7"}}' ;;
  */download/v9.8.7/provenant-apps-aarch64-apple-darwin.tar.gz.sha256)
    [ "${{PROVENANT_TEST_APPS_CHECKSUM_FAIL:-}}" != 1 ] || exit 22
    checksum=${{PROVENANT_TEST_APPS_CHECKSUM:-{apps_digest}}}
    [ "${{PROVENANT_TEST_APPS_BAD_TAR:-}}" != 1 ] || checksum='{invalid_apps_digest}'
    [ "${{PROVENANT_TEST_APPS_INCOMPLETE:-}}" != 1 ] || checksum='{incomplete_apps_digest}'
    printf '%s\n' "$checksum" > "$out" ;;
  */download/v9.8.7/provenant-apps-aarch64-apple-darwin.tar.gz)
    if [ "${{PROVENANT_TEST_MISSING_APPS:-}}" = 1 ]; then printf '%s' 404; exit 0; fi
    if [ -n "${{PROVENANT_TEST_APPS_STATUS:-}}" ]; then printf '%s' "$PROVENANT_TEST_APPS_STATUS"; exit 0; fi
    [ "${{PROVENANT_TEST_APPS_BAD_TAR:-}}" != 1 ] || {{ cp '{invalid_apps_archive_path}' "$out"; printf '%s' 200; exit 0; }}
    [ "${{PROVENANT_TEST_APPS_INCOMPLETE:-}}" != 1 ] || {{ cp '{incomplete_apps_archive_path}' "$out"; printf '%s' 200; exit 0; }}
    cp '{}' "$out"
    [ -z "${{format:-}}" ] || printf '%s' 200 ;;
  */download/v9.8.7/*.sha256) printf '%s  package.tar.gz\n' "${{PROVENANT_TEST_CHECKSUM:-{digest}}}" > "$out" ;;
  */download/v9.8.7/*.tar.gz) cp '{}' "$out"; [ -z "${{format:-}}" ] || printf '%s' 200 ;;
  *) exit 99 ;;
esac
"#,
            apps_archive.display(),
            archive.display(),
            curl_log.display()
        ),
    )?;
    fs::set_permissions(curl, fs::Permissions::from_mode(0o755))?;
    fake_supported_uname(&fake_bin)?;
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = format!("{}:/usr/bin:/bin", fake_bin.display()).into();
        }
    }
    rig.env
        .push(("PROVENANT_TEST_CHECKSUM".into(), "0".repeat(64).into()));
    let rejected = rig.command(&["upgrade", "--hosts", "all", "--json"])?;
    let rejected_value: serde_json::Value = serde_json::from_slice(&rejected.stdout.bytes)?;
    assert_ne!(code(&rejected), 0, "{rejected_value}");
    assert_eq!(
        rejected_value["data"][0]["success"], false,
        "{rejected_value}"
    );
    assert!(
        rejected_value["data"][0]["output"]
            .as_str()
            .ok_or("output")?
            .contains("Release checksum mismatch"),
        "{rejected_value}"
    );
    let before: provenant_records::InstallReceipt = provenant_records::decode_v1(
        fs::read(rig.state.join("install/receipt.json"))?.as_slice(),
        provenant_records::Limits::CONFIG,
    )?;
    assert_eq!(
        before.source_digest,
        Release::read(&first, env!("PROVENANT_TARGET_TRIPLE"))?
            .manifest()
            .source_digest
    );
    rig.env.retain(|(key, _)| key != "PROVENANT_TEST_CHECKSUM");
    rig.env
        .push(("PROVENANT_TEST_APPS_CHECKSUM".into(), "0".repeat(64).into()));
    let rejected = rig.command(&["upgrade", "--json"])?;
    let rejected_value: serde_json::Value = serde_json::from_slice(&rejected.stdout.bytes)?;
    assert_eq!(rejected_value["data"][0]["success"], false);
    assert!(
        rejected_value["data"][0]["output"]
            .as_str()
            .ok_or("output")?
            .contains("Bundled app checksum mismatch")
    );
    assert!(installed_app.join("old-build").exists());
    assert!(!launch_log.exists());
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_APPS_CHECKSUM");
    rig.env
        .push(("PROVENANT_TEST_APPS_STATUS".into(), "503".into()));
    let unavailable = rig.command(&["upgrade", "--json"])?;
    let unavailable_value: serde_json::Value = serde_json::from_slice(&unavailable.stdout.bytes)?;
    assert_eq!(unavailable_value["data"][0]["success"], false);
    assert!(
        unavailable_value["data"][0]["output"]
            .as_str()
            .unwrap()
            .contains("HTTP 503")
    );
    assert!(installed_app.join("old-build").exists());
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_APPS_STATUS");
    rig.env
        .push(("PROVENANT_TEST_APPS_CHECKSUM_FAIL".into(), "1".into()));
    let checksum_failure = rig.command(&["upgrade", "--json"])?;
    let checksum_failure_value: serde_json::Value =
        serde_json::from_slice(&checksum_failure.stdout.bytes)?;
    assert_eq!(checksum_failure_value["data"][0]["success"], false);
    assert!(
        checksum_failure_value["data"][0]["output"]
            .as_str()
            .unwrap()
            .contains("Cannot download bundled app checksum")
    );
    assert!(installed_app.join("old-build").exists());
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_APPS_CHECKSUM_FAIL");
    rig.env
        .push(("PROVENANT_TEST_APPS_BAD_TAR".into(), "1".into()));
    let bad_tar = rig.command(&["upgrade", "--json"])?;
    let bad_tar_value: serde_json::Value = serde_json::from_slice(&bad_tar.stdout.bytes)?;
    assert_eq!(bad_tar_value["data"][0]["success"], false);
    assert!(
        bad_tar_value["data"][0]["output"]
            .as_str()
            .unwrap()
            .contains("Cannot unpack bundled app archive")
    );
    assert!(installed_app.join("old-build").exists());
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_APPS_BAD_TAR");
    rig.env
        .push(("PROVENANT_TEST_APPS_INCOMPLETE".into(), "1".into()));
    let incomplete = rig.command(&["upgrade", "--json"])?;
    let incomplete_value: serde_json::Value = serde_json::from_slice(&incomplete.stdout.bytes)?;
    assert_eq!(incomplete_value["data"][0]["success"], false);
    assert!(
        incomplete_value["data"][0]["output"]
            .as_str()
            .unwrap()
            .contains("executable is missing")
    );
    assert!(installed_app.join("old-build").exists());
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_APPS_INCOMPLETE");
    let mv = fake_bin.join("mv");
    fs::write(
        &mv,
        format!(
            "#!/bin/sh\ncase \"$1\" in */.provenant-retained.*/*) if [ ! -f '{0}' ]; then touch '{0}'; exit 1; fi ;; esac\nexec /bin/mv \"$@\"\n",
            rig.root.join("mv-failed-once").display()
        ),
    )?;
    fs::set_permissions(&mv, fs::Permissions::from_mode(0o755))?;
    let failed_swap = rig.command(&["upgrade", "--json"])?;
    let failed_swap_value: serde_json::Value = serde_json::from_slice(&failed_swap.stdout.bytes)?;
    assert_eq!(failed_swap_value["data"][0]["success"], false);
    assert!(installed_app.join("old-build").exists());
    let committed_after_app_failure: provenant_records::InstallReceipt =
        provenant_records::decode_v1(
            fs::read(rig.state.join("install/receipt.json"))?.as_slice(),
            provenant_records::Limits::CONFIG,
        )?;
    assert_ne!(
        committed_after_app_failure.source_digest,
        before.source_digest
    );
    assert!(fs::read_dir(home.join("Applications"))?.all(|entry| {
        entry.is_ok_and(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".provenant-retained.")
        })
    }));
    fs::remove_file(mv)?;
    if cfg!(target_os = "macos") {
        // A failed agent restart rolls back both the app and the migrated agent plist.
        let before_plist = fs::read_to_string(&agent)?;
        rig.env
            .push(("PROVENANT_TEST_BOOTSTRAP_FAIL".into(), "1".into()));
        let failed = rig.command(&["upgrade", "--json"])?;
        let failed_value: serde_json::Value = serde_json::from_slice(&failed.stdout.bytes)?;
        assert_eq!(failed_value["data"][0]["success"], false, "{failed_value}");
        rig.env
            .retain(|(key, _)| key != "PROVENANT_TEST_BOOTSTRAP_FAIL");
        assert_eq!(fs::read_to_string(&agent)?, before_plist);
        assert!(installed_app.join("old-build").exists());
        assert!(!installed_app.join(MENU_EXECUTABLE).exists());
    }
    fs::write(&curl_log, "")?;
    rig.env
        .push(("PROVENANT_TEST_PEER_REFUSE".into(), "1".into()));
    let before_peer_refusal = snapshot(&rig.state)?;
    let result = rig.command(&["upgrade", "--hosts", "all", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&result.stdout.bytes)?;
    assert_eq!(value["data"][0]["host"], "worker", "{value}");
    assert_eq!(value["data"][0]["success"], false);
    assert_ne!(code(&result), 0);
    assert_eq!(snapshot(&rig.state)?, before_peer_refusal);
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_PEER_REFUSE");
    fs::write(&curl_log, "")?;
    let result = rig.command(&["upgrade", "--hosts", "all", "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&result.stdout.bytes)?;
    assert_eq!(value["data"][0]["host"], "local", "{value}");
    assert_eq!(value["data"][0]["success"], true, "{value}");
    assert_eq!(value["data"][1]["host"], "worker");
    assert_eq!(value["data"][1]["success"], false);
    assert_ne!(code(&result), 0);
    let arguments = fs::read_to_string(ssh_args)?;
    assert!(arguments.contains("worker.example\n"));
    assert!(arguments.contains("'--tag' 'v9.8.7'"));
    assert!(arguments.contains("-oBatchMode=yes"));
    let downloads = fs::read_to_string(&curl_log)?;
    // Preflight and apply may each fetch artifacts. Every fetch must stay
    // pinned, and the app archive must always have its checksum available.
    let artifacts: Vec<_> = downloads
        .lines()
        .filter(|url| url.contains("/download/"))
        .collect();
    assert!(!artifacts.is_empty());
    assert!(
        artifacts
            .iter()
            .all(|url| url.contains("/download/v9.8.7/"))
    );
    for artifact in [
        "provenant-apps-aarch64-apple-darwin.tar.gz",
        "provenant-apps-aarch64-apple-darwin.tar.gz.sha256",
    ] {
        assert!(artifacts.iter().any(|url| url.ends_with(artifact)));
    }
    assert!(!installed_app.join("old-build").exists());
    assert!(
        installed_app
            .join("Contents/MacOS/provenant-console")
            .is_file()
    );
    assert!(installed_app.join(MENU_EXECUTABLE).is_file());
    if cfg!(target_os = "macos") {
        let plist = fs::read_to_string(&agent)?;
        let agent_path = agent.to_str().ok_or("agent")?;
        assert!(
            plist.contains(
                installed_app
                    .join(MENU_EXECUTABLE)
                    .to_str()
                    .ok_or("agent path")?
            )
        );
        assert!(plist.contains("--kept"));
        let launches = fs::read_to_string(&launch_log)?;
        assert!(launches.contains("bootout gui/"));
        assert!(launches.contains("bootstrap gui/"));
        assert!(
            launches
                .lines()
                .filter(|line| line.starts_with("bootstrap gui/") && line.contains(agent_path))
                .count()
                >= 2
        );
        assert!(launches.contains("kickstart -k gui/"));
        assert!(launches.contains(agent_path));
    }
    rig.env
        .push(("PROVENANT_TEST_MISSING_APPS".into(), "1".into()));
    let older = rig.command(&["upgrade", "--json"])?;
    let older_value: serde_json::Value = serde_json::from_slice(&older.stdout.bytes)?;
    assert_eq!(older_value["data"][0]["success"], true, "{older_value}");
    assert!(String::from_utf8_lossy(&older.stdout.bytes).contains("no bundled app archive"));
    assert!(installed_app.join(MENU_EXECUTABLE).is_file());
    if cfg!(target_os = "macos") {
        // A repeated upgrade leaves the already-migrated agent pointing at the same helper.
        let again = fs::read_to_string(&agent)?;
        assert_eq!(again.matches(MENU_EXECUTABLE).count(), 1, "{again}");
    }
    rig.env
        .retain(|(key, _)| key != "PROVENANT_TEST_MISSING_APPS");
    let receipt: provenant_records::InstallReceipt = provenant_records::decode_v1(
        fs::read(rig.state.join("install/receipt.json"))?.as_slice(),
        provenant_records::Limits::CONFIG,
    )?;
    assert_eq!(receipt.harnesses, vec![provenant_records::Harness::Codex]);
    assert_eq!(
        receipt.harness_targets[0].skills_dir.to_path_buf()?,
        rig.root.join("codex/skills")
    );
    assert_ne!(
        receipt.source_digest,
        provenant_config::Release::read(&first, env!("PROVENANT_TARGET_TRIPLE"))?
            .manifest()
            .source_digest
    );
    Ok(())
}

#[test]
fn source_upgrade_builds_tracked_files_on_each_host() -> Result {
    let mut rig = TestRig::new()?;
    let peer = TestRig::new()?;
    let first = staged(&rig)?;
    let second = staged_named(&rig, "next-release", b"source upgrade")?;
    for host in [&rig, &peer] {
        let installed = host.command(&[
            "install",
            "--source",
            first.to_str().ok_or("stage")?,
            "--harness",
            "codex",
            "--json",
        ])?;
        assert_eq!(code(&installed), 0);
    }
    let source = rig.root.join("source");
    fs::create_dir_all(source.join("tools/front-door"))?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh"),
        source.join("install.sh"),
    )?;
    fs::write(
        source.join("tools/front-door/rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.1\"\n",
    )?;
    for command in [vec!["init", "-q"], vec!["add", "."]] {
        let mut args = vec!["-C", source.to_str().ok_or("source")?];
        args.extend(command);
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let result = rig.execute(&git, &args, None)?;
        assert_eq!(
            code(&result),
            0,
            "{}",
            String::from_utf8_lossy(&result.stderr.bytes)
        );
    }
    fs::write(source.join("untracked-canary"), "do not transfer")?;
    let fake_bin = rig.root.join("fake-bin");
    fs::create_dir(&fake_bin)?;
    let archive = rig.root.join("sent-source.tar.gz");
    std::os::unix::fs::symlink(
        provenant_os::git::git_executable().ok_or("git")?,
        fake_bin.join("git"),
    )?;
    for (name, content) in [
        ("rustup", "#!/bin/sh\nexit 0\n".to_owned()),
        (
            "cargo",
            format!(
                "#!/bin/sh\n[ \"$1\" = run ] || exit 0\nwhile [ \"$1\" != --out ]; do shift; done\ncp -Rp '{}' \"$2\"\n",
                second.display()
            ),
        ),
        (
            "ssh",
            format!(
                r#"#!/bin/sh
for last do :; done
cat > '{}'
exec env HOME='{}' PROVENANT_STATE_DIR='{}' CODEX_HOME='{}' CLAUDE_CONFIG_DIR='{}' /bin/sh -c "$last" < '{}'
"#,
                archive.display(),
                peer.home.display(),
                peer.state.display(),
                peer.root.join("codex").display(),
                peer.root.join("claude").display(),
                archive.display()
            ),
        ),
    ] {
        let file = fake_bin.join(name);
        fs::write(&file, content)?;
        fs::set_permissions(file, fs::Permissions::from_mode(0o755))?;
    }
    fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric]\nssh_executable={:?}\n[domains.fabric.peers.worker]\nhost='{}'\ndestination='worker.example'\n",
            fake_bin.join("ssh").to_str().ok_or("ssh")?,
            provenant_core::HostId::from_bytes([1; 16])
        ),
    )?;
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = format!("{}:/usr/bin:/bin", fake_bin.display()).into();
        }
    }
    let upgraded = rig.command(&[
        "upgrade",
        "--from-source",
        source.to_str().ok_or("source")?,
        "--json",
    ])?;
    let value: serde_json::Value = serde_json::from_slice(&upgraded.stdout.bytes)?;
    assert_eq!(code(&upgraded), 0, "{value}");
    assert_eq!(value["data"][0]["success"], true);
    assert_eq!(value["data"][1]["success"], true);
    let listing = rig.execute_os(
        Path::new("/usr/bin/tar"),
        &["-tzf".into(), archive.into()],
        None,
    )?;
    let listing = String::from_utf8(listing.stdout.bytes)?;
    assert!(listing.contains("install.sh"));
    assert!(!listing.contains("untracked-canary"));
    for host in [&rig, &peer] {
        let receipt: provenant_records::InstallReceipt = provenant_records::decode_v1(
            fs::read(host.state.join("install/receipt.json"))?.as_slice(),
            provenant_records::Limits::CONFIG,
        )?;
        assert_eq!(
            receipt.source_digest,
            Release::read(&second, env!("PROVENANT_TARGET_TRIPLE"))?
                .manifest()
                .source_digest
        );
        assert_eq!(receipt.harnesses, vec![provenant_records::Harness::Codex]);
    }
    Ok(())
}

#[test]
#[allow(clippy::print_stderr)]
fn install_compacts_routine_output_and_keeps_json_actions_once() -> Result {
    let executable = std::env::current_exe()?;
    let rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    let stage = staged(&rig)?;
    let preview = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &args(&stage, true),
        None,
    )?;
    assert_eq!(code(&preview), 0);
    let mut human_args = args(&stage, true);
    human_args.retain(|arg| arg != "--json");
    let human = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &human_args,
        None,
    )?;
    assert_eq!(code(&human), 0);
    let applied = rig.execute_os(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &args(&stage, false),
        None,
    )?;
    assert_eq!(
        code(&applied),
        0,
        "{}",
        String::from_utf8_lossy(&applied.stderr.bytes)
    );
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout.bytes)?;
    let text = String::from_utf8(human.stdout.bytes.clone())?;
    assert!(
        human.stdout.bytes.len() < preview.stdout.bytes.len(),
        "{text}"
    );
    for line in text.lines() {
        assert!(!line.trim_start().starts_with("action:"), "{line}");
    }
    assert!(
        report["data"]["outcome"]["actions"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(report["data"].get("completed").is_none());
    assert!(report["data"].get("remaining").is_none());
    assert!(report["data"].get("disk_guard_agent").is_none());
    Ok(())
}

#[test]
fn invalid_install_home_names_the_repair() -> Result {
    let mut rig = TestRig::new()?;
    let stage = staged(&rig)?;
    let file = rig.root.join("home-file");
    fs::write(&file, "not a directory")?;
    for invalid in [
        PathBuf::from("relative-home"),
        rig.root.join("missing-home"),
        file,
    ] {
        for (key, value) in &mut rig.env {
            if key == "HOME" {
                *value = invalid.clone().into_os_string();
            }
        }
        let output = rig.execute_os(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &args(&stage, true),
            None,
        )?;
        let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(value["status"], "invalid", "{value}");
        assert_eq!(value["error"]["code"], "HOME", "{value}");
        let repair = value["error"]["repair"].as_str().ok_or("repair")?;
        assert!(
            repair.contains("HOME") && repair.contains("absolute home directory"),
            "{repair}"
        );
    }
    Ok(())
}

fn run_json(rig: &TestRig, args: &[&str]) -> Result<(i64, serde_json::Value)> {
    let mut arguments = args.to_vec();
    arguments.push("--json");
    let output = rig.command(&arguments)?;
    let value = serde_json::from_slice(&output.stdout.bytes).map_err(|error| {
        format!(
            "{args:?}: {error}: {} {}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        )
    })?;
    Ok((code(&output), value))
}

/// The managed hooks of a Claude settings file, ignoring everything user-owned.
fn managed_hooks(settings: &Path) -> Result<serde_json::Value> {
    let document: serde_json::Value = serde_json::from_slice(&fs::read(settings)?)?;
    Ok(document["hooks"].clone())
}

/// A fixture `claude` whose sign-in status names its config directory.
fn fake_claude(rig: &mut TestRig) -> Result {
    let bin = rig.root.join("claude-bin");
    fs::create_dir_all(&bin)?;
    let claude = bin.join("claude");
    fs::write(
        &claude,
        "#!/bin/sh\nprintf '{\"loggedIn\":true,\"email\":\"%s@example.test\"}\\n' \"${CLAUDE_CONFIG_DIR##*/}\"\n",
    )?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    Ok(())
}

#[test]
fn install_and_upgrade_leave_every_registered_account_identical() -> Result {
    let executable = std::env::current_exe()?;
    let rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    let first = staged(&rig)?;
    let second = staged_named(&rig, "new-release", b"new release")?;
    let claude = [
        rig.root.join("claude"),
        rig.home.join(".claude-work"),
        rig.home.join(".claude-pro"),
    ];
    let codex = [rig.root.join("codex"), rig.home.join(".codex-alt")];
    for directory in claude.iter().chain(&codex).skip(1) {
        fs::create_dir_all(directory)?;
    }
    support::register_account(&rig, "claude", "work", "work-key", &claude[1])?;
    support::register_account(&rig, "claude", "pro", "pro-key", &claude[2])?;
    support::register_account(&rig, "codex", "alt", "alt-key", &codex[1])?;
    enable_fabric_mcp(&rig)?;
    // User-owned content and credentials sit beside the managed entries.
    fs::write(
        claude[2].join("settings.json"),
        "{\n  \"model\": \"opus\",\n  \"env\": {\"KEPT\": \"1\"}\n}\n",
    )?;
    fs::write(claude[1].join(".credentials.json"), b"fixture credential")?;

    let installed = rig.command(&[
        "install",
        "--source",
        first.to_str().ok_or("first")?,
        "--harness",
        "claude",
        "--harness",
        "codex",
        "--hooks",
        "on",
        "--json",
    ])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    let identical = |release: &Path| -> Result {
        let reference = managed_hooks(&claude[0].join("settings.json"))?;
        assert!(reference["PreToolUse"].is_array(), "{reference}");
        for directory in &claude {
            let link = fs::read_link(directory.join("skills/example"))?;
            assert!(link.starts_with(release), "{directory:?}: {link:?}");
            assert_eq!(managed_hooks(&directory.join("settings.json"))?, reference);
            let mcp: serde_json::Value =
                serde_json::from_slice(&fs::read(directory.join(".claude.json"))?)?;
            assert_eq!(mcp["mcpServers"]["fabric"]["type"], "stdio");
        }
        for directory in &codex {
            assert!(fs::read_link(directory.join("skills/example"))?.starts_with(release));
            assert!(fs::read_to_string(directory.join("config.toml"))?.contains("fabric"));
        }
        Ok(())
    };
    let release_of = |stage: &Path| -> Result<PathBuf> {
        let digest = Release::read(stage, env!("PROVENANT_TARGET_TRIPLE"))?
            .manifest()
            .source_digest;
        Ok(provenant_config::release_root(&rig.state, digest))
    };
    identical(&release_of(&first)?)?;
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_eq!(status, 0, "{checked}");

    // Without flags, an upgrade reuses the receipt and still reaches every account.
    let upgraded = rig.command(&[
        "install",
        "--source",
        second.to_str().ok_or("second")?,
        "--json",
    ])?;
    assert_eq!(code(&upgraded), 0, "{upgraded:?}");
    identical(&release_of(&second)?)?;
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_eq!(status, 0, "{checked}");

    let kept: serde_json::Value =
        serde_json::from_slice(&fs::read(claude[2].join("settings.json"))?)?;
    assert_eq!(kept["model"], "opus");
    assert_eq!(kept["env"]["KEPT"], "1");
    assert_eq!(
        fs::read(claude[1].join(".credentials.json"))?,
        b"fixture credential"
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn check_install_reports_drift_per_account_and_accounts_sync_repairs_it() -> Result {
    let executable = std::env::current_exe()?;
    let rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    let stage = staged(&rig)?;
    let work = rig.home.join(".claude-work");
    let pro = rig.home.join(".claude-pro");
    for directory in [&work, &pro] {
        fs::create_dir_all(directory)?;
    }
    support::register_account(&rig, "claude", "work", "work-key", &work)?;
    support::register_account(&rig, "claude", "pro", "pro-key", &pro)?;
    let installed = rig.command(&[
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--harness",
        "claude",
        "--hooks",
        "on",
        "--json",
    ])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    let (status, listed) = run_json(&rig, &["accounts", "list"])?;
    assert_eq!(status, 0, "{listed}");
    assert!(
        listed["data"]["sync"]
            .as_array()
            .ok_or("sync")?
            .iter()
            .all(|account| account["state"] == "synced"),
        "{listed}"
    );

    // One account loses a skill link; the other loses its managed hooks.
    fs::remove_file(work.join("skills/example"))?;
    let settings = pro.join("settings.json");
    let hooks = managed_hooks(&settings)?;
    fs::write(&settings, "{\"model\": \"opus\"}\n")?;
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_ne!(status, 0, "{checked}");
    let messages: Vec<_> = checked["data"]["findings"]
        .as_array()
        .ok_or("findings")?
        .iter()
        .filter_map(|finding| finding["message"].as_str())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("account work (claude)")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("account pro (claude)")),
        "{messages:?}"
    );
    let (_, listed) = run_json(&rig, &["accounts", "list"])?;
    let states: Vec<_> = listed["data"]["sync"]
        .as_array()
        .ok_or("sync")?
        .iter()
        .map(|account| {
            (
                account["label"].as_str().unwrap_or(""),
                account["state"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert!(states.contains(&("work", "drift")), "{states:?}");
    assert!(states.contains(&("pro", "drift")), "{states:?}");

    let (status, doctor) = run_json(&rig, &["hosts", "doctor"])?;
    assert_eq!(status, 0, "{doctor}");
    let managed: Vec<_> = doctor["data"]["managed"]
        .as_array()
        .ok_or("managed")?
        .iter()
        .map(|row| {
            (
                row["label"].as_str().unwrap_or(""),
                row["state"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert!(managed.contains(&("work", "drift")), "{managed:?}");
    assert!(managed.contains(&("pro", "drift")), "{managed:?}");

    let (status, preview) = run_json(&rig, &["accounts", "sync", "--dry-run"])?;
    assert_eq!(status, 0, "{preview}");
    assert!(fs::symlink_metadata(work.join("skills/example")).is_err());

    let (status, synced) = run_json(&rig, &["accounts", "sync"])?;
    assert_eq!(status, 0, "{synced}");
    assert!(fs::symlink_metadata(work.join("skills/example")).is_ok());
    // Removing every managed hook is the account's own choice and sticks.
    assert!(hooks["PreToolUse"].is_array());
    assert!(managed_hooks(&settings)?.is_null());
    let repaired: serde_json::Value = serde_json::from_slice(&fs::read(&settings)?)?;
    assert_eq!(repaired["model"], "opus");
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_eq!(status, 0, "{checked}");
    let (_, listed) = run_json(&rig, &["accounts", "list"])?;
    assert!(
        listed["data"]["sync"]
            .as_array()
            .ok_or("sync")?
            .iter()
            .all(|account| account["state"] == "synced"),
        "{listed}"
    );
    let (_, doctor) = run_json(&rig, &["hosts", "doctor"])?;
    assert!(
        doctor["data"]["managed"]
            .as_array()
            .ok_or("managed")?
            .iter()
            .all(|row| row["state"] == "synced"),
        "{doctor}"
    );
    Ok(())
}

#[test]
fn accounts_sync_without_an_install_names_the_missing_receipt() -> Result {
    let rig = TestRig::new()?;
    let (status, value) = run_json(&rig, &["accounts", "sync"])?;
    assert_ne!(status, 0);
    assert_eq!(value["status"], "invalid", "{value}");
    assert_eq!(value["error"]["code"], "SYNC_RECEIPT", "{value}");
    Ok(())
}

#[test]
fn registering_an_account_installs_managed_content_when_an_install_exists() -> Result {
    let executable = std::env::current_exe()?;
    let mut rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    fake_claude(&mut rig)?;
    let stage = staged(&rig)?;
    let early = rig.home.join(".claude-early");
    let late = rig.home.join(".claude-late");
    for directory in [&early, &late] {
        fs::create_dir_all(directory)?;
    }
    let register = |label: &str, directory: &Path| -> Result<serde_json::Value> {
        let (status, value) = run_json(
            &rig,
            &[
                "accounts",
                "register",
                label,
                "--adapter",
                "claude",
                "--directory",
                directory.to_str().ok_or("directory")?,
            ],
        )?;
        assert_eq!(status, 0, "{value}");
        Ok(value)
    };
    // Nothing is installed yet, so registration only records the account.
    let value = register("early", &early)?;
    assert_eq!(
        value["data"]["sync"]["skipped"], "no install receipt",
        "{value}"
    );
    assert!(!early.join("skills").exists());

    let installed = rig.command(&[
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--harness",
        "claude",
        "--hooks",
        "on",
        "--json",
    ])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    assert!(fs::symlink_metadata(early.join("skills/example")).is_ok());

    let value = register("late", &late)?;
    assert!(
        value["data"]["sync"]["changes"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "{value}"
    );
    assert!(fs::symlink_metadata(late.join("skills/example")).is_ok());
    assert_eq!(
        managed_hooks(&late.join("settings.json"))?,
        managed_hooks(&rig.root.join("claude/settings.json"))?
    );
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_eq!(status, 0, "{checked}");
    Ok(())
}

/// Opt the Fabric MCP server in, keeping whatever the config already holds.
fn enable_fabric_mcp(rig: &TestRig) -> Result {
    fs::create_dir_all(&rig.state)?;
    let config = rig.state.join("config.toml");
    let mut doc: toml_edit::DocumentMut = match fs::read_to_string(&config) {
        Ok(text) => text.parse()?,
        Err(_) => "schema_version = 1\n".parse()?,
    };
    doc["knobs"]["fabric.mcp_server"] = toml_edit::value(true);
    fs::write(config, doc.to_string())?;
    Ok(())
}

fn write_accounts_section(rig: &TestRig, edit: impl FnOnce(&mut toml_edit::Table)) -> Result {
    let config = rig.state.join("config.toml");
    let mut doc: toml_edit::DocumentMut = fs::read_to_string(&config)?.parse()?;
    let accounts = doc["domains"]["accounts"]
        .as_table_mut()
        .ok_or("accounts")?;
    edit(accounts);
    fs::write(config, doc.to_string())?;
    Ok(())
}

fn finding_details(value: &serde_json::Value, rule: &str) -> Vec<String> {
    value["data"]["outcome"]["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|finding| finding["rule"] == rule)
        .filter_map(|finding| finding["detail"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn shared_settings_sync_conflict_adopt_force_and_refusal() -> Result {
    let executable = std::env::current_exe()?;
    let rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    let stage = staged(&rig)?;
    let work = rig.home.join(".claude-work");
    let pro = rig.home.join(".claude-pro");
    for directory in [&work, &pro] {
        fs::create_dir_all(directory)?;
    }
    support::register_account(&rig, "claude", "work", "work-key", &work)?;
    support::register_account(&rig, "claude", "pro", "pro-key", &pro)?;
    enable_fabric_mcp(&rig)?;
    let source = rig.root.join("source-settings.json");
    let write_source = |style: &str| -> Result {
        fs::write(
            &source,
            format!(
                "{{\"outputStyle\": \"{style}\", \"permissions\": {{\"allow\": [\"Bash(ls)\"]}}, \"model\": \"opus\"}}\n"
            ),
        )?;
        Ok(())
    };
    write_source("terse")?;
    write_accounts_section(&rig, |accounts| {
        let mut shared = toml_edit::Table::new();
        shared["source"] = toml_edit::value(source.to_str().unwrap_or_default());
        accounts["shared_settings"] = toml_edit::Item::Table(shared);
    })?;
    fs::write(
        pro.join(".claude.json"),
        "{\"oauthAccount\": {\"emailAddress\": \"x@example.test\"}}\n",
    )?;
    let identity_before = fs::read(pro.join(".claude.json"))?;
    let installed = rig.command(&[
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--harness",
        "claude",
        "--json",
    ])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    let style = |directory: &Path| -> Result<serde_json::Value> {
        Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(
            directory.join("settings.json"),
        )?)?)
    };
    for directory in [&work, &pro] {
        let settings = style(directory)?;
        assert_eq!(settings["outputStyle"], "terse");
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls)");
        assert!(
            settings.get("model").is_none(),
            "unlisted keys are not synced"
        );
    }
    // Only the MCP entry is added to `.claude.json`.
    let identity: serde_json::Value = serde_json::from_slice(&fs::read(pro.join(".claude.json"))?)?;
    assert_eq!(identity["oauthAccount"]["emailAddress"], "x@example.test");
    assert_ne!(fs::read(pro.join(".claude.json"))?, identity_before);
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_eq!(status, 0, "{checked}");

    // A local edit survives a source change and is reported by account and key.
    let edited = fs::read_to_string(pro.join("settings.json"))?.replace("terse", "mine");
    fs::write(pro.join("settings.json"), edited)?;
    write_source("formal")?;
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_ne!(status, 0, "{checked}");
    assert!(
        checked.to_string().contains("account pro key outputStyle"),
        "{checked}"
    );
    let (status, synced) = run_json(&rig, &["accounts", "sync"])?;
    assert_eq!(status, 0, "{synced}");
    assert_eq!(
        finding_details(&synced, "settings_conflict"),
        ["account pro key outputStyle"]
    );
    assert_eq!(style(&pro)?["outputStyle"], "mine");
    assert_eq!(style(&work)?["outputStyle"], "formal");
    let human = rig.command(&["accounts", "sync"])?;
    let human = String::from_utf8_lossy(&human.stdout.bytes);
    assert!(
        human.contains("--adopt") && human.contains("--force-settings"),
        "{human}"
    );

    // Adopting pulls the account's value into the source and the other accounts.
    let (status, adopted) = run_json(&rig, &["accounts", "sync", "--adopt", "pro"])?;
    assert_eq!(status, 0, "{adopted}");
    assert!(fs::read_to_string(&source)?.contains("\"outputStyle\": \"mine\""));
    assert_eq!(style(&work)?["outputStyle"], "mine");
    assert!(finding_details(&adopted, "settings_conflict").is_empty());

    // Forcing overwrites the edit with the source's value.
    let edited = fs::read_to_string(pro.join("settings.json"))?.replace("mine", "again");
    fs::write(pro.join("settings.json"), edited)?;
    write_source("final")?;
    let (status, forced) = run_json(&rig, &["accounts", "sync", "--force-settings"])?;
    assert_eq!(status, 0, "{forced}");
    assert_eq!(style(&pro)?["outputStyle"], "final");
    assert_eq!(style(&work)?["outputStyle"], "final");
    let (status, checked) = run_json(&rig, &["check", "install"])?;
    assert_eq!(status, 0, "{checked}");

    // Identity keys are refused, and a configuration defect never blocks removal.
    write_accounts_section(&rig, |accounts| {
        let mut keys = toml_edit::Array::new();
        keys.push("outputStyle");
        keys.push("apiKeyHelper");
        accounts["shared_settings"]["keys"] = toml_edit::value(keys);
    })?;
    let (status, refused) = run_json(&rig, &["accounts", "sync"])?;
    assert_ne!(status, 0, "{refused}");
    assert!(refused.to_string().contains("is never synced"), "{refused}");
    assert_eq!(refused["error"]["code"], "SHARED_SETTINGS_NEVER_SYNCED");
    assert!(
        refused["error"]["repair"]
            .as_str()
            .unwrap()
            .contains("shared settings")
    );
    for command in [&["accounts", "list"][..], &["route", "list"][..]] {
        let (status, refused) = run_json(&rig, command)?;
        assert_ne!(status, 0, "{command:?}: {refused}");
        assert_eq!(
            refused["error"]["code"], "SHARED_SETTINGS_NEVER_SYNCED",
            "{command:?}: {refused}"
        );
        assert!(refused.to_string().contains("is never synced"), "{refused}");
        assert!(
            refused["error"]["repair"]
                .as_str()
                .unwrap()
                .contains("shared settings")
        );
    }
    write_accounts_section(&rig, |accounts| {
        let mut keys = toml_edit::Array::new();
        keys.push("env");
        accounts["shared_settings"]["keys"] = toml_edit::value(keys);
    })?;
    fs::write(
        &source,
        r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"TOKEN_SENTINEL"}}"#,
    )?;
    let (status, refused) = run_json(&rig, &["accounts", "sync"])?;
    assert_ne!(status, 0, "{refused}");
    assert_eq!(refused["error"]["code"], "SHARED_SETTINGS_NEVER_SYNCED");
    assert!(!refused.to_string().contains("TOKEN_SENTINEL"));
    write_accounts_section(&rig, |accounts| {
        let mut keys = toml_edit::Array::new();
        keys.push("apiKeyHelper");
        accounts["shared_settings"]["keys"] = toml_edit::value(keys);
    })?;
    let removed = rig.command(&["uninstall", "--harness", "claude"])?;
    assert_eq!(code(&removed), 0, "{removed:?}");
    // `CLAUDE_CONFIG_DIR` selects the one matching recorded target.
    assert!(fs::symlink_metadata(rig.root.join("claude/skills/example")).is_err());
    assert!(
        String::from_utf8_lossy(&removed.stdout.bytes).contains("configuration ignored")
            || String::from_utf8_lossy(&removed.stderr.bytes).contains("configuration ignored")
    );
    Ok(())
}

#[test]
fn an_unregistered_account_is_orphaned_not_synced_and_prune_removes_it() -> Result {
    let executable = std::env::current_exe()?;
    let rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    let stage = staged(&rig)?;
    let work = rig.home.join(".claude-work");
    let pro = rig.home.join(".claude-pro");
    for directory in [&work, &pro] {
        fs::create_dir_all(directory)?;
    }
    support::register_account(&rig, "claude", "work", "work-key", &work)?;
    support::register_account(&rig, "claude", "pro", "pro-key", &pro)?;
    let installed = rig.command(&[
        "install",
        "--source",
        stage.to_str().ok_or("stage")?,
        "--harness",
        "claude",
        "--hooks",
        "on",
        "--json",
    ])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    write_accounts_section(&rig, |accounts| {
        if let Some(entries) = accounts["entries"].as_table_mut() {
            entries.remove("claude-pro");
        }
    })?;
    fs::remove_file(pro.join("skills/example"))?;
    let (status, synced) = run_json(&rig, &["accounts", "sync"])?;
    assert_eq!(status, 0, "{synced}");
    assert_eq!(
        finding_details_for_path(&synced, "orphaned_account"),
        1,
        "{synced}"
    );
    assert!(
        fs::symlink_metadata(pro.join("skills/example")).is_err(),
        "an orphan is reported, not synced"
    );
    let human = rig.command(&["accounts", "sync"])?;
    assert!(String::from_utf8_lossy(&human.stdout.bytes).contains("--prune-orphans"));

    let pruned = rig.command(&["accounts", "sync", "--prune-orphans"])?;
    assert_eq!(code(&pruned), 0, "{pruned:?}");
    let pruned = String::from_utf8_lossy(&pruned.stdout.bytes).into_owned();
    assert!(pruned.contains("removed managed content from"), "{pruned}");
    assert!(
        !fs::read_to_string(pro.join("settings.json"))?.contains("'hook' 'claude'"),
        "{pruned} {}",
        fs::read_to_string(pro.join("settings.json"))?
    );
    assert!(fs::symlink_metadata(work.join("skills/example")).is_ok());
    assert!(fs::read_to_string(work.join("settings.json"))?.contains("'hook' 'claude'"));
    Ok(())
}

fn finding_details_for_path(value: &serde_json::Value, rule: &str) -> usize {
    value["data"]["outcome"]["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|finding| finding["rule"] == rule)
        .count()
}

fn snapshot(root: &Path) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name != "usage.jsonl") {
                // usage.jsonl is the per-command telemetry every verb appends to.
                files.insert(path.clone(), fs::read(&path)?);
            }
        }
    }
    Ok(files)
}

fn git(rig: &TestRig, directory: &Path, args: &[&str]) -> Result {
    let mut argv: Vec<std::ffi::OsString> = vec![
        "-C".into(),
        directory.into(),
        "-c".into(),
        "user.name=t".into(),
        "-c".into(),
        "user.email=t@example.com".into(),
    ];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let output = rig.execute_os(&git, &argv, None)?;
    assert_eq!(code(&output), 0, "{args:?}: {output:?}");
    Ok(())
}

#[test]
fn format_one_state_is_reset_only_on_acknowledgement() -> Result {
    let executable = std::env::current_exe()?;
    let rig = TestRig::in_directory(executable.parent().ok_or("test executable parent")?)?;
    let stage = staged(&rig)?;
    let source = stage.to_str().ok_or("stage")?;
    let state = &rig.state;
    let dropped = [
        "run.sqlite3",
        "run.sqlite3-wal",
        "run.sqlite3-shm",
        "run.sqlite3-journal",
        "runs/r1/attempts/a1/tmp/scratch",
        "attempts/base/tmp/scratch",
        "children/c1/attempts/detached/tmp/scratch",
        "children/c1/run.sqlite3",
        "admission.sqlite3",
        "fabric/fabric.sqlite3",
        "fabric/fabric.sqlite3-wal",
        "fabric/fabric.sqlite3-shm",
        "fabric/fabric.sqlite3-journal",
        "fabric/host.sqlite3",
        "fabric/host.sqlite3-wal",
        "fabric/host.sqlite3-shm",
        "fabric/graphs/p/g.json",
        "work-index.sqlite3",
        "pr-contributions.sqlite3",
    ];
    let kept = [
        "quota/history/work.claude.jsonl",
        "provider-history/existing.jsonl",
        "workflow/projects/p/sessions/s/checkpoint.md",
        "workflow/projects/p/chair-logs/s/log.sqlite3",
    ];
    // A lane worktree with a commit the main checkout does not have.
    git(&rig, &rig.project, &["init", "-q", "-b", "main"])?;
    git(
        &rig,
        &rig.project,
        &["commit", "-q", "--allow-empty", "-m", "base"],
    )?;
    let lane = rig.root.join("lane");
    git(
        &rig,
        &rig.project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane/unlanded",
            lane.to_str().ok_or("lane")?,
        ],
    )?;
    git(
        &rig,
        &lane,
        &["commit", "-q", "--allow-empty", "-m", "unlanded"],
    )?;

    let install = |extra: &[&str]| {
        let mut args = vec![
            "install",
            "--source",
            source,
            "--harness",
            "claude",
            "--json",
        ];
        args.extend(extra);
        rig.command(&args)
    };

    // Create a real receipt, then turn it into the old format-1 receipt.
    let installed = install(&[])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    let receipt_path = state.join("install/receipt.json");
    let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    receipt["state_format_version"] = 1.into();
    fs::write(&receipt_path, serde_json::to_vec(&receipt)?)?;
    for name in dropped
        .iter()
        .chain(&kept)
        .filter(|name| !name.starts_with("run.sqlite3") && !name.starts_with("fabric/host.sqlite3"))
    {
        let path = state.join(name);
        fs::create_dir_all(path.parent().ok_or("parent")?)?;
        match *name {
            "fabric/fabric.sqlite3" => old_sqlite_header(&path, 15)?,
            "children/c1/run.sqlite3" => old_sqlite_header(&path, 10)?,
            "admission.sqlite3" => old_sqlite_header(&path, 6)?,
            "pr-contributions.sqlite3" => old_sqlite_header(&path, 4)?,
            _ => fs::write(&path, format!("format-1 {name}"))?,
        }
    }
    // The old database has a dead queued row, with no attempt process or lock.
    {
        let database = rusqlite::Connection::open(state.join("run.sqlite3"))?;
        database.execute_batch(
            "CREATE TABLE attempts(state TEXT, started_at INTEGER);\
             INSERT INTO attempts VALUES('queued', NULL); PRAGMA user_version=10;",
        )?;
    }
    // The old host index: Fabric schema 15, run-keyed chair bindings. Format 2
    // refuses any schema but its own, so the reset must remove it.
    {
        let database = rusqlite::Connection::open(state.join("fabric/host.sqlite3"))?;
        database.execute_batch(
            "CREATE TABLE run_chairs(run_id TEXT PRIMARY KEY, chair TEXT);\
             INSERT INTO run_chairs VALUES('r1', 'chair');\
             PRAGMA user_version = 15;",
        )?;
    }
    for name in [
        "run.sqlite3-wal",
        "run.sqlite3-shm",
        "run.sqlite3-journal",
        "fabric/host.sqlite3-wal",
        "fabric/host.sqlite3-shm",
    ] {
        fs::write(state.join(name), format!("format-1 {name}"))?;
    }
    let usage = state.join("usage-ledger/segment-0.jsonl");
    fs::create_dir_all(usage.parent().ok_or("usage parent")?)?;
    fs::write(&usage, "format-1 usage row")?;
    let histories = [
        "attempts/base/provider-history/candidate-0/transcript.jsonl",
        "runs/r1/attempts/a1/provider-history/candidate-0/session.jsonl",
        "children/c1/attempts/detached/provider-history/candidate-0/history.jsonl",
    ];
    for name in histories {
        fs::create_dir_all(state.join(name).parent().ok_or("history parent")?)?;
        fs::write(state.join(name), name)?;
    }
    let before = snapshot(state)?;

    // No acknowledgement: one typed code, nothing changes.
    let blocked = install(&[])?;
    let value: serde_json::Value = serde_json::from_slice(&blocked.stdout.bytes)?;
    assert_eq!(value["error"]["code"], "STATE_FORMAT_BREAK", "{value}");
    assert_eq!(code(&blocked), 6);
    assert!(snapshot(state)? == before);

    // The CLI does not accept the bootstrap script's --yes as an acknowledgement.
    let yes = install(&["--yes"])?;
    let value: serde_json::Value = serde_json::from_slice(&yes.stdout.bytes)?;
    assert_eq!(value["error"]["code"], "INVALID", "{value}");
    assert!(snapshot(state)? == before);

    // A live lane blocks even an acknowledged reset.
    let attempt = state.join("runs/r1/attempts/a1");
    let held = provenant_os::fs::Directory::open_anchor(&attempt)?.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let before = snapshot(state)?;
    let live = install(&["--reset-state"])?;
    let value: serde_json::Value = serde_json::from_slice(&live.stdout.bytes)?;
    assert_eq!(value["error"]["code"], "STATE_RESET_LIVE", "{value}");
    assert_ne!(code(&live), 6);
    assert!(snapshot(state)? == before);
    drop(held);

    // The preflight lists the unlanded worktree and changes nothing.
    let preflight = rig.command(&[
        "install",
        "--source",
        source,
        "--upgrade-preflight",
        "--reset-state",
    ])?;
    assert_eq!(code(&preflight), 0, "{preflight:?}");
    assert!(
        String::from_utf8_lossy(&preflight.stderr.bytes).contains("lane (1)"),
        "{}",
        String::from_utf8_lossy(&preflight.stderr.bytes)
    );

    let kept_before: Vec<_> = kept
        .iter()
        .map(|name| {
            let bytes = fs::read(state.join(name))?;
            Ok(sha2::Sha256::digest(bytes).to_vec())
        })
        .collect::<io::Result<_>>()?;
    let done = install(&["--reset-state"])?;
    assert_eq!(code(&done), 0, "{done:?}");
    for name in dropped {
        assert!(!state.join(name).exists(), "{name}");
    }
    let kept_after: Vec<_> = kept
        .iter()
        .map(|name| {
            let bytes = fs::read(state.join(name))?;
            Ok(sha2::Sha256::digest(bytes).to_vec())
        })
        .collect::<io::Result<_>>()?;
    assert_eq!(kept_after, kept_before);
    for name in histories {
        assert_eq!(
            fs::read_to_string(state.join("provider-history/format-1").join(name))?,
            name
        );
    }
    assert!(!state.join("usage-ledger").exists());
    assert_eq!(
        fs::read_to_string(state.join("usage-history/format-1/segment-0.jsonl"))?,
        "format-1 usage row"
    );
    assert!(lane.join(".git").exists());
    git(
        &rig,
        &rig.project,
        &["rev-parse", "--verify", "lane/unlanded"],
    )?;
    // The receipt now names format 2, so the next install is not a break.
    assert_eq!(code(&install(&[])?), 0);
    Ok(())
}

fn old_sqlite_header(path: &Path, version: u32) -> Result {
    let mut header = vec![0; 64];
    header[..16].copy_from_slice(b"SQLite format 3\0");
    header[60..64].copy_from_slice(&version.to_be_bytes());
    fs::write(path, header)?;
    Ok(())
}

fn reset_fixture() -> Result<(TestRig, PathBuf)> {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    assert_eq!(code(&reset_install(&rig, &stage, &[])?), 0);
    set_receipt_format(&rig, 1)?;
    old_sqlite_header(&rig.state.join("run.sqlite3"), 10)?;
    Ok((rig, stage))
}
fn set_receipt_format(rig: &TestRig, format: u64) -> Result {
    let path = rig.state.join("install/receipt.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    value["state_format_version"] = format.into();
    fs::write(path, serde_json::to_vec(&value)?)?;
    Ok(())
}
fn reset_install(
    rig: &TestRig,
    stage: &Path,
    extra: &[&str],
) -> Result<provenant_os::exec::ExecOutcome> {
    let mut args = vec![
        "install",
        "--source",
        stage.to_str().ok_or("source")?,
        "--harness",
        "claude",
        "--json",
    ];
    args.extend_from_slice(extra);
    rig.command(&args)
}

// Every receipt row exercises the install boundary and preserves bytes on refusal.
fn check_receipt_header_decisions(receipt: Option<u64>) -> Result {
    for (store, old, intermediate, current) in [
        ("run.sqlite3", 10, 5, provenant_run::RUN_SCHEMA_VERSION),
        (
            "fabric/fabric.sqlite3",
            15,
            8,
            provenant_fabric_store::CURRENT_SCHEMA_VERSION as u32,
        ),
        (
            "fabric/host.sqlite3",
            15,
            8,
            provenant_fabric_store::CURRENT_SCHEMA_VERSION as u32,
        ),
        (
            "admission.sqlite3",
            6,
            3,
            provenant_run::ADMISSION_SCHEMA_VERSION,
        ),
        (
            "pr-contributions.sqlite3",
            4,
            2,
            provenant_run::CONTRIBUTIONS_SCHEMA_VERSION,
        ),
    ] {
        let (rig, stage) = reset_fixture()?;
        fs::remove_file(rig.state.join("run.sqlite3"))?;
        match receipt {
            Some(format) => set_receipt_format(&rig, format)?,
            None => fs::remove_file(rig.state.join("install/receipt.json"))?,
        }
        let path = rig.state.join(store);
        fs::create_dir_all(path.parent().ok_or("store parent")?)?;
        for version in [0, old, intermediate, current - 1, current, current + 1] {
            old_sqlite_header(&path, version)?;
            let before = snapshot(&rig.state)?;
            let output = reset_install(&rig, &stage, &["--upgrade-preflight"])?;
            let expected = match receipt {
                Some(1) if version < current => Some(6),
                Some(1) => None,
                _ if version == old => Some(6),
                _ if version == 0 => Some(0),
                Some(2) if version > old => Some(0),
                _ => None,
            };
            if let Some(expected) = expected {
                assert_eq!(
                    code(&output),
                    expected,
                    "{store} {version} {receipt:?}: {output:?}"
                );
            } else {
                let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
                assert_eq!(
                    value["error"]["code"], "STATE_RESET_UNSAFE",
                    "{store} {version} {receipt:?}: {output:?}"
                );
            }
            assert_eq!(snapshot(&rig.state)?, before);
        }
        old_sqlite_header(&path, 0)?;
        let zero_header = fs::read(&path)?;
        for (bytes, companion, unknown) in [
            (Vec::new(), None, false),
            (Vec::new(), Some("-wal"), false),
            (vec![0; 12], Some("-wal"), true),
            (vec![0; 12], Some("-shm"), true),
            (zero_header.clone(), Some("-wal"), false),
            (zero_header, Some("-shm"), false),
            (b"not a database".to_vec(), None, true),
        ] {
            fs::write(&path, &bytes)?;
            let companion_path = companion.map(|suffix| {
                path.with_file_name(format!(
                    "{}{suffix}",
                    path.file_name().expect("store name").to_string_lossy()
                ))
            });
            if let Some(companion) = &companion_path {
                fs::write(companion, "uncheckpointed")?;
            }
            let before = snapshot(&rig.state)?;
            let output = reset_install(&rig, &stage, &["--upgrade-preflight"])?;
            if unknown {
                let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
                assert_eq!(value["error"]["code"], "STATE_RESET_UNSAFE", "{output:?}");
            } else {
                assert_eq!(
                    code(&output),
                    if receipt == Some(1) { 6 } else { 0 },
                    "{output:?}"
                );
            }
            assert_eq!(snapshot(&rig.state)?, before);
            if let Some(companion) = companion_path {
                fs::remove_file(companion)?;
            }
        }
        // Each identified store can be removed without a sibling authorising it.
        old_sqlite_header(
            &path,
            if receipt == Some(1) {
                intermediate
            } else {
                old
            },
        )?;
        let output = reset_install(&rig, &stage, &["--reset-state"])?;
        assert_eq!(code(&output), 0, "{output:?}");
        assert!(!path.exists());
    }
    Ok(())
}

#[test]
fn reset_without_receipt_requires_shipped_headers_per_store() -> Result {
    check_receipt_header_decisions(None)
}

#[test]
fn reset_with_format_one_receipt_checks_each_store_against_its_current_version() -> Result {
    check_receipt_header_decisions(Some(1))
}

#[test]
fn reset_with_format_two_receipt_requires_shipped_headers_and_preserves_current_stores() -> Result {
    check_receipt_header_decisions(Some(2))
}

#[test]
fn reset_zero_version_header_is_fresh_only_with_at_most_one_page() -> Result {
    let (rig, stage) = reset_fixture()?;
    set_receipt_format(&rig, 2)?;
    let path = rig.state.join("run.sqlite3");
    for pages in [0_u32, 1, 2] {
        old_sqlite_header(&path, 0)?;
        let mut header = fs::read(&path)?;
        header[28..32].copy_from_slice(&pages.to_be_bytes());
        fs::write(&path, header)?;
        let before = snapshot(&rig.state)?;
        let output = reset_install(&rig, &stage, &["--upgrade-preflight"])?;
        if pages <= 1 {
            assert_eq!(code(&output), 0, "{output:?}");
        } else {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
            assert_eq!(value["error"]["code"], "STATE_RESET_UNSAFE");
        }
        assert_eq!(snapshot(&rig.state)?, before);
    }
    Ok(())
}

#[test]
fn reset_selects_only_identified_stores_and_their_payloads() -> Result {
    let (rig, stage) = reset_fixture()?;
    set_receipt_format(&rig, 2)?;
    // Root run and Fabric are live; only admission and a nested child are old.
    old_sqlite_header(
        &rig.state.join("run.sqlite3"),
        provenant_run::RUN_SCHEMA_VERSION,
    )?;
    fs::create_dir_all(rig.state.join("fabric/graphs"))?;
    old_sqlite_header(
        &rig.state.join("fabric/fabric.sqlite3"),
        provenant_fabric_store::CURRENT_SCHEMA_VERSION as u32,
    )?;
    fs::write(rig.state.join("fabric/graphs/live.json"), "live graph")?;
    fs::create_dir_all(rig.state.join("attempts/live/provider-history"))?;
    fs::write(
        rig.state.join("attempts/live/provider-history/transcript"),
        "live history",
    )?;
    old_sqlite_header(&rig.state.join("admission.sqlite3"), 6)?;
    for (child, version) in [("old", 10), ("live", provenant_run::RUN_SCHEMA_VERSION)] {
        let root = rig.state.join("children").join(child);
        fs::create_dir_all(root.join("attempts/a1/tmp"))?;
        old_sqlite_header(&root.join("run.sqlite3"), version)?;
        fs::write(root.join("attempts/a1/tmp/marker"), child)?;
    }
    let before = snapshot(&rig.state)?;
    let output = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let after = snapshot(&rig.state)?;
    for (path, bytes) in before.iter().filter(|(path, _)| {
        ["run.sqlite3", "fabric", "attempts", "children/live"]
            .iter()
            .any(|prefix| path.starts_with(rig.state.join(prefix)))
    }) {
        assert_eq!(after.get(path), Some(bytes), "{}", path.display());
    }
    assert!(!rig.state.join("admission.sqlite3").exists());
    assert!(!rig.state.join("children/old/run.sqlite3").exists());
    assert!(!rig.state.join("children/old/attempts").exists());
    Ok(())
}

// Medium: real install receipts and filesystem archives, scripted launchctl only.
#[test]
fn reset_requires_confirmed_service_absence_before_any_state_moves() -> Result {
    use provenant_config::{InstallError, InstallReporter};
    use provenant_core::{Clock, WaitBudget};
    use provenant_fabric_runtime::peer_agent::{AgentError, ServiceState};
    use std::ffi::OsStr;

    struct ResetReporter<F> {
        plan: state_reset::Plan,
        home: PathBuf,
        run: F,
    }
    impl<F> InstallReporter for ResetReporter<F>
    where
        F: FnMut(&[&OsStr], &Path) -> std::result::Result<(bool, String), AgentError>,
    {
        fn preview(&mut self, _: &[provenant_config::Action]) -> io::Result<()> {
            Ok(())
        }
        fn before_receipt(&mut self) -> io::Result<()> {
            state_reset::remove(&self.plan, || {
                state_reset::quiesce_launch_agents(
                    &self.home,
                    &[
                        "dev.provenant.peer",
                        "dev.provenant.disk-guard",
                        "dev.provenant.gc",
                        "dev.provenant.tick",
                    ],
                    &mut self.run,
                    |_, result| result.map(|_| ()),
                )
            })
        }
    }
    // Exercise every label, including a late failure after earlier agents stop.
    for label in [
        "dev.provenant.peer",
        "dev.provenant.disk-guard",
        "dev.provenant.gc",
        "dev.provenant.tick",
    ] {
        for failure in [
            Some(ServiceState::Indeterminate),
            Some(ServiceState::Running),
            None,
        ] {
            let rig = TestRig::new()?;
            let stage = staged(&rig)?;
            let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
            inputs.environment_state_dir = Some(rig.state.clone());
            let targets = provenant_config::Targets::new(
                inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
                &rig.home,
                &[provenant_records::Harness::Claude],
            );
            let release = Release::read(&stage, env!("PROVENANT_TARGET_TRIPLE"))?;
            let manifest = release.manifest();
            let smoke = FixtureSmoke(VersionInfo {
                extra: Default::default(),
                source_digest: manifest.source_digest,
                asset_digest: manifest.asset_digest,
                target: manifest.target.clone(),
                toolchain: manifest.toolchain.clone(),
                record_schema_version: 1,
                config_schema_version: 1,
            });
            let clock = provenant_os::clock::SystemClock::new();
            let cancel = CancelToken::new();
            let context = provenant_config::InstallContext {
                clock: &clock,
                cancel: &cancel,
                budget: WaitBudget::cli(clock.mono_now(), 30)?,
            };
            struct Initial;
            impl InstallReporter for Initial {
                fn preview(&mut self, _: &[provenant_config::Action]) -> io::Result<()> {
                    Ok(())
                }
            }
            provenant_config::plan_and_install(
                &release,
                &targets,
                &context,
                &mut Initial,
                &smoke,
                false,
            )?;
            set_receipt_format(&rig, 1)?;
            old_sqlite_header(&rig.state.join("run.sqlite3"), 10)?;
            let history = rig
                .state
                .join("attempts/old/provider-history/transcript.json");
            fs::create_dir_all(history.parent().ok_or("history parent")?)?;
            fs::write(&history, b"keep transcript")?;
            fs::create_dir_all(rig.state.join("usage-ledger"))?;
            fs::write(
                rig.state.join("usage-ledger/segment.jsonl"),
                b"keep telemetry",
            )?;
            let before = snapshot(&rig.state)?;
            let mut stopped = std::collections::BTreeSet::new();
            let run = |args: &[&OsStr], _: &Path| {
                let service = args[1].to_string_lossy();
                let current = service.rsplit('/').next().unwrap();
                if args[0] == "bootout" {
                    stopped.insert(current.to_owned());
                    return Ok((true, String::new()));
                }
                if current == label && failure == Some(ServiceState::Indeterminate) {
                    return Ok((false, "timed out after 10s".into()));
                }
                if stopped.contains(current)
                    && !(current == label && failure == Some(ServiceState::Running))
                {
                    Ok((
                        false,
                        format!(
                            "exit 113: Bad request.\nCould not find service \"{current}\" in domain for user gui: 501"
                        ),
                    ))
                } else {
                    Ok((true, "state = running\n pid = 41".into()))
                }
            };
            let mut reporter = ResetReporter {
                plan: state_reset::plan(&rig.state, Some(1))?.ok_or("reset plan")?,
                home: rig.home.clone(),
                run,
            };
            let result = provenant_config::plan_and_install(
                &release,
                &targets,
                &context,
                &mut reporter,
                &smoke,
                false,
            );
            if let Some(expected) = failure {
                let mut error = result.expect_err("unconfirmed shutdown must refuse installation");
                while let InstallError::Partial { source, .. } = error {
                    error = *source;
                }
                let InstallError::Preview(error) = error else {
                    panic!("expected typed reset refusal: {error:?}");
                };
                assert!(
                    matches!(error.get_ref().and_then(|e| e.downcast_ref::<AgentError>()),
                    Some(AgentError::Quiescence { state, .. }) if *state == expected)
                );
                assert_eq!(snapshot(&rig.state)?, before);
            } else {
                result?;
                assert!(!rig.state.join("run.sqlite3").exists());
                assert!(!history.exists());
                assert_eq!(
                    fs::read(rig.state.join(
                        "provider-history/format-1/attempts/old/provider-history/transcript.json"
                    ))?,
                    b"keep transcript"
                );
                assert_eq!(
                    fs::read(rig.state.join("usage-history/format-1/segment.jsonl"))?,
                    b"keep telemetry"
                );
                let receipt: serde_json::Value =
                    serde_json::from_slice(&fs::read(rig.state.join("install/receipt.json"))?)?;
                assert_eq!(receipt["state_format_version"], 2);
            }
        }
    }
    Ok(())
}

#[test]
fn reset_archives_telemetry_only_without_a_format_two_receipt() -> Result {
    for receipt in [None, Some(1), Some(2)] {
        let (rig, stage) = reset_fixture()?;
        match receipt {
            Some(format) => set_receipt_format(&rig, format)?,
            None => fs::remove_file(rig.state.join("install/receipt.json"))?,
        }
        let telemetry = [
            (
                "usage-ledger/segment.jsonl",
                "usage-history/format-1/segment.jsonl",
            ),
            (
                "ingest/schema-1.json",
                "usage-history/format-1-ingest/schema-1.json",
            ),
            (
                "ingest-cursors.json",
                "usage-history/format-1-ingest-cursors.json",
            ),
        ];
        for (source, _) in telemetry {
            let path = rig.state.join(source);
            fs::create_dir_all(path.parent().ok_or("telemetry parent")?)?;
            fs::write(path, source)?;
        }
        let output = reset_install(&rig, &stage, &["--reset-state"])?;
        assert_eq!(code(&output), 0, "{output:?}");
        for (source, archive) in telemetry {
            if receipt == Some(2) {
                assert_eq!(fs::read_to_string(rig.state.join(source))?, source);
                assert!(!rig.state.join(archive).exists());
            } else {
                assert!(!rig.state.join(source).exists());
                assert_eq!(fs::read_to_string(rig.state.join(archive))?, source);
            }
        }
    }
    Ok(())
}

#[test]
fn reset_usage_archive_refuses_unsafe_sources_and_destinations_before_writes() -> Result {
    for name in ["usage-ledger", "usage-history", "usage-history/format-1"] {
        for kind in ["file", "link", "directory"] {
            if name != "usage-history/format-1" && kind == "directory" {
                continue;
            }
            let (rig, stage) = reset_fixture()?;
            let outside = rig.root.join("outside-usage");
            fs::create_dir(&outside)?;
            fs::write(outside.join("rows.jsonl"), "keep")?;
            let invalid = rig.state.join(name);
            fs::create_dir_all(invalid.parent().ok_or("usage parent")?)?;
            match kind {
                "file" => fs::write(&invalid, "unknown")?,
                "link" => std::os::unix::fs::symlink(&outside, &invalid)?,
                _ => fs::create_dir(&invalid)?,
            }
            if name != "usage-ledger" {
                fs::create_dir(rig.state.join("usage-ledger"))?;
                fs::write(rig.state.join("usage-ledger/old.jsonl"), "old usage")?;
            }
            let before = snapshot(&rig.state)?;
            let refused = reset_install(&rig, &stage, &["--upgrade-preflight", "--reset-state"])?;
            assert_ne!(code(&refused), 0, "{refused:?}");
            assert_eq!(snapshot(&rig.state)?, before);
            assert_eq!(fs::read_to_string(outside.join("rows.jsonl"))?, "keep");
        }
    }
    Ok(())
}

#[test]
fn reset_refuses_writable_roots_and_unsupported_lock_entries_before_writes() -> Result {
    for writable_root in [true, false] {
        let (rig, stage) = reset_fixture()?;
        if writable_root {
            fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o722))?;
        } else {
            fs::create_dir_all(rig.state.join("attempts/a1/custody.lock"))?;
        }
        let before = snapshot(&rig.state)?;
        let refused = reset_install(&rig, &stage, &["--upgrade-preflight", "--reset-state"])?;
        assert_ne!(code(&refused), 0, "{refused:?}");
        let value: serde_json::Value = serde_json::from_slice(&refused.stdout.bytes)?;
        assert_eq!(value["error"]["code"], "STATE_RESET_UNSAFE");
        assert_eq!(snapshot(&rig.state)?, before);
    }
    Ok(())
}

#[test]
fn reset_invalid_source_preserves_old_state() -> Result {
    let (rig, _) = reset_fixture()?;
    let before = snapshot(&rig.state)?;
    let failed = reset_install(&rig, &rig.root.join("missing"), &["--reset-state"])?;
    assert_ne!(code(&failed), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_preview_conflict_preserves_old_state() -> Result {
    let (rig, stage) = reset_fixture()?;
    // A later registered account must also preview successfully before reset.
    let account = rig.home.join(".claude-work");
    fs::create_dir_all(account.join("skills/example"))?;
    fs::write(
        account.join("skills/example/SKILL.md"),
        "user-owned conflict",
    )?;
    support::register_account(&rig, "claude", "work", "work-key", &account)?;
    let before = snapshot(&rig.state)?;
    let failed = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_ne!(code(&failed), 0, "{failed:?}");
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_failed_smoke_preserves_old_state() -> Result {
    let (rig, _) = reset_fixture()?;
    let bad_binary = rig.root.join("bad-binary");
    fs::write(&bad_binary, "#!/bin/sh\nexit 9\n")?;
    fs::set_permissions(&bad_binary, fs::Permissions::from_mode(0o755))?;
    let stage = staged_binary(&rig, "bad-smoke", b"bad smoke", Some(&bad_binary))?;
    let before = snapshot(&rig.state)?;
    let failed = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_ne!(code(&failed), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_dry_run_describes_deletion_without_writes() -> Result {
    let (rig, stage) = reset_fixture()?;
    let before = snapshot(&rig.state)?;
    let preview = reset_install(&rig, &stage, &["--dry-run"])?;
    assert_eq!(code(&preview), 0, "{preview:?}");
    let text = String::from_utf8(preview.stderr.bytes)?;
    assert!(
        text.contains("run.sqlite3")
            && text.contains("admission.sqlite3")
            && text.contains("provider-history")
    );
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_corrupt_receipt_refuses_without_deletion() -> Result {
    let (rig, stage) = reset_fixture()?;
    fs::write(rig.state.join("install/receipt.json"), b"{")?;
    let before = snapshot(&rig.state)?;
    assert_ne!(code(&reset_install(&rig, &stage, &["--reset-state"])?), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_newer_receipt_refuses_downgrade() -> Result {
    let (rig, stage) = reset_fixture()?;
    set_receipt_format(&rig, 3)?;
    let before = snapshot(&rig.state)?;
    let failed = reset_install(&rig, &stage, &["--reset-state"])?;
    let value: serde_json::Value = serde_json::from_slice(&failed.stdout.bytes)?;
    assert_eq!(value["error"]["code"], "STATE_FORMAT_NEWER");
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_unmarked_root_refuses_deletion() -> Result {
    let rig = TestRig::new()?;
    let stage = staged(&rig)?;
    fs::create_dir_all(&rig.state)?;
    fs::write(rig.state.join("run.sqlite3"), "unrelated database")?;
    let before = snapshot(&rig.state)?;
    assert_ne!(code(&reset_install(&rig, &stage, &["--reset-state"])?), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_symlinked_fabric_refuses_outside_deletion() -> Result {
    let (rig, stage) = reset_fixture()?;
    let outside = rig.root.join("outside");
    fs::create_dir(&outside)?;
    fs::write(outside.join("fabric.sqlite3"), "outside store")?;
    std::os::unix::fs::symlink(&outside, rig.state.join("fabric"))?;
    let before = snapshot(&rig.state)?;
    assert_ne!(code(&reset_install(&rig, &stage, &["--reset-state"])?), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    assert_eq!(
        fs::read_to_string(outside.join("fabric.sqlite3"))?,
        "outside store"
    );
    Ok(())
}

#[test]
fn reset_detached_child_custody_blocks_deletion() -> Result {
    let (rig, stage) = reset_fixture()?;
    let child = rig.state.join("children/c1/children/c2/attempts/a1");
    fs::create_dir_all(&child)?;
    let held = provenant_os::fs::Directory::open_anchor(&child)?.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let before = snapshot(&rig.state)?;
    assert_ne!(code(&reset_install(&rig, &stage, &["--reset-state"])?), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    drop(held);
    Ok(())
}

#[test]
fn reset_restores_readonly_payload_access_and_unlinks_only_dropped_links() -> Result {
    let (rig, stage) = reset_fixture()?;
    let tmp = rig.state.join("attempts/a1/tmp/cache");
    fs::create_dir_all(&tmp)?;
    fs::write(tmp.join("Cargo.lock"), "cache lock")?;
    let outside = rig.root.join("outside-store");
    fs::write(&outside, "keep target")?;
    std::os::unix::fs::symlink(&outside, rig.state.join("admission.sqlite3"))?;
    std::os::unix::fs::symlink(&outside, tmp.join("outside-link"))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o000))?;
    let result = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_eq!(code(&result), 0, "{result:?}");
    assert!(!rig.state.join("attempts").exists());
    assert!(
        fs::symlink_metadata(rig.state.join("admission.sqlite3"))?
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(outside)?, "keep target");
    Ok(())
}

#[test]
fn reset_invalid_provider_history_refuses_during_preflight() -> Result {
    for name in [
        "attempts/a1/provider-history",
        "provider-history",
        "provider-history/format-1",
        "provider-history/format-1/attempts/a1/provider-history",
    ] {
        for link in [false, true] {
            let (rig, stage) = reset_fixture()?;
            fs::create_dir_all(rig.state.join("attempts/a1"))?;
            let invalid = rig.state.join(name);
            fs::create_dir_all(invalid.parent().ok_or("history parent")?)?;
            let outside = rig.root.join("outside-history");
            fs::create_dir(&outside)?;
            fs::write(outside.join("transcript"), "keep")?;
            if link {
                std::os::unix::fs::symlink(&outside, &invalid)?;
            } else {
                fs::write(&invalid, "not a directory")?;
            }
            let before = snapshot(&rig.state)?;
            let refused = reset_install(&rig, &stage, &["--upgrade-preflight", "--reset-state"])?;
            assert_ne!(code(&refused), 0, "{refused:?}");
            let value: serde_json::Value = serde_json::from_slice(&refused.stdout.bytes)?;
            assert_eq!(value["error"]["code"], "STATE_RESET_UNSAFE");
            assert_eq!(snapshot(&rig.state)?, before);
            assert_eq!(fs::read_to_string(outside.join("transcript"))?, "keep");
        }
    }
    Ok(())
}

#[test]
fn reset_archive_collision_refuses_before_writes_and_is_rerunnable() -> Result {
    let (rig, stage) = reset_fixture()?;
    // A current receipt can coexist with foreign stores recreated by an old agent.
    set_receipt_format(&rig, 2)?;
    let source = rig.state.join("attempts/a1/provider-history");
    let archive = rig
        .state
        .join("provider-history/format-1/attempts/a1/provider-history");
    fs::create_dir_all(&source)?;
    fs::write(source.join("transcript.jsonl"), "original transcript")?;
    fs::create_dir_all(&archive)?;
    fs::write(archive.join("transcript.jsonl"), "existing archive")?;
    let before = snapshot(&rig.state)?;
    let failed = reset_install(&rig, &stage, &["--upgrade-preflight", "--reset-state"])?;
    assert_ne!(code(&failed), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    assert!(!String::from_utf8_lossy(&failed.stderr.bytes).contains("Agents stopped"));
    assert_eq!(
        fs::read_to_string(source.join("transcript.jsonl"))?,
        "original transcript"
    );
    assert_eq!(
        fs::read_to_string(archive.join("transcript.jsonl"))?,
        "existing archive"
    );
    assert!(rig.state.join("run.sqlite3").exists());
    // Resolve the collision without deleting its content, then retry.
    fs::rename(&archive, rig.state.join("provider-history/previous"))?;
    let result = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_eq!(code(&result), 0, "{result:?}");
    assert_eq!(
        fs::read_to_string(archive.join("transcript.jsonl"))?,
        "original transcript"
    );
    assert_eq!(
        fs::read_to_string(rig.state.join("provider-history/previous/transcript.jsonl"))?,
        "existing archive"
    );
    assert!(!rig.state.join("run.sqlite3").exists());
    // Interrupted after the move, before payload deletion: the source is gone
    // and a valid archive is present. Retrying must preserve that archive.
    old_sqlite_header(&rig.state.join("run.sqlite3"), 10)?;
    fs::create_dir_all(rig.state.join("attempts/a1/tmp"))?;
    fs::write(rig.state.join("attempts/a1/tmp/leftover"), "old payload")?;
    let retried = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_eq!(code(&retried), 0, "{retried:?}");
    assert!(!rig.state.join("attempts/a1").exists());
    assert_eq!(
        fs::read_to_string(archive.join("transcript.jsonl"))?,
        "original transcript"
    );
    Ok(())
}

#[test]
fn reset_recovers_shipped_store_after_format_two_receipt() -> Result {
    let (rig, stage) = reset_fixture()?;
    set_receipt_format(&rig, 2)?;
    fs::remove_file(rig.state.join("run.sqlite3"))?;
    let recreated = rig.state.join("children/c1/children/c2/run.sqlite3");
    fs::create_dir_all(recreated.parent().ok_or("child store")?)?;
    let database = rusqlite::Connection::open(&recreated)?;
    database.execute_batch("PRAGMA user_version=10; CREATE TABLE old(value TEXT);")?;
    drop(database);
    let before = snapshot(&rig.state)?;
    let refused = reset_install(&rig, &stage, &[])?;
    assert_eq!(code(&refused), 6);
    assert_eq!(snapshot(&rig.state)?, before);
    let installed = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    assert!(!recreated.exists());
    // A native format-2 header must not trigger another reset.
    let database = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    database.pragma_update(None, "user_version", provenant_run::RUN_SCHEMA_VERSION)?;
    database.execute_batch("CREATE TABLE native(value TEXT);")?;
    drop(database);
    assert_eq!(code(&reset_install(&rig, &stage, &[])?), 0);
    assert!(rig.state.join("run.sqlite3").exists());
    Ok(())
}

#[test]
fn upgrade_refuses_format_break_before_any_captured_installer() -> Result {
    let (rig, _) = reset_fixture()?;
    let before = snapshot(&rig.state)?;
    let failed = rig.command(&["upgrade", "--yes", "--json"])?;
    assert_eq!(code(&failed), 6);
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn bootstrap_captured_upgrade_refuses_without_reading_a_terminal() -> Result {
    let mut rig = TestRig::new()?;
    let source = rig.root.join("source");
    fs::create_dir_all(source.join("tools/front-door"))?;
    fs::write(
        source.join("tools/front-door/rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.1\"\n",
    )?;
    fs::create_dir_all(rig.state.join("install"))?;
    fs::write(rig.state.join("install/receipt.json"), "{}")?;
    let fake_bin = rig.root.join("fake-bin");
    fs::create_dir(&fake_bin)?;
    let installer = rig.root.join("fixture-installer");
    fs::write(
        &installer,
        r#"#!/bin/sh
case "$*" in
  *--upgrade-preflight*) case "$*" in *--reset-state*) exit "${ACKNOWLEDGED_STATUS:-0}" ;; esac; exit "$PREFLIGHT_STATUS" ;;
esac
printf installed > "$HOME/installed"
"#,
    )?;
    fs::set_permissions(&installer, fs::Permissions::from_mode(0o755))?;
    for (name, content) in [
        ("rustup", "#!/bin/sh\nexit 0\n".to_owned()),
        (
            "cargo",
            format!(
                r#"#!/bin/sh
[ "$1" = run ] || exit 0
while [ "$1" != --out ]; do shift; done
mkdir -p "$2/bin"
cp '{}' "$2/bin/provenant"
"#,
                installer.display()
            ),
        ),
    ] {
        let file = fake_bin.join(name);
        fs::write(&file, content)?;
        fs::set_permissions(file, fs::Permissions::from_mode(0o755))?;
    }
    for (key, value) in &mut rig.env {
        if key == "PATH" {
            *value = format!("{}:/usr/bin:/bin", fake_bin.display()).into();
        }
    }
    rig.env.push(("PROVENANT_INSTALL_TTY".into(), "1".into()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh");
    let mut args = vec![
        script.into_os_string(),
        "--upgrade".into(),
        "--source".into(),
        source.into_os_string(),
    ];
    rig.env.push(("PREFLIGHT_STATUS".into(), "2".into()));
    let invalid = rig.execute_os(Path::new("/bin/sh"), &args, Some(b"reset\n"))?;
    assert_eq!(code(&invalid), 2);
    assert!(!rig.home.join("installed").exists());
    assert!(!String::from_utf8_lossy(&invalid.stderr.bytes).contains("Type reset"));
    rig.env.retain(|(key, _)| key != "PREFLIGHT_STATUS");
    rig.env.push(("PREFLIGHT_STATUS".into(), "6".into()));
    let prompted = rig.execute_os(Path::new("/bin/sh"), &args, Some(b"reset\n"))?;
    assert_eq!(code(&prompted), 0, "{prompted:?}");
    assert!(String::from_utf8_lossy(&prompted.stderr.bytes).contains("Type reset"));
    assert!(rig.home.join("installed").exists());
    fs::remove_file(rig.home.join("installed"))?;
    let captured_stderr = rig.root.join("captured-stderr");
    rig.env
        .push(("CAPTURE".into(), captured_stderr.clone().into_os_string()));
    let mut redirected = vec![
        "-c".into(),
        "unset PROVENANT_INSTALL_TTY; exec /bin/sh \"$@\" </dev/null 2>\"$CAPTURE\"".into(),
        "bootstrap-test".into(),
    ];
    redirected.extend(args.clone());
    let captured = rig.execute_os(Path::new("/bin/sh"), &redirected, None)?;
    assert_eq!(code(&captured), 6, "{captured:?}");
    assert!(!rig.home.join("installed").exists());
    let refusal = fs::read_to_string(&captured_stderr)?;
    assert!(!refusal.contains("Type reset"));
    assert!(refusal.contains("original install command") && refusal.contains("--reset-state"));
    args.push("--reset-state".into());
    let accepted = rig.execute_os(Path::new("/bin/sh"), &args, None)?;
    assert_eq!(code(&accepted), 0, "{accepted:?}");
    assert!(rig.home.join("installed").exists());
    fs::remove_file(rig.home.join("installed"))?;
    rig.env.push(("ACKNOWLEDGED_STATUS".into(), "2".into()));
    let failed_validation = rig.execute_os(Path::new("/bin/sh"), &args, None)?;
    assert_eq!(code(&failed_validation), 2);
    assert!(!rig.home.join("installed").exists());
    Ok(())
}

#[test]
fn reset_receipt_read_error_refuses_without_deletion() -> Result {
    let (rig, stage) = reset_fixture()?;
    let receipt = rig.state.join("install/receipt.json");
    fs::remove_file(&receipt)?;
    fs::create_dir(&receipt)?;
    let before = snapshot(&rig.state)?;
    assert_ne!(code(&reset_install(&rig, &stage, &["--reset-state"])?), 0);
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn reset_unlinks_lock_named_symlinks_without_touching_their_targets() -> Result {
    let (rig, stage) = reset_fixture()?;
    let outside = rig.root.join("outside-lock-target");
    fs::write(&outside, "keep target")?;
    std::os::unix::fs::symlink(&outside, rig.state.join("run.sqlite3.lock"))?;
    let installed = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_eq!(code(&installed), 0, "{installed:?}");
    assert!(!rig.state.join("run.sqlite3").exists());
    assert!(fs::symlink_metadata(rig.state.join("run.sqlite3.lock")).is_err());
    assert_eq!(fs::read_to_string(outside)?, "keep target");
    Ok(())
}

#[test]
fn reset_recovers_retired_store_companions_after_format_two_receipt() -> Result {
    let (rig, stage) = reset_fixture()?;
    set_receipt_format(&rig, 2)?;
    fs::remove_file(rig.state.join("run.sqlite3"))?;
    fs::write(
        rig.state.join("work-index.sqlite3-wal"),
        "retired companion",
    )?;
    let before = snapshot(&rig.state)?;
    assert_eq!(code(&reset_install(&rig, &stage, &[])?), 6);
    assert_eq!(snapshot(&rig.state)?, before);
    let result = reset_install(&rig, &stage, &["--reset-state"])?;
    assert_eq!(code(&result), 0, "{result:?}");
    assert!(!rig.state.join("work-index.sqlite3-wal").exists());
    Ok(())
}
