//! Typed snapshots and boundaries for live model discovery.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub(crate) fn parse_timestamp(raw: &str) -> Option<jiff::Timestamp> {
    provenant_core::py::python_iso_timestamp(&raw.replace('Z', "+00:00"))?
        .to_jiff()
        .ok()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveModel {
    pub id: String,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default, rename = "default")]
    pub is_default: bool,
    #[serde(default)]
    pub inherits: Vec<String>,
}

/// Cache freshness matches model_route_live._cached_live_models.
pub const CAPABILITY_MAX_AGE_SECONDS: i64 = 86400;
pub const OPENCODE_DISCOVERY_BUDGET: f64 = 12.0;
pub const OPENCODE_REFRESH_MIN_REMAINING: f64 = 4.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySnapshot {
    pub observed_at: String,
    #[serde(default)]
    pub models: Vec<String>,
}

impl CapabilitySnapshot {
    /// The caller supplies its clock; this record never reads wall time.
    /// Python's datetime has microsecond precision, including for ISO input.
    pub fn is_fresh(&self, now: jiff::Timestamp) -> bool {
        parse_timestamp(&self.observed_at).is_some_and(|observed| {
            let age =
                now.as_nanosecond().div_euclid(1000) - observed.as_nanosecond().div_euclid(1000);
            age <= i128::from(CAPABILITY_MAX_AGE_SECONDS) * 1_000_000
        })
    }
}

#[derive(Debug, Error)]
pub enum LiveProbeError {
    #[error("provider listing unavailable")]
    Unavailable,
    #[error("provider listing is malformed")]
    Malformed,
    #[error("provider listing exceeded its time budget")]
    DeadlineExceeded,
}

/// Provider discovery is an injected edge; route decisions stay synchronous and deterministic.
pub trait LiveModelSource {
    fn cached_snapshot(&self, adapter: &str) -> Option<CapabilitySnapshot>;
    fn probe(
        &self,
        adapter: &str,
        deadline: Option<f64>,
    ) -> Result<CapabilitySnapshot, LiveProbeError>;
}

/// Cache freshness matches Python's inclusive one-day boundary and permits future stamps.
pub fn cached_live_models(
    cache: &crate::ordered_json::OrderedValue,
    adapter: &str,
    now: jiff::Timestamp,
) -> Vec<String> {
    use crate::ordered_json::OrderedValue;
    let Some(entry) = cache.get(adapter) else {
        return vec![];
    };
    let Some(observed) = entry
        .get("observed_at")
        .and_then(OrderedValue::as_str)
        .and_then(parse_timestamp)
    else {
        return vec![];
    };
    if now.as_nanosecond().div_euclid(1000) - observed.as_nanosecond().div_euclid(1000)
        > 86_400_000_000
    {
        return vec![];
    }
    match entry.get("models") {
        Some(OrderedValue::Array(items)) => items
            .iter()
            .filter_map(OrderedValue::as_str)
            .map(str::to_owned)
            .collect(),
        Some(OrderedValue::String(text)) => text.chars().map(|c| c.to_string()).collect(),
        Some(OrderedValue::Object(map)) => map.0.iter().map(|(key, _)| key.clone()).collect(),
        _ => vec![],
    }
}

/// Normalize cooldown identities, retaining the lexically newest record and original position.
pub fn normalise_cooldowns(
    records: &crate::ordered_json::OrderedObject,
    resolver: &crate::matching::ModelCatalog<'_>,
) -> Result<crate::ordered_json::OrderedObject, crate::matching::MatchError> {
    use crate::ordered_json::{OrderedObject, OrderedValue};
    let mut normalized = OrderedObject::default();
    for (key, record) in &records.0 {
        let Some((adapter, model)) = key.split_once('/') else {
            continue;
        };
        let canonical = format!("{adapter}/{}", resolver.canonical_model(adapter, model)?);
        if record.as_object().is_none() {
            continue;
        }
        let string = |r: &OrderedValue| {
            r.get("cooling_until")
                .map(crate::pending::ordered_string)
                .unwrap_or_default()
        };
        if normalized
            .get(&canonical)
            .is_none_or(|old| old.as_object().is_none() || string(record) > string(old))
        {
            normalized.insert(canonical, record.clone());
        }
    }
    Ok(normalized)
}

pub fn cooling(
    adapter: &str,
    model: &str,
    cooldowns: &crate::ordered_json::OrderedObject,
    resolver: &crate::matching::ModelCatalog<'_>,
    now: jiff::Timestamp,
) -> Result<String, crate::matching::MatchError> {
    use crate::ordered_json::OrderedValue;
    let canonical = resolver.canonical_model(adapter, model)?;
    for key in [format!("{adapter}/{canonical}"), format!("{adapter}/*")] {
        let Some(item) = cooldowns.get(&key).filter(|v| v.as_object().is_some()) else {
            continue;
        };
        if let Some(covers) = item.get("covers").filter(|v| crate::pending::truthy(v)) {
            let prefixes: Vec<&str> = match covers {
                OrderedValue::Array(items) => {
                    let mut matched = false;
                    for item in items {
                        let prefix = item.as_str().ok_or(crate::matching::MatchError::Catalog(
                            "cooldown cover prefix",
                        ))?;
                        if canonical.starts_with(prefix) {
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        continue;
                    }
                    vec![]
                }
                OrderedValue::Object(map) => map.0.iter().map(|(key, _)| key.as_str()).collect(),
                OrderedValue::String(text) => {
                    if !text.chars().any(|c| canonical.starts_with(c)) {
                        continue;
                    }
                    vec![]
                }
                _ => return Err(crate::matching::MatchError::Catalog("cooldown covers")),
            };
            if matches!(covers, OrderedValue::Object(_))
                && !prefixes.iter().any(|prefix| canonical.starts_with(prefix))
            {
                continue;
            }
        }
        if let Some(until) = item.get("cooling_until") {
            let raw = crate::pending::ordered_string(until);
            if parse_timestamp(&raw).is_some_and(|stamp| stamp > now) {
                return Ok(raw);
            }
        }
    }
    Ok(String::new())
}

/// A malformed timestamp invalidates the whole advisory file, matching the Python reader.
pub fn recent_failed_task_class_models(
    document: &serde_json::Value,
    adapter: &str,
    task_class: &str,
    now: jiff::Timestamp,
) -> std::collections::BTreeSet<String> {
    use serde_json::Value;
    let mut failed = std::collections::BTreeSet::new();
    if document.get("schema").and_then(Value::as_str) != Some("fabric.route-health.v1") {
        return failed;
    }
    let Some(routes) = document.get("routes").and_then(Value::as_object) else {
        return failed;
    };
    for record in routes.values() {
        if !record.is_object()
            || record.get("adapter").and_then(Value::as_str) != Some(adapter)
            || record.get("task_class").and_then(Value::as_str) != Some(task_class)
        {
            continue;
        }
        let Some(latest) = record
            .get("recent")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .filter(|r| r.is_object())
        else {
            continue;
        };
        let raw = latest
            .get("at")
            .map(crate::pending::python_string)
            .unwrap_or_default();
        let Some(observed) = parse_timestamp(&raw) else {
            return std::collections::BTreeSet::new();
        };
        let age = now.as_nanosecond().div_euclid(1000) - observed.as_nanosecond().div_euclid(1000);
        if (0..=7 * 86_400_000_000).contains(&age) {
            let status = latest.get("status");
            if status.is_some_and(|s| s.is_object() || s.is_array()) {
                return std::collections::BTreeSet::new();
            }
            if status
                .and_then(Value::as_str)
                .is_some_and(|s| ["failed", "empty_output"].contains(&s))
            {
                if let Some(model) = record.get("model").and_then(Value::as_str) {
                    failed.insert(model.into());
                }
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::{CapabilitySnapshot, LiveModel};

    #[test]
    fn snapshots_read_the_python_capability_cache_shape() {
        let snapshot: CapabilitySnapshot = serde_json::from_str(
            r#"{"observed_at":"2026-10-23T00:00:00Z","models":["gpt-6.1-sol"]}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(snapshot).unwrap()["observed_at"],
            "2026-10-23T00:00:00Z"
        );
    }

    #[test]
    fn cached_capabilities_use_the_python_day_cutoff_with_an_explicit_clock() {
        let snapshot = CapabilitySnapshot {
            observed_at: "2026-10-23T00:00:00.000001Z".into(),
            models: vec!["gpt-6.1-sol".into()],
        };
        assert!(snapshot.is_fresh("2026-10-24T00:00:00.000001Z".parse().unwrap()));
        assert!(!snapshot.is_fresh("2026-10-24T00:00:00.000002Z".parse().unwrap()));
        assert!(snapshot.is_fresh("2026-10-22T00:00:00Z".parse().unwrap()));
        let bad = CapabilitySnapshot {
            observed_at: "invalid".into(),
            models: vec![],
        };
        assert!(!bad.is_fresh("2026-10-24T00:00:00Z".parse().unwrap()));
        let model = LiveModel {
            id: "gpt-6.2-sol".into(),
            inherits: vec!["gpt-6.1-sol".into()],
            ..Default::default()
        };
        assert_eq!(model.inherits, ["gpt-6.1-sol"]);
    }
}

#[cfg(test)]
mod advisory_tests {
    use super::*;
    use serde_json::{json, Value};
    #[test]
    fn cooldown_scope_and_lexical_identity_merge_match_python() {
        let catalog = json!({"adapters":{"codex":{"models":[{"id":"gpt-6-sol","names":["sol"]}]}}});
        let cached = std::collections::BTreeMap::new();
        let resolver = crate::matching::ModelCatalog {
            catalog: &catalog,
            cached: &cached,
        };
        let raw = r#"{"codex/sol":{"cooling_until":"2026-10-24T00:00:00Z"},"codex/gpt-6-sol":{"cooling_until":"2026-10-25T00:00:00Z"},"bad":{},"codex/*":{"cooling_until":"2026-10-26T00:00:00Z","covers":["other"]},"codex/bad":false}"#;
        let records: crate::ordered_json::OrderedObject = crate::pending::from_json(raw).unwrap();
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"
import json,pathlib,sys,types
from datetime import datetime,timezone
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route as m
d=json.load(sys.stdin)
class Clock(datetime):
 @classmethod
 def now(cls,tz=None):return cls(2026,10,23,tzinfo=timezone.utc)
m.model_route_live.datetime=Clock
m.model_route_live._fabric_records=lambda:types.SimpleNamespace(read_cooldowns=lambda:json.loads(d['raw']))
m.model_route_live._quota=False
records=m.model_route_live._cooldowns(d['catalog'])
print(json.dumps([records,m.model_route_live._cooling('codex','sol',records,d['catalog'])]))
"#,
            &json!({"raw":raw,"catalog":catalog}),
        ) else {
            return;
        };
        let normalized = normalise_cooldowns(&records, &resolver).unwrap();
        let until = cooling(
            "codex",
            "sol",
            &normalized,
            &resolver,
            "2026-10-23T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(json!([normalized, until]), expected);
    }
}
