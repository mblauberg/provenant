//! Strict run-store records. Facts are constructed only after store validation.
use crate::{CanonicalError, CanonicalV1, FileDigest, LimitSource, PathWire};
use provenant_core::{
    ActorId, AttemptId, DeliveryId, Digest, GitOid, OperationId, ProjectId, WallTime,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalRequest {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Submission proposal only. Readers trust the host-owned Fabric index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chair: Option<crate::custody::Chair>,
    /// A fresh continuation retains the parent for lineage and admission, but
    /// starts the provider without its previous session. The reason is sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_continuation: Option<String>,
    pub schema_version: u64,
    /// One launch admits exactly one attempt.
    pub task: TaskRequest,
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
/// A one-shot budget continuation allowance carried by its successor request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetWrapUp {
    pub remaining: u64,
}

/// Effective dispatch policy, sealed with the request and copied to each attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessProfile {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub name: String,
    pub knobs: std::collections::BTreeMap<String, serde_json::Value>,
    pub overrides: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confidential: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parent_sandboxed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_by: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub knob_sources: std::collections::BTreeMap<String, LimitSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_wrap_up: Option<BudgetWrapUp>,
    /// Resolved policy strengths for this run, sealed by the dispatch owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_policy: Option<crate::policy::Policy>,
    /// Dispatch-level policy overrides after typed validation.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub policy_overrides: std::collections::BTreeMap<String, crate::policy::Strength>,
    /// Project key used to resolve project-scoped policy at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_project: Option<ProjectId>,
    /// Repository classification sealed before the provider is selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_policy: Option<LaunchPolicy>,
    /// Declared project-relative write scope for writer lanes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_scope: Vec<String>,
    /// Require the adapter to expose its own skill location.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_skill_location: bool,
    /// Remaining settlement retries sealed for a graph task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_retry_limit: Option<u8>,
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
    /// Sources of writer budget fields, retained by graph compilation.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub budget_sources: std::collections::BTreeMap<String, crate::LimitSource>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    /// Pull request associated with this work item, when it is a PR review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    /// Review lens requested by the work item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// The graph's one-based launch order within the node. With `graph_id`
    /// and `node_id` it is the attempt's work key; the store never assigns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
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
            &self.lens,
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
            && self.attempt != Some(0)
            && self.pr != Some(0)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkContext>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Named limit increases requested by `retry --extend`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub limit_extensions: std::collections::BTreeMap<String, u64>,
    /// Select the build-heavy scheduling priority for this task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub build_heavy: bool,
    /// Human-supplied task title, when provided at submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "HarnessProfile::is_empty")]
    pub profile: HarnessProfile,
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

/// Capability name to the source that enabled it, shared by graph and access records.
pub type CapabilitySources = std::collections::BTreeMap<String, String>;

/// Access a dispatch adds to its mode's defaults, sealed in the authority
/// digest. Paths are canonical directories checked at the submission edge.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneAccess {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub capability_sources: CapabilitySources,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody_project: Option<ProjectId>,
    pub project: std::path::PathBuf,
    pub worktree: std::path::PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterDeliveryRequest {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Push is the default delivery mode; integration never checks out either head.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
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
    /// Absent when this receipt did not publish to a destination.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required_family: Option<String>,
        provider: String,
        name: String,
    },
    Model {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required_family: Option<String>,
        provider: String,
        model: String,
        effort: Option<String>,
    },
    Named {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required_family: Option<String>,
        name: String,
        effort: Option<String>,
    },
    /// Pool selection with families already used by this review group.
    Review {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required_family: Option<String>,
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
    pub fn required_family(&self) -> Option<&str> {
        match self {
            Self::Adapter {
                required_family, ..
            }
            | Self::Model {
                required_family, ..
            }
            | Self::Named {
                required_family, ..
            }
            | Self::Review {
                required_family, ..
            } => required_family.as_deref(),
        }
    }

    pub fn set_required_family(&mut self, family: Option<String>) {
        match self {
            Self::Adapter {
                required_family, ..
            }
            | Self::Model {
                required_family, ..
            }
            | Self::Named {
                required_family, ..
            }
            | Self::Review {
                required_family, ..
            } => *required_family = family,
        }
    }

    pub fn workload(
        &self,
        profile: &HarnessProfile,
    ) -> Result<crate::route_selection::Workload, serde_json::Error> {
        if matches!(self, Self::Review { .. }) {
            return Ok(crate::route_selection::Workload::Review);
        }
        profile
            .knobs
            .get("routing.workload")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map(|value| value.unwrap_or_default())
    }
    pub fn projection(self) -> crate::RouteFact {
        match self {
            Self::Adapter { provider, .. } => crate::RouteFact {
                provider: Some(provider),
                model: None,
                effort: None,
                account_label: None,
                account_key: None,
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
                account_key: None,
            },
            Self::Named { effort, .. } | Self::Review { effort, .. } => crate::RouteFact {
                provider: None,
                model: None,
                effort,
                account_label: None,
                account_key: None,
            },
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_context: Option<crate::route_selection::RouteAttemptContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_native: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_warning: Option<crate::route_selection::EffortWarning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_warning: Option<String>,
    /// Human-readable, host-generated warnings retained with the applied route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launch_warnings: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_source: Option<AccountSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_token: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_lane_token: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_paths: Option<ProjectPathsReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv_prompt_limit: Option<ArgvPromptLimitReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_preparation: Option<DependencyPreparationReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_skills: Option<UserSkillsReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_home: Option<CargoHomeReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_closure: Option<Vec<ToolClosureReceipt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_access: Option<AppliedAccessReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_warnings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_credential: Option<GitHubCredentialReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_scope: Option<CiScopeReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxModeReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_admission: Option<SandboxAdmissionReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_grant_limit: Option<WriteGrantLimitReceipt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSource {
    Continuation,
    Pin,
    Automatic,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxSourceReceipt {
    Parent,
    Route,
    Restriction,
    Platform,
    Knob,
    #[default]
    Default,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxModeReceipt {
    #[serde(default)]
    pub requested: bool,
    #[serde(default)]
    pub effective: bool,
    pub source: SandboxSourceReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxFloorStrength {
    Off,
    #[default]
    Warn,
    Strict,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxAdmissionReceipt {
    pub parent_sandboxed: bool,
    pub boundary: crate::diagnostic::BoundaryClass,
    pub strength: SandboxFloorStrength,
    pub knob: String,
    pub source: crate::LimitSource,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginReceipt {
    Compiled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    User {
        path: crate::PathWire,
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
}

impl Default for OriginReceipt {
    fn default() -> Self {
        Self::Compiled {
            profile: None,
            extra: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArgvPromptLimitReceipt {
    #[serde(default)]
    pub value: u64,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub origin: OriginReceipt,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteGrantLimitOrigin {
    Configuration,
    #[default]
    CompiledDefault,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WriteGrantLimitReceipt {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: u64,
    #[serde(default)]
    pub origin: WriteGrantLimitOrigin,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectPathEntry {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub source: String,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectPathWarning {
    pub code: Option<String>,
    pub source: Option<String>,
    pub key: Option<String>,
    pub reason: Option<String>,
    pub policy_code: Option<String>,
    pub policy_message: Option<String>,
    pub credential_source: Option<String>,
    pub path: Option<String>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPathsReceipt {
    #[serde(default)]
    pub shared_paths: Vec<ProjectPathEntry>,
    #[serde(default)]
    pub exec_roots: Vec<ProjectPathEntry>,
    #[serde(default)]
    pub warnings: Vec<ProjectPathWarning>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DependencyPreparationReceipt {
    pub command: Option<String>,
    pub outcome: Option<String>,
    pub reason: Option<String>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSkillWarningReceipt {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub path: String,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSkillsSourceReceipt {
    pub route_policy: Option<Vec<String>>,
    pub key: Option<String>,
    pub origin: Option<OriginReceipt>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSkillsReceipt {
    #[serde(default)]
    pub mode: crate::UserSkillsMode,
    #[serde(default)]
    pub exposed: bool,
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default)]
    pub staged_file_count: usize,
    #[serde(default)]
    pub warnings: Vec<UserSkillWarningReceipt>,
    #[serde(default)]
    pub source: UserSkillsSourceReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_max_bytes: Option<ArgvPromptLimitReceipt>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CargoHomeReceipt {
    #[serde(default)]
    pub path: std::path::PathBuf,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_writes: bool,
    #[serde(default)]
    pub source_writes_source: String,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolClosureReceipt {
    pub tool: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub reasons: Vec<String>,
    pub unresolved: Vec<String>,
    pub libraries: Vec<String>,
    pub already_covered: Vec<String>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppliedAccessGrantReceipt {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub scope: PathScope,
    #[serde(default)]
    pub path_id: String,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OmittedAccessReceipt {
    pub code: String,
    pub reason: String,
    pub credential_source: Option<String>,
    pub path: String,
    pub path_id: String,
    pub source: String,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppliedAccessReceipt {
    pub libraries: Vec<AppliedAccessGrantReceipt>,
    pub staged_content: Option<String>,
    pub toolchain_roots: Vec<AppliedAccessGrantReceipt>,
    pub omitted_grants: Vec<OmittedAccessReceipt>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubCredentialReceipt {
    pub identity: Option<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub delivery: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub mediation: String,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiScopeReceipt {
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub source: crate::LimitSource,
    #[serde(default)]
    pub key: String,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}
impl RouteRecord {
    /// Copy the resolved routing decision into the launch record.
    pub fn from_resolved(
        route: &crate::ResolvedRoute,
        account_label: Option<&str>,
        source: &str,
    ) -> Self {
        Self {
            extra: Default::default(),
            selection_context: Some(route.selection_context.clone()),
            host_native: None,
            effort_warning: route.diagnostics.effort_warning.clone(),
            availability_warning: None,
            launch_warnings: route.diagnostics.launch_warnings.clone(),
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
            account_key: r.account_key,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_attempt: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_total: Option<Box<UsageRecord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_invocations: Option<GhInvocationReceipt>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhInvocationReceipt {
    #[serde(default)]
    pub count: u64,
    pub budget: Option<u64>,
    pub source: Option<crate::LimitSource>,
    #[serde(default)]
    pub over_budget: bool,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
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
/// The persisted notification policy resolved at refusal time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalNotice {
    Inbox,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecord {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight_digest_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_notice: Option<RefusalNotice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refusal_notice_done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_intent_digest: Option<Digest>,
    pub schema_version: u64,
    pub project_id: ProjectId,
    pub actor_id: ActorId,
    pub operation_id: OperationId,
    pub kind: OperationKind,
    pub request_digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_attempt: Option<AttemptId>,
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
    pub refusal_chair: Option<crate::custody::Chair>,
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
    /// The pinned source view of an isolated lane could not be built.
    SourceViewRefused,
    AccountLoginRequired,
    RouteResolutionRefused,
    AccessPolicyRefused,
    DiskFloor,
    DiskObservationUnavailable,
    StoredPromptUnreadable,
    /// A request stored before cloud placement was removed names `cloud`;
    /// it never runs under the local supervisor.
    CloudPlacementRemoved,
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
    IdentityMismatch,
    /// A writer declared unfinished work and its pinned head equals its base.
    StoppedNoWork,
    /// A writer finished without a final message.
    EmptyWriterResult,
    /// The host stopped a writer at its context handoff threshold so a fresh
    /// attempt continues from its handoff on the same worktree.
    ContextHandoff,
    /// A writer's handoff says `blocked`; the stop carries its note.
    Blocked,
    /// The lane's launch-time access token was revoked or expired mid-run
    /// while the account login stayed usable.
    LaneTokenLost,
    /// The host interrupted this attempt to deliver a queued steering turn.
    Steered,
}

impl TerminalCause {
    /// Bounded label for human status rows. Structured results use `detail`.
    pub fn reason(self) -> &'static str {
        match self {
            Self::SourceViewRefused => "source view refused; provenant output ID",
            Self::AccountLoginRequired => "login required; provenant output ID",
            Self::RouteResolutionRefused => "route refused; provenant output ID",
            Self::AccessPolicyRefused => "access policy refused; provenant output ID",
            Self::DiskFloor => "disk below gc.critical_free; provenant output ID",
            Self::DiskObservationUnavailable => "disk probe unavailable; provenant output ID",
            Self::StoredPromptUnreadable => "stored prompt unreadable; provenant output ID",
            Self::CloudPlacementRemoved => "cloud placement removed; provenant output ID",
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
            Self::IdentityMismatch => "provider identity mismatch; provenant output ID",
            Self::StoppedNoWork => "stopped with no work (no new commit); provenant output ID",
            Self::EmptyWriterResult => "empty final message; review writer commit",
            Self::ContextHandoff => "context handoff threshold; provenant output ID",
            Self::Blocked => "blocked; provenant output ID",
            Self::LaneTokenLost => "lane token lost; provenant output ID",
            Self::Steered => "steering interruption; provenant output ID",
        }
    }

    /// Full safe cause and remedy for structured results and error responses.
    pub fn detail(self) -> &'static str {
        match self {
            Self::AccountLoginRequired => {
                crate::diagnostic::ProviderFailureFact::AuthRequired.reason()
            }
            Self::SourceViewRefused => {
                "the pinned source view of the lane could not be built; the step and cause follow. Retry, or raise domains.run.source_view_seconds, source_view_entries or source_view_bytes when the cause names a limit"
            }
            Self::RouteResolutionRefused => {
                "route resolution refused the lane; check the model, effort, exclusions and availability with `provenant route resolve` and `provenant quota tick`"
            }
            Self::DiskFloor => {
                "the host writer disk precheck refused launch below gc.critical_free; free disk space or run `provenant gc --apply` before retrying"
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
            Self::CloudPlacementRemoved => {
                "this attempt was stored with cloud placement, which was removed, so it never runs locally; relaunch the brief with `provenant cloud launch --file BRIEF`"
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
            Self::IdentityMismatch => {
                "observed provider identity differs from the requested route; inspect the local session evidence"
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
            Self::Blocked => {
                "the writer declared itself blocked: an owner decision, a missing capability or an external change prevents progress; its handoff note says what"
            }
            Self::LaneTokenLost => {
                "the lane's access token was revoked or expired mid-run while the account login stayed usable; a writer continues from its handoff on a new token"
            }
            Self::Steered => {
                "the host interrupted this attempt for a queued steering turn; continuation requires the retained session for this provider, model and account"
            }
        }
    }
}

/// Why a running lane yielded while its graph children were active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildWait {
    Children,
    Admission,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_wait: Option<ChildWait>,
    },
    Settling {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        publication_pending: bool,
    },
    Terminal {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_cause: Option<TerminalCause>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop: Option<crate::Stop>,
        outcome: Outcome,
        exit_code: Option<i64>,
        finished_at: WallTime,
        reason: Option<String>,
    },
}
impl AttemptState {
    /// Read the sealed typed stop. Malformed wire values fail while the record
    /// is decoded, before they can be observed as an absent stop.
    pub fn stop(&self) -> Option<crate::Stop> {
        match self {
            Self::Terminal { stop, .. } => stop.clone(),
            _ => None,
        }
    }

    pub fn terminal_cause(&self) -> Option<TerminalCause> {
        match self {
            Self::Terminal { terminal_cause, .. } => *terminal_cause,
            _ => None,
        }
    }

    pub fn terminal_note(&self) -> Option<&str> {
        match self {
            Self::Terminal { terminal_note, .. } => terminal_note.as_deref(),
            _ => None,
        }
    }
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_accounts: Vec<crate::SkippedAccount>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// `adapter/model`, or the adapter alone for an account-level skip.
    pub route: String,
    pub kind: SkipKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_error: Option<crate::diagnostic::PlanFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_boundary: Option<crate::diagnostic::BoundaryClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_resource: Option<crate::diagnostic::ResourceFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confinement_guard: Option<crate::diagnostic::ConfinementGuard>,
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
        self.guard_boundary = boundary;
        self.guard_resource = resource;
        Ok(())
    }

    /// Describe a refused route for public output without provider or OS text.
    pub fn describe(&self) -> String {
        let diagnostic = self.diagnostic();
        let mut reason = format!(
            "{}: {}",
            crate::diagnostic::safe_text(&self.route),
            diagnostic
                .detail
                .as_deref()
                .unwrap_or("the route was refused")
        );
        if let Some(next) = diagnostic.next {
            reason.push_str("; ");
            reason.push_str(&next);
        }
        let accounts = &self.skipped_accounts;
        if !accounts.is_empty() {
            reason.push_str("; accounts skipped: ");
            reason.push_str(&crate::SkippedAccount::describe(accounts));
        }
        reason
    }

    /// Project typed facts only; `detail` may contain provider or OS text.
    pub fn diagnostic(&self) -> crate::diagnostic::Diagnostic {
        let mut diagnostic = self
            .cause
            .as_ref()
            .map(|cause| cause.diagnostic("REFUSED"))
            .unwrap_or_else(|| self.kind.diagnostic());
        if self.cause == Some(crate::RouteExclusionReason::ConfinementUnavailable)
            && let Some(guard) = self.confinement_guard
        {
            diagnostic = guard.project(diagnostic);
        }
        // Preserve the preparation diagnostic when both typed fact sets exist.
        if let Some(failure) = self.plan_error {
            diagnostic.detail = Some(failure.detail().into());
        }
        match self.guard_resource {
            Some(fact) => fact
                .project(diagnostic)
                .fact(crate::diagnostic::DiagnosticFact::Resource(fact)),
            None => diagnostic,
        }
    }
}

impl Default for SkippedRoute {
    fn default() -> Self {
        Self {
            skipped_accounts: Vec::new(),
            extra: Default::default(),
            route: String::new(),
            kind: SkipKind::Unavailable,
            plan_error: None,
            guard_boundary: None,
            guard_resource: None,
            confinement_guard: None,
            cause: None,
            detail: String::new(),
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathScope {
    #[default]
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

/// Who owns a settlement cause, independently of any completed provider work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CauseSource {
    Provider,
    Host,
    Owner,
    /// Retained terminal data did not carry a valid typed stop or reason.
    Unavailable,
}

/// Whether a settled attempt may release its graph dependants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementNodeOutcome {
    Succeeded,
    Failed,
}

/// The source and graph result for one settlement reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettlementRule {
    pub reason: SettlementReason,
    pub source: CauseSource,
    pub node_outcome: SettlementNodeOutcome,
    pub terminal_outcome: SettlementTerminalOutcome,
    pub stop_cause: Option<crate::stop::StopCause>,
}

/// How a settlement reason normalises a successful-looking provisional outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementTerminalOutcome {
    Preserve,
    Succeeded,
    ReviewRequired,
    Uncommitted,
}

impl SettlementTerminalOutcome {
    fn apply(self, observed: Outcome) -> Outcome {
        match self {
            Self::Preserve => observed,
            Self::Succeeded => Outcome::Succeeded,
            Self::ReviewRequired if observed == Outcome::Succeeded => Outcome::ReviewRequired,
            Self::ReviewRequired => observed,
            Self::Uncommitted
                if matches!(observed, Outcome::Succeeded | Outcome::ReviewRequired) =>
            {
                Outcome::Uncommitted
            }
            Self::Uncommitted => observed,
        }
    }
}

/// One declaration owns the durable reasons and their cross-cutting policy.
/// Generated enum, ALL and table cannot drift apart.
macro_rules! settlement_reasons {
    ($($(#[$meta:meta])* $reason:ident { source: $source:ident, node: $node:ident, outcome: $outcome:ident, stop: $stop:expr }),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum SettlementReason {
            $($(#[$meta])* $reason),+
        }

        impl SettlementReason {
            pub const ALL: &'static [Self] = &[$(Self::$reason),+];

            pub const fn rule(self) -> SettlementRule {
                match self {
                    $(Self::$reason => SettlementRule {
                        reason: Self::$reason,
                        source: CauseSource::$source,
                        node_outcome: SettlementNodeOutcome::$node,
                        terminal_outcome: SettlementTerminalOutcome::$outcome,
                        stop_cause: $stop,
                    }),+
                }
            }
        }

        pub const SETTLEMENT_TABLE: &[SettlementRule] = &[
            $(SettlementRule {
                reason: SettlementReason::$reason,
                source: CauseSource::$source,
                node_outcome: SettlementNodeOutcome::$node,
                terminal_outcome: SettlementTerminalOutcome::$outcome,
                stop_cause: $stop,
            }),+
        ];
    };
}

settlement_reasons! {
    /// The provider finished and reported its results.
    Completed { source: Provider, node: Succeeded, outcome: Preserve, stop: None },
    /// A complete handoff preceded the host token-budget stop.
    CompletedBeforeBudget { source: Host, node: Succeeded, outcome: Succeeded, stop: Some(crate::stop::StopCause::TokenBudget) },
    /// The provider finished without a final handoff, so nothing was reported.
    HandoffMissing { source: Provider, node: Failed, outcome: ReviewRequired, stop: Some(crate::stop::StopCause::HandoffMissing) },
    /// A final handoff was present but malformed; retain its field diagnostics.
    HandoffMalformed { source: Provider, node: Failed, outcome: ReviewRequired, stop: Some(crate::stop::StopCause::HandoffMalformed) },
    /// The writer exited without a final message.
    EmptyResult { source: Provider, node: Failed, outcome: ReviewRequired, stop: Some(crate::stop::StopCause::EmptyResult) },
    /// The lane declared that work remains.
    DeclaredStop { source: Provider, node: Failed, outcome: ReviewRequired, stop: None },
    /// The provider stopped for a reason other than a finished turn.
    ProviderStop { source: Provider, node: Failed, outcome: ReviewRequired, stop: Some(crate::stop::StopCause::ProviderStop) },
    /// A writer left edits it did not commit.
    UncommittedWork { source: Provider, node: Failed, outcome: Uncommitted, stop: None },
    /// Refused before any process started.
    PreLaunchRefusal { source: Host, node: Failed, outcome: Preserve, stop: None },
    Cancelled { source: Owner, node: Failed, outcome: Preserve, stop: Some(crate::stop::StopCause::Cancelled) },
    /// The host interrupted a run to deliver its queued continuation turn.
    Steered { source: Host, node: Failed, outcome: Preserve, stop: Some(crate::stop::StopCause::Steered) },
    Lost { source: Host, node: Failed, outcome: Preserve, stop: Some(crate::stop::StopCause::Lost) },
    Unsettled { source: Host, node: Failed, outcome: Preserve, stop: Some(crate::stop::StopCause::Unsettled) },
    /// The provider ran and ended in a non-success outcome.
    Failed { source: Provider, node: Failed, outcome: Preserve, stop: None },
}
impl SettlementReason {
    /// Derive the reason from the complete terminal facts before any reader
    /// interprets the outcome, stop or next step independently.
    pub fn derive(
        outcome: Outcome,
        evidence: Option<&TerminalEvidence>,
        partial: &[PartialSignal],
        terminal_cause: Option<TerminalCause>,
        handoff: Result<Option<&crate::handoff::Handoff>, &crate::handoff::HandoffError>,
    ) -> Self {
        match (evidence, outcome) {
            (_, Outcome::Lost) => Self::Lost,
            (_, Outcome::Unsettled) => Self::Unsettled,
            (Some(TerminalEvidence::Lost { .. }), _) => Self::Lost,
            (Some(TerminalEvidence::Unsettled { .. }), _) => Self::Unsettled,
            (_, Outcome::Cancelled) if terminal_cause == Some(TerminalCause::Steered) => {
                Self::Steered
            }
            (_, Outcome::Cancelled) => Self::Cancelled,
            (Some(TerminalEvidence::PreLaunchRefusal), _) => Self::PreLaunchRefusal,
            _ if handoff.is_err() => Self::HandoffMalformed,
            _ => match terminal_cause {
                Some(TerminalCause::EmptyWriterResult) => Self::EmptyResult,
                Some(TerminalCause::Blocked | TerminalCause::StoppedNoWork) => Self::DeclaredStop,
                Some(TerminalCause::ProviderEarlyStop) => Self::ProviderStop,
                _ if handoff
                    .is_ok_and(|handoff| handoff.is_some_and(|handoff| handoff.unfinished())) =>
                {
                    Self::DeclaredStop
                }
                _ if partial.contains(&PartialSignal::DeclaredStop) => Self::DeclaredStop,
                _ if partial.contains(&PartialSignal::HandoffMissing) => Self::HandoffMissing,
                _ if partial.contains(&PartialSignal::ProviderStop) => Self::ProviderStop,
                _ if partial.contains(&PartialSignal::UncommittedWork)
                    || outcome == Outcome::Uncommitted =>
                {
                    Self::UncommittedWork
                }
                Some(TerminalCause::TokenBudget)
                    if matches!(
                        outcome,
                        Outcome::Succeeded
                            | Outcome::ReviewRequired
                            | Outcome::Failed
                            | Outcome::TimedOut
                    ) && handoff.is_ok_and(|handoff| {
                        handoff.is_some_and(|handoff| {
                            !handoff.unfinished() && handoff.validate().is_ok()
                        })
                    }) =>
                {
                    Self::CompletedBeforeBudget
                }
                _ if matches!(outcome, Outcome::Succeeded | Outcome::ReviewRequired) => {
                    Self::Completed
                }
                _ => Self::Failed,
            },
        }
    }

    /// A completed budget boundary retains its host stop without closing
    /// dependants. Other typed stops keep dependants closed.
    pub const fn node_outcome(self, stop: Option<crate::stop::StopCause>) -> SettlementNodeOutcome {
        if stop.is_some()
            && !matches!(
                self.rule().terminal_outcome,
                SettlementTerminalOutcome::Succeeded
            )
        {
            SettlementNodeOutcome::Failed
        } else {
            self.rule().node_outcome
        }
    }

    /// Derive the persisted reason, sealed stop, source and graph result from
    /// the same complete terminal facts.
    pub fn classify(
        outcome: Outcome,
        evidence: Option<&TerminalEvidence>,
        partial: &[PartialSignal],
        terminal_cause: Option<TerminalCause>,
        handoff: Result<Option<&crate::handoff::Handoff>, &crate::handoff::HandoffError>,
    ) -> Result<SettlementClassification, crate::stop::StopError> {
        let reason = Self::derive(outcome, evidence, partial, terminal_cause, handoff);
        let outcome = reason.rule().terminal_outcome.apply(outcome);
        let stop = crate::stop::Stop::sealed(
            terminal_cause,
            outcome,
            reason,
            handoff
                .map(|handoff| handoff.cloned())
                .map_err(Clone::clone),
        )?;
        let stop_cause = stop.as_ref().map(crate::stop::Stop::cause);
        let source = stop_cause.map_or(reason.rule().source, crate::stop::StopCause::source);
        let node_outcome = reason.node_outcome(stop_cause);
        Ok(SettlementClassification {
            outcome,
            reason,
            stop,
            source,
            node_outcome,
        })
    }
}

/// Complete terminal classification shared by persistence, graph transitions
/// and route quality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementClassification {
    pub outcome: Outcome,
    pub reason: SettlementReason,
    pub stop: Option<crate::stop::Stop>,
    pub source: CauseSource,
    pub node_outcome: SettlementNodeOutcome,
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

fn decode_continuation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    if value.as_deref() == Some("automatic") {
        return Err(serde::de::Error::custom(
            "automatic continuation was removed",
        ));
    }
    Ok(value)
}

/// Stable identity of a graph hosted by a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerGraph {
    pub host: provenant_core::HostId,
    pub graph_id: String,
}

/// Typed peer graph metadata carried beside an attempt or an output page.
/// Its serde flattening keeps the established wire keys at the owning record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerGraphFields {
    #[serde(rename = "peer_graph", skip_serializing_if = "Option::is_none")]
    pub peer_graph: Option<PeerGraph>,
    #[serde(
        rename = "peer_graph_collected",
        skip_serializing_if = "Option::is_none"
    )]
    pub peer_graph_collected: Option<bool>,
    #[serde(
        rename = "peer_origin_work_key",
        skip_serializing_if = "Option::is_none"
    )]
    pub peer_origin_work_key: Option<crate::WorkKey>,
    #[serde(
        rename = "peer_origin_context",
        skip_serializing_if = "Option::is_none"
    )]
    pub peer_origin_context: Option<WorkContext>,
    #[serde(rename = "peer_subtree", skip_serializing_if = "Option::is_none")]
    pub peer_subtree: Option<PeerGraphSubtree>,
    #[serde(
        rename = "graph_custody_released",
        skip_serializing_if = "Option::is_none"
    )]
    pub graph_custody_released: Option<bool>,
    #[serde(
        rename = "graph_handoff",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "decode_peer_handoff"
    )]
    /// The peer's sealed settlement outcome: a declaration, an explicit
    /// malformed declaration, or none.
    pub graph_handoff: Option<crate::handoff::HandoffResult>,
}

impl PeerGraphFields {
    /// Apply a projection while preserving peer metadata already attached to the record.
    pub fn merge(&mut self, newer: Self) {
        if newer.peer_graph.is_some() {
            self.peer_graph = newer.peer_graph;
        }
        if newer.peer_graph_collected.is_some() {
            self.peer_graph_collected = newer.peer_graph_collected;
        }
        if newer.peer_origin_work_key.is_some() {
            self.peer_origin_work_key = newer.peer_origin_work_key;
        }
        if newer.peer_origin_context.is_some() {
            self.peer_origin_context = newer.peer_origin_context;
        }
        if newer.peer_subtree.is_some() {
            self.peer_subtree = newer.peer_subtree;
        }
        if newer.graph_custody_released.is_some() {
            self.graph_custody_released = newer.graph_custody_released;
        }
        if newer.graph_handoff.is_some() {
            self.graph_handoff = newer.graph_handoff;
        }
    }
}

/// Output-only peer metadata, with the byte length owned by the output boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerOutputFields {
    #[serde(flatten)]
    pub graph: PeerGraphFields,
    #[serde(rename = "length", skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    #[serde(rename = "planner_fallback", skip_serializing_if = "Option::is_none")]
    pub planner_fallback: Option<String>,
}

impl PeerOutputFields {
    pub fn merge_graph(&mut self, newer: PeerGraphFields) {
        self.graph.merge(newer);
    }
}

impl From<PeerGraphFields> for PeerOutputFields {
    fn from(graph: PeerGraphFields) -> Self {
        Self {
            graph,
            length: None,
            planner_fallback: None,
        }
    }
}

fn decode_peer_handoff<'de, D>(
    deserializer: D,
) -> Result<Option<crate::handoff::HandoffResult>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(sealed) = Option::<crate::handoff::HandoffResult>::deserialize(deserializer)? else {
        return Ok(None);
    };
    // A sealed declaration was validated on its host; one that no longer
    // validates is corrupt, not evidence.
    match sealed {
        Ok(Some(handoff)) => {
            let bytes = serde_json::to_vec(&handoff).map_err(serde::de::Error::custom)?;
            crate::handoff::Handoff::from_json(&bytes)
                .map(|handoff| Some(Ok(Some(handoff))))
                .map_err(serde::de::Error::custom)
        }
        other => Ok(Some(other)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerGraphSubtree {
    pub status: crate::graph::GraphStatus,
    pub outcome: Option<crate::graph::GraphOutcome>,
    pub next_node: Option<usize>,
    pub checkpoint: PeerGraphCheckpoint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerGraphCheckpoint {
    pub schema_version: u64,
    pub graph_id: String,
    pub plan_digest: String,
    pub cancelled: bool,
    pub nodes: Vec<PeerGraphNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerGraphNode {
    pub id: String,
    pub state: crate::graph::NodeState,
    pub worktree: Option<String>,
    pub attempts: Vec<PeerGraphAttempt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerGraphAttempt {
    pub launched: bool,
    pub key: crate::graph::AttemptKey,
    pub route: PeerGraphRoute,
    pub state: crate::graph::AttemptState,
    pub attempt_ref: Option<String>,
    pub outcome: Option<crate::graph::AttemptOutcome>,
    pub launch_actor: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerGraphRoute {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub alias: Option<String>,
    pub name: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_family: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkContext>,
    /// The attempt's work identity: graph, node and attempt number.
    pub key: crate::WorkKey,
    #[serde(flatten)]
    pub peer: PeerGraphFields,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_overrun: Option<crate::limit::BudgetOverrun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_wrap_up_refused: Option<crate::limit::BudgetWrapUpRefused>,
    /// Frozen repository command and lease phase used for admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_admission_binding: Option<RepositoryAdmissionBinding>,
    /// Human-supplied task title, when provided at submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `fresh` when a resume could not use provider continuation.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "decode_continuation"
    )]
    pub continuation: Option<String>,
    #[serde(default, skip_serializing_if = "HarnessProfile::is_empty")]
    pub profile: HarnessProfile,
    /// The limits resolved when the attempt was admitted to launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::LaneLimits>,
    pub schema_version: u64,
    pub attempt_id: AttemptId,
    pub project_id: ProjectId,
    pub attempt_number: u64,
    pub revision: u64,
    pub generation: u64,
    pub delivery_id: Option<DeliveryId>,
    pub retry_of: Option<AttemptId>,
    /// Current policy tail on peer observations; absent in durable receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<AttemptId>,
    /// Read-only projection of the host's private steering ledger. This is
    /// populated by RunStore readers and is never persisted in the attempt row.
    #[serde(skip)]
    pub steering: Option<SteeringStatus>,
    pub requested_route: RequestedRoute,
    pub resolved_route: Option<RouteRecord>,
    pub applied_route: Option<RouteRecord>,
    pub observed_route: Option<RouteRecord>,
    pub authority_digest: Digest,
    /// Project-relative write exceptions actually granted before launch.
    pub read_only_writable: Vec<String>,
    /// Explicit write allowances from the plan handed to confinement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_grants: Vec<WriteGrantRecord>,
    /// Writers only; concrete Git paths stay out of the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_grants_digest: Option<Digest>,
    pub subject: Option<GitOid>,
    pub head: Option<GitOid>,
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
    /// Transient guardian observation. It is carried over live peer reads but
    /// is never stored in the durable attempt record.
    #[serde(skip)]
    pub provider_silent_for_ms: Transient<Option<u64>>,
    /// Whether the observation checkpoint was absent or could be read and
    /// identity-checked. Used only for live coverage reporting.
    #[serde(skip)]
    pub process_observation_valid: Transient<bool>,
    /// Custody evidence. Absence is an unknown launch state and never authorises a writer retry.
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
    /// Trace context stamped when the attempt is created.
    pub trace: crate::Trace,
}

/// Outcome visible for an accepted steering turn, without exposing its text.
/// A live session acknowledgement reports `Resumed` promptly; terminal proof
/// can still revise that state to `NotContinued`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringState {
    Queued,
    Resumed,
    NotContinued,
}

/// Safe reason an accepted steering turn could not continue the provider
/// session. This projection never carries provider text or local paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringCause {
    SessionUnavailable,
    SessionMismatch,
    SessionProofIncomplete,
    TerminalStop,
    ContinuationRefused,
}

impl SteeringCause {
    /// Stable value persisted in the private RunStore outcome ledger.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionUnavailable => "session_unavailable",
            Self::SessionMismatch => "session_mismatch",
            Self::SessionProofIncomplete => "session_proof_incomplete",
            Self::TerminalStop => "terminal_stop",
            Self::ContinuationRefused => "continuation_refused",
        }
    }

    /// Parse the bounded RunStore outcome value.
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "session_unavailable" => Some(Self::SessionUnavailable),
            "session_mismatch" => Some(Self::SessionMismatch),
            "session_proof_incomplete" => Some(Self::SessionProofIncomplete),
            "terminal_stop" => Some(Self::TerminalStop),
            "continuation_refused" => Some(Self::ContinuationRefused),
            _ => None,
        }
    }
}

/// Stable identifiers for one accepted steering operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringOperation {
    pub operation_id: OperationId,
    pub message_id: provenant_core::MessageId,
}

/// Safe status projection of the private steering ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringStatus {
    pub state: SteeringState,
    pub operations: Vec<SteeringOperation>,
    pub successor: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<SteeringCause>,
}

/// Encode the public form of an attempt, retaining all public extra metadata.
pub fn public_attempt_value(
    record: &AttemptRecord,
) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = serde_json::to_value(record)?;
    let object = value.as_object_mut().ok_or_else(|| {
        <serde_json::Error as serde::ser::Error>::custom("attempt record must be an object")
    })?;
    object.remove("repository_admission_binding");
    Ok(value)
}

/// Serialize an attempt for read-only owner and peer projections.
pub fn serialise_public_attempt<S: serde::Serializer>(
    record: &AttemptRecord,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    public_attempt_value(record)
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

impl AttemptRecord {
    /// Usage attributable to this attempt; unmarked Codex totals are session-wide.
    pub fn effective_usage(&self) -> UsageRecord {
        if self
            .applied_route
            .as_ref()
            .and_then(|route| route.provider.as_deref())
            == Some("codex")
            && self.usage.per_attempt != Some(true)
        {
            UsageRecord::default()
        } else {
            self.usage.clone()
        }
    }

    pub fn user_skills(&self) -> Option<&UserSkillsReceipt> {
        self.applied_route.as_ref()?.user_skills.as_ref()
    }

    /// The recorded sandbox of a launched attempt: requested/effective/source.
    pub fn sandbox(&self) -> Option<&SandboxModeReceipt> {
        self.applied_route.as_ref()?.sandbox.as_ref()
    }

    /// Whether this attempt carries a user-enabled sandbox admission floor.
    /// Missing evidence does not enable sandboxing. Children use the selected
    /// warn/off/strict policy rather than treating this as an authority grant.
    pub fn floors_children(&self) -> bool {
        sandbox_floors_children(self.sandbox())
    }
}
/// [`AttemptRecord::floors_children`] for a recorded sandbox mode.
fn sandbox_floors_children(sandbox: Option<&SandboxModeReceipt>) -> bool {
    sandbox.is_some_and(|sandbox| {
        sandbox.effective
            && (sandbox.requested
                || matches!(
                    sandbox.source,
                    SandboxSourceReceipt::Parent | SandboxSourceReceipt::Route
                ))
    })
}
/// Sealed launch classification, shared by local, peer and recovered attempts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPolicy {
    pub confidential: bool,
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_config: Option<String>,
}

/// The one sandbox admission floor. The parent fact comes from a host-sealed
/// attempt, never from prompt text. This is advisory policy (boundary None),
/// with warn as its default; strict is an explicit user override.
pub fn admission_floor(
    parent_sandboxed: bool,
    sandbox: Option<&str>,
    strength: &str,
) -> (bool, bool) {
    let applies = parent_sandboxed && sandbox != Some("on");
    (
        applies && strength == "strict",
        applies && strength == "warn",
    )
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
    /// Identity evidence is incomplete; usage stays attributed to the requested account.
    IdentityUnobserved,
    /// A successful final answer did not include a final handoff.
    HandoffMissing,
    /// A final handoff was present but malformed.
    HandoffMalformed,
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
            Self::IdentityUnobserved => "identity_unobserved",
            Self::HandoffMissing => "handoff_missing",
            Self::HandoffMalformed => "handoff_malformed",
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
/// Why a lane is waiting, including the facts needed to render a useful label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaneWaitReason {
    HostStalled,
    Children,
    Admission,
    ProviderSilence { silent_for_ms: u64 },
    LaunchPreparation,
    RecoveryWorkerRefused { used: u64, limit: u64 },
}

impl LaneWaitReason {
    pub fn description(&self) -> String {
        match self {
            Self::HostStalled => "host_stalled: supervisor is in uninterruptible sleep".into(),
            Self::Children => "children: host and project admission yielded".into(),
            Self::Admission => "admission: waiting for host or project capacity".into(),
            Self::ProviderSilence { silent_for_ms } => provider_silence_reason(*silent_for_ms),
            Self::LaunchPreparation => "launch preparation".into(),
            Self::RecoveryWorkerRefused { used, limit } => format!(
                "launch: last recovery worker refused: user process count {used} of {limit}"
            ),
        }
    }
}

/// Lifecycle phase for a frozen repository admission command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryAdmissionPhase {
    AcquirePending,
    Acquired,
    ReleasePending,
    Released,
}

impl RepositoryAdmissionPhase {
    pub fn command(self) -> Option<&'static str> {
        match self {
            Self::AcquirePending => Some("acquire"),
            Self::ReleasePending => Some("release"),
            Self::Acquired | Self::Released => None,
        }
    }
}

/// The command, source and lifecycle phase frozen with an admitted attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RepositoryAdmissionBinding {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub source: crate::PortSource,
    pub command: crate::PortCommand,
    pub command_digest: FileDigest,
    pub lease_epoch: u64,
    pub phase: RepositoryAdmissionPhase,
}

impl<'de> Deserialize<'de> for RepositoryAdmissionBinding {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(flatten)]
            extra: crate::ExtraFields,
            source: crate::PortSource,
            command: crate::PortCommand,
            command_digest: FileDigest,
            lease_epoch: u64,
            phase: RepositoryAdmissionPhase,
        }
        let wire = Wire::deserialize(deserializer)?;
        let binding = Self {
            extra: wire.extra,
            source: wire.source,
            command: wire.command,
            command_digest: wire.command_digest,
            lease_epoch: wire.lease_epoch,
            phase: wire.phase,
        };
        if !binding.valid() {
            return Err(serde::de::Error::custom(
                "invalid repository admission binding",
            ));
        }
        Ok(binding)
    }
}

impl RepositoryAdmissionBinding {
    pub fn new(source: crate::PortSource, command: &crate::PortCommand) -> Self {
        Self {
            extra: Default::default(),
            source,
            command: command.clone(),
            command_digest: command.digest(),
            lease_epoch: 1,
            phase: RepositoryAdmissionPhase::AcquirePending,
        }
    }

    pub fn valid(&self) -> bool {
        self.lease_epoch != 0
            && self.command.is_valid()
            && self.command_digest == self.command.digest()
    }

    pub fn can_transition_to(&self, next: RepositoryAdmissionPhase, active_owner: bool) -> bool {
        matches!(
            (self.phase, next),
            (
                RepositoryAdmissionPhase::AcquirePending,
                RepositoryAdmissionPhase::Acquired
            ) | (
                RepositoryAdmissionPhase::AcquirePending,
                RepositoryAdmissionPhase::ReleasePending
            ) | (
                RepositoryAdmissionPhase::Acquired,
                RepositoryAdmissionPhase::ReleasePending
            ) | (
                RepositoryAdmissionPhase::ReleasePending,
                RepositoryAdmissionPhase::Released
            )
        ) || (active_owner
            && self.phase == RepositoryAdmissionPhase::Released
            && next == RepositoryAdmissionPhase::AcquirePending)
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
    pub waiting_for: LaneWaitReason,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionTicket {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub schema_version: u64,
    pub attempt_id: AttemptId,
    pub project_id: ProjectId,
    pub sequence: u64,
    pub generation: u64,
    pub holder: Option<ActorId>,
    pub active: bool,
    /// Advisory available-memory floor as basis points of installed memory.
    pub memory_floor_basis_points: u64,
    /// Spacing for this lane's class, fixed at submission.
    pub startup_spacing_ms: u64,
    pub lane_class: LaneClass,
    pub quota_permit: Option<String>,
    pub admission_deadline: WallTime,
    /// The provider's wall-clock allowance once it starts, in seconds.
    pub runtime_seconds: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub schema_version: u64,
    pub sequence: u64,
    pub project_id: ProjectId,
    pub attempt_id: AttemptId,
    pub revision: u64,
    pub change: AttemptState,
    pub trace: crate::Trace,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_state_fields_round_trip_and_reject_malformed_values() {
        let base = serde_json::json!({
            "state":"terminal",
            "outcome":"failed",
            "exit_code":1,
            "finished_at":serde_json::to_value(WallTime::from_micros(1).unwrap()).unwrap(),
            "reason":"fixture"
        });
        for (base, field, value, malformed) in [
            (
                base.clone(),
                "terminal_cause",
                serde_json::json!("token_budget"),
                serde_json::json!("unknown"),
            ),
            (
                base.clone(),
                "terminal_note",
                serde_json::json!("bounded reason"),
                serde_json::json!(17),
            ),
            (
                serde_json::json!({"state":"running"}),
                "child_wait",
                serde_json::json!("children"),
                serde_json::json!("unknown"),
            ),
            (
                serde_json::json!({"state":"running"}),
                "child_wait",
                serde_json::json!("admission"),
                serde_json::json!({"kind":"children"}),
            ),
        ] {
            let mut wire = base;
            wire[field] = value;
            let bytes = serde_json::to_vec(&wire).unwrap();
            let decoded: AttemptState =
                crate::json::decode(bytes.as_slice(), crate::json::Limits::default()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
            wire[field] = malformed;
            let bytes = serde_json::to_vec(&wire).unwrap();
            assert!(matches!(
                crate::json::decode::<AttemptState, _>(
                    bytes.as_slice(),
                    crate::json::Limits::default()
                ),
                Err(crate::json::DecodeError::Typed { .. })
            ));
        }
    }

    #[test]
    fn only_a_user_enabled_sandbox_floors_children() -> Result<(), Box<dyn std::error::Error>> {
        let project = ProjectId::from_bytes([1; 16]);
        let mut record: AttemptRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1, "key":crate::WorkKey::new(project, "sandbox-test", "lane", 1)?,
            "attempt_id":AttemptId::from_bytes([2; 16]), "project_id":project,
            "attempt_number":1, "revision":1, "generation":1,
            "requested_route":{"kind":"named","name":"fixture"},
            "authority_digest":Digest::from_bytes([3; 32]), "queued_at":WallTime::from_micros(1)?,
            "state":{"state":"queued"}, "usage":{}, "trace":{},
            "read_only_writable":[], "protected_paths":[],
            "prompt":{"path":{"encoding":"utf8","path":"prompt"},
                "digest":CanonicalV1::file_digest(b"fixture"),"bytes":7,"complete":true,"redacted":false}
        }))?;
        assert!(!record.floors_children());
        let mut floors = |source: SandboxSourceReceipt, requested: bool, effective: bool| {
            record.applied_route = Some(RouteRecord {
                sandbox: Some(SandboxModeReceipt {
                    requested,
                    effective,
                    source,
                    warning: None,
                    extra: Default::default(),
                }),
                ..Default::default()
            });
            record.floors_children()
        };
        for (source, parent_derived) in [
            (SandboxSourceReceipt::Route, true),
            (SandboxSourceReceipt::Parent, true),
            (SandboxSourceReceipt::Knob, false),
            (SandboxSourceReceipt::Platform, false),
            (SandboxSourceReceipt::Restriction, false),
            (SandboxSourceReceipt::Default, false),
        ] {
            for requested in [false, true] {
                for effective in [false, true] {
                    let expected = effective && (requested || parent_derived);
                    assert_eq!(floors(source, requested, effective), expected);
                }
            }
        }
        assert!(!floors(SandboxSourceReceipt::Default, false, false));
        Ok(())
    }

    #[test]
    fn preparation_plan_diagnostics_use_closed_facts_after_round_trip()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut skipped: SkippedRoute = serde_json::from_value(serde_json::json!({
            "route": "fixture", "kind": "refused", "detail": "SECRET_SENTINEL",
            "plan_error": "protected_overlap", "guard_boundary": "authority",
            "cause": "confinement_unavailable", "confinement_guard": "protected_path_overlap"
        }))?;
        skipped = serde_json::from_slice(&serde_json::to_vec(&skipped)?)?;
        assert_eq!(
            skipped.guard_boundary,
            Some(crate::diagnostic::BoundaryClass::Authority)
        );
        assert_eq!(
            skipped.diagnostic().detail.as_deref(),
            Some(crate::diagnostic::PlanFailure::ProtectedOverlap.detail())
        );
        let mut malformed = serde_json::to_value(&skipped)?;
        malformed["plan_error"] = serde_json::json!("SECRET_SENTINEL");
        assert!(serde_json::from_value::<SkippedRoute>(malformed).is_err());
        assert!(!serde_json::to_string(&skipped.diagnostic())?.contains("SECRET_SENTINEL"));
        Ok(())
    }

    #[test]
    fn work_context_pr_and_lens_round_trip_and_reject_wrong_types()
    -> Result<(), Box<dyn std::error::Error>> {
        let value = serde_json::json!({"pr":3078,"lens":"architecture","future":true});
        let context: WorkContext = serde_json::from_value(value.clone())?;
        assert_eq!(
            (context.pr, context.lens.as_deref()),
            (Some(3078), Some("architecture"))
        );
        assert_eq!(serde_json::to_value(&context)?, value);
        for malformed in [
            serde_json::json!({"pr":"3078"}),
            serde_json::json!({"lens":7}),
        ] {
            assert!(serde_json::from_value::<WorkContext>(malformed).is_err());
        }
        Ok(())
    }

    #[test]
    fn requested_route_required_family_round_trips_and_rejects_wrong_type()
    -> Result<(), Box<dyn std::error::Error>> {
        let value = serde_json::json!({"kind":"named","name":"fixture","effort":null,
            "required_family":"claude","future":true});
        let route: RequestedRoute = serde_json::from_value(value.clone())?;
        assert_eq!(route.required_family(), Some("claude"));
        assert_eq!(serde_json::to_value(&route)?, value);
        let malformed = serde_json::json!({"kind":"named","name":"fixture","effort":null,
            "required_family":7});
        assert!(serde_json::from_value::<RequestedRoute>(malformed).is_err());
        Ok(())
    }

    #[test]
    fn peer_graph_route_required_family_round_trips_and_rejects_wrong_type()
    -> Result<(), Box<dyn std::error::Error>> {
        let value = serde_json::json!({
            "adapter":"codex","required_family":"openai","future":true
        });
        let route: PeerGraphRoute = serde_json::from_value(value.clone())?;
        assert_eq!(route.required_family.as_deref(), Some("openai"));
        let encoded = serde_json::to_value(&route)?;
        let decoded: PeerGraphRoute = serde_json::from_value(encoded.clone())?;
        assert_eq!(decoded, route);
        assert_eq!(encoded["required_family"], "openai");
        assert_eq!(encoded["future"], true);
        assert!(
            serde_json::from_value::<PeerGraphRoute>(serde_json::json!({
                "required_family":7
            }))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn peer_output_planner_fallback_round_trips_and_rejects_wrong_type()
    -> Result<(), Box<dyn std::error::Error>> {
        // A flattened fragment of its owning record, which keeps the future fields.
        let value = serde_json::json!({"planner_fallback":"planner failed"});
        let peer: PeerOutputFields = serde_json::from_value(value.clone())?;
        assert_eq!(peer.planner_fallback.as_deref(), Some("planner failed"));
        assert_eq!(serde_json::to_value(&peer)?, value);
        assert!(
            serde_json::from_value::<PeerOutputFields>(serde_json::json!({
                "planner_fallback":{"reason":"wrong shape"}
            }))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn route_metadata_fields_round_trip_and_reject_wrong_types()
    -> Result<(), Box<dyn std::error::Error>> {
        let cases = [
            ("account_source", serde_json::json!("continuation")),
            ("lane_token", serde_json::json!(true)),
            ("registered_lane_token", serde_json::json!(true)),
            (
                "project_paths",
                serde_json::json!({
                    "shared_paths":[{"path":"./shared","source":"domains.lanes.projects.x.shared_paths[0]"}],
                    "exec_roots":[],"warnings":[],"future_nested":{"kept":true}
                }),
            ),
            (
                "argv_prompt_limit",
                serde_json::json!({
                    "value":65536,"key":"domains.run.argv_prompt_limit_bytes","origin":{"kind":"compiled"}
                }),
            ),
            (
                "dependency_preparation",
                serde_json::json!({
                    "command":"pnpm install --frozen-lockfile","outcome":"warning","reason":"bounded"
                }),
            ),
            (
                "user_skills",
                serde_json::json!({
                    "mode":"discover","exposed":false,"directory":null,"staged_file_count":0,
                    "warnings":[],"source":{"route_policy":["domains.lanes.user_skills"]}
                }),
            ),
            (
                "cargo_home",
                serde_json::json!({
                    "path":"/home/agent/.cargo","source":"host HOME/.cargo","source_writes":true,
                    "source_writes_source":"compiled default"
                }),
            ),
            (
                "tool_closure",
                serde_json::json!([{
                    "tool":"cargo","outcome":"complete","reason":null,"reasons":[],
                    "unresolved":[],"libraries":[],"already_covered":[]
                }]),
            ),
            (
                "applied_access",
                serde_json::json!({
                    "libraries":[],"staged_content":null,"toolchain_roots":[],"omitted_grants":[]
                }),
            ),
            (
                "discovery_warnings",
                serde_json::json!(["omitted optional grant"]),
            ),
            (
                "github_credential",
                serde_json::json!({
                    "identity":"user","source":"default","delivery":"ambient","detail":"host login",
                    "mediation":"none"
                }),
            ),
            (
                "ci_scope",
                serde_json::json!({"value":"none","source":"default","key":"lanes.ci_scope"}),
            ),
            (
                "sandbox",
                serde_json::json!({
                    "requested":true,"effective":true,"source":"route","warning":null
                }),
            ),
            (
                "sandbox_admission",
                serde_json::json!({
                    "parent_sandboxed":false,"boundary":"none","strength":"warn",
                    "knob":"lanes.sandbox_floor","source":"default"
                }),
            ),
            (
                "write_grant_limit",
                serde_json::json!({
                    "key":"domains.run.write_grant_limit_bytes","value":1048576,"origin":"compiled_default"
                }),
            ),
        ];
        for (field, typed) in cases {
            let value = serde_json::json!({(field): typed, "future_route_field": true});
            let route: RouteRecord = serde_json::from_value(value.clone())?;
            let encoded = serde_json::to_value(&route)?;
            let decoded: RouteRecord = serde_json::from_value(encoded.clone())?;
            assert_eq!(decoded, route, "{field}");
            assert_eq!(encoded["future_route_field"], true, "{field}");
            assert!(!route.extra.contains_key(field), "{field}");
            let malformed = serde_json::json!({(field): "not a receipt"});
            assert!(
                serde_json::from_value::<RouteRecord>(malformed).is_err(),
                "{field}"
            );
        }
        Ok(())
    }

    #[test]
    fn skipped_route_guard_fields_round_trip_and_reject_wrong_types()
    -> Result<(), Box<dyn std::error::Error>> {
        let cases = [
            ("plan_error", serde_json::json!("protected_overlap")),
            ("guard_boundary", serde_json::json!("authority")),
            (
                "guard_resource",
                serde_json::json!({"kind":"processes","used":4,"limit":3,"reserved":0}),
            ),
            (
                "confinement_guard",
                serde_json::json!("protected_path_overlap"),
            ),
        ];
        for (field, typed) in cases {
            let value = serde_json::json!({
                "route":"fixture","kind":"refused","detail":"safe","future_route_field":true,
                (field):typed
            });
            let route: SkippedRoute = serde_json::from_value(value.clone())?;
            assert_eq!(serde_json::to_value(&route)?, value, "{field}");
            assert!(!route.extra.contains_key(field), "{field}");
            let malformed = serde_json::json!({
                "route":"fixture","kind":"refused","detail":"safe",(field):"wrong type"
            });
            assert!(
                serde_json::from_value::<SkippedRoute>(malformed).is_err(),
                "{field}"
            );
        }
        Ok(())
    }

    #[test]
    fn usage_metadata_round_trips_and_rejects_wrong_types() -> Result<(), Box<dyn std::error::Error>>
    {
        let value = serde_json::json!({
            "per_attempt":true,
            "session_total":{"uncached_input":12,"future_nested":true},
            "gh_invocations":{"count":4,"budget":3,"source":"launch","over_budget":true},
            "future":true
        });
        let usage: UsageRecord = serde_json::from_value(value.clone())?;
        assert_eq!(usage.per_attempt, Some(true));
        assert_eq!(
            usage
                .session_total
                .as_deref()
                .and_then(|total| total.uncached_input),
            Some(12)
        );
        assert_eq!(
            usage.gh_invocations.as_ref().map(|receipt| receipt.count),
            Some(4)
        );
        let encoded = serde_json::to_value(&usage)?;
        let decoded: UsageRecord = serde_json::from_value(encoded.clone())?;
        assert_eq!(decoded, usage);
        assert_eq!(encoded["future"], true);
        assert_eq!(encoded["session_total"]["future_nested"], true);
        for malformed in [
            serde_json::json!({"per_attempt":"yes"}),
            serde_json::json!({"session_total":false}),
            serde_json::json!({"gh_invocations":{"count":"4","over_budget":true}}),
        ] {
            assert!(serde_json::from_value::<UsageRecord>(malformed).is_err());
        }
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
    fn typed_profile_fields_round_trip_and_reject_malformed_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = HarnessProfile {
            name: "typed".into(),
            confidential: true,
            parent_sandboxed: true,
            admitted_by: Some("parent".into()),
            knob_sources: [("lanes.ci_scope".into(), LimitSource::Launch)].into(),
            budget_wrap_up: Some(BudgetWrapUp { remaining: 17 }),
            launch_policy: Some(crate::policy::Policy::default()),
            policy_overrides: [("WRITE_SCOPE".into(), crate::policy::Strength::Warn)].into(),
            policy_project: Some(ProjectId::from_bytes([7; 16])),
            repository_policy: Some(LaunchPolicy {
                confidential: true,
                sources: vec!["git".into()],
                repository_config: Some(".provenant.toml".into()),
            }),
            write_scope: vec!["src".into()],
            ..HarnessProfile::default()
        };
        let mut wire = serde_json::to_value(&profile)?;
        assert_eq!(
            serde_json::from_value::<HarnessProfile>(wire.clone())?,
            profile
        );
        for (field, malformed) in [
            ("confidential", serde_json::json!("true")),
            ("parent_sandboxed", serde_json::json!(1)),
            ("admitted_by", serde_json::json!(false)),
            (
                "knob_sources",
                serde_json::json!({"lanes.ci_scope":"unowned"}),
            ),
            ("budget_wrap_up", serde_json::json!({"remaining":"17"})),
            ("launch_policy", serde_json::json!({"rules":"not-a-list"})),
            (
                "policy_overrides",
                serde_json::json!({"WRITE_SCOPE":"refuse-me"}),
            ),
            ("policy_project", serde_json::json!([])),
            ("repository_policy", serde_json::json!("host")),
            ("write_scope", serde_json::json!([1])),
        ] {
            let mut malformed_value = wire.clone();
            malformed_value[field] = malformed;
            assert!(
                serde_json::from_value::<HarnessProfile>(malformed_value).is_err(),
                "{field}"
            );
        }
        wire["unknown_future_profile_field"] = serde_json::json!(true);
        let decoded: HarnessProfile = serde_json::from_value(wire)?;
        assert!(decoded.extra.contains_key("unknown_future_profile_field"));
        Ok(())
    }

    #[test]
    fn task_limit_extensions_round_trip_and_reject_malformed_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let task = TaskRequest {
            context: None,
            extra: Default::default(),
            limit_extensions: [("token_budget".into(), 5000)].into(),
            build_heavy: false,
            title: None,
            profile: HarnessProfile::default(),
            prompt_digest: CanonicalV1::file_digest(b"typed request"),
            head: None,
            source_head: None,
            base: None,
            base_ref: None,
            route: RequestedRoute::Named {
                extra: Default::default(),
                required_family: None,
                name: "junior".into(),
                effort: None,
            },
            account: None,
            authority_digest: Digest::from_bytes([3; 32]),
            writer: None,
            deliver: None,
            provider_timeout_seconds: 60,
            delivery_id: None,
            access: LaneAccess::default(),
        };
        let mut wire = serde_json::to_value(&task)?;
        assert_eq!(serde_json::from_value::<TaskRequest>(wire.clone())?, task);
        wire["limit_extensions"] = serde_json::json!({"token_budget":"5000"});
        assert!(serde_json::from_value::<TaskRequest>(wire).is_err());
        Ok(())
    }

    #[test]
    fn writer_custody_project_round_trips_and_rejects_malformed_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = WriterRequest {
            extra: Default::default(),
            custody_project: Some(ProjectId::from_bytes([9; 16])),
            project: "/project".into(),
            worktree: "/project/.worktrees/lane".into(),
        };
        let mut wire = serde_json::to_value(&request)?;
        assert_eq!(
            serde_json::from_value::<WriterRequest>(wire.clone())?,
            request
        );
        wire["custody_project"] = serde_json::json!({"id": "invalid"});
        assert!(serde_json::from_value::<WriterRequest>(wire).is_err());
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
                SettlementReason::derive(Outcome::Lost, Some(&evidence), &[], None, Ok(None)),
                SettlementReason::Lost
            );
            assert_eq!(
                SettlementReason::derive(Outcome::Unsettled, Some(&evidence), &[], None, Ok(None)),
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
    fn default_access_denies_database_ipc_and_explicit_access_round_trips() {
        let defaults = LaneAccess::default();
        assert!(!defaults.database_ipc);
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
    #[test]
    fn lane_wait_fields_round_trip_and_reject_malformed_values()
    -> Result<(), Box<dyn std::error::Error>> {
        for reason in [
            LaneWaitReason::HostStalled,
            LaneWaitReason::Children,
            LaneWaitReason::Admission,
            LaneWaitReason::ProviderSilence {
                silent_for_ms: 5_000,
            },
            LaneWaitReason::LaunchPreparation,
            LaneWaitReason::RecoveryWorkerRefused { used: 5, limit: 10 },
        ] {
            let wait = LaneWait {
                extra: Default::default(),
                attempt_id: AttemptId::from_bytes([1; 16]),
                lane_class: LaneClass::Writer,
                waiting_for: reason,
            };
            let encoded = serde_json::to_vec(&wait)?;
            let decoded: LaneWait =
                crate::json::decode(encoded.as_slice(), crate::json::Limits::default())?;
            assert_eq!(decoded, wait);
        }
        let malformed = serde_json::to_vec(&serde_json::json!({
            "attempt_id":AttemptId::from_bytes([1; 16]), "lane_class":"writer",
            "waiting_for":{"kind":"provider_silence", "silent_for_ms":"many"}
        }))?;
        assert!(matches!(
            crate::json::decode::<LaneWait, _>(
                malformed.as_slice(),
                crate::json::Limits::default()
            ),
            Err(crate::json::DecodeError::Typed { .. })
        ));
        Ok(())
    }

    #[test]
    fn admission_binding_fields_round_trip_and_reject_malformed_values() {
        let command = crate::PortCommand {
            argv: vec!["admission-check".into()],
            exit_codes: crate::CommandExitCodes {
                success: vec![0],
                unknown: vec![],
                other: crate::ExitDisposition::Refused,
            },
        };
        let binding = RepositoryAdmissionBinding::new(crate::PortSource::Project, &command);
        let wire = serde_json::to_value(&binding).unwrap();
        let bytes = serde_json::to_vec(&wire).unwrap();
        let decoded: RepositoryAdmissionBinding =
            crate::json::decode(bytes.as_slice(), crate::json::Limits::default()).unwrap();
        assert_eq!(decoded, binding);
        for (field, invalid) in [
            ("source", serde_json::json!("unknown")),
            (
                "command",
                serde_json::json!({"argv":[],"exit_codes":{"success":[0],"unknown":[],"other":"refused"}}),
            ),
            (
                "command_digest",
                serde_json::json!(CanonicalV1::file_digest(b"wrong command")),
            ),
            ("lease_epoch", serde_json::json!(0)),
            ("phase", serde_json::json!("unknown")),
        ] {
            let mut malformed = wire.clone();
            malformed[field] = invalid;
            let bytes = serde_json::to_vec(&malformed).unwrap();
            assert!(
                matches!(
                    crate::json::decode::<RepositoryAdmissionBinding, _>(
                        bytes.as_slice(),
                        crate::json::Limits::default()
                    ),
                    Err(crate::json::DecodeError::Typed { .. })
                ),
                "{field}"
            );
        }
    }

    #[test]
    fn attempt_host_fields_round_trip_and_reject_malformed_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = ProjectId::from_bytes([1; 16]);
        let base = serde_json::json!({
            "schema_version":1, "key":crate::WorkKey::new(project, "typed-run-fields", "lane", 1)?,
            "attempt_id":AttemptId::from_bytes([2; 16]), "project_id":project,
            "attempt_number":1, "revision":1, "generation":1,
            "requested_route":{"kind":"named","name":"fixture"},
            "authority_digest":Digest::from_bytes([3; 32]), "queued_at":WallTime::from_micros(1)?,
            "state":{"state":"queued"}, "usage":{}, "trace":{},
            "read_only_writable":[], "protected_paths":[],
            "prompt":{"path":{"encoding":"utf8","path":"prompt"},
                "digest":CanonicalV1::file_digest(b"fixture"),"bytes":7,"complete":true,"redacted":false}
        });
        let command: crate::PortCommand = serde_json::from_value(serde_json::json!({
            "argv":["admission-check"],
            "exit_codes":{"success":[0],"unknown":[],"other":"refused"}
        }))?;
        let binding = RepositoryAdmissionBinding::new(crate::PortSource::Project, &command);
        let field_values = [
            (
                "budget_overrun",
                serde_json::json!({"schema_version":1,"tokens":101,"budget":100}),
                serde_json::json!(false),
            ),
            (
                "budget_wrap_up_refused",
                serde_json::json!({"schema_version":1,"reason":"admission refused"}),
                serde_json::json!(false),
            ),
            (
                "repository_admission_binding",
                serde_json::to_value(binding)?,
                serde_json::json!(false),
            ),
            (
                "peer_graph",
                serde_json::json!({"host":provenant_core::HostId::from_bytes([4;16]),"graph_id":"peer-graph"}),
                serde_json::json!(false),
            ),
            (
                "peer_graph_collected",
                serde_json::json!(true),
                serde_json::json!("yes"),
            ),
            (
                "peer_origin_work_key",
                serde_json::to_value(crate::WorkKey::new(project, "origin", "parent", 1)?)?,
                serde_json::json!(false),
            ),
            (
                "peer_origin_context",
                serde_json::json!({"graph_id":"origin","node_id":"parent","attempt":1}),
                serde_json::json!(false),
            ),
            (
                "peer_subtree",
                serde_json::json!({
                    "status":"complete","outcome":"succeeded","next_node":null,
                    "checkpoint":{"schema_version":1,"graph_id":"peer-graph","plan_digest":"digest","cancelled":false,"nodes":[
                        {"id":"node","state":"succeeded","worktree":null,"attempts":[{
                            "launched":true,"key":{"graph_id":"peer-graph","node_id":"node","attempt":1,"route_index":0},
                            "route":{"adapter":"fixture","model":"fixture/model","alias":null,"name":null,"effort":null,"account":null},
                            "state":"terminal","attempt_ref":"host/attempt","outcome":"succeeded","launch_actor":"actor"
                        }]}
                    ]}
                }),
                serde_json::json!({"status":"complete","checkpoint":{"nodes":[{"state":"unknown"}]}}),
            ),
            (
                "graph_custody_released",
                serde_json::json!(true),
                serde_json::json!("yes"),
            ),
            (
                "graph_handoff",
                serde_json::json!({"Ok":{"schema_version":2,"status":"done","note":"finished","remaining":[],"decisions":[],"verify":[]}}),
                serde_json::json!({"Ok":{"schema_version":1,"status":"done","note":"finished","remaining":[],"decisions":[],"verify":[]}}),
            ),
        ];
        for (field, value, malformed_value) in field_values {
            let mut input = base.clone();
            input[field] = value.clone();
            let decoded: AttemptRecord = serde_json::from_value(input)?;
            let round_trip = serde_json::to_value(&decoded)?;
            assert_eq!(round_trip[field], value, "{field}");
            assert!(
                !decoded.extra.contains_key(field),
                "typed host field leaked into ExtraFields: {field}"
            );
            if field == "repository_admission_binding" {
                assert!(
                    decoded
                        .repository_admission_binding
                        .as_ref()
                        .is_some_and(RepositoryAdmissionBinding::valid)
                );
            }
            let mut malformed = base.clone();
            malformed[field] = malformed_value;
            let bytes = serde_json::to_vec(&malformed)?;
            assert!(
                matches!(
                    crate::json::decode::<AttemptRecord, _>(
                        bytes.as_slice(),
                        crate::json::Limits::default()
                    ),
                    Err(crate::json::DecodeError::Typed { .. })
                ),
                "{field}"
            );
        }
        let mut oversized = base.clone();
        oversized["graph_handoff"] = serde_json::json!({"Ok":{
            "schema_version":2,"status":"done","note":"finished","remaining":[],"decisions":[],"verify":[],
            "future_payload":"x".repeat(13_000)
        }});
        let oversized_bytes = serde_json::to_vec(&oversized)?;
        assert!(matches!(
            crate::json::decode::<AttemptRecord, _>(
                oversized_bytes.as_slice(),
                crate::json::Limits::default()
            ),
            Err(crate::json::DecodeError::Typed { .. })
        ));
        let mut future = base;
        future["future_user_field"] = serde_json::json!({"kept":true});
        let decoded: AttemptRecord = serde_json::from_value(future)?;
        assert_eq!(
            decoded.extra.get("future_user_field"),
            Some(&serde_json::json!({"kept":true}))
        );
        Ok(())
    }
}
