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
