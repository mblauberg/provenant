//! Contracts for the dispatch cycle. Policy and effects remain with its owner.
//! Task/route objects are intentionally open: source validation is dynamic and
//! successful route records retain unknown members.
pub use crate::pending::{OrderedMap, OrderedValue};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct DispatchTask(pub OrderedMap<OrderedValue>);

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(transparent)]
pub struct FastRoute(OrderedMap<OrderedValue>);

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
                Ok(route) => json!({"wire":route.python_wire().unwrap()}),
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
        assert_eq!(route.python_wire().unwrap(), r#"{"z":0,"a":[0,0.0]}"#);
    }

    #[test]
    fn route_objects_keep_source_member_order_including_nested_unknown_fields() {
        let raw = r#"{"z":{"z":1,"a":2},"status":null,"a":[{"z":3,"a":4}]}"#;
        let typed: FastRoute = serde_json::from_str(raw).unwrap();
        let serialised = typed.python_wire().unwrap();
        if let Some(outputs) = crate::test_support::oracle("preflight_wire", &json!([{"raw":raw}]))
        {
            assert_eq!(json!(serialised), outputs[0]);
        }
        let task: DispatchTask = serde_json::from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&task).unwrap(), raw);
    }

    #[test]
    fn direct_route_deserialisation_validates_and_preserves_ordered_extensions() {
        for raw in [
            r#"{"status":"ok"}"#,
            r#"{"x":1,"x":2}"#,
            r#"{"future":{"nested":{"x":1,"x":2}}}"#,
        ] {
            assert!(
                serde_json::from_str::<FastRoute>(raw).is_err(),
                "accepted {raw}"
            );
        }

        let raw = r#"{"status":"ok","resolved_model":"m","model_family":"family","endpoint_provider":"provider","identity_source":"fixture","extension":{"z":[{"third":3,"first":1}],"a":true}}"#;
        let route: FastRoute = serde_json::from_str(raw).unwrap();
        assert_eq!(route.python_wire().unwrap(), raw);
        let keys: Vec<_> = route.entries().map(|(key, _)| key).collect();
        assert_eq!(
            keys,
            [
                "status",
                "resolved_model",
                "model_family",
                "endpoint_provider",
                "identity_source",
                "extension"
            ]
        );
    }

    #[test]
    fn fast_route_wire_matches_python_order_and_dynamic_json_values() {
        let raws = [
            r#"{"status":"ok","resolved_model":"m","model_family":"family","endpoint_provider":"provider","identity_source":"fixture","future":{"z":[null,true,2,2.5],"a":"雪\n"}}"#,
            r#"{"z":{"nested":{"third":3,"first":1}},"status":null,"effort":null,"a":[{"z":3,"a":4}]}"#,
            r#"{"status":"ok","resolved_model":"m","model_family":"family","endpoint_provider":"provider","identity_source":"fixture","z":-0,"a":[-0,0.0]}"#,
        ];
        let cases = json!(raws.map(|raw| json!({"raw":raw})));
        let Some(expected) =
            crate::test_support::oracle_script("oracle_interfaces.py", "preflight_wire", &cases)
        else {
            return;
        };
        for (raw, expected) in raws.iter().zip(expected.as_array().unwrap()) {
            let route = FastRoute::from_json(raw).unwrap();
            let actual = route.python_wire().unwrap();
            assert_eq!(actual, expected["wire"]);
        }
    }

    #[test]
    fn fast_route_refusal_edges_match_python_text() {
        let raws = [
            r#"{"x":1,"x":2}"#,
            r#"{"status":true}"#,
            r#"{"status":"ok"}"#,
            r#"{"status":"ok","resolved_model":"m","model_family":"family","endpoint_provider":"provider","identity_source":"fixture","effort":42}"#,
        ];
        let cases = json!(raws.map(|raw| json!({"raw":raw})));
        let Some(expected) =
            crate::test_support::oracle_script("oracle_interfaces.py", "preflight_decode", &cases)
        else {
            return;
        };
        for (raw, expected) in raws.iter().zip(expected.as_array().unwrap()) {
            let actual = match FastRoute::from_json(raw) {
                Ok(route) => json!({"wire":route.python_wire().unwrap()}),
                Err(error) => json!({"error":error.to_string()}),
            };
            assert_eq!(actual, *expected);
        }
    }

    #[test]
    fn python_accepted_non_finite_and_surrogate_route_edges_remain_partial() {
        let raws = [r#"{"future":NaN}"#, r#"{"future":"\ud800"}"#];
        let cases = json!(raws.map(|raw| json!({"raw":raw})));
        let Some(expected) =
            crate::test_support::oracle_script("oracle_interfaces.py", "preflight_wire", &cases)
        else {
            return;
        };
        assert_eq!(expected[0]["wire"], "{\"future\":NaN}");
        assert!(expected[1].get("surrogatepass_hex").is_some());
        for raw in raws {
            assert!(FastRoute::from_json(raw).is_err());
        }
    }

    #[test]
    fn fast_route_python_wire_matches_source_float_notation() {
        let raw = r#"{"status":"ok","resolved_model":"m","model_family":"family","endpoint_provider":"provider","identity_source":"fixture","numbers":{"small":1e-7,"large":1e20,"negative_zero":-0.0}}"#;
        let cases = json!([{"raw":raw}]);
        let Some(expected) =
            crate::test_support::oracle_script("oracle_interfaces.py", "preflight_wire", &cases)
        else {
            return;
        };
        let route = FastRoute::from_json(raw).unwrap();
        let wire = route.python_wire().unwrap();
        assert_eq!(wire, expected[0]["wire"]);
        assert!(wire.contains(r#""small":1e-07"#));
        assert!(wire.contains(r#""large":1e+20"#));
        assert!(wire.contains(r#""negative_zero":-0.0"#));
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
    /// Read route members without exposing a way to bypass route validation.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &OrderedValue)> {
        self.0.entries()
    }

    /// Render the compact JSON wire form used by Python's route interface.
    pub fn python_wire(&self) -> Result<String, serde_json::Error> {
        crate::pending::python_wire(self)
    }

    /// Use this for raw planner text so integer bounds become typed errors.
    /// Serde also validates route shape, fields, and duplicate members, but
    /// leaves numeric overflow in serde's error vocabulary.
    pub fn from_json(raw: &str) -> Result<Self, RouteDecodeError> {
        let value =
            provenant_records::checked_json::from_slice(raw.as_bytes()).map_err(|e| match e {
                provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                    RouteDecodeError::NumberOutOfRange
                }
                provenant_records::checked_json::DecodeError::Json(e) => RouteDecodeError::Json(e),
            })?;
        let mut route: Self = serde_json::from_str(raw).map_err(map_route_decode_error)?;
        // Normalise integer -0 while preserving member order and all other bytes.
        // The checked value is used only to distinguish integer -0 from float -0.0.
        normalise_negative_integer_zero(&mut route.0, &value);
        Ok(route)
    }
}

impl<'de> Deserialize<'de> for FastRoute {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = UniqueMembers::deserialize(deserializer)?.0;
        validate_route(&value).map_err(serde::de::Error::custom)?;
        match value {
            OrderedValue::Object(object) => Ok(Self(object)),
            _ => Err(serde::de::Error::custom(RouteDecodeError::NotObject)),
        }
    }
}

fn validate_route(value: &OrderedValue) -> Result<(), RouteDecodeError> {
    let OrderedValue::Object(route) = value else {
        return Err(RouteDecodeError::NotObject);
    };
    if matches!(route.get("status"), Some(OrderedValue::String(status)) if status == "ok") {
        for key in [
            "resolved_model",
            "model_family",
            "endpoint_provider",
            "identity_source",
        ] {
            if route
                .get(key)
                .and_then(|value| match value {
                    OrderedValue::String(value) => Some(value.as_str()),
                    _ => None,
                })
                .is_none_or(|value| provenant_core::py::python_strip(value).is_empty())
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
        if let Some(member) = route
            .get(key)
            .filter(|value| !matches!(value, OrderedValue::Null))
        {
            if !matches!(member, OrderedValue::String(value) if !value.contains('\0')) {
                return Err(RouteDecodeError::InvalidField(format!(
                    "route field {key} must be a NUL-free string"
                )));
            }
        }
    }
    Ok(())
}

fn map_route_decode_error(error: serde_json::Error) -> RouteDecodeError {
    let message = error.to_string();
    let location = format!(" at line {} column {}", error.line(), error.column());
    let message = message.strip_suffix(&location).unwrap_or(&message);
    if let Some(member) = message.strip_prefix("duplicate route member: ") {
        RouteDecodeError::DuplicateMember(member.to_string())
    } else if message == "route must be a JSON object" {
        RouteDecodeError::NotObject
    } else if message.starts_with("successful route requires non-empty ")
        || message.starts_with("route field ")
    {
        RouteDecodeError::InvalidField(message.to_string())
    } else {
        RouteDecodeError::Json(error)
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
struct UniqueMembers(OrderedValue);
impl<'de> Deserialize<'de> for UniqueMembers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueMembers;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with unique object members and supported JSON values")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut keys = std::collections::BTreeSet::new();
                let mut object = OrderedMap::default();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate route member: {key}"
                        )));
                    }
                    let value = map.next_value::<UniqueMembers>()?.0;
                    object.insert(key, value);
                }
                Ok(UniqueMembers(OrderedValue::Object(object)))
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> Result<Self::Value, S::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<UniqueMembers>()? {
                    values.push(value.0);
                }
                Ok(UniqueMembers(OrderedValue::Array(values)))
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueMembers(OrderedValue::Bool(value)))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueMembers(OrderedValue::Number(value.into())))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueMembers(OrderedValue::Number(value.into())))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueMembers(OrderedValue::Number(number)))
                    .ok_or_else(|| serde::de::Error::custom("non-finite route number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueMembers(OrderedValue::String(value.to_string())))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueMembers(OrderedValue::String(value)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueMembers(OrderedValue::Null))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
