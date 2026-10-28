//! Typed F3 run records and helpers consumed by the binary facade.

use super::coordination::facade::{AttemptTarget, WaitCondition};
use provenant_core::OperationId;
use provenant_records::run::{AttemptRecord, RunEvent};
use serde::{Deserialize, Serialize};

/// A successful submission acknowledgement. The operation id is the
/// idempotency key; targets identify the attempts accepted by the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionReceipt {
    pub operation_id: OperationId,
    pub targets: Vec<AttemptTarget>,
    /// The writer worktree a peer created for this submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PeerWorktree>,
}

/// Where a peer writer's commits stay when nothing delivers them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerWorktree {
    pub path: String,
    pub branch: String,
}

/// A compact listing row. Full execution evidence is available through status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptSummary {
    pub attempt_id: provenant_core::AttemptId,
    pub run_id: provenant_core::RunId,
    pub task_id: provenant_core::TaskId,
    pub operation_id: OperationId,
    pub state: provenant_records::run::AttemptState,
    pub requested_route: provenant_records::run::RequestedRoute,
    pub applied_route: Option<provenant_records::run::RouteRecord>,
    pub queued_at: provenant_core::WallTime,
    pub started_at: Option<provenant_core::WallTime>,
    pub last_output_at: Option<provenant_core::WallTime>,
}

impl From<AttemptRecord> for AttemptSummary {
    fn from(record: AttemptRecord) -> Self {
        Self {
            attempt_id: record.attempt_id,
            run_id: record.run_id,
            task_id: record.task_id,
            operation_id: record.operation_id,
            state: record.state,
            requested_route: record.requested_route,
            applied_route: record.applied_route.map(|mut route| {
                route.account_key = None;
                route.source = None;
                route.pool_selection = None;
                route
            }),
            queued_at: record.queued_at,
            started_at: record.started_at,
            last_output_at: record.last_output_at,
        }
    }
}

/// A bounded page of summaries, newest queued first. `next_after` is supplied
/// only when a caller can continue the same ordered selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptPage {
    pub items: Vec<AttemptSummary>,
    pub next_after: Option<AttemptTarget>,
    pub truncated: bool,
}

/// Typed result for the operations in `RunInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunResult {
    List {
        page: AttemptPage,
    },
    Status {
        #[serde(with = "attempt_wire")]
        attempt: AttemptRecord,
    },
    Wait {
        condition: WaitCondition,
        #[serde(with = "attempts_wire")]
        attempts: Vec<AttemptRecord>,
        satisfied: bool,
        timed_out: bool,
    },
    Cancel {
        #[serde(with = "attempt_wire")]
        attempt: AttemptRecord,
    },
    Resume {
        #[serde(with = "attempt_wire")]
        attempt: AttemptRecord,
    },
}

fn encode_attempt(record: &AttemptRecord) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = serde_json::to_value(record)?;
    let seconds = record.provider_ms.map(|millis| millis as f64 / 1000.0);
    let object = value.as_object_mut().ok_or_else(|| {
        <serde_json::Error as serde::ser::Error>::custom("attempt record must be an object")
    })?;
    object.remove("provider_ms");
    object.insert("provider_seconds".into(), serde_json::to_value(seconds)?);
    Ok(value)
}

fn decode_attempt(mut value: serde_json::Value) -> Result<AttemptRecord, String> {
    let object = value.as_object_mut().ok_or("attempt must be an object")?;
    if object.contains_key("provider_ms") {
        return Err("provider_ms is not a wire field".into());
    }
    let seconds = object
        .remove("provider_seconds")
        .ok_or("missing provider_seconds")?;
    let millis = if seconds.is_null() {
        None
    } else {
        let seconds = seconds.as_f64().ok_or("invalid provider_seconds")?;
        let millis = seconds * 1000.0;
        if !millis.is_finite()
            || millis < 0.0
            || millis >= u64::MAX as f64
            || (millis - millis.round()).abs() > 0.000001
        {
            return Err("provider_seconds must be non-negative with millisecond precision".into());
        }
        Some(millis.round() as u64)
    };
    object.insert(
        "provider_ms".into(),
        serde_json::to_value(millis).map_err(|error| error.to_string())?,
    );
    serde_json::from_value(value).map_err(|error| error.to_string())
}

pub(crate) mod attempt_wire {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        record: &AttemptRecord,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        encode_attempt(record)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<AttemptRecord, D::Error> {
        decode_attempt(serde_json::Value::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

pub(crate) mod attempts_wire {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        records: &[AttemptRecord],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        records
            .iter()
            .map(encode_attempt)
            .collect::<Result<Vec<_>, _>>()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<AttemptRecord>, D::Error> {
        Vec::<serde_json::Value>::deserialize(deserializer)?
            .into_iter()
            .map(|value| decode_attempt(value).map_err(serde::de::Error::custom))
            .collect()
    }
}

/// Run events are paged by their monotonic sequence. `cursor` is the sequence
/// a caller should pass as `after` on the next read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEventsPage {
    pub items: Vec<RunEvent>,
    pub cursor: u64,
    pub truncated: bool,
}

/// Reports operations the bound owner can actually serve in this context.
/// Host configuration by itself must not make an execution capability true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCapabilities {
    pub submit: bool,
    pub list: bool,
    pub status: bool,
    pub wait: bool,
    pub cancel: bool,
    pub resume: bool,
    pub output: bool,
    pub events: bool,
}

/// Input-required wakes either condition; other outcomes remain result data.
pub fn wait_satisfied<'a>(
    condition: WaitCondition,
    states: impl Iterator<Item = &'a provenant_records::run::AttemptState>,
) -> bool {
    use provenant_records::run::{AttemptState, Outcome};
    let mut any = false;
    let mut all = true;
    let mut input = false;
    for state in states {
        let terminal = matches!(state, AttemptState::Terminal { .. });
        any |= terminal;
        all &= terminal;
        input |= matches!(
            state,
            AttemptState::Terminal {
                outcome: Outcome::InputRequired,
                ..
            }
        );
    }
    input
        || match condition {
            WaitCondition::Any => any,
            WaitCondition::All => all,
        }
}
#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::run::{AttemptState, Outcome};
    #[test]
    fn input_required_wakes_all_without_waiting_for_a_sibling() {
        let input = AttemptState::Terminal {
            outcome: Outcome::InputRequired,
            exit_code: None,
            finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
            reason: None,
        };
        let states = [AttemptState::Running, input];
        assert!(wait_satisfied(WaitCondition::All, states.iter()));
        assert!(!wait_satisfied(
            WaitCondition::All,
            [AttemptState::Running, AttemptState::Queued].iter()
        ));
    }
}
