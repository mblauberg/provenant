//! Strict run-store records. Facts are constructed only after store validation.
use crate::{CanonicalError, CanonicalV1, FileDigest, PathWire};
use provenant_core::{
    ActorId, AttemptId, DeliveryId, Digest, GitOid, HostId, OperationId, ProjectId, RunId, TaskId,
    WallTime,
};
use serde::{Deserialize, Serialize};

/// Compiled scheduling defaults, also used when configured priority is unavailable.
pub const DEFAULT_WRITER_PRIORITY: u8 = 10;
pub const DEFAULT_READER_PRIORITY: u8 = 5;
pub const DEFAULT_BUILD_HEAVY_PRIORITY: u8 = 15;

pub fn default_lane_priority(writer: bool, build_heavy: bool) -> u8 {
    if build_heavy {
        DEFAULT_BUILD_HEAVY_PRIORITY
    } else if writer {
        DEFAULT_WRITER_PRIORITY
    } else {
        DEFAULT_READER_PRIORITY
    }
}

pub const RECORD_LIMIT: u64 = 1024 * 1024;
/// Default warning threshold for the encoded applied write-grant ledger.
pub const MAX_WRITE_GRANT_BYTES: u64 = 256 * 1024;
/// Maximum configurable warning threshold, independent of record capacity.
pub const MAX_WRITE_GRANT_BYTES_CEILING: u64 = 8 * RECORD_LIMIT;
/// Host-owned attempt and terminal decisions retain the complete diagnostic
/// ledger. Admission thresholds must not become storage refusals. This does
/// not change bounded request, result, capture or provider-input readers.
pub const TERMINAL_RECORD_LIMIT: u64 = u64::MAX;
pub const RESULT_LIMIT: u64 = 4 * 1024 * 1024;
pub const CAPTURE_LIMIT: u64 = 64 * 1024 * 1024;
pub const OUTPUT_LIMIT: u64 = 65536;

/// Explicit submitting chair identity, separate from a worker provider session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChairBinding {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub host: HostId,
    pub actor: ActorId,
    /// The chair's host session, when its harness names one. It addresses the
    /// chair log that settlement appends to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalRequest {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Host-verifiable child run and attempt IDs. Absent on legacy, unindexed runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_attempt_id_version: Option<u64>,
    /// Submission proposal only. Readers trust the host-owned Fabric index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chair: Option<ChairBinding>,
    /// A fresh continuation retains the parent for lineage and admission, but
    /// starts the provider without its previous session. The reason is sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_continuation: Option<String>,
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

    /// The host-derived run ID for this request under `operation`, or `None`
    /// for a legacy, unversioned request. Version 1 binds the ID to the whole
    /// proposal and the host's namespace, so no proposed ID is ever adopted.
    pub fn child_run_id(
        &self,
        project: ProjectId,
        operation: &OperationId,
    ) -> Result<Option<RunId>, ChildIdError> {
        match self.child_attempt_id_version {
            None => Ok(None),
            Some(1) => {
                let parent = self.parent_attempt.ok_or(ChildIdError::MissingParent)?;
                use sha2::{Digest as _, Sha256};
                let mut hash = Sha256::new();
                hash.update(b"provenant-child-run-v1\0");
                hash.update(project.as_bytes());
                hash.update(parent.as_bytes());
                hash.update(CanonicalV1::file_digest(operation.as_str().as_bytes()).bytes());
                hash.update(self.digest()?.as_bytes());
                let digest = hash.finalize();
                let mut id = [0; 16];
                id.copy_from_slice(&digest[..16]);
                Ok(Some(RunId::from_bytes(id)))
            }
            Some(_) => Err(ChildIdError::UnsupportedVersion),
        }
    }
}
/// Why a request cannot yield a host-derived child run ID.
#[derive(Debug, thiserror::Error)]
pub enum ChildIdError {
    #[error("unsupported child attempt ID version")]
    UnsupportedVersion,
    #[error("derived child IDs require a parent attempt")]
    MissingParent,
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}
/// Effective dispatch policy, sealed with the request and copied to each attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessProfile {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub name: String,
    pub knobs: std::collections::BTreeMap<String, serde_json::Value>,
    pub overrides: Vec<String>,
    /// Require the adapter to expose its own skill location.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_skill_location: bool,
    /// Remaining settlement retries sealed for a graph task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_retry_limit: Option<u8>,
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
/// Work labels and host-stamped graph provenance. Labels are at most 256 UTF-8 bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkContext {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Stamped by trusted graph launch admission, never inferred from labels.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub graph_template_launch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_arm: Option<String>,
    /// The candidate assigned while the control executed under shadow mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_source: Option<String>,
    /// Requested capability tier (for example `senior`) of a graph node's attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Template the attempt's graph node was compiled from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Node that admitted the attempt's node through a patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_node: Option<String>,
    /// Host-verified graph writer predecessor, checked again under launch custody.
    /// Graph retry lineage remains independent of run-policy `retry_of`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_predecessor: Option<AttemptId>,
}
impl WorkContext {
    /// A new request names only the fixed fields; stored records keep unknown ones for forward compatibility.
    pub fn accepted(&self) -> bool {
        self.extra.is_empty()
            && self.valid()
            && (!self.graph_template_launch || (self.graph_id.is_some() && self.node_id.is_some()))
    }
    pub fn valid(&self) -> bool {
        [
            &self.work_item,
            &self.risk_tier,
            &self.task_kind,
            &self.graph_id,
            &self.node_id,
            &self.preset,
            &self.pattern,
            &self.role,
            &self.role_digest,
            &self.experiment_arm,
            &self.shadow_of,
            &self.selection_source,
            &self.tier,
            &self.template,
            &self.parent_node,
        ]
        .into_iter()
        .flatten()
        .all(|s| !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRequest {
    /// Cloud custody is sealed with the request before an attempt can launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<CloudPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkContext>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Select the build-heavy scheduling priority for this task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub build_heavy: bool,
    /// Human-supplied task title, when provided at submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "HarnessProfile::is_empty")]
    pub profile: HarnessProfile,
    pub task_id: TaskId,
    pub prompt_digest: FileDigest,
    /// Chair-verified review head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Original pinned source commit when a continuation advances head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_head: Option<String>,
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

/// Provider-independent cloud target snapshot used for restart recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudPlacement {
    pub target: String,
    pub provider: String,
    #[serde(default)]
    pub environment: String,
    pub repository: String,
    /// Base branch the job starts from and may merge.
    pub branch: String,
    pub model: String,
    pub effort: Option<String>,
    pub deadline_seconds: u64,
    /// Named branch a writer job commits to and pushes without force. Dispatch
    /// fills it from the delivery branch or the writer's branch; a platform
    /// that restricts names may push elsewhere, and the result says where.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_branch: Option<String>,
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
pub struct LaneAccess {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Explicit network reach; absent defaults to open network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkAccess>,
    /// Explicit PostgreSQL capability; never inferred from network or write access.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub database_ipc: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<std::path::PathBuf>,
    /// Canonical image files explicitly supplied to this lane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<std::path::PathBuf>,
    /// A graph node's materialised upstream results, read-only. The host
    /// grants it from its own graph-inputs directory under the state root,
    /// where a `--read-root` may not reach; any other path is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_root: Option<std::path::PathBuf>,
    /// Host-verified graph source checkout for a read-only launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<std::path::PathBuf>,
    /// Use the verified source checkout directly, without a writer lease.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_live: bool,
    /// Filesystem identity of the bound source directory at admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_identity: Option<SourceIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub device: u64,
    pub inode: u64,
}
impl LaneAccess {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    /// Every lane defaults to open network; explicit flags may restrict it.
    pub fn network(&self, _writer: bool) -> NetworkAccess {
        self.network.unwrap_or(NetworkAccess::Open)
    }
}

/// Whether a lane discovers the host's user skills read-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserSkillsMode {
    #[default]
    Discover,
    Off,
}

impl UserSkillsMode {
    pub fn is_discover(&self) -> bool {
        matches!(self, Self::Discover)
    }
}

/// Canonical repository and linked worktree selected at the submission edge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterRequest {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub project: std::path::PathBuf,
    pub worktree: std::path::PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterDeliveryRequest {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Push is the legacy behaviour; integration never checks out either head.
    #[serde(default, skip_serializing_if = "DeliveryMode::is_push")]
    pub mode: DeliveryMode,
    /// A configured remote name, never a URL supplied by a provider.
    pub remote: String,
    /// Short destination branch name; the host pushes an explicit full refspec.
    pub branch: String,
    #[serde(default)]
    pub open_pr: bool,
    /// An integration may only advance its target to the delivered head.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast_forward_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum DeliveryMode {
    None,
    #[default]
    Push,
    Integrate(String),
    Pr,
}
impl DeliveryMode {
    pub fn is_push(&self) -> bool {
        matches!(self, Self::Push)
    }
}
impl std::str::FromStr for DeliveryMode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "push" => Ok(Self::Push),
            "pr" => Ok(Self::Pr),
            value => value
                .strip_prefix("integrate:")
                .filter(|branch| {
                    !branch.is_empty()
                        && !branch.starts_with('-')
                        && branch.len() <= 240
                        && !branch
                            .contains([' ', '\n', '\r', '\0', ':', '~', '^', '?', '*', '[', '\\'])
                        && !branch.contains("..")
                        && !branch.contains("@{")
                        && !branch.ends_with(['/', '.'])
                })
                .map(|branch| Self::Integrate(branch.to_owned()))
                .ok_or_else(|| "expected none, push, integrate:<branch> or pr".into()),
        }
    }
}
impl TryFrom<String> for DeliveryMode {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<DeliveryMode> for String {
    fn from(mode: DeliveryMode) -> Self {
        match mode {
            DeliveryMode::None => "none".into(),
            DeliveryMode::Push => "push".into(),
            DeliveryMode::Pr => "pr".into(),
            DeliveryMode::Integrate(branch) => format!("integrate:{branch}"),
        }
    }
}
impl std::fmt::Display for DeliveryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from(self.clone()))
    }
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
    Integrate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WriterDeliveryState {
    Delivered {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        pull_request: Option<String>,
    },
    /// Delivery was requested but the lane head equals the head at launch, so
    /// nothing was pushed. The lane is not failed; the chair sees no commit.
    NoNewCommits {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// Delivery was requested but no host delivery ran for this attempt (for
    /// example a recovered terminal), so nothing was pushed and the chair must
    /// deliver the head by hand.
    NotAttempted {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// The writer succeeded, but host delivery needs chair pickup. No retry.
    Failed {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        stage: WriterDeliveryStage,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterReviewAcceptance {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub actor: ActorId,
    pub protected_paths: Vec<PathWire>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterDeliveryReceipt {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub schema_version: u64,
    pub remote: String,
    pub branch: String,
    pub head: Option<GitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_acceptance: Option<WriterReviewAcceptance>,
    pub state: WriterDeliveryState,
    /// The destination ref's tips around a delivered push or integration.
    /// Absent on receipts that did not publish, and on older receipts, whose
    /// destinations are then unobserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<WriterIntegration>,
}

/// The ref delivery changed, read back from the endpoint the host bound at
/// launch. `previous` is the tip delivery leased or found; it is absent when
/// delivery created the ref. `integrated` is the tip read back after the
/// update: the delivered head, or the merge commit integration made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterIntegration {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// The bound remote name, or empty for a ref in the local repository.
    pub remote: String,
    /// The bound push URL; absent for a local ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_url: Option<String>,
    /// Full destination ref, such as `refs/heads/main`.
    pub target: String,
    pub previous: Option<GitOid>,
    pub integrated: GitOid,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestedRoute {
    /// A named configuration entry executed by a model-less adapter.
    Adapter {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        provider: String,
        name: String,
    },
    Model {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        provider: String,
        model: String,
        effort: Option<String>,
    },
    Named {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        name: String,
        effort: Option<String>,
    },
    /// Pool selection with families already used by this review group.
    Review {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        name: String,
        effort: Option<String>,
        excluded_families: Vec<String>,
        distinct_families: bool,
        /// Additional families to avoid when possible, without weakening required exclusions.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preferred_families: Vec<String>,
        /// Refuse a route inside an excluded family instead of substituting one.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        require_distinct: bool,
    },
}
impl RequestedRoute {
    pub fn projection(self) -> crate::RouteFact {
        match self {
            Self::Adapter { provider, .. } => crate::RouteFact {
                provider: Some(provider),
                model: None,
                effort: None,
                account_label: None,
            },
            Self::Model {
                provider,
                model,
                effort,
                ..
            } => crate::RouteFact {
                provider: Some(provider),
                model: Some(model),
                effort,
                account_label: None,
            },
            Self::Named { effort, .. } | Self::Review { effort, .. } => crate::RouteFact {
                provider: None,
                model: None,
                effort,
                account_label: None,
            },
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRecord {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
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
impl RouteRecord {
    /// Copy the resolved routing decision into the launch record.
    pub fn from_resolved(
        route: &crate::ResolvedRoute,
        account_label: Option<&str>,
        source: &str,
    ) -> Self {
        Self {
            provider: Some(route.adapter.clone()),
            model: Some(format!("{}/{}", route.adapter, route.model)),
            effort: route.effort.clone(),
            account_label: account_label.map(str::to_owned),
            account_key: route.account_key.clone(),
            pool_selection: route.pool_selection.clone(),
            source: Some(source.into()),
            ..Default::default()
        }
    }
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
pub struct UsageRecord {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub uncached_input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    /// The long-TTL part of `cache_write`, where the provider splits writes.
    #[serde(default)]
    pub cache_write_1h: Option<u64>,
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
            cache_write_1h: u.cache_write_1h,
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
pub struct RunRecord {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
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
    /// Dispatch was refused before a run was admitted.
    Refused,
    LaunchUnknown,
    /// Launch preparation failed and custody could not be released.
    Blocked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Dispatch,
    Resume,
    Cancel,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecord {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub schema_version: u64,
    pub project_id: ProjectId,
    pub actor_id: ActorId,
    pub operation_id: OperationId,
    pub kind: OperationKind,
    pub request_digest: Digest,
    pub run_id: RunId,
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// Safe diagnostic retained for a pre-admission refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<crate::diagnostic::Diagnostic>,
    /// Worktree identifier, not an absolute host path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_at: Option<WallTime>,
    /// Chair bound at submission, retained for refusal notice recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_chair: Option<ChairBinding>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
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
    /// Terminal, but the evidence is insufficient to establish another outcome.
    Unsettled,
    /// A writer succeeded but left uncommitted edits and no new commit.
    Uncommitted,
    InputRequired,
}
impl Outcome {
    /// A safe status reason derived only from the typed terminal outcome.
    pub fn diagnostic_reason(self) -> &'static str {
        use crate::diagnostic::ProviderFailureFact;
        match self {
            Self::Succeeded => "the lane completed; inspect its result",
            Self::ReviewRequired => "the lane needs review; inspect its result and evidence",
            Self::Failed => "the lane failed; inspect its result and local evidence",
            Self::Rejected => "the lane was refused; inspect the request and access policy",
            Self::Cancelled => {
                "the lane was cancelled; inspect its status before taking further action"
            }
            Self::TimedOut => {
                "the lane timed out; inspect its result and runtime limit before resuming"
            }
            Self::RateLimited => ProviderFailureFact::RateLimit.reason(),
            Self::QuotaLimited => ProviderFailureFact::Quota { free_pool: false }.reason(),
            Self::AuthRequired => ProviderFailureFact::AuthRequired.reason(),
            Self::ModelUnavailable => {
                "the model is unavailable; check the selected model or wait for capacity"
            }
            Self::PermissionDenied => {
                "access was denied; check the declared grants and filesystem permissions"
            }
            Self::Quarantined => {
                "the lane is quarantined; inspect its evidence before further action"
            }
            Self::Lost | Self::Unsettled => {
                "the lane outcome is uncertain; inspect its status before retrying"
            }
            Self::Uncommitted => {
                "the writer has uncommitted changes; preserve or commit the edits before retrying"
            }
            Self::InputRequired => {
                "the lane needs input; inspect its request and provide the missing input"
            }
        }
    }
}

impl From<Outcome> for crate::AttemptOutcome {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Succeeded => Self::Succeeded,
            Outcome::ReviewRequired => Self::ReviewRequired,
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
            Outcome::Unsettled => Self::Unsettled,
            Outcome::Uncommitted => Self::Uncommitted,
            Outcome::InputRequired => Self::InputRequired,
        }
    }
}
/// Safe cause of a terminal decision, independent of provider text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCause {
    PreparationRefused,
    /// The pinned source view of an isolated lane could not be built.
    SourceViewRefused,
    AccountLoginRequired,
    RouteResolutionRefused,
    AccessPolicyRefused,
    DiskFloor,
    DiskObservationUnavailable,
    StoredPromptUnreadable,
    WriteGrantsRefused,
    WorktreeBusy,
    HandoffRefused,
    RetainedWriterImport,
    UnsupportedPlatform,
    UnknownPacingAccount,
    UnregisteredAccount,
    DeliveryPrepareCredentialHelper,
    SpawnFailure,
    ProcessCrash,
    TokenBudget,
    SettlementFailure,
    ProviderEarlyStop,
    FreePoolQuota,
    CommandUnavailable,
    HostNativeUnconfigured,
    IdentityUnobserved,
    /// A writer declared unfinished work and its pinned head equals its base.
    StoppedNoWork,
    /// A writer finished without a final message.
    EmptyWriterResult,
    /// The host stopped a writer at its context handoff threshold so a fresh
    /// attempt continues from its handoff on the same worktree.
    ContextHandoff,
    /// A lane made more GitHub requests than its budget.
    GithubRequestBudget,
}

impl TerminalCause {
    /// Bounded label for human status rows. Structured results use `detail`.
    pub fn reason(self) -> &'static str {
        match self {
            Self::PreparationRefused => "preparation refused; provenant output ID",
            Self::SourceViewRefused => "source view refused; provenant output ID",
            Self::AccountLoginRequired => "login required; provenant output ID",
            Self::RouteResolutionRefused => "route refused; provenant output ID",
            Self::AccessPolicyRefused => "access policy refused; provenant output ID",
            Self::DiskFloor => "disk below gc.free_floor; provenant output ID",
            Self::DiskObservationUnavailable => "disk probe unavailable; provenant output ID",
            Self::StoredPromptUnreadable => "stored prompt unreadable; provenant output ID",
            Self::WriteGrantsRefused => "write grants refused; provenant output ID",
            Self::WorktreeBusy => "worktree busy; provenant output ID",
            Self::HandoffRefused => "launch handoff refused; provenant output ID",
            Self::RetainedWriterImport => "writer import retained; provenant output ID",
            Self::UnsupportedPlatform => "sandbox unavailable; provenant output ID",
            Self::UnknownPacingAccount => "pacing account unknown; provenant output ID",
            Self::UnregisteredAccount => "account unregistered; provenant output ID",
            Self::DeliveryPrepareCredentialHelper => {
                "Prepare (credential helper) failed; provenant output ID"
            }
            Self::SpawnFailure => "provider spawn failed; provenant output ID",
            Self::ProcessCrash => "provider crashed; provenant output ID",
            Self::TokenBudget => "token budget exhausted; provenant output ID",
            Self::SettlementFailure => "settlement failed; provenant output ID",
            Self::ProviderEarlyStop => "provider stopped early; provenant output ID",
            Self::FreePoolQuota => "free-pool quota exhausted; provenant output ID",
            Self::CommandUnavailable => "command unavailable; provenant output ID",
            Self::HostNativeUnconfigured => "host-native unconfigured; provenant output ID",
            Self::IdentityUnobserved => "provider identity unobserved; provenant output ID",
            Self::StoppedNoWork => "stopped with no work (no new commit); provenant output ID",
            Self::EmptyWriterResult => "empty final message; review writer commit",
            Self::ContextHandoff => "context handoff threshold; provenant output ID",
            Self::GithubRequestBudget => "github request budget exhausted; provenant output ID",
        }
    }

    /// Full safe cause and remedy for structured results and error responses.
    pub fn detail(self) -> &'static str {
        match self {
            Self::AccountLoginRequired => {
                crate::diagnostic::ProviderFailureFact::AuthRequired.reason()
            }
            Self::PreparationRefused => {
                "local launch preparation refused the lane; inspect route and account configuration and local launch evidence before retrying"
            }
            Self::SourceViewRefused => {
                "the pinned source view of the lane could not be built; the step and cause follow. Retry, or raise domains.run.source_view_seconds, source_view_entries or source_view_bytes when the cause names a limit"
            }
            Self::RouteResolutionRefused => {
                "route resolution refused the lane; check the model, effort, exclusions and availability with `provenant route resolve` and `provenant quota tick`"
            }
            Self::DiskFloor => {
                "the host writer disk precheck refused launch below gc.free_floor; free disk space or run `provenant gc --apply` before retrying"
            }
            Self::DiskObservationUnavailable => {
                "the host writer disk precheck could not observe free space; restore the host disk observation before retrying"
            }
            Self::AccessPolicyRefused => {
                "launch access policy refused the lane; correct the request and declared access grants before retrying"
            }
            Self::StoredPromptUnreadable => {
                "launch preparation could not read the stored prompt; inspect the run's local files and resubmit the request"
            }
            Self::WriteGrantsRefused => {
                "launch preparation refused the write grants; check grant paths and domains.run.write_grant_limit_bytes before retrying"
            }
            Self::WorktreeBusy => {
                "the worktree is busy; wait for its writer to finish or select another worktree"
            }
            Self::HandoffRefused => {
                "the host refused the launch handoff; inspect local launch evidence and host configuration before retrying"
            }
            Self::RetainedWriterImport => {
                "a retained writer import blocks launch; reconcile the earlier writer's import before retrying"
            }
            Self::UnsupportedPlatform => {
                "sandbox backend is unavailable: UnsupportedPlatform; select a host with a supported sandbox backend before retrying"
            }
            Self::UnknownPacingAccount => {
                "pacing configuration references an unregistered account; correct the pacing profile"
            }
            Self::UnregisteredAccount => {
                "the selected account is not registered for the requested route; run `provenant accounts list`"
            }
            Self::DeliveryPrepareCredentialHelper => {
                "host writer delivery failed at Prepare (credential helper); check the host credential helper configuration"
            }
            Self::SpawnFailure => "the provider could not start; inspect local launch evidence",
            Self::ProcessCrash => {
                "the provider process crashed; inspect local evidence before retrying"
            }
            Self::TokenBudget => {
                "the host stopped the provider at its token budget; inspect the retained work before retrying"
            }
            Self::SettlementFailure => "lane settlement failed; inspect local recovery evidence",
            Self::ProviderEarlyStop => {
                "provider stopped early; inspect the result and resume if needed"
            }
            Self::FreePoolQuota => {
                crate::diagnostic::ProviderFailureFact::Quota { free_pool: true }.reason()
            }
            Self::CommandUnavailable => {
                "the configured command could not be resolved; correct its argv in domains.fabric.commands"
            }
            Self::HostNativeUnconfigured => {
                "the requested host-native route is not configured in domains.fabric.host_native; select a configured route"
            }
            Self::IdentityUnobserved => {
                "provider identity was not observed; inspect the local session evidence"
            }
            Self::StoppedNoWork => {
                "the writer stopped with no work: its branch has no new commit; acceptance remains unmet"
            }
            Self::EmptyWriterResult => {
                "the provider ended with an empty final message; its commit is kept for review"
            }
            Self::ContextHandoff => {
                "the host stopped the writer at lanes.context_handoff_tokens; a fresh attempt continues from its handoff on the same worktree"
            }
            Self::GithubRequestBudget => {
                "the host stopped the lane at its github request budget; inspect the retained work before retrying"
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AttemptState {
    Queued {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    Starting {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    Running {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    Settling {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        publication_pending: bool,
    },
    Terminal {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        outcome: Outcome,
        exit_code: Option<i64>,
        finished_at: WallTime,
        reason: Option<String>,
    },
}
/// A typed refusal made during local provider preparation, before custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationFailure {
    AuthRequired,
    UsageLimited,
    AdapterUnavailable,
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
impl SkipKind {
    /// Safe, actionable reason when a skipped route has no more specific fact.
    pub fn diagnostic(self) -> crate::diagnostic::Diagnostic {
        use crate::diagnostic::Diagnostic;
        let (detail, next) = match self {
            Self::Unavailable => (
                "the selected route is unavailable",
                "check route readiness with `provenant quota tick`",
            ),
            Self::Account => (
                "the selected account could not be observed",
                "check the account and run `provenant quota tick`",
            ),
            Self::AuthRequired => (
                "the selected account requires login",
                crate::diagnostic::ProviderFailureFact::AuthRequired.reason(),
            ),
            Self::Setup => (
                "the selected route could not be prepared",
                "inspect local setup and retry after correcting it",
            ),
            Self::Refused => (
                "the selected route was refused during preparation",
                "inspect the request and access policy before retrying",
            ),
        };
        Diagnostic::new("REFUSED")
            .category("refused")
            .detail(detail)
            .next(next)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedRoute {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// `adapter/model`, or the adapter alone for an account-level skip.
    pub route: String,
    pub kind: SkipKind,
    /// Typed readiness cause, when the router supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<crate::RouteExclusionReason>,
    pub detail: String,
}
impl SkippedRoute {
    /// Attach a typed cause and persist only its bounded safe projection.
    pub fn set_cause(&mut self, cause: crate::RouteExclusionReason) {
        self.cause = Some(cause);
        let diagnostic = self.diagnostic();
        self.detail = format!(
            "{}; {}",
            diagnostic.detail.unwrap_or_default(),
            diagnostic.next.unwrap_or_default()
        );
    }

    /// Preserve an individual candidate's safe guard facts across fallback.
    pub fn set_guard_facts(
        &mut self,
        boundary: Option<crate::diagnostic::BoundaryClass>,
        resource: Option<crate::diagnostic::ResourceFact>,
    ) -> Result<(), serde_json::Error> {
        if let Some(boundary) = boundary {
            self.extra
                .insert("guard_boundary".into(), serde_json::to_value(boundary)?);
        }
        if let Some(resource) = resource {
            self.extra
                .insert("guard_resource".into(), serde_json::to_value(resource)?);
        }
        Ok(())
    }

    /// Project typed facts only; `detail` may contain provider or OS text.
    pub fn diagnostic(&self) -> crate::diagnostic::Diagnostic {
        let mut diagnostic = self
            .cause
            .as_ref()
            .map(|cause| cause.diagnostic("REFUSED"))
            .unwrap_or_else(|| self.kind.diagnostic());
        if self.cause == Some(crate::RouteExclusionReason::ConfinementUnavailable)
            && let Some(guard) = self.extra.get("confinement_guard").and_then(|value| {
                serde_json::from_value::<crate::diagnostic::ConfinementGuard>(value.clone()).ok()
            })
        {
            diagnostic = guard.project(diagnostic);
        }
        // Preserve the preparation diagnostic when both typed fact sets exist.
        if let Some(failure) = self.extra.get("plan_error").and_then(|value| {
            serde_json::from_value::<crate::diagnostic::PlanFailure>(value.clone()).ok()
        }) {
            diagnostic.detail = Some(failure.detail().into());
        }
        match self.extra.get("guard_resource").and_then(|value| {
            serde_json::from_value::<crate::diagnostic::ResourceFact>(value.clone()).ok()
        }) {
            Some(fact) => fact
                .project(diagnostic)
                .fact(crate::diagnostic::DiagnosticFact::Resource(fact)),
            None => diagnostic,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub path: PathWire,
    pub digest: FileDigest,
    pub bytes: u64,
    pub complete: bool,
    pub redacted: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteGrantRecord {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
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

/// Evidence on which a terminal decision was based.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalEvidence {
    ProcessExit,
    PreLaunchRefusal,
    EffectiveCancellation,
    /// Host observed cancellation while importing, after the provider exited.
    ImportCancellation,
    Unsettled {
        reason: String,
    },
    /// The holder is proven lost for the supplied reason.
    Lost {
        reason: String,
    },
}

/// The closed reason an attempt reached its terminal state. The free-text
/// reason on the state explains; this enum is what callers match on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementReason {
    /// The provider finished and reported its results.
    Completed,
    /// The provider finished without a final handoff, so nothing was reported.
    HandoffMissing,
    /// The lane declared that work remains.
    DeclaredStop,
    /// The provider stopped for a reason other than a finished turn.
    ProviderStop,
    /// A writer left edits it did not commit.
    UncommittedWork,
    /// Refused before any process started.
    PreLaunchRefusal,
    Cancelled,
    Lost,
    Unsettled,
    /// The provider ran and ended in a non-success outcome.
    Failed,
}
impl SettlementReason {
    /// Derive the reason from facts the terminal decision already holds.
    pub fn derive(
        outcome: Outcome,
        evidence: Option<&TerminalEvidence>,
        partial: &[PartialSignal],
    ) -> Self {
        match (evidence, outcome) {
            // A lost or unsettled outcome may still carry a process exit: the
            // process ran, but custody or reporting was not proven. The
            // outcome names the reason, whatever evidence accompanied it.
            (_, Outcome::Lost) => Self::Lost,
            (_, Outcome::Unsettled) => Self::Unsettled,
            (Some(TerminalEvidence::Lost { .. }), _) => Self::Lost,
            (Some(TerminalEvidence::Unsettled { .. }), _) => Self::Unsettled,
            (_, Outcome::Cancelled) => Self::Cancelled,
            (Some(TerminalEvidence::PreLaunchRefusal), _) => Self::PreLaunchRefusal,
            (_, Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted) => [
                (PartialSignal::DeclaredStop, Self::DeclaredStop),
                (PartialSignal::HandoffMissing, Self::HandoffMissing),
                (PartialSignal::ProviderStop, Self::ProviderStop),
                (PartialSignal::UncommittedWork, Self::UncommittedWork),
            ]
            .into_iter()
            .find(|(signal, _)| partial.contains(signal))
            .map_or(Self::Completed, |(_, reason)| reason),
            _ => Self::Failed,
        }
    }
}

/// Independently retried work after a terminal decision.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EffectDisposition {
    #[default]
    Pending,
    Done,
    Cancelled {
        reason: String,
        retryable: bool,
    },
    Failed {
        reason: String,
        retryable: bool,
    },
}

/// Names an independently tracked settlement effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementEffect {
    GitImport,
    ManifestConflict,
    CleanupCustodyRelease,
    /// Remote delivery of a succeeded writer lane.
    Delivery,
}

/// Disposition of an explicit cancellation request relative to provider exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationDisposition {
    Effective,
    ImportEffective,
    DeliveryEffective,
    AcknowledgedIneffective,
    Unknown,
}

/// Current receipts for the effects following a terminal decision.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementReceipts {
    pub git_import: EffectDisposition,
    pub manifest_conflict: EffectDisposition,
    pub cleanup_custody_release: EffectDisposition,
    #[serde(default)]
    pub delivery: EffectDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<CancellationDisposition>,
    /// A request received after the terminal decision was acknowledged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub late_cancellation_acknowledged: bool,
}

impl SettlementReceipts {
    /// The receipt that tracks `effect`.
    pub fn slot_mut(&mut self, effect: SettlementEffect) -> &mut EffectDisposition {
        match effect {
            SettlementEffect::GitImport => &mut self.git_import,
            SettlementEffect::ManifestConflict => &mut self.manifest_conflict,
            SettlementEffect::CleanupCustodyRelease => &mut self.cleanup_custody_release,
            SettlementEffect::Delivery => &mut self.delivery,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkContext>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Human-supplied task title, when provided at submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `fresh` when a resume could not use provider continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
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
    /// Current policy tail on peer observations; absent in durable receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<AttemptId>,
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
    /// Compatibility with records from earlier development versions; never used as authority.
    #[serde(default, skip_serializing)]
    pub granted_tracking_ref: Option<String>,
    pub subject: Option<GitOid>,
    pub head: Option<GitOid>,
    #[serde(default)]
    pub protected_paths: Vec<PathWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<WriterDeliveryReceipt>,
    pub verified_owner: Option<ActorId>,
    pub session: Option<String>,
    pub queued_at: WallTime,
    pub started_at: Option<WallTime>,
    /// Observed nice value of the provider process, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    /// Scheduling priority requested for the provider process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_priority: Option<u8>,
    /// Most recent supervisor-observed stdout/stderr activity while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_at: Option<WallTime>,
    /// Cloud evidence projected by the host; never a local process observation.
    #[serde(skip)]
    pub cloud_observation: Transient<Option<CloudObservation>>,
    /// Redacted cloud job details observed by the host.
    #[serde(skip)]
    pub cloud_view: Transient<Option<serde_json::Value>>,
    /// Transient guardian observation. It is carried over live peer reads but
    /// is never stored in the durable attempt record.
    #[serde(skip)]
    pub provider_silent_for_ms: Transient<Option<u64>>,
    /// Whether the observation checkpoint was absent or could be read and
    /// identity-checked. Used only for live coverage reporting.
    #[serde(skip)]
    pub process_observation_valid: Transient<bool>,
    /// Whether the guardian reports a current disk pause or the host reports
    /// a build-slot wait; either makes silence inapplicable.
    #[serde(skip)]
    pub provider_silence_suppressed: Transient<bool>,
    /// Custody evidence. None in older records must never authorise a writer retry.
    #[serde(default)]
    pub provider_launched: Option<bool>,
    /// Confirmed pre-custody preparation refusal, sealed with its terminal row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_failure: Option<PreparationFailure>,
    pub provider_ms: Option<u64>,
    pub admission_deadline: Option<WallTime>,
    pub runtime_deadline: Option<WallTime>,
    /// Signals that the lane finished only part of its brief. Empty for a lane
    /// with none, which is not proof that it finished everything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial_signals: Vec<PartialSignal>,
    pub state: AttemptState,
    /// Absent on records written before terminal decisions were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_evidence: Option<TerminalEvidence>,
    /// Why the attempt ended, as a closed set a waiter can match on. Stamped
    /// with every terminal decision, including refusals before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_reason: Option<SettlementReason>,
    /// The cancellation disposition known when the decision was sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_disposition: Option<CancellationDisposition>,
    /// Mutable effect receipts, projected independently of the sealed decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_receipts: Option<SettlementReceipts>,
    /// A terminal attempt can still hold its custody fence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody_pending: Option<bool>,
    /// Raw provider evidence; readers must use `effective_usage`.
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
impl AttemptRecord {
    /// Usage attributable to this attempt, excluding legacy Codex session totals.
    pub fn effective_usage(&self) -> UsageRecord {
        if self
            .applied_route
            .as_ref()
            .and_then(|route| route.provider.as_deref())
            == Some("codex")
            && self.usage.extra.get("per_attempt") != Some(&serde_json::Value::Bool(true))
        {
            UsageRecord::default()
        } else {
            self.usage.clone()
        }
    }

    pub fn user_skills(&self) -> Option<&serde_json::Value> {
        self.applied_route.as_ref()?.extra.get("user_skills")
    }
}
/// A host view of an advisory cloud job, never a local supervisor exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudObservation {
    Submitting,
    Running,
    Passed,
    Failed,
    SetupFailed,
    Cancelled,
    Abandoned,
    CancellationUnconfirmed,
    Unknown,
}
impl CloudObservation {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed | Self::SetupFailed | Self::Cancelled | Self::Abandoned
        )
    }
    pub fn state(self) -> &'static str {
        match self {
            Self::Submitting => "cloud_submitting",
            Self::Running => "cloud_running",
            Self::Passed => "cloud_passed",
            Self::Failed => "cloud_failed",
            Self::SetupFailed => "cloud_setup_failed",
            Self::Cancelled => "cloud_cancelled",
            Self::Abandoned => "cloud_abandoned",
            Self::CancellationUnconfirmed => "cloud_cancellation_unconfirmed",
            Self::Unknown => "cloud_unknown",
        }
    }
}

/// A live-only value that is deliberately absent from durable identity and
/// equality. It can decorate a record read without making two reads of that
/// record compare differently.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Transient<T>(pub T);
impl<T> PartialEq for Transient<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl<T> Eq for Transient<T> {}

/// Human-readable, transient explanation for a guardian silence observation.
pub fn provider_silence_reason(silent_for_ms: u64) -> String {
    let seconds = silent_for_ms / 1_000;
    format!("provider silent for {seconds}s; provider still running")
}
/// Why a settled attempt is flagged as a partial finish. A flag is advice for
/// the chair, who decides whether to resume; it never changes the outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialSignal {
    /// A successful final answer did not include a valid final handoff.
    HandoffMissing,
    /// The final answer ends with a valid `handoff` block whose status is
    /// `stopped_early` or `blocked`: the lane's statement that work remains.
    DeclaredStop,
    /// The provider reported a stop reason other than a finished turn.
    ProviderStop,
    /// A writer left edits it did not commit.
    UncommittedWork,
}
impl PartialSignal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HandoffMissing => "handoff_missing",
            Self::DeclaredStop => "declared_stop",
            Self::ProviderStop => "provider_stop",
            Self::UncommittedWork => "uncommitted_work",
        }
    }
}

/// The admission class of a lane: it picks the startup spacing a lane waits
/// out and which earlier starts it is measured against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneClass {
    /// A `worktree-write` lane. A record with no class is a writer, the
    /// conservative reading.
    #[default]
    Writer,
    /// Every other lane.
    ReadOnly,
}
impl LaneClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::ReadOnly => "read_only",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "writer" => Some(Self::Writer),
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }
}
/// What a lane that has not started is waiting for, shown by `lanes` and carried
/// over the peer channel so a peer's lanes read like local ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneWait {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub attempt_id: AttemptId,
    pub lane_class: LaneClass,
    pub waiting_for: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionTicket {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub schema_version: u64,
    pub operation_id: OperationId,
    pub attempt_id: AttemptId,
    pub project_id: ProjectId,
    pub sequence: u64,
    pub generation: u64,
    pub holder: Option<ActorId>,
    pub active: bool,
    /// Advisory available-memory floor as basis points of installed memory.
    pub memory_floor_basis_points: u64,
    pub disk_floor_bytes: u64,
    /// Spacing for this lane's class, fixed at submission.
    pub startup_spacing_ms: u64,
    /// Absent on tickets from before lane classes, which decode as writers.
    #[serde(default)]
    pub lane_class: LaneClass,
    /// The owning run's batch concurrency, distinct from host admission caps.
    pub run_concurrency: u64,
    pub quota_permit: Option<String>,
    pub admission_deadline: WallTime,
    /// The provider's wall-clock allowance once it starts, in seconds. Absent
    /// (zero) on tickets from before runtime deadlines were recorded.
    #[serde(default)]
    pub runtime_seconds: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_plan_diagnostics_use_closed_facts_after_round_trip()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut skipped: SkippedRoute = serde_json::from_value(serde_json::json!({
            "route": "fixture", "kind": "refused", "detail": "SECRET_SENTINEL",
            "plan_error": "protected_overlap", "guard_boundary": "authority",
            "cause": "confinement_unavailable", "confinement_guard": "protected_path_overlap"
        }))?;
        skipped = serde_json::from_slice(&serde_json::to_vec(&skipped)?)?;
        assert_eq!(skipped.extra["guard_boundary"], "authority");
        assert_eq!(
            skipped.diagnostic().detail.as_deref(),
            Some(crate::diagnostic::PlanFailure::ProtectedOverlap.detail())
        );
        skipped
            .extra
            .insert("plan_error".into(), "SECRET_SENTINEL".into());
        assert!(!serde_json::to_string(&skipped.diagnostic())?.contains("SECRET_SENTINEL"));
        Ok(())
    }

    #[test]
    fn graph_launch_provenance_is_typed_bound_and_canonical()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            !WorkContext {
                graph_template_launch: true,
                ..Default::default()
            }
            .accepted()
        );
        let labels = serde_json::json!({"graph_id":"graph-one", "node_id":"node-one"});
        let mut context: WorkContext = serde_json::from_value(labels.clone())?;
        assert!(context.accepted());
        assert!(!context.graph_template_launch);
        assert_eq!(serde_json::to_value(&context)?, labels);
        let before = CanonicalV1::record_digest("run-request-context", 1, &context)?;
        context.graph_template_launch = true;
        assert!(context.accepted());
        assert_eq!(
            serde_json::to_value(&context)?["graph_template_launch"],
            true
        );
        assert_ne!(
            CanonicalV1::record_digest("run-request-context", 1, &context)?,
            before
        );
        assert_eq!(
            serde_json::from_value::<WorkContext>(serde_json::to_value(&context)?)?,
            context
        );
        for missing in ["graph_id", "node_id"] {
            let mut value = serde_json::to_value(&context)?;
            value
                .as_object_mut()
                .ok_or("context fields")?
                .remove(missing);
            assert!(!serde_json::from_value::<WorkContext>(value)?.accepted());
            let mut empty = serde_json::to_value(&context)?;
            empty[missing] = serde_json::json!("");
            assert!(!serde_json::from_value::<WorkContext>(empty)?.accepted());
        }
        let mut unknown = serde_json::to_value(&context)?;
        unknown["unknown_provenance"] = true.into();
        assert!(!serde_json::from_value::<WorkContext>(unknown)?.accepted());
        let explicit_false: WorkContext = serde_json::from_value(serde_json::json!({
            "graph_id":"graph-one", "node_id":"node-one", "graph_template_launch":false
        }))?;
        assert_eq!(serde_json::to_value(explicit_false)?, labels);
        Ok(())
    }

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
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![TaskRequest {
                cloud: None,
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                profile: HarnessProfile {
                    name: "rigorous".into(),
                    ..HarnessProfile::default()
                },
                task_id: TaskId::from_bytes([3; 16]),
                prompt_digest: CanonicalV1::file_digest(b"prompt"),
                base: None,
                head: None,
                source_head: None,
                base_ref: None,
                route: RequestedRoute::Named {
                    extra: Default::default(),
                    name: "senior".into(),
                    effort: None,
                },
                account: None,
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
        assert!(sealed["tasks"][0].get("title").is_none());
        assert!(sealed["tasks"][0].get("fallback").is_none());
        let legacy: CanonicalRequest = serde_json::from_value(sealed.clone())?;
        assert_eq!(serde_json::to_value(&legacy)?, sealed);
        assert_eq!(legacy.digest()?, request.digest()?);
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

    #[test]
    fn a_lost_or_unsettled_outcome_names_its_reason_with_any_evidence() {
        // The process ran, but custody or reporting was not proven.
        for evidence in [
            TerminalEvidence::ProcessExit,
            TerminalEvidence::PreLaunchRefusal,
            TerminalEvidence::Lost {
                reason: "gone".into(),
            },
        ] {
            assert_eq!(
                SettlementReason::derive(Outcome::Lost, Some(&evidence), &[]),
                SettlementReason::Lost
            );
            assert_eq!(
                SettlementReason::derive(Outcome::Unsettled, Some(&evidence), &[]),
                SettlementReason::Unsettled
            );
        }
    }

    #[test]
    fn lane_access_network_defaults_to_open() {
        let access = LaneAccess::default();
        assert!(!access.database_ipc);
        assert_eq!(access.network(false), NetworkAccess::Open);
        assert_eq!(access.network(true), NetworkAccess::Open);
    }

    #[test]
    fn database_ipc_is_explicit_and_round_trips() {
        let old: LaneAccess = serde_json::from_str("{}").expect("old access");
        assert!(!old.database_ipc);
        let access = LaneAccess {
            database_ipc: true,
            ..Default::default()
        };
        let value = serde_json::to_value(&access).expect("access JSON");
        assert_eq!(value["database_ipc"], true);
        assert_eq!(
            serde_json::from_value::<LaneAccess>(value).expect("access round trip"),
            access
        );
        assert!(!access.is_default());
    }
}
