//! Contracts that break the execution/batch import cycle. No process ownership here.
use crate::batch_execution::{BatchInput, CancelRequest, RouteInput};
use crate::field::Field;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

pub const MAX_EXECUTION_WAIT_SECONDS: u8 = 55;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    Dispatch,
    Batch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchInput {
    #[serde(flatten)]
    pub route: RouteInput,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub prompt: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub prompt_file: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub resume: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub handoff: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub session: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub fresh: Field<bool>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub task_id: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub timeout_seconds: Field<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub wait_seconds: Field<serde_json::Number>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub run_id: String,
    pub task_id: String,
    pub attempt: u64,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub run_dir: Field<PathBuf>,
}

/// Failure to record a launch prevents the owner from starting.
pub trait LaunchObserver {
    fn on_launch(&mut self, launch: &Launch) -> Result<(), ExecutionError>;
    fn continuation(&self) -> bool {
        false
    }
    fn guarded_council(&self) -> bool {
        false
    }
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("{fix}")]
    Rejected { code: String, fix: String },
    #[error("execution wait cancelled")]
    Cancelled,
    #[error("{operation} failed at {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// State lives in the facade instance. Implementations own cancellation and
/// terminal publication; dropping a caller does not transfer process custody.
pub trait ExecutionFacade {
    fn dispatch(
        &mut self,
        input: DispatchInput,
        observer: &mut dyn LaunchObserver,
    ) -> Result<BTreeMap<String, Value>, ExecutionError>;
    fn batch(
        &mut self,
        input: BatchInput,
        observer: &mut dyn LaunchObserver,
    ) -> Result<BTreeMap<String, Value>, ExecutionError>;
    fn cancel(&mut self, request: CancelRequest)
    -> Result<BTreeMap<String, Value>, ExecutionError>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerIdentification {
    pub kind: ExecutionKind,
    pub identifier: String,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub routes: Field<Value>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub task_ids: Field<Vec<String>>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub task_alias: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub timeout: Field<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub resume: Field<bool>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub batch_id: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub next_attempt: Field<u64>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub workspace: Field<PathBuf>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub cwd_projects: Field<BTreeMap<String, PathBuf>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn launch_wire_has_original_field_names_and_no_absent_path() {
        let launch = Launch {
            run_id: "r".into(),
            task_id: "t".into(),
            attempt: 1,
            run_dir: Field::Missing,
        };
        assert_eq!(
            serde_json::to_value(launch).unwrap(),
            json!({"runId":"r","taskId":"t","attempt":1})
        );
    }
    #[test]
    fn dispatch_envelope_keeps_continuation_and_unknown_route_fields() {
        let wire =
            json!({"session":"s", "wait_seconds":55.5, "confidential":true, "adapter":"fixture"});
        let input: DispatchInput = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), wire);
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    #[test]
    fn null_fields_survive_round_trip() {
        let wire = serde_json::json!({"prompt":null,"prompt_file":null,"fresh":null,"wait_seconds":null,"resume":null});
        let input: DispatchInput = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), wire);
    }
}
