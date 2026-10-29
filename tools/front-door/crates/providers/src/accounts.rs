//! Durable non-secret registrations and host identity checks.
use crate::{ProviderError, host::PROBE_DIR_OPERATION, identity::RegisteredAccount};
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, Profile, SectionError};
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
    /// Further config directories signed into the same account. Schema only;
    /// native session ingest will attribute these to this label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<PathBuf>,
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
            entry.directories.clear();
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
        for (id, entry) in &self.entries {
            for index in provenant_quota::invalid_plan_rows(&entry.plans) {
                errors.push(SectionError {
                    field: format!("entries.{id}.plans.{index}"),
                    code: "plans",
                });
            }
            if !provenant_quota::valid_icon(id) {
                errors.push(SectionError {
                    field: format!("entries.{id}"),
                    code: "display",
                });
            }
            if entry
                .display_name
                .as_deref()
                .is_some_and(|name| !valid_display_name(name))
            {
                errors.push(SectionError {
                    field: format!("entries.{id}.display_name"),
                    code: "display",
                });
            }
            if entry.order.is_some_and(|order| !(1..=999).contains(&order)) {
                errors.push(SectionError {
                    field: format!("entries.{id}.order"),
                    code: "display",
                });
            }
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
        && entry.directories.len() <= 8
        && entry.directories.iter().all(|d| {
            d.is_absolute()
                && !d.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
        })
        && !entry.directory.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
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
fn boundary_error(error: &SectionError) -> bool {
    if error.code == "unknown_key" {
        return false;
    }
    if matches!(error.code, "registration" | "row_type")
        || error.field == "shared_settings"
        || error.field.starts_with("shared_settings.")
    {
        return true;
    }
    if !matches!(error.code, "type" | "range" | "schema") {
        return false;
    }
    let Some(path) = error.field.strip_prefix("entries.") else {
        return true;
    };
    // Account IDs may contain dots. Decode diagnostics identify the leaf, so
    // classify from its end instead of assuming an ID has one path segment.
    let field = path.rsplit('.').find(|part| part.parse::<usize>().is_err());
    !matches!(
        field,
        Some(
            "plan"
                | "plans"
                | "priority"
                | "billing_anchor"
                | "limits"
                | "monthly_usd"
                | "cycle_anchor"
                | "weekly_anchor"
                | "display_name"
                | "hidden"
                | "order"
                | "headline"
                | "name"
                | "price"
                | "currency"
                | "effective_from"
                | "notional_usd"
        )
    )
}
pub const FIELD_METADATA: &[FieldMetadata] = &[
    FieldMetadata::int("entries.{id}.order", 1, 999),
    FieldMetadata::new("entries.{id}.hidden", "bool"),
    FieldMetadata::new("entries.{id}.display_name", "string"),
    FieldMetadata::enumeration("entries.{id}.headline", &["auto", "5h", "weekly"]),
    FieldMetadata::new("shared_settings.source", "string"),
    FieldMetadata::new("shared_settings.keys", "array"),
];
pub const SECTION: DomainSection = DomainSection::new::<Settings>("accounts", defaults)
    .with_boundary_check(boundary_error)
    .with_fields(FIELD_METADATA);

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

#[cfg(test)]
mod config_tests {
    use super::*;
    use provenant_config::{DomainSections, Origin};

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Domains {
        accounts: Settings,
    }
    const SECTIONS: DomainSections<Domains> = DomainSections::new(&[SECTION]);
    const ENTRY: &str = "schema_version=1\n[domains.accounts.entries.work]\nadapter='claude'\nlabel='work'\nkey='work-key'\ndirectory='/fixture/claude-work'\n";

    fn roots() -> provenant_core::Roots {
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
                Ok(path.to_owned())
            }
        }
        let mut input =
            provenant_core::RootInputs::new("/fixture/home".into(), "/fixture/repo".into());
        input.state_dir = Some("/fixture/state".into());
        input.resolve(&Resolver).unwrap()
    }

    #[test]
    fn optional_account_metadata_warns_without_losing_registration() {
        for extra in [
            "display_name=''\norder=0\nplans=[{name='plan',price=-1,currency='USD',effective_from='2026-01-01'}]",
            "display_name=42\norder='first'\nplans=false",
        ] {
            let text = format!("{ENTRY}priority=2\nhidden=true\n{extra}");
            let config =
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .unwrap();
            let entry = &config.domains.accounts.entries["work"];
            assert_eq!(entry.key, "work-key");
            assert_eq!(entry.priority, Some(2));
            assert!(entry.hidden);
            assert!(entry.display_name.is_none());
            assert!(entry.order.is_none());
            assert!(entry.plans.is_empty());
            assert_eq!(config.warnings.len(), 3, "{:?}", config.warnings);
            assert!(matches!(
                config.origins["domains.accounts.entries.work.priority"],
                Origin::User { .. }
            ));
            assert!(
                config
                    .origins
                    .get("domains.accounts.entries.work.display_name")
                    .is_none_or(|origin| matches!(origin, Origin::Compiled { .. }))
            );
        }
    }

    #[test]
    fn account_identity_and_shared_settings_errors_remain_hard() {
        for (before, after) in [
            ("adapter='claude'", "adapter=42"),
            ("label='work'", "label=42"),
            ("key='work-key'", "key=42"),
            ("directory='/fixture/claude-work'", "directory=42"),
            ("directory='/fixture/claude-work'", "directory='relative'"),
            ("adapter='claude'\n", ""),
        ] {
            let text = ENTRY.replace(before, after);
            assert!(
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .is_err(),
                "{text}"
            );
        }
        for extra in [
            "directories=42",
            "org_fingerprint=42",
            "settings=42",
            "[domains.accounts.shared_settings]\nsource=42",
            "[domains.accounts.shared_settings]\nkeys=['env.ANTHROPIC_AUTH_TOKEN']",
        ] {
            let text = format!("{ENTRY}{extra}");
            assert!(
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .is_err(),
                "{text}"
            );
        }
        let incomplete = ENTRY
            .replace("entries.work", "entries.'work.alias.plan'")
            .replace("adapter='claude'\n", "");
        assert!(
            provenant_config::load_with_domains(&roots(), Some(incomplete.as_bytes()), &SECTIONS)
                .is_err(),
            "{incomplete}"
        );
        for row in ["plan=42", "'my.plan'=42", "'my.plan'=[]"] {
            let text = format!("schema_version=1\n[domains.accounts.entries]\n{row}");
            assert!(
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .is_err(),
                "{text}"
            );
        }
        assert!((SECTION.boundary)(&SectionError {
            field: "entries.work.alias.directory".into(),
            code: "type"
        }));
        assert!(!(SECTION.boundary)(&SectionError {
            field: "entries.work.alias.display_name".into(),
            code: "type"
        }));
    }

    #[test]
    fn invalid_optional_plan_rows_preserve_valid_siblings_and_identity() {
        for invalid in [
            "{name='bad',price=-1,effective_from='2026-02-01'}",
            "{name='bad',price='bad',effective_from='2026-02-01'}",
            "{name='bad',effective_from='2026-02-01'}",
        ] {
            let text = format!(
                "{ENTRY}plans=[{{name='first',price=1,effective_from='2026-01-01'}},{invalid},{{name='third',price=3,effective_from='2026-03-01'}}]"
            );
            let config =
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .unwrap();
            let entry = &config.domains.accounts.entries["work"];
            assert_eq!(entry.key, "work-key");
            assert_eq!(
                entry
                    .plans
                    .iter()
                    .map(|plan| plan.name.as_str())
                    .collect::<Vec<_>>(),
                ["first", "third"]
            );
            assert_eq!(config.warnings.len(), 1, "{:?}", config.warnings);
            assert!(config.warnings[0].message.contains("entries.work.plans.1"));
            assert!(matches!(
                config.origins["domains.accounts.entries.work.plans.1.name"],
                Origin::User { .. }
            ));
        }
    }

    #[test]
    fn invalid_metadata_on_a_dotted_account_id_keeps_every_registration() {
        let text = format!(
            "{}display_name=''\n[domains.accounts.entries.home]\nadapter='codex'\nlabel='home'\nkey='home-key'\ndirectory='/fixture/codex-home'\n",
            ENTRY.replace("entries.work", "entries.'work.alias'")
        );
        let config =
            provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                .unwrap();
        let entries = &config.domains.accounts.entries;
        assert_eq!(entries["work.alias"].key, "work-key");
        assert!(entries["work.alias"].display_name.is_none());
        assert_eq!(entries["home"].key, "home-key");
        assert_eq!(config.warnings.len(), 1, "{:?}", config.warnings);
    }

    #[test]
    fn optional_metadata_wrong_container_types_keep_identity() {
        for field in ["limits=42", "priority=[]", "display_name={}"] {
            let text = format!("{ENTRY}{field}");
            let config =
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .unwrap();
            assert_eq!(config.domains.accounts.entries["work"].key, "work-key");
            assert_eq!(config.warnings.len(), 1);
        }
    }
}
