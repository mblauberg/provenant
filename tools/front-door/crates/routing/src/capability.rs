//! Validated runtime capability snapshots. Source trust and age are explicit policy.
use crate::live::parse_timestamp;
use crate::{
    casefold::fold,
    catalog,
    ordered_json::{OrderedObject, OrderedValue},
    pending::{self, JsonError},
    probe::load_json,
};
use serde_json::Value;
use std::path::Path;
use thiserror::Error;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, Error)]
pub enum CapabilityFailure {
    #[serde(rename = "capability_discovery_failed")]
    #[error("capability_discovery_failed")]
    DiscoveryFailed,
    #[serde(rename = "capability_snapshot_untrusted")]
    #[error("capability_snapshot_untrusted")]
    Untrusted,
    #[serde(rename = "capability_snapshot_stale")]
    #[error("capability_snapshot_stale")]
    Stale,
}
pub type CapabilityResult = (OrderedObject, Option<CapabilityFailure>);
fn refused(reason: CapabilityFailure) -> CapabilityResult {
    (OrderedObject::default(), Some(reason))
}

/// Like Python's optional path: no path is an empty successful observation.
pub fn load_capabilities_path(
    path: Option<&Path>,
    adapter: &str,
    catalog: &Value,
    now: jiff::Timestamp,
) -> Result<CapabilityResult, JsonError> {
    let Some(path) = path.filter(|p| !p.as_os_str().is_empty()) else {
        return Ok((OrderedObject::default(), None));
    };
    match std::fs::read_to_string(path) {
        Ok(raw) => load_capabilities(&raw, adapter, catalog, now),
        Err(_) => Ok(refused(CapabilityFailure::DiscoveryFailed)),
    }
}
pub fn load_capabilities(
    raw: &str,
    adapter: &str,
    catalog: &Value,
    now: jiff::Timestamp,
) -> Result<CapabilityResult, JsonError> {
    use CapabilityFailure::*;
    let data = match load_json(raw) {
        Ok(v) => v,
        Err(error @ JsonError::NumberOutOfRange(_)) => return Err(error),
        Err(_) => return Ok(refused(DiscoveryFailed)),
    };
    let Some(object) = data.as_object() else {
        return Ok(refused(DiscoveryFailed));
    };
    let version = object.get("schema_version").map(OrderedValue::to_value);
    if !pending::python_one(version.as_ref())
        || object
            .get("models")
            .and_then(OrderedValue::as_object)
            .is_none()
    {
        return Ok(refused(DiscoveryFailed));
    }
    let source = object.get("source").and_then(OrderedValue::as_str);
    let trusted = match source {
        Some("codex debug models") => Some("codex"),
        Some("claude subscription canary") => Some("claude"),
        Some("agy models") => Some("agy"),
        _ => None,
    };
    if trusted != Some(adapter) {
        return Ok(refused(Untrusted));
    }
    if adapter == "claude" {
        let provenance = data.get("provenance");
        if provenance
            .and_then(|p| p.get("kind"))
            .and_then(OrderedValue::as_str)
            != Some("subscription_runtime_canary")
            || provenance
                .and_then(|p| p.get("auth_method"))
                .and_then(OrderedValue::as_str)
                != Some("claude.ai")
            || provenance
                .and_then(|p| p.get("subscription_type"))
                .and_then(OrderedValue::as_str)
                .is_none_or(str::is_empty)
        {
            return Ok(refused(Untrusted));
        }
    }
    let Some(observed) = data
        .get("observed_at")
        .and_then(OrderedValue::as_str)
        .and_then(parse_timestamp)
    else {
        return Ok(refused(Untrusted));
    };
    let age = now.as_nanosecond().div_euclid(1000) - observed.as_nanosecond().div_euclid(1000);
    if !(-60_000_000..=300_000_000).contains(&age) {
        return Ok(refused(Stale));
    }
    let Some(models) = data
        .get("models")
        .and_then(OrderedValue::as_object)
        .filter(|m| !m.0.is_empty())
    else {
        return Ok(refused(DiscoveryFailed));
    };
    let family = catalog
        .get("adapters")
        .and_then(|a| a.get(adapter))
        .and_then(|a| a.get("fixed_model_family"))
        .and_then(Value::as_str);
    let aliases = family
        .map(|f| {
            catalog::family_alias_candidates(
                catalog
                    .get("families")
                    .and_then(|fs| fs.get(f))
                    .unwrap_or(&Value::Null),
                catalog,
                f,
            )
        })
        .unwrap_or_default()
        .into_iter()
        .map(|s| fold(&s))
        .collect::<std::collections::BTreeSet<_>>();
    let mut keys = std::collections::BTreeSet::new();
    let mut normalized = OrderedObject::default();
    for (key, item) in &models.0 {
        let Some(entry) = item.as_object() else {
            return Ok(refused(DiscoveryFailed));
        };
        let k = fold(key);
        if pending::python_strip(key).is_empty() || keys.contains(&k) {
            return Ok(refused(DiscoveryFailed));
        }
        let Some(resolved) = entry
            .get("resolved_model")
            .and_then(OrderedValue::as_str)
            .filter(|s| !pending::python_strip(s).is_empty())
        else {
            return Ok(refused(DiscoveryFailed));
        };
        if !catalog::capability_key_matches_model(adapter, &k, resolved, aliases.contains(&k)) {
            return Ok(refused(DiscoveryFailed));
        }
        if adapter == "claude" {
            if entry.get("supported_efforts").is_some()
                || entry
                    .get("requested_effort")
                    .and_then(OrderedValue::as_str)
                    .is_none_or(|s| pending::python_strip(s).is_empty())
                || entry.get("effort_verified") != Some(&OrderedValue::Bool(false))
            {
                return Ok(refused(DiscoveryFailed));
            }
        } else {
            match entry.get("supported_efforts") {
                Some(OrderedValue::Array(efforts))
                    if !efforts.is_empty()
                        && efforts.iter().all(|e| {
                            e.as_str()
                                .is_some_and(|s| !pending::python_strip(s).is_empty())
                        }) => {}
                _ => return Ok(refused(DiscoveryFailed)),
            }
        }
        let r = fold(resolved);
        if keys.contains(&r) {
            return Ok(refused(DiscoveryFailed));
        }
        keys.insert(k.clone());
        keys.insert(r.clone());
        normalized.insert(r, item.clone());
        normalized.insert(k, item.clone());
    }
    Ok((normalized, None))
}

#[cfg(test)]
mod tests;
