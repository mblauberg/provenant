//! Validation, migration and model-name helpers for weighted route pools.

use std::collections::{BTreeMap, BTreeSet};

use crate::{casefold::fold, pending::python_strip};
use serde_json::{Number, Value};
use thiserror::Error;

const WEIGHTS: [(&str, u64); 4] = [("high", 8), ("normal", 4), ("sparing", 1), ("off", 0)];

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

/// Preserve numeric kind and integer precision (PORTING v4 R4).
pub fn weight_value(raw: &Value) -> Option<Number> {
    match raw {
        Value::Null => Some(Number::from(4)),
        Value::Bool(_) => None,
        Value::Number(number) => {
            let nonnegative = if number.is_f64() {
                number.as_f64().is_some_and(|value| value >= 0.0)
            } else {
                number.as_u64().is_some()
            };
            nonnegative.then(|| number.clone())
        }
        Value::String(name) => WEIGHTS
            .iter()
            .find(|(key, _)| *key == fold(python_strip(name)))
            .map(|(_, value)| Number::from(*value)),
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
) -> Result<Vec<Value>, MergeRouteError> {
    let base: Vec<_> = base.iter().cloned().map(Into::into).collect();
    let overlay: Vec<_> = overlay.iter().cloned().map(Into::into).collect();
    merge_route_ordered(&base, &overlay, path, drift, effort_order).map(|values| {
        values
            .iter()
            .map(crate::ordered_json::OrderedValue::to_value)
            .collect()
    })
}

pub use crate::pool_migration::renamed_message;
pub use crate::pool_migration::{
    merge_route_ordered, migrate_renamed as migrate_renamed_ordered,
    reject_renamed as reject_renamed_ordered, MergeRouteError,
};

/// Value callers have already supplied canonical key order. Ordered text ingress
/// uses migrate_renamed_ordered directly and retains the source dictionary order.
pub fn reject_renamed(overlay: &Value, label: &str) -> Result<Value, RenamedNameError> {
    crate::pool_migration::reject_renamed(&overlay.clone().into(), label).map(|v| v.to_value())
}
pub fn migrate_renamed(
    document: &Value,
) -> Result<(Value, Vec<String>, Vec<String>), MigrationConflict> {
    let (value, notes, moved) = crate::pool_migration::migrate_renamed(&document.clone().into())?;
    Ok((value.to_value(), notes, moved))
}

pub fn route_name(catalog: &Value, name: &str) -> Result<String, PoolError> {
    let routes = catalog.get("routes").and_then(Value::as_object);
    let key = fold(python_strip(name));
    if routes.is_some_and(|routes| routes.contains_key(&key)) {
        return Ok(key);
    }
    if let Some(message) = renamed_message("route", python_strip(name)) {
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

/// Executable resolution belongs to the composing OS owner. In particular it
/// must use the host access check and Python's default PATH when PATH is absent;
/// inspecting permission bits here cannot reproduce shutil.which.
pub trait BinaryLocator {
    fn which(&self, command: &str) -> Option<std::path::PathBuf>;
}
pub const BINARIES: &[(&str, &str)] = &[("cursor", "cursor-agent"), ("kiro", "kiro-cli")];
pub fn default_installed(adapter: &str, locator: &dyn BinaryLocator) -> bool {
    let command = BINARIES
        .iter()
        .find(|(name, _)| *name == adapter)
        .map(|(_, binary)| *binary)
        .unwrap_or(adapter);
    locator.which(command).is_some()
}

pub use crate::render::{render, RenderError};

/// Resolve explicit adapter prefixes before inferring an owner from a bare id.
pub fn resolve_model(
    resolver: &crate::matching::ModelCatalog<'_>,
    text: &str,
) -> Result<(String, String, bool), crate::matching::MatchError> {
    let text = python_strip(text);
    let adapters = resolver.catalog.get("adapters");
    if let Some((head, rest)) = text.split_once('/') {
        if adapters.and_then(|a| a.get(head)).is_some() {
            for candidate in [rest, text] {
                if let Some(entry) = resolver.registered_match(head, candidate)?.0 {
                    let model = entry
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or(crate::matching::MatchError::Catalog("model id"))?;
                    return Ok((head.into(), model.into(), true));
                }
            }
            return Ok((
                head.into(),
                if known_provider_path(text) && !rest.contains('/') {
                    text.into()
                } else {
                    rest.into()
                },
                false,
            ));
        }
    }
    let adapter = resolver.owner_adapter(text)?;
    let entry = if adapters.and_then(|a| a.get(&adapter)).is_some() {
        resolver.registered_match(&adapter, text)?.0
    } else {
        None
    };
    let model = entry
        .as_ref()
        .and_then(|e| e.get("id"))
        .and_then(Value::as_str)
        .unwrap_or(text)
        .to_owned();
    Ok((adapter, model, entry.is_some()))
}
pub fn split_model(
    resolver: &crate::matching::ModelCatalog<'_>,
    text: &str,
) -> Result<(String, String), crate::matching::MatchError> {
    let (a, m, _) = resolve_model(resolver, text)?;
    Ok((a, m))
}
pub fn model_traits(
    resolver: &crate::matching::ModelCatalog<'_>,
    adapter: &str,
    model: &str,
) -> Result<Vec<String>, crate::matching::MatchError> {
    use crate::matching::{is_free_model, looks_free, training_flag};
    let mut traits = BTreeSet::new();
    let config = resolver
        .catalog
        .get("adapters")
        .and_then(|a| a.get(adapter))
        .unwrap_or(&Value::Null);
    let registered = if config.get("models").is_some() {
        resolver.registered_match(adapter, model)?.0
    } else {
        None
    };
    let mut names = vec![model.to_owned()];
    if let Some(entry) = &registered {
        if let Some(name) = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            names.push(name.into());
        }
        if let Some(inherits) = entry.get("inherits").and_then(Value::as_array) {
            names.extend(inherits.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    for name in names {
        for key in [format!("{adapter}/{name}"), name] {
            if let Some(values) = resolver
                .catalog
                .get("model_traits")
                .and_then(|t| t.get(&key))
                .and_then(Value::as_array)
            {
                traits.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
    }
    if training_flag(config, registered.as_ref(), model)? == Some(true) {
        traits.insert("trains-on-prompts".into());
    }
    let cap = registered.as_ref().and_then(|e| e.get("plan_cap_usd"));
    let zero = cap.is_some_and(|v| match v {
        Value::Bool(b) => !*b,
        Value::Number(n) if n.is_f64() => n.as_f64() == Some(0.0),
        Value::Number(n) => n.as_i64() == Some(0) || n.as_u64() == Some(0),
        _ => false,
    });
    if zero || is_free_model(config, model)? || (registered.is_none() && looks_free(config, model)?)
    {
        traits.insert("free".into());
    }
    if traits.contains("free")
        && registered.as_ref().and_then(|e| e.get("trains_on_prompts")) != Some(&Value::Bool(false))
    {
        traits.insert("trains-on-prompts".into());
    }
    Ok(traits.into_iter().collect())
}
pub fn spread_family(model: &str, catalog: &Value) -> Result<String, crate::catalog::CatalogError> {
    Ok(crate::catalog::infer_family(model, catalog)?
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            crate::catalog::model_slug_for_family(model)
                .split('-')
                .next()
                .unwrap_or_default()
                .to_owned()
        }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::collections::BTreeMap;

    use super::{
        effort_band, entry_is_valid, merge_route, migrate_renamed, reject_renamed, weight_value,
        PoolError, RenamedNameError,
    };

    #[test]
    fn integer_weights_retain_exact_integer_wire_form() {
        for wire in ["9007199254740993", "18446744073709551615", "0", "8"] {
            let raw = crate::pending::from_json(wire).unwrap();
            assert_eq!(
                serde_json::to_string(&weight_value(&raw).unwrap()).unwrap(),
                wire
            );
        }
        assert_eq!(
            serde_json::to_string(&weight_value(&json!("high")).unwrap()).unwrap(),
            "8"
        );
        assert_eq!(
            serde_json::to_string(&weight_value(&json!(4.5)).unwrap()).unwrap(),
            "4.5"
        );
    }

    #[test]
    fn pool_weights_and_effort_bands_reject_invalid_values() {
        assert_eq!(
            weight_value(&json!(" HIGH ")),
            Some(serde_json::Number::from(8))
        );
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
        let merged = merge_route(&base, &overlay, "routes.senior", &mut drift, &order).unwrap();
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
        assert!(matches!(
            reject_renamed(&json!({"routes":{"strong":[]}}), "overlay"),
            Err(RenamedNameError(message)) if message.contains("renamed to 'senior'")
        ));
    }
}

#[cfg(test)]
mod model_policy_tests {
    use super::*;
    use crate::matching::ModelCatalog;
    use serde_json::json;
    #[test]
    fn adapter_paths_and_live_inherited_privacy_match_python() {
        let mut catalog: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../config/model-routing.json"
        )))
        .unwrap();
        catalog["model_traits"] =
            json!({"codex/gpt-6.1-sol":["trains-on-prompts"],"agy/gemini-3.8-flash":["private"]});
        let cached = BTreeMap::from([
            ("codex".into(), vec!["gpt-6.2-sol".into()]),
            ("agy".into(), vec!["gemini-3.9-flash-high".into()]),
        ]);
        let resolver = ModelCatalog {
            catalog: &catalog,
            cached: &cached,
        };
        let models = [
            "codex/sol",
            "gpt-6.2-sol",
            "claude/sonnet",
            "opencode/big-pickle",
            "opencode/opencode-go/kimi-k2",
            "opencode-go/kimi-k2",
            "openrouter/unknown",
            "unknown",
            "agy/gemini-3.9-flash-high",
        ];
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"
import json,sys,pathlib,tempfile
from datetime import datetime,timezone
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route as m
d=json.load(sys.stdin)
class Clock(datetime):
 @classmethod
 def now(cls,tz=None): return cls(2026,10,23,tzinfo=timezone.utc)
m.model_route_live.datetime=Clock
with tempfile.TemporaryDirectory() as home:
 root=pathlib.Path(home);(root/'capabilities.json').write_text(json.dumps({a:{'observed_at':'2026-10-23T00:00:00Z','models':v} for a,v in d['cached'].items()}))
 m.model_route_probe._state_root=lambda:root
 result=[]
 for model in d['models']:
  a,model,known=m._pools.resolve_model(m,d['catalog'],model)
  result.append([a,model,known,m._pools.model_traits(m,d['catalog'],a,model)])
 print(json.dumps(result))
"#,
            &json!({"catalog":catalog,"cached":cached,"models":models}),
        ) else {
            return;
        };
        let actual: Vec<_> = models
            .iter()
            .map(|model| {
                let (a, m, known) = crate::pools::resolve_model(&resolver, model).unwrap();
                json!([
                    a,
                    m,
                    known,
                    crate::pools::model_traits(&resolver, &a, &m).unwrap()
                ])
            })
            .collect();
        assert_eq!(json!(actual), expected);
    }
}
