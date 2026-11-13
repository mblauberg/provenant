//! Production CLI dispatch against a fake Codex executable, never live credentials.
use super::worktree_tests::support::{Result, TestRig};
use super::worktree_tests::{git, ok, repository, writer_backend_available, writer_context};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[test]
fn guardian_capture_masks_split_credentials_before_retention() {
    struct Capture;
    impl super::Process for Capture {
        fn supervise(
            &self,
            _: &[std::ffi::OsString],
            hooks: provenant_supervisor::GuardianHooks<'_>,
        ) -> provenant_supervisor::custody::Result<()> {
            use provenant_os::{capture::StreamCapture, exec::StreamPolicy};
            let stdout = hooks
                .stdout_filter
                .ok_or_else(|| std::io::Error::other("stdout redaction"))?;
            let stderr = hooks
                .stderr_filter
                .ok_or_else(|| std::io::Error::other("stderr redaction"))?;
            let mut stdout =
                StreamCapture::new(StreamPolicy::Capture { limit: 1024 }, None, Some(stdout))
                    .map_err(std::io::Error::other)?;
            let mut stderr =
                StreamCapture::new(StreamPolicy::Capture { limit: 1024 }, None, Some(stderr))
                    .map_err(std::io::Error::other)?;
            // Interleave streams and split the token marker and body. EOF
            // must flush safe trailing text without leaking pending bytes.
            for bytes in [
                b"credential gh".as_slice(),
                b"p_abcdefghijklmnop",
                b"qrstuvwxyz0123456789 end",
            ] {
                assert!(stdout.push(bytes).is_ok());
                assert!(stderr.push(bytes).is_ok());
            }
            stdout.finish();
            stderr.finish();
            for capture in [&stdout, &stderr] {
                assert_eq!(capture.outcome().bytes, b"credential [REDACTED] end");
                assert!(!capture.outcome().filter_failed);
            }
            Ok(())
        }
    }
    assert_eq!(super::supervise(&[], &Default::default(), &Capture), 0);
}

fn fixture() -> Result<TestRig> {
    let mut rig = repository()?;
    let seed = rig
        .state
        .join("cargo-target-seeds/generations/fixture/target");
    fs::create_dir_all(&seed)?;
    fs::write(seed.join("artifact"), b"host seed")?;
    ok(
        &rig,
        &["worktree", "create", "compose", "--base", "HEAD", "--json"],
    )?;
    for name in [
        ".cargo/bin",
        ".cargo/registry",
        ".cargo/git",
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
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
    // The rig's CODEX_HOME is the auto-detected Codex identity.
    fs::write(
        rig.root.join("codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-compose"}}"#,
    )?;
    // Existing global configuration stays unreadable to the lane. Git must
    // use the provider's pinned config environment to run normally.
    fs::write(
        rig.home.join(".gitconfig"),
        b"[include]\npath = /host-config-must-not-load\n",
    )?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("codex"))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    Ok(rig)
}

fn dispatch(rig: &TestRig, mode: &str) -> Result<PathBuf> {
    let prefix: Vec<std::ffi::OsString> = vec![
        "--state-dir".into(),
        rig.state.clone().into_os_string(),
        "--project".into(),
        rig.project.join(".worktrees/compose").into_os_string(),
    ];
    let mut args = prefix.clone();
    args.extend([
        "run".into(),
        "task".into(),
        "--model".into(),
        "codex/gpt-6-luna".into(),
        mode.into(),
        "--name".into(),
        "compose-dispatch".into(),
        "--json".into(),
    ]);
    if mode == "writer" {
        args.extend([
            "--mode".into(),
            "worktree-write".into(),
            "--worktree".into(),
            rig.project.join(".worktrees/compose").into_os_string(),
        ]);
    }
    // The production binary keeps both acquisition and its same-binary
    // guardian on the native boot observer. Only Codex is the test executable.
    let executable = option_env!("CARGO_BIN_EXE_provenant")
        .ok_or("production binary path is only available to integration targets")?;
    let output = rig.execute_os(Path::new(executable), &args, None)?;
    let mut stderr = String::from_utf8_lossy(&output.stderr.bytes).into_owned();
    // The launch names a graph; waiting settles it and status names its attempt.
    let mut wait = prefix.clone();
    wait.extend(
        ["wait", "compose-dispatch", "--timeout", "30s", "--json"]
            .into_iter()
            .map(Into::into),
    );
    let waited = rig.execute_os(Path::new(executable), &wait, None)?;
    stderr.push_str(&String::from_utf8_lossy(&waited.stderr.bytes));
    let mut status = prefix;
    status.extend(
        ["status", "compose-dispatch", "--json"]
            .into_iter()
            .map(Into::into),
    );
    let status = rig.execute_os(Path::new(executable), &status, None)?;
    let stdout = String::from_utf8_lossy(&status.stdout.bytes).into_owned();
    assert!(
        !stderr.contains("ignored invalid configuration"),
        "a domain section is not an unknown key to the census policy read"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("{error}: stdout={stdout} stderr={stderr}"))?;
    // Codex's fixture protocol carries no observed answering model, so the
    // lane settles as failed. Check the provider's exit code independently
    // of that outcome, as compose-proof precedes its final protocol output.
    let attempt_ref = value["data"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["kind"] == "attempt")
        .and_then(|row| row["attempt_id"].as_str())
        .ok_or_else(|| format!("no attempt: stdout={stdout} stderr={stderr}"))?;
    let (_, attempt) = attempt_ref.rsplit_once('/').ok_or("attempt reference")?;
    let attempt = fs::read_dir(rig.state.join("runs"))?
        .flatten()
        .map(|run| run.path().join("attempts").join(attempt))
        .find(|path| path.is_dir())
        .ok_or_else(|| format!("no attempt directory: stdout={stdout}"))?;
    let record = attempt_record(rig, &attempt)?;
    assert_eq!(
        record["state"]["exit_code"],
        serde_json::json!(0),
        "provider must exit successfully: state={} stderr={}",
        record["state"],
        capture(&attempt.join("stderr.log")).unwrap_or_default()
    );
    let proof =
        fs::read(attempt.join("provider-state/candidate-0/compose-proof")).map_err(|error| {
            format!(
                "compose proof: {error}\nprovider stderr: {}\nstdout={stdout}",
                capture(&attempt.join("stderr.log")).unwrap_or_default()
            )
        })?;
    assert_eq!(proof, b"proved");
    Ok(attempt)
}

fn attempt_record(rig: &TestRig, attempt: &Path) -> Result<serde_json::Value> {
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
    Ok(serde_json::from_slice(&raw)?)
}

fn capture(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(8192).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn read_only_dispatch_applies_bound_git_and_project_writes() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    // Separate state roots avoid the production host startup-spacing window.
    for configured in [false, true] {
        let rig = fixture()?;
        // This proves confinement's denied writes and selected exceptions.
        fs::write(
            rig.state.join("config.toml"),
            "schema_version = 1\n[knobs]\n\"lanes.sandbox\" = \"on\"\n[gc]\nfree_floor='1B'\n",
        )?;
        let cleanup = provenant_supervisor::fixture::ProcessCleanupGuard::new(&rig.state);
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
        let attempt = dispatch(&rig, mode)?;
        let cargo: serde_json::Value =
            serde_json::from_slice(&fs::read(attempt.join("cargo-setup.json"))?)?;
        assert_eq!(
            cargo,
            serde_json::json!({"status": "skipped", "reason": "read_only_lane"})
        );
        let record = attempt_record(&rig, &attempt)?;
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
        provenant_test_support::wait_for(
            "reader reservation release",
            std::time::Duration::from_secs(5),
            || !attempt.join("reservation.json").exists(),
            |released| *released,
        )?;
        cleanup.disarm();
    }
    Ok(())
}

pub(crate) fn writer_dispatch_uses_lane_cache_and_narrow_provider_state() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = fixture()?;
    // The cargo seed is on by default; this test is about the lane cache and
    // provider state, so it pins the seed off. The fixture asserts that
    // writes outside its grants fail, so the writer runs sandboxed.
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[knobs]\n\"lanes.sandbox\" = \"on\"\n[gc]\nfree_floor='1B'\n[domains.fabric.lanes.cargo_seed]\nenabled = false\n",
    )?;
    let (_, context) = writer_context(&rig)?;
    let cleanup = super::worktree_tests::writer_process_cleanup(&context);
    let attempt = dispatch(&rig, "writer")?;
    let cargo: serde_json::Value =
        serde_json::from_slice(&fs::read(attempt.join("cargo-setup.json"))?)?;
    assert_eq!(
        cargo,
        serde_json::json!({"status": "skipped", "reason": "disabled"})
    );
    let cache = PathBuf::from(fs::read_to_string(
        attempt.join("provider-state/candidate-0/compose-cache"),
    )?);
    assert!(cache.starts_with(rig.home.join("Library/Caches/provenant-lane-cargo")));
    assert!(
        fs::read(cache.join(".provenant-worktree-owner"))?
            .starts_with(b"provenant-lane-cargo-owner-v2\n")
    );
    assert_eq!(fs::read(cache.join("target/compose-target"))?, b"target");
    assert_eq!(
        fs::read(
            rig.state
                .join("cargo-target-seeds/generations/fixture/target/artifact")
        )?,
        b"host seed"
    );
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
    let cwd = std::env::current_dir().map_err(|error| format!("current directory: {error}"))?;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?);
    let state = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("CODEX_HOME")?);
    if prompt.ends_with("reader") || prompt.ends_with("reader-empty") {
        let primary = cwd.parent().and_then(Path::parent).ok_or("primary")?;
        fs::read(primary.join(".git/config"))
            .map_err(|error| format!("common Git read: {error}"))?;
        denied(&primary.join(".git/config"))?;
        if prompt.ends_with("reader-empty") {
            denied(&cwd.join(".agent-run/locks/compose-lock"))?;
        } else {
            fs::write(cwd.join(".agent-run/locks/compose-lock"), b"lock")
                .map_err(|error| format!("configured project write: {error}"))?;
        }
        denied(&cwd.join("outside-exception"))?;
    } else if prompt.ends_with("writer") {
        let target =
            PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").ok_or("CARGO_TARGET_DIR missing")?);
        let cargo = PathBuf::from(std::env::var_os("CARGO_HOME").ok_or("CARGO_HOME missing")?);
        let cache = target.parent().ok_or("cache")?;
        // Cargo home is lane-private, inside the writer cache. The user's
        // own stores and installed binaries are read-only.
        assert_eq!(cargo, cache.join("cargo-home"));
        fs::create_dir_all(&target)?;
        fs::write(target.join("compose-target"), b"target")?;
        fs::create_dir_all(cargo.join("registry"))?;
        fs::write(cargo.join("registry/compose-home"), b"cargo")?;
        let host_state = PathBuf::from(
            std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV)
                .ok_or("host state missing")?,
        );
        denied(&host_state.join("cargo-target-seeds/generations/fixture/target/artifact"))?;
        fs::write(
            state.join("compose-cache"),
            cache.as_os_str().as_encoded_bytes(),
        )?;
        for name in [
            ".claude/settings.json",
            ".claude/hooks/compose",
            ".cursor/extensions/compose",
            ".gemini/config/mcp_config.json",
            ".codex/config.toml",
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
    fs::write(state.join("compose-proof"), b"proved")
        .map_err(|error| format!("provider state write: {error}"))?;
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
