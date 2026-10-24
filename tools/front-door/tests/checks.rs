mod support;
use support::{Result, TestRig, code};

// Install inspection intentionally refuses a device crossing from its anchor.
// CI supplies a temporary root on the checkout device; /tmp can be a tmpfs.
fn check_rig() -> Result<TestRig> {
    TestRig::in_directory(&std::env::temp_dir())
}

#[test]
fn config_defects_are_findings_and_install_absence_is_not_success() -> Result {
    let rig = check_rig()?;
    std::fs::create_dir_all(&rig.state)?;
    std::fs::write(
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
    let value: serde_json::Value = serde_json::from_slice(&config.stdout.bytes)?;
    assert_eq!(value["data"]["count"], 1);
    let install = rig.command(&["check", "install", "--json"])?;
    assert_eq!(
        code(&install),
        1,
        "{}",
        String::from_utf8_lossy(&install.stdout.bytes)
    );
    rig.assert_legacy_unchanged()
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
fn malformed_inspected_config_and_install_are_findings_but_escape_is_error() -> Result {
    use std::fs;
    let rig = check_rig()?;
    fs::create_dir_all(rig.state.join("install"))?;
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
        r#"{"schema_version":1,"target_skill":"example","cases":[{"id":"positive","relation":"positive","prompt":"use this skill","tags":["direct"],"expected":{"primary_skill":"example","companion_skills":[]}}]}"#,
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
