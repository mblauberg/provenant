//! Catalogue family inference and risk-tier override validation.

use std::collections::BTreeSet;

use regex::Regex;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{
    casefold::fold,
    ordered_json::{OrderedObject, OrderedValue},
    pending::python_strip,
};

const BROKER_PREFIXES: [&str; 3] = ["opencode/", "opencode-go/", "openrouter/"];
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "ultra"];
const ALIASES: [&str; 3] = ["junior", "senior", "principal"];

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalogue model_patterns must be an array")]
    InvalidPatterns,
    #[error("catalogue pattern entry is malformed")]
    InvalidPatternEntry,
    #[error("invalid catalogue regular expression: {0}")]
    InvalidRegex(#[from] regex::Error),
}

/// Strip repeated broker prefixes and leading OpenRouter shorthand.
pub fn model_slug_for_family(model: &str) -> String {
    let mut slug = python_strip(model).to_lowercase();
    while let Some(rest) = slug.strip_prefix('~') {
        slug = rest.to_owned();
    }
    for _ in 0..8 {
        if let Some(prefix) = BROKER_PREFIXES
            .iter()
            .find(|prefix| slug.starts_with(**prefix))
        {
            slug = slug[prefix.len()..].trim_start_matches('~').to_owned();
        } else {
            break;
        }
    }
    slug
}

pub fn infer_family(model: &str, catalog: &Value) -> Result<Option<String>, CatalogError> {
    Ok(matching_model_families(model, catalog)?.into_iter().next())
}

pub fn matching_model_families(model: &str, catalog: &Value) -> Result<Vec<String>, CatalogError> {
    let candidates = [
        model_slug_for_family(model),
        python_strip(model).to_lowercase(),
    ];
    let mut matches = Vec::new();
    for candidate in candidates.into_iter().filter(|item| !item.is_empty()) {
        let patterns = catalog
            .get("model_patterns")
            .and_then(Value::as_array)
            .ok_or(CatalogError::InvalidPatterns)?;
        for item in patterns {
            let pattern = item
                .get("pattern")
                .and_then(Value::as_str)
                .ok_or(CatalogError::InvalidPatternEntry)?;
            let family = item
                .get("family")
                .and_then(Value::as_str)
                .ok_or(CatalogError::InvalidPatternEntry)?;
            if Regex::new(pattern)?.is_match(&candidate)
                && !matches.iter().any(|found| found == family)
            {
                matches.push(family.to_owned());
            }
        }
    }
    Ok(matches)
}

pub fn family_is_assurance_eligible(family: Option<&str>, family_source: &str) -> bool {
    family.is_some_and(|name| !name.is_empty() && !matches!(name, "generic-open" | "open-weight"))
        && family_source != "unresolved"
}

pub fn attribute_model_family(
    model: &str,
    catalog: &Value,
    endpoint_family: Option<&str>,
) -> Result<(Option<String>, String), CatalogError> {
    if let Some(family) = infer_family(model, catalog)? {
        return Ok((Some(family), "slug-inferred".into()));
    }
    let lowered = python_strip(model).to_lowercase();
    if BROKER_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
    {
        if let Some(family) = endpoint_family.filter(|family| !family.is_empty()) {
            return Ok((Some(family.into()), "endpoint-profile-fallback".into()));
        }
        return Ok((Some("generic-open".into()), "broker-default".into()));
    }
    if let Some(family) = endpoint_family.filter(|family| !family.is_empty()) {
        return Ok((Some(family.into()), "endpoint-profile-fallback".into()));
    }
    Ok((None, "unresolved".into()))
}

pub fn model_has_alias(model: &str, alias: &str) -> bool {
    fold(model).contains(&fold(alias))
}

pub fn risk_tier_override_reserves_model(model: &str, occupant: &str) -> bool {
    if model_has_alias(model, occupant) {
        return true;
    }
    fn stable_name(identifier: &str) -> Vec<String> {
        let words: Vec<_> = fold(identifier)
            .split(|ch: char| !ch.is_ascii_lowercase())
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect();
        if words.len() > 1 && words[0] == "claude" {
            words[1..].to_vec()
        } else {
            words
        }
    }
    let occupant_name = stable_name(occupant);
    !occupant_name.is_empty() && stable_name(model) == occupant_name
}

pub fn capability_key_matches_model(
    adapter: &str,
    key: &str,
    resolved_model: &str,
    is_alias: bool,
) -> bool {
    let normalized_key = fold(key);
    let normalized_model = fold(resolved_model);
    if adapter == "claude" {
        if is_alias {
            return normalized_model.starts_with(&format!("claude-{normalized_key}-"));
        }
        if normalized_key.starts_with("claude-") {
            return normalized_model == normalized_key;
        }
        return normalized_model.starts_with("claude-")
            && normalized_model
                .split('-')
                .any(|part| part == normalized_key);
    }
    normalized_model == normalized_key
}

pub fn ultra_eligible_roles_are_valid(family_config: &Value) -> bool {
    if !family_config.is_object() {
        return false;
    }
    family_config
        .get("ultra_eligible_roles")
        .is_none_or(|roles| {
            roles.as_array().is_some_and(|roles| {
                roles.iter().all(|role| {
                    role.as_str().is_some_and(|name| {
                        !python_strip(name).is_empty() && name == python_strip(name)
                    })
                })
            })
        })
}

pub fn risk_tier_override_is_well_formed(override_value: &Value) -> bool {
    let Some(alias) = override_value.get("alias").and_then(Value::as_str) else {
        return false;
    };
    let Some(default_effort) = override_value.get("default_effort").and_then(Value::as_str) else {
        return false;
    };
    let Some(maximum_effort) = override_value.get("maximum_effort").and_then(Value::as_str) else {
        return false;
    };
    let Some(roles) = override_value.get("roles").and_then(Value::as_array) else {
        return false;
    };
    let Some(models) = override_value.get("models").and_then(Value::as_array) else {
        return false;
    };
    let (Some(default_index), Some(maximum_index)) = (
        EFFORTS.iter().position(|v| *v == default_effort),
        EFFORTS.iter().position(|v| *v == maximum_effort),
    ) else {
        return false;
    };
    let role_names: Option<Vec<_>> = roles.iter().map(Value::as_str).collect();
    let model = models.first().and_then(Value::as_str);
    ALIASES.contains(&alias)
        && default_index <= maximum_index
        && roles.len() == 2
        && role_names.as_ref().is_some_and(|names| {
            names
                .iter()
                .all(|name| !python_strip(name).is_empty() && *name == python_strip(name))
                && names[0] != names[1]
        })
        && models.len() == 1
        && model.is_some_and(|name| !python_strip(name).is_empty() && name == python_strip(name))
}

pub fn family_alias_candidates(
    family_config: &Value,
    catalog: &Value,
    family: &str,
) -> BTreeSet<String> {
    let mut candidates = BTreeSet::new();
    if let Some(aliases) = family_config.get("aliases").and_then(Value::as_object) {
        collect_string_arrays(aliases, &mut candidates);
    }
    if let Some(overrides) = family_config
        .get("role_overrides")
        .and_then(Value::as_object)
    {
        for role_aliases in overrides.values().filter_map(Value::as_object) {
            collect_string_arrays(role_aliases, &mut candidates);
        }
    }
    let prefixes = catalog
        .get("adapters")
        .and_then(Value::as_object)
        .map(|adapters| {
            adapters
                .iter()
                .filter_map(|(name, value)| {
                    (value.get("fixed_model_family").and_then(Value::as_str) == Some(family))
                        .then_some(name.clone())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let initial: Vec<_> = candidates.iter().cloned().collect();
    for prefix in prefixes {
        candidates.extend(
            initial
                .iter()
                .map(|candidate| format!("{prefix}-{candidate}")),
        );
    }
    candidates
}

fn collect_string_arrays(table: &Map<String, Value>, out: &mut BTreeSet<String>) {
    for values in table.values().filter_map(Value::as_array) {
        out.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
    }
}

pub fn risk_tier_overrides_are_valid(
    family: &str,
    family_config: &Value,
    catalog: &Value,
) -> Result<bool, CatalogError> {
    let Some(config) = family_config.as_object() else {
        return Ok(false);
    };
    let Some(overrides) = config.get("risk_tier_overrides") else {
        return Ok(true);
    };
    let Some(overrides) = overrides.as_object() else {
        return Ok(false);
    };
    if overrides.is_empty() {
        return Ok(true);
    }
    let candidates = family_alias_candidates(family_config, catalog, family);
    for override_value in overrides.values() {
        if !risk_tier_override_is_well_formed(override_value) {
            return Ok(false);
        }
        let occupant = override_value["models"][0].as_str().unwrap_or_default();
        if ALIASES.contains(&occupant)
            || infer_family(occupant, catalog)?.is_none()
            || candidates.iter().any(|candidate| {
                model_has_alias(candidate, occupant)
                    || model_has_alias(occupant, candidate)
                    || risk_tier_override_reserves_model(candidate, occupant)
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Preserve catalogue dictionary order at the wire boundary and in the scan result.
pub fn override_scan_families(
    model: &str,
    catalog: &OrderedValue,
) -> Result<OrderedObject, CatalogError> {
    let Some(families) = catalog.get("families").and_then(OrderedValue::as_object) else {
        return Ok(OrderedObject::default());
    };
    let family = if model.is_empty() {
        None
    } else {
        infer_family(model, &catalog.to_value())?
    };
    if let Some(name) = family {
        if let Some(config) = families
            .get(&name)
            .filter(|value| value.as_object().is_some_and(|object| !object.0.is_empty()))
        {
            return Ok(OrderedObject(vec![(name, config.clone())]));
        }
    }
    Ok(families.clone())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        attribute_model_family, capability_key_matches_model, family_is_assurance_eligible,
        matching_model_families, model_has_alias, model_slug_for_family,
        risk_tier_override_is_well_formed, risk_tier_override_reserves_model,
        ultra_eligible_roles_are_valid,
    };

    #[test]
    fn override_scan_keeps_family_order_and_propagates_malformed_patterns() {
        let catalog = crate::pending::from_json(r#"{"families":{"z":{"a":1},"a":{},"m":{}},"model_patterns":[{"pattern":"x","family":"z"}]}"#).unwrap();
        let all = super::override_scan_families("missing", &catalog).unwrap();
        assert_eq!(
            serde_json::to_string(&all).unwrap(),
            r#"{"z":{"a":1},"a":{},"m":{}}"#
        );
        let one = super::override_scan_families("x", &catalog).unwrap();
        assert_eq!(serde_json::to_string(&one).unwrap(), r#"{"z":{"a":1}}"#);
        let broken = crate::pending::from_json(r#"{"families":{"z":{}}}"#).unwrap();
        assert!(super::override_scan_families("", &broken).is_ok());
        assert!(super::override_scan_families("x", &broken).is_err());
    }

    #[test]
    fn empty_models_do_not_read_model_patterns() {
        for model in ["", " ", "\u{1c}\u{1f}"] {
            assert!(matching_model_families(model, &json!({}))
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn nested_broker_slugs_infer_the_vendor_family() {
        let catalog = json!({"model_patterns": [
            {"pattern": "deepseek", "family": "deepseek"},
            {"pattern": "kimi", "family": "moonshot"}
        ]});
        assert_eq!(
            model_slug_for_family("~openrouter/moonshotai/kimi-k2"),
            "moonshotai/kimi-k2"
        );
        assert_eq!(
            matching_model_families("openrouter/moonshotai/kimi-k2", &catalog).unwrap(),
            ["moonshot"]
        );
        assert_eq!(
            attribute_model_family("opencode/deepseek-v3", &catalog, None).unwrap(),
            (Some("deepseek".into()), "slug-inferred".into())
        );
    }

    #[test]
    fn broker_fallback_is_not_assurance_eligible() {
        let catalog = json!({"model_patterns": []});
        let (family, source) =
            attribute_model_family("openrouter/stealth/model", &catalog, None).unwrap();
        assert_eq!(family.as_deref(), Some("generic-open"));
        assert_eq!(source, "broker-default");
        assert!(!family_is_assurance_eligible(family.as_deref(), &source));
        assert!(!family_is_assurance_eligible(Some(""), "slug-inferred"));
        assert!(!family_is_assurance_eligible(None, "slug-inferred"));
    }

    #[test]
    fn ultra_eligible_roles_requires_a_family_object_and_valid_roles() {
        assert!(!ultra_eligible_roles_are_valid(&json!(null)));
        assert!(!ultra_eligible_roles_are_valid(&json!([])));
        assert!(!ultra_eligible_roles_are_valid(&json!("family")));
        assert!(ultra_eligible_roles_are_valid(&json!({})));
        assert!(ultra_eligible_roles_are_valid(
            &json!({"ultra_eligible_roles":["lead", "orchestrator"]})
        ));
        assert!(!ultra_eligible_roles_are_valid(
            &json!({"ultra_eligible_roles":[" lead"]})
        ));
    }

    #[test]
    fn empty_endpoint_family_uses_the_same_fallback_as_python() {
        let catalog = json!({"model_patterns": []});
        let result = attribute_model_family("openrouter/unknown", &catalog, Some("")).unwrap();
        assert_eq!(
            result,
            (Some("generic-open".into()), "broker-default".into())
        );
    }

    #[test]
    fn effort_override_shape_and_claude_capability_binding_are_checked() {
        let override_value = json!({"alias":"principal","default_effort":"high","maximum_effort":"ultra","roles":["lead","orchestrator"],"models":["claude-opus-4"]});
        assert!(risk_tier_override_is_well_formed(&override_value));
        assert!(!risk_tier_override_is_well_formed(
            &json!({"alias":"principal","default_effort":"ultra","maximum_effort":"high","roles":["lead","orchestrator"],"models":["claude-opus-4"]})
        ));
        assert!(capability_key_matches_model(
            "claude",
            "opus",
            "claude-opus-4-1",
            true
        ));
        assert!(!capability_key_matches_model(
            "codex", "gpt-5", "gpt-5.6", false
        ));
        assert!(model_has_alias("claude-ſonnet-4", "sonnet"));
        assert!(model_has_alias("ﬀ", "ff"));
        assert!(model_has_alias("µ", "μ"));
        assert!(capability_key_matches_model(
            "codex", "strasse", "straße", false
        ));
        assert!(risk_tier_override_reserves_model(
            "claude-straße-4",
            "claude-strasse-5"
        ));
        assert!(risk_tier_override_reserves_model(
            "claude-ǰ-4",
            "claude-ǰ-5"
        ));
    }

    #[test]
    fn family_matching_matches_the_python_oracle_when_available() {
        let catalogs = vec![
            json!({"model_patterns":[{"pattern":"deepseek","family":"deepseek"},{"pattern":"kimi","family":"moonshot"},{"pattern":"stealth","family":"generic-open"}]}),
            json!({"model_patterns":[{"pattern":"^gpt","family":"openai"},{"pattern":"claude","family":"anthropic"},{"pattern":"gpt|claude","family":"overlap"},{"pattern":"gpt","family":"openai"}]}),
            json!({"model_patterns":[]}),
            json!({}),
        ];
        let models = vec![
            "",
            " ",
            "\u{1c}\u{1f}",
            "gpt-6.1-sol",
            " GPT-6.1-SOL ",
            "claude-sonnet-5",
            "opencode/deepseek-v3",
            "openrouter/moonshotai/kimi-k2",
            "openrouter/stealth/model",
            "~openrouter/gpt-6",
            "opencode/openrouter/~~gpt-6",
            "opencode-go/opencode/deepseek-v3",
            "unknown",
            "\u{1c}GPT-6\u{1f}",
        ];
        let mut fixtures = Vec::new();
        for catalog in catalogs {
            for model in &models {
                // A missing patterns key is only valid when no candidate is visited.
                if catalog.get("model_patterns").is_some()
                    || crate::pending::python_strip(model).is_empty()
                {
                    fixtures.push(json!({"model":model,"catalog":catalog}));
                }
            }
        }
        let fixture = json!(fixtures);
        let Some(expected): Option<Vec<Vec<String>>> = crate::oracle::python(
            "import importlib.util,json,pathlib,sys; p=pathlib.Path('scripts/model_route_catalog.py').resolve(); s=importlib.util.spec_from_file_location('catalog',p); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); data=json.load(sys.stdin); print(json.dumps([m.matching_model_families(d['model'],d['catalog']) for d in data]))", &fixture
        ) else { return; };
        let actual: Vec<Vec<String>> = fixtures
            .iter()
            .map(|d| matching_model_families(d["model"].as_str().unwrap(), &d["catalog"]).unwrap())
            .collect();
        assert_eq!(actual, expected);
    }
}

#[cfg(test)]
mod policy_differential {
    use super::*;
    use serde_json::json;

    #[test]
    fn validators_and_reservations_match_python_on_malformed_and_valid_records() {
        let catalog = json!({"model_patterns":[{"pattern":"claude|sonnet|opus","family":"anthropic"}],"adapters":{"claude":{"fixed_model_family":"anthropic"}}});
        let valid = json!({"alias":"principal","default_effort":"high","maximum_effort":"ultra","roles":["lead","orchestrator"],"models":["claude-mythos-5"]});
        let mut overrides = vec![json!(null), json!([]), json!({}), valid.clone()];
        for (key, values) in [
            ("alias", vec![json!(1), json!("strong"), json!(null)]),
            (
                "roles",
                vec![
                    json!(["lead", "lead"]),
                    json!(["lead"]),
                    json!([" lead", "worker"]),
                    json!(["\u{1c}", "worker"]),
                    json!(null),
                ],
            ),
            (
                "models",
                vec![json!([]), json!(["x", "y"]), json!([1]), json!([" x"])],
            ),
            (
                "default_effort",
                vec![json!("unknown"), json!(true), json!("ultra")],
            ),
            ("maximum_effort", vec![json!("low"), json!(null)]),
        ] {
            for value in values {
                let mut item = valid.clone();
                item[key] = value;
                overrides.push(item);
            }
        }
        let families = vec![
            json!(null),
            json!([]),
            json!({}),
            json!({"risk_tier_overrides":null}),
            json!({"risk_tier_overrides":[]}),
            json!({"ultra_eligible_roles":null}),
            json!({"ultra_eligible_roles":["lead"]}),
            json!({"ultra_eligible_roles":["lead "]}),
            json!({"aliases":{"senior":["sonnet",3]},"role_overrides":{"review":{"principal":["opus"]}},"risk_tier_overrides":{"crucial":valid}}),
        ];
        let fixture = json!({"catalog":catalog,"overrides":overrides,"families":families});
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('catalog',pathlib.Path('scripts/model_route_catalog.py')); m=importlib.util.module_from_spec(s); s.loader.exec_module(m)
d=json.load(sys.stdin); c=d['catalog']
print(json.dumps({'overrides':[m.risk_tier_override_is_well_formed(x) for x in d['overrides']], 'roles':[m.ultra_eligible_roles_are_valid(x) for x in d['families']], 'families':[m.risk_tier_overrides_are_valid('anthropic',x,c) for x in d['families']]}))
"#,
            &fixture,
        ) else {
            return;
        };
        assert_eq!(
            json!({"overrides":overrides.iter().map(risk_tier_override_is_well_formed).collect::<Vec<_>>(),"roles":families.iter().map(ultra_eligible_roles_are_valid).collect::<Vec<_>>(),"families":families.iter().map(|x|risk_tier_overrides_are_valid("anthropic",x,&catalog).unwrap()).collect::<Vec<_>>()}),
            expected
        );
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn malformed_pattern_remains_an_error_during_override_validation() {
        let config = json!({"risk_tier_overrides":{"crucial":{"alias":"principal","default_effort":"high","maximum_effort":"ultra","roles":["lead","orchestrator"],"models":["occupant"]}}});
        assert!(matches!(
            risk_tier_overrides_are_valid(
                "family",
                &config,
                &json!({"model_patterns":[{"pattern":"[","family":"family"}]})
            ),
            Err(CatalogError::InvalidRegex(_))
        ));
    }
}
