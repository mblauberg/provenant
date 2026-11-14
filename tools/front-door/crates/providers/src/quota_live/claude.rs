use super::*;
#[cfg(test)]
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

// No Debug/Serialize: an identity can carry an access token only in memory.
struct Identity {
    token: Option<String>,
    expires: Option<i64>,
    fingerprint: String,
    login_expires: Option<i64>,
}
fn usable(identity: &Identity, at: i64) -> bool {
    identity.token.is_some() && identity.expires.is_some_and(|e| e > at.saturating_add(60))
}
fn is_fingerprint(key: &str) -> bool {
    key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit())
}
fn expected_fingerprint(expected: &str, live: &str) -> Result<(), ProbeFailure> {
    if !is_fingerprint(expected) {
        return Err(ProbeFailure::Malformed);
    }
    if live != expected {
        return Err(ProbeFailure::AccountMismatch);
    }
    Ok(())
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

struct AuthStatus {
    fingerprint: String,
    login_expires: Option<i64>,
}
fn login_expiry(value: &Value) -> Option<i64> {
    if let Some(seconds) = value.as_i64() {
        return Some(if seconds > 100_000_000_000 {
            seconds / 1000
        } else {
            seconds
        });
    }
    value
        .as_str()?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|at| at.as_second())
}
#[cfg(test)]
fn status_fingerprint(result: Result<ExecOutcome, ProbeFailure>) -> Result<String, ProbeFailure> {
    auth_status(result).map(|status| status.fingerprint)
}
fn auth_status(result: Result<ExecOutcome, ProbeFailure>) -> Result<AuthStatus, ProbeFailure> {
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
    let fingerprint = state
        .get("email")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| fingerprint(s, 16))
        .filter(|s| is_fingerprint(s))
        .ok_or(ProbeFailure::Network)?;
    if state.get("loggedIn").and_then(Value::as_bool) != Some(true) {
        return Err(ProbeFailure::Malformed);
    }
    Ok(AuthStatus {
        fingerprint,
        login_expires: state.get("loginExpiresAt").and_then(login_expiry),
    })
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
    minimum_budget_ms: u64,
    force: bool,
) -> Result<(), ProbeFailure> {
    use std::sync::atomic::{AtomicBool, Ordering};

    // Preserve directory failures, which the quota report must distinguish
    // from credential failures even when the shared refresh returns early.
    struct Watch<'a> {
        executor: &'a dyn Executor,
        directory_failed: AtomicBool,
    }
    impl Executor for Watch<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let result = self.executor.execute(options);
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
    };
    let result = crate::claude::refresh_with_policy(
        &watched,
        login,
        now_ms,
        deadline_ms,
        None,
        minimum_budget_ms,
        force,
    );
    if watched.directory_failed.load(Ordering::Relaxed) {
        Err(ProbeFailure::ProbeDirectory)
    } else {
        result.map(|_| ()).map_err(|error| match error {
            crate::ProviderError::CredentialExpired
            | crate::ProviderError::CredentialNotFound
            | crate::ProviderError::RefreshDeferred => ProbeFailure::RefreshDeferred,
            _ => ProbeFailure::Network,
        })
    }
}

impl LiveQuota {
    fn claude_selector<'a>(&self, dir: &'a Path) -> Option<(&'static str, &'a Path)> {
        (dir != self.home.join(".claude")).then_some(("CLAUDE_CONFIG_DIR", dir))
    }
    fn claude_identity(
        &self,
        dir: &Path,
        expected: &str,
        deadline: i64,
    ) -> Result<Identity, ProbeFailure> {
        let status = auth_status(self.command(
            "claude",
            &["auth", "status", "--json"],
            self.claude_selector(dir),
            None,
            deadline,
            10,
        ));
        let status = match status {
            Err(ProbeFailure::Auth) => {
                *self.signed_in.borrow_mut() = Some(false);
                *self.refresh_pending.borrow_mut() = false;
                return Err(ProbeFailure::Auth);
            }
            other => other?,
        };
        let fingerprint = status.fingerprint;
        // Verify the registered identity before opening its credential. A
        // directory can have been signed in as another account since the last
        // observation; its secret must not be attributed to this registration.
        expected_fingerprint(expected, &fingerprint)?;
        *self.signed_in.borrow_mut() = Some(true);
        *self.login_expiry.borrow_mut() = status.login_expires;
        let credential = optional_probe(self.claude_token(dir, deadline))?;
        let (token, expires) = credential.map_or((None, None), |(t, e)| (Some(t), Some(e)));
        // Every selected directory can rotate while a tick is in flight.
        let confirmed = auth_status(self.command(
            "claude",
            &["auth", "status", "--json"],
            self.claude_selector(dir),
            None,
            deadline,
            10,
        ));
        let confirmed = match confirmed {
            Err(ProbeFailure::Auth) => {
                *self.signed_in.borrow_mut() = Some(false);
                *self.refresh_pending.borrow_mut() = false;
                return Err(ProbeFailure::Auth);
            }
            other => other?,
        };
        if confirmed.fingerprint != fingerprint {
            return Err(ProbeFailure::Network);
        }
        *self.access_expiry.borrow_mut() = expires;
        *self.login_expiry.borrow_mut() = confirmed.login_expires;
        Ok(Identity {
            token,
            expires,
            fingerprint,
            login_expires: confirmed.login_expires,
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
        *self.login_expiry.borrow_mut() = fixed.login_expires;
        *self.refresh_pending.borrow_mut() = !usable(&fixed, self.now());
        if !usable(&fixed, self.now()) {
            if !self.refresh_expired {
                return Err(ProbeFailure::RefreshDeferred);
            }
            let refreshed = self.refresh_claude(account, false);
            // The refresh owns its duration; observation gets a new read budget.
            // Read status even after failure: only loggedIn:false proves that
            // the CLI cleared a rejected login, while another caller may also
            // have completed a usable renewal.
            fixed = self.claude_identity(&account.identity_dir, &expected, self.now() + 20)?;
            if !usable(&fixed, self.now()) {
                return Err(refreshed.err().unwrap_or(ProbeFailure::RefreshDeferred));
            }
        }
        *self.refresh_pending.borrow_mut() = false;
        details.fingerprint = Some(expected.clone());
        let usage = |identity: &Identity, deadline| {
            self.http(
                "https://api.anthropic.com/api/oauth/usage",
                &[
                    (
                        "Authorization",
                        format!(
                            "Bearer {}",
                            identity
                                .token
                                .as_ref()
                                .ok_or(ProbeFailure::RefreshDeferred)?
                        ),
                    ),
                    ("anthropic-beta", "oauth-2025-04-20".into()),
                ],
                deadline,
            )
        };
        let request_deadline = deadline.max(self.now() + 15);
        let result = match usage(&fixed, request_deadline) {
            Err(ProbeFailure::Auth) if self.refresh_expired => {
                // One CLI refresh and one retry. A still-signed-in identity
                // remains refreshable even when the usage endpoint refuses it.
                *self.refresh_pending.borrow_mut() = true;
                let refreshed = self.refresh_claude(account, true);
                fixed = self.claude_identity(&account.identity_dir, &expected, self.now() + 20)?;
                refreshed?;
                usage(&fixed, self.now() + 15).map_err(|failure| {
                    if failure == ProbeFailure::Auth {
                        ProbeFailure::RefreshDeferred
                    } else {
                        failure
                    }
                })
            }
            Err(ProbeFailure::Auth) => {
                *self.refresh_pending.borrow_mut() = true;
                Err(ProbeFailure::RefreshDeferred)
            }
            other => other,
        };
        if result.is_ok() {
            *self.refresh_pending.borrow_mut() = false;
        }
        result
    }

    fn refresh_claude(&self, account: &Account, force: bool) -> Result<(), ProbeFailure> {
        #[cfg(test)]
        let executable = self
            .executor
            .as_ref()
            .map(|_| PathBuf::from("/fixture/claude"))
            .or_else(claude_executable);
        #[cfg(not(test))]
        let executable = claude_executable();
        let executable = executable.ok_or(ProbeFailure::MissingCli)?;
        let user = std::env::var_os("USER")
            .or_else(|| std::env::var_os("LOGNAME"))
            .ok_or(ProbeFailure::Network)?;
        let system = SystemExecutor::new(1).map_err(|_| ProbeFailure::Network)?;
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
        refresh(
            &executor,
            &login,
            &now_ms,
            now_ms().saturating_add(self.refresh_budget_seconds.saturating_mul(1000)),
            self.refresh_min_budget_seconds.saturating_mul(1000),
            force,
        )
    }
}
#[cfg(test)]
mod tests {
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
            Err(ProbeFailure::Malformed)
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
        refresh(&executor, &login, &|| 1000000, 1060000, 60_000, false).unwrap();
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
            refresh(&Busy, &login, &|| 1000, 100000, 60_000, false),
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
            refresh(&executor, &login, &|| 1000, 100000, 60_000, false),
            Err(ProbeFailure::Network)
        );
        assert_eq!(
            refresh(&executor, &login, &|| 1000, 100000, 60_000, false),
            Ok(())
        );
        assert_eq!(executor.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn rejected_refresh_stays_pending_without_signed_out_observation() {
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
            refresh(&Reject, &login, &|| 1000, 100000, 60_000, false),
            Err(ProbeFailure::RefreshDeferred)
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
            refresh(&Completed, &login, &|| 1000, 100000, 60_000, false),
            Err(ProbeFailure::RefreshDeferred)
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
        let result = refresh(
            &Fail(AtomicUsize::new(0)),
            &login,
            &|| 1,
            100_001,
            60_000,
            false,
        );
        assert_eq!(result, Err(ProbeFailure::ProbeDirectory));
    }

    #[test]
    fn current_registration_requires_its_matching_account_fingerprint() {
        use super::{ProbeFailure, expected_fingerprint};
        assert_eq!(
            expected_fingerprint("0123456789abcdef", "0123456789abcdef"),
            Ok(())
        );
        assert_eq!(
            expected_fingerprint("0123456789abcdef", "fedcba9876543210"),
            Err(ProbeFailure::AccountMismatch)
        );
        assert_eq!(
            expected_fingerprint("hand-keyed", "0123456789abcdef"),
            Err(ProbeFailure::Malformed)
        );
    }
    #[test]
    fn selected_identity_needs_a_token_with_safety_margin() {
        use super::{Identity, usable};
        let mut selected = Identity {
            token: Some("fake".into()),
            expires: Some(1061),
            login_expires: None,
            fingerprint: "fake".into(),
        };
        assert!(usable(&selected, 1000));
        assert!(!usable(&selected, 1001));
        selected.token = None;
        assert!(!usable(&selected, 1000));
    }
}
