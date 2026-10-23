//! Exit-cycle contracts. This module grants no process or filesystem authority.
use crate::pending::{OrderedMap, OrderedValue as Value, PythonDictionary};
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
    #[error("lane exit process fields must be integers")]
    ProcessFields,
    #[error("unhashable type: '{0}'")]
    UnhashableForced(&'static str),
}

impl ExitRecord {
    pub fn from_json(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        Self::decode(raw)
    }
}

impl ExitRecordFields {
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
        // Python constructs the warnings iterable before looking up the forced
        // key. A list/dict forced key raises TypeError even though the fallback
        // argument to dict.get contains the same value.
        let mut warnings = iterable(&self.census_warnings)?;
        warnings.extend(iterable(&self.io_errors)?);
        match &forced {
            Value::Array(_) => return Err(ExitError::UnhashableForced("list")),
            Value::Object(_) => return Err(ExitError::UnhashableForced("dict")),
            _ => {}
        }
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

/// Receipt translation accepts an open source record, including unrelated
/// supervisor extensions. Validate only the members the Python function reads.
pub fn receipt_from_object(record: &OrderedMap<Value>) -> Result<ReceiptFields, ExitError> {
    if record.get("schema").and_then(Value::as_str) != Some("provenant.lane.exit.v1") {
        return Err(ExitError::Schema);
    }
    if record.get("forced").and_then(Value::as_str) == Some("stalled") {
        return Err(ExitError::Silence);
    }
    let process = |name: &str| -> Result<Member<Option<ProcessInteger>>, ExitError> {
        match record.get(name) {
            None => Ok(Member::Absent),
            Some(Value::Null) => Ok(Member::Present(None)),
            Some(Value::Number(n)) if !n.is_f64() => {
                if let Some(n) = n.as_i64() {
                    Ok(Member::Present(Some(ProcessInteger::Signed(n))))
                } else if let Some(n) = n.as_u64() {
                    Ok(Member::Present(Some(ProcessInteger::Unsigned(n))))
                } else {
                    Err(ExitError::NumberOutOfRange)
                }
            }
            _ => Err(ExitError::ProcessFields),
        }
    };
    let member = |key: &str| {
        record
            .get(key)
            .cloned()
            .map_or(Member::Absent, Member::Present)
    };
    let fields = ExitRecordFields {
        schema: "provenant.lane.exit.v1".into(),
        pid: process("pid")?,
        pgid: process("pgid")?,
        exit: process("exit")?,
        signal: process("signal")?,
        forced: member("forced"),
        terminal_event_seen: member("terminal_event_seen"),
        reaped: member("reaped"),
        spared: member("spared"),
        capture: member("capture"),
        census_warnings: member("census_warnings"),
        io_errors: member("io_errors"),
        started_at: member("started_at"),
        ended_at: member("ended_at"),
        last_progress_at: member("last_progress_at"),
        observer_lost: Member::Absent,
        observer_dropped: Member::Absent,
        reaped_truncated: Member::Absent,
        spared_truncated: Member::Absent,
        census_warnings_truncated: Member::Absent,
        io_errors_truncated: Member::Absent,
        extra: OrderedMap::default(),
    };
    // Receipt translation needs no retained-record template.
    fields.receipt_fields()
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

/// The retained observer restoration boundary is more permissive than the
/// observer's emitted summary: Python assigns these values without validating
/// them. Keep them ordered and dynamic until the provider adapter consumes them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RestoredObserver {
    #[serde(flatten)]
    pub semantic: OrderedMap<Value>,
    pub terminal_event_seen: bool,
    pub live_failure: Value,
    pub semantic_failures: Value,
    pub retry_failure: Value,
    pub terminal_text: Value,
    pub text_chunks: Vec<String>,
    pub text_truncated: Value,
    pub model_changes: Value,
    pub last_model: Value,
    pub context: PythonDictionary,
    pub models: PythonDictionary,
    #[serde(skip)]
    pub text_size: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum RestorationError {
    #[error("observer text chunk has no encode attribute")]
    TextEncodeAttribute,
}

/// Return None when the source discards a partially restored summary and
/// replays captures. Uncaught source AttributeError stays an explicit error.
pub fn restore_observer_summary(
    summary: &OrderedMap<Value>,
) -> Result<Option<RestoredObserver>, RestorationError> {
    // Required reads may fail after earlier fields were assigned. Build a fresh
    // snapshot and publish it only after every restoration operation succeeds.
    const REQUIRED: &[&str] = &[
        "terminal_event_seen",
        "live_failure",
        "semantic_failures",
        "retry_failure",
        "terminal_text",
        "text_truncated",
        "model_changes",
        "last_model",
        "text_chunks",
    ];
    if REQUIRED.iter().any(|key| summary.get(key).is_none()) {
        return Ok(None);
    }
    let get = |name: &str| summary.get(name).cloned().unwrap_or(Value::Null);
    let mut restored = RestoredObserver::empty(Value::Null);
    for key in ["session_id", "observed_model", "reset_at", "retry_after"] {
        if let Some(value) = summary.get(key).filter(|v| !matches!(v, Value::Null)) {
            restored.semantic.insert(key.into(), value.clone());
        }
    }
    restored.terminal_event_seen = get("terminal_event_seen").truthy();
    restored.live_failure = get("live_failure");
    restored.semantic_failures = get("semantic_failures");
    restored.retry_failure = get("retry_failure");
    restored.terminal_text = get("terminal_text");
    restored.text_truncated = get("text_truncated");
    restored.model_changes = get("model_changes");
    restored.last_model = get("last_model");
    let chunks = match get("text_chunks") {
        Value::Array(values) => values,
        Value::String(text) => text.chars().map(|c| Value::String(c.to_string())).collect(),
        Value::Object(values) => values
            .entries()
            .map(|(key, _)| Value::String(key.into()))
            .collect(),
        _ => return Ok(None), // TypeError is caught by settle_observer.
    };
    for chunk in chunks {
        let Value::String(text) = chunk else {
            return Err(RestorationError::TextEncodeAttribute);
        };
        restored.text_size += text.len();
        restored.text_chunks.push(text);
    }
    if !update_dictionary(&mut restored.context, &get("context"))? {
        return Ok(None);
    }
    if let Some(models) = summary.get("models") {
        if !update_dictionary(&mut restored.models, models)? {
            return Ok(None);
        }
    }
    Ok(Some(restored))
}

fn update_dictionary(into: &mut PythonDictionary, value: &Value) -> Result<bool, RestorationError> {
    fn iterable(value: &Value) -> Option<Vec<Value>> {
        match value {
            Value::Array(values) => Some(values.clone()),
            Value::String(text) => {
                Some(text.chars().map(|c| Value::String(c.to_string())).collect())
            }
            Value::Object(values) => Some(
                values
                    .entries()
                    .map(|(key, _)| Value::String(key.into()))
                    .collect(),
            ),
            _ => None,
        }
    }
    if let Value::Object(values) = value {
        for (key, value) in values.entries() {
            into.insert(Value::String(key.into()), value.clone());
        }
        return Ok(true);
    }
    let Some(pairs) = iterable(value) else {
        return Ok(false);
    };
    for pair in pairs {
        let Some(pair) = iterable(&pair) else {
            return Ok(false);
        };
        let [key, value] = pair.as_slice() else {
            return Ok(false);
        };
        if matches!(key, Value::Array(_) | Value::Object(_)) {
            return Ok(false); // Unhashable key TypeError is caught.
        }
        into.insert(key.clone(), value.clone());
    }
    Ok(true)
}

impl RestoredObserver {
    pub fn empty(model: Value) -> Self {
        fn fields(names: &[&str]) -> PythonDictionary {
            let mut result = PythonDictionary::default();
            for name in names {
                result.insert(Value::String((*name).into()), Value::Null);
            }
            result
        }
        let mut models = fields(&["init", "answered", "final", "usage"]);
        models.insert(Value::String("answered".into()), Value::Array(vec![]));
        models.insert(Value::String("usage".into()), Value::Array(vec![]));
        Self {
            semantic: OrderedMap::default(),
            terminal_event_seen: false,
            live_failure: Value::Null,
            semantic_failures: Value::Object(OrderedMap::default()),
            retry_failure: Value::Null,
            terminal_text: Value::Null,
            text_chunks: vec![],
            text_truncated: Value::Bool(false),
            model_changes: Value::Array(vec![]),
            last_model: model,
            context: fields(&[
                "context_tokens",
                "input_tokens",
                "output_tokens",
                "cached_input_tokens",
                "cache_write_tokens",
                "reasoning_tokens",
                "context_window_tokens",
                "context_percent",
                "source",
            ]),
            models,
            text_size: 0,
        }
    }
}

#[cfg(test)]
mod restoration_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restored_summary_preserves_dynamic_values_and_discards_partial_state() {
        let base = json!({"session_id":"retained", "terminal_event_seen":true,
            "live_failure":null, "semantic_failures":{}, "retry_failure":null,
            "terminal_text":"DONE", "text_chunks":["é🙂"], "text_truncated":false,
            "model_changes":[], "last_model":"fixture", "context":{"input_tokens":4}});
        let mut cases = vec![base.clone()];
        let mut scalar_keys = base.clone();
        scalar_keys["context"] = json!([
            [1, "integer"],
            [true, "bool"],
            [1.0, "float"],
            ["1", "string"],
            [null, "null"],
            [false, "false"],
            [0, "zero"],
            [1e-7, "fraction"]
        ]);
        scalar_keys["models"] = json!([[null, 0], ["null", 1]]);
        cases.push(scalar_keys);
        let mut unhashable_key = base.clone();
        unhashable_key["context"] = json!([[[], 1]]);
        cases.push(unhashable_key);
        let mut changed = base.clone();
        changed["context"] = json!([["z", 1], ["context_tokens", 4]]);
        changed["models"] = json!({"z":null,"init":"retained"});
        changed["text_chunks"] = json!({"z":1,"a":2});
        changed["text_truncated"] = json!([false]);
        changed["terminal_event_seen"] = json!("yes");
        cases.push(changed);
        for key in ["terminal_text", "context", "text_chunks", "last_model"] {
            let mut incomplete = base.clone();
            incomplete.as_object_mut().unwrap().remove(key);
            cases.push(incomplete);
        }
        let mut invalid = base.clone();
        invalid["context"] = serde_json::Value::Null;
        cases.push(invalid);
        let actual: Vec<_> = cases
            .iter()
            .map(|case| {
                let summary = serde_json::from_value(case.clone()).unwrap();
                let restored = restore_observer_summary(&summary)
                    .unwrap()
                    .unwrap_or_else(|| RestoredObserver::empty(Value::String("fixture".into())));
                json!({"wire":restored.python_wire(),"text_size":restored.text_size})
            })
            .collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_lane.py", "restore", &json!(cases))
        {
            assert_eq!(json!(actual), expected);
        }
    }
}

/// Retain the source capture's head/tail without treating a write failure as a
/// lifecycle event. The confinement owner supplies the bound regular-file IO.
pub fn retained_capture(previous: &[u8], data: &[u8], limit: std::num::NonZeroUsize) -> Vec<u8> {
    let limit = limit.get();
    let previous = &previous[..previous.len().min(limit)];
    if previous.len().saturating_add(data.len()) <= limit {
        return previous.iter().chain(data).copied().collect();
    }
    let head = limit / 2;
    let tail = limit - head;
    let mut result: Vec<_> = previous.iter().chain(data).take(head).copied().collect();
    if data.len() >= tail {
        result.extend_from_slice(&data[data.len() - tail..]);
    } else {
        let from_previous = tail - data.len();
        result.extend_from_slice(&previous[previous.len() - from_previous..]);
        result.extend_from_slice(data);
    }
    result
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    #[test]
    fn retained_capture_keeps_head_and_diagnostic_tail_for_odd_and_even_limits() {
        for limit in 1..=9 {
            let bound = std::num::NonZeroUsize::new(limit).unwrap();
            for old in [b"".as_slice(), b"abcdefghi", b"abcdefghiJKL"] {
                for data in [b"".as_slice(), b"!", b"0123456789"] {
                    let mut expected: Vec<_> =
                        old.iter().take(limit).chain(data).copied().collect();
                    if expected.len() > limit {
                        let head = limit / 2;
                        expected = expected[..head]
                            .iter()
                            .chain(&expected[expected.len() - (limit - head)..])
                            .copied()
                            .collect();
                    }
                    assert_eq!(retained_capture(old, data, bound), expected);
                }
            }
        }
    }
}

impl RestoredObserver {
    /// Scalar dictionary keys may have identical JSON spellings. Encode them
    /// directly instead of decoding a serde map and losing those duplicate wires.
    pub fn python_wire(&self) -> String {
        let mut fields: Vec<_> = self
            .semantic
            .entries()
            .map(|(key, value)| {
                format!(
                    "{}:{}",
                    Value::String(key.into()).python_wire(),
                    value.python_wire()
                )
            })
            .collect();
        let value = |key: &str, value: &Value| {
            format!(
                "{}:{}",
                Value::String(key.into()).python_wire(),
                value.python_wire()
            )
        };
        fields.push(value(
            "terminal_event_seen",
            &Value::Bool(self.terminal_event_seen),
        ));
        for (key, field) in [
            ("live_failure", &self.live_failure),
            ("semantic_failures", &self.semantic_failures),
            ("retry_failure", &self.retry_failure),
            ("terminal_text", &self.terminal_text),
        ] {
            fields.push(value(key, field));
        }
        fields.push(value(
            "text_chunks",
            &Value::Array(
                self.text_chunks
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        ));
        for (key, field) in [
            ("text_truncated", &self.text_truncated),
            ("model_changes", &self.model_changes),
            ("last_model", &self.last_model),
        ] {
            fields.push(value(key, field));
        }
        fields.push(format!("\"context\":{}", self.context.python_wire()));
        fields.push(format!("\"models\":{}", self.models.python_wire()));
        format!("{{{}}}", fields.join(","))
    }
}
