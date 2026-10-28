use super::*;
use provenant_os::fs::{Directory, PublishMode};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

// No Debug/Serialize: an identity can carry an access token only in memory.
#[derive(Default)]
pub(super) struct Identity {
    fingerprint: Option<String>,
    token: Option<String>,
    expires: Option<i64>,
}
fn freshest<'a>(
    fingerprint: &str,
    fixed: &'a Identity,
    default: &'a Identity,
    at: i64,
) -> Option<&'a Identity> {
    [fixed, default]
        .into_iter()
        .filter(|i| {
            i.fingerprint.as_deref() == Some(fingerprint)
                && i.token.is_some()
                && i.expires.is_some_and(|e| e > at + 60)
        })
        .max_by_key(|i| i.expires)
}
fn expected_fingerprint<'a>(
    configured: Option<&'a str>,
    previous: Option<&'a str>,
    live: Option<&'a str>,
) -> Result<&'a str, ProbeFailure> {
    let expected = configured
        .or(previous)
        .or(live)
        .ok_or(ProbeFailure::Network)?;
    let live = live.ok_or(ProbeFailure::Network)?;
    if live != expected {
        return Err(ProbeFailure::Auth);
    }
    Ok(expected)
}
fn claude_executable() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("claude"))
        .filter(|path| provenant_os::sys::is_executable(path))
        .find_map(|path| path.canonicalize().ok())
}
fn may_refresh(
    fixed: &Path,
    default: &Path,
    mut store: impl FnMut(&Path) -> Result<Value, ProbeFailure>,
) -> Result<bool, ProbeFailure> {
    if fixed == default {
        return Ok(true);
    }
    // Refresh tokens are single-use. Compare only in memory: refreshing a
    // cloned fixed login would invalidate the default session's shared token.
    let fixed = store(fixed)?;
    let default = match store(default) {
        Ok(value) => value,
        Err(ProbeFailure::ProbeDirectory) => return Err(ProbeFailure::ProbeDirectory),
        Err(_) => return Ok(false),
    };
    let fixed = fixed
        .pointer("/claudeAiOauth/refreshToken")
        .and_then(Value::as_str);
    let default = default
        .pointer("/claudeAiOauth/refreshToken")
        .and_then(Value::as_str);
    Ok(!fixed.is_some_and(|fixed| Some(fixed) == default))
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
        serde_json::from_slice(&out.stdout.bytes).map_err(|_| ProbeFailure::Network)?;
    if state.get("loggedIn").and_then(Value::as_bool) == Some(false)
        || state
            .get("email")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(ProbeFailure::Auth);
    }
    if out.termination != Termination::Exit(0) {
        return Err(ProbeFailure::Network);
    }
    state
        .get("email")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| fingerprint(s, 16))
        .ok_or(ProbeFailure::Auth)
}

fn default_identity(result: Result<Identity, ProbeFailure>) -> Result<Identity, ProbeFailure> {
    match result {
        Ok(identity) => Ok(identity),
        Err(ProbeFailure::ProbeDirectory) => Err(ProbeFailure::ProbeDirectory),
        Err(_) => Ok(Identity::default()),
    }
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
    let _ = crate::claude::refresh_if_expiring(&watched, login, now_ms, deadline_ms, None);
    if watched.directory_failed.load(Ordering::Relaxed) {
        Err(ProbeFailure::ProbeDirectory)
    } else {
        Ok(())
    }
}

impl LiveQuota {
    fn claude_selector<'a>(&self, dir: &'a Path) -> Option<(&'static str, &'a Path)> {
        (dir != self.home.join(".claude")).then_some(("CLAUDE_CONFIG_DIR", dir))
    }
    fn claude_identity(&self, dir: &Path, deadline: i64) -> Result<Identity, ProbeFailure> {
        let fingerprint = status_fingerprint(self.command(
            "claude",
            &["auth", "status", "--json"],
            self.claude_selector(dir),
            None,
            deadline,
            10,
        ))?;
        let (token, expires) = optional_probe(self.claude_token(dir, deadline))?
            .map_or((None, None), |(t, e)| (Some(t), Some(e)));
        // The default session can rotate while a tick is in flight. Confirm
        // the identity after reading its credential before attributing it.
        if dir == self.home.join(".claude") {
            let confirmed = status_fingerprint(self.command(
                "claude",
                &["auth", "status", "--json"],
                None,
                None,
                deadline,
                10,
            ))?;
            if confirmed != fingerprint {
                return Err(ProbeFailure::Network);
            }
        }
        Ok(Identity {
            fingerprint: Some(fingerprint),
            token,
            expires,
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
        serde_json::from_slice(&raw).map_err(|_| ProbeFailure::Network)
    }
    fn claude_token(&self, dir: &Path, deadline: i64) -> Result<(String, i64), ProbeFailure> {
        let value = self.claude_credentials(dir, deadline)?;
        let oauth = value.get("claudeAiOauth").ok_or(ProbeFailure::Auth)?;
        let token = oauth
            .get("accessToken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or(ProbeFailure::Auth)?
            .to_owned();
        let expires = oauth
            .get("expiresAt")
            .and_then(Value::as_i64)
            .map(|e| e / 1000)
            .ok_or(ProbeFailure::Auth)?;
        Ok((token, expires))
    }
    fn mark_refresh(&self, account: &Account, expires: i64) -> Result<bool, ProbeFailure> {
        let key = account.scope.account_key.clone();
        // Re-read under the quota tick lock; unreadable markers must not allow
        // another refresh attempt for an already attempted expiry.
        let dir = Directory::open_anchor(&self.state).map_err(|_| ProbeFailure::Network)?;
        let mut markers: BTreeMap<String, i64> =
            match dir.open_read(OsStr::new("quota-claude-refresh.json")) {
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
                Err(e) if e.is_not_found() => BTreeMap::new(),
                Err(_) => return Err(ProbeFailure::Network),
            };
        if markers.get(&key) == Some(&expires) {
            return Ok(false);
        }
        markers.retain(|key, _| {
            self.accounts
                .iter()
                .any(|a| a.scope.adapter == "claude" && a.scope.account_key == *key)
        });
        markers.insert(key, expires);
        let bytes = serde_json::to_vec(&markers).map_err(|_| ProbeFailure::Malformed)?;
        provenant_os::fs::Directory::atomic_write_with_permissions(
            &dir,
            OsStr::new("quota-claude-refresh.json"),
            ".quota-refresh",
            &bytes,
            0o600,
            PublishMode::Replace,
        )
        .map_err(|_| ProbeFailure::Network)?
        .require_durable()
        .map_err(|_| ProbeFailure::Network)?;
        Ok(true)
    }
    pub(super) fn claude(
        &self,
        account: &Account,
        previous: Option<&AccountRecord>,
        deadline: i64,
        details: &mut QuotaDetails,
    ) -> Result<Value, ProbeFailure> {
        if self.claude_default.borrow().is_none() {
            let default =
                default_identity(self.claude_identity(&self.home.join(".claude"), deadline))?;
            *self.claude_default.borrow_mut() = Some(default);
        }
        let mut fixed = self.claude_identity(&account.identity_dir, deadline)?;
        let expected = expected_fingerprint(
            account.options.get("fingerprint").and_then(Value::as_str),
            previous
                .and_then(|r| r.snapshot.as_ref())
                .and_then(|s| s.details.as_ref())
                .and_then(|d| d.fingerprint.as_deref()),
            fixed.fingerprint.as_deref(),
        )?
        .to_owned();
        // Refresh only through Claude Code. The deliberately unknown model is
        // rejected before inference; credentials are never written by Provenant.
        let has_fresh = self
            .claude_default
            .borrow()
            .as_ref()
            .is_some_and(|default| freshest(&expected, &fixed, default, now()).is_some());
        if !has_fresh
            && self.refresh_expired
            && fixed.expires.is_some_and(|e| e <= now() + 60)
            && may_refresh(&account.identity_dir, &self.home.join(".claude"), |dir| {
                self.claude_credentials(dir, deadline)
            })?
            && self.mark_refresh(account, fixed.expires.ok_or(ProbeFailure::Auth)?)?
        {
            // The shared refresh resolves the executable and applies the
            // hardening flags on every platform; only its keychain read is
            // macOS-specific, and the identity is re-read below regardless.
            if let (Some(executable), Some(user)) = (
                claude_executable(),
                std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME")),
            ) && let Ok(system) = SystemExecutor::new(1)
            {
                // Each command gets a fresh private cwd under the stable host
                // state root, so a killed run's leftovers are swept next time.
                let executor = crate::host::HostProbeExecutor::new(&system, &self.state)
                    .map_err(|_| ProbeFailure::ProbeDirectory)?;
                let login = crate::claude::Login {
                    executable: &executable,
                    home: &self.home,
                    user: user.as_os_str(),
                    config_dir: &account.identity_dir,
                };

                let now_ms = || (now().max(0) as u64).saturating_mul(1000);
                let deadline_ms = (deadline.max(0) as u64).saturating_mul(1000);
                refresh(&executor, &login, &now_ms, deadline_ms)?;
            }
            fixed = self.claude_identity(&account.identity_dir, deadline)?;
        }
        let borrowed = self.claude_default.borrow();
        let default = borrowed.as_ref().ok_or(ProbeFailure::Auth)?;
        let identity = freshest(&expected, &fixed, default, now()).ok_or(ProbeFailure::Auth)?;
        details.fingerprint = Some(expected);
        self.http(
            "https://api.anthropic.com/api/oauth/usage",
            &[
                (
                    "Authorization",
                    format!(
                        "Bearer {}",
                        identity.token.as_ref().ok_or(ProbeFailure::Auth)?
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
    fn signed_out_status_and_optional_default_are_classified() {
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
        for json in [br#"{"loggedIn":false}"#.as_slice(), br#"{}"#] {
            assert_eq!(
                status_fingerprint(Ok(outcome(json, 1, false))),
                Err(ProbeFailure::Auth)
            );
        }
        assert_eq!(
            status_fingerprint(Ok(outcome(b"invalid", 1, false))),
            Err(ProbeFailure::Network)
        );
        assert_eq!(
            status_fingerprint(Ok(outcome(br#"{"loggedIn":false}"#, 1, true))),
            Err(ProbeFailure::Network)
        );
        let fixed = Identity {
            fingerprint: Some("fixed".into()),
            token: Some("fake".into()),
            expires: Some(5000),
        };
        for error in [ProbeFailure::Auth, ProbeFailure::Network] {
            let default = default_identity(Err(error)).unwrap();
            assert!(
                default.fingerprint.is_none()
                    && default.token.is_none()
                    && default.expires.is_none()
            );
            assert!(freshest("fixed", &fixed, &default, 1000).is_some());
        }
        assert!(matches!(
            default_identity(Err(ProbeFailure::ProbeDirectory)),
            Err(ProbeFailure::ProbeDirectory)
        ));
        assert_eq!(
            credential_open_error(std::io::ErrorKind::NotFound.into()),
            ProbeFailure::Auth
        );
        assert_eq!(
            credential_open_error(std::io::ErrorKind::PermissionDenied.into()),
            ProbeFailure::Network
        );
        assert_eq!(
            may_refresh(Path::new("fixed"), Path::new("default"), |p| {
                if p == Path::new("fixed") {
                    Ok(serde_json::json!({}))
                } else {
                    Err(ProbeFailure::Network)
                }
            }),
            Ok(false)
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
    fn fixed_refresh_skips_byte_identical_default_refresh_token() {
        use super::{ProbeFailure, may_refresh};
        use serde_json::json;
        use std::{collections::BTreeMap, path::Path};
        let fixed = Path::new("/fake/fixed");
        let default = Path::new("/fake/.claude");
        let mut stores = BTreeMap::from([
            (
                fixed,
                json!({"claudeAiOauth": {"refreshToken": "fake-shared"}}),
            ),
            (
                default,
                json!({"claudeAiOauth": {"refreshToken": "fake-shared"}}),
            ),
        ]);
        let read = |path: &Path| stores.get(path).cloned().ok_or(ProbeFailure::Auth);
        assert!(!may_refresh(fixed, default, read).unwrap());
        assert!(may_refresh(default, default, read).unwrap());
        stores.insert(
            default,
            json!({"claudeAiOauth": {"refreshToken": "fake-other"}}),
        );
        assert!(
            may_refresh(fixed, default, |path| stores
                .get(path)
                .cloned()
                .ok_or(ProbeFailure::Auth))
            .unwrap()
        );
    }
    #[test]
    fn fixed_directory_rejects_changed_account_fingerprint() {
        use super::{ProbeFailure, expected_fingerprint};
        for (configured, previous) in [(Some("pinned"), None), (None, Some("recorded"))] {
            assert!(matches!(
                expected_fingerprint(configured, previous, Some("other")),
                Err(ProbeFailure::Auth)
            ));
        }
        assert_eq!(
            expected_fingerprint(None, None, Some("first-login")).unwrap(),
            "first-login"
        );
        assert_eq!(
            expected_fingerprint(Some("pinned"), None, None),
            Err(ProbeFailure::Network)
        );
    }
    #[test]
    fn rotating_default_is_attributed_only_to_matching_fingerprint_and_expiry() {
        use super::{Identity, freshest};
        let fixed = Identity {
            fingerprint: Some("pro-hash".into()),
            token: Some("redacted-fixed".into()),
            expires: Some(900),
        };
        let default = Identity {
            fingerprint: Some("pro-hash".into()),
            token: Some("redacted-default".into()),
            expires: Some(5000),
        };
        assert_eq!(
            freshest("pro-hash", &fixed, &default, 1000).and_then(|i| i.token.as_deref()),
            Some("redacted-default")
        );
        assert!(freshest("anese-hash", &fixed, &default, 1000).is_none());
        let rotated = Identity {
            fingerprint: Some("anese-hash".into()),
            token: Some("redacted-rotated".into()),
            expires: Some(6000),
        };
        assert!(freshest("pro-hash", &fixed, &rotated, 1000).is_none());
        assert!(freshest("pro-hash", &fixed, &default, 5000).is_none());
    }
}
