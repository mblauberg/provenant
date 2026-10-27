use provenant_core::{
    ActorId, AttemptId, DeliveryId, GitOid, OperationId, ProjectId, TaskId, WallTime,
};
use serde::Serialize;

use crate::{FileDigest, PathWire, Trace};

/// Terminal attempt outcomes published to workflow, telemetry and the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    ReviewRequired,
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
    Uncommitted,
    InputRequired,
}

/// Route details resolved for an attempt. Unknown provider observations remain
/// absent rather than being filled from a requested route.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RouteFact {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account_label: Option<String>,
}

/// Disjoint provider usage counts. Unknown categories remain absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub uncached_input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    /// Output tokens excluding reasoning.
    pub output_excluding_reasoning: Option<u64>,
    pub reasoning: Option<u64>,
}

/// Evidence supporting reviewer attribution, as required by workflow policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerProvenance {
    Observed,
    ChairAttested,
    Unknown,
}

/// Verified reviewer attribution included when the run owner has evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewerFact {
    pub provenance: ReviewerProvenance,
    pub actor_id: ActorId,
    pub family: Option<String>,
    pub model: Option<String>,
}

/// A contained output reference and the digest of its exact published bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputRef {
    pub path: PathWire,
    pub digest: FileDigest,
}

/// Pointer to the provider's own transcript of an attempt. The transcript
/// stays with the provider; only its identity is recorded here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TranscriptRef {
    pub adapter: String,
    pub session: String,
}

/// Immutable Serialize-only projection of one completed attempt. The run owner
/// constructs it after validating the stored attempt; it has no Deserialize
/// implementation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttemptFact {
    pub attempt_id: AttemptId,
    pub task_id: TaskId,
    pub attempt_number: u64,
    pub operation_id: OperationId,
    pub project_id: ProjectId,
    pub terminal_state: AttemptOutcome,
    pub exit_code: Option<i64>,
    pub queued_at: WallTime,
    pub started_at: Option<WallTime>,
    pub provider_ms: Option<u64>,
    pub finished_at: WallTime,
    pub resolved_route: Option<RouteFact>,
    pub usage: Usage,
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
    pub trace: Trace,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptRef>,
}
