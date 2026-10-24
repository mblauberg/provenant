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
    pub access: crate::graph::AccessMode,
    #[serde(default)]
    pub worktree: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub inputs: Vec<ArtifactRef>,
    pub route: RoutePreference,
    pub timeout_ms: u64,
    #[serde(default)]
    pub delivery_id: Option<DeliveryId>,
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
            concurrency: u16,
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
    Combined,
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
    #[error("request violates confinement")]
    ConfinementRefused,
    #[error("stored state is corrupt")]
    StateCorrupt,
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
            Self::ConfinementRefused => "confinement_refused",
            Self::StateCorrupt => "state_corrupt",
            Self::PublicationFailed => "publication_failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Io => "io",
        }
    }
}

/// The F3-owned side of the service seam. Each associated response is a
/// concrete serde record selected by the future run facade. Every method must
/// return within its budget and observe cancellation cooperatively. Cancelling
/// an observation must never stop a launched run; only explicit run cancel may.
/// The shared records do not yet expose run requests, so F3 consumes these
/// contracts directly until those owner types land.
#[derive(Debug, Default, Clone, Serialize)]
pub struct EventCoverage {
    pub unknown_hosts: Vec<HostId>,
    pub more_events: bool,
}
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
