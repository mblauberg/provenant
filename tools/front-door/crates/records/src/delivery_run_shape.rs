//! Flat shape checks and pure predicates for delivery run receipts.
#![allow(dead_code)] // Inert until the workflow owner switches to Rust.

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq)]
pub enum ReceiptValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl<'de> Deserialize<'de> for ReceiptValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValueVisitor;
        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = ReceiptValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ReceiptValue::Null)
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(ReceiptValue::Bool(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(ReceiptValue::Number(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(ReceiptValue::Number(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(ReceiptValue::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(ReceiptValue::String(v.to_owned()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(ReceiptValue::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element()? {
                    values.push(value);
                }
                Ok(ReceiptValue::Array(values))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some((key, value)) = map.next_entry()? {
                    if let Some((_, previous)) = values.iter_mut().find(|(name, _)| name == &key) {
                        *previous = value;
                    } else {
                        values.push((key, value));
                    }
                }
                Ok(ReceiptValue::Object(values))
            }
        }
        deserializer.deserialize_any(ValueVisitor)
    }
}

impl Serialize for ReceiptValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(v) => serializer.serialize_bool(*v),
            Self::Number(v) => v.serialize(serializer),
            Self::String(v) => serializer.serialize_str(v),
            Self::Array(v) => v.serialize(serializer),
            Self::Object(v) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(v.len()))?;
                for (key, value) in v {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl ReceiptValue {
    pub fn from_json_str(input: &str) -> Result<Self, crate::checked_json::DecodeError> {
        let normalized = crate::checked_json::normalize(input)?;
        Ok(serde_json::from_str(&normalized)?)
    }

    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn contains_key(&self, key: &str) -> bool {
        matches!(self, Self::Object(fields) if fields.iter().any(|(name, _)| name == key))
    }
    fn as_str(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "NoneType",
            Self::Bool(_) => "bool",
            Self::Number(value) if value.is_i64() || value.is_u64() => "int",
            Self::Number(_) => "float",
            Self::String(_) => "str",
            Self::Array(_) => "list",
            Self::Object(_) => "dict",
        }
    }
}

#[derive(Clone, Copy)]
enum Allowed {
    Int,
    String,
    Object,
    Bool,
    List,
    NullableObject,
    NullableString,
}

const FIELDS: &[(&str, Allowed)] = &[
    ("schema_version", Allowed::Int),
    ("contract", Allowed::String),
    ("run_id", Allowed::String),
    ("fabric_relationships", Allowed::Object),
    ("profile", Allowed::String),
    ("risk_tier", Allowed::String),
    ("initial_risk_tier", Allowed::String),
    ("chair_family", Allowed::String),
    ("writer_model", Allowed::NullableString),
    ("risk_assessment", Allowed::Object),
    ("risk_override", Allowed::Object),
    ("high_stakes", Allowed::Bool),
    ("intent", Allowed::Object),
    ("authority", Allowed::Object),
    ("artifacts", Allowed::List),
    ("design", Allowed::Object),
    ("evidence", Allowed::List),
    ("measures", Allowed::Object),
    ("assurance", Allowed::Object),
    ("reviews", Allowed::List),
    ("security", Allowed::Object),
    ("high_stakes_controls", Allowed::NullableObject),
    ("human_gates", Allowed::Object),
    ("observation", Allowed::Object),
    ("incident", Allowed::NullableObject),
    ("retrospective", Allowed::NullableObject),
    ("repair_cycles", Allowed::Int),
    ("escaped_defect", Allowed::Bool),
    ("human_corrections", Allowed::List),
    ("checkpoint", Allowed::Object),
    ("degradation", Allowed::NullableObject),
    ("project_policy", Allowed::Object),
    ("software_delivery", Allowed::Object),
    ("delivery_acceptance", Allowed::Object),
];
const OPTIONAL: &[&str] = &[
    "fabric_relationships",
    "project_policy",
    "software_delivery",
    "delivery_acceptance",
    "writer_model",
];

impl Allowed {
    fn name(self) -> &'static str {
        match self {
            Self::Int => "an integer",
            Self::String => "a string",
            Self::Object => "an object",
            Self::Bool => "a boolean",
            Self::List => "a list",
            Self::NullableObject => "an object or null",
            Self::NullableString => "a string or null",
        }
    }
    fn accepts(self, value: &ReceiptValue) -> bool {
        match (self, value) {
            (Self::Int, ReceiptValue::Number(n)) => n.is_i64() || n.is_u64(),
            (Self::String, ReceiptValue::String(_))
            | (Self::NullableString, ReceiptValue::String(_)) => true,
            (Self::Object, ReceiptValue::Object(_))
            | (Self::NullableObject, ReceiptValue::Object(_)) => true,
            (Self::Bool, ReceiptValue::Bool(_)) => true,
            (Self::List, ReceiptValue::Array(_)) => true,
            (Self::NullableObject | Self::NullableString, ReceiptValue::Null) => true,
            _ => false,
        }
    }
}

fn python_one(value: &ReceiptValue) -> bool {
    match value {
        ReceiptValue::Bool(v) => *v,
        ReceiptValue::Number(n) if n.is_i64() => n.as_i64() == Some(1),
        ReceiptValue::Number(n) if n.is_u64() => n.as_u64() == Some(1),
        ReceiptValue::Number(n) => n.as_f64() == Some(1.0),
        _ => false,
    }
}

pub fn shape_errors(run: &ReceiptValue) -> Vec<String> {
    let ReceiptValue::Object(fields) = run else {
        return vec!["RUN root must be an object".into()];
    };
    let mut errors = Vec::new();
    if run.get("contract").and_then(ReceiptValue::as_str) != Some("delivery-run")
        || !run.get("schema_version").is_some_and(python_one)
    {
        errors.push("delivery receipt must use contract delivery-run schema_version 1".into());
    }
    for (field, allowed) in FIELDS {
        let Some(value) = run.get(field) else {
            if !OPTIONAL.contains(field) {
                errors.push(format!("{field} is required"));
            }
            continue;
        };
        if !allowed.accepts(value) {
            errors.push(format!(
                "{field} must be {}, got {}",
                allowed.name(),
                value.type_name()
            ));
        }
    }
    let known: std::collections::BTreeSet<_> = FIELDS.iter().map(|(key, _)| *key).collect();
    let mut unknown: Vec<_> = fields
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| !known.contains(key))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        errors.push(format!(
            "RUN.json declares unknown fields: {}",
            unknown.join(", ")
        ));
    }
    errors
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ShapeError(pub String);

pub fn check_shape(run: &ReceiptValue) -> Result<&ReceiptValue, ShapeError> {
    if let Some(error) = shape_errors(run).into_iter().next() {
        Err(ShapeError(error))
    } else {
        Ok(run)
    }
}

// port: belongs in provenant-workflow
pub fn gate_approved(run: &ReceiptValue, path: &[&str]) -> bool {
    let mut value = run;
    for key in path {
        let Some(next) = value.get(key) else {
            return false;
        };
        value = next;
    }
    matches!(value, ReceiptValue::Object(_))
        && value.get("status").and_then(ReceiptValue::as_str) == Some("approved")
}
// port: belongs in provenant-workflow
pub fn release_approved(run: &ReceiptValue) -> bool {
    gate_approved(run, &["human_gates", "release"])
}
// port: belongs in provenant-workflow
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GateError {
    #[error("unhashable type: '{0}'")]
    UnhashableStatus(&'static str),
}

pub fn run_closed(run: &ReceiptValue) -> Result<bool, GateError> {
    let status = run.get("observation").and_then(|v| v.get("status"));
    match status {
        Some(ReceiptValue::Array(_)) => return Err(GateError::UnhashableStatus("list")),
        Some(ReceiptValue::Object(_)) => return Err(GateError::UnhashableStatus("dict")),
        _ => {}
    }
    let settled = status
        .and_then(ReceiptValue::as_str)
        .is_some_and(|s| s == "pass" || s == "not_applicable");
    Ok(release_approved(run) && settled)
}

pub fn intent_approved(run: &ReceiptValue) -> bool {
    gate_approved(run, &["intent", "approval"])
}
pub fn acceptance_approved(run: &ReceiptValue) -> bool {
    gate_approved(run, &["human_gates", "acceptance"]) || run.contains_key("delivery_acceptance")
}

const TIMESTAMP_FIELDS: &[&str] = &["recorded_at", "finished_at", "observed_at", "at"];
fn collect_timestamps(value: &ReceiptValue, found: &mut Vec<String>) {
    match value {
        ReceiptValue::Object(fields) => {
            for (key, child) in fields {
                if TIMESTAMP_FIELDS.contains(&key.as_str()) {
                    if let ReceiptValue::String(timestamp) = child {
                        if !timestamp.is_empty() {
                            found.push(timestamp.clone());
                        }
                    } else {
                        collect_timestamps(child, found);
                    }
                } else {
                    collect_timestamps(child, found);
                }
            }
        }
        ReceiptValue::Array(values) => {
            for child in values {
                collect_timestamps(child, found);
            }
        }
        _ => {}
    }
}

pub fn recorded_timestamps(run: &ReceiptValue) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(value) = run.get("evidence") {
        collect_timestamps(value, &mut found);
    }
    if let Some(value) = run.get("human_corrections") {
        collect_timestamps(value, &mut found);
    }
    if let Some(observation) = run.get("observation") {
        for field in ["started_at", "ended_at"] {
            if let Some(ReceiptValue::String(value)) = observation.get(field) {
                if !value.is_empty() {
                    found.push(value.clone());
                }
            }
        }
    }
    found
}
