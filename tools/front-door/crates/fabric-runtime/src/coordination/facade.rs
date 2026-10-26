//! Typed run/identity seam for F3. Runtime implements this trait; MCP consumes
//! it without a reverse dependency. F9 owns production composition.
use super::input::bounded_input;
use provenant_core::{AttemptId, CancelToken, DeliveryId, Digest, HostId, OperationId, WaitBudget};
use provenant_records::ArtifactRef;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An MCP run target is an attempt ID, optionally qualified by its owning host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptTarget {
    pub attempt: AttemptId,
    #[serde(default)]
    pub host: Option<HostId>,
}

/// A closed set of route preferences accepted for a submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePreference {
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub task_class: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

/// One task in an MCP submission. Project, actor, authority and cwd come from
/// the service's authenticated identity rather than request supplied paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmittedTask {
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub access: crate::graph::AccessMode,
    #[serde(default)]
    pub worktree: Option<String>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub route: RoutePreference,
    pub timeout_ms: u64,
    #[serde(default)]
    pub delivery_id: Option<DeliveryId>,
    #[serde(default)]
    pub deliver: Option<provenant_records::run::WriterDeliveryRequest>,
    /// A writer worktree the receiving peer creates in its own clone, in place
    /// of a `worktree` path. Omitted when absent, so read-only requests stay
    /// readable by a peer that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_worktree: Option<NewWorktree>,
}

/// A new branch at `base`, which the peer resolves after fetching its remotes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewWorktree {
    pub base: String,
    pub branch: String,
}

bounded_input! {
    pub enum IdentityInput as WireIdentityInput {
        Inspect {},
    }
}

bounded_input! {
    pub enum SubmitInput as WireSubmitInput {
        Submission {
            operation_id: OperationId,
            tasks: Vec<SubmittedTask>,
            #[serde(default)]
            concurrency: Option<u16>,
            #[serde(default)]
            knobs: Vec<String>,
            /// Explicit peer placement; read-only tasks only. Omitted when
            /// absent, so a peer that predates the field still accepts it.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            host: Option<HostId>,
        },
    }
}

bounded_input! {
    pub enum GraphInput as WireGraphInput {
        Run { document: std::path::PathBuf },
        Status { id: String },
        Cancel { id: String },
        Resume { document: std::path::PathBuf },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitCondition {
    Any,
    All,
}

bounded_input! {
    pub enum RunInput as WireRunInput {
        List {
            #[serde(default)]
            after: Option<AttemptTarget>,
            limit: u16,
        },
        Status { target: AttemptTarget },
        Wait {
            targets: Vec<AttemptTarget>,
            condition: WaitCondition,
            wait_ms: u64,
        },
        Cancel { target: AttemptTarget, operation_id: OperationId },
        Resume {
            target: AttemptTarget,
            operation_id: OperationId,
            prompt: String,
            expected_generation: u64,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputPart {
    Stdout,
    Stderr,
}

bounded_input! {
    pub enum OutputInput as WireOutputInput {
        Read {
            target: AttemptTarget,
            part: OutputPart,
            offset: u64,
            max_bytes: u64,
        },
        Fetch {
            target: AttemptTarget,
            artifact: ArtifactRef,
            max_bytes: u64,
        },
    }
}

bounded_input! {
    pub enum HostsInput as WireHostsInput {
        List {
            #[serde(default)]
            after: Option<HostId>,
            limit: u16,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FacadeError {
    #[error("run execution owner is unavailable")]
    OwnerUnavailable,
    #[error("invalid input")]
    InvalidInput,
    #[error("record not found")]
    NotFound,
    #[error("request conflicts with current state")]
    Conflict,
    #[error("service is busy")]
    Busy,
    #[error("ownership fence is stale")]
    StaleFence,
    #[error("home host is unavailable")]
    HomeUnavailable,
    #[error("peer protocol version is incompatible")]
    PeerVersion,
    #[error("run launch outcome is unknown for operation {operation_id}")]
    LaunchUnknown { operation_id: OperationId },
    #[error("run continuation is unsupported")]
    ContinuationUnsupported,
    #[error("lane depth exceeds lanes.max_depth")]
    LaneDepth,
    #[error("coordination mutations are not allowed inside a lane")]
    LaneCoordinationRefused,
    #[error("request violates confinement")]
    ConfinementRefused,
    #[error("local worktree does not exist; create it with `provenant worktree create`")]
    WorktreeMissing,
    #[error(
        "writer base is not reachable from the peer's remotes after a fetch; push it, or check the peer's Git access"
    )]
    BaseUnreachable,
    #[error("writer branch already exists on the peer; choose another --branch")]
    BranchExists,
    #[error("artifact exceeds max_bytes; page it with an output read")]
    OutputTooLarge,
    #[error("stored state is corrupt")]
    StateCorrupt,
    #[error("Fabric store upgrade was refused")]
    UpgradeRefused,
    #[error("output publication failed")]
    PublicationFailed,
    #[error("request was cancelled")]
    Cancelled,
    #[error("wait timed out")]
    Timeout,
    #[error("I/O operation failed")]
    Io,
}

impl FacadeError {
    /// Decode a stable peer error code, retaining an operation ID for an
    /// uncertain launch result. Unknown codes fail closed as unavailable.
    pub fn from_code(code: &str, operation_id: Option<OperationId>) -> Self {
        match (code, operation_id) {
            ("owner_unavailable", _) => Self::OwnerUnavailable,
            ("invalid_input", _) => Self::InvalidInput,
            ("not_found", _) => Self::NotFound,
            ("conflict", _) => Self::Conflict,
            ("busy", _) => Self::Busy,
            ("stale_fence", _) => Self::StaleFence,
            ("home_unavailable", _) => Self::HomeUnavailable,
            ("peer_version", _) => Self::PeerVersion,
            ("launch_unknown", Some(operation_id)) => Self::LaunchUnknown { operation_id },
            ("continuation_unsupported", _) => Self::ContinuationUnsupported,
            ("confinement_refused", _) => Self::ConfinementRefused,
            ("lane_depth", _) => Self::LaneDepth,
            ("lane_coordination_refused", _) => Self::LaneCoordinationRefused,
            ("worktree_missing", _) => Self::WorktreeMissing,
            ("base_unreachable", _) => Self::BaseUnreachable,
            ("branch_exists", _) => Self::BranchExists,
            ("output_too_large", _) => Self::OutputTooLarge,
            ("state_corrupt", _) => Self::StateCorrupt,
            ("upgrade_refused", _) => Self::UpgradeRefused,
            ("publication_failed", _) => Self::PublicationFailed,
            ("cancelled", _) => Self::Cancelled,
            ("timeout", _) => Self::Timeout,
            ("io", _) => Self::Io,
            _ => Self::HomeUnavailable,
        }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::OwnerUnavailable => "owner_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
            Self::StaleFence => "stale_fence",
            Self::HomeUnavailable => "home_unavailable",
            Self::PeerVersion => "peer_version",
            Self::LaunchUnknown { .. } => "launch_unknown",
            Self::ContinuationUnsupported => "continuation_unsupported",
            Self::LaneDepth => "lane_depth",
            Self::LaneCoordinationRefused => "lane_coordination_refused",
            Self::ConfinementRefused => "confinement_refused",
            Self::WorktreeMissing => "worktree_missing",
            Self::BaseUnreachable => "base_unreachable",
            Self::BranchExists => "branch_exists",
            Self::OutputTooLarge => "output_too_large",
            Self::StateCorrupt => "state_corrupt",
            Self::UpgradeRefused => "upgrade_refused",
            Self::PublicationFailed => "publication_failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Io => "io",
        }
    }
}

/// Coverage information returned when refreshing events from peer hosts.
#[derive(Debug, Default, Clone, Serialize)]
pub struct EventCoverage {
    pub unknown_hosts: Vec<HostId>,
    pub more_events: bool,
}

/// Typed runtime seam shared by the F3 facade and MCP. The run owner is
/// composed in `src/fabric.rs`; calls are bounded and cancellable, while only
/// the explicit run-cancel operation can stop a launched run.
pub trait F3Facade {
    fn refresh_events(
        &mut self,
        _budget: WaitBudget,
        _cancel: &CancelToken,
    ) -> Result<EventCoverage, FacadeError> {
        Ok(EventCoverage::default())
    }
    type Identity: Serialize;
    type Submit: Serialize;
    type Run: Serialize;
    type Hosts: Serialize;

    fn identity(
        &mut self,
        input: IdentityInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self::Identity, FacadeError>;
    fn submit(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self::Submit, FacadeError>;
    fn run(
        &mut self,
        input: RunInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self::Run, FacadeError>;
    fn graph(
        &mut self,
        _input: GraphInput,
        _budget: WaitBudget,
        _cancel: &CancelToken,
    ) -> Result<crate::graph::GraphRun, FacadeError> {
        Err(FacadeError::OwnerUnavailable)
    }
    fn output(
        &mut self,
        input: OutputInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<OutputResult, FacadeError>;
    fn hosts(
        &mut self,
        input: HostsInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self::Hosts, FacadeError>;
}

/// Output payload encoding is explicit and lossless across MCP JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "data", rename_all = "snake_case")]
pub enum OutputBytes {
    Utf8(String),
    Bytes(Vec<u8>),
}

impl OutputBytes {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self::Utf8(text),
            Err(error) => Self::Bytes(error.into_bytes()),
        }
    }
}

/// Typed, bounded output response produced by F3's run facade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputResult {
    Chunk {
        target: AttemptTarget,
        part: OutputPart,
        offset: u64,
        next_offset: u64,
        eof: bool,
        digest: Digest,
        bytes: OutputBytes,
    },
    Artifact {
        target: AttemptTarget,
        artifact: ArtifactRef,
        digest: Digest,
        bytes: OutputBytes,
    },
}
