//! Contracts that break the execution/batch import cycle. No process ownership here.
use crate::batch_execution::{BatchInput, CancelRequest, RouteInput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

pub const MAX_EXECUTION_WAIT_SECONDS: u8 = 55;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind { Dispatch, Batch }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchInput {
    #[serde(flatten)]
    pub route: RouteInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fresh: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_seconds: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub run_id: String,
    pub task_id: String,
    pub attempt: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<PathBuf>,
}

/// Failure to record a launch prevents the owner from starting.
pub trait LaunchObserver {
    fn on_launch(&mut self, launch: &Launch) -> Result<(), ExecutionError>;
    fn continuation(&self) -> bool { false }
    fn guarded_council(&self) -> bool { false }
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("{fix}")]
    Rejected { code: String, fix: String },
    #[error("execution wait cancelled")]
    Cancelled,
    #[error("{operation} failed at {path}")]
    Io { operation: &'static str, path: PathBuf, #[source] source: std::io::Error },
}

/// State lives in the facade instance. Implementations own cancellation and
/// terminal publication; dropping a caller does not transfer process custody.
pub trait ExecutionFacade {
    fn dispatch(&mut self, input: DispatchInput, observer: &mut dyn LaunchObserver)
        -> Result<BTreeMap<String, Value>, ExecutionError>;
    fn batch(&mut self, input: BatchInput, observer: &mut dyn LaunchObserver)
        -> Result<BTreeMap<String, Value>, ExecutionError>;
    fn cancel(&mut self, request: CancelRequest)
        -> Result<BTreeMap<String, Value>, ExecutionError>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerIdentification {
    pub kind: ExecutionKind,
    pub identifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routes: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd_projects: Option<BTreeMap<String, PathBuf>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn launch_wire_has_original_field_names_and_no_absent_path() {
        let launch = Launch { run_id:"r".into(), task_id:"t".into(), attempt:1, run_dir:None };
        assert_eq!(serde_json::to_value(launch).unwrap(), json!({"runId":"r","taskId":"t","attempt":1}));
    }
    #[test]
    fn dispatch_envelope_keeps_continuation_and_unknown_route_fields() {
        let wire = json!({"session":"s", "wait_seconds":55.5, "confidential":true, "adapter":"fixture"});
        let input: DispatchInput = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), wire);
    }
}
