use super::*;
use provenant_os::fs::{Directory, PublishMode};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

// No Debug/Serialize: an identity can carry an access token only in memory.
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
    let expected = configured.or(previous).or(live).ok_or(ProbeFailure::Auth)?;
    if live != Some(expected) {
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
    let default = store(default)?;
    let fixed = fixed
        .pointer("/claudeAiOauth/refreshToken")
        .and_then(Value::as_str);
    let default = default
        .pointer("/claudeAiOauth/refreshToken")
        .and_then(Value::as_str);
    Ok(!fixed.is_some_and(|fixed| Some(fixed) == default))
}

// Missing credentials or CLI output may leave an identity unknown, but a
// failure to prepare its private directory must reach the quota report.
fn optional_probe<T>(result: Result<T, ProbeFailure>) -> Result<Option<T>, ProbeFailure> {
    match result {
        Err(ProbeFailure::ProbeDirectory) => Err(ProbeFailure::ProbeDirectory),
        other => Ok(other.ok()),
    }
}

fn refresh(
    executor: &dyn Executor,
    login: &provenant_providers::claude::Login<'_>,
    now_ms: &dyn Fn() -> u64,
    deadline_ms: u64,
) -> Result<(), ProbeFailure> {
    use std::sync::atomic::{AtomicBool, Ordering};

    // The shared refresh tolerates failed credential reads. Remember directory
    // failures even if it makes another call and discards the original error.
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
    let _ = provenant_providers::claude::refresh_if_expiring(
        &watched,
        login,
        now_ms,
        deadline_ms,
        None,
    );
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
        let state = optional_probe(self.json_command(
            "claude",
            &["auth", "status", "--json"],
            self.claude_selector(dir),
            deadline,
            4,
        ))?;
        let fingerprint = state
            .as_ref()
            .and_then(|s| s.get("email").and_then(Value::as_str))
            .map(|s| fingerprint(s, 16));
        let (token, expires) = optional_probe(self.claude_token(dir, deadline))?
            .map_or((None, None), |(t, e)| (Some(t), Some(e)));
        // The default session can rotate while a tick is in flight. Confirm
        // the identity after reading its credential before attributing it.
        if dir == self.home.join(".claude") {
            let after = optional_probe(self.json_command(
                "claude",
                &["auth", "status", "--json"],
                None,
                deadline,
                4,
            ))?;
            let confirmed = after
                .as_ref()
                .and_then(|s| s.get("email").and_then(Value::as_str))
                .map(|s| super::fingerprint(s, 16));
            if confirmed != fingerprint {
                return Ok(Identity {
                    fingerprint: None,
                    token: None,
                    expires: None,
                });
            }
        }
        Ok(Identity {
            fingerprint,
            token,
            expires,
        })
    }
    fn claude_credentials(&self, dir: &Path, deadline: i64) -> Result<Value, ProbeFailure> {
        let raw = if cfg!(target_os = "macos") {
            let mut service = "Claude Code-credentials".to_owned();
            if dir != self.home.join(".claude") {
                service.push('-');
                service.push_str(&crate::hosts::keychain_suffix(dir.as_os_str().as_bytes()));
            }
            let out = self.command(
                "/usr/bin/security",
                &["find-generic-password", "-s", &service, "-w"],
                None,
                None,
                deadline,
                3,
            )?;
            if !clean(&out) || out.termination != Termination::Exit(0) {
                return Err(ProbeFailure::Auth);
            }
            out.stdout.bytes
        } else {
            read(&dir.join(".credentials.json"))?
        };
        serde_json::from_slice(&raw).map_err(|_| ProbeFailure::Auth)
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
            let default = self.claude_identity(&self.home.join(".claude"), deadline)?;
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
                let executor =
                    provenant_providers::host::HostProbeExecutor::new(&system, &self.state)
                        .map_err(|_| ProbeFailure::ProbeDirectory)?;
                let login = provenant_providers::claude::Login {
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
        let login = provenant_providers::claude::Login {
            executable: Path::new("/bin/claude"),
            home: Path::new("/home/test"),
            user: OsStr::new("test"),
            config_dir: Path::new("/home/test/.claude"),
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
