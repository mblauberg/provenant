mod support;
use support::{Result, TestRig, code};

// Install inspection intentionally refuses a device crossing from its anchor.
// CI supplies a temporary root on the checkout device; /tmp can be a tmpfs.
fn check_rig() -> Result<TestRig> {
    TestRig::in_directory(&std::env::temp_dir())
}

#[test]
fn config_and_install_defects_are_findings_but_escape_is_an_error() -> Result {
    use std::fs;
    let rig = check_rig()?;
    fs::create_dir_all(rig.state.join("install"))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\nunknown=true\n",
    )?;
    let config = rig.command(&["check", "config", "--json"])?;
    assert_eq!(
        code(&config),
        1,
        "{}",
        String::from_utf8_lossy(&config.stdout.bytes)
    );
    assert_eq!(json(&config)?["data"]["count"], 1);
    assert_eq!(code(&rig.command(&["check", "install", "--json"])?), 1);

    fs::write(rig.state.join("config.toml"), "schema_version = [")?;
    assert_eq!(code(&rig.command(&["check", "config", "--json"])?), 1);
    fs::write(rig.state.join("install/receipt.json"), "{")?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        rig.state.join("install/receipt.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    assert_eq!(code(&rig.command(&["check", "install", "--json"])?), 1);

    fs::remove_file(rig.state.join("config.toml"))?;
    fs::write(rig.home.join("outside"), "must never be read")?;
    std::os::unix::fs::symlink(rig.home.join("outside"), rig.state.join("config.toml"))?;
    let escaped = rig.command(&["check", "config", "--json"])?;
    assert_eq!(code(&escaped), 3);
    assert!(!String::from_utf8_lossy(&escaped.stdout.bytes).contains("must never be read"));
    rig.assert_legacy_unchanged()
}

#[test]
fn check_install_smokes_a_staged_binary_with_private_home_and_keeps_scan_status() -> Result {
    use provenant_config::{
        InstallContext, InstallReporter, ReleaseSmoke, Snapshots, Targets, package_release,
        plan_install,
    };
    use provenant_core::{CancelToken, Clock, PathResolver, RootInputs, WaitBudget};
    use provenant_records::{SourceInput, VersionInfo, source_digest};
    use provenant_supervisor::{clock::SystemClock, fs::LocalFs};
    use std::os::unix::fs::PermissionsExt;
    use std::{
        fs, io,
        path::{Path, PathBuf},
    };

    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }
    }
    struct FixedSmoke(VersionInfo);
    impl ReleaseSmoke for FixedSmoke {
        fn check(&self, _: &provenant_config::Release, _: &CancelToken) -> io::Result<VersionInfo> {
            Ok(self.0.clone())
        }
    }
    struct Reporter;
    impl InstallReporter for Reporter {
        fn preview(&mut self, _: &[provenant_config::Action]) -> io::Result<()> {
            Ok(())
        }
    }

    let rig = check_rig()?;
    let source = rig.root.join("staged-release");
    fs::create_dir_all(source.join("bin"))?;
    fs::create_dir_all(source.join("skills/example"))?;
    let skill = b"---\nname: example\ndescription: fixture\n---\n";
    fs::write(source.join("skills/example/SKILL.md"), skill)?;
    let mut asset_bytes = vec![0];
    asset_bytes.extend_from_slice(skill);
    let version = VersionInfo {
        source_digest: source_digest(&[SourceInput {
            name: "src/fixture.rs",
            bytes: b"fixture source",
        }])?,
        asset_digest: source_digest(&[SourceInput {
            name: "skills/example/SKILL.md",
            bytes: &asset_bytes,
        }])?,
        target: env!("PROVENANT_TARGET_TRIPLE").into(),
        toolchain: env!("PROVENANT_TOOLCHAIN").into(),
        record_schema_version: 1,
        config_schema_version: 1,
    };
    let marker = rig.root.join("smoke-home.txt");
    let version_json = serde_json::to_string(&serde_json::json!({
        "command": "version",
        "status": "ok",
        "data": version,
    }))?;
    let executable = source.join("bin/provenant");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$HOME\" > '{}'\nprintf '%s\\n' smoke > \"$HOME/keep\"\nif [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' '{}'\nfi\n",
            marker.display(),
            version_json,
        ),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let release = package_release(
        &source,
        &version,
        &LocalFs,
        &FixedSmoke(version.clone()),
        &CancelToken::new(),
    )?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&Resolver)?;
    let targets = Targets::new(roots, &rig.home, &[provenant_records::Harness::Codex]);
    let snapshots = Snapshots::capture(&release, &targets, &LocalFs)?;
    let plan = plan_install(release, targets, snapshots)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = InstallContext {
        fs: &LocalFs,
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 1)?,
        cancel: &cancel,
    };
    provenant_config::install(&plan, &context, &mut Reporter, &FixedSmoke(version), false)?;

    let output = rig.command(&["check", "install", "--json"])?;
    let home = PathBuf::from(fs::read_to_string(&marker)?.trim());
    assert_ne!(home, rig.home);
    assert!(home.starts_with(fs::canonicalize("/tmp")?));
    assert!(home.join("keep").is_file());
    fs::remove_file(home.join("keep"))?;
    fs::remove_dir(&home)?;
    fs::remove_file(&marker)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let stderr = String::from_utf8_lossy(&output.stderr.bytes);
    assert!(
        stderr.contains("warning") && stderr.contains("private check directory"),
        "{stderr}"
    );
    Ok(())
}

fn json(output: &provenant_supervisor::exec::ExecOutcome) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}
fn git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let output = rig.execute(std::path::Path::new("/usr/bin/git"), args, None)?;
    assert_eq!(
        code(&output),
        0,
        "Git fixture: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
}
fn commit(rig: &TestRig, message: &str) -> Result<String> {
    git(rig, &["add", "--all"])?;
    git(
        rig,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ],
    )?;
    git(rig, &["rev-parse", "HEAD"])
}
fn legal(rig: &TestRig) -> Result {
    for name in [
        "LICENSE",
        "NOTICE",
        "THIRD_PARTY_NOTICES.md",
        "LICENSES/grill-me-pocock-MIT.txt",
        "LICENSES/impeccable-APACHE-2.0.txt",
        "LICENSES/skill-optimizer-MIT.txt",
        "LICENSES/ui-ux-pro-max-MIT.txt",
        "README.md",
        "MAINTAINING.md",
        "SECURITY.md",
        "ACKNOWLEDGEMENTS.md",
        "docs/ARCHITECTURE.md",
        "docs/worktrees.md",
    ] {
        let path = rig.project.join(name);
        std::fs::create_dir_all(path.parent().ok_or("legal parent")?)?;
        std::fs::write(path, "fixture legal attribution\n")?;
    }
    Ok(())
}
#[test]
fn cursors_recheck_all_inputs_and_later_pages_keep_findings_exit() -> Result {
    let rig = check_rig()?;
    std::fs::create_dir_all(&rig.state)?;
    let config = format!(
        "schema_version=1\n{}",
        (0..105)
            .map(|i| format!("unknown_{i}=true\n"))
            .collect::<String>()
    );
    std::fs::write(rig.state.join("config.toml"), &config)?;
    let first = rig.command(&["check", "config", "--limit", "100", "--json"])?;
    assert_eq!(code(&first), 1);
    let first = json(&first)?;
    assert_eq!(first["data"]["count"], 105);
    assert_eq!(
        first["data"]["findings"]
            .as_array()
            .ok_or("findings")?
            .len(),
        100
    );
    let cursor = first["data"]["next_cursor"].as_str().ok_or("cursor")?;
    let next = rig.command(&["check", "config", "--cursor", cursor, "--json"])?;
    assert_eq!(code(&next), 1);
    let next = json(&next)?;
    assert_eq!(next["data"]["offset"], 100);
    assert_eq!(
        next["data"]["findings"].as_array().ok_or("findings")?.len(),
        5
    );
    assert!(next["data"]["next_cursor"].is_null());
    let mismatched = rig.command(&["check", "install", "--cursor", cursor, "--json"])?;
    assert_eq!(
        code(&mismatched),
        2,
        "{}",
        String::from_utf8_lossy(&mismatched.stdout.bytes)
    );
    std::fs::write(
        rig.state.join("config.toml"),
        config.replace("true", "false"),
    )?;
    assert_eq!(
        code(&rig.command(&["check", "config", "--cursor", cursor, "--json"])?),
        2
    );
    assert_eq!(
        code(&rig.command(&["check", "config", "--cursor", "malformed", "--json"])?),
        2
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn harness_uses_tracked_yaml_links_anchors_and_json_routes() -> Result {
    use std::fs;
    let rig = check_rig()?;
    fs::create_dir_all(rig.project.join("skills/example/evals"))?;
    fs::write(
        rig.project.join("skills/example/SKILL.md"),
        "---\nname: example\ndescription: Example fixture skill.\n---\n# Hello\n# Hello\n[duplicate](#hello-1)\n",
    )?;
    fs::write(
        rig.project.join("skills/example/evals/trigger_cases.json"),
        r#"{"schema_version":1,"target_skill":"example","cases":[{"id":"q1","relation":"positive","prompt":"use this skill","tags":["direct"],"expected":{"primary_skill":"example","companion_skills":[]}}]}"#,
    )?;
    git(&rig, &["add", "."])?;
    fs::write(rig.project.join("untracked.md"), "[untracked](missing)")?;
    let clean = rig.command(&["check", "harness", "--json"])?;
    assert_eq!(
        code(&clean),
        0,
        "{}",
        String::from_utf8_lossy(&clean.stdout.bytes)
    );
    fs::write(
        rig.project.join("skills/example/SKILL.md"),
        "---\nname: example\ndescription: 123\n---\n[bad](#missing)\n",
    )?;
    let findings = rig.command(&["check", "harness", "--limit", "1", "--json"])?;
    assert_eq!(code(&findings), 1);
    let findings = json(&findings)?;
    let cursor = findings["data"]["next_cursor"]
        .as_str()
        .ok_or("harness cursor")?;
    let next = rig.command(&["check", "harness", "--cursor", cursor, "--json"])?;
    assert_eq!(code(&next), 1);
    fs::write(
        rig.project.join("skills/example/evals/trigger_cases.json"),
        "{}",
    )?;
    assert_eq!(
        code(&rig.command(&["check", "harness", "--cursor", cursor, "--json"])?),
        2
    );
    fs::remove_file(rig.project.join("skills/example/SKILL.md"))?;
    std::os::unix::fs::symlink(
        rig.home.join("outside.md"),
        rig.project.join("skills/example/SKILL.md"),
    )?;
    fs::write(rig.home.join("outside.md"), "private fixture content")?;
    assert_eq!(code(&rig.command(&["check", "harness", "--json"])?), 3);
    rig.assert_legacy_unchanged()
}
#[test]
fn release_scans_added_then_deleted_blobs_and_commit_identity_bytes() -> Result {
    use std::fs;
    let rig = check_rig()?;
    legal(&rig)?;
    let base = commit(&rig, "base")?;
    let clean = rig.command(&["check", "release", "--json"])?;
    assert_eq!(
        code(&clean),
        0,
        "{}",
        String::from_utf8_lossy(&clean.stdout.bytes)
    );
    fs::write(
        rig.project.join("temporary.txt"),
        format!("{}{}\n", "ghp_", "a".repeat(30)),
    )?;
    commit(&rig, "add temporary content")?;
    fs::remove_file(rig.project.join("temporary.txt"))?;
    let head = commit(&rig, "delete temporary content")?;
    assert_eq!(code(&rig.command(&["check", "release", "--json"])?), 0);
    let range = rig.command(&[
        "check", "release", "--base", &base, "--head", &head, "--json",
    ])?;
    assert_eq!(code(&range), 1);
    assert!(
        json(&range)?["data"]["findings"]
            .as_array()
            .ok_or("release findings")?
            .iter()
            .any(|f| f["rule"] == "release.github-token")
    );
    assert_eq!(
        code(&rig.command(&[
            "check", "release", "--base", &head, "--head", &base, "--json"
        ])?),
        2
    );
    assert_eq!(
        code(&rig.command(&[
            "check", "release", "--base", &head, "--head", &head, "--json"
        ])?),
        2
    );
    let message = format!("{}{}", "/Users/", "fixture/private-path/");
    commit(&rig, &message)?;
    let raw = rig.command(&["check", "release", "--json"])?;
    assert_eq!(code(&raw), 1);
    assert!(
        json(&raw)?["data"]["findings"]
            .as_array()
            .ok_or("commit findings")?
            .iter()
            .any(|f| f["path"]["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("@commit/")))
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn release_flags_personal_email_in_commit_author_and_committer() -> Result {
    let rig = check_rig()?;
    legal(&rig)?;
    commit(&rig, "public baseline")?;
    git(
        &rig,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=person@gmail.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "personal identity",
        ],
    )?;
    let output = rig.command(&["check", "release", "--json"])?;
    assert_eq!(code(&output), 1);
    let result = json(&output)?;
    let findings = result["data"]["findings"]
        .as_array()
        .ok_or("release findings")?;
    let head = git(&rig, &["rev-parse", "HEAD"])?;
    let identities: Vec<_> = findings
        .iter()
        .filter(|finding| finding["rule"] == "release.personal-email")
        .cloned()
        .collect();
    assert_eq!(
        identities,
        serde_json::json!([
            {
                "rule": "release.personal-email",
                "path": {"encoding": "utf8", "path": format!("@commit/{head}")},
                "line": 3,
                "message": "commit author exposes a personal email"
            },
            {
                "rule": "release.personal-email",
                "path": {"encoding": "utf8", "path": format!("@commit/{head}")},
                "line": 4,
                "message": "commit committer exposes a personal email"
            }
        ])
        .as_array()
        .ok_or("expected identities")?
        .to_vec()
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn release_ranges_read_history_behind_a_shallow_boundary() -> Result {
    use std::fs;
    let rig = check_rig()?;
    legal(&rig)?;
    let base = commit(&rig, "public baseline")?;
    fs::write(
        rig.project.join("temporary.txt"),
        format!("{}\\n", "/Users/fixture/private-path/"),
    )?;
    commit(&rig, "private history /Users/fixture/private-path/")?;
    fs::write(rig.project.join("middle.txt"), "middle")?;
    let middle = commit(&rig, "middle")?;
    fs::remove_file(rig.project.join("temporary.txt"))?;
    fs::write(rig.project.join("tip.txt"), "tip")?;
    let tip = commit(&rig, "tip")?;
    fs::write(rig.project.join(".git/shallow"), format!("{middle}\\n"))?;

    let output = rig.command(&[
        "check", "release", "--base", &base, "--head", &tip, "--json",
    ])?;
    assert_eq!(
        code(&output),
        1,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let result = json(&output)?;
    let findings = result["data"]["findings"]
        .as_array()
        .ok_or("release findings")?;
    assert!(findings.iter().any(|finding| {
        finding["rule"] == "release.home-path"
            && finding["path"]["path"]
                .as_str()
                .is_some_and(|path| path.starts_with("@commit/"))
    }));
    rig.assert_legacy_unchanged()
}
#[test]
fn release_requires_tracked_public_files() -> Result {
    let rig = check_rig()?;
    legal(&rig)?;
    commit(&rig, "public baseline")?;
    git(&rig, &["rm", "README.md"])?;
    let output = rig.command(&["check", "release", "--json"])?;
    assert_eq!(code(&output), 1);
    let result = json(&output)?;
    let findings = result["data"]["findings"]
        .as_array()
        .ok_or("release findings")?;
    assert!(findings.iter().any(|finding| {
        finding["rule"] == "release.public"
            && finding["path"]["path"] == "README.md"
            && finding["line"] == 1
            && finding["message"] == "required public file is not tracked"
    }));
    rig.assert_legacy_unchanged()
}
#[test]
fn release_checks_legal_private_symlink_and_bounded_utf8_evidence() -> Result {
    use std::fs;
    let rig = check_rig()?;
    legal(&rig)?;
    commit(&rig, "base")?;
    fs::remove_file(rig.project.join("NOTICE"))?;
    assert_eq!(code(&rig.command(&["check", "release", "--json"])?), 1);
    fs::write(rig.project.join("NOTICE"), "restored")?;
    fs::create_dir_all(rig.project.join(".agent-run"))?;
    fs::write(rig.project.join(".agent-run/private.txt"), "private")?;
    std::os::unix::fs::symlink(rig.home.join("outside"), rig.project.join("link"))?;
    git(&rig, &["add", "--all"])?;
    let found = rig.command(&["check", "release", "--limit", "1", "--json"])?;
    assert_eq!(code(&found), 1);
    let first = json(&found)?;
    let cursor = first["data"]["next_cursor"]
        .as_str()
        .ok_or("release cursor")?;
    assert_eq!(
        code(&rig.command(&["check", "release", "--cursor", cursor, "--json"])?),
        1
    );
    fs::write(rig.project.join("NOTICE"), "edited again")?;
    assert_eq!(
        code(&rig.command(&["check", "release", "--cursor", cursor, "--json"])?),
        2
    );
    fs::write(rig.project.join("NOTICE"), [0xff])?;
    assert_eq!(code(&rig.command(&["check", "release", "--json"])?), 3);
    fs::write(rig.project.join("NOTICE"), vec![b'a'; 5 * 1024 * 1024 + 1])?;
    assert_eq!(code(&rig.command(&["check", "release", "--json"])?), 3);
    rig.assert_legacy_unchanged()
}

#[test]
fn harness_accepts_recorded_agent_skill_and_openai_sidecar() -> Result {
    let rig = check_rig()?;
    let dir = rig.project.join("skills/refactor");
    std::fs::create_dir_all(dir.join("agents"))?;
    // These external-format files are tracked tool inputs, not Python oracles.
    let skill = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skills/refactor/SKILL.md"
    ));
    // Keep frontmatter; the full reference graph belongs to the real repository.
    let end = skill
        .get(4..)
        .and_then(|s| s.find("\n---"))
        .ok_or("recorded frontmatter")?
        + 8;
    std::fs::write(
        dir.join("SKILL.md"),
        skill.get(..end).ok_or("frontmatter boundary")?,
    )?;
    std::fs::write(
        dir.join("agents/openai.yaml"),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/refactor/agents/openai.yaml"
        )),
    )?;
    git(&rig, &["add", "."])?;
    let output = rig.command(&["check", "harness", "--json"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn release_reads_packed_objects_native_alternates_and_linked_worktrees() -> Result {
    let rig = check_rig()?;
    legal(&rig)?;
    commit(&rig, "legal baseline")?;
    git(&rig, &["gc", "--quiet"])?;
    let alternate = rig.root.join("shared");
    git(
        &rig,
        &[
            "clone",
            "--quiet",
            "--shared",
            rig.project.to_str().ok_or("project path")?,
            alternate.to_str().ok_or("alternate path")?,
        ],
    )?;
    let output = rig.command(&[
        "--project",
        alternate.to_str().ok_or("alternate path")?,
        "check",
        "release",
        "--json",
    ])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let linked = rig.root.join("linked");
    git(
        &rig,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().ok_or("linked path")?,
        ],
    )?;
    assert_eq!(
        code(&rig.command(&[
            "--project",
            linked.to_str().ok_or("linked path")?,
            "check",
            "release",
            "--json"
        ])?),
        0
    );
    rig.assert_legacy_unchanged()
}
#[test]
fn release_ignores_grafts_and_rejects_missing_ancestry_evidence() -> Result {
    let rig = check_rig()?;
    legal(&rig)?;
    let base = commit(&rig, "base")?;
    std::fs::write(
        rig.project.join("transient"),
        format!("{}{}", "sk-", "x".repeat(30)),
    )?;
    commit(&rig, "transient secret")?;
    std::fs::remove_file(rig.project.join("transient"))?;
    let head = commit(&rig, "head")?;
    std::fs::write(
        rig.project.join(".git/info/grafts"),
        format!("{head} {base}\n"),
    )?;
    let found = rig.command(&[
        "check", "release", "--base", &base, "--head", &head, "--json",
    ])?;
    assert_eq!(code(&found), 1);
    // A missing required HEAD object must never produce a clean scan.
    std::fs::remove_file(
        rig.project
            .join(".git/objects")
            .join(head.get(..2).ok_or("oid prefix")?)
            .join(head.get(2..).ok_or("oid tail")?),
    )?;
    assert_eq!(code(&rig.command(&["check", "release", "--json"])?), 3);
    rig.assert_legacy_unchanged()
}

#[test]
fn check_target_outside_a_checkout_is_invalid_input() -> Result {
    let mut rig = check_rig()?;
    rig.project = rig.root.join("not-a-checkout");
    std::fs::create_dir(&rig.project)?;
    let output = rig.command(&["check", "harness", "--json"])?;
    assert_eq!(code(&output), 2);
    assert_eq!(json(&output)?["status"], "invalid");
    rig.assert_legacy_unchanged()
}

#[test]
fn harness_ignores_inline_code_links_and_checks_real_links() -> Result {
    let rig = check_rig()?;
    let path = rig.project.join("README.md");
    std::fs::write(
        &path,
        "`[literal](missing.md)` and `` `[other](missing.md)` ``\n[real](#heading)\n# Heading\n",
    )?;
    git(&rig, &["add", "."])?;
    assert_eq!(code(&rig.command(&["check", "harness", "--json"])?), 0);
    std::fs::write(&path, "Use `example\n[not a link](missing.md)\n` here.\n")?;
    assert_eq!(code(&rig.command(&["check", "harness", "--json"])?), 0);
    std::fs::write(&path, "# Title `\n[broken](missing.md)\n# End `\n")?;
    let output = rig.command(&["check", "harness", "--json"])?;
    assert_eq!(code(&output), 1);
    assert_eq!(json(&output)?["data"]["findings"][0]["line"], 2);
    std::fs::write(path, "`[literal](missing.md)` and [real](missing.md)\n")?;
    let output = rig.command(&["check", "harness", "--json"])?;
    assert_eq!(code(&output), 1);
    assert_eq!(json(&output)?["data"]["count"], 1);
    rig.assert_legacy_unchanged()
}

#[test]
fn writing_checks_use_typed_findings_and_input_exits() -> Result {
    let rig = check_rig()?;
    let owner = rig.project.join("skills/natural-writing");
    std::fs::create_dir_all(&owner)?;
    std::fs::write(
        owner.join("lint-rules.json"),
        include_str!("../../../skills/natural-writing/lint-rules.json"),
    )?;
    std::fs::write(rig.project.join("clean.md"), "Plain prose.\n")?;
    let clean = rig.command(&["check", "writing", "clean.md", "--json"])?;
    assert_eq!(
        code(&clean),
        0,
        "{}",
        String::from_utf8_lossy(&clean.stdout.bytes)
    );
    assert_eq!(json(&clean)?["data"]["count"], 0);
    std::fs::write(rig.project.join("draft.md"), "é — robust\n")?;
    let found = rig.command(&["check", "writing", "draft.md", "--json"])?;
    assert_eq!(code(&found), 1);
    let result = json(&found)?;
    assert_eq!(result["command"], "check writing");
    assert_eq!(result["data"]["findings"][0]["column"], 3);
    assert_eq!(result["data"]["findings"][0]["start"], 3);
    let invalid = rig.command(&["check", "writing", "../outside.md", "--json"])?;
    assert_eq!(code(&invalid), 2);
    assert_eq!(json(&invalid)?["error"]["code"], "INVALID");
    let missing = rig.command(&["check", "writing", "missing.md", "--json"])?;
    assert_eq!(code(&missing), 2);
    std::os::unix::fs::symlink(rig.project.join("clean.md"), rig.project.join("link.md"))?;
    assert_eq!(
        code(&rig.command(&["check", "writing", "link.md", "--json"])?),
        3
    );
    rig.assert_legacy_unchanged()
}

#[test]
fn writing_pages_bind_rules_files_and_selector() -> Result {
    let rig = check_rig()?;
    let owner = rig.project.join("skills/natural-writing");
    std::fs::create_dir_all(&owner)?;
    std::fs::write(
        owner.join("lint-rules.json"),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/natural-writing/lint-rules.json"
        )),
    )?;
    std::fs::write(rig.project.join("draft.md"), "robust\n".repeat(120))?;
    let first = rig.command(&["check", "writing", "draft.md", "--json", "--limit", "1"])?;
    assert_eq!(code(&first), 1);
    assert!(first.stdout.bytes.len() < 1024 * 1024);
    let first = json(&first)?;
    assert_eq!(first["data"]["count"], 120);
    let cursor = first["data"]["next_cursor"].as_str().ok_or("cursor")?;
    let second = rig.command(&[
        "check", "writing", "draft.md", "--json", "--limit", "1", "--cursor", cursor,
    ])?;
    assert_eq!(json(&second)?["data"]["findings"][0]["line"], 2);
    std::fs::write(rig.project.join("draft.md"), "Plain prose.")?;
    assert_eq!(
        code(&rig.command(&["check", "writing", "draft.md", "--json", "--cursor", cursor])?),
        2
    );
    Ok(())
}
