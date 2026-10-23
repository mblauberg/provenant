//! Catalogue family inference and risk-tier override validation.

use std::collections::{BTreeSet, HashMap};

use regex::Regex;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::casefold::fold;

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
    let mut slug = model.trim().to_lowercase();
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
    let patterns = catalog
        .get("model_patterns")
        .and_then(Value::as_array)
        .ok_or(CatalogError::InvalidPatterns)?;
    let candidates = [model_slug_for_family(model), model.trim().to_lowercase()];
    let mut matches = Vec::new();
    for candidate in candidates.into_iter().filter(|item| !item.is_empty()) {
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
    let lowered = model.trim().to_lowercase();
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
                    role.as_str()
                        .is_some_and(|name| !name.trim().is_empty() && name == name.trim())
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
                .all(|name| !name.trim().is_empty() && *name == name.trim())
                && names[0] != names[1]
        })
        && models.len() == 1
        && model.is_some_and(|name| !name.trim().is_empty() && name == name.trim())
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

pub fn risk_tier_overrides_are_valid(family: &str, family_config: &Value, catalog: &Value) -> bool {
    let Some(config) = family_config.as_object() else {
        return false;
    };
    let Some(overrides) = config.get("risk_tier_overrides") else {
        return true;
    };
    let Some(overrides) = overrides.as_object() else {
        return false;
    };
    if overrides.is_empty() {
        return true;
    }
    let candidates = family_alias_candidates(family_config, catalog, family);
    for override_value in overrides.values() {
        if !risk_tier_override_is_well_formed(override_value) {
            return false;
        }
        let occupant = override_value["models"][0].as_str().unwrap_or_default();
        if ALIASES.contains(&occupant)
            || infer_family(occupant, catalog).ok().flatten().is_none()
            || candidates.iter().any(|candidate| {
                model_has_alias(candidate, occupant)
                    || model_has_alias(occupant, candidate)
                    || risk_tier_override_reserves_model(candidate, occupant)
            })
        {
            return false;
        }
    }
    true
}

pub fn override_scan_families(model: &str, catalog: &Value) -> HashMap<String, Value> {
    let Some(families) = catalog.get("families").and_then(Value::as_object) else {
        return HashMap::new();
    };
    let family = if model.is_empty() {
        None
    } else {
        infer_family(model, catalog).ok().flatten()
    };
    if let Some(name) = family {
        if let Some(config) = families
            .get(&name)
            .filter(|value| value.as_object().is_some_and(|map| !map.is_empty()))
        {
            return HashMap::from([(name, config.clone())]);
        }
    }
    families
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
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
        use std::path::Path;
        use std::process::Command;

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = manifest.join("../../../../scripts/model_route_catalog.py");
        let fixture = json!({"catalog":{"model_patterns":[
            {"pattern":"deepseek","family":"deepseek"},
            {"pattern":"kimi","family":"moonshot"}
        ]},"models":["opencode/deepseek-v3","openrouter/moonshotai/kimi-k2","openrouter/stealth/model"]});
        let code = "import importlib.util,json,sys; p=sys.argv[1]; s=importlib.util.spec_from_file_location('model_route_catalog',p); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); d=json.loads(sys.argv[2]); print(json.dumps([list(m.matching_model_families(x,d['catalog'])) for x in d['models']]))";
        let output = match Command::new("python3")
            .args([
                "-c",
                code,
                source.to_str().unwrap_or_default(),
                &fixture.to_string(),
            ])
            .output()
        {
            Ok(output) => output,
            Err(_) => return,
        };
        assert!(
            output.status.success(),
            "python oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Vec<Vec<String>> = serde_json::from_slice(&output.stdout).unwrap();
        let catalog = &fixture["catalog"];
        let actual: Vec<Vec<String>> = fixture["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| matching_model_families(model.as_str().unwrap(), catalog).unwrap())
            .collect();
        assert_eq!(actual, expected);
    }
}
