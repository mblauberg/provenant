//! Production CLI dispatch against a fake Codex executable, never live credentials.
use super::worktree_tests::support::{Result, TestRig, code};
use super::worktree_tests::{git, ok, repository, writer_backend_available, writer_context};
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn fixture() -> Result<TestRig> {
    let mut rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "compose", "--base", "HEAD", "--json"],
    )?;
    for name in [
        ".cargo/bin",
        ".cargo/registry",
        ".rustup/toolchains",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(name))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), b"")?;
    fs::write(rig.home.join(".codex/auth.json"), b"fixture only")?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    fs::copy(std::env::current_exe()?, bin.join("codex"))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    Ok(rig)
}

fn dispatch(rig: &TestRig, mode: &str, operation: &str) -> Result<(serde_json::Value, PathBuf)> {
    let mut args = vec![
        "--state-dir".into(),
        rig.state.clone().into_os_string(),
        "--project".into(),
        rig.project.join(".worktrees/compose").into_os_string(),
        "dispatch".into(),
        "--model".into(),
        "codex/gpt-6-luna".into(),
        "--operation-id".into(),
        operation.into(),
        "--prompt-stdin".into(),
        "--wait".into(),
        "5".into(),
        "--json".into(),
    ];
    if mode == "writer" {
        args.extend([
            "--mode".into(),
            "worktree-write".into(),
            "--worktree".into(),
            ".worktrees/compose".into(),
        ]);
    }
    let output = rig.execute_os(&std::env::current_exe()?, &args, Some(mode.as_bytes()))?;
    assert_eq!(
        code(&output),
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    let lane = value["data"]["lanes"][0].clone();
    // Codex's fixture protocol carries no observed answering model. Prove the
    // confined process exited cleanly without waiving Run's identity gate.
    assert!(lane["exit_code"] == 0, "{value}");
    let attempt = rig
        .state
        .join("runs")
        .join(lane["run_id"].as_str().ok_or("run id")?)
        .join("attempts")
        .join(lane["attempt_id"].as_str().ok_or("attempt id")?);
    assert_eq!(
        fs::read(attempt.join("provider-state/compose-proof"))?,
        b"proved"
    );
    Ok((lane, attempt))
}

pub(crate) fn read_only_dispatch_applies_bound_git_and_project_writes() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    // Separate state roots avoid the production host startup-spacing window.
    for configured in [false, true] {
        let rig = fixture()?;
        let (_, context) = writer_context(&rig)?;
        let policy = rig
            .state
            .join("projects")
            .join(context.project_id.to_string())
            .join("read-only.json");
        let locks = rig.project.join(".worktrees/compose/.agent-run/locks");
        fs::create_dir_all(&locks)?;
        if configured {
            fs::write(policy, br#"{"readOnlyWritable":[".agent-run/locks"]}"#)?;
        }
        let mode = if configured { "reader" } else { "reader-empty" };
        let (_, attempt) = dispatch(&rig, mode, "compose-reader")?;
        let db = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
        let raw: Vec<u8> = db.query_row(
            "SELECT record FROM attempts WHERE id=?1",
            [attempt
                .file_name()
                .ok_or("attempt name")?
                .to_str()
                .ok_or("attempt id")?],
            |row| row.get(0),
        )?;
        let record: serde_json::Value = serde_json::from_slice(&raw)?;
        let expected: &[&str] = if configured {
            &[".agent-run/locks"]
        } else {
            &[]
        };
        assert_eq!(record["read_only_writable"], serde_json::json!(expected));
        assert_eq!(locks.join("compose-lock").exists(), configured);
        if configured {
            assert_eq!(fs::read(locks.join("compose-lock"))?, b"lock");
        }
    }
    Ok(())
}

pub(crate) fn writer_dispatch_uses_lane_cache_and_narrow_provider_state() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = fixture()?;
    let (_, context) = writer_context(&rig)?;
    let cleanup = super::worktree_tests::writer_process_cleanup(&context);
    let (_, attempt) = dispatch(&rig, "writer", "compose-writer")?;
    let cache = PathBuf::from(fs::read_to_string(
        attempt.join("provider-state/compose-cache"),
    )?);
    assert!(cache.starts_with(rig.home.join("Library/Caches/provenant-lane-cargo")));
    assert_eq!(fs::read(cache.join("target/compose-target"))?, b"target");
    assert_eq!(fs::read(cache.join("cargo-home/compose-home"))?, b"cargo");
    // Shared config remains immutable until the host delivery owner composes #1255.
    assert_eq!(git(&rig, &["config", "--get", "user.name"])?, "Fixture");
    let store = super::worktree_tests::open_writer_store(&context)?;
    super::worktree_tests::wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

/// Invoked as `codex exec ...` by the real Registry, inside the compiled profile.
pub(crate) fn provider() -> Result {
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    let cwd = std::env::current_dir()?;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?);
    let state = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("CODEX_HOME")?);
    if prompt.ends_with("reader") || prompt.ends_with("reader-empty") {
        let primary = cwd.parent().and_then(Path::parent).ok_or("primary")?;
        fs::read(primary.join(".git/config"))?;
        let args: Vec<OsString> = [
            "/usr/bin/git",
            "--no-optional-locks",
            "status",
            "--porcelain",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        use provenant_supervisor::exec::{ExecOptions, Executor, SystemExecutor};
        let mut env: Vec<_> = std::env::vars_os().collect();
        env.extend([
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ]);
        let mut options = ExecOptions::new(&args, &cwd, std::time::Duration::from_secs(3));
        options.env = Some(&env);
        assert_eq!(code(&SystemExecutor::new(1)?.execute(options)?), 0);
        denied(&primary.join(".git/config"))?;
        if prompt.ends_with("reader-empty") {
            denied(&cwd.join(".agent-run/locks/compose-lock"))?;
        } else {
            fs::write(cwd.join(".agent-run/locks/compose-lock"), b"lock")?;
        }
        denied(&cwd.join("outside-exception"))?;
    } else if prompt.ends_with("writer") {
        let target =
            PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").ok_or("CARGO_TARGET_DIR missing")?);
        let cargo = PathBuf::from(std::env::var_os("CARGO_HOME").ok_or("CARGO_HOME missing")?);
        let cache = target.parent().ok_or("cache")?;
        assert_eq!(cargo, cache.join("cargo-home"));
        fs::create_dir_all(&target)?;
        fs::create_dir_all(&cargo)?;
        fs::write(target.join("compose-target"), b"target")?;
        fs::write(cargo.join("compose-home"), b"cargo")?;
        fs::write(
            state.join("compose-cache"),
            cache.as_os_str().as_encoded_bytes(),
        )?;
        for name in [
            ".claude/settings.json",
            ".claude/hooks/compose",
            ".kiro/settings/compose",
            ".gemini/config/mcp_config.json",
            ".cursor/extensions/compose",
            ".codex/config.toml",
            ".npm/compose",
        ] {
            denied(&home.join(name))?;
        }
        denied(
            &cwd.parent()
                .and_then(Path::parent)
                .ok_or("primary")?
                .join(".git/config"),
        )?;
        let error = std::os::unix::fs::symlink(cwd.join("AGENTS.md"), state.join("escape"))
            .err()
            .ok_or("provider state allowed symlink creation")?;
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    } else {
        return Err("unknown compose prompt".into());
    }
    fs::write(state.join("compose-proof"), b"proved")?;
    std::io::stdout().write_all(b"{\"type\":\"thread.started\",\"thread_id\":\"compose-fixture\"}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"confinement proved\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n")?;
    Ok(())
}
fn denied(path: &Path) -> Result {
    let error = fs::write(path, b"forbidden")
        .err()
        .ok_or_else(|| format!("write escaped: {}", path.display()))?;
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{}: {error}",
        path.display()
    );
    Ok(())
}
