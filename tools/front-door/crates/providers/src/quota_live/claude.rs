use super::*;
use provenant_os::fs::{Directory, PublishMode};
use std::os::unix::ffi::OsStrExt;
use std::{collections::BTreeMap, ffi::OsStr};

// No Debug/Serialize: an identity can carry an access token only in memory.
struct Identity {
    token: Option<String>,
    expires: Option<i64>,
    missing: bool,
    fingerprint: String,
}
fn usable(identity: &Identity, at: i64) -> bool {
    identity.token.is_some() && identity.expires.is_some_and(|e| e > at.saturating_add(60))
}
// An expired access token alone does not prove that refresh was rejected.
fn no_usable_identity(fixed: &Identity) -> ProbeFailure {
    if fixed.missing {
        ProbeFailure::Auth
    } else {
        ProbeFailure::Network
    }
}
fn is_fingerprint(key: &str) -> bool {
    key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit())
}
fn expected_fingerprint<'a>(
    expected: &str,
    live: Option<&'a str>,
) -> Result<&'a str, ProbeFailure> {
    let live = live.ok_or(ProbeFailure::Network)?;
    // Older hand-keyed registrations do not encode an identity. Adopt the
    // observed fingerprint for this probe and bracket every credential read.
    let fingerprint_key = is_fingerprint(expected);
    if fingerprint_key && live != expected {
        return Err(ProbeFailure::AccountMismatch);
    }
    Ok(live)
}
fn claude_executable() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("claude"))
        .filter(|path| provenant_os::sys::is_executable(path))
        .find_map(|path| path.canonicalize().ok())
}
// Missing credentials may leave an identity without a token. Transient errors
// must reach the quota report rather than masquerading as missing credentials.
fn optional_probe<T>(result: Result<T, ProbeFailure>) -> Result<Option<T>, ProbeFailure> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ProbeFailure::Auth) => Ok(None),
        Err(error) => Err(error),
    }
}

fn keychain_output(out: ExecOutcome) -> Result<Vec<u8>, ProbeFailure> {
    if !clean(&out) {
        return Err(ProbeFailure::Network);
    }
    match out.termination {
        Termination::Exit(0) => Ok(out.stdout.bytes),
        Termination::Exit(44) => Err(ProbeFailure::Auth),
        _ => Err(ProbeFailure::Network),
    }
}

fn status_fingerprint(result: Result<ExecOutcome, ProbeFailure>) -> Result<String, ProbeFailure> {
    let out = result.map_err(|error| match error {
        ProbeFailure::ProbeDirectory => error,
        _ => ProbeFailure::Network,
    })?;
    if !clean(&out) || !matches!(out.termination, Termination::Exit(_)) {
        return Err(ProbeFailure::Network);
    }
    let state: Value =
        serde_json::from_slice(&out.stdout.bytes).map_err(|_| ProbeFailure::Malformed)?;
    if state.get("loggedIn").and_then(Value::as_bool) == Some(false) {
        return Err(ProbeFailure::Auth);
    }
    if state
        .get("email")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(ProbeFailure::Network);
    }
    if out.termination != Termination::Exit(0) {
        return Err(ProbeFailure::Network);
    }
    state
        .get("email")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| fingerprint(s, 16))
        .filter(|s| is_fingerprint(s))
        .ok_or(ProbeFailure::Network)
}

fn parse_credentials(raw: &[u8]) -> Result<Value, ProbeFailure> {
    if raw.is_empty() {
        return Err(ProbeFailure::Auth);
    }
    let value: Value = serde_json::from_slice(raw).map_err(|_| ProbeFailure::Malformed)?;
    if value.get("claudeAiOauth").is_none() {
        return Err(ProbeFailure::Auth);
    }
    Ok(value)
}

fn credential_open_error(error: std::io::Error) -> ProbeFailure {
    if error.kind() == std::io::ErrorKind::NotFound {
        ProbeFailure::Auth
    } else {
        ProbeFailure::Network
    }
}

fn refresh(
    executor: &dyn Executor,
    login: &crate::claude::Login<'_>,
    now_ms: &dyn Fn() -> u64,
    deadline_ms: u64,
) -> Result<(), ProbeFailure> {
    use std::sync::atomic::{AtomicBool, Ordering};

    // Preserve directory failures, which the quota report must distinguish
    // from credential failures even when the shared refresh returns early.
    struct Watch<'a> {
        executor: &'a dyn Executor,
        directory_failed: AtomicBool,
        rejected: AtomicBool,
    }
    impl Executor for Watch<'_> {
        fn execute(&self, mut options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let probe = options.argv.get(1).is_some_and(|arg| arg == "-p");
            if probe {
                options.stderr = StreamPolicy::Capture { limit: 16 * 1024 };
            }
            let mut result = self.executor.execute(options);
            if let Ok(out) = &mut result
                && probe
            {
                if clean(out) && !matches!(out.termination, Termination::Exit(0)) {
                    let text = String::from_utf8_lossy(&out.stderr.bytes).to_ascii_lowercase();
                    if provenant_records::Harness::Claude
                        .spec()
                        .auth_markers
                        .iter()
                        .any(|marker| text.contains(marker))
                    {
                        self.rejected.store(true, Ordering::Relaxed);
                    }
                }
                out.stderr.bytes.clear();
            }
            if matches!(
                &result,
                Err(ExecError::Io {
                    operation: PROBE_DIR_OPERATION,
                    ..
                })
            ) {
                self.directory_failed.store(true, Ordering::Relaxed);
            }
            result
        }
    }
    let watched = Watch {
        executor,
        directory_failed: AtomicBool::new(false),
        rejected: AtomicBool::new(false),
    };
    let result = crate::claude::refresh_if_expiring(&watched, login, now_ms, deadline_ms, None);
    if watched.directory_failed.load(Ordering::Relaxed) {
        Err(ProbeFailure::ProbeDirectory)
    } else {
        result.map(|_| ()).map_err(|error| match error {
            crate::ProviderError::CredentialExpired => ProbeFailure::LoginExpired,
            crate::ProviderError::CredentialNotFound => ProbeFailure::Auth,
            _ => ProbeFailure::Network,
        })
    }
}

impl LiveQuota {
    fn pin_first_fingerprint(
        &self,
        key: &str,
        live: &str,
        confirmed: bool,
    ) -> Result<(), ProbeFailure> {
        if is_fingerprint(key) {
            return expected_fingerprint(key, Some(live)).map(|_| ());
        }
        // Quota ticks hold the quota lock. Check an existing binding before
        // opening the credential; save a first binding after both status probes agree.
        let dir = Directory::open_anchor(&self.state).map_err(|_| ProbeFailure::Network)?;
        let mut pins: BTreeMap<String, String> =
            match dir.open_read(OsStr::new("quota-claude-identities.json")) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(LIMIT as u64 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| ProbeFailure::Network)?;
                    if bytes.len() > LIMIT {
                        return Err(ProbeFailure::Malformed);
                    }
                    serde_json::from_slice(&bytes).map_err(|_| ProbeFailure::Malformed)?
                }
                Err(error) if error.is_not_found() => BTreeMap::new(),
                Err(_) => return Err(ProbeFailure::Network),
            };
        if let Some(expected) = pins.get(key) {
            if !is_fingerprint(expected) {
                return Err(ProbeFailure::Malformed);
            }
            return expected_fingerprint(expected, Some(live)).map(|_| ());
        }
        if !confirmed {
            return Ok(());
        }
        pins.insert(key.to_owned(), live.to_owned());
        let bytes = serde_json::to_vec(&pins).map_err(|_| ProbeFailure::Malformed)?;
        dir.atomic_write_with_permissions(
            OsStr::new("quota-claude-identities.json"),
            ".quota-identities",
            &bytes,
            0o600,
            PublishMode::Replace,
        )
        .map_err(|_| ProbeFailure::Network)?
        .require_durable()
        .map_err(|_| ProbeFailure::Network)?;
        Ok(())
    }
    fn claude_selector<'a>(&self, dir: &'a Path) -> Option<(&'static str, &'a Path)> {
        (dir != self.home.join(".claude")).then_some(("CLAUDE_CONFIG_DIR", dir))
    }
    fn claude_identity(
        &self,
        dir: &Path,
        expected: &str,
        deadline: i64,
    ) -> Result<Identity, ProbeFailure> {
        let fingerprint = status_fingerprint(self.command(
            "claude",
            &["auth", "status", "--json"],
            self.claude_selector(dir),
            None,
            deadline,
            10,
        ))?;
        // Verify the registered identity before opening its credential. A
        // directory can have been signed in as another account since the last
        // observation; its secret must not be attributed to this registration.
        self.pin_first_fingerprint(expected, &fingerprint, false)?;
        let credential = optional_probe(self.claude_token(dir, deadline))?;
        let missing = credential.is_none();
        let (token, expires) = credential.map_or((None, None), |(t, e)| (Some(t), Some(e)));
        // Every selected directory can rotate while a tick is in flight.
        let confirmed = status_fingerprint(self.command(
            "claude",
            &["auth", "status", "--json"],
            self.claude_selector(dir),
            None,
            deadline,
            10,
        ))?;
        if confirmed != fingerprint {
            return Err(ProbeFailure::Network);
        }
        self.pin_first_fingerprint(expected, &fingerprint, true)?;
        Ok(Identity {
            token,
            expires,
            missing,
            fingerprint,
        })
    }
    fn claude_credentials(&self, dir: &Path, deadline: i64) -> Result<Value, ProbeFailure> {
        let raw = if cfg!(target_os = "macos") {
            let mut service = "Claude Code-credentials".to_owned();
            if dir != self.home.join(".claude") {
                service.push('-');
                service.push_str(&crate::claude::keychain_suffix(dir.as_os_str().as_bytes()));
            }
            let out = self
                .command(
                    "/usr/bin/security",
                    &["find-generic-password", "-s", &service, "-w"],
                    None,
                    None,
                    deadline,
                    10,
                )
                .map_err(|error| match error {
                    ProbeFailure::ProbeDirectory => error,
                    _ => ProbeFailure::Network,
                })?;
            keychain_output(out)?
        } else {
            let file =
                fs::File::open(dir.join(".credentials.json")).map_err(credential_open_error)?;
            let mut bytes = Vec::new();
            file.take(LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ProbeFailure::Network)?;
            if bytes.len() > LIMIT {
                return Err(ProbeFailure::Network);
            }
            bytes
        };
        parse_credentials(&raw)
    }
    fn claude_token(&self, dir: &Path, deadline: i64) -> Result<(String, i64), ProbeFailure> {
        let value = self.claude_credentials(dir, deadline)?;
        let oauth = value.get("claudeAiOauth").ok_or(ProbeFailure::Auth)?;
        let token = oauth
            .get("accessToken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or(ProbeFailure::Network)?
            .to_owned();
        let expires = oauth
            .get("expiresAt")
            .and_then(Value::as_i64)
            .map(|e| e / 1000)
            .ok_or(ProbeFailure::Network)?;
        Ok((token, expires))
    }
    pub(super) fn claude(
        &self,
        account: &Account,
        deadline: i64,
        details: &mut QuotaDetails,
    ) -> Result<Value, ProbeFailure> {
        let mut fixed =
            self.claude_identity(&account.identity_dir, &account.scope.account_key, deadline)?;
        let expected = fixed.fingerprint.clone();
        *self.login_expiry.borrow_mut() = fixed.expires;
        // Distinct account fingerprints prove that the selected and default
        // directories cannot hold the same account's single-use refresh token.
        if !usable(&fixed, self.now())
            && self.refresh_expired
            && fixed.expires.is_some_and(|e| e <= self.now() + 60)
        {
            let default = self.home.join(".claude");
            let independent = if account.identity_dir == default {
                true
            } else {
                let default_fingerprint = status_fingerprint(self.command(
                    "claude",
                    &["auth", "status", "--json"],
                    None,
                    None,
                    deadline,
                    10,
                ))
                .map_err(|error| {
                    if error == ProbeFailure::Auth {
                        ProbeFailure::Network
                    } else {
                        error
                    }
                })?;
                default_fingerprint != fixed.fingerprint
            };
            let mut refresh_error = None;
            if independent {
                #[cfg(test)]
                let executable = self
                    .executor
                    .as_ref()
                    .map(|_| PathBuf::from("/fixture/claude"))
                    .or_else(claude_executable);
                #[cfg(not(test))]
                let executable = claude_executable();
                if let (Some(executable), Some(user)) = (
                    executable,
                    std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME")),
                ) && let Ok(system) = SystemExecutor::new(1)
                {
                    #[cfg(test)]
                    let base: &dyn Executor = self.executor.as_deref().unwrap_or(&system);
                    #[cfg(not(test))]
                    let base: &dyn Executor = &system;
                    let executor = crate::host::HostProbeExecutor::new(base, &self.state)
                        .map_err(|_| ProbeFailure::ProbeDirectory)?;
                    let login = crate::claude::Login {
                        executable: &executable,
                        home: &self.home,
                        user: user.as_os_str(),
                        config_dir: &account.identity_dir,
                    };
                    let now_ms = || (self.now().max(0) as u64).saturating_mul(1000);
                    let deadline_ms = (deadline.max(0) as u64).saturating_mul(1000);
                    refresh_error = refresh(&executor, &login, &now_ms, deadline_ms).err();
                }
                *self.login_expiry.borrow_mut() = None;
                fixed = self.claude_identity(&account.identity_dir, &expected, deadline)?;
                *self.login_expiry.borrow_mut() = fixed.expires;
            }
            if !usable(&fixed, self.now()) {
                return Err(refresh_error.unwrap_or_else(|| no_usable_identity(&fixed)));
            }
        }
        if !usable(&fixed, self.now()) {
            return Err(no_usable_identity(&fixed));
        }
        details.fingerprint = Some(expected);
        self.http(
            "https://api.anthropic.com/api/oauth/usage",
            &[
                (
                    "Authorization",
                    format!(
                        "Bearer {}",
                        fixed.token.as_ref().ok_or(ProbeFailure::Network)?
                    ),
                ),
                ("anthropic-beta", "oauth-2025-04-20".into()),
            ],
            deadline,
        )
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn unusable_identity_distinguishes_expired_from_uncertain() {
        use super::*;
        let mut identity = Identity {
            token: Some("fake".into()),
            expires: Some(1000),
            missing: false,
            fingerprint: "fake".into(),
        };
        assert_eq!(no_usable_identity(&identity), ProbeFailure::Network);
        assert_eq!(no_usable_identity(&identity), ProbeFailure::Network);
        identity.expires = None;
        assert_eq!(no_usable_identity(&identity), ProbeFailure::Network);
        identity.missing = true;
        assert_eq!(no_usable_identity(&identity), ProbeFailure::Auth);
    }

    #[test]
    fn timed_out_keychain_read_is_transient() {
        use super::*;
        let out = ExecOutcome {
            termination: Termination::Exit(44),
            stdout: Default::default(),
            stderr: Default::default(),
            timed_out: true,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        };
        assert_eq!(keychain_output(out), Err(ProbeFailure::Network));
        let missing = ExecOutcome {
            termination: Termination::Exit(44),
            stdout: Default::default(),
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        };
        assert_eq!(keychain_output(missing), Err(ProbeFailure::Auth));
        let cancelled = ExecOutcome {
            termination: Termination::Exit(44),
            stdout: Default::default(),
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: true,
            elapsed_micros: 0,
        };
        assert_eq!(keychain_output(cancelled), Err(ProbeFailure::Network));
    }

    #[test]
    fn timed_out_auth_status_is_transient() {
        use super::*;
        let out = ExecOutcome {
            termination: Termination::Exit(0),
            stdout: Default::default(),
            stderr: Default::default(),
            timed_out: true,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        };
        assert_eq!(status_fingerprint(Ok(out)), Err(ProbeFailure::Network));
        assert_eq!(
            status_fingerprint(Err(ProbeFailure::Malformed)),
            Err(ProbeFailure::Network)
        );
    }

    #[test]
    fn absent_credentials_are_auth_and_invalid_json_is_malformed() {
        use super::*;
        assert_eq!(parse_credentials(b""), Err(ProbeFailure::Auth));
        assert_eq!(parse_credentials(b"{}"), Err(ProbeFailure::Auth));
        assert_eq!(parse_credentials(b"invalid"), Err(ProbeFailure::Malformed));
        assert!(parse_credentials(br#"{"claudeAiOauth":{}}"#).is_ok());
    }

    #[test]
    fn malformed_status_output_is_not_a_network_failure() {
        use super::*;
        let out = ExecOutcome {
            termination: Termination::Exit(0),
            stdout: provenant_os::exec::CapturedStream {
                bytes: b"invalid".to_vec(),
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        };
        assert_eq!(status_fingerprint(Ok(out)), Err(ProbeFailure::Malformed));
    }

    #[test]
    fn signed_out_status_is_classified() {
        use super::*;
        let outcome = |json: &[u8], code, timed_out| ExecOutcome {
            termination: Termination::Exit(code),
            stdout: provenant_os::exec::CapturedStream {
                bytes: json.to_vec(),
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        };
        for code in [0, 1] {
            assert_eq!(
                status_fingerprint(Ok(outcome(br#"{"loggedIn":false}"#, code, false))),
                Err(ProbeFailure::Auth)
            );
        }
        assert_eq!(
            status_fingerprint(Ok(outcome(br#"{}"#, 1, false))),
            Err(ProbeFailure::Network)
        );
        assert_eq!(
            status_fingerprint(Ok(outcome(b"invalid", 1, false))),
            Err(ProbeFailure::Network)
        );
        assert_eq!(
            status_fingerprint(Ok(outcome(br#"{"loggedIn":false}"#, 1, true))),
            Err(ProbeFailure::Network)
        );
        assert_eq!(
            credential_open_error(std::io::ErrorKind::NotFound.into()),
            ProbeFailure::Auth
        );
        assert_eq!(
            credential_open_error(std::io::ErrorKind::PermissionDenied.into()),
            ProbeFailure::Network
        );
    }

    #[test]
    fn quota_refresh_renews_near_expiry_file_credentials() {
        use super::*;
        struct Renew<'a>(&'a Path, std::sync::atomic::AtomicUsize);
        impl Executor for Renew<'_> {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert_eq!(options.argv[0], OsStr::new("/fake/claude"));
                self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::fs::write(
                    self.0,
                    br#"{"claudeAiOauth":{"accessToken":"fake-renewed","expiresAt":5000000}}"#,
                )
                .unwrap();
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
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
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".credentials.json");
        std::fs::write(
            &path,
            br#"{"claudeAiOauth":{"accessToken":"fake-expiring","expiresAt":1001000}}"#,
        )
        .unwrap();
        let executor = Renew(&path, Default::default());
        let login = crate::claude::Login {
            executable: Path::new("/fake/claude"),
            home: temp.path(),
            user: OsStr::new("fake"),
            config_dir: temp.path(),
        };
        refresh(&executor, &login, &|| 1000000, 1060000).unwrap();
        assert_eq!(executor.1.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("fake-renewed")
        );
    }

    #[test]
    fn refresh_reports_a_transient_failure_without_provider_output() {
        use super::*;
        struct Busy;
        impl Executor for Busy {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                Err(ExecError::Busy)
            }
        }
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"fake-expiring","expiresAt":1000}}"#,
        )
        .unwrap();
        let login = crate::claude::Login {
            executable: Path::new("/fake/claude"),
            home: Path::new("/fake/home"),
            user: OsStr::new("fake"),
            config_dir: temp.path(),
        };
        assert_eq!(
            refresh(&Busy, &login, &|| 1000, 100000),
            Err(ProbeFailure::Network)
        );
    }

    #[test]
    fn transient_refresh_can_be_retried_successfully() {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Retry<'a> {
            path: &'a Path,
            calls: AtomicUsize,
        }
        impl Executor for Retry<'_> {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    return Err(ExecError::Busy);
                }
                std::fs::write(
                    self.path,
                    br#"{"claudeAiOauth":{"accessToken":"fake-new","expiresAt":5000000}}"#,
                )
                .unwrap();
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
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
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".credentials.json");
        std::fs::write(
            &path,
            br#"{"claudeAiOauth":{"accessToken":"fake-old","expiresAt":1000}}"#,
        )
        .unwrap();
        let executor = Retry {
            path: &path,
            calls: AtomicUsize::new(0),
        };
        let login = crate::claude::Login {
            executable: Path::new("/fake/claude"),
            home: temp.path(),
            user: OsStr::new("fake"),
            config_dir: temp.path(),
        };
        assert_eq!(
            refresh(&executor, &login, &|| 1000, 100000),
            Err(ProbeFailure::Network)
        );
        assert_eq!(refresh(&executor, &login, &|| 1000, 100000), Ok(()));
        assert_eq!(executor.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn rejected_refresh_is_distinct_from_transient_failure() {
        use super::*;
        struct Reject;
        impl Executor for Reject {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                Ok(ExecOutcome {
                    termination: Termination::Exit(1),
                    stdout: Default::default(),
                    stderr: provenant_os::exec::CapturedStream {
                        bytes: b"authentication_failed".to_vec(),
                        ..Default::default()
                    },
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"fake","expiresAt":1000}}"#,
        )
        .unwrap();
        let login = crate::claude::Login {
            executable: Path::new("/fake/claude"),
            home: temp.path(),
            user: OsStr::new("fake"),
            config_dir: temp.path(),
        };
        assert_eq!(
            refresh(&Reject, &login, &|| 1000, 100000),
            Err(ProbeFailure::LoginExpired)
        );
    }

    #[test]
    fn unchanged_expired_credential_after_probe_is_reported() {
        use super::*;
        struct Completed;
        impl Executor for Completed {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
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
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"fake","expiresAt":1000}}"#,
        )
        .unwrap();
        let login = crate::claude::Login {
            executable: Path::new("/fake/claude"),
            home: temp.path(),
            user: OsStr::new("fake"),
            config_dir: temp.path(),
        };
        assert_eq!(
            refresh(&Completed, &login, &|| 1000, 100000),
            Err(ProbeFailure::LoginExpired)
        );
    }

    #[test]
    fn refresh_preserves_a_probe_directory_failure_even_if_the_next_call_differs() {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Fail(AtomicUsize);
        impl Executor for Fail {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                    Err(ExecError::Io {
                        operation: PROBE_DIR_OPERATION,
                        source: std::io::Error::other("private directory refused"),
                    })
                } else {
                    Err(ExecError::Busy)
                }
            }
        }
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"fake-expiring","expiresAt":1000}}"#,
        )
        .unwrap();
        let login = crate::claude::Login {
            executable: Path::new("/bin/claude"),
            home: Path::new("/home/test"),
            user: OsStr::new("test"),
            config_dir: temp.path(),
        };
        let result = refresh(&Fail(AtomicUsize::new(0)), &login, &|| 1, 1000);
        assert_eq!(result, Err(ProbeFailure::ProbeDirectory));
    }

    #[test]
    fn fixed_directory_rejects_changed_account_fingerprint() {
        use super::{ProbeFailure, expected_fingerprint};
        assert!(matches!(
            expected_fingerprint("0123456789abcdef", Some("fedcba9876543210")),
            Err(ProbeFailure::AccountMismatch)
        ));
        assert_eq!(
            expected_fingerprint("hand-keyed", Some("0123456789abcdef")).unwrap(),
            "0123456789abcdef"
        );
        assert_eq!(
            expected_fingerprint("pinned", None),
            Err(ProbeFailure::Network)
        );
    }
    #[test]
    fn hand_keyed_registration_pins_its_first_observed_login() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        let quota = LiveQuota {
            home: temp.path().to_owned(),
            state: temp.path().to_owned(),
            roots: None,
            accounts: RefCell::new(Vec::new()),
            refresh_expired: false,
            login_expiry: RefCell::new(None),
            readiness_failure: RefCell::new(None),
            last_reconcile_error: RefCell::new(None),
            clock: SystemClock::new(),
            executor: None,
        };
        assert_eq!(
            quota.pin_first_fingerprint("manual", "0123456789abcdef", false),
            Ok(())
        );
        assert!(!temp.path().join("quota-claude-identities.json").exists());
        assert_eq!(
            quota.pin_first_fingerprint("manual", "0123456789abcdef", true),
            Ok(())
        );
        assert_eq!(
            quota.pin_first_fingerprint("manual", "0123456789abcdef", true),
            Ok(())
        );
        assert_eq!(
            quota.pin_first_fingerprint("manual", "fedcba9876543210", true),
            Err(ProbeFailure::AccountMismatch)
        );
    }
    #[test]
    fn selected_identity_needs_a_token_with_safety_margin() {
        use super::{Identity, usable};
        let mut selected = Identity {
            token: Some("fake".into()),
            expires: Some(1061),
            missing: false,
            fingerprint: "fake".into(),
        };
        assert!(usable(&selected, 1000));
        assert!(!usable(&selected, 1001));
        selected.token = None;
        assert!(!usable(&selected, 1000));
    }
}
