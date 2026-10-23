//! Batch boundary for the cyclic execution facade. Owner implementation is a later unit.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Shared route fields remain lossless until execution-input is ported. This is
/// an envelope, not validation: unknown selectors must reach the policy owner.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RouteInput(pub BTreeMap<String, Value>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchTaskInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(flatten)]
    pub route: RouteInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchInput {
    pub tasks: Vec<BatchTaskInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(flatten)]
    pub route: RouteInput,
}

/// Fully validated entry points return a record or a typed execution failure.
/// Cancellation always names its target; callers must never widen a task to a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CancelRequest {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batch_wire_preserves_selectors_and_invalid_numbers_for_policy() {
        let wire = json!({"tasks":[{"prompt":"hi", "model":"fixture", "future":null}],
            "concurrency":1.5, "timeout_seconds":-1, "models":["a","b"]});
        let input: BatchInput = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), wire);
    }

    #[test]
    fn cancellation_does_not_invent_optional_fields() {
        let wire = json!({"id":"task-1"});
        let input: CancelRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), wire);
    }
}
