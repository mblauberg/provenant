//! Source launch-builder wire shapes. Execution remains with the supervisor.
use crate::lane_exit::{ExitRecord, Member, ProcessInteger, ReceiptFields};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::num::NonZeroU64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LaunchError {
    #[error("lane durations must be finite positive seconds")]
    Duration,
    #[error("invalid batch cancel identity")]
    BatchIdentity,
    #[error("launch integer is outside the u64 range")]
    NumberOutOfRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Milliseconds(NonZeroU64);
impl Milliseconds {
    pub fn get(self) -> u64 {
        self.0.get()
    }
    pub fn from_seconds(seconds: &Value) -> Result<Self, LaunchError> {
        if let Some(n) = seconds.as_u64().filter(|n| *n > 0) {
            return n
                .checked_mul(1000)
                .and_then(NonZeroU64::new)
                .map(Self)
                .ok_or(LaunchError::NumberOutOfRange);
        }
        let n = seconds.as_f64().ok_or(LaunchError::Duration)?;
        Self::from_float_seconds(n)
    }
    pub fn from_float_seconds(seconds: f64) -> Result<Self, LaunchError> {
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(LaunchError::Duration);
        }
        let ms = (seconds * 1000.0).ceil();
        if !ms.is_finite() || ms >= 18_446_744_073_709_551_616.0 {
            return Err(LaunchError::NumberOutOfRange);
        }
        NonZeroU64::new(ms as u64)
            .map(Self)
            .ok_or(LaunchError::Duration)
    }
}

/// Validation before constructing a batch cancellation path. Falsy identities
/// ask the dispatch owner for its published identity instead.
pub fn batch_cancel_identity(value: &Value) -> Result<Option<&str>, LaunchError> {
    if !provenant_core::py::python_truth(value) {
        return Ok(None);
    }
    let text = value.as_str().ok_or(LaunchError::BatchIdentity)?;
    let suffix = text
        .strip_prefix("batch-")
        .filter(|s| !s.is_empty())
        .ok_or(LaunchError::BatchIdentity)?;
    if !suffix.chars().all(crate::pending::python_digit) {
        return Err(LaunchError::BatchIdentity);
    }
    Ok(Some(text))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub schema: String,
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(with = "crate::pending::ordered_pairs")]
    pub env: Vec<(String, String)>,
    pub stdin: StdinSpec,
    pub attempt_dir: String,
    pub run_token: String,
    pub capture: CaptureSpec,
    pub silence_observation_ms: Milliseconds,
    pub timeouts_ms: Timeouts,
    pub progress: ProgressSpec,
    pub cancel_markers: Vec<String>,
    pub observer: ObserverCommand,
    pub spare: SparePolicy,
}
impl LaunchSpec {
    pub fn from_json(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        crate::pending::checked_ordered_decode(raw)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StdinSpec {
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub path: Member<StdinPath>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct StdinPath(String);
#[derive(Debug, thiserror::Error)]
#[error("stdin path must be non-empty")]
pub struct EmptyStdinPath;
impl StdinPath {
    pub fn new(path: String) -> Result<Self, EmptyStdinPath> {
        if path.is_empty() {
            Err(EmptyStdinPath)
        } else {
            Ok(Self(path))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for StdinPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureSpec {
    pub events: String,
    pub stderr: String,
    pub max_bytes: NonZeroU64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeouts {
    pub startup: Milliseconds,
    pub wall: Milliseconds,
    pub settle: Milliseconds,
    pub child_grace: Milliseconds,
    pub root_flush: Milliseconds,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressSpec {
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub writer_tree: Member<WriterTree>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriterTree {
    pub root: String,
    pub exclude: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserverCommand {
    pub argv: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparePolicy {
    pub code_mode_host: bool,
}

/// Source stdout notifications also accept the older `type` discriminator. The
/// raw notification remains open until the launcher applies that precedence.
pub type LaunchNotification = crate::pending::OrderedRecord<LaunchNotificationFields>;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchNotificationFields {
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub event: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent", rename = "type")]
    pub legacy_type: Member<Value>,
    #[serde(flatten)]
    pub fields: crate::pending::OrderedMap<crate::pending::OrderedValue>,
}

/// Provider identity is observation only, without an OS process handle.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderIdentity {
    pub pid: ProcessInteger,
    pub returncode: Option<ProcessInteger>,
}
/// This is a native return value, not a JSON enum with an invented wire tag.
#[derive(Debug, Clone, PartialEq)]
pub enum LaunchOutcome {
    Unsupported {
        warning: String,
    },
    Supported {
        record: Box<ExitRecord>,
        fields: Box<ReceiptFields>,
        provider: Option<ProviderIdentity>,
        started_at: Value,
        warning: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn source_builder_records_round_trip_with_optional_stdin_and_writer_progress() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut cases = vec![];
        for mode in ["worktree_write", "read_only"] {
            for stdin in [true, false] {
                cases.push(json!({"root":root,"mode":mode,"stdin":stdin}));
            }
        }
        if let Some(outputs) = crate::test_support::oracle("lane_launch_contract", &json!(cases)) {
            for raw in outputs.as_array().unwrap() {
                let wire = raw.as_str().unwrap();
                let typed = LaunchSpec::from_json(wire.as_bytes()).unwrap();
                assert_eq!(serde_json::to_string(&typed).unwrap(), wire);
                assert_eq!(typed.timeouts_ms.wall.get(), 1125);
                assert_eq!(typed.timeouts_ms.startup.get(), 25);
                assert_eq!(typed.silence_observation_ms.get(), 125);
                assert_eq!(
                    typed
                        .env
                        .iter()
                        .find(|(key, _)| key == "SECRET_REFERENCE")
                        .map(|(_, v)| v)
                        .unwrap(),
                    "synthetic-value"
                );
                assert_eq!(typed.capture.events, "events.jsonl");
            }
        }
        for raw in [
            json!({"event":"started","pid":123}),
            json!({"event":"progress","last_progress_at":"stamp"}),
            json!({"type":"started","pid":null,"unknown":true}),
        ] {
            let typed: LaunchNotification = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(
                serde_json::to_string(&typed).unwrap(),
                serde_json::to_string(&raw).unwrap()
            );
        }
    }
    #[test]
    fn notification_and_environment_preserve_source_order() {
        let raw = r#"{"event":"started","z":{"z":1,"a":2},"a":3}"#;
        let typed: LaunchNotification = serde_json::from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&typed).unwrap(), raw);
        #[derive(Serialize, Deserialize)]
        struct Environment(#[serde(with = "crate::pending::ordered_pairs")] Vec<(String, String)>);
        let raw = r#"{"Z":"1","A":"2"}"#;
        let typed: Environment = serde_json::from_str(raw).unwrap();
        assert_eq!(typed.0[0].0, "Z");
        assert_eq!(serde_json::to_string(&typed).unwrap(), raw);
    }
    #[test]
    fn launch_wire_rejects_zero_limits_and_deadlines() {
        assert!(serde_json::from_str::<CaptureSpec>(
            r#"{"events":"e","stderr":"s","max_bytes":0}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Timeouts>(
            r#"{"startup":0,"wall":1,"settle":1,"child_grace":1,"root_flush":1}"#
        )
        .is_err());
    }
    #[test]
    fn source_duration_and_batch_failures_match() {
        let cases = json!([{"raw":"true"},{"raw":"NaN"},{"raw":"0"},{"raw":"-1"},{"raw":"0.0001"},{"raw":"1.0001"}]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let result = if c["raw"] == "NaN" {
                    Milliseconds::from_float_seconds(f64::NAN)
                } else {
                    Milliseconds::from_seconds(
                        &serde_json::from_str::<Value>(c["raw"].as_str().unwrap()).unwrap(),
                    )
                };
                match result {
                    Ok(ms) => json!({"wire":ms.get().to_string()}),
                    Err(e) => json!({"error":e.to_string()}),
                }
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("lane_duration", &cases) {
            assert_eq!(json!(actual), expected);
        }
        let cases = json!([{"batch_id":true},{"batch_id":"batch-"},{"batch_id":"batch-no"},{"batch_id":"../batch-123"},{"batch_id":"batch-²"},{"batch_id":"batch-123"}]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|c| match batch_cancel_identity(&c["batch_id"]) {
                Ok(_) => Value::Null,
                Err(e) => json!(e.to_string()),
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("lane_batch", &cases) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn stdin_builder_contract_has_no_null_path_variant() {
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":""})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":null})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":""})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({})).is_ok());
    }
}
