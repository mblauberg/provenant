//! Strict run-store records. Facts are constructed only after store validation.
use crate::{CanonicalError, CanonicalV1, FileDigest, PathWire};
use provenant_core::{
    ActorId, AttemptId, DeliveryId, Digest, GitOid, OperationId, ProjectId, RunId, TaskId, WallTime,
};
use serde::{Deserialize, Serialize};

pub const RECORD_LIMIT: u64 = 1024 * 1024;
/// Encoded write grants an attempt may record, leaving terminal headroom.
pub const MAX_WRITE_GRANT_BYTES: u64 = 256 * 1024;
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
    /// Depth assigned to this run when it was dispatched from a lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_depth: Option<u64>,
    /// Attempt whose provider submitted this nested run, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_attempt: Option<AttemptId>,
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
    pub knobs: std::collections::BTreeMap<String, serde_json::Value>,
    pub overrides: Vec<String>,
    // Preserve the removed field only to verify sealed request and attempt digests.
    #[doc(hidden)]
    #[serde(
        rename = "task_class",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_legacy_task_class"
    )]
    pub legacy_task_class: Option<Option<String>>,
}
fn deserialize_legacy_task_class<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
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
    /// Git base for host-written attempt context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The caller's Git ref, retained for idempotent operation identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    pub route: RequestedRoute,
    /// Optional dispatch override; normal placement comes from routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub fallback: Vec<RouteRecord>,
    /// Digest of the verified confinement/authority plan supplied by the edge.
    pub authority_digest: Digest,
    #[serde(default)]
    pub writer: Option<WriterRequest>,
    /// Explicit authority for host delivery, separate from the writer sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver: Option<WriterDeliveryRequest>,
    pub provider_timeout_seconds: u64,
    pub delivery_id: Option<DeliveryId>,
    /// Dispatch-time access beyond the mode's defaults.
    #[serde(default, skip_serializing_if = "LaneAccess::is_default")]
    pub access: LaneAccess,
}

/// Network reach for a lane's provider and the tools it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    /// Only the provider's own recorded API endpoints.
    Provider,
    /// Provider endpoints plus localhost services: TCP and Postgres sockets.
    Local,
    /// Any outbound host plus localhost services.
    Open,
}

/// Access a dispatch adds to its mode's defaults, sealed in the authority
/// digest. Paths are canonical directories checked at the submission edge.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneAccess {
    /// Explicit network reach; absent takes the mode default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkAccess>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<std::path::PathBuf>,
}
impl LaneAccess {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    /// Writers build and test, so they default to open network; read-only
    /// lanes reach only their provider.
    pub fn network(&self, writer: bool) -> NetworkAccess {
        self.network.unwrap_or(if writer {
            NetworkAccess::Open
        } else {
            NetworkAccess::Provider
        })
    }
}

/// Canonical repository and linked worktree selected at the submission edge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterRequest {
    pub project: std::path::PathBuf,
    pub worktree: std::path::PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterDeliveryRequest {
    /// A configured remote name, never a URL supplied by a provider.
    pub remote: String,
    /// Short destination branch name; the host pushes an explicit full refspec.
    pub branch: String,
    #[serde(default)]
    pub open_pr: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterDeliveryStage {
    Prepare,
    Head,
    Push,
    Fetch,
    Upstream,
    Verify,
    PullRequest,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriterDeliveryState {
    Delivered {
        pull_request: Option<String>,
    },
    /// Delivery was requested but the lane head equals the head at launch, so
    /// nothing was pushed. The lane is not failed; the chair sees no commit.
    NoNewCommits,
    /// Delivery was requested but no host delivery ran for this attempt (for
    /// example a recovered terminal), so nothing was pushed and the chair must
    /// deliver the head by hand.
    NotAttempted,
    /// The writer succeeded, but host delivery needs chair pickup. No retry.
    Failed {
        stage: WriterDeliveryStage,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterDeliveryReceipt {
    pub schema_version: u64,
    pub remote: String,
    pub branch: String,
    pub head: Option<GitOid>,
    pub state: WriterDeliveryState,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_selection: Option<crate::RouteSelectionReceipt>,
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
    /// Continuation target, independently of nested dispatch metadata.
    pub parent: Option<AttemptId>,
    /// Attempt that submitted this nested run; grants no cancellation authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_attempt: Option<AttemptId>,
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
/// Why a fallback candidate did not launch. Typed so `lanes` and `output` can
/// show a skipped leg without reading prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipKind {
    /// Readiness, adapter, policy or executable unavailable for the route.
    Unavailable,
    /// The account was unregistered, unreadable or unavailable to probe.
    Account,
    /// The account needs sign-in or its credential refresh failed.
    AuthRequired,
    /// Per-candidate state or setup could not be prepared.
    Setup,
    /// Confinement, continuation or provider preparation refused the candidate.
    Refused,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedRoute {
    /// `adapter/model`, or the adapter alone for an account-level skip.
    pub route: String,
    pub kind: SkipKind,
    pub detail: String,
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
pub struct WriteGrantRecord {
    pub source: WriteSource,
    pub path: crate::AbsolutePath,
    pub scope: PathScope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathScope {
    Exact,
    Subtree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteSource {
    WriteRoot,
    ToolWrite,
    StateWrite,
    CredentialWrite,
    Scratch,
    PrivateState,
    ReadOnlyException,
    LaneCache,
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
    /// Explicit write allowances from the plan handed to confinement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_grants: Vec<WriteGrantRecord>,
    /// Writers only; concrete Git paths stay out of the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_grants_digest: Option<Digest>,
    pub subject: Option<GitOid>,
    pub head: Option<GitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<WriterDeliveryReceipt>,
    pub verified_owner: Option<ActorId>,
    pub session: Option<String>,
    pub queued_at: WallTime,
    pub started_at: Option<WallTime>,
    /// Most recent supervisor-observed stdout/stderr activity while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_at: Option<WallTime>,
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
    /// Fallback candidates that did not launch before this attempt did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_routes: Vec<SkippedRoute>,
    /// Stamped when the attempt is created; a record without one decodes as
    /// all unknown.
    #[serde(default)]
    pub trace: crate::Trace,
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
    #[serde(default)]
    pub trace: crate::Trace,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_without_lane_metadata_retains_its_sealed_digest()
    -> Result<(), Box<dyn std::error::Error>> {
        let original = serde_json::json!({
            "schema_version": 1, "tasks": [], "concurrency": 1, "parent": null
        });
        let digest = CanonicalV1::record_digest("run-request", 1, &original)?;
        let request: CanonicalRequest = serde_json::from_value(original.clone())?;
        assert_eq!(request.digest()?, digest);
        assert_eq!(serde_json::to_value(request)?, original);
        Ok(())
    }

    #[test]
    fn request_with_a_removed_task_class_retains_its_sealed_digest()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = CanonicalRequest {
            schema_version: 1,
            tasks: vec![TaskRequest {
                profile: HarnessProfile {
                    name: "rigorous".into(),
                    ..HarnessProfile::default()
                },
                task_id: TaskId::from_bytes([3; 16]),
                prompt_digest: CanonicalV1::file_digest(b"prompt"),
                base: None,
                base_ref: None,
                route: RequestedRoute::Named {
                    name: "senior".into(),
                    effort: None,
                },
                account: None,
                fallback: Vec::new(),
                authority_digest: Digest::from_bytes([5; 32]),
                writer: None,
                deliver: None,
                provider_timeout_seconds: 60,
                delivery_id: None,
                access: LaneAccess::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: None,
            parent_attempt: None,
        };
        let mut sealed = serde_json::to_value(&request)?;
        for task_class in [serde_json::Value::Null, serde_json::json!("mechanical")] {
            sealed["tasks"][0]["profile"]["task_class"] = task_class;
            let digest = CanonicalV1::record_digest("run-request", 1, &sealed)?;
            let parsed: CanonicalRequest = serde_json::from_value(sealed.clone())?;
            assert_eq!(parsed.digest()?, digest);
            assert_eq!(serde_json::to_value(parsed)?, sealed);
        }
        Ok(())
    }

    #[test]
    fn new_profile_omits_task_class() -> Result<(), Box<dyn std::error::Error>> {
        let profile = HarnessProfile {
            name: "rigorous".into(),
            ..HarnessProfile::default()
        };
        let sealed = serde_json::to_value(&profile)?;
        assert!(sealed.get("task_class").is_none());
        let parsed: HarnessProfile = serde_json::from_value(sealed.clone())?;
        assert_eq!(serde_json::to_value(parsed)?, sealed);
        Ok(())
    }
}
