//! Strict run-store records. Facts are constructed only after store validation.
use crate::{CanonicalError, CanonicalV1, FileDigest, PathWire};
use provenant_core::{
    ActorId, AttemptId, DeliveryId, Digest, GitOid, OperationId, ProjectId, RunId, TaskId, WallTime,
};
use serde::{Deserialize, Serialize};

pub const RECORD_LIMIT: u64 = 1024 * 1024;
pub const RESULT_LIMIT: u64 = 4 * 1024 * 1024;
pub const CAPTURE_LIMIT: u64 = 64 * 1024 * 1024;
pub const OUTPUT_LIMIT: u64 = 65536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRequest {
    pub schema_version: u64,
    pub tasks: Vec<TaskRequest>,
    pub concurrency: u64,
    pub parent: Option<AttemptId>,
}
impl CanonicalRequest {
    pub fn digest(&self) -> Result<Digest, CanonicalError> {
        CanonicalV1::record_digest("run-request", 1, self)
    }
}
/// Effective dispatch policy, sealed with the request and copied to each attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessProfile {
    pub name: String,
    pub task_class: Option<String>,
    pub knobs: std::collections::BTreeMap<String, serde_json::Value>,
    pub overrides: Vec<String>,
}
impl HarnessProfile {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
    pub fn knob_u64(&self, name: &str) -> Option<u64> {
        self.knobs.get(name).and_then(serde_json::Value::as_u64)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRequest {
    #[serde(default, skip_serializing_if = "HarnessProfile::is_empty")]
    pub profile: HarnessProfile,
    pub task_id: TaskId,
    pub prompt_digest: FileDigest,
    pub route: RequestedRoute,
    /// Optional dispatch override; normal placement comes from routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub fallback: Vec<RouteRecord>,
    /// Digest of the verified confinement/authority plan supplied by the edge.
    pub authority_digest: Digest,
    #[serde(default)]
    pub writer: Option<WriterRequest>,
    pub provider_timeout_seconds: u64,
    pub delivery_id: Option<DeliveryId>,
}

/// Canonical repository and linked worktree selected at the submission edge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterRequest {
    pub project: std::path::PathBuf,
    pub worktree: std::path::PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestedRoute {
    Model {
        provider: String,
        model: String,
        effort: Option<String>,
    },
    Named {
        name: String,
        effort: Option<String>,
    },
}
impl RequestedRoute {
    pub fn projection(self) -> crate::RouteFact {
        match self {
            Self::Model {
                provider,
                model,
                effort,
            } => crate::RouteFact {
                provider: Some(provider),
                model: Some(model),
                effort,
                account_label: None,
            },
            Self::Named { effort, .. } => crate::RouteFact {
                provider: None,
                model: None,
                effort,
                account_label: None,
            },
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRecord {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account_label: Option<String>,
}
impl From<RouteRecord> for crate::RouteFact {
    fn from(r: RouteRecord) -> Self {
        Self {
            provider: r.provider,
            model: r.model,
            effort: r.effort,
            account_label: r.account_label,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRecord {
    pub uncached_input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub output_excluding_reasoning: Option<u64>,
    pub reasoning: Option<u64>,
    pub source: Option<String>,
    pub context_basis_points: Option<u64>,
}
impl From<UsageRecord> for crate::Usage {
    fn from(u: UsageRecord) -> Self {
        Self {
            uncached_input: u.uncached_input,
            cache_read: u.cache_read,
            cache_write: u.cache_write,
            output_excluding_reasoning: u.output_excluding_reasoning,
            reasoning: u.reasoning,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Closure {
    Open,
    Closing,
    Closed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub schema_version: u64,
    pub run_id: RunId,
    pub project_id: ProjectId,
    pub actor_id: ActorId,
    pub operation_id: OperationId,
    pub request_digest: Digest,
    pub task_ids: Vec<TaskId>,
    pub parent: Option<AttemptId>,
    pub children: Vec<RunId>,
    pub revision: u64,
    pub generation: u64,
    pub created_at: WallTime,
    pub closed_at: Option<WallTime>,
    pub closure: Closure,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Prepared,
    Accepted,
    Terminal,
    LaunchUnknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Dispatch,
    Resume,
    Cancel,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRecord {
    pub schema_version: u64,
    pub project_id: ProjectId,
    pub actor_id: ActorId,
    pub operation_id: OperationId,
    pub kind: OperationKind,
    pub request_digest: Digest,
    pub run_id: RunId,
    pub state: OperationState,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
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
impl From<Outcome> for crate::AttemptOutcome {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Succeeded => Self::Succeeded,
            Outcome::Failed => Self::Failed,
            Outcome::Rejected => Self::Rejected,
            Outcome::Cancelled => Self::Cancelled,
            Outcome::TimedOut => Self::TimedOut,
            Outcome::RateLimited => Self::RateLimited,
            Outcome::QuotaLimited => Self::QuotaLimited,
            Outcome::AuthRequired => Self::AuthRequired,
            Outcome::ModelUnavailable => Self::ModelUnavailable,
            Outcome::PermissionDenied => Self::PermissionDenied,
            Outcome::Quarantined => Self::Quarantined,
            Outcome::Lost => Self::Lost,
            Outcome::InputRequired => Self::InputRequired,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttemptState {
    Queued,
    Starting,
    Running,
    Settling {
        publication_pending: bool,
    },
    Terminal {
        outcome: Outcome,
        exit_code: Option<i64>,
        finished_at: WallTime,
        reason: Option<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    pub path: PathWire,
    pub digest: FileDigest,
    pub bytes: u64,
    pub complete: bool,
    pub redacted: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    #[serde(default, skip_serializing_if = "HarnessProfile::is_empty")]
    pub profile: HarnessProfile,
    pub schema_version: u64,
    pub attempt_id: AttemptId,
    pub run_id: RunId,
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub operation_id: OperationId,
    pub attempt_number: u64,
    pub revision: u64,
    pub generation: u64,
    pub delivery_id: Option<DeliveryId>,
    pub retry_of: Option<AttemptId>,
    pub requested_route: RequestedRoute,
    pub resolved_route: Option<RouteRecord>,
    pub applied_route: Option<RouteRecord>,
    pub observed_route: Option<RouteRecord>,
    pub authority_digest: Digest,
    /// Project-relative write exceptions actually granted before launch.
    #[serde(default)]
    pub read_only_writable: Vec<String>,
    pub subject: Option<GitOid>,
    pub head: Option<GitOid>,
    pub verified_owner: Option<ActorId>,
    pub session: Option<String>,
    pub queued_at: WallTime,
    pub started_at: Option<WallTime>,
    /// Custody evidence. None in older records must never authorise a writer retry.
    #[serde(default)]
    pub provider_launched: Option<bool>,
    pub provider_ms: Option<u64>,
    pub admission_deadline: Option<WallTime>,
    pub runtime_deadline: Option<WallTime>,
    pub state: AttemptState,
    pub usage: UsageRecord,
    pub prompt: ArtifactRecord,
    pub result: Option<ArtifactRecord>,
    pub publication_error: Option<String>,
    pub exit_evidence: Option<ArtifactRecord>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionTicket {
    pub schema_version: u64,
    pub operation_id: OperationId,
    pub attempt_id: AttemptId,
    pub project_id: ProjectId,
    pub sequence: u64,
    pub generation: u64,
    pub holder: Option<ActorId>,
    pub active: bool,
    /// Available memory floor as basis points of installed memory.
    pub memory_floor_basis_points: u64,
    pub disk_floor_bytes: u64,
    pub startup_spacing_ms: u64,
    /// The owning run's batch concurrency, distinct from host admission caps.
    pub run_concurrency: u64,
    pub quota_permit: Option<String>,
    pub admission_deadline: WallTime,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvent {
    pub schema_version: u64,
    pub sequence: u64,
    pub project_id: ProjectId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub revision: u64,
    pub change: AttemptState,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCursor {
    pub schema_version: u64,
    pub project_id: ProjectId,
    pub actor_id: ActorId,
    pub selection: String,
    pub sequence: u64,
}
