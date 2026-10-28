#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use provenant_core::{PathResolver, RootInputs};
use provenant_os::clock::SystemClock;
use provenant_records::{SourceInput, source_digest};
use std::{
    fs, io,
    os::unix::fs::{PermissionsExt, symlink},
};
struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, p: &Path) -> io::Result<PathBuf> {
        Ok(p.into())
    }
}
struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
    source: PathBuf,
    targets: Targets,
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        let source = base.join("staged");
        fs::create_dir(&home).unwrap();
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("skills/example")).unwrap();
        for (path, bytes, mode) in [
            ("bin/provenant", b"fixture executable".as_slice(), 0o755),
            (
                "skills/example/SKILL.md",
                b"---\nname: example\n---\n".as_slice(),
                0o600,
            ),
        ] {
            let p = source.join(path);
            fs::write(&p, bytes).unwrap();
            fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
        }
        let mut inputs = RootInputs::new(home.clone(), base.join("checkout"));
        // Explicit captured HOME/PROVENANT_STATE_DIR/harness inputs, no process
        // environment mutation or access to real config/state.
        inputs.environment_state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&Resolver).unwrap();
        let targets = Targets::new(roots, &home, &[Harness::Codex]);
        Self {
            _tmp: tmp,
            base,
            home,
            source,
            targets,
        }
    }
    fn release(&self) -> Release {
        let digest = source_digest(&[SourceInput {
            name: "src/lib.rs",
            bytes: b"fixture",
        }])
        .unwrap();
        package_fixture_release(&self.source, digest)
    }
    fn plan(&self) -> InstallPlan {
        let r = self.release();
        let s = Snapshots::capture(&r, &self.targets).unwrap();
        plan_install(r, self.targets.clone(), s).unwrap()
    }
    fn link(&self) -> PathBuf {
        self.home.join(".codex/skills/example")
    }
    fn receipt(&self) -> PathBuf {
        self.targets
            .roots
            .state
            .as_path()
            .join("install/receipt.json")
    }
}
fn package_fixture_release(
    source: &Path,
    source_digest: provenant_records::SourceDigest,
) -> Release {
    let mut files = BTreeMap::new();
    for relative in crate::release::inventory(source).unwrap() {
        let path = source.join(&relative);
        let bytes = fs::read(&path).unwrap();
        let actual_mode = fs::metadata(&path).unwrap().permissions().mode();
        let mode = if relative == Path::new("bin/provenant") || actual_mode & 0o111 != 0 {
            0o700
        } else {
            0o600
        };
        files.insert(relative, (bytes, mode));
    }
    let expected = VersionInfo {
        source_digest,
        asset_digest: crate::release::asset_inventory_digest(&files).unwrap(),
        target: "fixture-target".into(),
        toolchain: "fixture-toolchain".into(),
        record_schema_version: 1,
        config_schema_version: 1,
    };
    crate::package_release(source, &expected, &Smoke, &CancelToken::new()).unwrap()
}

#[derive(Default)]
struct Reporter {
    seen: usize,
    fail: bool,
}
impl InstallReporter for Reporter {
    fn preview(&mut self, a: &[Action]) -> io::Result<()> {
        self.seen += a.len();
        if self.fail {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        } else {
            Ok(())
        }
    }
}
struct Smoke;

#[test]
fn peer_agent_accepts_a_symlinked_home() {
    let mut f = Fixture::new();
    f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    let alias = f.base.join("home-alias");
    symlink(&f.home, &alias).unwrap();
    f.targets.home = alias;
    f.targets.peer_agent = Some(b"rendered plist\n".to_vec());
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &f.targets).unwrap();
    let plan = plan_install(release, f.targets.clone(), snapshots).unwrap();
    assert!(plan.actions().iter().any(|action| {
        action.selector == "peer-agent"
            && action.path.to_path_buf().unwrap()
                == f.home.join("Library/LaunchAgents/dev.provenant.peer.plist")
    }));
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    f.targets.home = f.home.clone();
    assert!(f.plan().findings().is_empty());
}

#[test]
fn uninstall_prunes_empty_managed_hook_groups() {
    let mut f = Fixture::new();
    f.targets.harnesses =
        Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]).harnesses;
    f.targets.hooks = true;
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(f.home.join(".claude/settings.json")).unwrap()).unwrap();
    assert_eq!(settings["hooks"], serde_json::json!({}));
}

#[test]
fn uninstall_keeps_hook_siblings_and_groups_with_extra_keys() {
    let mut f = Fixture::new();
    f.targets.harnesses =
        Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]).harnesses;
    f.targets.hooks = true;
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let path = f.home.join(".claude/settings.json");
    let mut bytes = fs::read(&path).unwrap();
    // An extra key protects the group even when its last owned leaf is removed.
    bytes = crate::splice::set(
        &bytes,
        &["hooks", "PreCompact", "0", "user"],
        Some(&serde_json::json!(true)),
    )
    .unwrap();
    let foreign = serde_json::json!({"type": "command", "command": "herdr capture"});
    bytes = crate::splice::set(
        &bytes,
        &["hooks", "PreToolUse", "0", "hooks", "1"],
        Some(&foreign),
    )
    .unwrap();
    // The second group moves when the first, owned-only group is pruned.
    let sibling = serde_json::json!({"matcher": "startup", "hooks": [foreign.clone()]});
    bytes = crate::splice::set(&bytes, &["hooks", "SessionStart", "1"], Some(&sibling)).unwrap();
    let mut commented = b"{ // keep user comment\n".to_vec();
    commented.extend_from_slice(&bytes[1..]);
    fs::write(&path, commented).unwrap();
    uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    let bytes = fs::read(&path).unwrap();
    let settings: serde_json::Value = provenant_records::decode_jsonc(
        std::io::Cursor::new(&bytes),
        provenant_records::Limits::CONFIG,
    )
    .unwrap();
    assert_eq!(
        settings["hooks"],
        serde_json::json!({
            "PreToolUse": [{"matcher": "Bash", "hooks": [foreign]}],
            "PreCompact": [{"hooks": [], "user": true}],
            "SessionStart": [sibling],
        })
    );
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("// keep user comment")
    );
}

#[test]
fn install_keeps_the_settings_indent_and_leaves_unchanged_settings_alone() {
    let mut f = Fixture::new();
    f.targets.harnesses =
        Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]).harnesses;
    f.targets.hooks = true;
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    fs::create_dir_all(f.home.join(".claude")).unwrap();
    let path = f.home.join(".claude/settings.json");
    fs::write(&path, "{\n    \"model\": \"opus\"\n}\n").unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let installed = fs::read_to_string(&path).unwrap();
    assert!(installed.contains("\n    \"hooks\": {\n"), "{installed}");
    assert!(!installed.contains("\n  \"hooks\""), "{installed}");
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), installed);
}

#[test]
fn missing_peer_agent_is_stopped_only_when_the_last_harness_is_removed() {
    let mut f = Fixture::new();
    f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    f.targets.harnesses = Targets::new(
        f.targets.roots.clone(),
        &f.home,
        &[Harness::Claude, Harness::Codex],
    )
    .harnesses;
    f.targets.peer_agent = Some(b"rendered plist\n".to_vec());
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    fs::remove_file(peer_agent_plist(f.targets.roots.state.as_path()).unwrap()).unwrap();
    struct StopReporter(usize);
    impl InstallReporter for StopReporter {
        fn preview(&mut self, _: &[Action]) -> io::Result<()> {
            Ok(())
        }
        fn before_remove(&mut self, action: &Action) -> io::Result<()> {
            if action.selector == "peer-agent" {
                self.0 += 1;
            }
            Ok(())
        }
    }
    let mut reporter = StopReporter(0);
    let codex = f.targets.harnesses.pop().unwrap();
    uninstall(&f.targets, &ctx, &mut reporter).unwrap();
    assert_eq!(reporter.0, 0);
    assert!(
        read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.selector == "peer-agent")
    );
    f.targets.harnesses = vec![codex];
    uninstall(&f.targets, &ctx, &mut reporter).unwrap();
    assert_eq!(reporter.0, 1);
    assert!(
        !read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.selector == "peer-agent")
    );
}

#[test]
fn uninstall_after_home_rename_never_acts_on_old_paths() {
    let mut f = Fixture::new();
    f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    f.targets.peer_agent = Some(b"old home plist\n".to_vec());
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        fs::write(f.source.join("instructions").join(name), b"instructions\n").unwrap();
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let original_target = fs::read_link(f.link()).unwrap();
    let old_state = f.targets.roots.state.as_path().to_path_buf();
    let moved_home = f.base.join("home-moved");
    fs::rename(&f.home, &moved_home).unwrap();
    fs::create_dir_all(f.link().parent().unwrap()).unwrap();
    symlink(&original_target, f.link()).unwrap();
    let roots = RootInputs::new(moved_home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    let mut targets = Targets::new(roots, &moved_home, &[Harness::Codex]);
    targets.peer_agent = Some(b"new home plist\n".to_vec());
    let plist = peer_agent_plist(targets.roots.state.as_path()).unwrap();
    let inspection = inspect_install(&targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert_eq!(
        inspection
            .findings
            .iter()
            .filter(|finding| finding.rule == "relocated_stale_entry")
            .count(),
        6
    );
    struct StopReporter(usize);
    impl InstallReporter for StopReporter {
        fn preview(&mut self, _: &[Action]) -> io::Result<()> {
            Ok(())
        }
        fn before_remove(&mut self, _: &Action) -> io::Result<()> {
            self.0 += 1;
            Ok(())
        }
    }
    let mut reporter = StopReporter(0);
    let result = uninstall(&targets, &ctx, &mut reporter).unwrap();
    assert!(result.findings.iter().any(|finding| {
        finding.rule == "state_root_moved" && finding.path.to_path_buf().unwrap() == old_state
    }));
    assert!(result.findings.iter().any(|finding| {
        finding.rule == "relocated_stale_entry" && finding.path.to_path_buf().unwrap() == f.link()
    }));
    assert!(
        result
            .actions
            .iter()
            .all(|action| { !action.path.to_path_buf().unwrap().starts_with(&f.home) })
    );
    assert_eq!(fs::read_link(f.link()).unwrap(), original_target);
    assert_eq!(fs::read(&plist).unwrap(), b"old home plist\n");
    assert_eq!(reporter.0, 0);
    assert!(
        !read_receipt(&targets)
            .unwrap()
            .unwrap()
            .harnesses
            .contains(&Harness::Codex)
    );
    assert_eq!(installed_binary(&targets).unwrap(), None);
    let release = f.release();
    assert!(matches!(
        Snapshots::capture(&release, &targets),
        Err(InstallError::Conflict(path)) if path == targets.harnesses[0].mcp_file
    ));
    targets.adopt_existing = true;
    for path in [
        moved_home.join(".codex/skills/example"),
        moved_home.join(".codex/AGENTS.md"),
        moved_home.join(".codex/HARNESS.md"),
        targets.bin_dir.join("provenant"),
    ] {
        let snapshots = Snapshots::capture(&release, &targets).unwrap();
        assert!(matches!(
            plan_install(release.clone(), targets.clone(), snapshots),
            Err(InstallError::Conflict(_))
        ));
        assert!(fs::read_link(&path).unwrap().starts_with(&old_state));
        fs::remove_file(path).unwrap();
    }
    // launchctl bootout is a manual process-edge step; the fixture pins that
    // uninstall did not stop it and completes the documented plist cleanup.
    fs::rename(&plist, plist.with_extension("plist.saved")).unwrap();
    let snapshots = Snapshots::capture(&release, &targets).unwrap();
    let plan = plan_install(release, targets.clone(), snapshots).unwrap();
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let inspection = inspect_install(&targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert!(inspection.findings.is_empty(), "{:?}", inspection.findings);
    assert_eq!(fs::read_link(f.link()).unwrap(), original_target);
}

#[test]
fn home_rename_with_custom_state_layout_preserves_old_home_canaries() {
    for state_in_home in [true, false] {
        for (custom_bin, home_alias) in [(false, false), (true, false), (true, true)] {
            let mut f = Fixture::new();
            let state = if state_in_home {
                f.home.join("custom-state")
            } else {
                f.base.join("state")
            };
            let mut inputs = RootInputs::new(f.home.clone(), f.base.join("checkout"));
            inputs.environment_state_dir = Some(state);
            f.targets.roots = inputs.resolve(&Resolver).unwrap();
            if custom_bin {
                f.targets.bin_dir = f.home.join("tools/bin");
            }
            let alias = if home_alias {
                let alias = f.base.join("home-alias");
                symlink(&f.home, &alias).unwrap();
                Some(alias)
            } else {
                None
            };
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let ctx = context(&clock, &cancel);
            let release = f.release();
            let snapshots = Snapshots::capture(&release, &f.targets).unwrap();
            // Keep the caller's HOME spelling after capturing the resolved
            // destinations.
            if let Some(alias) = alias {
                f.targets.home = alias;
            }
            let plan = plan_install(release, f.targets.clone(), snapshots).unwrap();
            install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
            let receipt = read_receipt(&f.targets).unwrap().unwrap();
            let canaries: Vec<_> = receipt
                .entries
                .iter()
                .map(|entry| {
                    let path = entry.path.to_path_buf().unwrap();
                    let value = snapshot_scoped(&path, f.targets.roots.state.as_path())
                        .unwrap()
                        .unwrap()
                        .value;
                    (path, value)
                })
                .collect();
            let moved_home = f.base.join("home-moved");
            fs::rename(&f.home, &moved_home).unwrap();
            for (path, value) in &canaries {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                match value {
                    Value::Link(target) => symlink(target, path).unwrap(),
                    Value::File { bytes, mode } => {
                        fs::write(path, bytes).unwrap();
                        fs::set_permissions(path, fs::Permissions::from_mode(*mode as u32))
                            .unwrap();
                    }
                    Value::Other => panic!("unexpected canary"),
                }
            }
            // Inspection must not parse this recreated old-home file.
            fs::write(f.home.join(".claude.json"), b"invalid JSON\0").unwrap();
            let mut inputs = RootInputs::new(moved_home.clone(), f.base.join("checkout"));
            inputs.environment_state_dir = Some(if state_in_home {
                moved_home.join("custom-state")
            } else {
                f.base.join("state")
            });
            let targets = Targets::new(
                inputs.resolve(&Resolver).unwrap(),
                &moved_home,
                &[Harness::Codex],
            );
            // A --bin-dir install flag is not available to uninstall.
            let inspection =
                inspect_install(&targets, None, "fixture-target", &Smoke, &cancel).unwrap();
            assert!(inspection.error.is_none(), "{:?}", inspection.error);
            assert_eq!(installed_binary(&targets).unwrap(), None);
            let outcome = uninstall(&targets, &ctx, &mut Reporter::default()).unwrap();
            assert!(
                outcome
                    .findings
                    .iter()
                    .any(|finding| finding.rule == "relocated_stale_entry")
            );
            assert_eq!(
                outcome
                    .findings
                    .iter()
                    .any(|finding| finding.rule == "state_root_moved"),
                state_in_home
            );
            assert!(
                outcome.actions.iter().all(|action| !action
                    .path
                    .to_path_buf()
                    .unwrap()
                    .starts_with(&f.home))
            );
            for (path, value) in canaries {
                assert_eq!(
                    snapshot_scoped(&path, targets.roots.state.as_path())
                        .unwrap()
                        .unwrap()
                        .value,
                    value
                );
            }
            assert_eq!(
                fs::read(f.home.join(".claude.json")).unwrap(),
                b"invalid JSON\0"
            );
        }
    }
}

#[test]
fn direct_reinstall_after_home_rename_drops_old_targets() {
    let mut f = Fixture::new();
    f.targets.harnesses = Targets::new(
        f.targets.roots.clone(),
        &f.home,
        &[Harness::Claude, Harness::Codex],
    )
    .harnesses;
    // A mixed target must lose its outside-home selector ownership too when
    // its old-home skills destination is forgotten.
    f.targets.harnesses[0].mcp_file = f.base.join("claude.json");
    f.targets.hooks = true;
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let outside_config = fs::read(f.base.join("claude.json")).unwrap();
    let old_receipt = read_receipt(&f.targets).unwrap().unwrap();
    let moved_home = f.base.join("home-moved");
    fs::rename(&f.home, &moved_home).unwrap();
    let mut targets = Targets::new(f.targets.roots.clone(), &moved_home, &[Harness::Codex]);
    targets.adopt_existing = true;
    targets.hook_specs = f.targets.hook_specs.clone();
    // State stayed outside the home, so these links still resolve. The old
    // paths nevertheless cannot be carried into a fresh receipt.
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &targets).unwrap();
    let plan = plan_install(release, targets.clone(), snapshots).unwrap();
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let receipt = read_receipt(&targets).unwrap().unwrap();
    assert_eq!(receipt.harnesses, [Harness::Codex]);
    assert_eq!(receipt.harness_targets.len(), 1);
    assert!(
        receipt
            .entries
            .iter()
            .all(|entry| !entry.path.to_path_buf().unwrap().starts_with(&f.home))
    );
    assert_eq!(
        fs::read(f.base.join("claude.json")).unwrap(),
        outside_config
    );
    let inspection = inspect_install(&targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert!(inspection.findings.is_empty(), "{:?}", inspection.findings);

    // Earlier reinstalls refreshed the binary while carrying old target
    // metadata. Recover that persisted shape too.
    let mut historical = receipt;
    historical.harnesses.push(Harness::Claude);
    historical
        .harness_targets
        .push(old_receipt.harness_targets[0].clone());
    let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
    fs::write(
        &receipt_path,
        provenant_records::encode(&historical).unwrap(),
    )
    .unwrap();
    let canary = f.home.join(".agents/AGENTS.md");
    fs::create_dir_all(canary.parent().unwrap()).unwrap();
    fs::write(&canary, b"AGENT_FABRIC_LABEL\n").unwrap();
    let inspection = inspect_install(&targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert!(inspection.findings.is_empty(), "{:?}", inspection.findings);
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &targets).unwrap();
    let plan = plan_install(release, targets.clone(), snapshots).unwrap();
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let receipt = read_receipt(&targets).unwrap().unwrap();
    assert_eq!(receipt.harness_targets.len(), 1);
    assert_eq!(fs::read(&canary).unwrap(), b"AGENT_FABRIC_LABEL\n");
}

#[test]
fn check_after_home_rename_never_inspects_old_config_paths() {
    let mut f = Fixture::new();
    f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let moved_home = f.base.join("home-moved");
    fs::rename(&f.home, &moved_home).unwrap();
    fs::create_dir_all(f.home.join(".codex")).unwrap();
    fs::write(f.home.join(".codex/config.toml"), b"invalid TOML\0").unwrap();
    let roots = RootInputs::new(moved_home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    let targets = Targets::new(roots, &moved_home, &[Harness::Codex]);
    let inspection = inspect_install(&targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert!(
        inspection
            .findings
            .iter()
            .any(|finding| finding.rule == "state_root_moved")
    );
}

#[test]
fn reinstall_after_state_rename_requires_manual_link_cleanup() {
    let mut f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let old_state = f.targets.roots.state.as_path().to_path_buf();
    let old_binary = fs::read_link(f.targets.bin_dir.join("provenant")).unwrap();
    let moved_state = f.base.join("state-moved");
    fs::rename(&old_state, &moved_state).unwrap();
    fs::create_dir_all(old_binary.parent().unwrap()).unwrap();
    fs::write(&old_binary, b"old release canary\n").unwrap();
    let mut inputs = RootInputs::new(f.home.clone(), f.base.join("checkout"));
    inputs.environment_state_dir = Some(moved_state);
    f.targets.roots = inputs.resolve(&Resolver).unwrap();
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &f.targets).unwrap();
    assert!(matches!(
        plan_install(release, f.targets.clone(), snapshots),
        Err(InstallError::Conflict(path)) if path == f.link()
    ));
    fs::remove_file(f.link()).unwrap();
    fs::remove_file(f.targets.bin_dir.join("provenant")).unwrap();
    let plan = f.plan();
    assert!(plan.findings().iter().any(|finding| {
        finding.rule == "state_root_moved" && finding.path.to_path_buf().unwrap() == old_state
    }));
    assert_eq!(
        plan.findings()
            .iter()
            .filter(|finding| finding.rule == "relocated_stale_entry")
            .count(),
        2
    );
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    assert!(f.plan().findings().is_empty());
    assert_eq!(
        installed_binary(&f.targets).unwrap(),
        Some(f.targets.bin_dir.join("provenant"))
    );
    assert_eq!(fs::read(&old_binary).unwrap(), b"old release canary\n");
}

#[test]
fn partial_uninstall_after_home_rename_retains_unselected_stale_entries() {
    let mut f = Fixture::new();
    let roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    f.targets = Targets::new(roots, &f.home, &[Harness::Claude, Harness::Codex]);
    f.targets.peer_agent = Some(b"old home plist\n".to_vec());
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let moved_home = f.base.join("home-moved");
    fs::rename(&f.home, &moved_home).unwrap();
    let roots = RootInputs::new(moved_home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    let targets = Targets::new(roots, &moved_home, &[Harness::Codex]);
    let mut environment_targets = targets.clone();
    environment_targets.environment_homes.insert(Harness::Codex);
    let result = uninstall(&environment_targets, &ctx, &mut Reporter::default()).unwrap();
    assert!(result.actions.is_empty());
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.rule == "not_installed")
    );
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.rule == "state_root_moved")
    );
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.rule == "relocated_stale_entry")
    );
    let result = uninstall(&targets, &ctx, &mut Reporter::default()).unwrap();
    assert!(
        result
            .actions
            .iter()
            .all(|action| action.selector == "receipt")
    );
    let bytes = fs::read(targets.roots.state.as_path().join("install/receipt.json")).unwrap();
    let receipt: InstallReceipt =
        provenant_records::decode_v1(bytes.as_slice(), Limits::CONFIG).unwrap();
    assert_eq!(receipt.harnesses, vec![Harness::Claude]);
    assert!(
        receipt
            .entries
            .iter()
            .any(|entry| entry.harness == Some(Harness::Claude))
    );
    assert!(
        receipt
            .entries
            .iter()
            .all(|entry| entry.harness != Some(Harness::Codex))
    );
    assert!(
        receipt
            .entries
            .iter()
            .any(|entry| entry.selector == "peer-agent")
    );
    assert!(read_receipt(&targets).unwrap().is_some());
    let result = uninstall(&targets, &ctx, &mut Reporter::default()).unwrap();
    assert!(result.actions.is_empty());
    assert_eq!(result.findings[0].rule, "not_installed");
}

#[test]
fn moved_receipt_rejects_entries_outside_both_state_roots() {
    let mut f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let mut receipt = read_receipt(&f.targets).unwrap().unwrap();
    let moved_state = f.base.join("state-moved");
    fs::rename(f.targets.roots.state.as_path(), &moved_state).unwrap();
    let mut inputs = RootInputs::new(f.home.clone(), f.base.join("checkout"));
    inputs.environment_state_dir = Some(moved_state);
    f.targets.roots = inputs.resolve(&Resolver).unwrap();
    let skill = receipt
        .entries
        .iter_mut()
        .find(|entry| entry.selector == "skill")
        .unwrap();
    skill.expected = OwnedValue::Link {
        target: AbsolutePath::new(&f.base.join("foreign/skills/example")).unwrap(),
    };
    fs::write(f.receipt(), provenant_records::encode(&receipt).unwrap()).unwrap();
    assert!(matches!(
        read_receipt(&f.targets),
        Err(InstallError::Manifest(
            "receipt entry outside resolved targets"
        ))
    ));
}

#[test]
fn peer_agent_is_previewed_receipted_and_removed_only_on_an_exact_match() {
    for state in ["unchanged", "modified", "missing"] {
        let modified = state == "modified";
        let mut f = Fixture::new();
        f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
            .resolve(&Resolver)
            .unwrap();
        f.targets.peer_agent = Some(b"rendered plist\n".to_vec());
        let plist = peer_agent_plist(f.targets.roots.state.as_path()).unwrap();
        let plan = f.plan();
        assert!(
            plan.actions()
                .iter()
                .any(|action| action.selector == "peer-agent"
                    && action.path.to_path_buf().unwrap() == plist)
        );
        assert!(!plist.exists());
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let context = InstallContext {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30).unwrap(),
        };
        install(&plan, &context, &mut Reporter::default(), &Smoke, false).unwrap();
        assert!(f.plan().actions().is_empty());
        let receipt = read_receipt(&f.targets).unwrap().unwrap();
        assert!(
            receipt
                .entries
                .iter()
                .any(|entry| entry.selector == "peer-agent")
        );
        if modified {
            fs::write(&plist, b"user edited plist\n").unwrap();
            let release = f.release();
            let error = Snapshots::capture(&release, &f.targets)
                .and_then(|snapshots| plan_install(release, f.targets.clone(), snapshots))
                .unwrap_err()
                .to_string();
            assert!(error.contains(plist.to_str().unwrap()), "{error}");
            assert!(error.contains("move it aside"), "{error}");
            assert!(
                error.contains("launchctl bootout gui/$UID/dev.provenant.peer"),
                "{error}"
            );
        }
        if state == "missing" {
            fs::remove_file(&plist).unwrap();
        }
        struct StopReporter(usize);
        impl InstallReporter for StopReporter {
            fn preview(&mut self, _: &[Action]) -> io::Result<()> {
                Ok(())
            }
            fn before_remove(&mut self, action: &Action) -> io::Result<()> {
                if action.selector == "peer-agent" {
                    self.0 += 1;
                }
                Ok(())
            }
        }
        let mut reporter = StopReporter(0);
        let result = uninstall(&f.targets, &context, &mut reporter).unwrap();
        assert_eq!(reporter.0, usize::from(!modified));
        assert_eq!(
            result
                .actions
                .iter()
                .filter(|action| action.selector == "peer-agent")
                .count(),
            usize::from(!modified)
        );
        assert_eq!(plist.exists(), modified);
        assert_eq!(
            result
                .findings
                .iter()
                .any(|finding| finding.rule == "relinquished_modified_entry"
                    && finding.path.to_path_buf().unwrap() == plist),
            modified
        );
    }
}

#[test]
fn inherited_hook_refresh_tracks_event_identity_and_removes_the_old_leaf() {
    let mut f = Fixture::new();
    f.targets.harnesses =
        Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]).harnesses;
    f.targets.hooks = true;
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = InstallContext {
        clock: &clock,
        cancel: &cancel,
        budget: WaitBudget::cli(clock.mono_now(), 30).unwrap(),
    };
    install(&f.plan(), &context, &mut Reporter::default(), &Smoke, false).unwrap();
    let hook = f.targets.harnesses[0].hook_file.clone().unwrap();
    let old = read_receipt(&f.targets)
        .unwrap()
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.selector.starts_with("hook:") && entry.selector.contains("PreToolUse"))
        .unwrap();
    f.targets.hooks = false; // Omitted --hooks inherits the recorded target.
    f.targets.hook_specs[0].event = "ChangedEvent";
    install(&f.plan(), &context, &mut Reporter::default(), &Smoke, false).unwrap();
    assert!(
        crate::hooks::get(&fs::read(&hook).unwrap(), &old.selector)
            .unwrap()
            .is_none()
    );
    assert!(
        read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.selector.contains("ChangedEvent"))
    );
    uninstall(&f.targets, &context, &mut Reporter::default()).unwrap();
    assert!(
        !fs::read_to_string(hook)
            .unwrap()
            .contains("'hook' 'claude'")
    );
}

#[test]
fn edited_inherited_hook_is_relinquished_but_explicit_on_names_recovery() {
    for (edit_command, move_bin) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut f = Fixture::new();
        f.targets.harnesses =
            Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]).harnesses;
        f.targets.hooks = true;
        f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let context = InstallContext {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30).unwrap(),
        };
        install(&f.plan(), &context, &mut Reporter::default(), &Smoke, false).unwrap();
        let hook = f.targets.harnesses[0].hook_file.clone().unwrap();
        let entry = read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| {
                entry.selector.starts_with("hook:") && entry.selector.contains("PreToolUse")
            })
            .unwrap();
        let mut edited = crate::hooks::get(&fs::read(&hook).unwrap(), &entry.selector)
            .unwrap()
            .unwrap();
        if edit_command {
            edited["command"] = serde_json::json!("user edited command");
        } else {
            edited["timeout"] = serde_json::json!(123);
        }
        let bytes =
            crate::hooks::set(&fs::read(&hook).unwrap(), &entry.selector, Some(&edited)).unwrap();
        fs::write(&hook, &bytes).unwrap();
        if !edit_command {
            let receipt_before = fs::read(f.receipt()).unwrap();
            let release = f.release();
            let error = Snapshots::capture(&release, &f.targets)
                .and_then(|snapshots| plan_install(release, f.targets.clone(), snapshots))
                .unwrap_err();
            assert!(error.to_string().contains("--hooks off"), "{error}");
            assert_eq!(fs::read(f.receipt()).unwrap(), receipt_before);
            assert_eq!(fs::read(&hook).unwrap(), bytes);
        }

        f.targets.hooks = false;
        if move_bin {
            f.targets.bin_dir = f.home.join("new-bin");
        }
        for attempt in 0..2 {
            let outcome =
                install(&f.plan(), &context, &mut Reporter::default(), &Smoke, false).unwrap();
            if attempt == 0 {
                assert!(
                    outcome
                        .findings
                        .iter()
                        .any(|finding| finding.rule == "relinquished_modified_entry"
                            && finding.path == entry.path)
                );
            }
            let receipt = read_receipt(&f.targets).unwrap().unwrap();
            assert!(
                !receipt
                    .entries
                    .iter()
                    .any(|owned| owned.selector.starts_with("hook:")
                        && owned.selector.contains("PreToolUse"))
            );
            assert!(
                receipt
                    .entries
                    .iter()
                    .any(|owned| owned.selector.starts_with("hook:"))
            );
            if !move_bin {
                assert_eq!(fs::read(&hook).unwrap(), bytes);
            }
        }
        uninstall(&f.targets, &context, &mut Reporter::default()).unwrap();
        let selector = if edit_command {
            let (event, matcher, _): (String, Option<String>, String) =
                serde_json::from_str(entry.selector.strip_prefix("hook:").unwrap()).unwrap();
            crate::hooks::selector(&event, matcher.as_deref(), "user edited command").unwrap()
        } else {
            entry.selector
        };
        assert_eq!(
            crate::hooks::get(&fs::read(&hook).unwrap(), &selector).unwrap(),
            Some(edited)
        );
    }
}

#[test]
fn historical_peer_agent_is_adopted_or_uninstalled_only_for_recognised_bytes() {
    for (upgrade, recognised) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut f = Fixture::new();
        f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
            .resolve(&Resolver)
            .unwrap();
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let context = InstallContext {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30).unwrap(),
        };
        install(&f.plan(), &context, &mut Reporter::default(), &Smoke, false).unwrap();
        let plist = peer_agent_plist(f.targets.roots.state.as_path()).unwrap();
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(&plist, b"historical plist\n").unwrap();
        fs::set_permissions(&plist, fs::Permissions::from_mode(0o644)).unwrap();
        f.targets.peer_agent = Some(b"new plist\n".to_vec());
        f.targets.peer_agent_previous = recognised.then(|| b"historical plist\n".to_vec());
        if upgrade {
            let release = f.release();
            let plan = Snapshots::capture(&release, &f.targets)
                .and_then(|snapshots| plan_install(release, f.targets.clone(), snapshots));
            if recognised {
                install(
                    &plan.unwrap(),
                    &context,
                    &mut Reporter::default(),
                    &Smoke,
                    false,
                )
                .unwrap();
                assert_eq!(fs::read(&plist).unwrap(), b"new plist\n");
                assert_eq!(
                    fs::metadata(&plist).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                assert!(
                    read_receipt(&f.targets)
                        .unwrap()
                        .unwrap()
                        .entries
                        .iter()
                        .any(|entry| entry.selector == "peer-agent")
                );
            } else {
                let error = plan.unwrap_err().to_string();
                assert!(error.contains(plist.to_str().unwrap()), "{error}");
                assert!(error.contains("move it aside"), "{error}");
                assert!(
                    error.contains("launchctl bootout gui/$UID/dev.provenant.peer"),
                    "{error}"
                );
                assert_eq!(fs::read(&plist).unwrap(), b"historical plist\n");
            }
        } else {
            let result = uninstall(&f.targets, &context, &mut Reporter::default()).unwrap();
            assert_eq!(plist.exists(), !recognised);
            assert_eq!(
                result
                    .findings
                    .iter()
                    .any(|finding| finding.rule == "relinquished_modified_entry"),
                !recognised
            );
        }
    }
}

#[test]
fn failed_peer_stop_keeps_the_plist_and_receipt_for_retry() {
    for missing in [false, true] {
        let mut f = Fixture::new();
        f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
            .resolve(&Resolver)
            .unwrap();
        f.targets.peer_agent = Some(b"rendered plist\n".to_vec());
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let context = InstallContext {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30).unwrap(),
        };
        install(&f.plan(), &context, &mut Reporter::default(), &Smoke, false).unwrap();
        let plist = peer_agent_plist(f.targets.roots.state.as_path()).unwrap();
        if missing {
            fs::remove_file(&plist).unwrap();
        }
        let receipt = fs::read(f.receipt()).unwrap();
        struct FailStop;
        impl InstallReporter for FailStop {
            fn preview(&mut self, _: &[Action]) -> io::Result<()> {
                Ok(())
            }
            fn before_remove(&mut self, action: &Action) -> io::Result<()> {
                if action.selector == "peer-agent" {
                    Err(io::Error::other("bootout failed"))
                } else {
                    Ok(())
                }
            }
        }
        assert!(matches!(
            uninstall(&f.targets, &context, &mut FailStop),
            Err(InstallError::Partial { .. })
        ));
        assert_eq!(fs::read(f.receipt()).unwrap(), receipt);
        assert_eq!(plist.exists(), !missing);
        uninstall(&f.targets, &context, &mut Reporter::default()).unwrap();
        assert!(
            !peer_agent_plist(f.targets.roots.state.as_path())
                .unwrap()
                .exists()
        );
    }
}

#[test]
fn peer_agent_refuses_symlinked_plist_and_parent_before_writes() {
    for parent in [false, true] {
        let mut f = Fixture::new();
        f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
            .resolve(&Resolver)
            .unwrap();
        f.targets.peer_agent = Some(b"rendered plist\n".to_vec());
        let plist = peer_agent_plist(f.targets.roots.state.as_path()).unwrap();
        let outside = f.base.join("outside");
        if parent {
            fs::create_dir(&outside).unwrap();
            fs::create_dir_all(f.home.join("Library")).unwrap();
            symlink(&outside, plist.parent().unwrap()).unwrap();
        } else {
            fs::write(&outside, b"foreign").unwrap();
            fs::create_dir_all(plist.parent().unwrap()).unwrap();
            symlink(&outside, &plist).unwrap();
        }
        let release = f.release();
        let result = Snapshots::capture(&release, &f.targets)
            .and_then(|snapshots| plan_install(release, f.targets.clone(), snapshots));
        assert!(result.is_err());
        assert!(!f.receipt().exists());
        if !parent {
            assert_eq!(fs::read(outside).unwrap(), b"foreign");
        }
    }
}

impl ReleaseSmoke for Smoke {
    fn check(&self, release: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        Ok(VersionInfo {
            source_digest: release.manifest.source_digest,
            asset_digest: release.manifest.asset_digest,
            target: release.manifest.target.clone(),
            toolchain: release.manifest.toolchain.clone(),
            record_schema_version: 1,
            config_schema_version: 1,
        })
    }
}
#[derive(Clone, Copy)]
enum IdentityField {
    SourceDigest,
    AssetDigest,
    Target,
    Toolchain,
}
struct MismatchedSmoke {
    digest: provenant_records::SourceDigest,
    field: IdentityField,
}
impl ReleaseSmoke for MismatchedSmoke {
    fn check(&self, release: &Release, cancel: &CancelToken) -> io::Result<VersionInfo> {
        let mut version = Smoke.check(release, cancel)?;
        if release.manifest.source_digest == self.digest {
            match self.field {
                IdentityField::SourceDigest => {
                    version.source_digest = source_digest(&[SourceInput {
                        name: "src/other.rs",
                        bytes: b"other",
                    }])
                    .unwrap();
                }
                IdentityField::AssetDigest => {
                    version.asset_digest = source_digest(&[SourceInput {
                        name: "assets/other",
                        bytes: b"other",
                    }])
                    .unwrap();
                }
                IdentityField::Target => version.target = "wrong-target".into(),
                IdentityField::Toolchain => version.toolchain = "wrong-toolchain".into(),
            }
        }
        Ok(version)
    }
}
fn context<'a>(clock: &'a SystemClock, cancel: &'a CancelToken) -> InstallContext<'a> {
    InstallContext {
        clock,
        budget: WaitBudget::cli(clock.mono_now(), 1).unwrap(),
        cancel,
    }
}
#[test]
fn dry_run_and_broken_preview_create_nothing() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut reporter = Reporter::default();
    let outcome = install(
        &plan,
        &context(&clock, &cancel),
        &mut reporter,
        &Smoke,
        true,
    )
    .unwrap();
    assert!(outcome.dry_run);
    assert!(reporter.seen > 0);
    assert!(!f.targets.roots.state.as_path().exists());
    assert!(!f.targets.bin_dir.exists());
    assert!(!f.home.join(".codex").exists());
    let error = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter {
            fail: true,
            seen: 0,
        },
        &Smoke,
        false,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "preview output failed");
    assert!(!f.targets.roots.state.as_path().exists());
}
#[test]
fn occupied_skill_is_refused_without_touching_unmanaged_bytes() {
    let f = Fixture::new();
    fs::create_dir_all(f.link()).unwrap();
    fs::write(f.link().join("custom"), b"unmanaged").unwrap();
    let r = f.release();
    let s = Snapshots::capture(&r, &f.targets).unwrap();
    let error = plan_install(r, f.targets.clone(), s).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("unmanaged destination conflicts: {}", f.link().display())
    );
    assert_eq!(fs::read(f.link().join("custom")).unwrap(), b"unmanaged");
    assert!(!f.receipt().exists());
}
#[test]
fn reapplication_and_post_file_crash_do_not_rewrite_exact_desired_entries() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(
        inspect_install(
            &f.targets,
            Some(&f.release()),
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
    let binary = f.targets.bin_dir.join("provenant");
    let before = snapshot(&binary).unwrap();
    let link_before = fs::symlink_metadata(f.link()).unwrap();
    assert!(f.plan().actions().is_empty());
    // Crash after every desired file/link has been published but before receipt.
    fs::remove_file(f.receipt()).unwrap();
    let recovery = f.plan();
    assert_eq!(recovery.actions().len(), 1);
    assert_eq!(recovery.actions().first().unwrap().selector, "receipt");
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert_eq!(snapshot(&binary).unwrap(), before);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::symlink_metadata(f.link()).unwrap().ino(),
        link_before.ino()
    );
    assert!(f.plan().actions().is_empty());
}
#[test]
fn crash_after_any_publication_reapplies_without_a_receipt() {
    let f = Fixture::new();
    let plan = f.plan();
    // A process can die after each individually durable write. Reproduce every
    // prefix using the same publication boundary, without a rollback journal.
    for action in plan.actions().iter().filter(|a| a.selector != "receipt") {
        let path = action.path.to_path_buf().unwrap();
        let desired = &plan.desired.get(&path).unwrap().value;
        publish(&path, desired, false, f.targets.roots.state.as_path()).unwrap();
        let recovery = f.plan();
        assert!(!recovery.actions().iter().any(|a| a.path == action.path));
        assert!(!f.receipt().exists());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
}
#[test]
fn bytes_and_modes_are_verified_and_existing_release_is_immutable() {
    let f = Fixture::new();
    f.release();
    fs::write(f.source.join("bin/provenant"), b"tampered").unwrap();
    assert_eq!(
        Release::read(&f.source, "fixture-target")
            .unwrap_err()
            .to_string(),
        format!(
            "release file bytes or mode differ: {}",
            f.source.join("bin/provenant").display()
        )
    );
    let f = Fixture::new();
    f.release();
    fs::set_permissions(
        f.source.join("bin/provenant"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        matches!(Release::read(&f.source, "fixture-target"), Err(InstallError::Tampered(p)) if p == f.source.join("bin/provenant"))
    );
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let path = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(plan.release.manifest.source_digest.to_string())
        .join("bin/provenant");
    fs::write(&path, b"changed").unwrap();
    let r = f.release();
    let s = Snapshots::capture(&r, &f.targets).unwrap();
    assert_eq!(
        plan_install(r, f.targets.clone(), s)
            .unwrap_err()
            .to_string(),
        format!("release file bytes or mode differ: {}", path.display())
    );
    assert_eq!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .first()
        .unwrap()
        .rule,
        "release_drift"
    );
}
#[test]
fn uninstall_keeps_edited_links_and_unmanaged_siblings_and_binary() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let sibling = f.link().with_file_name("personal");
    fs::write(&sibling, b"personal").unwrap();
    fs::remove_file(f.link()).unwrap();
    symlink("/foreign/skill", f.link()).unwrap();
    let outcome = uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(
        outcome
            .actions
            .iter()
            .all(|action| { action.disposition != Disposition::Remove })
    );
    assert!(
        outcome
            .actions
            .iter()
            .any(|action| action.selector == "receipt")
    );
    assert!(
        outcome
            .findings
            .iter()
            .any(|finding| finding.rule == "relinquished_modified_entry"
                && finding.path.to_path_buf().unwrap() == f.link())
    );
    assert_eq!(
        fs::read_link(f.link()).unwrap(),
        PathBuf::from("/foreign/skill")
    );
    assert_eq!(fs::read(&sibling).unwrap(), b"personal");
    assert!(f.targets.bin_dir.join("provenant").is_file());
}
#[test]
fn drift_between_preview_and_apply_refuses_before_file_writes() {
    let f = Fixture::new();
    let plan = f.plan();
    fs::create_dir_all(&f.targets.bin_dir).unwrap();
    fs::write(f.targets.bin_dir.join("provenant"), b"editor").unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let err = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "destination changed since preview: {}",
            f.targets.bin_dir.join("provenant").display()
        )
    );
    assert!(!f.receipt().exists());
    assert!(!f.targets.roots.state.as_path().join("releases").exists());
}
#[test]
fn explicit_harnesses_platform_links_and_schema_refuse_before_mutation() {
    let mut f = Fixture::new();
    let r = f.release();
    assert_eq!(
        Release::read(&f.source, "other").unwrap_err().to_string(),
        "release platform does not match selected target"
    );
    f.targets.harnesses.clear();
    assert_eq!(
        Snapshots::capture(&r, &f.targets).unwrap_err().to_string(),
        "select at least one harness explicitly, without duplicates"
    );
    assert!(!f.targets.roots.state.as_path().exists());
    let f = Fixture::new();
    f.release();
    fs::remove_file(f.source.join("bin/provenant")).unwrap();
    symlink("/never/open", f.source.join("bin/provenant")).unwrap();
    assert!(matches!(
        Release::read(&f.source, "fixture-target"),
        Err(InstallError::Tampered(_))
    ));
}
// Inject individual publication and confinement failures while retaining the
// concrete filesystem implementation and real handles.
use provenant_core::{CancelToken as TestCancelToken, WaitBudget as TestWaitBudget};
use provenant_os::fs::{Directory, FsError as TestFsError, Result as TestFsResult};
use std::{
    ffi::OsStr,
    sync::{Arc, atomic::Ordering},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultMode {
    NoReads,
    StateMount,
    SourceMount,
    ReceiptPublish,
    InterruptedReleasePublish,
    CancelAfterFirstPublication,
    SkillLinkSync,
}
struct FaultState {
    mode: FaultMode,
    fired: std::sync::atomic::AtomicBool,
    cancel: Option<TestCancelToken>,
}
struct FaultInjection {
    _state: Arc<FaultState>,
}
impl FaultInjection {
    fn new(mode: FaultMode, cancel: Option<TestCancelToken>) -> Self {
        let state = Arc::new(FaultState {
            mode,
            fired: std::sync::atomic::AtomicBool::new(false),
            cancel,
        });
        ACTIVE_FAULT.with(|active| *active.borrow_mut() = Some(state.clone()));
        Self { _state: state }
    }
}
impl Drop for FaultInjection {
    fn drop(&mut self) {
        ACTIVE_FAULT.with(|active| *active.borrow_mut() = None);
    }
}
thread_local! {
    static ACTIVE_FAULT: std::cell::RefCell<Option<Arc<FaultState>>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn filesystem_operation<T>(
    directory: &Directory,
    name: &OsStr,
    bytes: Option<&[u8]>,
    kind: FilesystemOperation,
    operation: impl FnOnce() -> TestFsResult<T>,
) -> TestFsResult<T> {
    match kind {
        FilesystemOperation::Open => fault_directory(name)?,
        FilesystemOperation::Publish => fault_publish(directory, name, bytes.unwrap())?,
        FilesystemOperation::SyncLink => fault_link_sync(directory, name)?,
    }
    let result = operation()?;
    if matches!(kind, FilesystemOperation::Publish) {
        fault_after_publish();
    }
    Ok(result)
}

pub(crate) fn fault_directory(name: &OsStr) -> TestFsResult<()> {
    ACTIVE_FAULT.with(|active| {
        if let Some(state) = active.borrow().as_ref() {
            assert!(
                state.mode != FaultMode::NoReads,
                "unexpected filesystem read"
            );
        }
        if let Some(state) = active.borrow().as_ref()
            && ((state.mode == FaultMode::StateMount && name == "install")
                || (state.mode == FaultMode::SourceMount && name == "skills"))
        {
            return Err(FsError::CrossDevice { path: name.into() });
        }
        Ok(())
    })
}
pub(crate) fn fault_publish(dir: &Directory, name: &OsStr, bytes: &[u8]) -> TestFsResult<()> {
    ACTIVE_FAULT.with(|active| {
        let active = active.borrow();
        let Some(state) = active.as_ref() else {
            return Ok(());
        };
        if state.mode == FaultMode::InterruptedReleasePublish
            && name == "release.json"
            && !state.fired.swap(true, Ordering::AcqRel)
        {
            let temporary = provenant_os::fs::temporary_name()?;
            let mut file = dir.create_new(&temporary)?;
            std::io::Write::write_all(&mut file, bytes).unwrap();
            file.sync()?;
            return Err(TestFsError::Io {
                operation: "publish",
                path: name.into(),
                source: io::Error::other("interrupted release publication"),
            });
        }
        if state.mode == FaultMode::ReceiptPublish
            && name == "receipt.json"
            && !state.fired.swap(true, Ordering::AcqRel)
        {
            return Err(TestFsError::Io {
                operation: "publish",
                path: name.into(),
                source: io::Error::other("injected receipt publication failure"),
            });
        }
        Ok(())
    })
}
pub(crate) fn fault_after_publish() {
    ACTIVE_FAULT.with(|active| {
        if let Some(state) = active.borrow().as_ref()
            && state.mode == FaultMode::CancelAfterFirstPublication
            && !state.fired.swap(true, Ordering::AcqRel)
            && let Some(cancel) = &state.cancel
        {
            cancel.cancel();
        }
    });
}
pub(crate) fn fault_link_sync(dir: &Directory, name: &OsStr) -> TestFsResult<()> {
    ACTIVE_FAULT.with(|active| {
        if let Some(state) = active.borrow().as_ref()
            && state.mode == FaultMode::SkillLinkSync
            && dir
                .read_link(name)
                .is_ok_and(|target| target.components().any(|part| part.as_os_str() == "skills"))
        {
            return Err(TestFsError::Io {
                operation: "sync",
                path: "skill-link-parent".into(),
                source: io::Error::other("injected skill link directory sync failure"),
            });
        }
        Ok(())
    })
}
fn fault_context<'a>(clock: &'a SystemClock, cancel: &'a TestCancelToken) -> InstallContext<'a> {
    InstallContext {
        clock,
        budget: TestWaitBudget::cli(clock.mono_now(), 1).unwrap(),
        cancel,
    }
}

#[test]
fn receipt_publish_failure_reports_partial_actions_and_reapplies() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    let _fault_fs = FaultInjection::new(FaultMode::ReceiptPublish, None);
    let error = install(
        &plan,
        &fault_context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    drop(_fault_fs);
    let InstallError::Partial {
        completed,
        remaining,
        source,
    } = error
    else {
        panic!("expected partial install")
    };
    assert_eq!(completed, plan.actions()[..plan.actions().len() - 1]);
    assert_eq!(remaining, vec![plan.actions().last().unwrap().clone()]);
    assert_eq!(remaining[0].selector, "receipt");
    assert_eq!(
        source.to_string(),
        "filesystem publish failed for \"receipt.json\": injected receipt publication failure (Other)"
    );
    assert!(!f.receipt().exists());

    let recovery = f.plan();
    assert_eq!(recovery.actions().len(), 1);
    assert_eq!(recovery.actions()[0].selector, "receipt");
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
    assert!(
        inspect_install(
            &f.targets,
            Some(&f.release()),
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
}

#[test]
fn cancelled_apply_reports_completed_and_remaining_actions() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    let _fault_fs =
        FaultInjection::new(FaultMode::CancelAfterFirstPublication, Some(cancel.clone()));
    let error = install(
        &plan,
        &fault_context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    let InstallError::Partial {
        completed,
        remaining,
        source,
    } = error
    else {
        panic!("expected partial install")
    };
    assert_eq!(completed, vec![plan.actions()[0].clone()]);
    assert_eq!(remaining, plan.actions()[1..]);
    assert!(matches!(
        *source,
        InstallError::Fs(TestFsError::Cancelled { .. })
    ));
    assert!(!f.receipt().exists());

    let recovery = f.plan();
    let retry_cancel = TestCancelToken::new();
    install(
        &recovery,
        &context(&clock, &retry_cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
}

#[test]
fn visible_skill_link_with_failed_directory_sync_is_reconciled_on_retry() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    let _fault_fs = FaultInjection::new(FaultMode::SkillLinkSync, None);
    let error = install(
        &plan,
        &fault_context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    drop(_fault_fs);
    let InstallError::Partial {
        completed,
        remaining,
        source,
    } = error
    else {
        panic!("expected partial install")
    };
    assert!(completed.iter().any(|action| action.selector == "binary"));
    assert_eq!(remaining.first().unwrap().selector, "skill");
    assert_eq!(remaining.last().unwrap().selector, "receipt");
    assert!(matches!(
        *source,
        InstallError::Fs(TestFsError::Io {
            operation: "sync",
            ..
        })
    ));
    assert!(
        fs::symlink_metadata(f.link())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!f.receipt().exists());

    let recovery = f.plan();
    assert_eq!(recovery.actions().len(), 1);
    assert_eq!(recovery.actions()[0].selector, "receipt");
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
}

#[test]
fn adopt_existing_binary_does_not_adopt_a_symlink() {
    let f = Fixture::new();
    fs::create_dir_all(&f.targets.bin_dir).unwrap();
    let binary = f.targets.bin_dir.join("provenant");
    symlink("/foreign/provenant", &binary).unwrap();
    let mut targets = f.targets.clone();
    targets.adopt_existing = true;
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &targets).unwrap();
    let error = plan_install(release, targets, snapshots).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("unmanaged destination conflicts: {}", binary.display())
    );
    assert_eq!(
        fs::read_link(&binary).unwrap(),
        PathBuf::from("/foreign/provenant")
    );
    assert!(!f.receipt().exists());
}

#[test]
fn check_install_smokes_installed_and_requested_release_identity() {
    let f = Fixture::new();
    let installed = f.release();
    let installed_digest = installed.manifest.source_digest;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();

    for field in [
        IdentityField::SourceDigest,
        IdentityField::AssetDigest,
        IdentityField::Target,
        IdentityField::Toolchain,
    ] {
        let smoke = MismatchedSmoke {
            digest: installed_digest,
            field,
        };
        let error = inspect_install(&f.targets, None, "fixture-target", &smoke, &cancel)
            .unwrap()
            .error
            .unwrap();
        assert_eq!(
            error.to_string(),
            "invalid release manifest: binary identity differs from release manifest"
        );
    }

    let other = Fixture::new();
    let other_digest = source_digest(&[SourceInput {
        name: "src/lib.rs",
        bytes: b"different fixture",
    }])
    .unwrap();
    let other_source = package_fixture_release(&other.source, other_digest);
    for field in [
        IdentityField::SourceDigest,
        IdentityField::AssetDigest,
        IdentityField::Target,
        IdentityField::Toolchain,
    ] {
        let smoke = MismatchedSmoke {
            digest: other_source.manifest.source_digest,
            field,
        };
        let error = inspect_install(
            &f.targets,
            Some(&other_source),
            "fixture-target",
            &smoke,
            &cancel,
        )
        .unwrap()
        .error
        .unwrap();
        assert_eq!(
            error.to_string(),
            "invalid release manifest: binary identity differs from release manifest"
        );
    }
}

#[test]
fn check_install_compares_the_requested_source_digest() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let other_digest = source_digest(&[SourceInput {
        name: "src/lib.rs",
        bytes: b"different fixture",
    }])
    .unwrap();
    let other = Fixture::new();
    let other_source = package_fixture_release(&other.source, other_digest);
    let findings = inspect_install(
        &f.targets,
        Some(&other_source),
        "fixture-target",
        &Smoke,
        &CancelToken::new(),
    )
    .unwrap()
    .findings;
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.rule == "source_digest")
            .count(),
        1
    );
    assert_eq!(
        findings
            .iter()
            .find(|finding| finding.rule == "source_digest")
            .unwrap()
            .path
            .to_path_buf()
            .unwrap(),
        f.receipt()
    );
}

#[test]
fn uninstall_reconciles_entries_removed_before_receipt_publication() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    fs::remove_file(f.link()).unwrap();
    let result = uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .actions
            .iter()
            .map(|a| a.selector.as_str())
            .collect::<Vec<_>>(),
        ["config", "receipt"]
    );
    assert!(
        read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .all(|e| e.harness != Some(Harness::Codex))
    );
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
    // Once removal is reconciled, a later recreated matching link is unmanaged.
    let release = f.release();
    let path = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string())
        .join("skills/example");
    symlink(&path, f.link()).unwrap();
    assert!(
        uninstall(
            &f.targets,
            &context(&clock, &cancel),
            &mut Reporter::default()
        )
        .unwrap()
        .actions
        .is_empty()
    );
    assert_eq!(fs::read_link(f.link()).unwrap(), path);
}

#[test]
fn regular_user_instructions_are_preserved_with_manual_include_advice() {
    let f = Fixture::new();
    let assets = f.source.join("instructions");
    fs::create_dir_all(&assets).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let p = assets.join(name);
        fs::write(&p, b"release instructions").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let instruction = f.home.join(".codex/AGENTS.md");
    fs::create_dir_all(instruction.parent().unwrap()).unwrap();
    fs::write(&instruction, b"my instructions").unwrap();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let result = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert_eq!(fs::read(&instruction).unwrap(), b"my instructions");
    assert_eq!(result.manual_includes.len(), 1);
    assert_eq!(
        result.manual_includes[0].path.to_path_buf().unwrap(),
        instruction
    );
    assert!(
        !read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|e| e.path.to_path_buf().unwrap() == instruction)
    );
}

#[test]
fn manual_include_advice_is_omitted_when_shared_agents_is_installed_and_referenced() {
    let f = Fixture::new();
    let assets = f.source.join("instructions");
    fs::create_dir_all(&assets).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let p = assets.join(name);
        fs::write(&p, b"release instructions").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let instruction = f.home.join(".codex/AGENTS.md");
    fs::create_dir_all(instruction.parent().unwrap()).unwrap();
    let text = b"See ~/.agents/AGENTS.md for shared instructions";
    fs::write(&instruction, text).unwrap();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let result = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(
        result.manual_includes.is_empty(),
        "{:?}",
        result.manual_includes
    );
    assert_eq!(fs::read(&instruction).unwrap(), text);
}

#[test]
fn completed_immutable_release_missing_a_file_is_not_repaired() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let missing = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(plan.release.manifest.source_digest.to_string())
        .join("bin/provenant");
    fs::remove_file(&missing).unwrap();
    assert_eq!(
        Snapshots::capture(&f.release(), &f.targets)
            .unwrap_err()
            .to_string(),
        format!("release file bytes or mode differ: {}", missing.display())
    );
    assert!(!missing.exists());
}
#[test]
fn receipt_cannot_omit_binary_ownership_or_bless_a_foreign_binary_digest() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let mut receipt = read_receipt(&f.targets).unwrap().unwrap();
    let original = receipt.clone();
    receipt.entries.clear();
    fs::write(f.receipt(), provenant_records::encode(&receipt).unwrap()).unwrap();
    assert_eq!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .error
        .unwrap()
        .to_string(),
        "invalid release manifest: receipt requires one binary entry"
    );
    let mut receipt = original;
    for entry in &mut receipt.entries {
        if entry.selector == "binary" {
            entry.expected = OwnedValue::Link {
                target: AbsolutePath::new(Path::new("/foreign/provenant")).unwrap(),
            };
        }
    }
    fs::write(f.receipt(), provenant_records::encode(&receipt).unwrap()).unwrap();
    assert_eq!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .error
        .unwrap()
        .to_string(),
        "invalid release manifest: receipt entry outside resolved targets"
    );
}

impl Fixture {
    fn release_variant(&self, name: &str, skills: &[&str]) -> Release {
        let source = self.base.join(format!("staged-{name}"));
        for skill in skills {
            fs::create_dir_all(source.join("skills").join(skill)).unwrap();
        }
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(
            source.join("bin/provenant"),
            format!("fixture executable {name}"),
        )
        .unwrap();
        fs::set_permissions(
            source.join("bin/provenant"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        for skill in skills {
            let path = source.join("skills").join(skill).join("SKILL.md");
            fs::write(&path, format!("---\nname: {skill}\n---\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let digest = source_digest(&[SourceInput {
            name: "src/lib.rs",
            bytes: name.as_bytes(),
        }])
        .unwrap();
        package_fixture_release(&source, digest)
    }

    fn plan_for(&self, release: Release) -> InstallPlan {
        let snapshots = Snapshots::capture(&release, &self.targets).unwrap();
        plan_install(release, self.targets.clone(), snapshots).unwrap()
    }

    fn skill_link(&self, name: &str) -> PathBuf {
        self.home.join(".codex/skills").join(name)
    }
}

#[test]
fn upgrade_removes_only_unchanged_dropped_links_and_recovers_after_first_replacement() {
    let f = Fixture::new();
    let release_a = f.release_variant("a", &["common", "obsolete", "edited"]);
    let release_b = f.release_variant("b", &["common", "added"]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let plan_a = f.plan_for(release_a.clone());
    install(
        &plan_a,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();

    fs::remove_file(f.skill_link("edited")).unwrap();
    symlink("/foreign/edited", f.skill_link("edited")).unwrap();
    let unmanaged = f.skill_link("personal");
    fs::write(&unmanaged, b"personal").unwrap();

    let plan_b = f.plan_for(release_b.clone());
    let obsolete = AbsolutePath::new(&f.skill_link("obsolete")).unwrap();
    assert!(
        plan_b
            .actions()
            .iter()
            .any(|action| { action.path == obsolete && action.disposition == Disposition::Remove })
    );
    assert!(!plan_b.actions().iter().any(|action| {
        action.path == AbsolutePath::new(&f.skill_link("edited")).unwrap()
            && action.disposition == Disposition::Remove
    }));

    // Publish through the same atomic boundary until the first harness link is
    // replaced, then leave the old receipt behind as a crashed process would.
    let first_replacement = plan_b
        .actions
        .iter()
        .position(|action| {
            action.selector == "skill"
                && action.path.to_path_buf().unwrap() == f.skill_link("common")
        })
        .unwrap();
    for action in &plan_b.actions[..=first_replacement] {
        let path = action.path.to_path_buf().unwrap();
        if action.disposition == Disposition::Remove {
            break;
        }
        let desired = &plan_b.desired.get(&path).unwrap().value;
        let replace = plan_b.before.entries.get(&path).unwrap().is_some();
        publish(&path, desired, replace, f.targets.roots.state.as_path()).unwrap();
    }
    let receipt_after_crash: InstallReceipt =
        provenant_records::decode_v1(fs::read(f.receipt()).unwrap().as_slice(), Limits::CONFIG)
            .unwrap();
    assert_eq!(
        receipt_after_crash.source_digest,
        release_a.manifest.source_digest
    );

    let recovery = f.plan_for(release_b.clone());
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(fs::symlink_metadata(f.skill_link("obsolete")).is_err());
    assert_eq!(
        fs::read_link(f.skill_link("edited")).unwrap(),
        PathBuf::from("/foreign/edited")
    );
    assert_eq!(fs::read(&unmanaged).unwrap(), b"personal");
    let receipt: InstallReceipt =
        provenant_records::decode_v1(fs::read(f.receipt()).unwrap().as_slice(), Limits::CONFIG)
            .unwrap();
    assert_eq!(receipt.source_digest, release_b.manifest.source_digest);
    assert!(receipt.entries.iter().all(|entry| entry.path != obsolete));
    assert!(f.plan_for(release_b).actions().is_empty());
}

#[test]
fn bin_dir_upgrade_removes_only_the_unchanged_previous_shim() {
    for changed in [false, true] {
        let mut f = Fixture::new();
        let release_a = f.release_variant("a", &["common"]);
        let release_b = f.release_variant("b", &["common"]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        install(
            &f.plan_for(release_a),
            &context(&clock, &cancel),
            &mut Reporter::default(),
            &Smoke,
            false,
        )
        .unwrap();
        let old_binary = f.targets.bin_dir.join("provenant");
        if changed {
            fs::remove_file(&old_binary).unwrap();
            symlink("/personal/provenant", &old_binary).unwrap();
        }
        f.targets.bin_dir = f.home.join("new-bin");
        let plan = f.plan_for(release_b.clone());
        assert_eq!(
            plan.actions().iter().any(|action| {
                action.path == AbsolutePath::new(&old_binary).unwrap()
                    && action.disposition == Disposition::Remove
            }),
            !changed
        );
        install(
            &plan,
            &context(&clock, &cancel),
            &mut Reporter::default(),
            &Smoke,
            false,
        )
        .unwrap();
        if changed {
            assert_eq!(
                fs::read_link(&old_binary).unwrap(),
                PathBuf::from("/personal/provenant")
            );
        } else {
            assert!(fs::symlink_metadata(&old_binary).is_err());
        }
        assert!(f.targets.bin_dir.join("provenant").is_file());
        let receipt = read_receipt(&f.targets).unwrap().unwrap();
        assert_eq!(receipt.bin_dir.to_path_buf().unwrap(), f.targets.bin_dir);
        assert!(
            receipt
                .entries
                .iter()
                .all(|entry| entry.path != AbsolutePath::new(&old_binary).unwrap())
        );
        assert!(f.plan_for(release_b).actions().is_empty());
    }
}

#[test]
fn receipt_targets_cover_two_harnesses_and_uninstall_uses_recorded_paths() {
    let mut f = Fixture::new();
    let custom = f.home.join("custom");
    let codex_skills = custom.join("codex/skills");
    let codex_instruction = custom.join("codex/AGENTS.md");
    let claude_skills = custom.join("claude/skills");
    let claude_instruction = custom.join("claude/CLAUDE.md");
    f.targets.bin_dir = custom.join("bin");
    f.targets.harnesses[0].skills_dir = codex_skills.clone();
    f.targets.harnesses[0].instruction = Some(codex_instruction.clone());
    f.targets.harnesses.push(HarnessTarget {
        harness: Harness::Claude,
        mcp_file: f.home.join(".claude.json"),
        hook_file: Some(f.home.join(".claude/settings.json")),
        skills_dir: claude_skills.clone(),
        instruction: Some(claude_instruction.clone()),
    });
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let path = f.source.join("instructions").join(name);
        fs::write(path, b"release instruction").unwrap();
        fs::set_permissions(
            f.source.join("instructions").join(name),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    let release = f.release();
    let plan = f.plan_for(release);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();

    let receipt = read_receipt(&f.targets).unwrap().unwrap();
    assert_eq!(receipt.bin_dir.to_path_buf().unwrap(), f.targets.bin_dir);
    assert_eq!(receipt.harnesses, vec![Harness::Claude, Harness::Codex]);
    assert_eq!(
        receipt
            .harness_targets
            .iter()
            .find(|target| target.harness == Harness::Claude)
            .unwrap()
            .skills_dir
            .to_path_buf()
            .unwrap(),
        claude_skills
    );

    // Current CLI path resolution can differ from install time. Check and
    // uninstall must use the recorded locations from the receipt.
    let mut changed = f.targets.clone();
    changed.bin_dir = custom.join("changed-bin");
    for target in &mut changed.harnesses {
        target.skills_dir = custom
            .join("changed")
            .join(format!("{:?}/skills", target.harness));
        target.instruction = None;
    }
    assert!(
        inspect_install(
            &changed,
            None,
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );

    changed.harnesses = vec![HarnessTarget {
        harness: Harness::Claude,
        mcp_file: f.home.join(".claude.json"),
        hook_file: Some(f.home.join(".claude/settings.json")),
        skills_dir: custom.join("changed/claude/skills"),
        instruction: None,
    }];
    uninstall(
        &changed,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(fs::symlink_metadata(claude_skills.join("example")).is_err());
    assert!(fs::symlink_metadata(codex_skills.join("example")).is_ok());
    assert!(fs::symlink_metadata(claude_instruction).is_err());
    assert!(fs::symlink_metadata(codex_instruction).is_ok());
    let shrunk = read_receipt(&changed).unwrap().unwrap();
    assert_eq!(shrunk.harnesses, vec![Harness::Codex]);
    assert_eq!(shrunk.harness_targets.len(), 1);
    assert_eq!(shrunk.harness_targets[0].harness, Harness::Codex);
    assert!(
        inspect_install(
            &changed,
            None,
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
}

#[test]
fn receipt_targets_drive_check_and_uninstall_and_uninstall_shrinks_harnesses() {
    let f = Fixture::new();
    let release = f.release();
    let plan = f.plan_for(release.clone());
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();

    let receipt = read_receipt(&f.targets).unwrap().unwrap();
    assert_eq!(receipt.bin_dir.to_path_buf().unwrap(), f.targets.bin_dir);
    assert_eq!(receipt.harnesses, vec![Harness::Codex]);
    assert_eq!(
        receipt.harness_targets[0].skills_dir.to_path_buf().unwrap(),
        f.home.join(".codex/skills")
    );
    assert_eq!(
        receipt.harness_targets[0]
            .instruction
            .as_ref()
            .unwrap()
            .to_path_buf()
            .unwrap(),
        f.home.join(".codex/AGENTS.md")
    );
    let mut changed_targets = f.targets.clone();
    changed_targets.bin_dir = f.home.join("elsewhere/bin");
    changed_targets.harnesses[0].skills_dir = f.home.join("elsewhere/skills");
    changed_targets.harnesses[0].instruction = Some(f.home.join("elsewhere/AGENTS.md"));
    assert!(
        inspect_install(
            &changed_targets,
            Some(&release),
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
    let outcome = uninstall(
        &changed_targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(outcome.actions.iter().any(|action| {
        action.disposition == Disposition::Remove
            && action.path == AbsolutePath::new(&f.link()).unwrap()
    }));
    assert!(!f.link().exists());
    let receipt = read_receipt(&changed_targets).unwrap().unwrap();
    assert!(receipt.harnesses.is_empty());
    assert!(receipt.harness_targets.is_empty());
    assert_eq!(
        receipt
            .entries
            .iter()
            .filter(|entry| entry.selector == "binary")
            .count(),
        1
    );
}

#[test]
fn a_completed_release_with_a_different_manifest_is_a_typed_conflict() {
    let f = Fixture::new();
    let first = f.release();
    let first_digest = CanonicalV1::file_digest(&first.manifest_bytes);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan_for(first.clone()),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();

    let mut second = f.release();
    second.manifest.toolchain = "different-toolchain".into();
    second.manifest_bytes = provenant_records::encode(&second.manifest).unwrap();
    let second_digest = CanonicalV1::file_digest(&second.manifest_bytes);
    let error = Snapshots::capture(&second, &f.targets).unwrap_err();
    assert!(matches!(
        error,
        InstallError::ReleaseConflict {
            existing_manifest_digest,
            requested_manifest_digest,
            ..
        } if existing_manifest_digest == first_digest
            && requested_manifest_digest == second_digest
    ));
    assert!(error.to_string().contains(&first_digest.to_string()));
    assert!(error.to_string().contains(&second_digest.to_string()));
    assert!(error.to_string().contains("select a new source digest"));
}

#[test]
fn interrupted_release_publish_leaves_no_manifest_and_reapplies_with_real_staging_file() {
    let f = Fixture::new();
    let release = f.release();
    let release_root = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string());
    let binary = release_root.join("bin/provenant");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, b"interrupted old bytes").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let _faults = FaultInjection::new(FaultMode::InterruptedReleasePublish, None);
    let error = install(
        &f.plan_for(release.clone()),
        &fault_context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    assert!(matches!(error, InstallError::Partial { .. }));
    assert!(!release_root.join("release.json").exists());
    // Every payload was published before the failed completion marker.
    for (path, (bytes, _)) in &release.files {
        assert_eq!(fs::read(release_root.join(path)).unwrap(), *bytes);
    }
    let staging = fs::read_dir(&release_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".provenant-staging.")
        })
        .unwrap();
    assert!(staging.is_file());

    install(
        &f.plan_for(release.clone()),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(!staging.exists());
    Release::read(&release_root, &release.manifest.target).unwrap();
}

#[test]
fn capture_does_not_unlink_an_active_install_staging_file() {
    let f = Fixture::new();
    let release = f.release();
    let root = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string());
    let install_dir = directory(&f.targets.roots.state.as_path().join("install"), true).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let _lock = install_dir
        .lock(
            OsStr::new("install.lock"),
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &cancel,
            &clock,
        )
        .unwrap();
    let binary_dir = directory(&root.join("bin"), true).unwrap();
    let temporary = provenant_os::fs::temporary_name().unwrap();
    let mut file = binary_dir.create_new(&temporary).unwrap();
    std::io::Write::write_all(&mut file, b"live staged bytes").unwrap();
    file.sync().unwrap();

    // A second caller can preview while the first writer owns the lock. Its
    // inventory must leave that writer's open staging entry intact.
    Snapshots::capture(&release, &f.targets).unwrap();
    binary_dir
        .rename_noreplace(&temporary, &binary_dir, OsStr::new("provenant"))
        .unwrap();
    assert_eq!(
        fs::read(root.join("bin/provenant")).unwrap(),
        b"live staged bytes"
    );
}

#[test]
fn completed_release_tolerates_only_supervisor_staging_names() {
    let f = Fixture::new();
    let release = f.release();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan_for(release.clone()),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let root = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string());
    let staging = root.join(provenant_os::fs::temporary_name().unwrap());
    fs::write(&staging, b"leftover staging").unwrap();
    f.plan_for(release.clone());
    Release::read(&root, &release.manifest.target).unwrap();
    assert!(staging.exists());
    let foreign = root.join(".provenant-staging.not-supervisor");
    fs::write(&foreign, b"foreign").unwrap();
    assert!(matches!(Snapshots::capture(&release, &f.targets),
        Err(InstallError::Tampered(path)) if path == foreign));
}

struct WrongSmoke;
impl ReleaseSmoke for WrongSmoke {
    fn check(&self, release: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        let mut info = Smoke.check(release, &CancelToken::new())?;
        info.target = "wrong-target".into();
        Ok(info)
    }
}

#[test]
fn install_refuses_a_binary_whose_version_identity_differs_from_manifest() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let error = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &WrongSmoke,
        false,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid release manifest: binary identity differs from release manifest"
    );
    assert!(!f.targets.roots.state.as_path().exists());
    assert!(!f.targets.bin_dir.exists());
}

#[test]
fn harness_cutover_previews_mcp_and_dry_run_creates_nothing() {
    let f = Fixture::new();
    let plan = f.plan();
    assert!(plan.actions().iter().any(|a| a.selector == "config"));
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        true,
    )
    .unwrap();
    assert!(!f.home.join(".codex").exists());
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn mcp_selector_ownership_preserves_user_edits_and_uninstalls_only_unchanged() {
    let f = Fixture::new();
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let foreign =
        "# user comment\nmodel = 'user-model'\n[mcp_servers.other]\ncommand = 'foreign' # keep\n";
    fs::write(&config, foreign).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.starts_with("# user comment\nmodel = 'user-model'\n"));
    assert!(text.contains("[mcp_servers.other]\ncommand = 'foreign' # keep\n"));
    assert!(text.contains("--state-dir"));
    let edited = text.replace("user-model", "changed-model");
    fs::write(&config, &edited).unwrap();
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
    assert!(f.plan().actions().is_empty());
    uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    let after = fs::read_to_string(&config).unwrap();
    assert!(after.starts_with("# user comment\nmodel = 'changed-model'\n"));
    assert!(after.contains("[mcp_servers.other]\ncommand = 'foreign' # keep\n"));
    assert!(!after.contains("mcp_servers.fabric"));
    assert!(f.targets.bin_dir.join("provenant").is_symlink());
}

#[test]
fn hooks_on_and_symlinked_configs_refuse_before_any_write() {
    let mut f = Fixture::new();
    f.targets.hooks = true;
    let release = f.release();
    assert_eq!(
        Snapshots::capture(&release, &f.targets)
            .unwrap_err()
            .to_string(),
        "hook registration refused: no fixture-backed HookSpec is available"
    );
    assert!(!f.targets.roots.state.as_path().exists());
    f.targets.hooks = false;
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let regular = f.home.join("foreign.toml");
    fs::write(&regular, "model='keep'\n").unwrap();
    symlink(&regular, &config).unwrap();
    let err = Snapshots::capture(&release, &f.targets).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "symlinked or non-regular harness config refused: {}; replace the link with a regular copy, then rerun install",
            config.display()
        )
    );
    assert_eq!(fs::read_to_string(&regular).unwrap(), "model='keep'\n");
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn mcp_conflict_does_not_adopt_foreign_env_or_create_state() {
    let mut f = Fixture::new();
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let foreign =
        "[mcp_servers.fabric]\ncommand='foreign'\n[mcp_servers.fabric.env]\nTOKEN='do-not-copy'\n";
    fs::write(&config, foreign).unwrap();
    f.targets.adopt_existing = true;
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &f.targets);
    assert_eq!(
        snapshots.unwrap_err().to_string(),
        format!("unmanaged destination conflicts: {}", config.display())
    );
    assert_eq!(fs::read_to_string(config).unwrap(), foreign);
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn five_client_install_preserves_foreign_hooks_comments_and_reapplies() {
    let mut f = Fixture::new();
    f.targets = Targets::new(
        f.targets.roots.clone(),
        &f.home,
        &[
            Harness::Claude,
            Harness::Codex,
            Harness::Cursor,
            Harness::Agy,
            Harness::Opencode,
        ],
    );
    for target in &f.targets.harnesses {
        fs::create_dir_all(&target.skills_dir).unwrap();
        symlink(
            f.home.join("personal-skills/autopilot"),
            target.skills_dir.join("autopilot"),
        )
        .unwrap();
    }
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let p = f.source.join("instructions").join(name);
        fs::write(&p, b"PROVENANT_AGENT_LABEL instructions\n").unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let hook = f.home.join(".claude/settings.json");
    let herdr = r#"{"type":"command", "command":"herdr capture", "timeout":2}"#;
    let before = format!(
        "{{\n // herdr stays\n \"user\": 7,\n \"hooks\": {{\"PreCompact\":[{{\"hooks\":[{herdr}]}}]}}\n}}\n"
    );
    fs::write(&hook, &before).unwrap();
    let codex_hooks = f.home.join(".codex/hooks.json");
    let codex_foreign = b"{\n // Herdr\n \"hooks\":{\"Stop\":[{\"command\":\"herdr\"}]}\n}\n";
    fs::write(&codex_hooks, codex_foreign).unwrap();
    let opencode = f.home.join(".config/opencode/opencode.jsonc");
    fs::write(
        &opencode,
        "{// foreign comment\n\"instructions\":[\"foreign.md\",],\n\"theme\": \"user\",}\n",
    )
    .unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    let plan = f.plan();
    assert!(plan.actions().iter().any(|a| !a.edits.is_empty()));
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    for target in &f.targets.harnesses {
        assert_eq!(
            fs::read_link(target.skills_dir.join("autopilot")).unwrap(),
            f.home.join("personal-skills/autopilot")
        );
        assert!(target.mcp_file.is_file());
        if let Some(instruction) = &target.instruction {
            assert!(
                fs::read_link(instruction)
                    .unwrap()
                    .ends_with("instructions/AGENTS.md")
            );
        }
    }
    assert_eq!(fs::read_to_string(&hook).unwrap(), before);
    assert_eq!(fs::read(codex_hooks).unwrap(), codex_foreign);
    assert!(
        fs::read_to_string(&opencode)
            .unwrap()
            .contains("// foreign comment")
    );
    assert!(f.plan().actions().is_empty());
    let inspection = inspect_install(&f.targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert!(inspection.findings.is_empty(), "{:?}", inspection.findings);
    uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    let after = fs::read_to_string(opencode).unwrap();
    assert!(after.contains("foreign.md"));
    assert!(after.contains("// foreign comment"));
    assert!(!after.contains("instructions/AGENTS.md"));
}

#[test]
fn duplicate_owned_instruction_values_refuse_before_writes() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Opencode]);
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    fs::write(
        f.source.join("instructions/AGENTS.md"),
        "fixture instructions",
    )
    .unwrap();
    let release = f.release();
    let instruction = release_root(
        f.targets.roots.state.as_path(),
        release.manifest.source_digest,
    )
    .join("instructions/AGENTS.md");
    let config = f.targets.harnesses[0].mcp_file.clone();
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let bytes =
        serde_json::to_vec(&serde_json::json!({"instructions":[instruction,instruction]})).unwrap();
    fs::write(&config, &bytes).unwrap();
    assert_eq!(
        Snapshots::capture(&release, &f.targets)
            .unwrap_err()
            .to_string(),
        "invalid harness configuration: duplicate owned instruction"
    );
    assert_eq!(fs::read(config).unwrap(), bytes);
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn codex_inline_table_retains_foreign_values_and_comments() {
    let mut f = Fixture::new();
    f.targets.adopt_existing = true;
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let before = "# keep top\nmcp_servers = { other = { command = 'foreign' }, fabric = { command = 'old', env = {} } } # keep tail\nmodel = 'user'\n";
    fs::write(&config, before).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let after = fs::read_to_string(config).unwrap();
    assert!(after.starts_with("# keep top\n"));
    assert!(after.contains("other = { command = 'foreign' }"));
    assert!(after.contains("# keep tail\nmodel = 'user'\n"));
    assert!(f.plan().actions().is_empty());
}

#[test]
fn explicit_user_install_paths_win_over_harness_environment_even_when_default() {
    let mut f = Fixture::new();
    let state = f.targets.roots.state.as_path();
    fs::create_dir_all(state).unwrap();
    let config = format!(
        "schema_version=1\n[install.targets.codex]\nhome={}\nconfig={}\n",
        serde_json::to_string(f.home.join(".codex").to_str().unwrap()).unwrap(),
        serde_json::to_string(f.home.join(".codex/config.toml").to_str().unwrap()).unwrap()
    );
    fs::write(state.join("config.toml"), &config).unwrap();
    crate::apply_install_config_with_domains(&mut f.targets, &crate::domains::NO_DOMAINS).unwrap();
    f.targets
        .apply_environment(None, Some(&f.home.join("alternate")))
        .unwrap();
    assert_eq!(
        f.targets.harnesses[0].skills_dir,
        f.home.join(".codex/skills")
    );
    assert_eq!(
        f.targets.harnesses[0].mcp_file,
        f.home.join(".codex/config.toml")
    );
    let loaded = crate::load(&f.targets.roots, Some(config.as_bytes())).unwrap();
    crate::test_support::assert_leaf_origins(&loaded);
}

#[test]
fn claude_config_dir_moves_its_mcp_registration() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]);
    assert_eq!(f.targets.harnesses[0].mcp_file, f.home.join(".claude.json"));
    let alternate = f.home.join(".claude-alt");
    f.targets.apply_environment(Some(&alternate), None).unwrap();
    assert_eq!(
        f.targets.harnesses[0].mcp_file,
        alternate.join(".claude.json")
    );
    assert_eq!(f.targets.harnesses[0].skills_dir, alternate.join("skills"));
}

#[test]
fn claude_config_dirs_accumulate_and_uninstall_removes_only_the_selected_dir() {
    let mut f = Fixture::new();
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let path = f.source.join("instructions").join(name);
        fs::write(&path, b"release instruction").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let (roots, home) = (f.targets.roots.clone(), f.home.clone());
    let claude = |dir: Option<&Path>| {
        let mut targets = Targets::new(roots.clone(), &home, &[Harness::Claude]);
        targets.hooks = true;
        targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
        targets.apply_environment(dir, None).unwrap();
        targets
    };
    let (default, alternate) = (home.join(".claude"), home.join(".claude-pro"));
    let (default_mcp, alternate_mcp) = (home.join(".claude.json"), alternate.join(".claude.json"));
    let installed = |dir: &Path, mcp: &Path| {
        dir.join("skills/example").is_symlink()
            && dir.join("CLAUDE.md").is_symlink()
            && dir.join("HARNESS.md").is_symlink()
            && fs::read_to_string(mcp).is_ok_and(|text| text.contains("\"fabric\""))
            && fs::read_to_string(dir.join("settings.json"))
                .is_ok_and(|text| text.contains("pre-tool-use"))
    };
    let inspect = |targets: &Targets| {
        inspect_install(targets, None, "fixture-target", &Smoke, &CancelToken::new())
            .unwrap()
            .findings
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let release = f.release();
    let mut apply = |targets: Targets| {
        f.targets = targets;
        install(
            &f.plan_for(release.clone()),
            &context(&clock, &cancel),
            &mut Reporter::default(),
            &Smoke,
            false,
        )
        .unwrap();
    };
    apply(claude(None));
    apply(claude(Some(&alternate)));
    assert!(installed(&default, &default_mcp));
    assert!(installed(&alternate, &alternate_mcp));
    let receipt = read_receipt(&claude(None)).unwrap().unwrap();
    assert_eq!(receipt.harnesses, vec![Harness::Claude]);
    assert_eq!(receipt.harness_targets.len(), 2);

    // Check covers every recorded target, whichever one the environment names.
    assert!(inspect(&claude(None)).is_empty());
    let link = default.join("skills/example");
    fs::remove_file(&link).unwrap();
    assert!(inspect(&claude(Some(&alternate))).iter().any(|finding| {
        finding.rule == "owned_entry_drift" && finding.path.to_path_buf().unwrap() == link
    }));
    apply(claude(None));
    assert!(installed(&default, &default_mcp));
    assert!(installed(&alternate, &alternate_mcp));

    uninstall(
        &claude(Some(&alternate)),
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(installed(&default, &default_mcp));
    assert!(fs::symlink_metadata(alternate.join("skills/example")).is_err());
    assert!(fs::symlink_metadata(alternate.join("CLAUDE.md")).is_err());
    assert!(
        !fs::read_to_string(&alternate_mcp)
            .unwrap()
            .contains("\"fabric\"")
    );
    assert!(
        !fs::read_to_string(alternate.join("settings.json"))
            .unwrap()
            .contains("pre-tool-use")
    );
    let receipt = read_receipt(&claude(None)).unwrap().unwrap();
    assert_eq!(receipt.harness_targets.len(), 1);
    assert_eq!(
        receipt.harness_targets[0].skills_dir.to_path_buf().unwrap(),
        default.join("skills")
    );
    assert!(inspect(&claude(None)).is_empty());
}

#[test]
fn claude_settings_fixture_is_pretty_and_keeps_existing_key_order() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]);
    f.targets.hooks = true;
    f.targets.hook_specs = crate::CLAUDE_HOOKS.to_vec();
    let settings = f.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        b"{\n\"z\":{\"first\":1,\"second\":2,},\"hooks\":{},}",
    )
    .unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let text = fs::read_to_string(settings).unwrap();
    assert!(text.find("\"z\"").unwrap() < text.find("\"hooks\"").unwrap());
    assert!(
        text.contains("\n  \"z\": {\n    \"first\": 1,\n    \"second\": 2\n  },\n  \"hooks\": {"),
        "{text}"
    );
    assert!(!text.contains("\"hooks\":[{"));
    assert!(!text.contains("fixture comment"));
    assert!(!text.contains(",\n}"));
    let _: serde_json::Value = serde_json::from_str(&text).unwrap();
}

#[test]
fn install_uses_strict_configuration_before_planning() {
    let mut f = Fixture::new();
    let state = f.targets.roots.state.as_path();
    fs::create_dir_all(state).unwrap();
    let bytes = b"schema_version=1\nforeign_key='private sentinel'\n";
    fs::write(state.join("config.toml"), bytes).unwrap();
    let error =
        crate::apply_install_config_with_domains(&mut f.targets, &crate::domains::NO_DOMAINS)
            .unwrap_err();
    assert!(matches!(
        error,
        InstallError::Config(crate::ConfigError::Schema {
            rule: "unknown_key",
            ..
        })
    ));
    assert!(!error.to_string().contains("private sentinel"));
    assert_eq!(
        fs::read(f.targets.roots.state.as_path().join("config.toml")).unwrap(),
        bytes
    );
}

#[test]
fn protected_destination_is_refused_before_any_filesystem_read() {
    let mut f = Fixture::new();
    let release = f.release();
    let old = f.home.join("old-checkout");
    fs::create_dir(&old).unwrap();
    let sentinel = old.join("config.toml");
    fs::write(&sentinel, "private sentinel must not be read").unwrap();
    // Composition resolves aliases to a canonical protected root once; the
    // pure plan must reject every destination before opening any file.
    let alias = f.home.join("old-alias");
    symlink(&old, &alias).unwrap();
    f.targets
        .protected_roots
        .push(alias.canonicalize().unwrap());
    f.targets.harnesses[0].mcp_file = alias.join("config.toml");
    assert_eq!(
        f.targets
            .resolve_destinations(&provenant_os::fs::SystemPathResolver)
            .unwrap_err()
            .to_string(),
        format!("unmanaged destination conflicts: {}", sentinel.display())
    );
    let _fault = FaultInjection::new(FaultMode::NoReads, None);
    let error = Snapshots::capture(&release, &f.targets).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("unmanaged destination conflicts: {}", sentinel.display())
    );
    assert_eq!(
        fs::read_to_string(&sentinel).unwrap(),
        "private sentinel must not be read"
    );
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn mcp_registry_matches_recorded_client_config_contracts() {
    let mut f = Fixture::new();
    for (harness, fixture) in [
        (
            Harness::Claude,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/contracts/claude-code-2.1.310/config.json"
            )),
        ),
        (
            Harness::Codex,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/contracts/codex-cli-0.163.0/config.toml"
            )),
        ),
        (
            Harness::Agy,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/contracts/agy-1.4.6/mcp_config.json"
            )),
        ),
    ] {
        f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[harness]);
        let normalized = fixture
            .replace(
                "/FIXTURE_ROOT/bin/mcp-probe",
                f.targets.bin_dir.join("provenant").to_str().unwrap(),
            )
            .replace(
                "/FIXTURE_ROOT/state",
                f.targets.roots.state.as_path().to_str().unwrap(),
            );
        let document: serde_json::Value = if harness == Harness::Codex {
            toml_edit::de::from_str(&normalized).unwrap()
        } else {
            serde_json::from_str(&normalized).unwrap()
        };
        let mut expected = document[if harness == Harness::Codex {
            "mcp_servers"
        } else {
            "mcpServers"
        }]["fabric"]
            .clone();
        // Codex's recorded optional env is absent; an empty env has the same
        // accepted wire meaning and is emitted explicitly by the registry.
        if harness == Harness::Codex {
            expected["env"] = serde_json::json!({});
        }
        let plan = f.plan();
        let edit = plan
            .actions()
            .iter()
            .flat_map(|a| &a.edits)
            .find(|edit| edit.selector.ends_with("fabric"))
            .unwrap();
        assert_eq!(edit.after.as_ref(), Some(&expected));
        if harness == Harness::Claude {
            let recorded: serde_json::Value = serde_json::from_str(
                &include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/contracts/claude-code-2.1.310/stdio-argv.json"
                ))
                .replace(
                    "/FIXTURE_ROOT/state",
                    f.targets.roots.state.as_path().to_str().unwrap(),
                ),
            )
            .unwrap();
            assert_eq!(expected["args"], recorded);
        }
        assert!(!f.targets.roots.state.as_path().exists());
    }
}

#[test]
fn install_links_manifest_skills_and_library_skills() {
    let f = Fixture::new();
    let skills = f.targets.harnesses[0].skills_dir.clone();
    for relative in ["skills/autopilot", "library/playwright"] {
        let dir = f.source.join(relative);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: retained\n---\n").unwrap();
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(
        fs::read_link(skills.join("autopilot"))
            .unwrap()
            .ends_with("skills/autopilot")
    );
    assert!(
        fs::read_link(skills.join("playwright"))
            .unwrap()
            .ends_with("library/playwright")
    );
    assert!(f.plan().actions().is_empty());
}

#[test]
fn absolute_bootstrap_refuses_intermediate_state_and_release_symlinks() {
    let f = Fixture::new();
    let foreign = f.home.join("foreign");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("sentinel"), b"untouched").unwrap();
    for root in [
        f.targets.roots.state.as_path().to_path_buf(),
        f.source.clone(),
    ] {
        fs::create_dir_all(&root).unwrap();
        let link = root.join("escaped");
        symlink(&foreign, &link).unwrap();
        assert!(directory(&link.join("nested"), true).is_err());
        assert!(!foreign.join("nested").exists());
        assert_eq!(fs::read(foreign.join("sentinel")).unwrap(), b"untouched");
    }
}

#[test]
fn state_mount_descendant_refuses_before_lock_or_receipt_writes() {
    let f = Fixture::new();
    let plan = f.plan();
    fs::create_dir_all(f.targets.roots.state.as_path().join("install")).unwrap();
    let _fault = FaultInjection::new(FaultMode::StateMount, None);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let error = install(
        &plan,
        &InstallContext {
            ..context(&clock, &cancel)
        },
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    assert!(
        matches!(error, InstallError::Fs(FsError::CrossDevice { path }) if path == Path::new("install"))
    );
    assert!(!f.receipt().exists());
    assert!(
        !f.targets
            .roots
            .state
            .as_path()
            .join("install/install.lock")
            .exists()
    );
}

#[test]
fn source_mount_descendant_refuses_before_reading_manifest_assets() {
    let f = Fixture::new();
    f.release();
    let _fault = FaultInjection::new(FaultMode::SourceMount, None);
    let error = Release::read(&f.source, "fixture-target").unwrap_err();
    assert!(
        matches!(error, InstallError::Fs(FsError::CrossDevice { path }) if path == Path::new("skills"))
    );
}

#[test]
fn uninstall_without_receipt_reports_nothing_installed() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let outcome = uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(outcome.actions.is_empty());
    assert_eq!(outcome.findings[0].rule, "not_installed");
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn claude_large_harness_config_is_bounded_and_byte_preserved() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]);
    let path = f.home.join(".claude.json");
    let payload = "x".repeat(2 * 1024 * 1024);
    let before = format!("{{\"projects\":\"{payload}\"}}\n");
    fs::write(&path, &before).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains(&payload));
    assert!(f.plan().actions().is_empty());
    uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains(&payload));
    fs::write(&path, " ".repeat(16 * 1024 * 1024 + 1)).unwrap();
    assert!(Snapshots::capture(&f.release(), &f.targets).is_err());
}

#[test]
fn opencode_reapply_drops_old_release_and_keeps_receipt_ownership() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Opencode]);
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        fs::write(f.source.join("instructions").join(name), b"instructions").unwrap();
    }
    let config = f.targets.harnesses[0].mcp_file.clone();
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let a = f.release();
    let old = release_root(f.targets.roots.state.as_path(), a.manifest.source_digest)
        .join("instructions/AGENTS.md");
    fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({"instructions":[old, "personal.md"]})).unwrap(),
    )
    .unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(
        &f.plan_for(a),
        &ctx,
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let b = f.release_variant("opencode-b", &["example"]);
    fs::create_dir_all(b.source().join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        fs::write(
            b.source().join("instructions").join(name),
            b"new instructions",
        )
        .unwrap();
    }
    fs::remove_file(b.source().join("release.json")).unwrap();
    let b = package_fixture_release(b.source(), b.manifest.source_digest);
    let new = release_root(f.targets.roots.state.as_path(), b.manifest.source_digest)
        .join("instructions/AGENTS.md");
    // A partial prior publication may leave both the old receipt value and the
    // desired new value. Reapply must settle to one newly receipted entry.
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    document["instructions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(new));
    fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    install(
        &f.plan_for(b.clone()),
        &ctx,
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert!(
        !document["instructions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(old))
    );
    assert_eq!(
        document["instructions"].as_array().unwrap(),
        &vec![serde_json::json!("personal.md"), serde_json::json!(new)]
    );
    let receipt = read_receipt(&f.targets).unwrap().unwrap();
    assert!(
        receipt
            .entries
            .iter()
            .any(|entry| entry.selector == format!("array:{}", new.display()))
    );
    assert!(f.plan_for(b.clone()).actions().is_empty());
    let mut duplicate = document.clone();
    duplicate["instructions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(new));
    let bytes = serde_json::to_vec(&duplicate).unwrap();
    fs::write(&config, &bytes).unwrap();
    assert!(matches!(
        Snapshots::capture(&b, &f.targets),
        Err(InstallError::InvalidConfig("duplicate owned instruction"))
    ));
    assert_eq!(fs::read(&config).unwrap(), bytes);
}

#[test]
fn inspect_install_binds_changed_tampered_bytes_with_restored_metadata_and_findings() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let inspect =
        || inspect_install(&fixture.targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    let clean = inspect();
    assert!(clean.findings.is_empty());
    let repeated = inspect();
    assert_eq!(clean.findings, repeated.findings);
    assert_eq!(clean.input_digest, repeated.input_digest);
    assert!(repeated.error.is_none());
    let asset = fixture
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(plan.release.manifest.source_digest.to_string())
        .join("skills/example/SKILL.md");
    let before = fs::metadata(&asset).unwrap();
    let modified = before.modified().unwrap();
    let file = fs::OpenOptions::new().write(true).open(&asset).unwrap();
    let set_bytes = |byte| {
        fs::write(&asset, vec![byte; before.len() as usize]).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
    };
    set_bytes(b'a');
    let first = inspect();
    let metadata = snapshot(&asset).unwrap().unwrap().metadata;
    set_bytes(b'b');
    let after = snapshot(&asset).unwrap().unwrap().metadata;
    // Restoring mtime preserves every original metadata field, but ctime now
    // exposes the rewrite. Inspection's metadata evidence excludes ctime, so
    // its changed input digest must still bind the changed content bytes.
    assert_eq!(
        metadata,
        Metadata {
            changed_ns: metadata.changed_ns,
            ..after
        }
    );
    assert_ne!(metadata.changed_ns, after.changed_ns);
    let mut first_metadata = InspectionEvidence::default();
    first_metadata.metadata(&asset, metadata);
    let mut second_metadata = InspectionEvidence::default();
    second_metadata.metadata(&asset, after);
    assert_eq!(first_metadata.finish(), second_metadata.finish());
    let second = inspect();
    assert_eq!(first.findings, second.findings);
    assert!(
        first
            .findings
            .iter()
            .any(|finding| finding.rule == "release_drift")
    );
    assert_ne!(first.input_digest, second.input_digest);
}

/// Rewrites a file on early previews, as a live harness would between the
/// plan and the apply.
struct Racing {
    path: PathBuf,
    rewrites: usize,
    previews: usize,
}
impl InstallReporter for Racing {
    fn preview(&mut self, _: &[Action]) -> io::Result<()> {
        self.previews += 1;
        if self.previews <= self.rewrites {
            fs::write(&self.path, format!("{{\"startups\":{}}}", self.previews))?;
        }
        Ok(())
    }
}
fn claude_fixture() -> Fixture {
    let mut f = Fixture::new();
    f.targets.harnesses.push(HarnessTarget {
        harness: Harness::Claude,
        mcp_file: f.home.join(".claude.json"),
        hook_file: None,
        skills_dir: f.home.join("claude/skills"),
        instruction: None,
    });
    fs::write(f.home.join(".claude.json"), b"{}").unwrap();
    f
}
#[test]
fn live_harness_json_rewrite_between_preview_and_apply_is_retried() {
    let f = claude_fixture();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut reporter = Racing {
        path: f.home.join(".claude.json"),
        rewrites: 2,
        previews: 0,
    };
    plan_and_install(
        &f.release(),
        &f.targets,
        &context(&clock, &cancel),
        &mut reporter,
        &Smoke,
        false,
    )
    .unwrap();
    assert_eq!(reporter.previews, 3);
    let merged = fs::read_to_string(f.home.join(".claude.json")).unwrap();
    assert!(merged.contains("startups"), "{merged}");
    assert!(merged.contains("mcpServers"), "{merged}");
}
#[test]
fn json_rewrite_retries_are_bounded_and_other_drift_still_refuses() {
    let f = claude_fixture();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut reporter = Racing {
        path: f.home.join(".claude.json"),
        rewrites: 10,
        previews: 0,
    };
    let err = plan_and_install(
        &f.release(),
        &f.targets,
        &context(&clock, &cancel),
        &mut reporter,
        &Smoke,
        false,
    )
    .unwrap_err();
    assert!(matches!(err, InstallError::Drift(_)));
    assert_eq!(reporter.previews, 3);

    let f = claude_fixture();
    fs::create_dir_all(&f.targets.bin_dir).unwrap();
    let mut reporter = Racing {
        path: f.targets.bin_dir.join("provenant"),
        rewrites: 10,
        previews: 0,
    };
    let err = plan_and_install(
        &f.release(),
        &f.targets,
        &context(&clock, &cancel),
        &mut reporter,
        &Smoke,
        false,
    )
    .unwrap_err();
    assert!(matches!(err, InstallError::Drift(_)));
    assert_eq!(reporter.previews, 1);
}

#[test]
fn missing_gc_agent_is_stopped_only_when_the_last_harness_is_removed() {
    let mut f = Fixture::new();
    f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
        .resolve(&Resolver)
        .unwrap();
    f.targets.harnesses = Targets::new(
        f.targets.roots.clone(),
        &f.home,
        &[Harness::Claude, Harness::Codex],
    )
    .harnesses;
    f.targets.gc_agent = Some(b"rendered plist\n".to_vec());
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    fs::remove_file(gc_agent_plist(f.targets.roots.state.as_path()).unwrap()).unwrap();
    struct StopReporter(usize);
    impl InstallReporter for StopReporter {
        fn preview(&mut self, _: &[Action]) -> io::Result<()> {
            Ok(())
        }
        fn before_remove(&mut self, action: &Action) -> io::Result<()> {
            if action.selector == "gc-agent" {
                self.0 += 1;
            }
            Ok(())
        }
    }
    let mut reporter = StopReporter(0);
    let codex = f.targets.harnesses.pop().unwrap();
    uninstall(&f.targets, &ctx, &mut reporter).unwrap();
    assert_eq!(reporter.0, 0);
    assert!(
        read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.selector == "gc-agent")
    );
    f.targets.harnesses = vec![codex];
    uninstall(&f.targets, &ctx, &mut reporter).unwrap();
    assert_eq!(reporter.0, 1);
    assert!(
        !read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.selector == "gc-agent")
    );
}

#[test]
fn gc_agent_is_previewed_receipted_and_removed_only_on_an_exact_match() {
    for state in ["unchanged", "modified", "missing"] {
        let modified = state == "modified";
        let mut f = Fixture::new();
        f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
            .resolve(&Resolver)
            .unwrap();
        f.targets.gc_agent = Some(b"rendered plist\n".to_vec());
        let plist = gc_agent_plist(f.targets.roots.state.as_path()).unwrap();
        let plan = f.plan();
        assert!(
            plan.actions()
                .iter()
                .any(|action| action.selector == "gc-agent"
                    && action.path.to_path_buf().unwrap() == plist)
        );
        assert!(!plist.exists());
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let context = InstallContext {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30).unwrap(),
        };
        install(&plan, &context, &mut Reporter::default(), &Smoke, false).unwrap();
        assert!(f.plan().actions().is_empty());
        let receipt = read_receipt(&f.targets).unwrap().unwrap();
        assert!(
            receipt
                .entries
                .iter()
                .any(|entry| entry.selector == "gc-agent")
        );
        if modified {
            fs::write(&plist, b"user edited plist\n").unwrap();
            let release = f.release();
            let error = Snapshots::capture(&release, &f.targets)
                .and_then(|snapshots| plan_install(release, f.targets.clone(), snapshots))
                .unwrap_err()
                .to_string();
            assert!(error.contains(plist.to_str().unwrap()), "{error}");
        }
        if state == "missing" {
            fs::remove_file(&plist).unwrap();
        }
        struct StopReporter(usize);
        impl InstallReporter for StopReporter {
            fn preview(&mut self, _: &[Action]) -> io::Result<()> {
                Ok(())
            }
            fn before_remove(&mut self, action: &Action) -> io::Result<()> {
                if action.selector == "gc-agent" {
                    self.0 += 1;
                }
                Ok(())
            }
        }
        let mut reporter = StopReporter(0);
        let result = uninstall(&f.targets, &context, &mut reporter).unwrap();
        assert_eq!(reporter.0, usize::from(!modified));
        assert_eq!(
            result
                .actions
                .iter()
                .filter(|action| action.selector == "gc-agent")
                .count(),
            usize::from(!modified)
        );
        assert_eq!(plist.exists(), modified);
        assert_eq!(
            result
                .findings
                .iter()
                .any(|finding| finding.rule == "relinquished_modified_entry"
                    && finding.path.to_path_buf().unwrap() == plist),
            modified
        );
    }
}

#[test]
fn disabling_gc_agent_reinstall_stops_exact_or_missing_owned_plists() {
    for state in ["unchanged", "modified", "missing"] {
        let mut f = Fixture::new();
        f.targets.roots = RootInputs::new(f.home.clone(), f.base.join("checkout"))
            .resolve(&Resolver)
            .unwrap();
        f.targets.gc_agent = Some(b"rendered plist\n".to_vec());
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let ctx = context(&clock, &cancel);
        install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
        let plist = gc_agent_plist(f.targets.roots.state.as_path()).unwrap();
        match state {
            "modified" => fs::write(&plist, b"user edited plist\n").unwrap(),
            "missing" => fs::remove_file(&plist).unwrap(),
            _ => {}
        }
        f.targets.gc_agent = None;
        let plan = f.plan();
        let removable = state != "modified";
        assert_eq!(
            plan.actions()
                .iter()
                .any(|action| action.selector == "gc-agent"),
            removable
        );
        struct StopReporter(usize);
        impl InstallReporter for StopReporter {
            fn preview(&mut self, _: &[Action]) -> io::Result<()> {
                Ok(())
            }
            fn before_remove(&mut self, action: &Action) -> io::Result<()> {
                if action.selector == "gc-agent" {
                    self.0 += 1;
                }
                Ok(())
            }
        }
        let mut reporter = StopReporter(0);
        let preview = install(&plan, &ctx, &mut reporter, &Smoke, true).unwrap();
        assert!(preview.dry_run);
        assert_eq!(reporter.0, 0);
        let outcome = install(&plan, &ctx, &mut reporter, &Smoke, false).unwrap();
        assert_eq!(reporter.0, usize::from(removable));
        assert_eq!(plist.exists(), !removable);
        assert_eq!(
            outcome.findings.iter().any(|finding| {
                finding.rule == "relinquished_modified_entry"
                    && finding.path.to_path_buf().unwrap() == plist
            }),
            !removable
        );
        assert!(
            !read_receipt(&f.targets)
                .unwrap()
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.selector == "gc-agent")
        );
    }
}

#[test]
fn retired_disk_guard_is_not_managed_by_install_or_uninstall() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let mut receipt: InstallReceipt = read_receipt(&f.targets).unwrap().unwrap();
    let plist = f
        .home
        .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, b"old plist").unwrap();
    receipt.entries.push(OwnedEntry {
        path: AbsolutePath::new(&plist).unwrap(),
        harness: None,
        selector: "disk-guard-agent".into(),
        expected: owned(&Value::File {
            bytes: b"old plist".to_vec(),
            mode: 0o600,
        })
        .unwrap(),
    });
    fs::write(f.receipt(), serde_json::to_vec(&receipt).unwrap()).unwrap();
    let plan = f.plan();
    assert!(
        !plan
            .actions()
            .iter()
            .any(|action| action.selector == "disk-guard-agent")
    );
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    assert!(
        !read_receipt(&f.targets)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.selector == "disk-guard-agent")
    );
    let outcome = uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    assert!(
        !outcome
            .actions
            .iter()
            .any(|action| action.selector == "disk-guard-agent")
    );
    assert!(plist.exists(), "retirement belongs to the process edge");
}

#[test]
fn receipt_with_null_legacy_upgrades_cleanly() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(f.receipt()).unwrap()).unwrap();
    receipt["legacy"] = serde_json::Value::Null;
    fs::write(f.receipt(), serde_json::to_vec(&receipt).unwrap()).unwrap();

    let inspection = inspect_install(&f.targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert!(inspection.findings.is_empty());

    let release = f.release_variant("upgrade", &["example"]);
    let plan = f.plan_for(release.clone());
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let root = release_root(
        f.targets.roots.state.as_path(),
        release.manifest.source_digest,
    );
    assert_eq!(
        fs::read_link(f.targets.bin_dir.join("provenant")).unwrap(),
        root.join("bin/provenant")
    );
    assert_eq!(
        fs::read_link(f.link()).unwrap(),
        root.join("skills/example")
    );
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(f.receipt()).unwrap()).unwrap();
    assert!(receipt.get("legacy").is_none());
    assert_eq!(
        receipt["source_digest"],
        release.manifest.source_digest.to_string()
    );
    assert!(f.plan_for(release).actions().is_empty());

    receipt["legacy"] = serde_json::Value::Null;
    fs::write(f.receipt(), serde_json::to_vec(&receipt).unwrap()).unwrap();
    uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    assert!(fs::symlink_metadata(f.link()).is_err());
}

#[test]
fn receipt_with_a_legacy_object_upgrades_and_drops_it() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(f.receipt()).unwrap()).unwrap();
    receipt["legacy"] = serde_json::json!({
        "checkout": "/Users/dev/Repos/agents",
        "home": "/Users/dev",
        "manifest_digest": format!("sha256:{}", "ab".repeat(32)),
        "git_hooks_dir": "/Users/dev/.config/git/hooks",
    });
    fs::write(f.receipt(), serde_json::to_vec(&receipt).unwrap()).unwrap();

    let inspection = inspect_install(&f.targets, None, "fixture-target", &Smoke, &cancel).unwrap();
    assert!(inspection.error.is_none(), "{:?}", inspection.error);
    assert!(inspection.findings.is_empty());

    let release = f.release_variant("upgrade", &["example"]);
    let plan = f.plan_for(release.clone());
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let root = release_root(
        f.targets.roots.state.as_path(),
        release.manifest.source_digest,
    );
    assert_eq!(
        fs::read_link(f.targets.bin_dir.join("provenant")).unwrap(),
        root.join("bin/provenant")
    );
    assert_eq!(
        fs::read_link(f.link()).unwrap(),
        root.join("skills/example")
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(f.receipt()).unwrap()).unwrap();
    assert!(receipt.get("legacy").is_none());
    assert_eq!(
        receipt["source_digest"],
        release.manifest.source_digest.to_string()
    );
    assert!(f.plan_for(release).actions().is_empty());
}
