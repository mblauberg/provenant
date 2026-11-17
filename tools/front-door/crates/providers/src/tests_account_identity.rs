
#[derive(Deserialize)]
struct LiveCapture {
    id: String,
    process_exit: Option<i64>,
    outcome: OutcomeStatus,
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
    assert_eq!(outcome.status(), capture.outcome, "{id}");
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
        assert!(claude::Claude.observe(&mut stream, changed).is_ok());
        let outcome = claude::Claude.finish(stream, EXIT)?;
        assert!(outcome.session().is_none() || outcome.model().is_none());

        let mut strict_policy = Policy::default();
        strict_policy.overlay(
            &[(
                "STREAM_IDENTITY_MISMATCH".into(),
                provenant_records::policy::Strength::Refuse,
            )]
            .into_iter()
            .collect(),
            provenant_records::policy::Source::Project,
        );
        let mut stream = claude::Claude.stream(Some(&continuation))?.with_policy(strict_policy);
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
        if options.argv.get(1).is_some_and(|arg| arg == "-p") {
            assert!(options.wait_for_completion);
            assert!(options.process_group);
            assert!(options.inherited_lease.is_some());
        }
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
fn cursor_status_observes_the_signed_in_account() {
    let output = include_bytes!("../../../tests/fixtures/providers/cursor/status.stdout");
    assert_eq!(
        cursor::status_account(output),
        crate::identity::fingerprint("fixture@example.test")
    );
    assert_eq!(
        crate::host::local_signin(
            provenant_records::LoginReprobe::Cursor,
        &CursorDesktopToken(std::str::from_utf8(output).unwrap(), 0, "/requester/bin"),
            Path::new("/bin/cursor-agent"),
            Path::new("/home/test"),
            Path::new("/home/test"),
            Path::new("/"),
            Some(OsStr::new("/requester/bin")),
        ),
        Some(crate::host::LocalSignin::SignedIn)
    );
    for output in [
        b"Not logged in".as_slice(),
        b"Login successful",
        b"broken",
        b"\xff",
    ] {
        assert!(cursor::status_account(output).is_none());
    }
}

#[test]
fn cursor_cli_only_account_matches_desktop_identity() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("cli-config.json"),
        br#"{"authInfo":{"authId":"auth0|user_fixture","email":"fixture@example.test","userId":123}}"#,
    )
    .unwrap();
    let key = crate::accounts::detect(
        &CursorNoExecutor,
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
            &CursorNoExecutor,
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
struct CursorDesktopToken<'a>(&'a str, i64, &'a str);
impl provenant_os::exec::Executor for CursorDesktopToken<'_> {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        assert!(options.env.is_some_and(|env| {
            env.iter()
                .any(|(key, value)| key == "PATH" && value == self.2)
        }));
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
    write_cursor_database(directory.path(), None);
    std::fs::write(
        directory.path().join("cli-config.json"),
        br#"{"authInfo":{"authId":"auth0|user_fixture"}}"#,
    )
    .unwrap();
    let key = crate::accounts::detect(
        &CursorNoExecutor,
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
fn cursor_account_prefers_desktop_when_cli_is_also_signed_in() {
    let directory = tempfile::tempdir().unwrap();
    let desktop_token = "header.eyJzdWIiOiJhdXRoMHx1c2VyX2ZpeHR1cmUifQ.signature";
    write_cursor_database(directory.path(), Some(desktop_token));
    std::fs::write(
        directory.path().join("cli-config.json"),
        br#"{"authInfo":{"authId":"auth0|different_cli_user"}}"#,
    )
    .unwrap();
    let key = crate::accounts::detect(
        &CursorNoExecutor,
        "cursor",
        Path::new("/bin/cursor-agent"),
        Path::new("/home/test"),
        OsStr::new("test"),
        directory.path(),
        directory.path(),
    )
    .unwrap();
    assert_eq!(Some(key), crate::identity::cursor_account(desktop_token));
}

struct CursorNoExecutor;
impl provenant_os::exec::Executor for CursorNoExecutor {
    fn execute(
        &self,
        _: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        Err(provenant_os::exec::ExecError::Busy)
    }
}

fn write_cursor_database(directory: &Path, token: Option<&str>) {
    let connection = rusqlite::Connection::open(directory.join("state.vscdb")).unwrap();
    if let Some(token) = token {
        connection
            .execute("CREATE TABLE ItemTable (key TEXT, value TEXT)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable (key, value) VALUES ('cursorAuth/accessToken', ?1)",
                [token],
            )
            .unwrap();
    }
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
fn account_detection_preserves_probe_directory_executor_errors() {
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
    for adapter in ["claude", "agy"] {
        let identity = tempfile::tempdir().unwrap();
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
        assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
    }
}

#[test]
fn registration_preflights_probe_directory_before_cursor_identity() {
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
    let identity = tempfile::tempdir().unwrap();
    let error = crate::accounts::detect_for_registration(
        &executor,
        "cursor",
        Path::new("/bin/cursor-agent"),
        Path::new("/home/test"),
        OsStr::new("test"),
        identity.path(),
        state.path(),
    )
    .unwrap_err();
    assert!(matches!(&error, ProviderError::ProbeDirectory(_)));
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
    let attempt = child_store.join("attempts/attempt");
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

/// A registered long-lived lane token wins over the rotating login: the host
/// runs no refresh even when the login is about to expire, so no renewal can
/// revoke a token a live lane holds.
#[test]
fn claude_lane_prefers_the_registered_lane_token_and_never_rotates_the_login() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude-pro");
    let now = 1_000_000_000_u64;
    let expiring = format!(
        r#"{{"claudeAiOauth":{{"accessToken":"fixture-access","expiresAt":{}}}}}"#,
        now + 1000
    );
    let script = FileCredentialScript::new(
        &config,
        KeychainScript {
            items: std::sync::Mutex::new(vec![expiring.into_bytes()].into()),
            statuses: std::sync::Mutex::new(
                vec![br#"{"loggedIn":true,"email":"fixture@example.test"}"#.to_vec(); 2].into(),
            ),
            calls: Default::default(),
        },
    );
    let token = config.join(claude::LANE_TOKEN_FILE);
    std::fs::write(&token, "fixture-lane-token\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let login = claude::Login {
        executable: Path::new("/opt/homebrew/bin/claude"),
        home: temp.path(),
        user: OsStr::new("u"),
        config_dir: &config,
    };
    let key = crate::identity::fingerprint("fixture@example.test").unwrap();
    let account = crate::identity::RegisteredAccount::new("claude", &key, config.clone()).unwrap();
    let lane = claude::refresh_for_account(&script, &login, &account, &|| now, now + 60_000)
        .expect("lane token");
    assert_eq!(lane.access, "fixture-lane-token");
    let calls = script.script.calls.lock().unwrap();
    assert!(
        calls.iter().all(|(argv, _, _)| argv[1] == "auth"),
        "{calls:?}"
    );
}

#[test]
fn claude_lane_token_refuses_permissive_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude-pro");
    std::fs::create_dir_all(&config).unwrap();
    let token = config.join(claude::LANE_TOKEN_FILE);
    std::fs::write(&token, "fixture-lane-token\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    let login = claude::Login {
        executable: Path::new("/opt/homebrew/bin/claude"),
        home: temp.path(),
        user: OsStr::new("u"),
        config_dir: &config,
    };
    let err = claude::lane_token(&login).unwrap_err();
    assert!(matches!(err, ProviderError::CredentialReadFailed(_)));
}

#[test]
fn claude_concurrent_host_refreshes_do_not_reuse_a_rotating_token() {
    use crate::claude::{
        CredentialAuthority, Login, OAUTH_REFRESH_LOCK, refresh_for_account_authorised_using_clock,
        refresh_with_policy_using_clock,
    };
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    // Model the CLI reading a single-use refresh token before exchanging it.
    // Overlapping exchanges revoke that token; a later reader sees the renewal.
    struct RotatingCli {
        script: FileCredentialScript,
        first_exchange: AtomicBool,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl Executor for RotatingCli {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            if options.argv.get(1).is_some_and(|arg| arg == "-p") {
                if !self.first_exchange.swap(false, Ordering::SeqCst) {
                    return Err(ExecError::Busy);
                }
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
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
    let (release, released) = mpsc::channel();
    let (contended, contention_observed) = mpsc::channel();
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
        first_exchange: AtomicBool::new(true),
        entered,
        release: Mutex::new(released),
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
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0).unwrap(),
        provenant_core::MonoTime::from_micros(0),
    );
    let observe_lock = |directory: &provenant_os::fs::Directory| {
        let held = matches!(
            directory.try_lock_existing(
                OsStr::new(OAUTH_REFRESH_LOCK),
                provenant_os::fs::LockMode::Exclusive,
            ),
            Err(provenant_os::fs::FsError::LockTimeout { .. })
        );
        contended.send(held).unwrap();
    };
    std::thread::scope(|scope| {
        // Quota refresh and an authorised dispatch share the same account store.
        let quota = scope.spawn(|| {
            refresh_with_policy_using_clock(
                &executor,
                &login,
                &|| now,
                now + 60_000,
                None,
                60_000,
                false,
                &clock,
                &|_| {},
            )
        });
        started.recv().unwrap();
        let dispatch = scope.spawn(|| {
            refresh_for_account_authorised_using_clock(
                &executor,
                &login,
                &account,
                &|| now,
                now + 60_000,
                &authority,
                &clock,
                &observe_lock,
            )
        });
        let lock_was_held = contention_observed.recv().unwrap();
        release.send(()).unwrap();
        let quota_token = quota.join().unwrap().expect("quota refresh succeeds");
        let dispatch_token = dispatch
            .join()
            .unwrap()
            .expect("concurrent dispatch must not reuse the rotated refresh token");
        assert!(
            lock_was_held,
            "the second caller reached an unheld refresh lock"
        );
        assert_eq!(quota_token.access, "fixture-renewed");
        assert_eq!(dispatch_token.access, "fixture-renewed");
    });
}

const CLAUDE_REFRESH_TEST_NOW_MS: u64 = 1_000_000_000;

fn claude_refresh_item(token: &str, expires_at_ms: u64) -> Vec<u8> {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"fixture-refresh","expiresAt":{expires_at_ms}}}}}"#
    )
    .into_bytes()
}

fn run_claude_refresh(
    items: Vec<Vec<u8>>,
    deadline_ms: u64,
) -> (Result<crate::claude::OauthToken, ProviderError>, Vec<Call>) {
    use crate::claude::{Login, refresh_if_expiring};

    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude");
    let script = FileCredentialScript::new(
        &config,
        KeychainScript {
            items: std::sync::Mutex::new(items.into()),
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
    let result = refresh_if_expiring(
        &script,
        &login,
        &|| CLAUDE_REFRESH_TEST_NOW_MS,
        deadline_ms,
        None,
    );
    (result, script.script.calls.into_inner().unwrap())
}

fn refresh_probe(calls: &[Call]) -> Option<&Call> {
    calls
        .iter()
        .find(|(argv, _, _)| argv.iter().any(|arg| arg == "provenant-refresh-probe"))
}

#[test]
fn claude_refresh_admission_requires_a_sufficient_remaining_budget() {
    let now = CLAUDE_REFRESH_TEST_NOW_MS;
    for deadline in [now, now + 1_000] {
        let (result, calls) =
            run_claude_refresh(vec![claude_refresh_item("fixture-access", now)], deadline);
        assert!(matches!(result, Err(ProviderError::RefreshDeferred)));
        assert!(refresh_probe(&calls).is_none());
    }
}

#[test]
fn claude_refresh_returns_live_token_without_printing_it() {
    let now = CLAUDE_REFRESH_TEST_NOW_MS;
    let (result, calls) = run_claude_refresh(
        vec![claude_refresh_item("fixture-access", now + 3_600_000)],
        now + 3_600_000,
    );
    let token = result.expect("live credential");
    let printed = format!("{token:?}");
    assert!(!printed.contains("fixture-access"));
    assert!(!printed.contains("fixture-refresh"));
    assert!(refresh_probe(&calls).is_none());
}

#[test]
fn claude_refresh_probe_disables_provider_customisations() {
    let now = CLAUDE_REFRESH_TEST_NOW_MS;
    let (result, calls) = run_claude_refresh(
        vec![
            claude_refresh_item("fixture-access", now),
            claude_refresh_item("fixture-renewed", now + 3_600_000),
        ],
        now + 3_600_000,
    );
    let token = result.expect("renewed credential");
    assert_eq!(token.access, "fixture-renewed");
    let (argv, env, cwd) = refresh_probe(&calls).expect("refresh command");
    for flag in [
        "--safe-mode",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
    ] {
        assert!(argv.iter().any(|arg| arg == flag), "missing {flag}");
    }
    let settings = argv
        .windows(2)
        .find(|pair| pair[0] == "--settings")
        .and_then(|pair| serde_json::from_str::<serde_json::Value>(&pair[1]).ok())
        .expect("settings JSON");
    assert_eq!(
        settings
            .get("disableAllHooks")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(
        env.iter()
            .all(|entry| !entry.starts_with("CLAUDE_CONFIG_DIR="))
    );
    assert!(cwd.is_absolute());
}

#[test]
fn claude_refresh_accepts_only_a_usable_returned_token() {
    let now = CLAUDE_REFRESH_TEST_NOW_MS;
    for (remaining, accepted) in [(120_000, true), (1_000, false)] {
        let (result, calls) = run_claude_refresh(
            vec![
                claude_refresh_item("fixture-access", now),
                claude_refresh_item("fixture-renewed", now + remaining),
            ],
            now + 3_600_000,
        );
        assert_eq!(result.is_ok(), accepted);
        if !accepted {
            assert!(matches!(result, Err(ProviderError::RefreshDeferred)));
        }
        assert!(refresh_probe(&calls).is_some());
    }
}

#[test]
fn claude_refresh_replaces_an_expired_credential() {
    let now = CLAUDE_REFRESH_TEST_NOW_MS;
    let (result, calls) = run_claude_refresh(
        vec![
            claude_refresh_item("fixture-access", now),
            claude_refresh_item("fixture-renewed", now + 3_600_000),
        ],
        now + 3_600_000,
    );
    let token = result.expect("renewed credential");
    assert_eq!(token.access, "fixture-renewed");
    assert!(refresh_probe(&calls).is_some());
}

#[test]
fn claude_refresh_defers_when_the_returned_credential_is_still_expired() {
    let now = CLAUDE_REFRESH_TEST_NOW_MS;
    let (result, calls) = run_claude_refresh(
        vec![
            claude_refresh_item("fixture-access", now),
            claude_refresh_item("fixture-access", now),
        ],
        now + 3_600_000,
    );
    assert!(matches!(result, Err(ProviderError::RefreshDeferred)));
    assert!(refresh_probe(&calls).is_some());
}

#[test]
fn claude_refresh_refuses_corrupt_credentials_without_disclosing_them() {
    let now = CLAUDE_REFRESH_TEST_NOW_MS;
    let (result, calls) =
        run_claude_refresh(vec![b"not json: fixture-secret".to_vec()], now + 3_600_000);
    let Err(ProviderError::CredentialReadFailed(reason)) = result else {
        panic!("corrupt credential should be a typed read failure");
    };
    assert!(!reason.contains("fixture-secret"));
    assert!(refresh_probe(&calls).is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn claude_keychain_read_failure_does_not_spawn_refresh_probe() {
    use crate::claude::{Login, refresh_if_expiring};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ProbeKind {
        KeychainRead,
        Other,
    }
    struct FailedKeychainRead(std::sync::Mutex<Vec<ProbeKind>>);

    impl provenant_os::exec::Executor for FailedKeychainRead {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
            let kind = if options
                .argv
                .first()
                .is_some_and(|program| program == "/usr/bin/security")
            {
                ProbeKind::KeychainRead
            } else {
                ProbeKind::Other
            };
            self.0.lock().unwrap().push(kind);
            Ok(provenant_os::exec::ExecOutcome {
                termination: if kind == ProbeKind::KeychainRead {
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
    assert!(calls.contains(&ProbeKind::KeychainRead));
    assert!(!calls.contains(&ProbeKind::Other));
}
