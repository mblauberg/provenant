use std::collections::BTreeMap;

use provenant_core::{
    ActorId, AttemptId, DeliveryId, GitOid, OperationId, ProjectId, TaskId, WallTime,
};
use serde::Serialize;

use crate::{FileDigest, PathWire};

/// Terminal attempt outcomes published to workflow, telemetry and the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
    TimedOut,
    RateLimited,
    QuotaLimited,
    AuthRequired,
    ModelUnavailable,
    PermissionDenied,
    Quarantined,
    Lost,
    InputRequired,
}

/// Route details resolved for an attempt. Unknown provider observations remain
/// absent rather than being filled from a requested route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RouteFact {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account_label: Option<String>,
}

/// Named usage counts stay unsigned and retain only values observed by a
/// trusted provider adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageCounts {
    pub counts: BTreeMap<String, u64>,
}

/// Verified reviewer attribution included when the run owner has evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewerFact {
    pub actor_id: ActorId,
    pub family: Option<String>,
    pub model: Option<String>,
    pub outcome: Option<String>,
}

/// A contained output reference and the digest of its exact published bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputRef {
    pub path: PathWire,
    pub digest: FileDigest,
}

/// Immutable Serialize-only projection of one completed attempt. The run owner
/// constructs it after validating the stored attempt; it has no Deserialize
/// implementation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttemptFact {
    pub task_id: TaskId,
    pub attempt_number: u64,
    pub operation_id: OperationId,
    pub project_id: ProjectId,
    pub terminal_state: AttemptOutcome,
    pub exit_code: Option<i64>,
    pub started_at: WallTime,
    pub finished_at: WallTime,
    pub resolved_route: Option<RouteFact>,
    pub usage: UsageCounts,
    pub output_paths: Vec<PathWire>,
    pub reviewer: Option<ReviewerFact>,
    pub delivery_id: Option<DeliveryId>,
    pub subject: Option<GitOid>,
    pub head: Option<GitOid>,
    pub output_refs: Vec<OutputRef>,
    pub requested_route: Option<RouteFact>,
    pub applied_route: Option<RouteFact>,
    pub observed_route: Option<RouteFact>,
    pub verified_owner: Option<ActorId>,
    pub lineage: Vec<AttemptId>,
    pub reason: Option<String>,
}
