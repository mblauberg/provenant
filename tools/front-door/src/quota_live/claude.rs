use super::*;
use provenant_supervisor::fs::{Directory, Publication, PublishMode, SafeDir};
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
impl LiveQuota {
    fn claude_selector<'a>(&self, dir: &'a Path) -> Option<(&'static str, &'a Path)> {
        (dir != self.home.join(".claude")).then_some(("CLAUDE_CONFIG_DIR", dir))
    }
    fn claude_identity(&self, dir: &Path, deadline: i64) -> Identity {
        let state = self
            .json_command(
                "claude",
                &["auth", "status", "--json"],
                self.claude_selector(dir),
                deadline,
                4,
            )
            .ok();
        let fingerprint = state
            .as_ref()
            .and_then(|s| s.get("email").and_then(Value::as_str))
            .map(|s| fingerprint(s, 16));
        let (token, expires) = self
            .claude_token(dir, deadline)
            .map_or((None, None), |(t, e)| (Some(t), Some(e)));
        // The default session can rotate while a tick is in flight. Confirm
        // the identity after reading its credential before attributing it.
        if dir == self.home.join(".claude") {
            let after = self
                .json_command("claude", &["auth", "status", "--json"], None, deadline, 4)
                .ok();
            let confirmed = after
                .as_ref()
                .and_then(|s| s.get("email").and_then(Value::as_str))
                .map(|s| super::fingerprint(s, 16));
            if confirmed != fingerprint {
                return Identity {
                    fingerprint: None,
                    token: None,
                    expires: None,
                };
            }
        }
        Identity {
            fingerprint,
            token,
            expires,
        }
    }
    fn claude_token(&self, dir: &Path, deadline: i64) -> Result<(String, i64), ProbeFailure> {
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
        let value: Value = serde_json::from_slice(&raw).map_err(|_| ProbeFailure::Auth)?;
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
        match SafeDir::atomic_write(
            &dir,
            OsStr::new("quota-claude-refresh.json"),
            ".quota-refresh",
            &bytes,
            0o600,
            PublishMode::Replace,
        )
        .map_err(|_| ProbeFailure::Network)?
        {
            Publication::Durable => Ok(true),
            Publication::VisibleDurabilityUnconfirmed { .. } => Err(ProbeFailure::Network),
        }
    }
    pub(super) fn claude(
        &self,
        account: &Account,
        previous: Option<&AccountRecord>,
        deadline: i64,
        details: &mut QuotaDetails,
    ) -> Result<Value, ProbeFailure> {
        if self.claude_default.borrow().is_none() {
            let default = self.claude_identity(&self.home.join(".claude"), deadline);
            *self.claude_default.borrow_mut() = Some(default);
        }
        let mut fixed = self.claude_identity(&account.identity_dir, deadline);
        let expected = fixed
            .fingerprint
            .clone()
            .or_else(|| {
                account
                    .options
                    .get("fingerprint")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                previous
                    .and_then(|r| r.snapshot.as_ref())
                    .and_then(|s| s.details.as_ref())
                    .and_then(|d| d.fingerprint.clone())
            })
            .ok_or(ProbeFailure::Auth)?;
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
            && self.mark_refresh(account, fixed.expires.ok_or(ProbeFailure::Auth)?)?
        {
            let _ = self.command(
                "claude",
                &[
                    "-p",
                    "x",
                    "--model",
                    "provenant-refresh-probe",
                    "--max-turns",
                    "1",
                ],
                self.claude_selector(&account.identity_dir),
                None,
                deadline,
                12,
            );
            fixed = self.claude_identity(&account.identity_dir, deadline);
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
