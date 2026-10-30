//! Typed F3 run records and helpers consumed by the binary facade.

use super::coordination::facade::{AttemptTarget, WaitCondition};
use provenant_core::OperationId;
use provenant_records::run::{AttemptRecord, RunEvent};
use serde::{Deserialize, Serialize};

/// A successful submission acknowledgement. The operation id is the
/// idempotency key; targets identify the attempts accepted by the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionReceipt {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub operation_id: OperationId,
    pub targets: Vec<AttemptTarget>,
    /// The writer worktree a peer created for this submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PeerWorktree>,
}

/// Where a peer writer's commits stay when nothing delivers them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerWorktree {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub path: String,
    pub branch: String,
}

/// A compact listing row. Full execution evidence is available through status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptSummary {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
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
            extra: record.extra,
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
pub struct AttemptPage {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub items: Vec<AttemptSummary>,
    pub next_after: Option<AttemptTarget>,
    pub truncated: bool,
}

/// Typed result for the operations in `RunInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RunResult {
    List {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        page: AttemptPage,
    },
    Status {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        #[serde(with = "attempt_wire")]
        attempt: AttemptRecord,
    },
    Wait {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        condition: WaitCondition,
        #[serde(with = "attempts_wire")]
        attempts: Vec<AttemptRecord>,
        satisfied: bool,
        timed_out: bool,
    },
    Cancel {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        #[serde(with = "attempt_wire")]
        attempt: AttemptRecord,
    },
    Resume {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
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
    if let Some(silent_for_ms) = record.provider_silent_for_ms {
        object.insert(
            "provider_silent_for_ms".into(),
            serde_json::json!(silent_for_ms),
        );
    }
    Ok(value)
}

fn decode_attempt(mut value: serde_json::Value) -> Result<AttemptRecord, String> {
    let object = value.as_object_mut().ok_or("attempt must be an object")?;
    if object.contains_key("provider_ms") {
        return Err("provider_ms is not a wire field".into());
    }
    let silent_for_ms = object
        .remove("provider_silent_for_ms")
        .and_then(|value| value.as_u64());
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
    let mut record: AttemptRecord =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    record.provider_silent_for_ms = silent_for_ms;
    Ok(record)
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
pub struct RunEventsPage {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub items: Vec<RunEvent>,
    pub cursor: u64,
    pub truncated: bool,
}

/// Reports operations the bound owner can actually serve in this context.
/// Host configuration by itself must not make an execution capability true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    use provenant_core::{AttemptId, Digest, ProjectId, RunId, TaskId, WallTime};
    use provenant_records::{
        CanonicalV1,
        run::{AttemptState, Outcome},
    };

    fn attempt() -> AttemptRecord {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "wire-silence",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": Digest::from_bytes([5; 32]),
            "queued_at": WallTime::from_micros(1).unwrap(),
            "state": {"state":"running"},
            "usage": {},
            "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"},"digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
        }))
        .unwrap()
    }

    #[test]
    fn live_wire_carries_silence_without_persisting_it() {
        let mut record = attempt();
        record.provider_silent_for_ms = Some(1_800_000);
        assert!(
            serde_json::to_value(&record)
                .unwrap()
                .get("provider_silent_for_ms")
                .is_none()
        );
        let wire = encode_attempt(&record).unwrap();
        assert_eq!(wire["provider_silent_for_ms"], 1_800_000);
        assert_eq!(
            decode_attempt(wire).unwrap().provider_silent_for_ms,
            Some(1_800_000)
        );
    }

    #[test]
    fn input_required_wakes_all_without_waiting_for_a_sibling() {
        let input = AttemptState::Terminal {
            extra: Default::default(),
            outcome: Outcome::InputRequired,
            exit_code: None,
            finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
            reason: None,
        };
        let states = [
            AttemptState::Running {
                extra: Default::default(),
            },
            input,
        ];
        assert!(wait_satisfied(WaitCondition::All, states.iter()));
        assert!(!wait_satisfied(
            WaitCondition::All,
            [
                AttemptState::Running {
                    extra: Default::default()
                },
                AttemptState::Queued {
                    extra: Default::default()
                }
            ]
            .iter()
        ));
    }
}
