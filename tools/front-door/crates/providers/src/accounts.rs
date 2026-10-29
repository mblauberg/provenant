//! Durable non-secret registrations and host identity checks.
use crate::{ProviderError, host::PROBE_DIR_OPERATION, identity::RegisteredAccount};
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use provenant_os::{
    exec::{ExecError, ExecOptions, Executor, StreamPolicy, Termination},
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
    /// Claude settings kept identical across account directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_settings: Option<SharedSettings>,
}
/// `[domains.accounts.shared_settings]`. Both fields are optional: the source
/// defaults to `~/.claude/settings.json` when it exists, the keys to
/// `provenant_config::shared_settings::DEFAULT_KEYS`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
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
    /// Subscription prices over time, for value reporting. Not the detected `plan`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plans: Vec<provenant_quota::PlanPrice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
    /// Menu bar name; trimmed, non-empty, at most 64 characters, no control characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Menu bar position, 1..=999; absent means unordered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u16>,
    /// Per-account headline override; absent follows the global choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<provenant_quota::Headline>,
    /// Shared Claude settings this account holds instead of the source's:
    /// key to value, each key on the allow-list.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub settings: std::collections::BTreeMap<String, serde_json::Value>,
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
impl Settings {
    /// The Console's Zen allowance shares a login with Go, but never its quota
    /// or pacing identity. Derive it in memory without changing registrations.
    pub fn with_derived_accounts(&self) -> Self {
        let mut settings = self.clone();
        for spec in provenant_records::ADAPTER_SPECS {
            let Some((key, _)) = spec.free_account else {
                continue;
            };
            if settings
                .entries
                .values()
                .any(|entry| entry.adapter == spec.name && entry.key == key)
            {
                continue;
            }
            let Some(mut entry) = settings
                .entries
                .values()
                .find(|entry| entry.adapter == spec.name)
                .cloned()
            else {
                continue;
            };
            entry.key = key.into();
            entry.label = key.into();
            entry.plan = None;
            entry.plans.clear();
            entry.limits = None;
            entry.billing_anchor = None;
            entry.display_name = None;
            entry.hidden = false;
            entry.order = None;
            entry.headline = None;
            entry.settings.clear();
            let mut id = format!("{}-{key}", spec.name);
            while settings.entries.contains_key(&id) {
                id.push('_');
            }
            settings.entries.insert(id, entry);
        }
        settings
    }
}
const NOT_ALLOWED: &str =
    "a per-account `settings` key is not in `shared_settings.keys`; add it there first";
impl Settings {
    /// The allow-list, configured or default.
    pub fn settings_keys(&self) -> Vec<String> {
        match self.shared_settings.as_ref().and_then(|s| s.keys.as_ref()) {
            Some(keys) => keys.clone(),
            None => provenant_config::shared_settings::DEFAULT_KEYS
                .map(str::to_owned)
                .to_vec(),
        }
    }
    /// Why the shared-settings configuration must be refused, if it must.
    /// Identity, credential or hook keys in the allow-list, an override or an
    /// `env` name; or an override of a key the allow-list does not name.
    pub fn settings_error(&self) -> Option<&'static str> {
        use provenant_config::shared_settings::{check_key, check_value, interned};
        let refuse = |reason: String| {
            interned(format!(
                "{reason}; remove it from `keys` or `settings` (or the `env` it names)"
            ))
        };
        let keys = self.settings_keys();
        if let Some(reason) = keys.iter().find_map(|key| check_key(key).err()) {
            return Some(refuse(reason));
        }
        for entry in self.entries.values() {
            for (key, value) in &entry.settings {
                if let Err(reason) = check_key(key).and_then(|()| check_value(key, value)) {
                    return Some(refuse(reason));
                }
                if !keys.contains(key) {
                    return Some(NOT_ALLOWED);
                }
            }
        }
        None
    }
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
        let mut errors = Vec::new();
        if bad {
            errors.push(SectionError {
                field: "entries".into(),
                code: "registration",
            });
        }
        if self
            .entries
            .values()
            .any(|e| !provenant_quota::valid_plans(&e.plans))
        {
            errors.push(SectionError {
                field: "entries".into(),
                code: "plans",
            });
        }
        if self.entries.iter().any(|(id, e)| !valid_display(id, e)) {
            errors.push(SectionError {
                field: "entries".into(),
                code: "display",
            });
        }
        if let Some(code) = self.settings_error() {
            errors.push(SectionError {
                field: "shared_settings".into(),
                code,
            });
        }
        errors
    }
}
fn valid_entry(entry: &Registration) -> bool {
    provenant_records::AdapterSpec::get(&entry.adapter).is_some()
        && entry.key != "unknown"
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
/// Display fields only; the entry id must also be usable as a quota icon reference.
fn valid_display(id: &str, entry: &Registration) -> bool {
    provenant_quota::valid_icon(id)
        && entry.display_name.as_deref().is_none_or(valid_display_name)
        && entry.order.is_none_or(|o| (1..=999).contains(&o))
}
pub fn valid_display_name(s: &str) -> bool {
    !s.is_empty() && s.trim() == s && s.chars().count() <= 64 && !s.chars().any(char::is_control)
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

fn exec_error(error: ExecError) -> ProviderError {
    match error {
        ExecError::Io {
            operation: PROBE_DIR_OPERATION,
            source,
        } => ProviderError::ProbeDirectory(source.to_string()),
        _ => ProviderError::UnknownIdentity,
    }
}

/// Status output is projected immediately to its fingerprint; email is never retained.
/// Claude Code selects its keychain account from `USER`, so the probe carries it.
pub fn detect(
    executor: &dyn Executor,
    adapter: &str,
    executable: &Path,
    home: &Path,
    user: &OsStr,
    directory: &Path,
    probe_root: &Path,
) -> Result<String, ProviderError> {
    detect_with_org(
        executor, adapter, executable, home, user, directory, probe_root,
    )
    .map(|(key, _)| key)
}
/// The account key and, for Claude, the organisation fingerprint when exposed.
pub fn detect_with_org(
    executor: &dyn Executor,
    adapter: &str,
    executable: &Path,
    home: &Path,
    user: &OsStr,
    directory: &Path,
    probe_root: &Path,
) -> Result<(String, Option<String>), ProviderError> {
    let cwd = Path::new(crate::claude::PROBE_CWD);
    if adapter == "agy" {
        return detect_agy(executor, executable, home, user, cwd, probe_root)
            .map(|key| (key, None));
    }
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
        if db
            .try_exists()
            .map_err(|_| ProviderError::UnknownIdentity)?
        {
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
            let out = executor.execute(options).map_err(exec_error)?;
            if out.stdout.truncated || out.timed_out {
                return Err(ProviderError::UnknownIdentity);
            }
            if out.termination == Termination::Exit(0)
                && let Some(key) = std::str::from_utf8(&out.stdout.bytes)
                    .ok()
                    .and_then(|token| crate::identity::cursor_account(token.trim()))
            {
                return Ok((key, None));
            }
        }
        let bytes = read_following(
            &directory.join("cli-config.json"),
            ReadOptions { limit: 1024 * 1024 },
        )
        .map_err(|_| ProviderError::UnknownIdentity)?;
        let config: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ProviderError::UnknownIdentity)?;
        // authInfo.authId and the desktop JWT sub are the same subject, verified 27 Oct 2026.
        let key = config["authInfo"]["authId"]
            .as_str()
            .and_then(crate::identity::digest)
            .ok_or(ProviderError::UnknownIdentity)?;
        return Ok((format!("cursor-{key}"), None));
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
    let out = executor.execute(options).map_err(exec_error)?;
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

fn detect_agy(
    executor: &dyn Executor,
    executable: &Path,
    home: &Path,
    user: &OsStr,
    cwd: &Path,
    probe_root: &Path,
) -> Result<String, ProviderError> {
    // Production callers construct HostProbeExecutor first, which sweeps
    // directories left by crashed owners under this same host state root.
    let scratch = crate::host::HostProbeDir::create(probe_root)
        .map_err(|error| ProviderError::ProbeDirectory(error.to_string()))?;
    let log = scratch.path().join("cli.log");
    let argv: Vec<OsString> = [
        executable.as_os_str(),
        OsStr::new("-p"),
        OsStr::new("/usage"),
        OsStr::new("--output-format"),
        OsStr::new("json"),
        OsStr::new("--log-file"),
        log.as_os_str(),
    ]
    .map(Into::into)
    .to_vec();
    let env = [
        ("HOME".into(), home.into()),
        ("USER".into(), user.into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    let mut options = ExecOptions::new(
        &argv,
        cwd,
        Duration::from_secs(crate::agy::REGISTRATION.quota.probe_seconds),
    );
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let out = executor.execute(options).map_err(exec_error)?;
    if out.termination != Termination::Exit(0)
        || out.stdout.truncated
        || out.stdout.read_failed
        || out.timed_out
        || out.drain_truncated
        || out.overflow_stopped
    {
        return Err(ProviderError::UnknownIdentity);
    }
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout.bytes).map_err(|_| ProviderError::UnknownIdentity)?;
    if payload.get("status").and_then(serde_json::Value::as_str) != Some("SUCCESS") {
        return Err(ProviderError::LoginRequired);
    }
    let bytes = read_following(&log, ReadOptions { limit: 1024 * 1024 })
        .map_err(|_| ProviderError::UnknownIdentity)?;
    crate::identity::agy_fingerprint(&bytes)
        .map(|fingerprint| format!("agy-{fingerprint}"))
        .ok_or(ProviderError::UnknownIdentity)
}
