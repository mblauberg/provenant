//! Bounded, append-only configuration history: one JSON line per applied
//! edit in `<state>/config-history.jsonl`, written under the config lock.
//! Values that look like secrets are never recorded.
use crate::ConfigError;
use jiff::Timestamp;
use provenant_os::fs::{Directory, PublishMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, ffi::OsStr};

const FILE: &str = "config-history.jsonl";
const REDACTED: &str = "[redacted]";
const MAX_CHANGES: usize = 64;
const MAX_VALUE_CHARS: usize = 256;
const READ_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldChange {
    pub path: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// RFC3339 time the edit was committed.
    pub time: String,
    pub actor: String,
    pub channel: String,
    pub changes: Vec<FieldChange>,
    pub before_revision: String,
    pub after_revision: String,
}

/// Retention by age, count and size; the registry knobs `config.history_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    pub max_age_days: u64,
    pub max_entries: usize,
    pub max_bytes: usize,
}
impl HistoryLimits {
    pub(crate) fn from_knobs(knobs: &BTreeMap<String, crate::KnobValue>) -> Self {
        let get = |name: &str, fallback: u64| {
            knobs
                .get(name)
                .and_then(|knob| knob.value.as_u64())
                .unwrap_or(fallback)
        };
        Self {
            max_age_days: get("config.history_max_age_days", 365),
            max_entries: get("config.history_max_entries", 500) as usize,
            max_bytes: get("config.history_max_bytes", 262_144) as usize,
        }
    }
}

/// Fields that differ between two config documents, secrets redacted.
pub(crate) fn changes(before: &str, after: &str) -> Vec<FieldChange> {
    let flatten = |text: &str| {
        let mut leaves = BTreeMap::new();
        if let Ok(value) = toml_edit::de::from_str::<Value>(text) {
            leaves_of(&value, String::new(), &mut leaves);
        }
        leaves
    };
    let (old, new) = (flatten(before), flatten(after));
    let mut out = Vec::new();
    for path in old
        .keys()
        .chain(new.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let (was, is) = (old.get(path), new.get(path));
        if was != is {
            out.push(FieldChange {
                path: path.clone(),
                before: was.map(|v| record(path, v)),
                after: is.map(|v| record(path, v)),
            });
        }
        if out.len() == MAX_CHANGES {
            break;
        }
    }
    out
}
fn leaves_of(value: &Value, path: String, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                leaves_of(value, child, out);
            }
        }
        _ => {
            out.insert(path, value.clone());
        }
    }
}

fn record(path: &str, value: &Value) -> Value {
    if secret_path(path) || secret_value(value) {
        return Value::from(REDACTED);
    }
    match value {
        Value::String(text) if text.chars().count() > MAX_VALUE_CHARS => {
            Value::from(text.chars().take(MAX_VALUE_CHARS).collect::<String>() + "...")
        }
        Value::Array(_) if value.to_string().len() > MAX_VALUE_CHARS => Value::from("[large]"),
        _ => value.clone(),
    }
}
fn secret_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "api_key",
        "apikey",
        "private_key",
        "bearer",
    ]
    .iter()
    .any(|word| path.contains(word))
}
fn secret_value(value: &Value) -> bool {
    match value {
        Value::String(text) => secret_text(text),
        Value::Array(items) => items.iter().any(secret_value),
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| secret_path(key) || secret_value(value)),
        _ => false,
    }
}
fn secret_text(text: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "sk-",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "Bearer ",
    ];
    if text.contains("-----BEGIN") || PREFIXES.iter().any(|prefix| text.starts_with(prefix)) {
        return true;
    }
    // A long unbroken run of mixed letters and digits is a key, not a word.
    text.len() >= 32
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b))
        && text.bytes().any(|b| b.is_ascii_digit())
        && text.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Drop entries past the age, then the oldest past the count and size bounds.
/// `lines` are oldest first; the newest entry is always kept.
pub(crate) fn prune(lines: Vec<String>, limits: HistoryLimits, now: Timestamp) -> Vec<String> {
    let age = i64::try_from(limits.max_age_days.saturating_mul(86_400)).unwrap_or(i64::MAX);
    let cutoff = now.as_second().saturating_sub(age);
    let mut kept: Vec<String> = lines
        .into_iter()
        .filter(|line| {
            serde_json::from_str::<HistoryEntry>(line)
                .ok()
                .and_then(|entry| entry.time.parse::<Timestamp>().ok())
                .is_some_and(|time| time.as_second() >= cutoff)
        })
        .collect();
    let excess = kept.len().saturating_sub(limits.max_entries.max(1));
    kept.drain(..excess);
    let mut size: usize = kept.iter().map(|line| line.len() + 1).sum();
    let mut drop = 0;
    while size > limits.max_bytes && drop + 1 < kept.len() {
        size -= kept[drop].len() + 1;
        drop += 1;
    }
    kept.drain(..drop);
    kept
}

fn read_lines(state: &Directory) -> Result<Vec<String>, ConfigError> {
    match state.read_bounded(OsStr::new(FILE), READ_LIMIT) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(error) if error.is_not_found() => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Append one entry and prune, rewriting the bounded file atomically. Called
/// with the config lock held.
pub(crate) fn append(
    state: &Directory,
    entry: &HistoryEntry,
    limits: HistoryLimits,
    now: Timestamp,
) -> Result<(), ConfigError> {
    let mut lines = read_lines(state)?;
    lines.push(serde_json::to_string(entry).map_err(|_| ConfigError::Edit {
        reason: "history entry encoding",
    })?);
    let mut text = prune(lines, limits, now).join("\n");
    text.push('\n');
    state
        .atomic_write_with_permissions(
            OsStr::new(FILE),
            ".config-history-",
            text.as_bytes(),
            0o600,
            PublishMode::Replace,
        )?
        .require_durable()?;
    Ok(())
}

/// Entries newest first, at or after `since`, at most `limit`.
pub fn history(
    roots: &provenant_core::Roots,
    since: Option<Timestamp>,
    limit: usize,
) -> Result<Vec<HistoryEntry>, ConfigError> {
    let state = match Directory::open_anchor(roots.state.as_path()) {
        Ok(state) => state,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(read_lines(&state)?
        .iter()
        .rev()
        .filter_map(|line| serde_json::from_str::<HistoryEntry>(line).ok())
        .filter(|entry| {
            since.is_none_or(|since| entry.time.parse::<Timestamp>().is_ok_and(|t| t >= since))
        })
        .take(limit)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(time: &str) -> String {
        serde_json::to_string(&HistoryEntry {
            time: time.into(),
            actor: "a".into(),
            channel: "cli".into(),
            changes: Vec::new(),
            before_revision: "x".into(),
            after_revision: "y".into(),
        })
        .unwrap()
    }
    fn limits(age: u64, entries: usize, bytes: usize) -> HistoryLimits {
        HistoryLimits {
            max_age_days: age,
            max_entries: entries,
            max_bytes: bytes,
        }
    }

    #[test]
    fn pruning_applies_age_count_and_size_bounds() {
        let now: Timestamp = "2026-06-10T00:00:00Z".parse().unwrap();
        let lines = vec![
            entry("2026-01-01T00:00:00Z"),
            entry("2026-06-01T00:00:00Z"),
            entry("2026-06-02T00:00:00Z"),
            entry("2026-06-03T00:00:00Z"),
        ];
        assert_eq!(
            prune(lines.clone(), limits(30, 100, 1 << 20), now),
            lines[1..]
        );
        assert_eq!(
            prune(lines.clone(), limits(365, 2, 1 << 20), now),
            lines[2..]
        );
        let one = lines[3].len() + 1;
        assert_eq!(
            prune(lines.clone(), limits(365, 100, one * 2), now),
            lines[2..]
        );
        // The newest entry survives a bound smaller than itself.
        assert_eq!(prune(lines.clone(), limits(365, 100, 1), now), lines[3..]);
    }

    #[test]
    fn secret_looking_values_are_redacted() {
        let found = changes(
            "schema_version = 1\n",
            "schema_version = 1\n[x]\napi_token = \"abc\"\nnote = \"ghp_0123456789abcdef\"\nplain = \"fine\"\nlist = [\"sk-live\"]\n",
        );
        let by_path: BTreeMap<_, _> = found.iter().map(|c| (c.path.as_str(), c)).collect();
        for path in ["x.api_token", "x.note", "x.list"] {
            let after = by_path[path].after.clone().unwrap();
            assert!(after == REDACTED, "{path}: {after}");
        }
        assert_eq!(by_path["x.plain"].after, Some(Value::from("fine")));
        assert!(!serde_json::to_string(&found).unwrap().contains("ghp_"));
    }

    #[test]
    fn secrets_inside_object_arrays_are_redacted() {
        let found = changes(
            "schema_version = 1\n",
            "schema_version = 1\n[domains.routing.review.shapes.deep]\nreviewers = [{ pool = \"junior\", lenses = [\"ghp_fake1234567890\"] }, { token = \"plain\" }]\n",
        );
        let by_path: BTreeMap<_, _> = found.iter().map(|c| (c.path.as_str(), c)).collect();
        let after = by_path["domains.routing.review.shapes.deep.reviewers"]
            .after
            .clone()
            .unwrap();
        assert_eq!(after, REDACTED);
        assert!(!serde_json::to_string(&found).unwrap().contains("ghp_"));
        assert!(!serde_json::to_string(&found).unwrap().contains("plain"));
    }
}
