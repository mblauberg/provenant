//! User settings shared across Claude account directories.
//!
//! Only allow-listed top-level keys of `settings.json` are kept in step. Each
//! synced key is recorded in the install receipt with the value last written, so
//! a value edited in one account since then is reported, never overwritten.
//! Identity and credential keys are refused outright, whatever the allow-list
//! says. Hooks stay with the managed-hook mechanism.
use crate::{InstallError, managed::SelectorChange, splice};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

/// Keys synced when `keys` is not configured.
pub const DEFAULT_KEYS: [&str; 7] = [
    "permissions",
    "env",
    "statusLine",
    "enabledPlugins",
    "outputStyle",
    "includeCoAuthoredBy",
    "cleanupPeriodDays",
];

/// Selector prefix for a settings key recorded in the receipt.
pub(crate) const PREFIX: &str = "setting:";

const DENIED: [&str; 6] = [
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "forceLoginMethod",
    "forceLoginOrgUUID",
    "otelHeadersHelper",
];
const DENIED_WORDS: [&str; 11] = [
    "auth",
    "authentication",
    "authorization",
    "authorisation",
    "credential",
    "credentials",
    "token",
    "tokens",
    "oauth",
    "secret",
    "password",
];

/// Words of a camelCase, snake_case or kebab-case name, lower-cased.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether `name` identifies the user or carries a credential. Whole words
/// only, so `includeCoAuthoredBy` is a setting but `awsAuthRefresh` is not.
pub fn denied_key(name: &str) -> bool {
    if DENIED
        .iter()
        .any(|denied| denied.eq_ignore_ascii_case(name))
    {
        return true;
    }
    let words = words(name);
    words
        .iter()
        .any(|word| DENIED_WORDS.contains(&word.as_str()))
        || words.iter().any(|word| word == "apikey")
        || words.windows(2).any(|pair| {
            matches!(
                (pair[0].as_str(), pair[1].as_str()),
                ("api" | "secret" | "access" | "private", "key")
            )
        })
}

/// What may never be synced. `Err` names the offending key, never a value.
pub fn check_key(key: &str) -> Result<(), String> {
    if key == "hooks" {
        return Err(
            "`hooks` is managed by `provenant install --hooks`, not shared settings".into(),
        );
    }
    if denied_key(key) {
        return Err(format!(
            "`{key}` carries identity or credentials and is never synced"
        ));
    }
    Ok(())
}

/// Refuses an allow-list or a value tree that would sync identity or credentials.
/// Environment variable names inside `env` are held to the same rule.
pub fn check_value(key: &str, value: &Value) -> Result<(), String> {
    if key == "env"
        && let Some(env) = value.as_object()
        && let Some(name) = env.keys().find(|name| denied_key(name))
    {
        return Err(format!(
            "`env.{name}` carries identity or credentials and is never synced"
        ));
    }
    Ok(())
}

/// Best-effort canonical form, so a symlinked or relative spelling still matches.
pub fn canonical(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if parent != path => canonical(parent).join(name),
        _ => path.to_path_buf(),
    })
}

/// Names the account and key of a finding: `account work key permissions`.
pub(crate) fn detail(
    accounts: &[crate::managed::AccountHome],
    home: Option<&std::path::Path>,
    key: &str,
) -> String {
    let home = home.map(canonical);
    let label = accounts
        .iter()
        .find(|account| home.as_deref() == Some(canonical(&account.home).as_path()))
        .map_or("default", |account| account.label.as_str());
    format!("account {label} key {key}")
}

/// Resolved by the process edge from `[domains.accounts.shared_settings]`.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedSettings {
    /// The settings file the values come from. The account that owns it is
    /// the source and is never rewritten.
    pub source_file: PathBuf,
    /// Allow-listed keys present in the source.
    pub source: BTreeMap<String, Value>,
    /// The allow-list.
    pub keys: Vec<String>,
    /// Per-account values that win over the source, by account directory.
    pub overrides: BTreeMap<PathBuf, BTreeMap<String, Value>>,
}

impl SharedSettings {
    /// The value `home` should hold for each key it is to have.
    pub(crate) fn desired(&self, home: Option<&std::path::Path>) -> BTreeMap<String, Value> {
        let mut desired: BTreeMap<_, _> = self
            .source
            .iter()
            .filter(|(key, _)| self.keys.contains(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if let Some(overrides) = home.and_then(|home| self.overrides.get(home)) {
            for (key, value) in overrides {
                if self.keys.contains(key) {
                    desired.insert(key.clone(), value.clone());
                }
            }
        }
        desired
    }
}

#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub bytes: Vec<u8>,
    pub changes: Vec<SelectorChange>,
    /// Each key the account now owns with the value last synced.
    pub owned: Vec<(String, Value)>,
    /// Keys whose local value differs from what was last synced.
    pub conflicts: Vec<String>,
}

/// Reconciles `bytes` with the `desired` values. `previous` is what the
/// receipt recorded as last synced. A value edited since, or one the account
/// held before sync, is a conflict unless `force`. A key the account removed
/// itself stays removed; one the source dropped is removed if unedited.
pub(crate) fn plan(
    bytes: &[u8],
    keys: &[String],
    desired: &BTreeMap<String, Value>,
    previous: &BTreeMap<String, Value>,
    force: bool,
) -> Result<Plan, InstallError> {
    let mut plan = Plan {
        bytes: bytes.to_vec(),
        ..Plan::default()
    };
    for key in keys.iter().collect::<BTreeSet<_>>() {
        let current = splice::get(&plan.bytes, &[key.as_str()])?;
        let last = previous.get(key);
        let (value, replace) = match (desired.get(key), &current) {
            (Some(want), Some(have)) if have == want => (Some(want), false),
            (Some(want), None) if last.is_none() || force => (Some(want), true),
            (Some(_), None) => continue,
            (Some(want), Some(have)) if force || last == Some(have) => (Some(want), true),
            (Some(_), Some(_)) => {
                plan.conflicts.push(key.clone());
                if let Some(last) = last {
                    plan.owned.push((key.clone(), last.clone()));
                }
                continue;
            }
            (None, Some(have)) if last == Some(have) => (None, true),
            (None, _) => continue,
        };
        if replace {
            plan.bytes = splice::set(&plan.bytes, &[key.as_str()], value)?;
            plan.changes.push(SelectorChange {
                selector: format!("{PREFIX}{key}"),
                before: current,
                after: value.cloned(),
            });
        }
        if let Some(value) = value {
            plan.owned.push((key.clone(), value.clone()));
        }
    }
    Ok(plan)
}

/// Copies from `account` into `source` each allow-listed key the account
/// holds that differs from the source and that it was not given by sync (so it
/// is a local edit), leaving the rest of `source` byte for byte. Returns the new
/// source bytes and the keys adopted.
pub fn adopt(
    source: &[u8],
    account: &[u8],
    keys: &[String],
    synced: &BTreeMap<String, Value>,
    overridden: &BTreeSet<String>,
) -> Result<(Vec<u8>, Vec<String>), InstallError> {
    let mut out = source.to_vec();
    let mut adopted = Vec::new();
    for key in keys.iter().collect::<BTreeSet<_>>() {
        if check_key(key).is_err() || overridden.contains(key) {
            continue;
        }
        let Some(value) = splice::get(account, &[key.as_str()])? else {
            continue;
        };
        if check_value(key, &value).is_err()
            || synced.get(key) == Some(&value)
            || splice::get(&out, &[key.as_str()])?.as_ref() == Some(&value)
        {
            continue;
        }
        out = splice::set(&out, &[key.as_str()], Some(&value))?;
        adopted.push(key.clone());
    }
    Ok((out, adopted))
}
