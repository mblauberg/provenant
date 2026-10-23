//! Ordered route-pool ingress, migration and overlay refusal.
//!
//! The Python source operates on insertion-ordered dictionaries. Keep that order
//! through both the first-refusal scan and the rewritten document.

use crate::ordered_json::{OrderedObject, OrderedValue};
use crate::pools::{MigrationConflict, RenamedNameError};
use provenant_core::py::{python_json_eq, python_strip};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MergeRouteError {
    #[error("cannot use '{0}' as a dict key (unhashable type: '{0}')")]
    UnhashableBaseModel(&'static str),
    #[error("cannot use '{0}' as a dict key (unhashable type: '{0}')")]
    UnhashableOverlayModel(&'static str),
}

pub const RENAMED_TIERS: [(&str, &str); 6] = [
    ("scout", "junior"),
    ("bulk", "junior"),
    ("workhorse", "senior"),
    ("strong", "senior"),
    ("review", "senior"),
    ("flagship", "principal"),
];

pub const TIER_ROUTES: [&str; 4] = ["utility", "junior", "senior", "principal"];

pub fn renamed_target(name: &str) -> Option<&'static str> {
    let folded = crate::casefold::fold(python_strip(name));
    RENAMED_TIERS
        .iter()
        .find(|(old, _)| *old == folded)
        .map(|(_, target)| *target)
}

pub fn renamed_message(kind: &str, name: &str) -> Option<String> {
    renamed_target(name)
        .map(|target| format!("{kind} '{name}' was renamed to '{target}'; fix: use the new name"))
}

/// Merge an overlay pool while preserving source dictionary order at every
/// object boundary. JSON arrays and objects cannot be model keys in Python;
/// report those failures explicitly before filtering malformed entries.
pub fn merge_route_ordered(
    base: &[OrderedValue],
    overlay: &[OrderedValue],
    path: &str,
    drift: &mut Vec<String>,
    effort_order: &BTreeMap<String, usize>,
) -> Result<Vec<OrderedValue>, MergeRouteError> {
    let mut by_model: Vec<(OrderedValue, OrderedValue)> = Vec::new();
    for entry in base {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(model) = object.get("model") else {
            continue;
        };
        ensure_hashable_model(model, true)?;
        if let Some((_, previous)) = by_model
            .iter_mut()
            .find(|(key, _)| python_json_eq(&key.to_value(), &model.to_value()))
        {
            *previous = entry.clone();
        } else {
            by_model.push((model.clone(), entry.clone()));
        }
    }

    let mut merged = Vec::new();
    for (index, item) in overlay.iter().enumerate() {
        let candidate = if let Some(object) = item.as_object() {
            let model = object.get("model").cloned().unwrap_or(OrderedValue::Null);
            ensure_hashable_model(&model, false)?;
            let mut combined = by_model
                .iter()
                .find(|(key, _)| python_json_eq(&key.to_value(), &model.to_value()))
                .and_then(|(_, value)| value.as_object())
                .cloned()
                .unwrap_or_default();
            for (key, value) in &object.0 {
                combined.insert(key.clone(), value.clone());
            }
            OrderedValue::Object(combined)
        } else {
            item.clone()
        };

        if !crate::pools::entry_is_valid(&candidate.to_value(), effort_order) {
            drift.push(format!(
                "{path}[{index}]: malformed overlay entry dropped; fix: use model adapter/id, weight high|normal|sparing|off or a number, optional effort band"
            ));
            continue;
        }
        let model = candidate
            .get("model")
            .and_then(OrderedValue::as_str)
            .unwrap_or_default();
        merged.retain(|entry: &OrderedValue| {
            entry.get("model").and_then(OrderedValue::as_str) != Some(model)
        });
        merged.push(candidate);
    }

    let listed: BTreeSet<String> = merged
        .iter()
        .filter_map(|entry| {
            entry
                .get("model")
                .and_then(OrderedValue::as_str)
                .map(str::to_owned)
        })
        .collect();
    for entry in base {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let model = object.get("model");
        let already_listed = model
            .and_then(OrderedValue::as_str)
            .is_some_and(|model| listed.contains(model));
        if !already_listed {
            merged.push(entry.clone());
        }
    }
    Ok(merged)
}

fn ensure_hashable_model(value: &OrderedValue, base_key: bool) -> Result<(), MergeRouteError> {
    match value {
        OrderedValue::Array(_) => Err(if base_key {
            MergeRouteError::UnhashableBaseModel("list")
        } else {
            MergeRouteError::UnhashableOverlayModel("list")
        }),
        OrderedValue::Object(_) => Err(if base_key {
            MergeRouteError::UnhashableBaseModel("dict")
        } else {
            MergeRouteError::UnhashableOverlayModel("dict")
        }),
        _ => Ok(()),
    }
}

pub fn reject_renamed(
    overlay: &OrderedValue,
    label: &str,
) -> Result<OrderedValue, RenamedNameError> {
    let Some(object) = overlay.as_object() else {
        return Ok(overlay.clone());
    };

    if let Some(routes) = object.get("routes").and_then(OrderedValue::as_object) {
        for (name, _) in &routes.0 {
            if let Some(message) = renamed_message("route", name) {
                return Err(RenamedNameError(format!("{label} {message}")));
            }
        }
    }
    if let Some(synonyms) = object
        .get("route_synonyms")
        .and_then(OrderedValue::as_object)
    {
        for (_, target) in &synonyms.0 {
            if let Some(name) = target
                .as_str()
                .filter(|value| renamed_target(value).is_some())
            {
                return Err(RenamedNameError(format!(
                    "{label} {}",
                    renamed_message("route_synonyms target", name).unwrap_or_default()
                )));
            }
        }
    }
    if let Some(tasks) = object
        .get("task_class_routes")
        .and_then(OrderedValue::as_object)
    {
        for (_, route) in &tasks.0 {
            if let Some(alias) = route
                .get("alias")
                .and_then(OrderedValue::as_str)
                .filter(|value| renamed_target(value).is_some())
            {
                return Err(RenamedNameError(format!(
                    "{label} {}",
                    renamed_message("alias", alias).unwrap_or_default()
                )));
            }
        }
    }
    for section in ["families", "adapters"] {
        if let Some(entries) = object.get(section).and_then(OrderedValue::as_object) {
            for (_, entry) in &entries.0 {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                if let Some(aliases) = entry.get("aliases").and_then(OrderedValue::as_object) {
                    for (name, _) in &aliases.0 {
                        if let Some(message) = renamed_message("alias", name) {
                            return Err(RenamedNameError(format!("{label} {message}")));
                        }
                    }
                }
                for field in ["role_overrides", "role_effort_defaults"] {
                    if let Some(roles) = entry.get(field).and_then(OrderedValue::as_object) {
                        for (_, mapping) in &roles.0 {
                            if let Some(mapping) = mapping.as_object() {
                                for (name, _) in &mapping.0 {
                                    if let Some(message) = renamed_message("alias", name) {
                                        return Err(RenamedNameError(format!("{label} {message}")));
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(tiers) = entry
                    .get("risk_tier_overrides")
                    .and_then(OrderedValue::as_object)
                {
                    for (_, value) in &tiers.0 {
                        if let Some(alias) = value
                            .get("alias")
                            .and_then(OrderedValue::as_str)
                            .filter(|value| renamed_target(value).is_some())
                        {
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
    if let Some(synonyms) = object_child_mut(&mut result, "route_synonyms") {
        synonyms.0.retain(|(name, _)| {
            !TIER_ROUTES.contains(&name.as_str()) && name != "design" && name != "writing"
        });
    }
    Ok(result)
}

pub fn migrate_renamed(
    document: &OrderedValue,
) -> Result<(OrderedValue, Vec<String>, Vec<String>), MigrationConflict> {
    let mut result = document.clone();
    if result.as_object().is_none() {
        return Ok((result, Vec::new(), Vec::new()));
    }
    let mut notes = Vec::new();
    let mut moved = Vec::new();

    if let Some(routes) = object_child_mut(&mut result, "routes") {
        rename_map(routes, "routes", &mut notes, &mut moved)?;
    }
    if let Some(synonyms) = object_child_mut(&mut result, "route_synonyms") {
        let old = std::mem::take(&mut synonyms.0);
        for (name, mut target) in old {
            if renamed_target(&name).is_some() {
                notes.push(format!(
                    "route_synonyms.{name} dropped (the product maps task classes)"
                ));
                continue;
            }
            if let Some(raw) = target.as_str() {
                if let Some(new) = renamed_target(raw) {
                    notes.push(format!("route_synonyms.{name} {raw} -> {new}"));
                    target = OrderedValue::String(new.into());
                }
            }
            synonyms.insert(name, target);
        }
    }
    rename_nested(&mut result, &mut notes, &mut moved)?;
    Ok((result, notes, moved))
}

fn rename_nested(
    document: &mut OrderedValue,
    notes: &mut Vec<String>,
    moved: &mut Vec<String>,
) -> Result<(), MigrationConflict> {
    if let Some(defaults) =
        object_child_path_mut(document, &["task_classification", "role_defaults"])
    {
        rename_map(defaults, "task_classification.role_defaults", notes, moved)?;
        for (role, alias) in &mut defaults.0 {
            if let Some(raw) = alias.as_str() {
                if let Some(new) = renamed_target(raw) {
                    notes.push(format!(
                        "task_classification.role_defaults.{role} {raw} -> {new}"
                    ));
                    moved.push(format!("task_classification.role_defaults.{role}"));
                    *alias = OrderedValue::String(new.into());
                }
            }
        }
    }
    if let Some(tasks) = object_child_mut(document, "task_class_routes") {
        for (name, route) in &mut tasks.0 {
            rename_alias(route, &format!("task_class_routes.{name}"), notes);
        }
    }
    for section in ["families", "adapters"] {
        if let Some(entries) = object_child_mut(document, section) {
            for (name, entry) in &mut entries.0 {
                if entry.as_object().is_none() {
                    continue;
                }
                let base = format!("{section}.{name}");
                if let Some(aliases) = object_child_mut(entry, "aliases") {
                    rename_map(aliases, &format!("{base}.aliases"), notes, moved)?;
                }
                for field in ["role_overrides", "role_effort_defaults"] {
                    if let Some(roles) = object_child_mut(entry, field) {
                        for (role, mapping) in &mut roles.0 {
                            if let Some(mapping) = object_mut(mapping) {
                                rename_map(
                                    mapping,
                                    &format!("{base}.{field}.{role}"),
                                    notes,
                                    moved,
                                )?;
                            }
                        }
                    }
                }
                if let Some(tiers) = object_child_mut(entry, "risk_tier_overrides") {
                    for (tier, value) in &mut tiers.0 {
                        rename_alias(value, &format!("{base}.risk_tier_overrides.{tier}"), notes);
                    }
                }
            }
        }
    }
    Ok(())
}

fn rename_alias(holder: &mut OrderedValue, path: &str, notes: &mut Vec<String>) {
    let Some(raw) = holder.get("alias").and_then(OrderedValue::as_str) else {
        return;
    };
    let Some(target) = renamed_target(raw) else {
        return;
    };
    let note = format!("{path}.alias {raw} -> {target}");
    if let Some(object) = object_mut(holder) {
        object.insert("alias".into(), OrderedValue::String(target.into()));
        notes.push(note);
    }
}

fn rename_map(
    mapping: &mut OrderedObject,
    path: &str,
    notes: &mut Vec<String>,
    moved: &mut Vec<String>,
) -> Result<(), MigrationConflict> {
    if !mapping
        .0
        .iter()
        .any(|(key, _)| renamed_target(key).is_some())
    {
        return Ok(());
    }
    let old = std::mem::take(&mut mapping.0);
    for (key, value) in &old {
        if renamed_target(key).is_none() {
            mapping.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in &old {
        let Some(target) = renamed_target(key) else {
            continue;
        };
        let old_path = format!("{path}.{key}");
        let new_path = format!("{path}.{target}");
        settle(mapping, target, value, &old_path, &new_path)?;
        notes.push(format!("{old_path} -> {target}"));
        moved.push(new_path);
    }
    Ok(())
}

fn settle(
    target: &mut OrderedObject,
    name: &str,
    value: &OrderedValue,
    old_path: &str,
    new_path: &str,
) -> Result<(), MigrationConflict> {
    if let Some(current) = target.get(name) {
        if !python_json_eq(&value.to_value(), &current.to_value()) {
            return Err(MigrationConflict(format!(
                "{old_path} = {} conflicts with {new_path} = {}; keep one, delete the other, then refresh again",
                crate::pending::json_dumps(value, false, None),
                crate::pending::json_dumps(current, false, None)
            )));
        }
        return Ok(());
    }
    target.insert(name.into(), value.clone());
    Ok(())
}

fn object_mut(value: &mut OrderedValue) -> Option<&mut OrderedObject> {
    match value {
        OrderedValue::Object(object) => Some(object),
        _ => None,
    }
}

fn object_child_mut<'a>(value: &'a mut OrderedValue, name: &str) -> Option<&'a mut OrderedObject> {
    object_mut(object_value_child_mut(value, name)?)
}

fn object_value_child_mut<'a>(
    value: &'a mut OrderedValue,
    name: &str,
) -> Option<&'a mut OrderedValue> {
    object_mut(value)?
        .0
        .iter_mut()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

fn object_child_path_mut<'a>(
    mut value: &'a mut OrderedValue,
    path: &[&str],
) -> Option<&'a mut OrderedObject> {
    let (last, parents) = path.split_last()?;
    for name in parents {
        value = object_value_child_mut(value, name)?;
    }
    object_child_mut(value, last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn parse(text: &str) -> OrderedValue {
        crate::pending::from_json(text).unwrap()
    }

    fn ordered_shape(value: &OrderedValue) -> Value {
        match value {
            OrderedValue::Object(object) => json!([
                "object",
                object
                    .0
                    .iter()
                    .map(|(key, value)| json!([key, ordered_shape(value)]))
                    .collect::<Vec<_>>()
            ]),
            OrderedValue::Array(items) => {
                json!(["array", items.iter().map(ordered_shape).collect::<Vec<_>>()])
            }
            other => other.to_value(),
        }
    }

    #[test]
    fn migration_preserves_insertion_order_and_rewrites_each_nested_location() {
        let input = parse(
            r#"{"routes":{"senior":1.0,"keep":[],"strong":true},"route_synonyms":{"keep":"strong","junior":"drop","other":"bulk"},"task_classification":{"role_defaults":{"worker":"scout","review":"flagship"}},"task_class_routes":{"build":{"alias":"WORKHORSE"}},"families":{"f":{"aliases":{"principal":["p"],"flagship":["p"]},"role_overrides":{"worker":{"bulk":["j"]}},"role_effort_defaults":{"worker":{"review":["s"]}},"risk_tier_overrides":{"x":{"alias":"strong"}}}},"adapters":{"a":{"aliases":{"workhorse":["s"]}}}}"#,
        );
        let (migrated, notes, moved) = migrate_renamed(&input).unwrap();
        let wire = serde_json::to_string(&migrated).unwrap();
        assert_eq!(
            wire,
            r#"{"routes":{"senior":1.0,"keep":[]},"route_synonyms":{"keep":"senior","junior":"drop","other":"junior"},"task_classification":{"role_defaults":{"worker":"junior","senior":"principal"}},"task_class_routes":{"build":{"alias":"senior"}},"families":{"f":{"aliases":{"principal":["p"]},"role_overrides":{"worker":{"junior":["j"]}},"role_effort_defaults":{"worker":{"senior":["s"]}},"risk_tier_overrides":{"x":{"alias":"senior"}}}},"adapters":{"a":{"aliases":{"senior":["s"]}}}}"#
        );
        assert_eq!(
            notes,
            [
                "routes.strong -> senior",
                "route_synonyms.keep strong -> senior",
                "route_synonyms.other bulk -> junior",
                "task_classification.role_defaults.review -> senior",
                "task_classification.role_defaults.worker scout -> junior",
                "task_classification.role_defaults.senior flagship -> principal",
                "task_class_routes.build.alias WORKHORSE -> senior",
                "families.f.aliases.flagship -> principal",
                "families.f.role_overrides.worker.bulk -> junior",
                "families.f.role_effort_defaults.worker.review -> senior",
                "families.f.risk_tier_overrides.x.alias strong -> senior",
                "adapters.a.aliases.workhorse -> senior",
            ]
        );
        assert_eq!(
            moved,
            [
                "routes.senior",
                "task_classification.role_defaults.senior",
                "task_classification.role_defaults.worker",
                "task_classification.role_defaults.senior",
                "families.f.aliases.principal",
                "families.f.role_overrides.worker.junior",
                "families.f.role_effort_defaults.worker.senior",
                "adapters.a.aliases.senior",
            ]
        );
    }

    #[test]
    fn migration_uses_python_numeric_equality_and_conflict_json_spelling() {
        let equal = parse(r#"{"routes":{"strong":true,"senior":1.0}}"#);
        let (migrated, _, _) = migrate_renamed(&equal).unwrap();
        assert_eq!(
            serde_json::to_string(&migrated).unwrap(),
            r#"{"routes":{"senior":1.0}}"#
        );

        let conflict =
            parse(r#"{"routes":{"strong":{"z":"日本語","a":1},"senior":{"a":2,"z":"é"}}}"#);
        let error = migrate_renamed(&conflict).unwrap_err();
        assert_eq!(
            error.to_string(),
            "routes.strong = {\"z\": \"\\u65e5\\u672c\\u8a9e\", \"a\": 1} conflicts with routes.senior = {\"a\": 2, \"z\": \"\\u00e9\"}; keep one, delete the other, then refresh again"
        );
    }

    #[test]
    fn rejection_follows_source_order_then_prunes_reserved_synonym_names_in_order() {
        let bad =
            parse(r#"{"routes":{"workhorse":[],"strong":[]},"route_synonyms":{"x":"flagship"}}"#);
        assert_eq!(
            reject_renamed(&bad, "overlay").unwrap_err().to_string(),
            "overlay route 'workhorse' was renamed to 'senior'; fix: use the new name"
        );

        let valid = parse(
            r#"{"before":1,"route_synonyms":{"z":"senior","design":"craft","a":"junior","custom":"other"},"after":2}"#,
        );
        let result = reject_renamed(&valid, "overlay").unwrap();
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"before":1,"route_synonyms":{"z":"senior","a":"junior","custom":"other"},"after":2}"#
        );
    }

    #[test]
    fn migration_matches_python_ordered_document_and_diagnostics_oracle() {
        let raw = r#"{"routes":{"senior":[1],"strong":[true],"keep":[]},"route_synonyms":{"keep":"workhorse","junior":"old","craft":"principal"},"task_classification":{"role_defaults":{"worker":"bulk","review":"flagship"}},"families":{"f":{"aliases":{"strong":["x"],"keep":["y"]},"role_overrides":{"reviewer":{"review":["z"]}},"risk_tier_overrides":{"tier":{"alias":"scout"}}}},"adapters":{"a":{"role_effort_defaults":{"worker":{"workhorse":["m"]}}}}}"#;
        let input = parse(raw);
        let (actual, notes, moved) = migrate_renamed(&input).unwrap();
        let expected: Value = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('route_pools',pathlib.Path('scripts/model_route_pools.py'))
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
def ordered(v):
 if isinstance(v,dict): return ['object',[[k,ordered(x)] for k,x in v.items()]]
 if isinstance(v,list): return ['array',[ordered(x) for x in v]]
 return v
d=json.loads(json.load(sys.stdin))
value,notes,moved=m.migrate_renamed(d)
print(json.dumps([ordered(value),notes,moved]))
"#,
            &json!(raw),
        )
        .unwrap();
        assert_eq!(json!([ordered_shape(&actual), notes, moved]), expected);
    }

    #[test]
    fn migration_conflicts_and_rejection_errors_match_python_oracle() {
        let fixtures = [
            r#"{"routes":{"strong":{"z":"日本語","a":1},"senior":{"a":2,"z":"é"}}}"#,
            r#"{"routes":{" workhorse ":[],"strong":[]},"route_synonyms":{"x":"flagship"}}"#,
            r#"{"before":1,"route_synonyms":{"z":"senior","design":"craft","a":"junior","custom":"other"},"after":2}"#,
        ];
        let migration_error = migrate_renamed(&parse(fixtures[0])).unwrap_err();
        let reject_error = reject_renamed(&parse(fixtures[1]), "overlay").unwrap_err();
        let pruned = reject_renamed(&parse(fixtures[2]), "overlay").unwrap();
        let actual = json!([
            ["error", "MigrationConflict", migration_error.to_string()],
            ["error", "RenamedNameError", reject_error.to_string()],
            ["ok", ordered_shape(&pruned)],
        ]);
        let expected: Value = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('route_pools',pathlib.Path('scripts/model_route_pools.py'))
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
def ordered(v):
 if isinstance(v,dict): return ['object',[[k,ordered(x)] for k,x in v.items()]]
 if isinstance(v,list): return ['array',[ordered(x) for x in v]]
 return v
def outcome(call):
 try:
  result=call()
  return ['ok',ordered(result)]
 except Exception as error:
  return ['error',type(error).__name__,str(error)]
raws=json.load(sys.stdin)
try:
 value,notes,moved=m.migrate_renamed(json.loads(raws[0]))
 first=['ok',ordered(value),notes,moved]
except Exception as error:
 first=['error',type(error).__name__,str(error)]
second=outcome(lambda:m.reject_renamed(json.loads(raws[1]),'overlay'))
third=outcome(lambda:m.reject_renamed(json.loads(raws[2]),'overlay'))
print(json.dumps([first,second,third]))
"#,
            &json!(fixtures),
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn ordered_merge_matches_python_order_and_malformed_input_behavior() {
        let fixtures = [
            r#"{"path":"routes.senior","effort_order":{"low":0,"medium":1,"high":2},"base":[{"model":"adapter/one","weight":"high","base":"first","order":"old"},{"model":"adapter/one","weight":"sparing","base":"last","tail":true},{"model":"adapter/two","weight":"normal"},"ignored",{"weight":"high"},{"model":null,"weight":"normal","null":true}],"overlay":[{"model":"adapter/one","effort":["high","low"]},{"model":"adapter/new","weight":"off","first":1},{"model":"adapter/new","weight":"normal","second":2},"malformed",{"model":"no-separator","weight":"normal"},{"model":"adapter/two","weight":true}]}"#,
            r#"{"path":"routes.junior","effort_order":{},"base":[{"model":[]}],"overlay":[]}"#,
            r#"{"path":"routes.junior","effort_order":{},"base":[],"overlay":[{"model":{}}]}"#,
            r#"{"path":"routes.junior","effort_order":{},"base":[{"model":true,"weight":"high"},{"model":1,"weight":"off"}],"overlay":[{"model":1,"weight":"normal"}]}"#,
        ];
        let effort_order = BTreeMap::from([
            ("low".to_owned(), 0),
            ("medium".to_owned(), 1),
            ("high".to_owned(), 2),
        ]);
        let actual: Vec<Value> = fixtures
            .iter()
            .map(|raw| {
                let input = parse(raw);
                let path = input.get("path").and_then(OrderedValue::as_str).unwrap();
                let base = match input.get("base").unwrap() {
                    OrderedValue::Array(values) => values,
                    _ => unreachable!(),
                };
                let overlay = match input.get("overlay").unwrap() {
                    OrderedValue::Array(values) => values,
                    _ => unreachable!(),
                };
                let mut drift = Vec::new();
                match merge_route_ordered(base, overlay, path, &mut drift, &effort_order) {
                    Ok(entries) => json!([
                        "ok",
                        entries.iter().map(ordered_shape).collect::<Vec<_>>(),
                        drift
                    ]),
                    Err(error @ MergeRouteError::UnhashableBaseModel(_))
                    | Err(error @ MergeRouteError::UnhashableOverlayModel(_)) => {
                        json!(["TypeError", error.to_string()])
                    }
                }
            })
            .collect();
        let expected: Vec<Value> = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('route_pools',pathlib.Path('scripts/model_route_pools.py'))
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
def ordered(v):
 if isinstance(v,dict): return ['object',[[k,ordered(x)] for k,x in v.items()]]
 if isinstance(v,list): return ['array',[ordered(x) for x in v]]
 return v
out=[]
for raw in json.load(sys.stdin):
 d=json.loads(raw);drift=[]
 try:
  result=m.merge_route(d['base'],d['overlay'],d['path'],drift,d['effort_order'])
  out.append(['ok',[ordered(x) for x in result],drift])
 except Exception as error:
  out.append([type(error).__name__,str(error)])
print(json.dumps(out))
"#,
            &json!(fixtures),
        )
        .unwrap();
        for (index, (got, want)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(got, want, "merge fixture {index}");
        }
        assert_eq!(
            actual[0][2],
            json!([
                "routes.senior[3]: malformed overlay entry dropped; fix: use model adapter/id, weight high|normal|sparing|off or a number, optional effort band",
                "routes.senior[4]: malformed overlay entry dropped; fix: use model adapter/id, weight high|normal|sparing|off or a number, optional effort band",
                "routes.senior[5]: malformed overlay entry dropped; fix: use model adapter/id, weight high|normal|sparing|off or a number, optional effort band"
            ])
        );
    }
}
