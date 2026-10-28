//! Real dispatch composition with isolated, synthetic provider processes.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::ExitCode,
};
use support::{Result, TestRig, code};

fn fixture() -> Result {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|v| v == "--version") {
        let version = if std::path::Path::new(&args[0]).file_name()
            == Some(std::ffi::OsStr::new("opencode"))
        {
            "opencode v2.2.0"
        } else {
            "codex-cli 0.159.0"
        };
        writeln!(io::stdout(), "{version}")?;
        return Ok(());
    }
    if args.get(1).is_some_and(|v| v == "auth") {
        writeln!(
            io::stdout(),
            "{}",
            json!({"loggedIn":true,"email":"fixture@example.test"})
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|v| v == "run") {
        let mut prompt = String::new();
        io::stdin().read_to_string(&mut prompt)?;
        io::stdout().write_all(if prompt.contains("free limit") {
            include_bytes!("fixtures/providers/opencode/v2.2.0-free-limit.jsonl")
        } else if prompt.contains("model unavailable") {
            include_bytes!("fixtures/providers/opencode/model-unavailable.jsonl")
        } else {
            include_bytes!("fixtures/providers/opencode/v2.2.0-success.jsonl")
        })?;
        return Ok(());
    }
    if args.get(1).is_some_and(|v| v == "-p") {
        let dir = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").ok_or("CLAUDE_CONFIG_DIR")?);
        // Shell lanes get a private config directory; others use the account's.
        assert!(matches!(
            dir.file_name().and_then(|s| s.to_str()),
            Some(".claude-pro" | "claude-config")
        ));
        if args.iter().any(|v| v == "provenant-refresh-probe") {
            let expiry =
                u64::try_from(jiff::Timestamp::now().as_millisecond())?.saturating_add(3_600_000);
            fs::write(
                dir.join(".credentials.json"),
                serde_json::to_vec(
                    &json!({"claudeAiOauth":{"accessToken":"fixture-refreshed-token","expiresAt":expiry}}),
                )?,
            )?;
            fs::write(dir.join("refreshed"), b"yes")?;
            return Ok(());
        }
        assert!(std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_none());
        let fd: i32 = std::env::var("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR")?.parse()?;
        let token = provenant_os::sys::fixture_read_secret_pipe(fd)?;
        assert_eq!(token, b"fixture-refreshed-token");
        for frame in [
            json!({"type":"system","subtype":"init","session_id":"fixture-session"}),
            json!({"type":"assistant","session_id":"fixture-session","parent_tool_use_id":null,"message":{"model":"claude-sonnet-5-5","role":"assistant","content":[{"type":"text","text":"OK"}],"usage":{"input_tokens":1,"output_tokens":1}}}),
            json!({"type":"result","session_id":"fixture-session","subtype":"success","is_error":false,"result":"OK"}),
        ] {
            writeln!(io::stdout(), "{frame}")?;
        }
        return Ok(());
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt)?;
    if prompt.contains("fixture slow result") {
        std::thread::park_timeout(std::time::Duration::from_secs(3));
    }
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("CODEX_HOME")?);
    let auth: Value = serde_json::from_slice(&fs::read(home.join("auth.json"))?)?;
    assert_eq!(auth["tokens"]["account_id"], "fixture-one");
    let session = "01234567-89ab-cdef-0123-456789abcdef";
    let sessions = home.join("sessions/2026/10/24");
    fs::create_dir_all(&sessions)?;
    let recorded_session = if prompt.contains("mismatch") {
        "other-thread"
    } else {
        session
    };
    if !prompt.contains("missing") {
        fs::write(
            sessions.join(format!("rollout-2026-10-24T00-00-00-{session}.jsonl")),
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":recorded_session}}),
                json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
                json!({"type":"response_item","payload":{"content":"must not be retained"}})
            ),
        )?;
    }
    for frame in [
        json!({"type":"thread.started","thread_id":session}),
        json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
    ] {
        writeln!(io::stdout(), "{frame}")?;
    }
    Ok(())
}
fn opencode_dispatch_observes_v2_wal_session() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    for custom_data_home in [false, true] {
        let mut rig = TestRig::new()?;
        let cleanup = rig.process_cleanup();
        let data_home = if custom_data_home {
            let path = rig.root.join("custom-data");
            rig.env
                .push(("XDG_DATA_HOME".into(), path.clone().into_os_string()));
            path
        } else {
            rig.home.join(".local/share")
        };
        let data = data_home.join("opencode");
        fs::create_dir_all(&data)?;
        let db = rusqlite::Connection::open(data.join("opencode.db"))?;
        // Keep the writer open and disable checkpoints: the assistant row is
        // committed in the WAL when the guardian's read-only connection opens.
        db.execute_batch(
            r#"PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT);
            INSERT INTO session_message VALUES ('ses_x', 'assistant',
            '{"model":{"providerID":"opencode-go","id":"beam-free"},"finish":"stop"}');"#,
        )?;
        assert!(fs::metadata(data.join("opencode.db-wal"))?.len() > 0);
        let bin = rig.root.join("provider/bin");
        fs::create_dir_all(&bin)?;
        rig.copy_executable(&std::env::current_exe()?, &bin.join("opencode"))?;
        rig.env.retain(|(key, _)| key != "PATH");
        rig.env.push((
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ));
        let prompt = rig.root.join("prompt.md");
        for (operation, text, expected) in [
            ("opencode-success", "reply OK", "succeeded"),
            ("opencode-zen", "reply OK", "succeeded"),
            (
                "opencode-unavailable",
                "model unavailable",
                "model_unavailable",
            ),
            ("opencode-limit", "free limit", "quota_limited"),
        ] {
            fs::write(&prompt, text)?;
            let model = if expected == "model_unavailable" {
                "opencode/opencode/misspelt-free"
            } else if operation == "opencode-zen" {
                "opencode/opencode/beam-free"
            } else {
                "opencode/opencode-go/beam-free"
            };
            let provider = if model.starts_with("opencode/opencode-go/") {
                "opencode-go"
            } else {
                "opencode"
            };
            db.execute("UPDATE session_message SET data = ?1", [
                serde_json::to_string(&json!({"model": {"providerID": provider, "id": "beam-free"}, "finish": "stop"}))?
            ])?;
            let out = rig.command(&[
                "dispatch",
                "--model",
                model,
                "--operation-id",
                operation,
                "--prompt-file",
                prompt.to_str().ok_or("prompt")?,
                "--wait",
                "5s",
                "--json",
            ])?;
            assert_eq!(code(&out), 0, "{out:?}");
            let dispatched: Value = serde_json::from_slice(&out.stdout.bytes)?;
            let run = dispatched["data"]["run_id"].as_str().ok_or("run")?;
            let lanes = rig.command(&["lanes", "--id", run, "--json"])?;
            assert_eq!(code(&lanes), 0, "{lanes:?}");
            let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
            let lane = &lanes["data"]["lanes"][0];
            assert_eq!(lane["state"], expected, "{lane}");
            let account = if provider == "opencode-go" {
                "go"
            } else {
                "zen"
            };
            assert_eq!(lane["account"], account, "{lane}");
            assert_eq!(lane["applied_route"]["account_label"], account, "{lane}");
            assert_eq!(lane["applied_route"]["account_key"], account, "{lane}");
            if expected == "succeeded" {
                assert_eq!(lane["transcript"]["session"], "ses_x");
                assert_eq!(lane["observed_route"]["model"], model);
                assert_eq!(lane["observed_route"]["source"], "opencode_session");
            } else if expected == "quota_limited" {
                assert!(
                    lane["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.starts_with("Free usage limit reached.")),
                    "{lane}"
                );
            } else {
                assert_eq!(lane["reason"], "Model unavailable: opencode/misspelt-free");
            }
        }
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
    Ok(())
}

fn codex_dispatch_observes_rollout() -> Result {
    for (index, prompt) in ["reply OK", "missing", "mismatch"].iter().enumerate() {
        let mut rig = TestRig::new()?;
        let cleanup = rig.process_cleanup();
        // The production read-only profile anchors the host Rust runtime roots.
        for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
            fs::create_dir_all(rig.home.join(path))?;
        }
        fs::write(rig.home.join(".rustup/settings.toml"), "")?;
        let bin = rig.root.join("provider/bin");
        fs::create_dir_all(&bin)?;
        rig.copy_executable(&std::env::current_exe()?, &bin.join("codex"))?;
        let account = rig.root.join("codex");
        fs::write(
            account.join("auth.json"),
            br#"{"tokens":{"account_id":"fixture-one"}}"#,
        )?;
        rig.env.retain(|(k, _)| k != "PATH");
        rig.env.push((
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ));
        let prompt_file = rig.root.join("prompt.txt");
        fs::write(&prompt_file, prompt)?;
        let op = format!("{:032x}", index + 1);
        let out = rig.command(&[
            if index == 0 { "run" } else { "dispatch" },
            if index == 0 { "--route" } else { "--model" },
            if index == 0 {
                "utility"
            } else {
                "codex/gpt-6-luna"
            },
            "--effort",
            "low",
            "--operation-id",
            &op,
            "--prompt-file",
            prompt_file.to_str().ok_or("prompt")?,
            "--wait",
            "5",
            "--json",
        ])?;
        assert_eq!(
            code(&out),
            if index == 0 && !cfg!(target_os = "macos") {
                1
            } else {
                0
            },
            "{} {}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        let lanes = rig.command(&["lanes", "--all", "--json"])?;
        let value: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let rows = value["data"]["lanes"]
            .as_array()
            .ok_or_else(|| format!("lanes: {value}"))?;
        let lane = rows.first().ok_or_else(|| format!("lane: {value}"))?;
        if !cfg!(target_os = "macos") {
            assert_eq!(lane["state"], "rejected", "{lane}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("UnsupportedPlatform")),
                "{lane}"
            );
            rig.assert_legacy_unchanged()?;
            cleanup.disarm();
            return Ok(());
        }
        if index == 0 {
            assert_eq!(lane["state"], "succeeded", "{lane}");
            assert_eq!(lane["observed_route"]["model"], "codex/gpt-6-luna");
            assert_eq!(lane["observed_route"]["source"], "codex_rollout");
            assert!(lane["applied_route"]["account_label"].as_str().is_some());
            let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
            let id = lane["attempt_id"].as_str().ok_or("attempt")?;
            assert_eq!(result["data"]["result_tails"][id]["bytes"]["data"], "OK");
            fs::write(&prompt_file, "fixture slow result")?;
            let slow = rig.command(&[
                "run",
                "--model",
                "codex/gpt-6-luna",
                "--effort",
                "low",
                "--prompt-file",
                prompt_file.to_str().ok_or("prompt")?,
                "--wait",
                "1s",
            ])?;
            assert_eq!(code(&slow), 124, "{slow:?}");
            let text = String::from_utf8(slow.stdout.bytes)?;
            let id = text
                .lines()
                .find_map(|line| line.strip_prefix("attempt "))
                .ok_or("timeout attempt")?;
            assert!(
                text.contains(&format!("provenant lanes --id {id} --wait 4m")),
                "{text}"
            );
            let settled = rig.command(&["lanes", "--id", id, "--wait", "10s", "--json"])?;
            assert_eq!(code(&settled), 0, "{settled:?}");
        } else {
            assert_eq!(lane["state"], "failed", "{lane}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("provider identity was not observed: ")),
                "{lane}"
            );
        }
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
    Ok(())
}
fn claude_fixture() -> Result<TestRig> {
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("claude"))?;
    let account = rig.home.join(".claude-pro");
    fs::create_dir(&account)?;
    fs::write(
        account.join(".credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"fixture-expired-token","expiresAt":1}}"#,
    )?;
    rig.env.retain(|(k, _)| k != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let registered = rig.command(&[
        "accounts",
        "register",
        "pro",
        "--adapter",
        "claude",
        "--directory",
        account.to_str().ok_or("account path")?,
        "--json",
    ])?;
    assert_eq!(
        code(&registered),
        0,
        "{}",
        String::from_utf8_lossy(&registered.stdout.bytes)
    );
    let listed = rig.command(&["accounts", "list", "--json"])?;
    let listed: Value = serde_json::from_slice(&listed.stdout.bytes)?;
    assert_eq!(
        listed["data"]["entries"]["claude-pro"]["label"],
        "pro",
        "{} config={}",
        listed,
        fs::read_to_string(rig.state.join("config.toml"))?
    );
    Ok(rig)
}

fn opus_writer_dispatch_advises_a_cheaper_route() -> Result {
    {
        let rig = claude_fixture()?;
        let cleanup = rig.process_cleanup();
        let mut config = OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?;
        writeln!(
            config,
            "\n[domains.routing.pacing.accounts.pro]\ndeadline={{reset_of='removed'}}"
        )?;
        let prompt = rig.root.join("prompt.md");
        fs::write(&prompt, "Reply OK.")?;
        let out = rig.command(&[
            "dispatch",
            "--operation-id",
            "stale-profile",
            "--model",
            "claude/claude-sonnet-5-5",
            "--account",
            "pro",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--wait",
            "10s",
            "--json",
        ])?;
        assert_eq!(code(&out), 0, "{out:?}");
        let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let run = result["data"]["run_id"].as_str().ok_or("run")?;
        let lanes = rig.command(&["lanes", "--id", run, "--json"])?;
        assert_eq!(code(&lanes), 0, "{lanes:?}");
        let result: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let lane = &result["data"]["lanes"][0];
        assert_eq!(lane["state"], "rejected", "{lane}");
        assert!(
            lane["reason"].as_str().is_some_and(|message| message
                .contains("pacing.accounts.pro.deadline.reset_of")
                && message.contains("removed")),
            "{lane}"
        );
        cleanup.disarm();
    }
    for (model, writer, suppress) in [
        ("claude/claude-opus-5-5", true, false),
        ("claude/claude-opus-5-5", true, true),
        ("claude/claude-opus-5-5", false, false),
        ("claude/claude-sonnet-5-5", true, false),
        ("claude/claude-sonnet-5-6", false, false),
    ] {
        let rig = claude_fixture()?;
        let cleanup = rig.process_cleanup();
        if suppress {
            let mut config = OpenOptions::new()
                .append(true)
                .open(rig.state.join("config.toml"))?;
            writeln!(
                config,
                "\n[domains.routing]\nsuppress_opus_writer_warning = true"
            )?;
        }
        let git = provenant_os::git::git_executable().ok_or("git")?;
        for args in [
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "base"],
            vec!["branch", "-M", "main"],
        ] {
            assert_eq!(code(&rig.execute(&git, &args, None)?), 0);
        }
        let prompt = rig.root.join("prompt.md");
        fs::write(&prompt, "Reply OK without changing files.")?;
        let mut args = vec![
            "dispatch",
            "--operation-id",
            "opus-writer",
            "--model",
            model,
            "--account",
            "pro",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--json",
        ];
        if writer {
            args.extend([
                "--mode",
                "worktree-write",
                "--worktree-branch",
                "fixture/opus",
                "--base",
                "main",
                "--deliver",
                "none",
            ]);
        }
        let out = rig.command(&args)?;
        assert_eq!(
            code(&out),
            0,
            "{} {}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        let stderr = String::from_utf8_lossy(&out.stderr.bytes);
        assert!(!stderr.contains("not in the catalogue"), "{stderr}");
        let reply: Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(reply["status"], "ok", "{reply}");
        if !writer || suppress || model.contains("sonnet") {
            assert!(!stderr.contains("advisory"), "{stderr}");
        } else {
            let advisory = stderr
                .lines()
                .find(|line| line.contains("advisory"))
                .ok_or_else(|| format!("missing Opus writer advisory: {stderr}"))?;
            assert!(advisory.contains("claude/claude-opus-5-5"), "{advisory}");
            assert!(advisory.contains("codex/gpt-6.1-sol"), "{advisory}");
            assert!(advisory.contains("principal"), "{advisory}");
            assert_eq!(
                stderr
                    .lines()
                    .filter(|line| line.contains("advisory"))
                    .count(),
                1
            );
        }
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
    Ok(())
}

fn claude_dispatch_refreshes_registered_account() -> Result {
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    let account = rig.home.join(".claude-pro");
    let manifest = rig.root.join("manifest.json");
    fs::write(
        &manifest,
        serde_json::to_vec(
            &json!({"schema_version":1,"tasks":[{"prompt":"reply OK","route":{"kind":"model","provider":"claude","model":"claude-sonnet-5-5","effort":"medium"},"account":"pro"}]}),
        )?,
    )?;
    let out = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest")?,
        "--operation-id",
        "fixture-claude",
        "--wait",
        "5",
        "--json",
    ])?;
    assert_eq!(
        code(&out),
        0,
        "{} {}",
        String::from_utf8_lossy(&out.stdout.bytes),
        String::from_utf8_lossy(&out.stderr.bytes)
    );
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lane = &lanes["data"]["lanes"][0];
    if !cfg!(target_os = "macos") {
        assert_eq!(lane["state"], "rejected", "{lane}");
        assert!(
            lane["reason"]
                .as_str()
                .is_some_and(|r| r.contains("UnsupportedPlatform")),
            "{lane}"
        );
        assert!(account.join("refreshed").exists());
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
        return Ok(());
    }
    assert_eq!(lane["state"], "succeeded", "{lane}");
    assert_eq!(lane["account"], "pro");
    assert_eq!(lane["observed_route"]["model"], "claude/claude-sonnet-5-5");
    assert!(account.join("refreshed").exists());
    let attempt = rig
        .state
        .join("runs")
        .join(lane["run_id"].as_str().ok_or("run")?)
        .join("attempts")
        .join(lane["attempt_id"].as_str().ok_or("attempt")?);
    let authority: Value =
        serde_json::from_slice(&fs::read(attempt.join("credential-authority.json"))?)?;
    assert_eq!(authority["actor"], lane["run_id"]);
    assert_eq!(
        authority["source"]["path"],
        account.join(".credentials.json").to_str().ok_or("source")?
    );
    assert_eq!(authority["purpose"], "lane dispatch");
    for name in [
        "launch.json",
        "attempt.json",
        "stdout.log",
        "stderr.log",
        "credential-authority.json",
    ] {
        if let Ok(bytes) = fs::read(attempt.join(name)) {
            assert!(!String::from_utf8_lossy(&bytes).contains("fixture-refreshed-token"));
        }
    }
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

/// Whether graph fixtures can launch confined providers here.
fn graph_fixture_available() -> Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    if let Err(error) = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    ) {
        if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some() {
            return Err(error.into());
        }
        writeln!(io::stderr(), "graph fixture skipped: {error}")?;
        return Ok(false);
    }
    Ok(true)
}

fn graph_settlement_records_a_failed_advance_for_status() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    for path in [
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
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let route = json!({"adapter":"claude","model":"claude/claude-sonnet-5-5","effort":"medium","account":"pro"});
    // The dependant names a worktree that was never created, so starting it
    // fails when the first node settles and the guardian advances the graph.
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({"schema_version":1,"graph_id":"broken","nodes":[
            {"id":"first","prompt":"reply OK","access":"read_only","route":route},
            {"id":"second","prompt":"reply OK","access":"worktree_write","worktree":".worktrees/missing","depends_on":["first"],"route":route}
        ]}))?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    // Status only observes: the guardian's advance is the sole writer here.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let error = loop {
        let status = rig.command(&["fabric", "graph", "status", "broken", "--json"])?;
        assert_eq!(code(&status), 0);
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if let Some(error) = status["data"]["advance_error"].as_str() {
            break error.to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no advance error recorded: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    assert!(!error.is_empty());
    let human = rig.command(&["fabric", "graph", "status", "broken"])?;
    assert!(
        String::from_utf8_lossy(&human.stdout.bytes).contains(&error),
        "{}",
        String::from_utf8_lossy(&human.stdout.bytes)
    );
    cleanup.disarm();
    Ok(())
}

fn graph_settlement_advances_writer_and_account_routed_review() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    let git = std::env::var_os("PROVENANT_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    let commit = rig.execute(
        &git,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "Initial",
        ],
        None,
    )?;
    assert_eq!(code(&commit), 0);
    let created = rig.command(&[
        "worktree",
        "create",
        "graph-writer",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(
        code(&created),
        0,
        "{}",
        String::from_utf8_lossy(&created.stdout.bytes)
    );
    for path in [
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
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let route = json!({"adapter":"claude","model":"claude/claude-sonnet-5-5","effort":"medium","account":"pro"});
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({"schema_version":1,"graph_id":"settlement","nodes":[
            {"id":"write","prompt":"reply OK","access":"worktree_write","worktree":".worktrees/graph-writer","delivery":"none","route":route},
            {"id":"review","prompt":"reply OK","access":"read_only","depends_on":["write"],"route":route}
        ]}))?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    // Status only observes. No resume or other mutating command drives the graph.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = rig.command(&["fabric", "graph", "status", "settlement", "--json"])?;
        assert_eq!(
            code(&status),
            0,
            "{} {}",
            String::from_utf8_lossy(&status.stdout.bytes),
            String::from_utf8_lossy(&status.stderr.bytes)
        );
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if status["data"]["status"] == "complete" {
            let nodes = status["data"]["checkpoint"]["nodes"]
                .as_array()
                .ok_or("nodes")?;
            assert_eq!(nodes.len(), 2);
            for node in nodes {
                assert_eq!(
                    node["state"],
                    "succeeded",
                    "{status}; lanes={}",
                    String::from_utf8_lossy(
                        &rig.command(&["lanes", "--all", "--json"])?.stdout.bytes
                    )
                );
                assert_eq!(node["attempts"].as_array().ok_or("attempts")?.len(), 1);
            }
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph did not advance: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    }
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lanes = lanes["data"]["lanes"].as_array().ok_or("lanes")?;
    assert_eq!(lanes.len(), 2);
    for lane in lanes {
        assert_eq!(lane["state"], "succeeded", "{lane}");
        assert_eq!(lane["account"], "pro", "{lane}");
    }
    // Recreate a checkpoint saved before the terminal observation, after the
    // successful writer's checkout has already been pruned.
    assert_eq!(
        code(&rig.command(&["worktree", "remove", "graph-writer", "--json",])?),
        0
    );
    assert!(!rig.project.join(".worktrees/graph-writer").exists());
    {
        use provenant_core::{CancelToken, Clock, RootInputs, WaitBudget};
        use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphJournal, NodeState};
        let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let project = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
        )?
        .project_id();
        let clock = provenant_os::clock::SystemClock;
        let cancel = CancelToken::new();
        let mut journal = FileJournal::open(
            &roots.state,
            project,
            "settlement",
            WaitBudget::cli(clock.mono_now(), 5)?,
            &cancel,
            &clock,
        )?;
        let mut checkpoint = journal.load("settlement")?.ok_or("checkpoint")?;
        checkpoint.nodes[0].state = NodeState::Running;
        let attempt = checkpoint.nodes[0].attempts.last_mut().ok_or("attempt")?;
        attempt.state = AttemptState::Starting;
        attempt.outcome = None;
        journal.save(&checkpoint)?;
    }
    let replay = rig.command(&[
        "fabric",
        "graph",
        "resume",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&replay),
        0,
        "{}",
        String::from_utf8_lossy(&replay.stdout.bytes)
    );
    let replay: Value = serde_json::from_slice(&replay.stdout.bytes)?;
    assert_eq!(replay["data"]["status"], "complete", "{replay}");
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["state"], "succeeded",
        "{replay}"
    );
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["attempts"]
            .as_array()
            .ok_or("attempts")?
            .len(),
        1
    );
    cleanup.disarm();
    Ok(())
}

/// Finish one attempt on `stdout` as provider output and return the quota
/// record its refusal persisted for the applied account.
fn persisted_refusal(
    provider: &str,
    model: &str,
    (label, key): (&str, &str),
    stdout: &[u8],
) -> Result<provenant_quota::AccountRecord> {
    let (rig, terminal) = persisted_attempt(provider, model, (label, key), stdout, b"", 1)?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::QuotaLimited,
            ..
        }
    ));
    Ok(provenant_quota::AccountRecord::decode(&fs::read(
        rig.state.join(format!("quota/{key}/{provider}.json")),
    )?)?)
}

/// Publish sealed captures through the production holder and provider registry.
fn persisted_attempt(
    provider: &str,
    model: &str,
    (label, key): (&str, &str),
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i64,
) -> Result<(TestRig, provenant_run::AttemptRecord)> {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, RootInputs, RunId, TaskId,
        WaitBudget,
    };
    use provenant_os::clock::SystemClock;
    use provenant_os::exec::Termination;
    use provenant_os::fs::Directory;
    use provenant_records::{
        CanonicalV1,
        run::{CanonicalRequest, HarnessProfile, RequestedRoute, RouteRecord, TaskRequest},
    };
    use provenant_run::{ExecutionContext, NewAttempt, PreparedRun, Request, RunStore};
    use provenant_supervisor::custody::CaptureTotals;
    use provenant_supervisor::custody::ExitObservation;
    use provenant_supervisor::custody::PublicationState;
    use provenant_supervisor::custody::Scope;
    use provenant_supervisor::custody::SettlementEvidence;

    let rig = TestRig::new()?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    let project_id = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id();
    let actor_id = ActorId::from_bytes([22; 16]);
    let execution = ExecutionContext {
        state_root: roots.state.clone(),
        project_id,
        actor_id,
        read_prompts: true,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 10)?,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let run_id = RunId::from_bytes([23; 16]);
    let attempt_id = AttemptId::from_bytes([24; 16]);
    let task_id = TaskId::from_bytes([25; 16]);
    let applied = RouteRecord {
        provider: Some(provider.into()),
        model: Some(model.into()),
        effort: None,
        account_label: Some(label.into()),
        account_key: Some(key.into()),
        source: Some("fixture_route".into()),
        pool_selection: None,
    };
    let prompt = "reply OK";
    let prepared = PreparedRun {
        run_id,
        operation_id: OperationId::parse(&format!("{provider}-cooldown"))?,
        request: CanonicalRequest {
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![TaskRequest {
                title: None,
                base: None,
                base_ref: None,
                profile: HarnessProfile::default(),
                task_id,
                prompt_digest: CanonicalV1::file_digest(prompt.as_bytes()),
                route: RequestedRoute::Model {
                    provider: provider.into(),
                    model: model.into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
                authority_digest: Digest::from_bytes([26; 32]),
                writer: None,
                provider_timeout_seconds: 60,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: None,
            parent_attempt: None,
        },
        attempts: vec![NewAttempt {
            task_id,
            attempt_id,
            prompt: prompt.into(),
        }],
        created_at: clock.wall_now()?,
    };
    store.create(&prepared, &call)?;
    let started_at = clock.wall_now()?;
    let mut running = store.record(attempt_id, &call)?;
    running.revision = 2;
    running.resolved_route = Some(applied.clone());
    running.applied_route = Some(applied.clone());
    running.started_at = Some(started_at);
    running.provider_launched = Some(true);
    running.state = provenant_run::AttemptState::Running;
    let running_bytes = CanonicalV1::bytes(&running)?.as_bytes().to_vec();
    let database = rusqlite::Connection::open(roots.state.as_path().join("run.sqlite3"))?;
    database.execute(
        "UPDATE attempts SET revision=?1,record=?2 WHERE id=?3",
        rusqlite::params![2_i64, running_bytes, attempt_id.to_string()],
    )?;

    let attempt_dir = roots
        .state
        .as_path()
        .join("runs")
        .join(run_id.to_string())
        .join("attempts")
        .join(attempt_id.to_string());
    let provider_state = attempt_dir.join("provider-state");
    fs::create_dir_all(&provider_state)?;
    let evidence = SettlementEvidence::DarwinCensusAndCustodyLease;
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: run_id,
            attempt: attempt_id,
            generation: 0,
        },
        termination: Some(Termination::Exit(exit_code)),
        never_spawned: false,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: true,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: evidence,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    };
    let mut sealed_exit = serde_json::to_value(&exit)?;
    let fields = sealed_exit.as_object_mut().ok_or("exit fixture")?;
    fields.remove("publication");
    fields.insert("schema_version".into(), 1.into());
    let exit_bytes = serde_json::to_vec(&sealed_exit)?;
    let mut exit_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("process-exit.json"))?;
    exit_file.write_all(&exit_bytes)?;
    exit_file.sync_all()?;

    // Fixture provider output enters Registry.decode through RunStore::finish.
    let mut stdout_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("stdout.log"))?;
    stdout_file.write_all(stdout)?;
    stdout_file.sync_all()?;
    let mut stderr_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("stderr.log"))?;
    stderr_file.write_all(stderr)?;
    stderr_file.sync_all()?;
    let attempt = Directory::open_anchor(&attempt_dir)?;
    let terminal = store.finish(
        attempt_id,
        &attempt,
        &exit,
        &provenant_cli::run::Registry::default(),
        &call,
    )?;
    Ok((rig, terminal))
}

fn agy_denial_result_reaches_terminal_reason() -> Result {
    let (rig, terminal) = persisted_attempt(
        "agy",
        "agy/gemini-3.8-flash",
        ("work", "google-fixture"),
        include_bytes!("fixtures/providers/agy/denied-result.jsonl"),
        b"",
        0,
    )?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Failed,
            ..
        }
    ));
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    assert_eq!(code(&lanes), 0, "{:?}", lanes.stderr.bytes);
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    assert_eq!(
        lanes["data"]["lanes"][0]["reason"],
        "provider denied: RunCommand"
    );
    Ok(())
}

fn agy_denied_command_reaches_result_and_terminal_reason() -> Result {
    let denial = include_bytes!("fixtures/providers/agy/command-denied.stderr");
    // Synthetic partial stdout and the real stderr-only failure case.
    let partial = br#"{"event":"step_update","step_update":{"conversation_id":"fixture-session","step_type":"agent_response","text_delta":"I verified part of the claim."}}"#;
    let failed_response = br#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"I verified part of the claim.","error":"command permission denied","usage":{"input_tokens":0,"output_tokens":0}}}"#;
    for stdout in [
        b"".as_slice(),
        partial.as_slice(),
        failed_response.as_slice(),
    ] {
        let (rig, terminal) = persisted_attempt(
            "agy",
            "agy/gemini-3.8-flash",
            ("work", "google-fixture"),
            stdout,
            denial,
            1,
        )?;
        assert!(matches!(
            terminal.state,
            provenant_run::AttemptState::Terminal {
                outcome: provenant_run::Outcome::Failed,
                ..
            }
        ));
        let lanes = rig.command(&["lanes", "--all", "--json"])?;
        assert_eq!(code(&lanes), 0, "{:?}", lanes.stderr.bytes);
        let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let reason = lanes["data"]["lanes"][0]["reason"]
            .as_str()
            .ok_or("terminal reason")?;
        assert!(reason.contains(r#""command" permission"#), "{lanes}");
        let output = rig.command(&["output", &terminal.attempt_id.to_string(), "--json"])?;
        assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
        let output: Value = serde_json::from_slice(&output.stdout.bytes)?;
        let result = output["data"]["bytes"]["data"]
            .as_str()
            .ok_or("result bytes")?;
        assert!(result.contains(r#""command" permission"#), "{output}");
        if !stdout.is_empty() {
            assert!(
                result.starts_with("I verified part of the claim."),
                "{output}"
            );
        }
    }
    // A successful answer wins even when stderr contains a denial notice.
    let success = br#"{"event":"init","conversation_id":"fixture-session","init":{"model":"gemini-3.8-flash"}}
{"event":"result","result":{"conversation_id":"fixture-session","status":"SUCCESS","response":"Verified.","error":null,"usage":{"input_tokens":0,"output_tokens":0}}}"#;
    let (rig, terminal) = persisted_attempt(
        "agy",
        "agy/gemini-3.8-flash",
        ("work", "google-fixture"),
        success,
        denial,
        0,
    )?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Succeeded,
            ..
        }
    ));
    let output = rig.command(&["output", &terminal.attempt_id.to_string(), "--json"])?;
    assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
    let output: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(output["data"]["bytes"]["data"], "Verified.");
    Ok(())
}

fn opencode_free_pool_cools_second_model() -> Result {
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    let stdout = br#"{"type":"error","error":{"type":"provider.quota","message":"free tier exhausted","response":{"body":{"error":{"type":"FreeUsageLimitError","message":"fixture free allowance exhausted"}}}}}"#;
    let quota_record = persisted_refusal(
        "opencode",
        "opencode/opencode/deepseek-v4.1-flash-free",
        ("open-code-free", "keyless-free"),
        stdout,
    )?;
    assert_eq!(quota_record.refusals.len(), 1);
    assert_eq!(
        quota_record.refusals[0].pool.as_deref(),
        Some("opencode-free")
    );
    let now = quota_record.refusals[0].observed_unix_seconds;

    let second = ResolvedRoute {
        adapter: "opencode".into(),
        model: "opencode/glm-5.3-flash-free".into(),
        effort: None,
        catalogue_revision: "fixture-catalogue".into(),
        account_key: Some("keyless-free".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            availability_ref: "fixture-preflight".into(),
            expires_unix_seconds: now + 300,
        },
        confidentiality: RouteConfidentiality {
            evidence_ref: "fixture-access".into(),
        },
        pool_selection: None,
    };
    let pools = vec!["opencode-free".to_owned()];
    assert!(matches!(
        provenant_quota::Admission {
            policies: &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            now_unix_seconds: now,
            records: std::slice::from_ref(&quota_record),
            pools: &pools,
        }
        .check(&second),
        Err(provenant_quota::QuotaError::Refused(
            "free tier exhausted; use a paid route or wait for its allowance reset"
        ))
    ));
    Ok(())
}

fn opencode_free_reset_cools_pool_until_reset_and_restores() -> Result {
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    let fixture_stdout = include_bytes!("../tests/fixtures/providers/opencode/free-limit.jsonl");
    let quota_record = persisted_refusal(
        "opencode",
        "opencode/opencode/deepseek-v4.1-flash-free",
        ("open-code-free", "keyless-free"),
        fixture_stdout,
    )?;
    assert_eq!(quota_record.refusals.len(), 1);
    let refusal = &quota_record.refusals[0];
    assert_eq!(refusal.pool.as_deref(), Some("opencode-free"));
    let now = refusal.observed_unix_seconds;
    let resets = now + 240;
    assert_eq!(refusal.resets_unix_seconds, Some(resets));

    let first = ResolvedRoute {
        adapter: "opencode".into(),
        model: "opencode/deepseek-v4.1-flash-free".into(),
        effort: None,
        catalogue_revision: "fixture-catalogue".into(),
        account_key: Some("keyless-free".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            availability_ref: "fixture-preflight".into(),
            expires_unix_seconds: resets + 300,
        },
        confidentiality: RouteConfidentiality {
            evidence_ref: "fixture-access".into(),
        },
        pool_selection: None,
    };
    let mut second = first.clone();
    second.model = "opencode/glm-5.3-flash-free".into();

    let pools = vec!["opencode-free".to_owned()];
    let admit = |route: &ResolvedRoute, at| {
        provenant_quota::Admission {
            policies: &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            now_unix_seconds: at,
            records: std::slice::from_ref(&quota_record),
            pools: &pools,
        }
        .check(route)
    };

    // Both free models are refused until the stated reset time.
    for at in [now, now + 120, resets - 1] {
        for route in [&first, &second] {
            assert!(matches!(
                admit(route, at),
                Err(provenant_quota::QuotaError::Refused(
                    "free tier exhausted; use a paid route or wait for its allowance reset"
                ))
            ));
        }
    }

    // At and after the reset passes, both free models are admitted again.
    admit(&first, resets)?;
    admit(&second, resets)?;
    admit(&first, resets + 10)?;
    admit(&second, resets + 10)?;

    // Missing, null, negative and huge values fall back or clamp.
    for (retry_ms, expected_offset) in [
        (None, None),
        (Some(serde_json::Value::Null), None),
        (Some(serde_json::json!(-5000)), None),
        (Some(serde_json::json!(999_999_999)), Some(86_400)),
    ] {
        let mut err = serde_json::json!({
            "type": "provider.quota",
            "message": "free tier exhausted",
            "response": {
                "body": {
                    "error": {
                        "type": "FreeUsageLimitError",
                        "message": "fixture free allowance exhausted"
                    }
                }
            }
        });
        if let Some(val) = retry_ms {
            err["retryAfterMs"] = val;
        }
        let payload = serde_json::to_vec(&serde_json::json!({
            "type": "error",
            "error": err
        }))?;
        let record = persisted_refusal(
            "opencode",
            "opencode/opencode/deepseek-v4.1-flash-free",
            ("open-code-free", "keyless-free"),
            &payload,
        )?;
        let record_now = record.refusals[0].observed_unix_seconds;
        assert_eq!(
            record.refusals[0].resets_unix_seconds,
            expected_offset.map(|offset| record_now + offset),
            "failed for retry_ms: {err:?}"
        );
    }

    Ok(())
}

fn agy_individual_quota_cools_model_until_reset() -> Result {
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    let stdout = br#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"Individual quota reached. Resets in 7h23m.","usage":{"input_tokens":0,"output_tokens":0}}}"#;
    let quota_record = persisted_refusal(
        "agy",
        "agy/gemini-3.8-flash",
        ("work", "google-fixture"),
        stdout,
    )?;
    assert_eq!(quota_record.refusals.len(), 1);
    let refusal = &quota_record.refusals[0];
    assert_eq!(
        (refusal.model.as_str(), refusal.pool.as_deref()),
        ("gemini-3.8-flash", None)
    );
    let now = refusal.observed_unix_seconds;
    let resets = now + 7 * 3600 + 23 * 60;
    assert_eq!(refusal.resets_unix_seconds, Some(resets));

    let mut route = ResolvedRoute {
        adapter: "agy".into(),
        model: "gemini-3.8-flash".into(),
        effort: None,
        catalogue_revision: "fixture-catalogue".into(),
        account_key: Some("google-fixture".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            availability_ref: "fixture-preflight".into(),
            expires_unix_seconds: resets + 300,
        },
        confidentiality: RouteConfidentiality {
            evidence_ref: "fixture-access".into(),
        },
        pool_selection: None,
    };
    let admit = |route: &ResolvedRoute, now_unix_seconds| {
        provenant_quota::Admission {
            policies: &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            now_unix_seconds,
            records: std::slice::from_ref(&quota_record),
            pools: &[],
        }
        .check(route)
    };
    // Past the unknown-reset hour, the stated reset still holds the model.
    for at in [now, now + 3600, resets - 1] {
        assert!(matches!(
            admit(&route, at),
            Err(provenant_quota::QuotaError::Refused(
                "scoped provider refusal"
            ))
        ));
    }
    admit(&route, resets)?;
    route.model = "gemini-3.8-pro".into();
    admit(&route, now)?;
    Ok(())
}

fn owned_dispatch_retains_failed_attempt_and_replays() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
    ] {
        let output = rig.execute(&git, &args, None)?;
        assert_eq!(code(&output), 0);
    }
    let prompt = rig.root.join("owned-prompt.md");
    fs::write(&prompt, "Do not change files.")?;
    let args = [
        "dispatch",
        "--operation-id",
        "owned-replay",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "fixture/owned",
        "--base",
        "main",
        "--deliver",
        "push",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--json",
    ];
    // A failed attempt retains its checkout even when it is clean.
    let first = rig.command(&args)?;
    let first: Value = serde_json::from_slice(&first.stdout.bytes)?;
    let data = &first["data"];
    assert_eq!(data["worktree"]["owned"], true, "{first}");
    assert_eq!(
        data["worktree_settlements"][0]["state"], "retained",
        "{first}"
    );
    let path = data["worktree"]["path"].as_str().ok_or("worktree path")?;
    assert!(std::path::Path::new(path).exists());
    // Later repository protection governs new admissions and delivery, not
    // replay of this already accepted operation.
    for args in [
        vec![
            "remote",
            "add",
            "origin",
            rig.project.to_str().ok_or("project")?,
        ],
        vec![
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/fixture/owned",
        ],
    ] {
        assert_eq!(code(&rig.execute(&git, &args, None)?), 0);
    }
    let second = rig.command(&args)?;
    let second: Value = serde_json::from_slice(&second.stdout.bytes)?;
    assert_eq!(second["data"]["run_id"], data["run_id"], "{second}");
    assert!(std::path::Path::new(path).exists());
    // Explicit worktrees use the ordinary admission path, which must also
    // preserve replay after a publication target becomes protected.
    let explicit_args = [
        "dispatch",
        "--operation-id",
        "explicit-replay",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree",
        path,
        "--base",
        "main",
        "--deliver-remote",
        "origin",
        "--deliver-branch",
        "published-explicit",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--json",
    ];
    let explicit = rig.command(&explicit_args)?;
    let explicit: Value = serde_json::from_slice(&explicit.stdout.bytes)?;
    assert!(explicit["data"]["run_id"].is_string(), "{explicit}");
    assert_eq!(
        code(&rig.execute(
            &git,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/published-explicit",
            ],
            None
        )?),
        0
    );
    let explicit_replay = rig.command(&explicit_args)?;
    let explicit_replay: Value = serde_json::from_slice(&explicit_replay.stdout.bytes)?;
    assert_eq!(
        explicit_replay["data"]["run_id"], explicit["data"]["run_id"],
        "{explicit_replay}"
    );
    fs::write(&prompt, "Changed prompt")?;
    let conflict = rig.command(&args)?;
    assert_ne!(code(&conflict), 0);
    let explicit_conflict = rig.command(&explicit_args)?;
    assert_ne!(code(&explicit_conflict), 0);
    assert!(std::path::Path::new(path).exists());
    Ok(())
}

fn owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
    ] {
        assert_eq!(code(&rig.execute(&git, &args, None)?), 0);
    }
    let branch = "fixture/adopted";
    assert_eq!(
        code(&rig.command(&["worktree", "create", branch, "--base", "main", "--json",])?),
        0
    );
    let tree = rig.project.join(".worktrees/fixture-adopted");
    let admin = rig.execute(
        &git,
        &[
            "-C",
            tree.to_str().ok_or("tree")?,
            "rev-parse",
            "--absolute-git-dir",
        ],
        None,
    )?;
    assert_eq!(code(&admin), 0);
    let owner_path =
        PathBuf::from(String::from_utf8(admin.stdout.bytes)?.trim()).join("provenant-owner.json");
    let base_head = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    assert_eq!(code(&base_head), 0);
    let mut owner = json!({
        "attempt_id": null,
        "operation_id": null,
        "branch": branch,
        "base": String::from_utf8(base_head.stdout.bytes)?.trim(),
        "host": provenant_core::HostId::from_bytes([1; 16]),
        "created_at": provenant_core::WallTime::from_micros(1)?,
        "origin": "adopted",
        "label": "human-session"
    });
    let base = owner["base"].clone();
    let prompt = rig.root.join("owned-prompt.md");
    fs::write(&prompt, "Do not change files.")?;
    for same_operation in [false, true] {
        owner["operation_id"] = if same_operation {
            json!("adopted-refused")
        } else {
            Value::Null
        };
        fs::write(&owner_path, serde_json::to_vec(&owner)?)?;
        let before = fs::read(&owner_path)?;
        let out = rig.command(&[
            "dispatch",
            "--operation-id",
            "adopted-refused",
            "--model",
            "codex/gpt-6-luna",
            "--mode",
            "worktree-write",
            "--worktree-branch",
            branch,
            "--base",
            "main",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--json",
        ])?;
        assert_ne!(code(&out), 0);
        let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("adopted or foreign owner"),
            "{result}"
        );
        assert_eq!(fs::read(&owner_path)?, before);
        assert_eq!(owner["base"], base);
        assert_eq!(owner["label"], "human-session");
    }
    // A new checkout made before prompt loading fails must be removed.
    let missing = rig.root.join("missing-prompt.md");
    let out = rig.command(&[
        "dispatch",
        "--operation-id",
        "creation-refused",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "fixture/refused",
        "--base",
        "main",
        "--prompt-file",
        missing.to_str().ok_or("missing")?,
        "--json",
    ])?;
    assert_ne!(code(&out), 0);
    let refused_tree = rig.project.join(".worktrees/fixture-refused");
    if refused_tree.exists() {
        // A native census that cannot prove the tree free must retain its
        // durable lane owner for GC rather than force removal.
        let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("new managed worktree retained for gc"),
            "{result}"
        );
        let admin = rig.execute(
            &git,
            &[
                "-C",
                refused_tree.to_str().ok_or("tree")?,
                "rev-parse",
                "--absolute-git-dir",
            ],
            None,
        )?;
        let owner: Value = serde_json::from_slice(&fs::read(
            PathBuf::from(String::from_utf8(admin.stdout.bytes)?.trim())
                .join("provenant-owner.json"),
        )?)?;
        assert_eq!(owner["origin"], "lane");
        assert_eq!(owner["operation_id"], "creation-refused");
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args
        .first()
        .is_some_and(|s| s == "exec" || s == "run" || s == "--version" || s == "auth" || s == "-p")
    {
        fixture()
    } else {
        provenant_supervisor::fixture::run_libtest(
            &args,
            &[
                "owned_dispatch_retains_failed_attempt_and_replays",
                "owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation",
                "codex_dispatch_observes_rollout",
                "opencode_dispatch_observes_v2_wal_session",
                "opus_writer_dispatch_advises_a_cheaper_route",
                "claude_dispatch_refreshes_registered_account",
                "graph_settlement_advances_writer_and_account_routed_review",
                "graph_settlement_records_a_failed_advance_for_status",
                "opencode_free_pool_cools_second_model",
                "opencode_free_reset_cools_pool_until_reset_and_restores",
                "agy_individual_quota_cools_model_until_reset",
                "agy_denied_command_reaches_result_and_terminal_reason",
                "agy_denial_result_reaches_terminal_reason",
            ],
            |name| {
                let result = match name {
                    "owned_dispatch_retains_failed_attempt_and_replays" => {
                        owned_dispatch_retains_failed_attempt_and_replays()
                    }
                    "owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation" => {
                        owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation()
                    }
                    "codex_dispatch_observes_rollout" => codex_dispatch_observes_rollout(),
                    "opencode_dispatch_observes_v2_wal_session" => {
                        opencode_dispatch_observes_v2_wal_session()
                    }
                    "opus_writer_dispatch_advises_a_cheaper_route" => {
                        opus_writer_dispatch_advises_a_cheaper_route()
                    }
                    "claude_dispatch_refreshes_registered_account" => {
                        claude_dispatch_refreshes_registered_account()
                    }
                    "graph_settlement_advances_writer_and_account_routed_review" => {
                        graph_settlement_advances_writer_and_account_routed_review()
                    }
                    "graph_settlement_records_a_failed_advance_for_status" => {
                        graph_settlement_records_a_failed_advance_for_status()
                    }
                    "opencode_free_pool_cools_second_model" => {
                        opencode_free_pool_cools_second_model()
                    }
                    "opencode_free_reset_cools_pool_until_reset_and_restores" => {
                        opencode_free_reset_cools_pool_until_reset_and_restores()
                    }
                    "agy_individual_quota_cools_model_until_reset" => {
                        agy_individual_quota_cools_model_until_reset()
                    }
                    "agy_denied_command_reaches_result_and_terminal_reason" => {
                        agy_denied_command_reaches_result_and_terminal_reason()
                    }
                    "agy_denial_result_reaches_terminal_reason" => {
                        agy_denial_result_reaches_terminal_reason()
                    }
                    _ => opencode_free_pool_cools_second_model(),
                };
                result.map_err(|e| io::Error::other(e.to_string()))
            },
        )
        .map_err(Into::into)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "dispatch fixture: {error}");
            ExitCode::FAILURE
        }
    }
}
