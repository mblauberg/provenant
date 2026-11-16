//! Typed F3 run records and helpers consumed by the binary facade.

use super::coordination::facade::{AttemptTarget, FacadeError, WaitCondition};
use provenant_core::OperationId;
use provenant_records::WorkKey;
use provenant_records::run::{AttemptRecord, RunEvent};
use serde::{Deserialize, Serialize};

/// A successful submission acknowledgement. The operation id is the
/// idempotency key; targets identify the attempts accepted by the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_graph: Option<provenant_records::run::PeerGraph>,
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
    pub key: WorkKey,
    pub attempt_id: provenant_core::AttemptId,
    pub state: String,
    pub reason: Option<String>,
    pub exit_code: Option<i64>,
    pub finished_at: Option<provenant_core::WallTime>,
    pub retry_of: Option<provenant_core::AttemptId>,
    pub superseded_by: Option<provenant_core::AttemptId>,
    pub cause: Option<provenant_records::run::SettlementReason>,
    pub requested_route: provenant_records::run::RequestedRoute,
    pub applied_route: Option<provenant_records::run::RouteRecord>,
    pub queued_at: provenant_core::WallTime,
    pub started_at: Option<provenant_core::WallTime>,
    pub last_output_at: Option<provenant_core::WallTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering: Option<provenant_records::run::SteeringStatus>,
}

const PEER_PROJECT_BINDING_DETAIL: &str = "project binding state directory is unsafe on the host";
const PEER_PROJECT_BINDING_NEXT: &str = concat!(
    "ask the host owner to check that the state directory is owned by the current user ",
    "and private from other users, then retry",
);

fn is_project_binding_directory_refusal(state: &provenant_records::run::AttemptState) -> bool {
    matches!(
        state,
        provenant_records::run::AttemptState::Terminal {
            terminal_cause: Some(
                provenant_records::run::TerminalCause::ProjectBindingDirectoryUnsafe
            ),
            ..
        }
    )
}

fn project_peer_attempt_state_diagnostics(state: &mut provenant_records::run::AttemptState) {
    if !is_project_binding_directory_refusal(state) {
        return;
    }
    if let provenant_records::run::AttemptState::Terminal {
        terminal_note,
        reason,
        ..
    } = state
    {
        if reason.is_some() {
            *reason = Some(PEER_PROJECT_BINDING_DETAIL.into());
        }
        if terminal_note.is_some() {
            *terminal_note = Some(PEER_PROJECT_BINDING_NEXT.into());
        }
    }
}

/// Remove host-local project-binding repair details before an attempt crosses
/// the peer boundary. The stored/local record remains unchanged.
pub fn project_peer_attempt_diagnostics(record: &mut AttemptRecord) {
    project_peer_attempt_state_diagnostics(&mut record.state);
}

/// Project the terminal event's self-contained cause, without consulting the
/// mutable attempt record that may have changed since the event was written.
pub fn project_peer_run_event_diagnostics(event: &mut RunEvent) {
    project_peer_attempt_state_diagnostics(&mut event.change);
}

impl From<AttemptRecord> for AttemptSummary {
    fn from(record: AttemptRecord) -> Self {
        let projected = provenant_records::wire::LaneRow::project(&record, Default::default());
        let reason = if is_project_binding_directory_refusal(&record.state) {
            Some(PEER_PROJECT_BINDING_DETAIL.to_owned())
        } else {
            projected.reason
        };
        let finished_at = match &record.state {
            provenant_records::run::AttemptState::Terminal { finished_at, .. } => {
                Some(*finished_at)
            }
            _ => None,
        };
        let extra = record.extra;
        Self {
            extra,
            key: record.key,
            attempt_id: record.attempt_id,
            state: projected.state,
            reason,
            exit_code: projected.exit_code,
            finished_at,
            retry_of: record.retry_of,
            superseded_by: record.superseded_by,
            cause: record.settlement_reason,
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
            steering: projected.steering,
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
        lane_waits: Vec<provenant_records::run::LaneWait>,
        #[serde(with = "attempt_wire")]
        attempt: AttemptRecord,
    },
    Wait {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
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

impl RunResult {
    /// Project full records before peer transport. Stored records retain the
    /// detailed local repair; compact summaries use a safe reason.
    pub fn project_peer_diagnostics(&mut self) {
        match self {
            Self::List { .. } => {}
            Self::Status { attempt, .. }
            | Self::Cancel { attempt, .. }
            | Self::Resume { attempt, .. } => project_peer_attempt_diagnostics(attempt),
            Self::Wait { attempts, .. } => {
                for attempt in attempts {
                    project_peer_attempt_diagnostics(attempt);
                }
            }
        }
    }
}

fn encode_attempt(record: &AttemptRecord) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = provenant_records::run::public_attempt_value(record)?;
    let seconds = provenant_records::wire::provider_seconds(record.provider_ms);
    let object = value.as_object_mut().ok_or_else(|| {
        <serde_json::Error as serde::ser::Error>::custom("attempt record must be an object")
    })?;
    object.remove("provider_ms");
    object.insert("provider_seconds".into(), serde_json::to_value(seconds)?);
    if let Some(silent_for_ms) = record.provider_silent_for_ms.0 {
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
    record.provider_silent_for_ms.0 = silent_for_ms;
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

/// Finalise every wait return through the same snapshot rule. At expiry the
/// owner may refresh local records; the callback must not start remote reads or
/// grant another wait budget.
pub fn finish_wait_snapshot(
    condition: WaitCondition,
    mut attempts: Vec<AttemptRecord>,
    expired: bool,
    observation: Option<String>,
    refresh_local: impl FnOnce(&mut [AttemptRecord]) -> Result<(), FacadeError>,
) -> Result<RunResult, FacadeError> {
    if expired {
        refresh_local(&mut attempts)?;
    }
    let satisfied = wait_satisfied(condition, attempts.iter().map(|record| &record.state));
    Ok(RunResult::Wait {
        extra: Default::default(),
        observation,
        condition,
        attempts,
        satisfied,
        timed_out: expired && !satisfied,
    })
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
    use provenant_core::{AttemptId, Digest, ProjectId, WallTime};
    use provenant_records::WorkKey;
    use provenant_records::{
        CanonicalV1,
        run::{AttemptState, Outcome},
    };

    fn attempt() -> AttemptRecord {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": "02020202020202020202020202020202",
            "task_id": "04040404040404040404040404040404",
            "project_id": ProjectId::from_bytes([3; 16]),
            "key": WorkKey::new(ProjectId::from_bytes([3; 16]), "graph", "node", 1).unwrap(),
            "operation_id": "wire-silence",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": Digest::from_bytes([5; 32]),
            "queued_at": WallTime::from_micros(1).unwrap(),
            "state": {"state":"running"},
            "usage": {},
            "read_only_writable": [],
            "protected_paths": [], "trace": {},
            "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"},"digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
        }))
        .unwrap()
    }

    #[test]
    fn live_wire_carries_silence_without_persisting_it() {
        let mut record = attempt();
        record.provider_silent_for_ms.0 = Some(1_800_000);
        let mut other_read = record.clone();
        other_read.provider_silent_for_ms.0 = None;
        other_read.process_observation_valid.0 = true;
        assert_eq!(record, other_read);
        assert!(
            serde_json::to_value(&record)
                .unwrap()
                .get("provider_silent_for_ms")
                .is_none()
        );
        let wire = encode_attempt(&record).unwrap();
        assert_eq!(wire["provider_silent_for_ms"], 1_800_000);
        assert_eq!(
            decode_attempt(wire).unwrap().provider_silent_for_ms.0,
            Some(1_800_000)
        );
    }

    #[test]
    fn public_detail_and_summary_hide_only_private_attempt_metadata() {
        let mut record = attempt();
        let command: provenant_records::PortCommand = serde_json::from_value(serde_json::json!({
            "argv":["admission-check"],
            "exit_codes":{"success":[0],"unknown":[],"other":"refused"}
        }))
        .unwrap();
        let binding = provenant_records::run::RepositoryAdmissionBinding::new(
            provenant_records::PortSource::Project,
            &command,
        );
        record.repository_admission_binding = Some(binding.clone());
        record.extra.insert(
            "visible_extension".into(),
            serde_json::json!({"kept": true}),
        );

        // The stored record keeps the private lifecycle binding.
        assert_eq!(
            serde_json::to_value(&record).unwrap()["repository_admission_binding"],
            serde_json::to_value(&binding).unwrap()
        );

        let detail = encode_attempt(&record).unwrap();
        assert!(detail.get("repository_admission_binding").is_none());
        assert_eq!(
            detail["visible_extension"],
            serde_json::json!({"kept": true})
        );

        let summary =
            serde_json::to_value(AttemptSummary::from(record)).expect("summary projection");
        assert!(summary.get("repository_admission_binding").is_none());
        assert_eq!(
            summary["visible_extension"],
            serde_json::json!({"kept": true})
        );
    }

    // Spec: docs/specs/state.md#config-never-widens-authority
    #[test]
    fn peer_projection_keeps_binding_repair_local_and_projects_a_safe_reason() {
        let private_path = "/private/host-state/projects/fixture";
        let private_repair = "run chmod 700 '/private/host-state/projects/fixture' and retry";
        let mut record = attempt();
        record
            .skipped_routes
            .push(provenant_records::run::SkippedRoute {
                route: "fixture/model".into(),
                kind: provenant_records::run::SkipKind::Refused,
                detail: "project binding state directory is unsafe".into(),
                confinement_guard: Some(
                    provenant_records::diagnostic::ConfinementGuard::ProjectBindingDirectoryUnsafe,
                ),
                ..Default::default()
            });
        record.state = AttemptState::Terminal {
            extra: Default::default(),
            terminal_cause: Some(
                provenant_records::run::TerminalCause::ProjectBindingDirectoryUnsafe,
            ),
            terminal_note: Some(private_repair.into()),
            stop: None,
            outcome: Outcome::PermissionDenied,
            exit_code: None,
            finished_at: WallTime::from_micros(2).unwrap(),
            reason: Some(format!(
                "{private_path} has unsafe ownership or permissions"
            )),
        };

        let local = serde_json::to_string(&record).unwrap();
        assert!(local.contains(private_path));
        assert!(local.contains(private_repair));

        let mut peer = RunResult::Status {
            extra: Default::default(),
            lane_waits: Vec::new(),
            attempt: record.clone(),
        };
        peer.project_peer_diagnostics();
        let remote = serde_json::to_string(&peer).unwrap();
        assert!(!remote.contains(private_path));
        assert!(!remote.contains(private_repair));
        assert!(remote.contains(PEER_PROJECT_BINDING_DETAIL));
        assert!(remote.contains(PEER_PROJECT_BINDING_NEXT));

        let mut unrelated = record.clone();
        if let AttemptState::Terminal { terminal_cause, .. } = &mut unrelated.state {
            *terminal_cause = Some(provenant_records::run::TerminalCause::AccessPolicyRefused);
        }
        project_peer_attempt_diagnostics(&mut unrelated);
        let unrelated_remote = serde_json::to_string(&unrelated).unwrap();
        assert!(unrelated_remote.contains(private_path));
        assert!(unrelated_remote.contains(private_repair));

        let mut event = RunEvent {
            extra: Default::default(),
            schema_version: 1,
            sequence: 1,
            project_id: record.project_id,
            attempt_id: record.attempt_id,
            revision: record.revision,
            change: record.state.clone(),
            trace: Default::default(),
        };
        project_peer_run_event_diagnostics(&mut event);
        let remote_event = serde_json::to_string(&event).unwrap();
        assert!(!remote_event.contains(private_path));
        assert!(!remote_event.contains(private_repair));
        assert!(remote_event.contains(PEER_PROJECT_BINDING_DETAIL));
        assert!(remote_event.contains(PEER_PROJECT_BINDING_NEXT));

        let summary = serde_json::to_string(&AttemptSummary::from(record)).unwrap();
        assert!(!summary.contains(private_path));
        assert!(!summary.contains(private_repair));
        assert!(summary.contains(PEER_PROJECT_BINDING_DETAIL));
    }

    #[test]
    fn wait_observation_round_trips_and_rejects_wrong_types() {
        for observation in [None, Some("event subscription".to_owned())] {
            let result = finish_wait_snapshot(
                WaitCondition::Any,
                Vec::new(),
                false,
                observation.clone(),
                |_| Ok(()),
            )
            .unwrap();
            let mut value = serde_json::to_value(&result).unwrap();
            value["future_observation"] = serde_json::json!({"source": "newer minor"});
            let decoded: RunResult = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), value);
            for malformed in [
                serde_json::json!(false),
                serde_json::json!(7),
                serde_json::json!({}),
            ] {
                value["observation"] = malformed;
                assert!(serde_json::from_value::<RunResult>(value.clone()).is_err());
            }
        }
    }

    #[test]
    fn expired_wait_evaluates_the_final_local_snapshot_before_its_verdict() {
        for condition in [WaitCondition::Any, WaitCondition::All] {
            let pending = attempt();
            let result = finish_wait_snapshot(
                condition,
                vec![pending],
                true,
                Default::default(),
                |records| {
                    records[0].state = AttemptState::Terminal {
                        extra: Default::default(),
                        terminal_cause: None,
                        terminal_note: None,
                        stop: None,
                        outcome: Outcome::Succeeded,
                        exit_code: Some(0),
                        finished_at: WallTime::from_micros(2).unwrap(),
                        reason: None,
                    };
                    Ok(())
                },
            )
            .unwrap();
            assert!(matches!(
                result,
                RunResult::Wait {
                    satisfied: true,
                    timed_out: false,
                    ..
                }
            ));
        }
    }

    #[test]
    fn input_required_wakes_all_without_waiting_for_a_sibling() {
        let input = AttemptState::Terminal {
            extra: Default::default(),
            terminal_cause: None,
            terminal_note: None,
            stop: None,
            outcome: Outcome::InputRequired,
            exit_code: None,
            finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
            reason: None,
        };
        let states = [
            AttemptState::Running {
                extra: Default::default(),
                child_wait: None,
            },
            input,
        ];
        assert!(wait_satisfied(WaitCondition::All, states.iter()));
        assert!(!wait_satisfied(
            WaitCondition::All,
            [
                AttemptState::Running {
                    extra: Default::default(),
                    child_wait: None
                },
                AttemptState::Queued {
                    extra: Default::default()
                }
            ]
            .iter()
        ));
    }
    #[test]
    fn status_waits_round_trip_and_reject_malformed_reason_facts()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = RunResult::Status {
            extra: Default::default(),
            lane_waits: vec![provenant_records::run::LaneWait {
                extra: Default::default(),
                attempt_id: AttemptId::from_bytes([1; 16]),
                lane_class: provenant_records::run::LaneClass::Writer,
                waiting_for: provenant_records::run::LaneWaitReason::ProviderSilence {
                    silent_for_ms: 3_000,
                },
            }],
            attempt: attempt(),
        };
        let encoded = serde_json::to_value(&result)?;
        assert_eq!(
            serde_json::from_value::<RunResult>(encoded.clone())?,
            result
        );
        let mut malformed = encoded;
        malformed["lane_waits"][0]["waiting_for"] = serde_json::json!({
            "kind":"provider_silence", "silent_for_ms":"long"
        });
        assert!(serde_json::from_value::<RunResult>(malformed).is_err());
        Ok(())
    }
}
