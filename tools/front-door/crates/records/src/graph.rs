//! Shared graph state vocabulary used by runtime records and peer projections.
use crate::run::TerminalCause;
use serde::{Deserialize, Serialize};

/// Stable identity for one graph route attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptKey {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_attempt_ref: Option<String>,
    pub graph_id: String,
    pub node_id: String,
    /// One-based WorkKey attempt identity in the journal's project namespace.
    /// A continuation advances this number without consuming another route.
    pub attempt: u32,
    /// Zero-based position in that node's ordered route list.
    pub route_index: u16,
}

impl AttemptKey {
    pub fn work_key(
        &self,
        project: provenant_core::ProjectId,
    ) -> Result<crate::WorkKey, crate::work_key::WorkKeyError> {
        crate::WorkKey::new(project, &self.graph_id, &self.node_id, self.attempt)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Blocked,
    Skipped,
}

impl NodeState {
    pub fn settled(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Starting,
    Running,
    Unresolved,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    TaskFailed,
    DeclaredStop,
    StoppedNoWork,
    OtherFailure,
    ReviewRequired,
    AuthRequired,
    UsageLimited,
    AdapterUnavailable,
    PreparationRefused(TerminalCause),
    DeadlineExceeded,
    Unclassified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOutcome {
    Succeeded,
    Partial,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Pending,
    Complete,
    Cancelled,
    Paused,
}

/// A typed edge selected when a bounded delivery loop settles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopOutcomeEdge {
    pub outcome: LoopOutcome,
    pub node: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopOutcome {
    Succeeded,
    Exhausted,
}

/// Durable loop progress; round counts admitted repair attempts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryLoopProgress {
    pub name: String,
    pub round: usize,
    /// The WorkKey identity whose admission consumed the latest round.
    pub last_attempt: AttemptKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationProgress {
    /// Zero-based index into the sealed ladder.
    pub rung: usize,
    pub cause: EscalationCause,
    /// Repair identity that produced this cause; repeated observations are no-ops.
    pub transition_attempt: AttemptKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationCause {
    NoCommit,
    UncommittedEdits,
    ReviewRepair,
    CiRed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewShape {
    #[default]
    Single,
    Panel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HygieneOutcomeKind {
    Clean,
    NoCommit,
    UncommittedEdits,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HygieneOutcome {
    pub kind: HygieneOutcomeKind,
    pub before_head: Option<String>,
    pub after_head: Option<String>,
    pub handoff: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkReason {
    LoopExhausted,
    LadderExhausted,
    NoCommit,
    UncommittedEdits,
    CiUnavailable,
    Conflict,
    ReviewEvidence,
    OperationError,
}

#[cfg(test)]
mod delivery_contract_tests {
    use super::*;

    #[test]
    fn delivery_records_round_trip() {
        let progress = EscalationProgress {
            rung: 2,
            cause: EscalationCause::CiRed,
            transition_attempt: AttemptKey {
                extra: Default::default(),
                retry_generation: None,
                previous_attempt_ref: None,
                graph_id: "g".into(),
                node_id: "fix".into(),
                attempt: 3,
                route_index: 0,
            },
        };
        let wire = serde_json::to_value(&progress).unwrap();
        assert_eq!(wire["cause"], "ci_red");
        assert_eq!(
            serde_json::from_value::<EscalationProgress>(wire).unwrap(),
            progress
        );
        let hygiene = HygieneOutcome {
            kind: HygieneOutcomeKind::UncommittedEdits,
            before_head: Some("before".into()),
            after_head: Some("after".into()),
            handoff: None,
        };
        let wire = serde_json::to_value(&hygiene).unwrap();
        assert_eq!(wire["kind"], "uncommitted_edits");
        assert_eq!(
            serde_json::from_value::<HygieneOutcome>(wire).unwrap(),
            hygiene
        );
        for reason in [
            ParkReason::LoopExhausted,
            ParkReason::LadderExhausted,
            ParkReason::NoCommit,
            ParkReason::UncommittedEdits,
            ParkReason::CiUnavailable,
            ParkReason::Conflict,
            ParkReason::ReviewEvidence,
            ParkReason::OperationError,
        ] {
            let wire = serde_json::to_value(reason).unwrap();
            assert_eq!(serde_json::from_value::<ParkReason>(wire).unwrap(), reason);
        }
        let round = DeliveryLoopProgress {
            name: "repair".into(),
            round: 3,
            last_attempt: AttemptKey {
                extra: Default::default(),
                retry_generation: None,
                previous_attempt_ref: None,
                graph_id: "g".into(),
                node_id: "fix".into(),
                attempt: 3,
                route_index: 0,
            },
        };
        assert_eq!(
            serde_json::from_value::<DeliveryLoopProgress>(serde_json::to_value(&round).unwrap())
                .unwrap(),
            round
        );
    }
}
