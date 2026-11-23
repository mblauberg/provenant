//! Durable non-secret registrations and host identity checks.
use crate::{ProviderError, host::PROBE_DIR_OPERATION, identity::RegisteredAccount};
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, Profile, SectionError};
use provenant_core::names::{self, Catalogue, Fold, NameKind, NameRefusal, Resolved};
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
    /// No registrations is the default, so a configuration without them reads.
    #[serde(default)]
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
    /// Organisation observed at validation; absent when the provider does not expose it.
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
    /// One row per registered credential identity; no plan or account is invented.
    pub fn unique_accounts(&self) -> Self {
        let mut settings = self.clone();
        let mut identities = std::collections::BTreeSet::new();
        settings
            .entries
            .retain(|_, entry| identities.insert((entry.adapter.clone(), entry.key.clone())));
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
    pub fn settings_error(&self) -> Option<(&'static str, Option<&'static str>)> {
        use provenant_config::shared_settings::{check_key, check_value, interned};
        let refuse = |reason: provenant_config::shared_settings::SharedSettingsError| {
            let code = reason.code();
            let detail = interned(format!(
                "{reason}; remove it from `keys` or `settings` (or the `env` it names)"
            ));
            (detail, code)
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
                    return Some((NOT_ALLOWED, None));
                }
            }
        }
        None
    }
}
impl Registration {
    /// Compares the probed organisation with the recorded one. Both present and
    /// different refuses; when registration could not observe an organisation,
    /// the first available probe supplies it.
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
                detail: None,
                field: "entries".into(),
                code: "registration",
            });
        }
        for (id, entry) in &self.entries {
            for index in provenant_quota::invalid_plan_rows(&entry.plans) {
                errors.push(SectionError {
                    detail: None,
                    field: format!("entries.{id}.plans.{index}"),
                    code: "plans",
                });
            }
            if !provenant_quota::valid_icon(id) {
                errors.push(SectionError {
                    detail: None,
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
                    detail: None,
                    field: format!("entries.{id}.display_name"),
                    code: "display",
                });
            }
            if entry.order.is_some_and(|order| !(1..=999).contains(&order)) {
                errors.push(SectionError {
                    detail: None,
                    field: format!("entries.{id}.order"),
                    code: "display",
                });
            }
        }
        if let Some((message, stable_code)) = self.settings_error() {
            errors.push(SectionError {
                detail: stable_code.map(|_| message),
                field: "shared_settings".into(),
                code: stable_code.unwrap_or(message),
            });
        }
        errors
    }
}
fn valid_entry(entry: &Registration) -> bool {
    provenant_quota::component(&entry.adapter)
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

/// The one typed identity every account reference resolves to. Callers project
/// it to the canonical spelling their own output needs; no caller re-matches
/// the reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountIdentity {
    pub adapter: String,
    pub key: String,
    pub entry_id: String,
    pub label: String,
    /// A harness local login with no registry entry. It answers only to its
    /// harness-qualified spelling, so a bare `local` cannot steal a label.
    pub local_login: bool,
}

impl AccountIdentity {
    /// The harness-qualified spelling, `adapter/key`.
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.adapter, self.key)
    }
}

/// The spellings one calling context accepts. Declared as data so every
/// context shares the resolver while keeping its own accepted forms.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceForms {
    pub entry_id: bool,
    pub label: bool,
    pub key: bool,
    pub qualified: bool,
    pub local_logins: bool,
}

impl ReferenceForms {
    /// `accounts` commands: registry entry ids and unique labels.
    pub const REGISTRY: Self = Self {
        entry_id: true,
        label: true,
        key: false,
        qualified: false,
        local_logins: false,
    };
    /// Route configuration: every registered form and harness local logins.
    pub const ROUTING: Self = Self {
        entry_id: true,
        label: true,
        key: true,
        qualified: true,
        local_logins: true,
    };
    /// Run preparation: registered keys and labels within one adapter.
    pub const SELECTION: Self = Self {
        entry_id: false,
        label: true,
        key: true,
        qualified: false,
        local_logins: false,
    };
    /// Continuation: an exact provider and account key pair.
    pub const RETAINED: Self = Self {
        entry_id: false,
        label: false,
        key: true,
        qualified: false,
        local_logins: false,
    };
}

fn account_identities(settings: &Settings) -> Vec<AccountIdentity> {
    let mut identities: Vec<AccountIdentity> = settings
        .entries
        .iter()
        .map(|(entry_id, entry)| AccountIdentity {
            adapter: entry.adapter.clone(),
            key: entry.key.clone(),
            entry_id: entry_id.clone(),
            label: entry.label.clone(),
            local_login: false,
        })
        .collect();
    for spec in provenant_records::ADAPTER_SPECS
        .iter()
        .filter(|spec| spec.local_login)
    {
        let key = spec.local_account.1;
        let qualified = format!("{}/{}", spec.name, key);
        if identities
            .iter()
            .any(|identity| identity.adapter == spec.name && identity.key == key)
        {
            continue;
        }
        identities.push(AccountIdentity {
            adapter: spec.name.to_owned(),
            key: key.to_owned(),
            entry_id: qualified,
            label: spec.local_account.0.to_owned(),
            local_login: true,
        });
    }
    identities
}

/// Every identity answering to `reference` under `forms`, compared
/// case-insensitively. A local login is never matched by its bare label or key.
fn matching_identities<'a>(
    identities: &'a [AccountIdentity],
    forms: ReferenceForms,
    reference: &str,
) -> Vec<&'a AccountIdentity> {
    let folded = reference.to_lowercase();
    let matches_form = |form: &str| form == reference || form.to_lowercase() == folded;
    identities
        .iter()
        .filter(|identity| {
            if identity.local_login {
                return forms.local_logins
                    && forms.qualified
                    && matches_form(&identity.qualified());
            }
            (forms.label && matches_form(&identity.label))
                || (forms.entry_id && matches_form(&identity.entry_id))
                || (forms.key && matches_form(&identity.key))
                || (forms.qualified && matches_form(&identity.qualified()))
        })
        .collect()
}

impl Catalogue for AccountCatalogue {
    type Item = AccountIdentity;
    const KIND: NameKind = NameKind("account");

    fn names(&self) -> Vec<String> {
        if !self.forms.label {
            return Vec::new();
        }
        let mut labels: Vec<String> = self
            .identities
            .iter()
            .filter(|identity| !identity.local_login)
            .map(|identity| identity.label.clone())
            .collect();
        labels.sort();
        labels.dedup();
        labels
    }

    fn lookup(&self, canonical: &str) -> Option<Self::Item> {
        // Aliases point at a sentinel-prefixed entry id: a label is not a safe
        // key here because a local login may share a registered account's label.
        if let Some(entry_id) = canonical.strip_prefix('\0') {
            return self
                .identities
                .iter()
                .find(|identity| identity.entry_id == entry_id)
                .cloned();
        }
        let mut matching = self
            .identities
            .iter()
            .filter(|identity| !identity.local_login && identity.label == canonical);
        let first = matching.next()?;
        matching.next().is_none().then(|| (*first).clone())
    }

    fn aliases(&self) -> Vec<(String, String)> {
        let mut aliases = Vec::new();
        for identity in &self.identities {
            let canonical = format!("\0{}", identity.entry_id);
            let mut add = |form: &str| {
                aliases.push((form.to_owned(), canonical.clone()));
                let folded = form.to_lowercase();
                if folded != form {
                    aliases.push((folded, canonical.clone()));
                }
            };
            if identity.local_login {
                if self.forms.qualified {
                    add(&identity.qualified());
                }
                continue;
            }
            if self.forms.entry_id {
                add(&identity.entry_id);
            }
            if self.forms.key {
                add(&identity.key);
            }
            if self.forms.qualified {
                add(&identity.qualified());
            }
        }
        aliases
    }
}

struct AccountCatalogue {
    identities: Vec<AccountIdentity>,
    forms: ReferenceForms,
}

/// Labels of the accounts that answer to `reference` when more than one does
/// under `forms`; empty when the reference is unambiguous. Callers use this to
/// keep the ambiguity one refusal code family across every context.
pub fn ambiguous_labels(
    settings: &Settings,
    reference: &str,
    forms: ReferenceForms,
) -> Vec<String> {
    let identities = account_identities(settings);
    let matched = matching_identities(&identities, forms, reference);
    if matched.len() < 2 {
        return Vec::new();
    }
    let mut labels: Vec<String> = matched
        .iter()
        .map(|identity| identity.label.clone())
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

/// Resolve one account reference to its typed identity. Every command uses
/// this resolver; `forms` declares what the calling context accepts. A miss is
/// a [`NameRefusal`] carrying close matching labels only — never a directory,
/// key or credential value.
pub fn resolve(
    settings: &Settings,
    reference: &str,
    forms: ReferenceForms,
) -> Result<Resolved<AccountIdentity>, NameRefusal> {
    let identities = account_identities(settings);
    let matched = matching_identities(&identities, forms, reference);
    if matched.len() > 1 {
        let mut labels: Vec<String> = matched
            .iter()
            .map(|identity| identity.label.clone())
            .collect();
        labels.sort();
        labels.dedup();
        return Err(NameRefusal {
            kind: NameKind("account"),
            query: reference.to_owned(),
            candidates: labels,
        });
    }
    let catalogue = AccountCatalogue { identities, forms };
    let labels = catalogue.names();
    let mut candidate_labels: Vec<String> = catalogue
        .identities
        .iter()
        .filter(|identity| !identity.local_login)
        .map(|identity| identity.label.clone())
        .collect();
    candidate_labels.sort();
    candidate_labels.dedup();
    let name_matches = labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(reference));
    let alias_matches = catalogue
        .aliases()
        .iter()
        .any(|(alias, _)| alias == reference);
    let query = if name_matches || alias_matches {
        reference.to_owned()
    } else {
        reference.to_lowercase()
    };
    let folded = query != reference;
    match names::resolve(&catalogue, query.as_str()) {
        Ok(mut resolved) => {
            resolved.canonical = resolved.item.qualified();
            if folded {
                resolved.fold = Fold::CaseFolded;
            }
            Ok(resolved)
        }
        Err(mut refusal) => {
            refusal.candidates =
                names::close_matches(query.as_str(), candidate_labels.iter().map(String::as_str))
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
            refusal.query = reference.to_owned();
            Err(refusal)
        }
    }
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
    FieldMetadata::new("entries.{id}.adapter", "string"),
    FieldMetadata::new("entries.{id}.label", "string"),
    FieldMetadata::new("entries.{id}.key", "string"),
    FieldMetadata::new("entries.{id}.org_fingerprint", "string"),
    FieldMetadata::new("entries.{id}.directory", "string"),
    FieldMetadata::new("entries.{id}.directories", "array"),
    FieldMetadata::new("entries.{id}.plan", "string"),
    FieldMetadata::new("entries.{id}.plans", "array"),
    FieldMetadata::new("entries.{id}.priority", "int"),
    FieldMetadata::new("entries.{id}.billing_anchor", "string"),
    FieldMetadata::new("entries.{id}.limits", "table"),
    FieldMetadata::new("entries.{id}.limits.monthly_usd", "float"),
    FieldMetadata::new("entries.{id}.limits.cycle_anchor", "string"),
    FieldMetadata::new("entries.{id}.limits.weekly_anchor", "string"),
    FieldMetadata::new("entries.{id}.settings", "table"),
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
        ExecError::Io {
            operation: crate::agy::KEYCHAIN_OPERATION,
            source,
        } => ProviderError::IsolatedKeychainUnavailable(source.to_string()),
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
fn credential_identity_read_error(error: provenant_os::fs::FsError) -> ProviderError {
    match error {
        provenant_os::fs::FsError::Io { source, .. }
        | provenant_os::fs::FsError::ReadIo { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            ProviderError::CredentialNotFound
        }
        _ => ProviderError::UnknownIdentity,
    }
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
    detect_with_org_inner(
        executor, adapter, executable, home, user, directory, probe_root, true,
    )
}

/// Registration must observe the current identity, even when a recent probe is cached.
pub fn detect_for_registration(
    executor: &crate::host::HostProbeExecutor<'_>,
    adapter: &str,
    executable: &Path,
    home: &Path,
    user: &OsStr,
    directory: &Path,
    probe_root: &Path,
) -> Result<(String, Option<String>), ProviderError> {
    executor.preflight().map_err(exec_error)?;
    detect_with_org_inner(
        executor, adapter, executable, home, user, directory, probe_root, false,
    )
}

#[allow(clippy::too_many_arguments)]
fn detect_with_org_inner(
    executor: &dyn Executor,
    adapter: &str,
    executable: &Path,
    home: &Path,
    user: &OsStr,
    directory: &Path,
    probe_root: &Path,
    use_cache: bool,
) -> Result<(String, Option<String>), ProviderError> {
    let cwd = Path::new(crate::claude::PROBE_CWD);
    let spec = provenant_records::AdapterSpec::get(adapter);
    use provenant_records::AccountIdentityProbe;
    let probe = spec.map_or(AccountIdentityProbe::None, |spec| {
        spec.account_identity_probe
    });
    if probe == AccountIdentityProbe::KeychainLog {
        // An isolated account is signed in under its own directory as `HOME`.
        // The mode comes from the directory, so a missing keychain is refused
        // here rather than probed under the shared login.
        let probe_home = crate::agy::ProbeHome::select(directory, home)?;
        let guarded = probe_home.guard(executor);
        use provenant_core::Clock;
        let now = provenant_os::clock::SystemClock::new().unix_seconds();
        if use_cache && let Some(key) = crate::agy::cached_identity(probe_root, directory, now) {
            return Ok((key, None));
        }
        crate::agy::forget_identity(probe_root, directory);
        let stamp_before = crate::agy::keychain_stamp(directory);
        let (key, models) = detect_agy(
            &guarded,
            executable,
            probe_home.path(),
            user,
            cwd,
            probe_root,
        )?;
        crate::agy::remember_identity(probe_root, directory, &key, now, stamp_before, &models);
        return Ok((key, None));
    }
    if probe == AccountIdentityProbe::AuthJson {
        let bytes = read_following(
            &directory.join("auth.json"),
            ReadOptions { limit: 1024 * 1024 },
        )
        .map_err(credential_identity_read_error)?;
        return crate::identity::codex_account(&bytes)
            .map(|key| (key, None))
            .ok_or(ProviderError::UnknownIdentity);
    }
    if probe == AccountIdentityProbe::Vscdb {
        let identity = crate::cursor::read_identity(directory).map_err(|error| match error {
            crate::cursor::CursorIdentityError::CredentialNotFound => {
                ProviderError::CredentialNotFound
            }
            crate::cursor::CursorIdentityError::Unavailable
            | crate::cursor::CursorIdentityError::Malformed => ProviderError::UnknownIdentity,
        })?;
        return Ok((identity.account_key, None));
    }
    if probe != AccountIdentityProbe::CliAuthStatus
        || spec
            .and_then(|spec| spec.install)
            .is_some_and(|install| directory == home.join(install.home))
    {
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
    options.stderr = StreamPolicy::Capture { limit: 65536 };
    options.stop_on_output_limit = true;
    let out = executor.execute(options).map_err(exec_error)?;
    if out.stdout.truncated
        || out.stdout.read_failed
        || out.stderr.truncated
        || out.stderr.read_failed
        || out.cancelled
        || out.overflow_stopped
        || out.timed_out
        || out.drain_truncated
    {
        return Err(ProviderError::UnknownIdentity);
    }
    if !matches!(out.termination, Termination::Exit(_)) {
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
    if out.termination != Termination::Exit(0) {
        return Err(ProviderError::UnknownIdentity);
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
) -> Result<(String, Vec<String>), ProviderError> {
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
    options.stderr = StreamPolicy::Capture { limit: 65536 };
    options.stop_on_output_limit = true;
    let out = executor.execute(options).map_err(exec_error)?;
    if out.stdout.truncated
        || out.stdout.read_failed
        || out.stderr.truncated
        || out.stderr.read_failed
        || out.cancelled
        || out.timed_out
        || out.drain_truncated
        || out.overflow_stopped
    {
        return Err(ProviderError::UnknownIdentity);
    }
    if out.termination != Termination::Exit(0) {
        return Err(
            if crate::Adapter::failure_cause(&crate::agy::Agy, &out.stderr.bytes)
                == crate::FailureCause::AuthRequired
            {
                ProviderError::CredentialUnavailable
            } else {
                ProviderError::UnknownIdentity
            },
        );
    }
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout.bytes).map_err(|_| ProviderError::UnknownIdentity)?;
    if payload.get("status").and_then(serde_json::Value::as_str) != Some("SUCCESS") {
        return Err(ProviderError::LoginRequired);
    }
    let bytes = read_following(&log, ReadOptions { limit: 1024 * 1024 })
        .map_err(|_| ProviderError::UnknownIdentity)?;
    let key = crate::identity::agy_fingerprint(&bytes)
        .map(|fingerprint| {
            provenant_records::Harness::Agy
                .spec()
                .account_key(&fingerprint)
        })
        .ok_or(ProviderError::UnknownIdentity)?;
    let stderr_str = std::str::from_utf8(&out.stderr.bytes).ok();
    let models = crate::quota::agy::extract_agy_models(Some(&payload), stderr_str);
    Ok((key, models))
}

#[cfg(test)]
mod resolver_tests {
    use super::*;

    fn registration(adapter: &str, label: &str, key: &str, directory: &str) -> Registration {
        Registration {
            adapter: adapter.into(),
            label: label.into(),
            key: key.into(),
            org_fingerprint: None,
            directory: directory.into(),
            plan: None,
            priority: None,
            billing_anchor: None,
            plans: Vec::new(),
            directories: Vec::new(),
            limits: None,
            display_name: None,
            hidden: false,
            order: None,
            headline: None,
            settings: Default::default(),
        }
    }

    fn settings() -> Settings {
        let mut settings = Settings::default();
        settings.entries.insert(
            "work-id".into(),
            registration("claude", "work", "key-work", "/fixture/claude-work"),
        );
        settings.entries.insert(
            "home-id".into(),
            registration("codex", "home", "key-home", "/fixture/codex-home"),
        );
        settings
    }

    fn work_identity() -> AccountIdentity {
        AccountIdentity {
            adapter: "claude".into(),
            key: "key-work".into(),
            entry_id: "work-id".into(),
            label: "work".into(),
            local_login: false,
        }
    }

    // Spec: docs/specs/routing.md#account-references
    #[test]
    fn every_accepted_form_resolves_to_one_typed_identity() {
        let settings = settings();
        for reference in ["work-id", "work", "key-work", "claude/key-work"] {
            let resolved = resolve(&settings, reference, ReferenceForms::ROUTING)
                .unwrap_or_else(|error| panic!("{reference}: {error}"));
            assert_eq!(resolved.item, work_identity(), "{reference}");
            assert_eq!(resolved.item.qualified(), "claude/key-work");
        }
        for reference in ["work-id", "work"] {
            assert_eq!(
                resolve(&settings, reference, ReferenceForms::REGISTRY)
                    .unwrap()
                    .item,
                work_identity(),
                "{reference}"
            );
        }
        for reference in ["key-work", "work"] {
            assert_eq!(
                resolve(&settings, reference, ReferenceForms::SELECTION)
                    .unwrap()
                    .item,
                work_identity(),
                "{reference}"
            );
        }
        assert_eq!(
            resolve(&settings, "key-work", ReferenceForms::RETAINED)
                .unwrap()
                .item,
            work_identity()
        );
    }

    // Spec: docs/specs/routing.md#account-references
    #[test]
    fn a_missing_reference_lists_close_matching_labels_only() {
        let settings = settings();
        let refusal = resolve(&settings, "wokk", ReferenceForms::ROUTING).expect_err("miss");
        assert_eq!(refusal.kind, NameKind("account"));
        assert_eq!(refusal.query, "wokk");
        assert!(
            refusal.candidates.contains(&"work".to_owned()),
            "{refusal:?}"
        );
        for candidate in &refusal.candidates {
            assert!(candidate == "work" || candidate == "home", "{candidate}");
        }
        assert!(!refusal.to_string().contains("key-"), "{refusal}");
    }

    // Spec: docs/specs/routing.md#account-references
    #[test]
    fn a_shared_label_or_key_is_ambiguous_in_every_context() {
        let mut settings = settings();
        settings.entries.insert(
            "other-id".into(),
            registration("codex", "work", "key-work", "/fixture/codex-other"),
        );
        for forms in [
            ReferenceForms::REGISTRY,
            ReferenceForms::ROUTING,
            ReferenceForms::SELECTION,
        ] {
            let refusal = resolve(&settings, "work", forms).expect_err("shared label");
            assert_eq!(refusal.candidates, ["work".to_owned()], "{forms:?}");
            assert_eq!(
                ambiguous_labels(&settings, "work", forms),
                refusal.candidates
            );
        }
        let refusal =
            resolve(&settings, "key-work", ReferenceForms::ROUTING).expect_err("shared key");
        assert_eq!(refusal.candidates, ["work".to_owned()]);
    }

    // Spec: docs/specs/routing.md#account-references
    #[test]
    fn a_case_folded_reference_carries_the_fold_cue() {
        let settings = settings();
        let label = resolve(&settings, "WORK", ReferenceForms::ROUTING).expect("folded label");
        assert_eq!(label.item.label, "work");
        assert_eq!(label.fold, Fold::CaseFolded);
        let qualified = resolve(&settings, "CLAUDE/KEY-WORK", ReferenceForms::ROUTING)
            .expect("folded qualified reference");
        assert_eq!(qualified.item, work_identity());
        assert_eq!(qualified.fold, Fold::CaseFolded);
    }

    // Spec: docs/specs/routing.md#account-references
    #[test]
    fn a_local_login_answers_only_to_its_harness_qualified_reference() {
        let settings = Settings::default();
        let local = resolve(&settings, "agy/local", ReferenceForms::ROUTING).expect("local login");
        assert_eq!(local.item.qualified(), "agy/local");
        assert!(local.item.local_login);
        assert!(resolve(&settings, "local", ReferenceForms::ROUTING).is_err());
    }
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
    fn account_configuration_accepts_a_registry_only_adapter() {
        let text = ENTRY
            .replace("adapter='claude'", "adapter='fixture-provider'")
            .replace("/fixture/claude-work", "/fixture/accounts/work");
        let config =
            provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                .unwrap();
        let entry = &config.domains.accounts.entries["work"];
        let account = entry.registered().unwrap();
        assert_eq!(account.adapter(), "fixture-provider");
        assert_eq!(account.key(), "work-key");
        let invalid = ENTRY.replace("adapter='claude'", "adapter='../fixture'");
        assert!(
            provenant_config::load_with_domains(&roots(), Some(invalid.as_bytes()), &SECTIONS)
                .is_err()
        );
    }

    #[test]
    fn unknown_account_config_keys_warn_with_source_and_keep_known_fields() {
        let cases = [
            (
                "accounts settings",
                ENTRY.replace(
                    "schema_version=1\n",
                    "schema_version=1\n[domains.accounts]\nfuture_setting=true\n",
                ),
            ),
            ("registration", format!("{ENTRY}future_registration=true\n")),
            (
                "shared settings",
                format!("{ENTRY}[domains.accounts.shared_settings]\nfuture_setting=true\n"),
            ),
            (
                "limits",
                format!("{ENTRY}limits={{monthly_usd=42.0,future_limit=true}}\n"),
            ),
        ];
        for (shape, text) in cases {
            let config =
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .unwrap();
            assert_eq!(
                config.domains.accounts.entries["work"].key, "work-key",
                "{shape}"
            );
            let unknown: Vec<_> = config
                .warnings
                .iter()
                .filter(|warning| warning.rule == "unknown_key")
                .collect();
            assert_eq!(unknown.len(), 1, "{shape}");
            assert!(unknown[0].line > 0, "{shape} warning has a source");
        }
    }

    #[test]
    fn malformed_optional_metadata_warns_and_preserves_registration() {
        let cases = [
            ("display_name", "display_name=''", "work"),
            ("display_name", "display_name=42", "work"),
            ("display_name", "display_name={}", "work"),
            ("order", "order=0", "work"),
            ("order", "order='first'", "work"),
            (
                "plans",
                "plans=[{name='plan',price=-1,currency='USD',effective_from='2026-01-01'}]",
                "work",
            ),
            ("plans", "plans=false", "work"),
            ("limits", "limits=42", "work"),
            ("priority", "priority=[]", "work"),
            ("display_name", "display_name=''", "'work.alias'"),
        ];
        for (field, invalid, account_id) in cases {
            let entry_path = format!("entries.{account_id}");
            let mut text = ENTRY.replace("entries.work", &entry_path);
            // The invalid line may itself be the priority key; TOML forbids
            // duplicate keys, so the valid priority is only set otherwise.
            if field != "priority" {
                text.push_str("priority=2\n");
            }
            text.push_str("hidden=true\n");
            text.push_str(invalid);
            text.push('\n');
            if account_id == "'work.alias'" {
                text.push_str(
                    "[domains.accounts.entries.home]\nadapter='codex'\nlabel='home'\nkey='home-key'\ndirectory='/fixture/codex-home'\n",
                );
            }
            let config =
                provenant_config::load_with_domains(&roots(), Some(text.as_bytes()), &SECTIONS)
                    .unwrap();
            let account_key = account_id.trim_matches('\'');
            let entry = &config.domains.accounts.entries[account_key];
            assert_eq!(entry.key, "work-key", "{field}");
            let expected_priority = (field != "priority").then_some(2);
            assert_eq!(entry.priority, expected_priority, "{field}");
            assert!(entry.hidden, "{field}");
            assert!(
                config.warnings.iter().any(|warning| warning.line > 0),
                "invalid {field} should report its source"
            );
            if field == "display_name" {
                assert!(entry.display_name.is_none(), "{field}");
            }
            if field == "order" {
                assert!(entry.order.is_none(), "{field}");
            }
            if field == "plans" {
                assert!(entry.plans.is_empty(), "{field}");
            }
            if field == "limits" {
                assert!(entry.limits.is_none(), "{field}");
            }
            if account_id == "'work.alias'" {
                assert_eq!(config.domains.accounts.entries["home"].key, "home-key");
            }
            if field != "priority" {
                let priority_origin = format!("domains.accounts.entries.{account_key}.priority");
                assert!(matches!(
                    config.origins.get(&priority_origin),
                    Some(Origin::User { .. })
                ));
            }
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
            detail: None,
            field: "entries.work.alias.directory".into(),
            code: "type"
        }));
        assert!(!(SECTION.boundary)(&SectionError {
            detail: None,
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
}

#[cfg(test)]
mod account_pool_tests {
    use super::*;
    #[test]
    fn registered_plans_enumerate_once_without_inventing_identities() {
        let spec = provenant_records::AdapterSpec::get("opencode").unwrap();
        let mut settings = Settings::default();
        for (index, plan) in spec.account_model_plans.iter().enumerate() {
            let entry: Registration = serde_json::from_value(serde_json::json!({
                "adapter":spec.name, "label":plan.plan, "key":format!("identity-{index}"),
                "directory":format!("/fixture/{index}"), "plan":plan.plan
            }))
            .unwrap();
            settings
                .entries
                .insert(format!("entry-{index}"), entry.clone());
            settings.entries.insert(format!("alias-{index}"), entry);
            let effective = settings.unique_accounts();
            let identities: std::collections::BTreeSet<_> = settings
                .entries
                .values()
                .map(|entry| (&entry.adapter, &entry.key))
                .collect();
            assert_eq!(effective.entries.len(), identities.len());
            for account in effective.entries.values() {
                let plan = spec
                    .account_model_plans
                    .iter()
                    .find(|plan| Some(plan.plan) == account.plan.as_deref())
                    .unwrap();
                assert!(spec.account_serves_model(
                    account.plan.as_deref(),
                    &format!("{}fixture", plan.model_prefix)
                ));
            }
        }
    }
}
