//! Validation, migration and model-name helpers for weighted route pools.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;

use serde_json::{Map, Value};
use thiserror::Error;

const WEIGHTS: [(&str, f64); 4] = [
    ("high", 8.0),
    ("normal", 4.0),
    ("sparing", 1.0),
    ("off", 0.0),
];
const RENAMED_TIERS: [(&str, &str); 6] = [
    ("scout", "junior"),
    ("bulk", "junior"),
    ("workhorse", "senior"),
    ("strong", "senior"),
    ("review", "senior"),
    ("flagship", "principal"),
];
const TIER_ROUTES: [&str; 4] = ["utility", "junior", "senior", "principal"];

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{fix}")]
pub struct PoolError {
    pub code: String,
    pub fix: String,
}

impl PoolError {
    pub fn new(code: &str, fix: &str) -> Self {
        Self {
            code: code.into(),
            fix: fix.into(),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{0}")]
pub struct RenamedNameError(pub String);

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{0}")]
pub struct MigrationConflict(pub String);

pub fn weight_value(raw: &Value) -> Option<f64> {
    match raw {
        Value::Null => Some(4.0),
        Value::Bool(_) => None,
        Value::Number(number) => number.as_f64().filter(|value| *value >= 0.0),
        Value::String(name) => WEIGHTS
            .iter()
            .find(|(key, _)| *key == name.trim().to_lowercase())
            .map(|(_, value)| *value),
        _ => None,
    }
}

pub fn effort_band(raw: &Value, effort_order: &BTreeMap<String, usize>) -> Option<Vec<String>> {
    let values: Vec<&str> = match raw {
        Value::String(value) => vec![value],
        Value::Array(values) => values.iter().map(Value::as_str).collect::<Option<_>>()?,
        _ => return None,
    };
    if !(1..=2).contains(&values.len())
        || values.iter().any(|item| !effort_order.contains_key(*item))
    {
        return None;
    }
    let mut band: Vec<String> = values.into_iter().map(str::to_owned).collect();
    band.sort_by_key(|name| effort_order.get(name).copied().unwrap_or(usize::MAX));
    Some(band)
}

pub fn entry_is_valid(entry: &Value, effort_order: &BTreeMap<String, usize>) -> bool {
    let Some(object) = entry.as_object() else {
        return false;
    };
    let Some(model) = object.get("model").and_then(Value::as_str) else {
        return false;
    };
    if !model.trim_matches('/').contains('/')
        || object
            .get("weight")
            .is_some_and(|value| weight_value(value).is_none())
    {
        return false;
    }
    object
        .get("effort")
        .is_none_or(|value| effort_band(value, effort_order).is_some())
}

pub fn merge_route(
    base: &[Value],
    overlay: &[Value],
    path: &str,
    drift: &mut Vec<String>,
    effort_order: &BTreeMap<String, usize>,
) -> Vec<Value> {
    let mut by_model = BTreeMap::new();
    for entry in base {
        if let (Some(model), Some(object)) = (
            entry.get("model").and_then(Value::as_str),
            entry.as_object(),
        ) {
            by_model.insert(model.to_owned(), object.clone());
        }
    }
    let mut merged: Vec<Value> = Vec::new();
    for (index, item) in overlay.iter().enumerate() {
        let candidate = if let Some(object) = item.as_object() {
            let model = object
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut combined = by_model.get(model).cloned().unwrap_or_default();
            for (key, value) in object {
                combined.insert(key.clone(), value.clone());
            }
            Value::Object(combined)
        } else {
            item.clone()
        };
        if !entry_is_valid(&candidate, effort_order) {
            drift.push(format!("{path}[{index}]: malformed overlay entry dropped; fix: use model adapter/id, weight high|normal|sparing|off or a number, optional effort band"));
            continue;
        }
        let model = candidate
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        merged.retain(|entry| entry.get("model").and_then(Value::as_str) != Some(model));
        merged.push(candidate);
    }
    let listed: BTreeSet<String> = merged
        .iter()
        .filter_map(|entry| {
            entry
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    merged.extend(
        base.iter()
            .filter(|entry| {
                entry.as_object().is_some_and(|object| {
                    object
                        .get("model")
                        .and_then(Value::as_str)
                        .is_some_and(|model| !listed.contains(model))
                })
            })
            .cloned(),
    );
    merged
}

pub fn renamed_message(kind: &str, name: &str) -> Option<String> {
    renamed_target(name).map(|target| {
        format!(
            "{kind} '{}' was renamed to '{target}'; fix: use the new name",
            name.trim()
        )
    })
}

fn renamed_target(name: &str) -> Option<&'static str> {
    let folded = name.trim().to_lowercase();
    RENAMED_TIERS
        .iter()
        .find(|(old, _)| *old == folded)
        .map(|(_, target)| *target)
}

fn renamed(name: &Value) -> Option<&str> {
    name.as_str()
        .filter(|value| renamed_target(value).is_some())
}

pub fn reject_renamed(overlay: &Value, label: &str) -> Result<Value, RenamedNameError> {
    let Some(object) = overlay.as_object() else {
        return Ok(overlay.clone());
    };
    if let Some(routes) = object.get("routes").and_then(Value::as_object) {
        for name in routes.keys() {
            if let Some(message) = renamed_message("route", name) {
                return Err(RenamedNameError(format!("{label} {message}")));
            }
        }
    }
    if let Some(synonyms) = object.get("route_synonyms").and_then(Value::as_object) {
        for target in synonyms.values() {
            if let Some(name) = renamed(target) {
                return Err(RenamedNameError(format!(
                    "{label} {}",
                    renamed_message("route_synonyms target", name).unwrap_or_default()
                )));
            }
        }
    }
    if let Some(tasks) = object.get("task_class_routes").and_then(Value::as_object) {
        for route in tasks.values() {
            if let Some(alias) = route.get("alias").and_then(renamed) {
                return Err(RenamedNameError(format!(
                    "{label} {}",
                    renamed_message("alias", alias).unwrap_or_default()
                )));
            }
        }
    }
    for section in ["families", "adapters"] {
        if let Some(entries) = object.get(section).and_then(Value::as_object) {
            for entry in entries.values().filter_map(Value::as_object) {
                let mut mappings = Vec::new();
                mappings.push(entry.get("aliases"));
                for field in ["role_overrides", "role_effort_defaults"] {
                    if let Some(roles) = entry.get(field).and_then(Value::as_object) {
                        mappings.extend(roles.values().map(Some));
                    }
                }
                for mapping in mappings.into_iter().flatten().filter_map(Value::as_object) {
                    for name in mapping.keys() {
                        if let Some(message) = renamed_message("alias", name) {
                            return Err(RenamedNameError(format!("{label} {message}")));
                        }
                    }
                }
                if let Some(tiers) = entry.get("risk_tier_overrides").and_then(Value::as_object) {
                    for value in tiers.values() {
                        if let Some(alias) = value.get("alias").and_then(renamed) {
                            return Err(RenamedNameError(format!(
                                "{label} {}",
                                renamed_message("alias", alias).unwrap_or_default()
                            )));
                        }
                    }
                }
            }
        }
    }
    let mut result = overlay.clone();
    if let Some(synonyms) = result
        .get("route_synonyms")
        .and_then(Value::as_object)
        .cloned()
    {
        let kept: Map<String, Value> = synonyms
            .into_iter()
            .filter(|(name, _)| {
                !TIER_ROUTES.contains(&name.as_str()) && name != "design" && name != "writing"
            })
            .collect();
        result["route_synonyms"] = Value::Object(kept);
    }
    Ok(result)
}

pub fn migrate_renamed(
    document: &Value,
) -> Result<(Value, Vec<String>, Vec<String>), MigrationConflict> {
    let Some(_) = document.as_object() else {
        return Ok((document.clone(), Vec::new(), Vec::new()));
    };
    let mut result = document.clone();
    let mut notes = Vec::new();
    let mut moved = Vec::new();
    if let Some(routes) = result.get("routes").and_then(Value::as_object).cloned() {
        if routes.keys().any(|name| renamed_target(name).is_some()) {
            let mut merged: Map<String, Value> = routes
                .iter()
                .filter(|(name, _)| renamed_target(name).is_none())
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            for (name, value) in routes
                .iter()
                .filter(|(name, _)| renamed_target(name).is_some())
            {
                let target = renamed_target(name).unwrap_or_default();
                let old_path = format!("routes.{name}");
                let new_path = format!("routes.{target}");
                settle(&mut merged, target, value, &old_path, &new_path)?;
                notes.push(format!("{old_path} -> {target}"));
                moved.push(new_path);
            }
            result["routes"] = Value::Object(merged);
        }
    }
    if let Some(synonyms) = result
        .get("route_synonyms")
        .and_then(Value::as_object)
        .cloned()
    {
        let mut kept = Map::new();
        for (name, mut target) in synonyms {
            if renamed_target(&name).is_some() {
                notes.push(format!(
                    "route_synonyms.{name} dropped (the product maps task classes)"
                ));
                continue;
            }
            if let Some(old) = target.as_str().and_then(renamed_target) {
                notes.push(format!(
                    "route_synonyms.{name} {} -> {old}",
                    target.as_str().unwrap_or_default()
                ));
                target = Value::String(old.into());
            }
            kept.insert(name, target);
        }
        result["route_synonyms"] = Value::Object(kept);
    }
    rename_nested(&mut result, &mut notes, &mut moved)?;
    Ok((result, notes, moved))
}

fn settle(
    target: &mut Map<String, Value>,
    name: &str,
    value: &Value,
    old_path: &str,
    new_path: &str,
) -> Result<(), MigrationConflict> {
    if target.get(name).is_some_and(|current| current != value) {
        let current = target.get(name).map(Value::to_string).unwrap_or_default();
        return Err(MigrationConflict(format!("{old_path} = {value} conflicts with {new_path} = {current}; keep one, delete the other, then refresh again")));
    }
    target
        .entry(name.to_owned())
        .or_insert_with(|| value.clone());
    Ok(())
}

fn rename_nested(
    document: &mut Value,
    notes: &mut Vec<String>,
    moved: &mut Vec<String>,
) -> Result<(), MigrationConflict> {
    if let Some(defaults) = document
        .pointer_mut("/task_classification/role_defaults")
        .and_then(Value::as_object_mut)
    {
        rename_map(defaults, "task_classification.role_defaults", notes, moved)?;
        for (role, alias) in defaults.iter_mut() {
            if let Some(old) = alias.as_str().and_then(renamed_target) {
                notes.push(format!(
                    "task_classification.role_defaults.{role} {} -> {old}",
                    alias.as_str().unwrap_or_default()
                ));
                moved.push(format!("task_classification.role_defaults.{role}"));
                *alias = Value::String(old.into());
            }
        }
    }
    if let Some(tasks) = document
        .get_mut("task_class_routes")
        .and_then(Value::as_object_mut)
    {
        for (name, route) in tasks {
            rename_alias(route, &format!("task_class_routes.{name}"), notes);
        }
    }
    for section in ["families", "adapters"] {
        if let Some(entries) = document.get_mut(section).and_then(Value::as_object_mut) {
            for (name, entry) in entries {
                let Some(entry) = entry.as_object_mut() else {
                    continue;
                };
                let base = format!("{section}.{name}");
                if let Some(aliases) = entry.get_mut("aliases").and_then(Value::as_object_mut) {
                    rename_map(aliases, &format!("{base}.aliases"), notes, moved)?;
                }
                for field in ["role_overrides", "role_effort_defaults"] {
                    if let Some(roles) = entry.get_mut(field).and_then(Value::as_object_mut) {
                        for (role, map) in roles {
                            if let Some(map) = map.as_object_mut() {
                                rename_map(map, &format!("{base}.{field}.{role}"), notes, moved)?;
                            }
                        }
                    }
                }
                if let Some(tiers) = entry
                    .get_mut("risk_tier_overrides")
                    .and_then(Value::as_object_mut)
                {
                    for (tier, value) in tiers {
                        rename_alias(value, &format!("{base}.risk_tier_overrides.{tier}"), notes);
                    }
                }
            }
        }
    }
    Ok(())
}

fn rename_map(
    mapping: &mut Map<String, Value>,
    path: &str,
    notes: &mut Vec<String>,
    moved: &mut Vec<String>,
) -> Result<(), MigrationConflict> {
    if !mapping.keys().any(|key| renamed_target(key).is_some()) {
        return Ok(());
    }
    let old = std::mem::take(mapping);
    for (key, value) in &old {
        if renamed_target(key).is_none() {
            mapping.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in old.iter().filter(|(key, _)| renamed_target(key).is_some()) {
        let target = renamed_target(key).unwrap_or_default();
        let new_path = format!("{path}.{target}");
        settle(mapping, target, value, &format!("{path}.{key}"), &new_path)?;
        notes.push(format!("{path}.{key} -> {target}"));
        moved.push(new_path);
    }
    Ok(())
}

fn rename_alias(holder: &mut Value, path: &str, notes: &mut Vec<String>) {
    if let Some(alias) = holder
        .get("alias")
        .and_then(Value::as_str)
        .filter(|value| renamed_target(value).is_some())
    {
        let target = renamed_target(alias).unwrap_or_default();
        notes.push(format!("{path}.alias {alias} -> {target}"));
        holder["alias"] = Value::String(target.into());
    }
}

pub fn route_name(catalog: &Value, name: &str) -> Result<String, PoolError> {
    let routes = catalog.get("routes").and_then(Value::as_object);
    let key = name.trim().to_lowercase();
    if routes.is_some_and(|routes| routes.contains_key(&key)) {
        return Ok(key);
    }
    if let Some(message) = renamed_message("route", name.trim()) {
        return Err(PoolError::new("route_renamed", &message));
    }
    if let Some(target) = catalog
        .get("route_synonyms")
        .and_then(|synonyms| synonyms.get(&key))
        .and_then(Value::as_str)
    {
        if routes.is_some_and(|routes| routes.contains_key(target)) {
            return Ok(target.to_owned());
        }
    }
    let names = routes
        .map(|routes| routes.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    Err(PoolError::new(
        "route_invalid",
        &format!("Pass route {}.", names.join(", ")),
    ))
}

pub fn known_provider_path(text: &str) -> bool {
    let text = text.to_lowercase();
    ["opencode/", "opencode-go/", "openrouter/"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

pub fn default_installed(adapter: &str) -> bool {
    let command = match adapter {
        "cursor" => "cursor-agent",
        "kiro" => "kiro-cli",
        _ => adapter,
    };
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|directory| executable_in(&directory.join(command)))
    })
}

fn executable_in(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::collections::BTreeMap;

    use super::{
        effort_band, entry_is_valid, merge_route, migrate_renamed, weight_value, PoolError,
    };

    #[test]
    fn pool_weights_and_effort_bands_reject_invalid_values() {
        assert_eq!(weight_value(&json!(" HIGH ")), Some(8.0));
        assert_eq!(weight_value(&json!(true)), None);
        assert_eq!(weight_value(&json!(-0.5)), None);
        let order = BTreeMap::from([("low".into(), 0), ("medium".into(), 1), ("high".into(), 2)]);
        assert_eq!(
            effort_band(&json!(["high", "low"]), &order),
            Some(vec!["low".into(), "high".into()])
        );
        assert_eq!(effort_band(&json!(["max"]), &order), None);
        assert!(!entry_is_valid(
            &json!({"model":"codex/gpt-5","weight":true}),
            &order
        ));
    }

    #[test]
    fn overlay_reorders_entries_and_drops_malformed_rows_with_drift() {
        let base = vec![
            json!({"model":"codex/a","weight":"normal","effort":["high","high"]}),
            json!({"model":"claude/b","weight":"sparing"}),
        ];
        let overlay = vec![
            json!({"model":"codex/a","weight":"high"}),
            json!({"model":"broken","weight":"off"}),
        ];
        let mut drift = Vec::new();
        let order = BTreeMap::from([("high".into(), 2), ("xhigh".into(), 3)]);
        let merged = merge_route(&base, &overlay, "routes.senior", &mut drift, &order);
        assert_eq!(merged[0]["weight"], "high");
        assert_eq!(merged[0]["effort"], json!(["high", "high"]));
        assert_eq!(merged[1]["model"], "claude/b");
        assert_eq!(drift.len(), 1);
        assert_eq!(PoolError::new("x", "fix").code, "x");
    }

    #[test]
    fn renamed_tiers_migrate_and_conflicting_values_are_refused() {
        let (migrated, notes, moved) = migrate_renamed(&json!({"routes":{"strong":[1]},"task_classification":{"role_defaults":{"review":"flagship"}}})).unwrap();
        assert_eq!(migrated["routes"]["senior"], json!([1]));
        assert_eq!(
            migrated["task_classification"]["role_defaults"]["senior"],
            "principal"
        );
        assert!(notes.len() >= 2);
        assert!(moved.contains(&"routes.senior".to_owned()));
        assert!(migrate_renamed(&json!({"routes":{"strong":[1],"senior":[2]}})).is_err());
    }
}
