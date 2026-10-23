//! Exit-cycle contracts. This module grants no process or filesystem authority.
use crate::pending::{OrderedMap, OrderedValue as Value};
use serde::{Deserialize, Serialize};

/// A source member can be absent or present with null. Option alone loses this
/// distinction when a record is decoded and retained again.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Member<T> {
    #[default]
    Absent,
    Present(T),
}
impl<T> Member<T> {
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}
impl<T: Serialize> Serialize for Member<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Present(value) => value.serialize(serializer),
            Self::Absent => serializer.serialize_unit(),
        }
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Member<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProcessInteger {
    Signed(i64),
    Unsigned(u64),
}

/// Rows emitted by the supervisor, distinct from legacy receipt inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DescendantRow {
    pub pid: ProcessInteger,
    pub start_time: String,
    pub command: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisorRows {
    pub reaped: Vec<DescendantRow>,
    pub spared: Vec<DescendantRow>,
}

pub type ExitRecord = crate::pending::OrderedRecord<ExitRecordFields>;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExitRecordFields {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub pid: Member<Option<ProcessInteger>>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub pgid: Member<Option<ProcessInteger>>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub exit: Member<Option<ProcessInteger>>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub signal: Member<Option<ProcessInteger>>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub forced: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub terminal_event_seen: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub reaped: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub spared: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub capture: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub census_warnings: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub io_errors: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub started_at: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub ended_at: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub last_progress_at: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub observer_lost: Member<bool>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub observer_dropped: Member<u64>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub reaped_truncated: Member<u64>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub spared_truncated: Member<u64>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub census_warnings_truncated: Member<u64>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub io_errors_truncated: Member<u64>,
    #[serde(flatten)]
    pub extra: crate::pending::OrderedMap<crate::pending::OrderedValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptFields {
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub pid: Option<ProcessInteger>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub pgid: Option<ProcessInteger>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub exit: Option<ProcessInteger>,
    pub forced: Value,
    pub terminal_event_seen: bool,
    pub reaped: Value,
    pub spared: Value,
    pub capture: Value,
    pub warnings: Vec<String>,
    pub started_at: Value,
    pub ended_at: Value,
    pub last_progress_at: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ExitError {
    #[error("unsupported lane exit schema")]
    Schema,
    #[error("silence cannot force a provider stop")]
    Silence,
    #[error("lane exit value has no length or is not iterable")]
    InvalidCollection,
    #[error("signal negation is outside the integer range")]
    NumberOutOfRange,
}

impl ExitRecord {
    pub fn from_json(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        Self::decode(raw)
    }

    pub fn receipt_fields(&self) -> Result<ReceiptFields, ExitError> {
        fn python_truth(value: &Value) -> bool {
            value.truthy()
        }
        fn value(member: &Member<Value>) -> Value {
            match member {
                Member::Absent => Value::Null,
                Member::Present(v) => v.clone(),
            }
        }
        fn process(member: &Member<Option<ProcessInteger>>) -> Option<ProcessInteger> {
            match member {
                Member::Absent => None,
                Member::Present(v) => v.clone(),
            }
        }
        fn iterable(member: &Member<Value>) -> Result<Vec<Value>, ExitError> {
            match member {
                Member::Absent => Ok(vec![]),
                Member::Present(Value::Array(v)) => Ok(v.clone()),
                Member::Present(Value::String(v)) => {
                    Ok(v.chars().map(|c| Value::String(c.to_string())).collect())
                }
                Member::Present(Value::Object(v)) => Ok(v
                    .entries()
                    .map(|(k, _)| Value::String(k.to_owned()))
                    .collect()),
                _ => Err(ExitError::InvalidCollection),
            }
        }
        if self.schema != "provenant.lane.exit.v1" {
            return Err(ExitError::Schema);
        }
        let forced = value(&self.forced);
        if forced.as_str() == Some("stalled") {
            return Err(ExitError::Silence);
        }
        let exit = match process(&self.signal) {
            Some(ProcessInteger::Signed(i64::MIN)) => Some(ProcessInteger::Unsigned(1 << 63)),
            Some(ProcessInteger::Signed(n)) if n != 0 => Some(ProcessInteger::Signed(-n)),
            Some(ProcessInteger::Unsigned(n)) if n != 0 => Some(ProcessInteger::Signed(
                i64::try_from(-(i128::from(n))).map_err(|_| ExitError::NumberOutOfRange)?,
            )),
            _ => process(&self.exit),
        };
        let spared = value(&self.spared);
        let spared = if !python_truth(&spared) {
            Value::Number(0.into())
        } else {
            match spared {
                Value::Bool(_) => spared,
                Value::Number(ref n) if !n.is_f64() => spared,
                Value::Array(ref v) => Value::Number((v.len() as u64).into()),
                Value::Object(ref v) => Value::Number((v.entries().count() as u64).into()),
                Value::String(ref v) => Value::Number((v.chars().count() as u64).into()),
                _ => return Err(ExitError::InvalidCollection),
            }
        };
        let mut warnings = iterable(&self.census_warnings)?;
        warnings.extend(iterable(&self.io_errors)?);
        let reaped = value(&self.reaped);
        let capture = value(&self.capture);
        Ok(ReceiptFields {
            pid: process(&self.pid),
            pgid: process(&self.pgid),
            exit,
            forced: match forced.as_str() {
                Some("wall_timeout") => Value::String("timed_out".into()),
                Some("supervisor_signal") => Value::String("interrupted".into()),
                _ => forced,
            },
            terminal_event_seen: python_truth(&value(&self.terminal_event_seen)),
            reaped: if python_truth(&reaped) {
                reaped
            } else {
                Value::Array(vec![])
            },
            spared,
            capture: if python_truth(&capture) {
                capture
            } else {
                Value::Object(OrderedMap::default())
            },
            warnings: warnings
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.python_json())
                })
                .collect(),
            started_at: value(&self.started_at),
            ended_at: value(&self.ended_at),
            last_progress_at: value(&self.last_progress_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn raw_exit_inputs_translate_like_python_including_legacy_values() {
        let cases = json!([
            {"schema":"provenant.lane.exit.v1","spared":true},
            {"schema":"provenant.lane.exit.v1","spared":-3,"reaped":"abc","census_warnings":"ab"},
            {"schema":"provenant.lane.exit.v1","spared":"é🙂","signal":15,"exit":0,"forced":"wall_timeout","io_errors":[{"a":1}]},
            {"schema":"provenant.lane.exit.v1","reaped":[{"pid":124,"start_time":"123","command":"child"}],"spared":[],"observer_lost":false,"observer_dropped":2,"reaped_truncated":1},
            {"schema":"provenant.lane.exit.v1"}
        ]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|raw| {
                let typed =
                    ExitRecord::from_json(serde_json::to_string(raw).unwrap().as_bytes()).unwrap();
                serde_json::to_string(&typed.receipt_fields().unwrap()).unwrap()
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("lane_exit_contract", &cases) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn exit_record_keeps_nested_extension_order() {
        let raw = r#"{"schema":"provenant.lane.exit.v1","z":{"z":1,"a":2},"a":0}"#;
        let record = ExitRecord::from_json(raw.as_bytes()).unwrap();
        assert_eq!(serde_json::to_string(&record).unwrap(), raw);
    }
    #[test]
    fn raw_exit_rejections_and_integer_divergence_are_source_grounded() {
        let cases = json!([
            {"raw":r#"{"schema":"provenant.lane.exit.v1","pid":true}"#},
            {"raw":r#"{"schema":"provenant.lane.exit.v1","pid":1.0}"#}
        ]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| {
                assert!(ExitRecord::from_json(case["raw"].as_str().unwrap().as_bytes()).is_err());
                json!({"error":"lane exit process fields must be integers"})
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("lane_exit_raw", &cases) {
            assert_eq!(json!(actual), expected);
        }
        let raw = r#"{"schema":"provenant.lane.exit.v1","pid":1180591620717411303424}"#;
        assert!(matches!(
            ExitRecord::from_json(raw.as_bytes()),
            Err(provenant_records::checked_json::DecodeError::NumberOutOfRange(_))
        ));
        if let Some(expected) = crate::test_support::oracle("lane_exit_raw", &json!([{"raw":raw}]))
        {
            assert!(expected[0]["wire"]
                .as_str()
                .unwrap()
                .contains("1180591620717411303424"));
        }
    }
    #[test]
    fn receipt_translation_preserves_raw_collections_and_full_signal_range() {
        let cases = json!([
            {"raw":r#"{"schema":"provenant.lane.exit.v1","census_warnings":{"z":0,"a":0},"io_errors":[{"z":1,"a":2}]}"#},
            {"raw":r#"{"schema":"provenant.lane.exit.v1","reaped":[{"pid":124,"start_time":"123","command":"child"}],"capture":{"z":1,"a":2}}"#},
            {"raw":r#"{"schema":"provenant.lane.exit.v1","signal":-9223372036854775808}"#},
            {"raw":r#"{"schema":"provenant.lane.exit.v1","pid":-0,"capture":{"z":-0,"a":0.0}}"#}
        ]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| {
                let record =
                    ExitRecord::from_json(case["raw"].as_str().unwrap().as_bytes()).unwrap();
                json!({"wire":serde_json::to_string(&record.receipt_fields().unwrap()).unwrap()})
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("lane_exit_raw", &cases) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn exit_process_fields_refuse_bool_and_float() {
        for raw in [
            r#"{"schema":"provenant.lane.exit.v1","pid":true}"#,
            r#"{"schema":"provenant.lane.exit.v1","pid":1.0}"#,
        ] {
            assert!(ExitRecord::from_json(raw.as_bytes()).is_err());
        }
    }
}
