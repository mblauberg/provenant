//! Batch boundary for the cyclic execution facade. Owner implementation is a later unit.
use crate::field::Field;
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
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub id: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub prompt: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub prompt_file: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub timeout_seconds: Field<serde_json::Number>,
    #[serde(flatten)]
    pub route: RouteInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchInput {
    pub tasks: Vec<BatchTaskInput>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub concurrency: Field<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub wait_seconds: Field<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub timeout_seconds: Field<serde_json::Number>,
    #[serde(flatten)]
    pub route: RouteInput,
}

/// Fully validated entry points return a record or a typed execution failure.
/// Cancellation always names its target; callers must never widen a task to a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CancelRequest {
    pub id: String,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub reason: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub operation_id: Field<String>,
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

#[cfg(test)]
mod presence_tests {
    use super::*;
    #[test]
    fn null_fields_survive_round_trip() {
        let wire = serde_json::json!({"tasks":[{"prompt":null,"prompt_file":null,"timeout_seconds":null}],"concurrency":null,"wait_seconds":null});
        let input: BatchInput = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), wire);
    }
}

#[cfg(test)]
mod cancel_presence_tests {
    use super::*;
    #[test]
    fn cancellation_nulls_are_preserved() {
        let wire = serde_json::json!({"id":"task","reason":null,"operation_id":null});
        let input: CancelRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), wire);
    }
}
