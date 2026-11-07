
#[test]
fn opencode_writer_gets_the_rust_toolchain_in_its_lane_cache()
-> Result<(), Box<dyn std::error::Error>> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("opencode", "configured/model");
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let prepare = |home: &Path, plan: &_| {
        opencode::OpenCode.prepare(PrepareInput {
            home,
            ..fixture_input(&route, plan, &prompt, Path::new("/fixture/repo"), None)
        })
    };
    // Without Rust installed the lane keeps only the system tools.
    let spec = prepare(&home, &plan)?;
    assert!(spec.env.iter().all(|(name, _)| name != "CARGO_HOME"));
    std::fs::create_dir_all(home.join(".cargo/bin"))?;
    std::fs::create_dir_all(home.join(".rustup/toolchains"))?;
    let cache = Path::new("/fixture/home/Library/Caches/provenant-lane-cargo/fixture");
    let spec = prepare(&home, &plan)?;
    let env = |key: &str| {
        spec.env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| Path::new(value).to_path_buf())
    };
    let path = env("PATH").expect("PATH");
    assert!(
        std::env::split_paths(&path).any(|dir| dir == home.join(".cargo/bin")),
        "{path:?}"
    );
    let cache = env("CARGO_TARGET_DIR")
        .and_then(|target| target.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cache.to_path_buf());
    assert_eq!(env("CARGO_HOME"), Some(cache.join("cargo-home")));
    assert_eq!(env("CARGO_TARGET_DIR"), Some(cache.join("target")));
    assert!(cache.ends_with("fixture"), "{cache:?}");
    for root in [
        crate::launch::subtree(&cache),
        crate::launch::subtree(home.join(".cargo/bin")),
        crate::launch::subtree(home.join(".rustup/toolchains")),
    ] {
        assert!(
            spec.profile.spec.tool_exec_roots.contains(&root),
            "{root:?}"
        );
    }
    assert!(
        spec.profile
            .spec
            .runtime_reads
            .contains(&crate::launch::subtree(home.join(".cargo")))
    );
    // Read-only lanes deny bash, so they get no toolchain: only the search
    // binaries OpenCode's grep and glob tools execute.
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let spec = prepare(&home, &plan)?;
    assert_eq!(
        spec.profile.spec.tool_exec_roots,
        [
            crate::launch::exact(
                plan.permissions()
                    .private_state
                    .as_ref()
                    .unwrap()
                    .path
                    .join("cache/opencode/bin/rg")
            ),
            crate::launch::exact("/usr/bin/grep"),
            // The read-only Git launcher the lane puts first on PATH.
            crate::launch::subtree(plan.permissions().scratch.path.join("read-only-git")),
        ]
    );
    assert!(spec.env.iter().all(|(name, _)| name != "CARGO_HOME"));
    Ok(())
}

#[derive(Deserialize)]
struct LiveCapture {
    id: String,
    process_exit: Option<i64>,
    outcome: String,
    answering_model: Option<String>,
    argv: Vec<String>,
}

fn recordings(adapter: &str) -> Vec<LiveCapture> {
    let text = match adapter {
        "codex" => fixture!("codex/recordings.json"),
        "claude" => fixture!("claude/recordings.json"),
        "agy" => fixture!("agy/recordings.json"),
        _ => "[]",
    };
    serde_json::from_str(text).expect("recordings manifest")
}

fn requested_model(capture: &LiveCapture) -> String {
    let flag = capture
        .argv
        .iter()
        .position(|arg| arg == "-m" || arg == "--model")
        .expect("model flag");
    capture.argv[flag + 1].clone()
}

/// Replays one live capture through the real decoder; a resume capture is
/// bound to the continuation its fresh capture produced.
fn replay_live(
    adapter: &dyn Adapter,
    id: &str,
    bytes: &str,
    fresh: Option<&str>,
) -> Result<(), ProviderError> {
    let captures = recordings(adapter.id());
    let capture = captures.iter().find(|c| c.id == id).expect("capture");
    let route = route(adapter.id(), &requested_model(capture));
    let continuation = match fresh {
        Some(fresh) => Some(adapter.continuation(&replay(adapter, fresh, EXIT)?.1, &route, "g")?),
        None => None,
    };
    let mut state = adapter.stream(continuation.as_ref())?;
    for chunk in bytes.as_bytes().chunks(17) {
        adapter.observe(&mut state, chunk)?;
    }
    let exit = ExitObservation {
        code: capture.process_exit,
        timed_out: false,
    };
    let outcome = adapter.finish(state, exit)?;
    assert_eq!(format!("{:?}", outcome.status()), capture.outcome, "{id}");
    // Codex streams carry no model; its recorded answering model comes from the rollout.
    let streamed = capture
        .answering_model
        .as_deref()
        .filter(|_| adapter.id() != "codex");
    assert_eq!(outcome.model(), streamed, "{id}");
    assert!(outcome.session().is_some(), "{id}");
    Ok(())
}

macro_rules! live_replay {
    ($name:ident, $adapter:expr, $id:literal, $file:literal $(, after $fresh:literal)?) => {
        #[test]
        fn $name() -> Result<(), ProviderError> {
            #[allow(unused_mut, unused_assignments)]
            let mut fresh = None;
            $(fresh = Some(fixture!($fresh));)?
            replay_live(&$adapter, $id, fixture!($file), fresh)
        }
    };
}
live_replay!(
    live_codex_read_only_fresh,
    codex::Codex,
    "codex-live-read_only-fresh",
    "codex/live-read_only-fresh.jsonl"
);
live_replay!(live_codex_read_only_resume, codex::Codex, "codex-live-read_only-resume", "codex/live-read_only-resume.jsonl", after "codex/live-read_only-fresh.jsonl");
live_replay!(
    live_codex_worktree_write_fresh,
    codex::Codex,
    "codex-live-worktree_write-fresh",
    "codex/live-worktree_write-fresh.jsonl"
);
live_replay!(live_codex_worktree_write_resume, codex::Codex, "codex-live-worktree_write-resume", "codex/live-worktree_write-resume.jsonl", after "codex/live-worktree_write-fresh.jsonl");
live_replay!(
    live_claude_read_only_fresh,
    claude::Claude,
    "claude-live-read_only-fresh",
    "claude/live-read_only-fresh.jsonl"
);
live_replay!(live_claude_read_only_resume, claude::Claude, "claude-live-read_only-resume", "claude/live-read_only-resume.jsonl", after "claude/live-read_only-fresh.jsonl");
live_replay!(
    live_claude_worktree_write_fresh,
    claude::Claude,
    "claude-live-worktree_write-fresh",
    "claude/live-worktree_write-fresh.jsonl"
);
live_replay!(live_claude_worktree_write_resume, claude::Claude, "claude-live-worktree_write-resume", "claude/live-worktree_write-resume.jsonl", after "claude/live-worktree_write-fresh.jsonl");
live_replay!(
    live_agy_read_only_fresh,
    agy::Agy,
    "agy-live-read_only-fresh",
    "agy/live-read_only-fresh.jsonl"
);
live_replay!(live_agy_read_only_resume, agy::Agy, "agy-live-read_only-resume", "agy/live-read_only-resume.jsonl", after "agy/live-read_only-fresh.jsonl");
live_replay!(
    live_agy_worktree_write_fresh,
    agy::Agy,
    "agy-live-worktree_write-fresh",
    "agy/live-worktree_write-fresh.jsonl"
);
live_replay!(live_agy_worktree_write_resume, agy::Agy, "agy-live-worktree_write-resume", "agy/live-worktree_write-resume.jsonl", after "agy/live-worktree_write-fresh.jsonl");
live_replay!(
    live_claude_auth_required,
    claude::Claude,
    "claude-live-auth-required",
    "claude/live-auth-required.jsonl"
);

#[test]
fn finish_decodes_the_last_unterminated_line() -> Result<(), ProviderError> {
    let (events, outcome) = replay(
        &codex::Codex,
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"final line\"}}",
        EXIT,
    )?;
    assert!(events.is_empty());
    assert!(
        outcome
            .final_events()
            .contains(&ProviderEvent::Text("final line".into()))
    );
    Ok(())
}

#[test]
fn resumed_stream_rejects_session_or_answering_model_changes() -> Result<(), ProviderError> {
    let route = route("claude", "claude-haiku-4-5-20251001");
    let direct = replay(&claude::Claude, fixture!("claude/direct-model.jsonl"), EXIT)?.1;
    let continuation = claude::Claude.continuation(&direct, &route, "grants")?;
    for changed in [
        b"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"changed\",\"model\":\"selector\"}\n".as_slice(),
        b"{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"changed\",\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"session_id\":\"FIXTURE_SESSION_IDENTIFIER_004\",\"parent_tool_use_id\":null}\n{\"type\":\"result\",\"session_id\":\"FIXTURE_SESSION_IDENTIFIER_004\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"answer\"}\n".as_slice(),
    ] {
        let mut stream = claude::Claude.stream(Some(&continuation))?;
        assert!(matches!(claude::Claude.observe(&mut stream, changed), Err(ProviderError::IdentityMismatch)));
        assert_eq!(claude::Claude.finish(stream, EXIT)?.status(), OutcomeStatus::Malformed);
    }
    let mut stream = claude::Claude.stream(Some(&continuation))?;
    claude::Claude.observe(
        &mut stream,
        fixture!("claude/direct-model.jsonl").as_bytes(),
    )?;
    assert_eq!(
        claude::Claude.finish(stream, EXIT)?.status(),
        OutcomeStatus::Success
    );
    assert!(matches!(
        cursor::Cursor.stream(Some(&continuation)),
        Err(ProviderError::IdentityMismatch)
    ));
    Ok(())
}

#[test]
fn structured_http_auth_failure_is_not_generic_error_or_success() -> Result<(), ProviderError> {
    let mut state = claude::Claude.stream(None)?;
    claude::Claude.observe(&mut state, b"{\"type\":\"result\",\"session_id\":\"fixture\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"api_error_status\":401,\"result\":\"OAuth access token has expired\"}\n")?;
    assert_eq!(
        claude::Claude.account(&state)?,
        AccountDetection::AuthRequired
    );
    assert_eq!(
        claude::Claude.finish(state, EXIT)?.status(),
        OutcomeStatus::Refused
    );
    Ok(())
}

#[test]
fn claude_signin_does_not_call_a_locked_or_present_identity_signed_out() {
    use claude::{CredentialPresence as C, SigninState as S, signin_status};
    let negative = br#"{"loggedIn":false,"email":"never-publish@example.invalid"}"#;
    assert_eq!(
        signin_status(negative, Some(1), C::Present).state,
        S::CredentialStorePresent
    );
    assert_eq!(
        signin_status(negative, Some(1), C::Unavailable).state,
        S::KeychainUnavailable
    );
    assert_eq!(
        signin_status(negative, Some(1), C::Absent).state,
        S::SignedOut
    );
    assert_eq!(
        signin_status(negative, Some(1), C::Unknown).state,
        S::Unknown
    );
    assert_eq!(
        signin_status(br#"{"loggedIn":true}"#, Some(0), C::Unknown).state,
        S::SignedIn
    );
    assert_eq!(
        signin_status(b"broken", Some(1), C::Absent).state,
        S::Unknown
    );
    let report = serde_json::to_string(&signin_status(negative, Some(1), C::Present)).unwrap();
    assert!(!report.contains("never-publish"));
}

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// One recorded call: argv, environment and working directory.
type Call = (Vec<String>, Vec<String>, std::path::PathBuf);

/// Answers keychain reads from a script and records every call.
struct KeychainScript {
    items: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
    statuses: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
    calls: std::sync::Mutex<Vec<Call>>,
}

impl provenant_os::exec::Executor for KeychainScript {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        let text = |items: &[OsString]| {
            items
                .iter()
                .map(|item| item.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let argv = text(options.argv);
        let env = options
            .env
            .unwrap_or_default()
            .iter()
            .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
            .collect();
        let stdout = if argv[0] == "/usr/bin/security" {
            self.items.lock().unwrap().pop_front().unwrap()
        } else if argv.get(1).is_some_and(|a| a == "auth") {
            self.statuses.lock().unwrap().pop_front().unwrap()
        } else {
            vec![]
        };
        self.calls
            .lock()
            .unwrap()
            .push((argv, env, options.cwd.to_owned()));
        Ok(provenant_os::exec::ExecOutcome {
            termination: provenant_os::exec::Termination::Exit(0),
            stdout: provenant_os::exec::CapturedStream {
                bytes: stdout,
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        })
    }
}

/// File-backed fake credentials; only the fake CLI refresh rewrites storage.
struct FileCredentialScript {
    script: KeychainScript,
    path: std::path::PathBuf,
}
impl FileCredentialScript {
    fn new(config: &Path, script: KeychainScript) -> Self {
        std::fs::create_dir_all(config).unwrap();
        let path = config.join(".credentials.json");
        if let Some(item) = script.items.lock().unwrap().pop_front() {
            std::fs::write(&path, item).unwrap();
        }
        Self { script, path }
    }
}
impl provenant_os::exec::Executor for FileCredentialScript {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        assert_ne!(options.argv[0], OsStr::new("/usr/bin/security"));
        if options.argv.get(1).is_some_and(|arg| arg == "-p")
            && let Some(item) = self.script.items.lock().unwrap().pop_front()
        {
            std::fs::write(&self.path, item).unwrap();
        }
        self.script.execute(options)
    }
}

#[test]
fn cursor_cli_only_account_matches_desktop_identity() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct NoDesktop;
    impl Executor for NoDesktop {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            Err(ExecError::Busy)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("cli-config.json"),
        br#"{"authInfo":{"authId":"auth0|user_fixture","email":"fixture@example.test","userId":123}}"#,
    )
    .unwrap();
    let key = crate::accounts::detect(
        &NoDesktop,
        "cursor",
        Path::new("/bin/cursor-agent"),
        Path::new("/home/test"),
        OsStr::new("test"),
        directory.path(),
        directory.path(),
    )
    .unwrap();
    assert_eq!(
        Some(key),
        crate::identity::cursor_account("header.eyJzdWIiOiJhdXRoMHx1c2VyX2ZpeHR1cmUifQ.signature")
    );
    std::fs::remove_file(directory.path().join("cli-config.json")).unwrap();
    for config in [
        None,
        Some("{broken"),
        Some(r#"{"authInfo":{"email":"fixture@example.test"}}"#),
        Some(r#"{"authInfo":{"authId":""}}"#),
        Some(r#"{"authInfo":{"authId":123}}"#),
        Some(r#"{"authInfo":{"authId":"bad\nsubject"}}"#),
    ] {
        if let Some(config) = config {
            std::fs::write(directory.path().join("cli-config.json"), config).unwrap();
        }
        let result = crate::accounts::detect(
            &NoDesktop,
            "cursor",
            Path::new("/bin/cursor-agent"),
            Path::new("/home/test"),
            OsStr::new("test"),
            directory.path(),
            directory.path(),
        );
        if config.is_none() {
            assert!(matches!(result, Err(ProviderError::CredentialNotFound)));
        } else {
            assert!(matches!(result, Err(ProviderError::UnknownIdentity)));
        }
    }
}

/// Desktop token output at the sqlite executor boundary.
struct CursorDesktopToken<'a>(&'a str, i64);
impl provenant_os::exec::Executor for CursorDesktopToken<'_> {
    fn execute(
        &self,
        _: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        Ok(provenant_os::exec::ExecOutcome {
            termination: provenant_os::exec::Termination::Exit(self.1),
            stdout: provenant_os::exec::CapturedStream {
                bytes: self.0.as_bytes().to_vec(),
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        })
    }
}

#[test]
fn cursor_account_falls_back_to_cli_when_desktop_has_no_identity() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("state.vscdb"), b"").unwrap();
    std::fs::write(
        directory.path().join("cli-config.json"),
        br#"{"authInfo":{"authId":"auth0|user_fixture"}}"#,
    )
    .unwrap();
    // A logged-out database returns no rows; an empty database has no ItemTable.
    for status in [0, 1] {
        let key = crate::accounts::detect(
            &CursorDesktopToken("", status),
            "cursor",
            Path::new("/bin/cursor-agent"),
            Path::new("/home/test"),
            OsStr::new("test"),
            directory.path(),
            directory.path(),
        )
        .unwrap();
        assert_eq!(
            Some(key),
            crate::identity::cursor_account(
                "header.eyJzdWIiOiJhdXRoMHx1c2VyX2ZpeHR1cmUifQ.signature"
            )
        );
    }
}

#[test]
fn cursor_account_prefers_desktop_when_cli_is_also_signed_in() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("state.vscdb"), b"").unwrap();
    std::fs::write(
        directory.path().join("cli-config.json"),
        br#"{"authInfo":{"authId":"auth0|different_cli_user"}}"#,
    )
    .unwrap();
    let key = crate::accounts::detect(
        &CursorDesktopToken(
            "header.eyJzdWIiOiJhdXRoMHx1c2VyX2ZpeHR1cmUifQ.signature\n",
            0,
        ),
        "cursor",
        Path::new("/bin/cursor-agent"),
        Path::new("/home/test"),
        OsStr::new("test"),
        directory.path(),
        directory.path(),
    )
    .unwrap();
    assert_eq!(
        Some(key),
        crate::identity::cursor_account("header.eyJzdWIiOiJhdXRoMHx1c2VyX2ZpeHR1cmUifQ.signature")
    );
}

#[test]
fn agy_account_probe_logs_under_the_host_probe_root() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Probe<'a>(&'a Path);
    impl Executor for Probe<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let log = options
                .argv
                .windows(2)
                .find(|pair| pair[0] == "--log-file")
                .map(|pair| Path::new(&pair[1]))
                .unwrap();
            let directory = log.parent().unwrap();
            assert_eq!(directory.parent(), Some(self.0));
            assert!(
                directory
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("probe-")
            );
            std::fs::write(
                log,
                b"OAuth: authenticated successfully as fixture@example.test\n",
            )
            .unwrap();
            Ok(ExecOutcome {
                termination: provenant_os::exec::Termination::Exit(0),
                stdout: provenant_os::exec::CapturedStream {
                    bytes: br#"{"status":"SUCCESS"}"#.to_vec(),
                    ..Default::default()
                },
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    let key = crate::accounts::detect(
        &Probe(&host_root),
        "agy",
        Path::new("/bin/agy"),
        Path::new("/home/test"),
        OsStr::new("test"),
        Path::new("/home/test"),
        &host_root,
    )
    .unwrap();
    assert!(key.starts_with("agy-google-"));
    // Only the non-secret identity cache survives; the log and private probe
    // directory are still removed before detection returns.
    let entries: Vec<_> = std::fs::read_dir(&host_root)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0]
            .file_name()
            .to_string_lossy()
            .starts_with("agy-identity-")
    );
    assert_eq!(
        crate::agy::cached_identity(
            &host_root,
            Path::new("/home/test"),
            provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new()),
        )
        .as_deref(),
        Some(key.as_str())
    );
    // A second dispatch identity lookup consumes that unexpired observation.
    assert_eq!(
        crate::accounts::detect(
            &provenant_os::exec::SystemExecutor::new(1).unwrap(),
            "agy",
            Path::new("/does-not-exist/agy"),
            Path::new("/home/test"),
            OsStr::new("test"),
            Path::new("/home/test"),
            &host_root,
        )
        .unwrap(),
        key
    );
}

#[test]
fn agy_account_probe_removes_its_log_directory_when_the_command_fails() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Fail;
    impl Executor for Fail {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            Err(ExecError::Busy)
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    let result = crate::accounts::detect(
        &Fail,
        "agy",
        Path::new("/bin/agy"),
        Path::new("/home/test"),
        OsStr::new("test"),
        Path::new("/home/test"),
        &host_root,
    );
    assert!(matches!(result, Err(ProviderError::UnknownIdentity)));
    assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
}

#[test]
fn agy_account_probe_reports_a_missing_probe_root_as_a_probe_failure() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Count(AtomicUsize);
    impl Executor for Count {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ExecError::Busy)
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    let missing = host_root.join("missing");
    let executor = Count(AtomicUsize::new(0));
    let error = crate::accounts::detect(
        &executor,
        "agy",
        Path::new("/bin/agy"),
        Path::new("/home/test"),
        OsStr::new("test"),
        Path::new("/home/test"),
        &missing,
    )
    .unwrap_err();
    assert!(matches!(&error, ProviderError::ProbeDirectory(_)));
    assert!(error.to_string().contains(missing.to_str().unwrap()));
    assert_eq!(executor.0.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
}

/// An isolated agy account directory holds its own keychain as a regular file,
/// which the probe requires before it runs.
fn write_agy_keychain(directory: &Path) {
    let keychain = directory.join(KEYCHAIN_FILE);
    std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
    std::fs::write(keychain, b"").unwrap();
}

#[test]
fn registration_reports_probe_directory_failures_from_the_executor() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Fail;
    impl Executor for Fail {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            Err(ExecError::Io {
                operation: crate::host::PROBE_DIR_OPERATION,
                source: std::io::Error::other("fixture probe failure"),
            })
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    for adapter in ["claude", "cursor", "agy"] {
        // Cursor reads its CLI login without the executor unless the desktop
        // database exists, so give it one to reach the probe.
        let identity = tempfile::tempdir().unwrap();
        std::fs::write(identity.path().join("state.vscdb"), b"").unwrap();
        // An isolated agy account must hold its own keychain to reach the probe.
        write_agy_keychain(identity.path());
        let directory = if matches!(adapter, "cursor" | "agy") {
            identity.path()
        } else {
            Path::new("/home/test/identity")
        };
        let error = crate::accounts::detect_with_org(
            &Fail,
            adapter,
            Path::new("/bin/provider"),
            Path::new("/home/test"),
            OsStr::new("test"),
            directory,
            &host_root,
        )
        .unwrap_err();
        assert!(
            matches!(&error, ProviderError::ProbeDirectory(_)),
            "{adapter}: {error}"
        );
        assert!(error.to_string().contains("fixture probe failure"));
        assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
    }
}

#[test]
fn registration_names_probe_directory_failures() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};
    use std::os::unix::fs::PermissionsExt;

    struct Never;
    impl Executor for Never {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            panic!("the provider must not run without a private probe directory");
        }
    }
    let state = tempfile::tempdir().unwrap();
    let executor = crate::host::HostProbeExecutor::new(&Never, state.path()).unwrap();
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    for adapter in ["claude", "cursor", "agy"] {
        let identity = tempfile::tempdir().unwrap();
        std::fs::write(identity.path().join("state.vscdb"), b"").unwrap();
        // An isolated agy account must hold its own keychain to reach the probe.
        write_agy_keychain(identity.path());
        let directory = if matches!(adapter, "cursor" | "agy") {
            identity.path()
        } else {
            Path::new("/home/test/identity")
        };
        let error = crate::accounts::detect_with_org(
            &executor,
            adapter,
            Path::new("/bin/provider"),
            Path::new("/home/test"),
            OsStr::new("test"),
            directory,
            state.path(),
        )
        .unwrap_err();
        assert!(matches!(&error, ProviderError::ProbeDirectory(_)));
        assert!(error.to_string().contains("private directory"), "{error}");
    }
}

#[test]
fn registration_keeps_other_executor_failures_as_unknown_identity() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Fail {
        busy: bool,
    }
    impl Executor for Fail {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            Err(if self.busy {
                ExecError::Busy
            } else {
                ExecError::Io {
                    operation: "spawn",
                    source: std::io::Error::other("provider unavailable"),
                }
            })
        }
    }
    let state = tempfile::tempdir().unwrap();
    let agy = tempfile::tempdir().unwrap();
    let cursor = tempfile::tempdir().unwrap();
    std::fs::write(cursor.path().join("state.vscdb"), b"fixture").unwrap();
    write_agy_keychain(agy.path());
    for adapter in ["claude", "cursor", "agy"] {
        for busy in [false, true] {
            let result = crate::accounts::detect_with_org(
                &Fail { busy },
                adapter,
                Path::new("/bin/provider"),
                Path::new("/home/test"),
                OsStr::new("test"),
                if adapter == "agy" {
                    agy.path()
                } else if adapter == "cursor" {
                    cursor.path()
                } else {
                    Path::new("/home/test/identity")
                },
                state.path(),
            );
            assert!(matches!(result, Err(ProviderError::UnknownIdentity)));
            assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);
        }
    }
    // The user's own login has no isolated keychain to guard, so the guard
    // passes the executor's failure through unchanged.
    let result = crate::accounts::detect_with_org(
        &Fail { busy: false },
        "agy",
        Path::new("/bin/provider"),
        Path::new("/home/test"),
        OsStr::new("test"),
        Path::new("/home/test"),
        state.path(),
    );
    assert!(matches!(result, Err(ProviderError::UnknownIdentity)));
}

#[test]
fn host_preparation_never_uses_prepopulated_child_scratch_or_temporary_directories() {
    use crate::host::HostProbeExecutor;
    use provenant_os::exec::{ExecOptions, Executor};
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    // Check while the command is running, before the private cwd is removed.
    struct CheckCwd<'a> {
        host_root: &'a Path,
        script: &'a FileCredentialScript,
    }
    impl Executor for CheckCwd<'_> {
        fn execute(
            &self,
            options: ExecOptions<'_>,
        ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
            assert_eq!(options.cwd.parent(), Some(self.host_root));
            let metadata = std::fs::metadata(options.cwd).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::read_dir(options.cwd).unwrap().count(), 0);
            // A command may write settings. The next command must not see them.
            std::fs::create_dir(options.cwd.join(".claude")).unwrap();
            std::fs::write(options.cwd.join(".claude/settings.json"), b"{}").unwrap();
            self.script.execute(options)
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let host_root = temp.path().canonicalize().unwrap();
    let child_store = host_root.join("children/parent-attempt");
    let attempt = child_store.join("runs/run/attempts/attempt");
    let scratch = attempt.join("scratch");
    let candidate = attempt.join("tmp/candidate-0");
    for directory in [&scratch, &candidate] {
        std::fs::create_dir_all(directory.join(".claude")).unwrap();
        std::fs::write(
            directory.join(".claude/settings.json"),
            br#"{"hooks":{"SessionStart":[{"command":"child-controlled"}]}}"#,
        )
        .unwrap();
    }
    let now = 1_000_000_000_u64;
    let item = |expires| {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"fixture-access","expiresAt":{expires}}}}}"#)
            .into_bytes()
    };
    let mut seen = std::collections::HashSet::new();
    for supplied_cwd in [&scratch, &candidate] {
        let script = KeychainScript {
            items: std::sync::Mutex::new(vec![item(now + 1000), item(now + 3_600_000)].into()),
            statuses: std::sync::Mutex::new(
                vec![br#"{"loggedIn":true,"email":"fixture@example.test"}"#.to_vec(); 3].into(),
            ),
            calls: Default::default(),
        };
        let config = host_root.join(".claude-pro");
        let script = FileCredentialScript::new(&config, script);
        let checked = CheckCwd {
            host_root: &host_root,
            script: &script,
        };
        let executor = HostProbeExecutor::new(&checked, &host_root).unwrap();
        let login = claude::Login {
            executable: Path::new("/opt/homebrew/bin/claude"),
            home: &host_root,
            user: OsStr::new("u"),
            config_dir: &config,
        };
        let key = crate::accounts::detect(
            &executor,
            "claude",
            login.executable,
            login.home,
            login.user,
            login.config_dir,
            &host_root,
        )
        .unwrap();
        let account =
            crate::identity::RegisteredAccount::new("claude", &key, login.config_dir.to_owned())
                .unwrap();
        claude::refresh_for_account(&executor, &login, &account, &|| now, now + 60_000).unwrap();
        let argv = [login.executable.into(), "--version".into()];
        executor
            .execute(ExecOptions::new(
                &argv,
                supplied_cwd,
                Duration::from_secs(4),
            ))
            .unwrap();
        let calls = script.script.calls.lock().unwrap();
        assert_eq!(calls.len(), 5, "status, refresh and version commands");
        for (_, _, cwd) in calls.iter() {
            assert!(!cwd.starts_with(&child_store));
            assert!(!cwd.exists(), "host cwd must be removed after each command");
            assert!(seen.insert(cwd.clone()), "host cwds must never be reused");
        }
        assert!(supplied_cwd.join(".claude/settings.json").is_file());
    }
}

#[test]
fn claude_fresh_token_does_not_create_refresh_lock() {
    use crate::claude::{Login, refresh_if_expiring};

    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude");
    let now = 1_000_000_000_u64;
    let item = format!(
        r#"{{"claudeAiOauth":{{"accessToken":"fixture-access","refreshToken":"fixture-refresh","expiresAt":{}}}}}"#,
        now + 3_600_000
    );
    let script = FileCredentialScript::new(
        &config,
        KeychainScript {
            items: std::sync::Mutex::new(vec![item.into_bytes()].into()),
            statuses: Default::default(),
            calls: Default::default(),
        },
    );
    let login = Login {
        executable: Path::new("/opt/homebrew/bin/claude"),
        home: temp.path(),
        user: OsStr::new("u"),
        config_dir: &config,
    };
    refresh_if_expiring(&script, &login, &|| now, now + 60_000, None).expect("fresh token");
    assert!(!config.join("provenant-oauth-refresh.lock").exists());
}

#[test]
fn claude_concurrent_host_refreshes_do_not_reuse_a_rotating_token() {
    use crate::claude::{
        CredentialAuthority, Login, refresh_for_account_authorised, refresh_if_expiring,
    };
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};
    use std::sync::{Condvar, Mutex, mpsc};
    use std::time::Duration;

    // Model the CLI reading a single-use refresh token before exchanging it.
    // Overlapping exchanges revoke that token; a later reader sees the renewal.
    struct RotatingCli {
        script: FileCredentialScript,
        exchanges: Mutex<usize>,
        overlap: Condvar,
        entered: mpsc::Sender<()>,
    }
    impl Executor for RotatingCli {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            if options.argv.get(1).is_some_and(|arg| arg == "-p") {
                let mut exchanges = self.exchanges.lock().unwrap();
                *exchanges += 1;
                self.entered.send(()).unwrap();
                if *exchanges > 1 {
                    self.overlap.notify_all();
                    return Err(ExecError::Busy); // OAuth token revoked.
                }
                let _wait = self
                    .overlap
                    .wait_timeout_while(exchanges, Duration::from_secs(1), |count| *count == 1)
                    .unwrap();
            }
            self.script.execute(options)
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude");
    let now = 1_000_000_000_u64;
    let item = |access, refresh, expires| {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{expires}}}}}"#).into_bytes()
    };
    let (entered, started) = mpsc::channel();
    let executor = RotatingCli {
        script: FileCredentialScript::new(
            &config,
            KeychainScript {
                items: Mutex::new(
                    vec![
                        item("fixture-old", "fixture-single-use", now + 1000),
                        item("fixture-renewed", "fixture-rotated", now + 3_600_000),
                    ]
                    .into(),
                ),
                statuses: Mutex::new(
                    vec![br#"{"loggedIn":true,"email":"fixture@example.test"}"#.to_vec(); 2].into(),
                ),
                calls: Default::default(),
            },
        ),
        exchanges: Mutex::new(0),
        overlap: Condvar::new(),
        entered,
    };
    let login = Login {
        executable: Path::new("/opt/homebrew/bin/claude"),
        home: temp.path(),
        user: OsStr::new("u"),
        config_dir: &config,
    };
    let key = crate::identity::fingerprint("fixture@example.test").unwrap();
    let account = crate::identity::RegisteredAccount::new("claude", &key, config.clone()).unwrap();
    let authority =
        CredentialAuthority::for_dispatch("fixture".into(), &login, now + 60_000).unwrap();
    std::thread::scope(|scope| {
        // Quota refresh and an authorised dispatch share the same account store.
        let quota =
            scope.spawn(|| refresh_if_expiring(&executor, &login, &|| now, now + 60_000, None));
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let dispatch = refresh_for_account_authorised(
            &executor,
            &login,
            &account,
            &|| now,
            now + 60_000,
            &authority,
        );
        let quota_token = quota.join().unwrap().expect("quota refresh succeeds");
        let dispatch_token =
            dispatch.expect("concurrent dispatch must not reuse the rotated refresh token");
        for output in [format!("{quota_token:?}"), format!("{dispatch_token:?}")] {
            assert!(!output.contains("fixture-old"));
            assert!(!output.contains("fixture-renewed"));
            assert!(!output.contains("fixture-single-use"));
            assert!(!output.contains("fixture-rotated"));
        }
    });
    assert_eq!(
        *executor.exchanges.lock().unwrap(),
        1,
        "only one exchange may consume the refresh token"
    );
}

#[test]
fn claude_login_refreshes_only_near_expiry_with_customisations_off_and_never_prints_the_token() {
    use crate::claude::{Login, refresh_if_expiring};
    let now = 1_000_000_000_u64;
    let item = |token: &str, expires: u64| {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"fixture-refresh","expiresAt":{expires}}}}}"#
        )
        .into_bytes()
    };
    let run_by = |items: Vec<Vec<u8>>, deadline: u64| {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".claude");
        let script = FileCredentialScript::new(
            &config,
            KeychainScript {
                items: std::sync::Mutex::new(items.into()),
                statuses: Default::default(),
                calls: std::sync::Mutex::new(vec![]),
            },
        );
        let login = Login {
            executable: Path::new("/opt/homebrew/bin/claude"),
            home: temp.path(),
            user: OsStr::new("u"),
            config_dir: &config,
        };
        let result = refresh_if_expiring(&script, &login, &|| now, deadline, None);
        (result, script.script.calls.into_inner().unwrap())
    };
    let hour = 3_600_000;
    let run = |items: Vec<Vec<u8>>| run_by(items, now + hour);

    // A spent deadline runs nothing.
    let (token, calls) = run_by(vec![item("fixture-access", now + 1000)], now);
    assert!(token.is_err());
    assert!(calls.is_empty());

    // A live token is used as read: no refresh, and Debug never shows it.
    let (token, calls) = run(vec![item("fixture-access", now + hour)]);
    let token = token.expect("live token");
    let printed = format!("{token:?}");
    assert!(!printed.contains("fixture-access") && !printed.contains("fixture-refresh"));
    assert!(calls.is_empty());

    // Inside the window the CLI refreshes once, in the empty scratch directory
    // with hooks, MCP servers, slash commands and session files off.
    let expiring = item("fixture-access", now + 14 * 60 * 1000);
    let (token, calls) = run(vec![expiring.clone(), item("fixture-renewed", now + hour)]);
    token.expect("refreshed token");
    let (argv, env, cwd) = &calls[0];
    assert_eq!(argv[0], "/opt/homebrew/bin/claude");
    for flag in [
        "--safe-mode",
        "--strict-mcp-config",
        r#"{"disableAllHooks":true}"#,
        "--disable-slash-commands",
        "--no-session-persistence",
    ] {
        assert!(argv.iter().any(|a| a == flag), "{flag}");
    }
    assert_eq!(cwd, Path::new("/"));
    assert!(!env.iter().any(|v| v.starts_with("CLAUDE_CONFIG_DIR=")));
    assert_eq!(calls.len(), 1);

    // The CLI may keep a token inside the window; one with a minute left is
    // accepted, one about to expire is refused.
    let (token, _) = run(vec![expiring.clone(), expiring.clone()]);
    token.expect("still usable");
    let (token, _) = run(vec![expiring, item("fixture-access", now + 30_000)]);
    assert!(matches!(token, Err(ProviderError::CredentialUnavailable)));

    // An expired token is refreshed; it stays expired only if the CLI fails.
    let (token, calls) = run(vec![
        item("fixture-access", now),
        item("fixture-renewed", now + hour),
    ]);
    token.expect("expired token refreshed");
    assert_eq!(calls.len(), 1, "expired storage refreshes once");
    let (token, calls) = run(vec![
        item("fixture-access", now),
        item("fixture-access", now),
    ]);
    assert!(matches!(token, Err(ProviderError::CredentialExpired)));
    assert_eq!(calls.len(), 1);

    // Corrupt stored JSON cannot be repaired by refreshing.
    let (token, calls) = run(vec![b"not json: fixture-secret".to_vec()]);
    assert!(
        matches!(&token, Err(ProviderError::CredentialReadFailed(reason)) if reason == "stored credential JSON is invalid")
    );
    assert!(!token.unwrap_err().to_string().contains("fixture-secret"));
    assert!(
        calls.is_empty(),
        "corrupt file credentials must not refresh"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn claude_keychain_read_failure_does_not_spawn_refresh_probe() {
    use crate::claude::{Login, refresh_if_expiring};

    struct FailedKeychainRead(std::sync::Mutex<Vec<Call>>);

    impl provenant_os::exec::Executor for FailedKeychainRead {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
            let argv: Vec<String> = options
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.0.lock().unwrap().push((
                argv.clone(),
                options
                    .env
                    .unwrap_or_default()
                    .iter()
                    .map(|(key, value)| {
                        format!("{}={}", key.to_string_lossy(), value.to_string_lossy())
                    })
                    .collect(),
                options.cwd.into(),
            ));
            Ok(provenant_os::exec::ExecOutcome {
                termination: if argv.first().is_some_and(|arg| arg == "/usr/bin/security") {
                    provenant_os::exec::Termination::Exit(1)
                } else {
                    provenant_os::exec::Termination::Exit(0)
                },
                stdout: Default::default(),
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }

    let executor = FailedKeychainRead(Default::default());
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude-work");
    std::fs::create_dir(&config).unwrap();
    let login = Login {
        executable: Path::new("/opt/homebrew/bin/claude"),
        home: temp.path(),
        user: OsStr::new("u"),
        config_dir: &config,
    };

    let result = refresh_if_expiring(&executor, &login, &|| 1_000_000, 2_000_000, None);

    assert!(matches!(
        result,
        Err(ProviderError::CredentialReadFailed(_))
    ));
    let calls = executor.0.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "a failed keychain read must not start a refresh"
    );
    assert_eq!(calls[0].0[0], "/usr/bin/security");
    use std::os::unix::ffi::OsStrExt;
    let suffix = crate::claude::keychain_suffix(config.as_os_str().as_bytes());
    assert_eq!(calls[0].0[3], format!("Claude Code-credentials-{suffix}"));
    assert!(
        calls[0]
            .1
            .contains(&format!("CLAUDE_CONFIG_DIR={}", config.display()))
    );
}
