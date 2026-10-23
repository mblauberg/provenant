//! Source launch-builder wire shapes. Execution remains with the supervisor.
use crate::lane_exit::{ExitRecord, Member, ProcessInteger, ReceiptFields};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub silence_observation_ms: u64,
    pub timeouts_ms: Timeouts,
    pub progress: ProgressSpec,
    pub cancel_markers: Vec<String>,
    pub observer: ObserverCommand,
    pub spare: SparePolicy,
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
    pub max_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeouts {
    pub startup: u64,
    pub wall: u64,
    pub settle: u64,
    pub child_grace: u64,
    pub root_flush: u64,
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
                let typed: LaunchSpec = serde_json::from_value(raw.clone()).unwrap();
                assert_eq!(serde_json::to_value(&typed).unwrap(), *raw);
                assert_eq!(typed.timeouts_ms.wall, 1125);
                assert_eq!(typed.timeouts_ms.startup, 25);
                assert_eq!(typed.silence_observation_ms, 125);
                assert_eq!(
                    typed.env.iter().find(|(key, _)| key == "SECRET_REFERENCE").map(|(_, v)| v).unwrap(),
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
            assert_eq!(serde_json::to_value(typed).unwrap(), raw);
        }
    }
    #[test]
    fn notification_and_environment_preserve_source_order() {
        let raw = r#"{"event":"started","z":{"z":1,"a":2},"a":3}"#;
        let typed: LaunchNotification = serde_json::from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&typed).unwrap(), raw);
        #[derive(Serialize, Deserialize)]
        struct Environment(#[serde(with="crate::pending::ordered_pairs")] Vec<(String, String)>);
        let raw = r#"{"Z":"1","A":"2"}"#;
        let typed: Environment = serde_json::from_str(raw).unwrap();
        assert_eq!(typed.0[0].0, "Z");
        assert_eq!(serde_json::to_string(&typed).unwrap(), raw);
    }
    #[test]
    fn stdin_builder_contract_has_no_null_path_variant() {
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":""})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":null})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":""})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({})).is_ok());
    }
}
