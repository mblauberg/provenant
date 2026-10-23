//! Fallback policy wire types and execution-facing route request contracts.

use crate::{
    ordered_json::{OrderedObject, OrderedValue},
    pending::python_strip,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

const ADAPTERS: [&str; 7] = [
    "claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot",
];

/// A validated candidate retains every raw field, including nulls and dictionary order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FallbackCandidate {
    pub fields: OrderedObject,
}
impl FallbackCandidate {
    pub fn adapter(&self) -> &str {
        self.fields
            .get("adapter")
            .and_then(OrderedValue::as_str)
            .unwrap_or_default()
    }
    pub fn model(&self) -> &str {
        self.fields
            .get("model")
            .and_then(OrderedValue::as_str)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum FallbackPolicy {
    #[default]
    Default,
    Disabled,
    Enabled,
    Any,
    /// Python validates each entry, then returns the original list unchanged.
    Explicit(Vec<OrderedValue>),
}
impl Serialize for FallbackPolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Default => serializer.serialize_none(),
            Self::Disabled => serializer.serialize_bool(false),
            Self::Enabled => serializer.serialize_bool(true),
            Self::Any => serializer.serialize_str("any"),
            Self::Explicit(entries) => entries.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for FallbackPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = OrderedValue::deserialize(deserializer)?;
        validate_policy_value(&raw).map_err(serde::de::Error::custom)
    }
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
    #[error("JSON integer is outside the i64/u64 range")]
    NumberOutOfRange,
}

pub fn parse_candidate(
    raw: &OrderedValue,
    default_adapter: &str,
) -> Result<FallbackCandidate, FallbackError> {
    let mut object = match raw {
        OrderedValue::String(text) => {
            let (mut model, effort) = match text.split_once('@') {
                Some((model, effort)) => (model, Some(effort)),
                None => (text.as_str(), None),
            };
            let mut adapter = default_adapter;
            if let Some((prefix, rest)) = model.split_once('/') {
                if ADAPTERS.contains(&prefix) {
                    adapter = prefix;
                    model = rest;
                }
            }
            let mut object = OrderedObject(vec![
                ("adapter".into(), OrderedValue::String(adapter.into())),
                ("model".into(), OrderedValue::String(model.into())),
            ]);
            if let Some(effort) = effort {
                object.insert("effort".into(), OrderedValue::String(effort.into()));
            }
            object
        }
        OrderedValue::Object(object) => object.clone(),
        _ => return Err(FallbackError::InvalidEntry),
    };
    let model = ["model", "resolved_model", "id"]
        .iter()
        .filter_map(|key| object.get(key))
        .find(|value| crate::pending::truthy(value))
        .and_then(OrderedValue::as_str)
        .filter(|model| !python_strip(model).is_empty())
        .ok_or(FallbackError::MissingModel)?
        .to_owned();
    let adapter = match object.get("adapter") {
        Some(OrderedValue::String(adapter)) => adapter.as_str(),
        Some(_) => return Err(FallbackError::MissingModel),
        None => default_adapter,
    };
    if !ADAPTERS.contains(&adapter) {
        return Err(FallbackError::MissingModel);
    }
    if !matches!(
        object.get("effort"),
        None | Some(OrderedValue::Null | OrderedValue::String(_))
    ) {
        return Err(FallbackError::InvalidEffort);
    }
    let adapter = adapter.to_owned();
    object.insert("adapter".into(), OrderedValue::String(adapter));
    object.insert("model".into(), OrderedValue::String(model));
    Ok(FallbackCandidate { fields: object })
}

pub fn validate_policy(raw: &OrderedValue) -> Result<FallbackPolicy, FallbackError> {
    if let OrderedValue::String(text) = raw {
        if text == "any" {
            return Ok(FallbackPolicy::Any);
        }
        let parsed = crate::pending::from_json(text).map_err(|error| match error {
            crate::pending::JsonError::NumberOutOfRange => FallbackError::NumberOutOfRange,
            crate::pending::JsonError::Invalid(_) => FallbackError::InvalidPolicy,
        })?;
        return validate_policy_value(&parsed);
    }
    validate_policy_value(raw)
}
fn validate_policy_value(raw: &OrderedValue) -> Result<FallbackPolicy, FallbackError> {
    match raw {
        OrderedValue::String(text) if text == "any" => Ok(FallbackPolicy::Any),
        OrderedValue::Null => Ok(FallbackPolicy::Default),
        OrderedValue::Bool(false) => Ok(FallbackPolicy::Disabled),
        OrderedValue::Bool(true) => Ok(FallbackPolicy::Enabled),
        OrderedValue::Array(entries) => {
            for entry in entries {
                parse_candidate(entry, "codex")?;
            }
            Ok(FallbackPolicy::Explicit(entries.clone()))
        }
        _ => Err(FallbackError::InvalidPolicy),
    }
}

/// The route requests returned by Python candidates(), including an explicit null effort.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCandidate {
    pub adapter: String,
    pub model: String,
    pub effort: Option<String>,
}

/// Input plan consumed by candidates(); confidentiality and policy are separate arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRoutePlan {
    #[serde(default)]
    pub requested_model: Option<String>,
    pub route: OrderedObject,
    pub adapter: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(flatten)]
    pub extra: OrderedObject,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{FallbackCandidate, FallbackError, FallbackPolicy};
    fn parse_candidate(
        raw: &serde_json::Value,
        adapter: &str,
    ) -> Result<FallbackCandidate, FallbackError> {
        super::parse_candidate(&raw.clone().into(), adapter)
    }
    fn validate_policy(raw: &serde_json::Value) -> Result<FallbackPolicy, FallbackError> {
        super::validate_policy(&raw.clone().into())
    }

    #[test]
    fn fallback_json_string_preserves_typed_integer_range_errors() {
        let raw = crate::ordered_json::OrderedValue::String(
            r#"[{"model":"x","meta":18446744073709551616}]"#.into(),
        );
        assert_eq!(
            super::validate_policy(&raw),
            Err(FallbackError::NumberOutOfRange)
        );
    }

    #[test]
    fn fallback_wire_and_candidate_dicts_match_the_python_source() {
        let wires = vec![
            r#"null"#,
            r#"true"#,
            r#"false"#,
            r#""any""#,
            r#"["opencode/vendor/model@high",{"z":0,"effort":null,"id":"x","a":2}]"#,
            r#"{"effort":null,"id":"x","extra":{"z":1,"a":2}}"#,
            r#"{"model":"x","adapter":"claude","effort":""}"#,
            r#"{"model":[],"resolved_model":"x"}"#,
            r#"{"model":true,"id":"x"}"#,
            r#"{"adapter":null,"model":"x"}"#,
            r#"{"model":"x","effort":1}"#,
            r#""opencode/openrouter/vendor/model@high""#,
            r#""\u001c\u001f""#,
        ];
        let Some(expected): Option<Vec<Vec<String>>> = crate::oracle::python(
            "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nanswer=[]\nfor wire in json.load(sys.stdin):\n row=[]\n for fn in (m.validate_policy,lambda value:m.parse_candidate(value,'codex')):\n  try: row.append(json.dumps(fn(json.loads(wire)),ensure_ascii=False,separators=(',',':')))\n  except ValueError as error: row.append('error:'+str(error))\n answer.append(row)\nprint(json.dumps(answer))",
            &serde_json::json!(wires),
        ) else {
            return;
        };
        let actual: Vec<Vec<String>> = wires
            .iter()
            .map(|wire| {
                let raw = crate::pending::from_json(wire).unwrap();
                vec![
                    super::validate_policy(&raw)
                        .map(|value| serde_json::to_string(&value).unwrap())
                        .unwrap_or_else(|error| format!("error:{error}")),
                    super::parse_candidate(&raw, "codex")
                        .map(|value| serde_json::to_string(&value).unwrap())
                        .unwrap_or_else(|error| format!("error:{error}")),
                ]
            })
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn policies_round_trip_python_wire_forms_without_normalizing_the_raw_list() {
        for wire in [
            "null",
            "true",
            "false",
            r#""any""#,
            r#"["opencode/vendor/model@high",{"z":1,"effort":null,"id":"x","meta":{"z":0,"a":1}}]"#,
        ] {
            let policy: FallbackPolicy = crate::pending::from_json(wire).unwrap();
            assert_eq!(serde_json::to_string(&policy).unwrap(), wire);
            let raw = crate::pending::from_json(wire).unwrap();
            assert_eq!(super::validate_policy(&raw).unwrap(), policy);
        }
        assert!(serde_json::from_str::<FallbackPolicy>(r#"{"explicit":[]}"#).is_err());
    }

    #[test]
    fn parsed_candidates_keep_null_and_insertion_order() {
        let raw = crate::pending::from_json(
            r#"{"z":0,"effort":null,"resolved_model":"x","adapter":"claude","a":2}"#,
        )
        .unwrap();
        let candidate = super::parse_candidate(&raw, "codex").unwrap();
        assert_eq!(
            serde_json::to_string(&candidate).unwrap(),
            r#"{"z":0,"effort":null,"resolved_model":"x","adapter":"claude","a":2,"model":"x"}"#
        );
        let raw =
            crate::pending::from_json(r#"{"model":"x","effort":null,"adapter":"claude"}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&super::parse_candidate(&raw, "codex").unwrap()).unwrap(),
            r#"{"model":"x","effort":null,"adapter":"claude"}"#
        );
        assert!(
            super::parse_candidate(
                &crate::ordered_json::OrderedValue::String("\u{1c}\u{1f}".into()),
                "codex"
            )
            .is_err()
        );
    }

    #[test]
    fn execution_interfaces_match_plan_inputs_and_route_request_outputs() {
        let wire = r#"{"requested_model":"gpt-6.1-sol","route":{"alias":"senior","fallback_candidates":["claude/sonnet"]},"adapter":"codex","model":"gpt-6.1-sol","effort":"high","task":"work"}"#;
        let plan: super::ExecutionRoutePlan = crate::pending::from_json(wire).unwrap();
        assert_eq!(plan.requested_model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(
            plan.route
                .get("alias")
                .and_then(crate::ordered_json::OrderedValue::as_str),
            Some("senior")
        );
        assert_eq!(serde_json::to_string(&plan).unwrap(), wire);
        let candidate: super::ExecutionCandidate =
            serde_json::from_str(r#"{"adapter":"claude","model":"sonnet","effort":null}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&candidate).unwrap(),
            r#"{"adapter":"claude","model":"sonnet","effort":null}"#
        );
    }

    #[test]
    fn fallback_policy_wire_null_matches_python() {
        assert_eq!(
            serde_json::to_string(&FallbackPolicy::Default).unwrap(),
            "null"
        );
    }

    #[test]
    fn fallback_candidate_strings_preserve_nested_model_paths_and_effort() {
        let parsed =
            parse_candidate(&json!("opencode/openrouter/vendor/model@high"), "codex").unwrap();
        assert_eq!(parsed.adapter(), "opencode");
        assert_eq!(parsed.model(), "openrouter/vendor/model");
        assert_eq!(
            parsed
                .fields
                .get("effort")
                .and_then(crate::ordered_json::OrderedValue::as_str),
            Some("high")
        );
    }

    #[test]
    fn fallback_policy_accepts_only_the_supported_shapes() {
        assert_eq!(
            validate_policy(&json!(false)).unwrap(),
            FallbackPolicy::Disabled
        );
        assert_eq!(validate_policy(&json!("any")).unwrap(), FallbackPolicy::Any);
        assert_eq!(
            validate_policy(&json!(r#""any""#)).unwrap(),
            FallbackPolicy::Any
        );
        assert_eq!(
            validate_policy(&json!("false")).unwrap(),
            FallbackPolicy::Disabled
        );
        assert_eq!(
            validate_policy(&json!(r#""false""#)),
            Err(super::FallbackError::InvalidPolicy)
        );
        assert!(validate_policy(&json!([{"adapter":"bogus","model":"x"}])).is_err());
        assert!(parse_candidate(&json!({"adapter":"codex","model":"  "}), "codex").is_err());
    }

    #[test]
    fn fallback_object_fields_match_python_truthiness_and_presence() {
        assert!(parse_candidate(&json!({"adapter":null,"model":"x"}), "codex").is_err());
        assert!(parse_candidate(&json!({"model":42,"id":"x"}), "codex").is_err());
        assert!(parse_candidate(&json!({"model":" ","id":"x"}), "codex").is_err());
        assert_eq!(
            parse_candidate(&json!({"model":[],"id":"x"}), "codex")
                .unwrap()
                .model(),
            "x"
        );
        assert_eq!(
            parse_candidate(&json!({"model":"x"}), "codex")
                .unwrap()
                .adapter(),
            "codex"
        );
    }
}
