//! Contracts for the dispatch cycle. Policy and effects remain with its owner.
//! Task/route objects are intentionally open: source validation is dynamic and
//! successful route records retain unknown members.
pub use crate::pending::{OrderedMap, OrderedValue};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct DispatchTask(pub OrderedMap<OrderedValue>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct FastRoute(pub OrderedMap<OrderedValue>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRejection {
    pub task_id: OrderedValue,
    pub error: String,
    pub fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status")]
pub enum PreflightOutcome {
    #[serde(rename = "validated")]
    Validated { routes: Vec<FastRoute> },
    #[serde(rename = "rejected")]
    Rejected {
        error: String,
        fix: String,
        errors: Vec<TaskRejection>,
    },
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{fix}")]
pub struct PreflightError {
    pub code: String,
    pub fix: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn source_results_retain_open_route_members_and_invalid_task_ids() {
        let cases = json!([
            {"status":"validated","routes":[{"status":"ok","resolved_model":"m","model_family":"openai","endpoint_provider":"openai","identity_source":"fixture","future":null,"effort":null}]},
            {"status":"validated","routes":[]},
            {"status":"rejected","error":"invalid_task_id","fix":"Pass unique task ids containing letters, numbers, '.', '_' or '-'.","errors":[{"task_id":[null],"error":"invalid_task_id","fix":"Pass unique task ids containing letters, numbers, '.', '_' or '-'."}]}
        ]);
        for value in cases.as_array().unwrap() {
            let contract: PreflightOutcome = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(contract).unwrap(), *value);
        }
        // These are source parse_fast_route_json outputs, not an invented route schema.
        let routes = json!([{"status":"ok","resolved_model":"m","model_family":"openai","endpoint_provider":"openai","identity_source":"fixture","new":false},{"status":null,"effort":null},{}]);
        if let Some(expected) = crate::test_support::oracle("preflight_contract", &routes) {
            for (actual, expected) in routes
                .as_array()
                .unwrap()
                .iter()
                .zip(expected.as_array().unwrap())
            {
                let typed: FastRoute = serde_json::from_value(actual.clone()).unwrap();
                assert_eq!(serde_json::to_value(typed).unwrap(), *expected);
            }
        }
    }
    #[test]
    fn raw_route_refusals_and_zero_normalisation_match_source() {
        let cases = json!([
            {"raw":"{\"x\":1,\"x\":2}"},
            {"raw":"{\"x\":{\"a\":1,\"a\":2}}"},
            {"raw":"{\"status\":true}"},
            {"raw":"{\"status\":\"ok\"}"},
            {"raw":"{\"effort\":42}"},
            {"raw":"[]"},
            {"raw":"{\"z\":-0,\"a\":[-0,0.0]}"}
        ]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|c| match FastRoute::from_json(c["raw"].as_str().unwrap()) {
                Ok(route) => json!({"wire":serde_json::to_string(&route).unwrap()}),
                Err(error) => json!({"error":error.to_string()}),
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("preflight_decode", &cases) {
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn raw_route_boundary_rejects_duplicates_and_overflow_without_rounding() {
        for raw in [r#"{"x":1,"x":2}"#, r#"{"x":{"a":1,"a":2}}"#] {
            assert!(matches!(
                FastRoute::from_json(raw),
                Err(RouteDecodeError::DuplicateMember(_))
            ));
        }
        for raw in [
            r#"{"extra":18446744073709551616}"#,
            r#"{"extra":-9223372036854775809}"#,
        ] {
            assert!(matches!(
                FastRoute::from_json(raw),
                Err(RouteDecodeError::NumberOutOfRange)
            ));
        }
        let route = FastRoute::from_json(r#"{"z":-0,"a":[-0,0.0]}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&route).unwrap(),
            r#"{"z":0,"a":[0,0.0]}"#
        );
    }

    #[test]
    fn route_objects_keep_source_member_order_including_nested_unknown_fields() {
        let raw = r#"{"z":{"z":1,"a":2},"status":null,"a":[{"z":3,"a":4}]}"#;
        let typed: FastRoute = serde_json::from_str(raw).unwrap();
        let serialised = serde_json::to_string(&typed).unwrap();
        if let Some(outputs) = crate::test_support::oracle("preflight_wire", &json!([{"raw":raw}]))
        {
            assert_eq!(json!(serialised), outputs[0]);
        }
        let task: DispatchTask = serde_json::from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&task).unwrap(), raw);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouteDecodeError {
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("duplicate route member: {0}")]
    DuplicateMember(String),
    #[error("route must be a JSON object")]
    NotObject,
    #[error("{0}")]
    InvalidField(String),
    #[error("{0}")]
    Json(#[source] serde_json::Error),
}
impl FastRoute {
    /// Raw input must enter here. Serde deserialisation of retained, already
    /// checked values is a separate record conversion, not a raw JSON decoder.
    pub fn from_json(raw: &str) -> Result<Self, RouteDecodeError> {
        let value =
            provenant_records::checked_json::from_slice(raw.as_bytes()).map_err(|e| match e {
                provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                    RouteDecodeError::NumberOutOfRange
                }
                provenant_records::checked_json::DecodeError::Json(e) => RouteDecodeError::Json(e),
            })?;
        if let Err(error) = serde_json::from_str::<UniqueMembers>(raw) {
            let message = error.to_string();
            let location = format!(" at line {} column {}", error.line(), error.column());
            let message = message.strip_suffix(&location).unwrap_or(&message);
            if let Some(key) = message
                .strip_prefix("duplicate route member: ")
                .and_then(|s| serde_json::from_str::<String>(s).ok())
            {
                return Err(RouteDecodeError::DuplicateMember(key));
            }
            return Err(RouteDecodeError::Json(error));
        }
        if !value.is_object() {
            return Err(RouteDecodeError::NotObject);
        }
        if value.get("status").and_then(serde_json::Value::as_str) == Some("ok") {
            for key in [
                "resolved_model",
                "model_family",
                "endpoint_provider",
                "identity_source",
            ] {
                if value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|s| provenant_core::py::python_strip(s).is_empty())
                {
                    return Err(RouteDecodeError::InvalidField(format!(
                        "successful route requires non-empty {key}"
                    )));
                }
            }
        }
        for key in [
            "status",
            "resolved_model",
            "model_family",
            "endpoint_provider",
            "identity_source",
            "requested_effort",
            "effort",
            "effort_source",
            "effort_capability_source",
            "effort_substitution",
            "substitution",
            "fallback_model",
            "catalog_model",
            "model_selection",
            "model_override_tier",
            "policy_override",
            "alias",
            "reason",
            "endpoint_profile",
            "endpoint_base_url",
            "endpoint_token_env",
            "endpoint_wire_api",
        ] {
            if let Some(member) = value.get(key).filter(|v| !v.is_null()) {
                if member.as_str().is_none_or(|s| s.contains('\0')) {
                    return Err(RouteDecodeError::InvalidField(format!(
                        "route field {key} must be a NUL-free string"
                    )));
                }
            }
        }
        // Normalise integer -0 while preserving member order and all other bytes.
        // Decoding the checked normalised value would sort objects, so the
        // order-preserving decoder below receives the original source text.
        let mut route: Self = serde_json::from_str(raw).map_err(RouteDecodeError::Json)?;
        normalise_negative_integer_zero(&mut route.0, &value);
        Ok(route)
    }
}
fn normalise_negative_integer_zero(
    object: &mut OrderedMap<OrderedValue>,
    checked: &serde_json::Value,
) {
    fn visit(value: &mut OrderedValue, checked: &serde_json::Value) {
        match value {
            OrderedValue::Number(number) if checked.as_i64() == Some(0) => {
                *number = serde_json::Number::from(0)
            }
            OrderedValue::Object(object) => normalise_negative_integer_zero(object, checked),
            OrderedValue::Array(array) => {
                if let Some(check) = checked.as_array() {
                    for (value, checked) in array.iter_mut().zip(check) {
                        visit(value, checked);
                    }
                }
            }
            _ => {}
        }
    }
    for (key, value) in object.entries_mut() {
        if let Some(checked) = checked.get(key) {
            visit(value, checked);
        }
    }
}
struct UniqueMembers;
impl<'de> Deserialize<'de> for UniqueMembers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueMembers;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with unique object members")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut keys = std::collections::BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        let encoded =
                            serde_json::to_string(&key).map_err(serde::de::Error::custom)?;
                        return Err(serde::de::Error::custom(format!(
                            "duplicate route member: {encoded}"
                        )));
                    }
                    map.next_value::<UniqueMembers>()?;
                }
                Ok(UniqueMembers)
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> Result<Self::Value, S::Error> {
                while seq.next_element::<UniqueMembers>()?.is_some() {}
                Ok(UniqueMembers)
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueMembers)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
