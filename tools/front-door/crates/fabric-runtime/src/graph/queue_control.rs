//! Durable queue-control intent records.
use provenant_core::{GitOid, HumanAnswerId, HumanRequestId, OperationId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Accept,
    Fix,
    Retry,
    Drop,
}

/// Metadata is attached only to an existing user-facing operation. It is not
/// accepted from a graph document or stored plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub action: Action,
    pub expected_revision: u64,
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_head: Option<GitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueControlLedger {
    #[serde(default)]
    pub intents: BTreeMap<String, Intent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub digest: String,
    pub metadata: Metadata,
    pub graph_id: String,
    pub target_node: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub retry_generations: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gate_generations: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<HumanRequestId>,
    pub payload: Value,
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_id: Option<HumanAnswerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_revision: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Pending,
    Answered,
    DispositionAdmitted,
    DropAdmitted,
    Applied,
    Refused,
}

impl Phase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Applied | Self::Refused)
    }
}
