//! Route fallback policy and execution-facing candidate contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

const ADAPTERS: [&str; 7] = [
    "claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot",
];

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FallbackCandidate {
    pub adapter: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackPolicy {
    #[default]
    Default,
    Disabled,
    Enabled,
    Any,
    Explicit(Vec<FallbackCandidate>),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FallbackError {
    #[error("fallback entries must be adapter/model[@effort] or route objects")]
    InvalidEntry,
    #[error("fallback entry requires an adapter and non-empty model")]
    MissingModel,
    #[error("fallback effort must be a string")]
    InvalidEffort,
    #[error("fallback must be false, true, any or a JSON route list")]
    InvalidPolicy,
}

pub fn parse_candidate(
    raw: &Value,
    default_adapter: &str,
) -> Result<FallbackCandidate, FallbackError> {
    let mut object = match raw {
        Value::String(text) => {
            let (mut model, effort) = match text.split_once('@') {
                Some((model, effort)) => (model.to_owned(), Some(effort.to_owned())),
                None => (text.clone(), None),
            };
            let mut adapter = default_adapter.to_owned();
            if let Some((prefix, rest)) = model.split_once('/') {
                if ADAPTERS.contains(&prefix) {
                    adapter = prefix.to_owned();
                    model = rest.to_owned();
                }
            }
            let mut object = Map::from_iter([
                ("adapter".into(), Value::String(adapter)),
                ("model".into(), Value::String(model)),
            ]);
            if let Some(effort) = effort {
                object.insert("effort".into(), Value::String(effort));
            }
            object
        }
        Value::Object(object) => object.clone(),
        _ => return Err(FallbackError::InvalidEntry),
    };
    let model = ["model", "resolved_model", "id"]
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .find(|model| !model.trim().is_empty())
        .map(str::to_owned)
        .ok_or(FallbackError::MissingModel)?;
    let adapter = object
        .get("adapter")
        .and_then(Value::as_str)
        .unwrap_or(default_adapter)
        .to_owned();
    if !ADAPTERS.contains(&adapter.as_str()) {
        return Err(FallbackError::MissingModel);
    }
    let effort = match object.get("effort") {
        None | Some(Value::Null) => None,
        Some(Value::String(effort)) => Some(effort.clone()),
        Some(_) => return Err(FallbackError::InvalidEffort),
    };
    object.insert("adapter".into(), Value::String(adapter.clone()));
    object.insert("model".into(), Value::String(model.clone()));
    let extra = object
        .into_iter()
        .filter(|(key, _)| !["adapter", "model", "effort"].contains(&key.as_str()))
        .collect();
    Ok(FallbackCandidate {
        adapter,
        model,
        effort,
        extra,
    })
}

pub fn validate_policy(raw: &Value) -> Result<FallbackPolicy, FallbackError> {
    if let Value::String(text) = raw {
        if text == "any" {
            return Ok(FallbackPolicy::Any);
        }
        let parsed: Value = serde_json::from_str(text).map_err(|_| FallbackError::InvalidPolicy)?;
        return validate_policy(&parsed);
    }
    match raw {
        Value::Null => Ok(FallbackPolicy::Default),
        Value::Bool(false) => Ok(FallbackPolicy::Disabled),
        Value::Bool(true) => Ok(FallbackPolicy::Enabled),
        Value::Array(entries) => entries
            .iter()
            .map(|item| parse_candidate(item, "codex"))
            .collect::<Result<Vec<_>, _>>()
            .map(FallbackPolicy::Explicit),
        _ => Err(FallbackError::InvalidPolicy),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCandidate {
    pub adapter: String,
    pub model: String,
    #[serde(default)]
    pub traits: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRoutePlan {
    pub primary: ExecutionCandidate,
    #[serde(default)]
    pub fallbacks: Vec<ExecutionCandidate>,
    #[serde(default)]
    pub confidential: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_candidate, validate_policy, FallbackPolicy};

    #[test]
    fn fallback_candidate_strings_preserve_nested_model_paths_and_effort() {
        let parsed =
            parse_candidate(&json!("opencode/openrouter/vendor/model@high"), "codex").unwrap();
        assert_eq!(parsed.adapter, "opencode");
        assert_eq!(parsed.model, "openrouter/vendor/model");
        assert_eq!(parsed.effort.as_deref(), Some("high"));
    }

    #[test]
    fn fallback_policy_accepts_only_the_supported_shapes() {
        assert_eq!(
            validate_policy(&json!(false)).unwrap(),
            FallbackPolicy::Disabled
        );
        assert_eq!(validate_policy(&json!("any")).unwrap(), FallbackPolicy::Any);
        assert!(validate_policy(&json!([{"adapter":"bogus","model":"x"}])).is_err());
        assert!(parse_candidate(&json!({"adapter":"codex","model":"  "}), "codex").is_err());
    }
}
