//! Durable non-secret registrations and host identity checks.
use crate::{ProviderError, identity::RegisteredAccount};
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use provenant_supervisor::{
    exec::{ExecOptions, Executor, StreamPolicy, Termination},
    fs::{ReadOptions, read_following},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub entries: std::collections::BTreeMap<String, Registration>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub adapter: String,
    pub label: String,
    pub key: String,
    /// Organisation recorded at first validation; absent on legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_fingerprint: Option<String>,
    pub directory: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_anchor: Option<String>,
}
impl Registration {
    /// Compares the probed organisation with the recorded one. Both present and
    /// different refuses; an absent probe never refuses. `Ok(Some(org))` is a
    /// legacy record to upgrade with the probed organisation (trust on first use).
    pub fn check_org(&self, probed: Option<&str>) -> Result<Option<String>, ProviderError> {
        match (&self.org_fingerprint, probed) {
            (Some(stored), Some(probed)) if stored != probed => {
                Err(ProviderError::IdentityMismatch)
            }
            (None, Some(probed)) => Ok(Some(probed.to_owned())),
            _ => Ok(None),
        }
    }
    pub fn registered(&self) -> Result<RegisteredAccount, ProviderError> {
        RegisteredAccount::new(&self.adapter, &self.key, self.directory.clone())
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        let bad =
            self.entries.len() > 128
                || self.entries.values().enumerate().any(|(i, e)| {
                    !valid_entry(e)
                        || !label(&e.label)
                        || self.entries.values().take(i).any(|a| {
                            a.adapter == e.adapter && (a.label == e.label || a.key == e.key)
                        })
                });
        if bad {
            vec![SectionError {
                field: "entries".into(),
                code: "registration",
            }]
        } else {
            Vec::new()
        }
    }
}
fn valid_entry(entry: &Registration) -> bool {
    matches!(
        entry.adapter.as_str(),
        "claude" | "codex" | "agy" | "cursor" | "opencode"
    ) && entry.key != "unknown"
        && !entry.key.is_empty()
        && entry.key.len() <= 64
        && entry
            .key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && entry.directory.is_absolute()
        && !entry.directory.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}
pub fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn defaults(_: Profile) -> &'static str {
    "[entries]\n"
}
pub const SECTION: DomainSection = DomainSection::new::<Settings>("accounts", defaults);

/// Status output is projected immediately to its fingerprint; email is never retained.
/// Claude Code selects its keychain account from `USER`, so the probe carries it.
pub fn detect(
    executor: &dyn Executor,
    adapter: &str,
    executable: &Path,
    home: &Path,
    user: &OsStr,
    directory: &Path,
    cwd: &Path,
) -> Result<String, ProviderError> {
    detect_with_org(executor, adapter, executable, home, user, directory, cwd).map(|(key, _)| key)
}
/// The account key and, for Claude, the organisation fingerprint when exposed.
pub fn detect_with_org(
    executor: &dyn Executor,
    adapter: &str,
    executable: &Path,
    home: &Path,
    user: &OsStr,
    directory: &Path,
    cwd: &Path,
) -> Result<(String, Option<String>), ProviderError> {
    if adapter == "codex" {
        let bytes = read_following(
            &directory.join("auth.json"),
            ReadOptions { limit: 1024 * 1024 },
        )
        .map_err(|_| ProviderError::UnknownIdentity)?;
        return crate::identity::codex_account(&bytes)
            .map(|key| (key, None))
            .ok_or(ProviderError::UnknownIdentity);
    }
    if adapter == "cursor" {
        let db = directory.join("state.vscdb");
        let argv: Vec<OsString> = vec![
            "/usr/bin/sqlite3".into(),
            "-readonly".into(),
            db.into(),
            "SELECT value FROM ItemTable WHERE key='cursorAuth/accessToken' LIMIT 1".into(),
        ];
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(4));
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let out = executor
            .execute(options)
            .map_err(|_| ProviderError::UnknownIdentity)?;
        if out.termination != Termination::Exit(0) || out.stdout.truncated || out.timed_out {
            return Err(ProviderError::UnknownIdentity);
        }
        return std::str::from_utf8(&out.stdout.bytes)
            .ok()
            .and_then(|token| crate::identity::cursor_account(token.trim()))
            .map(|key| (key, None))
            .ok_or(ProviderError::UnknownIdentity);
    }
    if adapter != "claude" || directory == home.join(".claude") {
        return Err(ProviderError::UnknownIdentity);
    }
    let argv: Vec<OsString> = [
        executable.as_os_str(),
        OsStr::new("auth"),
        OsStr::new("status"),
        OsStr::new("--json"),
    ]
    .map(Into::into)
    .to_vec();
    let env = [
        ("HOME".into(), home.into()),
        ("USER".into(), user.into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("CLAUDE_CONFIG_DIR".into(), directory.into()),
    ];
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(4));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 65536 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let out = executor
        .execute(options)
        .map_err(|_| ProviderError::UnknownIdentity)?;
    if out.termination != Termination::Exit(0)
        || out.stdout.truncated
        || out.stdout.read_failed
        || out.timed_out
        || out.drain_truncated
    {
        return Err(ProviderError::UnknownIdentity);
    }
    #[derive(Deserialize)]
    struct Status {
        #[serde(rename = "loggedIn")]
        logged_in: bool,
        #[serde(default)]
        email: Option<String>,
        #[serde(rename = "orgId", default)]
        org_id: Option<String>,
    }
    let status: Status =
        serde_json::from_slice(&out.stdout.bytes).map_err(|_| ProviderError::UnknownIdentity)?;
    if !status.logged_in {
        return Err(ProviderError::LoginRequired);
    }
    let key = status
        .email
        .as_deref()
        .and_then(crate::identity::fingerprint)
        .ok_or(ProviderError::UnknownIdentity)?;
    Ok((
        key,
        status
            .org_id
            .as_deref()
            .and_then(crate::identity::org_fingerprint),
    ))
}
