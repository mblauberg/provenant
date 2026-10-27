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
    pub route: provenant_records::run::RequestedRoute,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub delivery_id: Option<DeliveryId>,
    #[serde(default)]
    pub deliver: Option<provenant_records::run::WriterDeliveryRequest>,
    /// A writer worktree the receiving peer creates in its own clone, in place
    /// of a `worktree` path.
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
        Submit {
            operation_id: OperationId,
            tasks: Vec<SubmittedTask>,
            #[serde(default)]
            concurrency: Option<u16>,
            #[serde(default)]
            knobs: Vec<String>,
            /// Explicit peer placement; read-only tasks only.
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
            wait_seconds: u64,
        },
        Cancel { target: AttemptTarget, operation_id: OperationId },
        Resume {
            target: AttemptTarget,
            operation_id: OperationId,
            prompt: String,
            expected_generation: u64,
            #[serde(default)]
            no_fallback: bool,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputPart {
    Prompt,
    Result,
    Stderr,
    Events,
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
        Hosts {
            #[serde(default)]
            after: Option<HostId>,
            limit: u16,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum FacadeError {
    #[error("run execution owner is unavailable")]
    OwnerUnavailable,
    #[error("invalid input")]
    InvalidInput,
    #[error("record not found")]
    NotFound,
    #[error("request conflicts with current state")]
    Conflict,
    #[error("attempt is not terminal")]
    NotTerminal,
    #[error("operation ID was reused with different input")]
    OperationConflict,
    #[error("service is busy")]
    Busy,
    #[error("ownership fence is stale")]
    StaleFence,
    #[error("home host is unavailable")]
    HomeUnavailable,
    #[error("peer protocol version is incompatible")]
    PeerVersion,
    #[error("peer {peer} speaks protocol {remote}, this host speaks {protocol}; upgrade both", protocol = crate::hosts::PROTOCOL)]
    PeerProtocolMismatch { peer: String, remote: u64 },
    #[error("run launch outcome is unknown for operation {operation_id}")]
    LaunchUnknown { operation_id: OperationId },
    #[error("run continuation is unsupported")]
    ContinuationUnsupported,
    #[error("provider state expired; use the retained result or transcripts as context")]
    ProviderStateExpired,
    #[error("lane depth exceeds lanes.max_depth")]
    LaneDepth,
    #[error("coordination mutations are not allowed inside a lane")]
    LaneCoordinationRefused,
    #[error("request violates confinement")]
    ConfinementRefused,
    #[error("{reason}")]
    DispatchRefused { reason: String },
    #[error("local worktree does not exist; create it with `provenant worktree create`")]
    WorktreeMissing,
    #[error(
        "worktree {path:?} is not registered with Provenant; create a writer with `provenant worktree create <new-branch> --base <branch>`"
    )]
    WorktreeUnregistered { path: String },
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
    #[error("I/O operation {operation:?} failed at {path:?}")]
    Io {
        operation: String,
        path: Option<String>,
    },
}

impl FacadeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::OwnerUnavailable => "owner_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::NotTerminal => "not_terminal",
            Self::OperationConflict => "operation_conflict",
            Self::Busy => "busy",
            Self::StaleFence => "stale_fence",
            Self::HomeUnavailable => "home_unavailable",
            Self::PeerVersion | Self::PeerProtocolMismatch { .. } => "peer_version",
            Self::LaunchUnknown { .. } => "launch_unknown",
            Self::ContinuationUnsupported => "continuation_unsupported",
            Self::ProviderStateExpired => "provider_state_expired",
            Self::LaneDepth => "lane_depth",
            Self::LaneCoordinationRefused => "lane_coordination_refused",
            Self::ConfinementRefused => "confinement_refused",
            Self::DispatchRefused { .. } => "dispatch_refused",
            Self::WorktreeMissing => "worktree_missing",
            Self::WorktreeUnregistered { .. } => "worktree_unregistered",
            Self::BaseUnreachable => "base_unreachable",
            Self::BranchExists => "branch_exists",
            Self::OutputTooLarge => "output_too_large",
            Self::StateCorrupt => "state_corrupt",
            Self::UpgradeRefused => "upgrade_refused",
            Self::PublicationFailed => "publication_failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Io { .. } => "io",
        }
    }
}

impl FacadeError {
    pub fn io(operation: impl Into<String>) -> Self {
        Self::Io {
            operation: operation.into(),
            path: None,
        }
    }

    pub fn filesystem(error: &provenant_os::fs::FsError) -> Self {
        Self::filesystem_anchored(error, None)
    }

    fn filesystem_anchored(
        error: &provenant_os::fs::FsError,
        anchor: Option<&std::path::Path>,
    ) -> Self {
        use provenant_os::fs::FsError;
        let (operation, path) = match error {
            FsError::Io {
                operation, path, ..
            } => (*operation, Some(path.clone())),
            FsError::ReadIo { path, .. } => ("read file", Some(path.clone())),
            FsError::Rename { to_path, .. } => ("rename", Some(to_path.clone())),
            FsError::Symlink { path, .. } => ("resolve path", Some(path.clone())),
            FsError::CrossDevice { path } => ("verify filesystem device", Some(path.clone())),
            FsError::LockTimeout { path, .. } | FsError::Cancelled { path } => {
                ("acquire lock", Some(path.clone()))
            }
            FsError::LockDeadlineOverflow { path } => ("set lock deadline", Some(path.clone())),
            _ => ("verify filesystem entry", None),
        };
        Self::Io {
            operation: operation.into(),
            path: path
                .map(|path| anchor.map_or_else(|| path.clone(), |anchor| anchor.join(&path)))
                .filter(|path| path.is_absolute())
                .map(|path| path.to_string_lossy().into_owned()),
        }
    }
}

impl From<&crate::hosts::PeerError> for FacadeError {
    fn from(error: &crate::hosts::PeerError) -> Self {
        use crate::hosts::PeerError;
        match error {
            PeerError::Version => Self::PeerVersion,
            PeerError::ProtocolMismatch { peer, remote } => Self::PeerProtocolMismatch {
                peer: peer.clone(),
                remote: *remote,
            },
            PeerError::Project | PeerError::SourceHost | PeerError::RequestTooLarge => {
                Self::InvalidInput
            }
            PeerError::Unavailable | PeerError::Failed(_) => Self::HomeUnavailable,
            PeerError::Unknown { operation_id } => Self::LaunchUnknown {
                operation_id: operation_id.clone(),
            },
            PeerError::Cancelled => Self::Cancelled,
            PeerError::Frame => Self::StateCorrupt,
            PeerError::Refused { error } => error.clone(),
        }
    }
}

impl From<&super::CoordinationError> for FacadeError {
    fn from(error: &super::CoordinationError) -> Self {
        use super::CoordinationError;
        match error {
            CoordinationError::LaneMutation => Self::LaneCoordinationRefused,
            CoordinationError::Peer(error) => Self::from(error),
            CoordinationError::InvalidInput(_) | CoordinationError::Input(_) => Self::InvalidInput,
            CoordinationError::Time(_) | CoordinationError::ResponseEncoding(_) => {
                Self::StateCorrupt
            }
            CoordinationError::Cancelled => Self::Cancelled,
            CoordinationError::Timeout => Self::Timeout,
            CoordinationError::Busy => Self::Busy,
            CoordinationError::Entropy(_) | CoordinationError::EntropyEncoding => {
                Self::io("generate entropy")
            }
            CoordinationError::UpgradeRefused(_) => Self::UpgradeRefused,
            CoordinationError::Store(error) => Self::from(error),
        }
    }
}

impl From<&provenant_fabric_store::Error> for FacadeError {
    fn from(error: &provenant_fabric_store::Error) -> Self {
        use provenant_fabric_store::Error;
        match error {
            Error::NotFound => Self::NotFound,
            Error::StaleFence => Self::StaleFence,
            Error::Conflict
            | Error::PushUnresolved { .. }
            | Error::PushTransition { .. }
            | Error::Overlap { .. } => Self::Conflict,
            Error::Busy => Self::Busy,
            Error::Cancelled => Self::Cancelled,
            Error::Invalid(_) | Error::PathEncoding => Self::InvalidInput,
            Error::Decode(_)
            | Error::Encode(_)
            | Error::SchemaVersion(_)
            | Error::UnversionedDatabase
            | Error::JournalMode(_)
            | Error::Time(_) => Self::StateCorrupt,
            Error::LegacyImportData { .. } => Self::UpgradeRefused,
            // checked_directory's device refusal names only the child component.
            Error::Filesystem(provenant_os::fs::FsError::CrossDevice { .. }) => {
                Self::io("verify Fabric filesystem device")
            }
            Error::Filesystem(error) => {
                // Fabric's held directories are traversed from `/`.
                Self::filesystem_anchored(error, Some(std::path::Path::new("/")))
            }
            Error::Io { path, .. } => Self::Io {
                operation: "access Fabric store".into(),
                path: Some(path.to_string_lossy().into_owned()),
            },
            Error::Sqlite(_) => Self::io("access Fabric database"),
            Error::UnsafePath(path) => Self::Io {
                operation: "verify Fabric store path".into(),
                path: Some(path.to_string_lossy().into_owned()),
            },
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::FacadeError;

    #[test]
    fn resume_preserves_no_fallback_and_defaults_for_older_senders()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut input = serde_json::json!({
            "op": "resume",
            "target": { "attempt": "00000000000000000000000000000001" },
            "operation_id": "resume-fixture",
            "prompt": "Continue",
            "expected_generation": 0
        });
        let legacy: super::RunInput = serde_json::from_value(input.clone())?;
        assert!(matches!(
            legacy,
            super::RunInput::Resume {
                no_fallback: false,
                ..
            }
        ));
        input["no_fallback"] = true.into();
        let explicit: super::RunInput = serde_json::from_value(input)?;
        assert!(matches!(
            explicit,
            super::RunInput::Resume {
                no_fallback: true,
                ..
            }
        ));
        assert_eq!(serde_json::to_value(explicit)?["no_fallback"], true);
        Ok(())
    }

    #[test]
    fn filesystem_diagnostic_omits_paths_with_unknown_anchors() {
        let error = provenant_os::fs::FsError::Io {
            operation: "openat read",
            path: "HEAD".into(),
            source: std::io::ErrorKind::NotFound.into(),
        };
        assert_eq!(
            FacadeError::filesystem(&error),
            FacadeError::Io {
                operation: "openat read".into(),
                path: None,
            }
        );
    }

    #[test]
    fn io_operation_display_escapes_controls() {
        let error = FacadeError::io("read\n\u{1b}[31m\"peer");
        let display = error.to_string();
        assert!(!display.contains('\n'), "{display:?}");
        assert!(!display.contains('\u{1b}'), "{display:?}");
        assert!(display.contains("\\n"), "{display:?}");
        assert!(display.contains("\\\"peer"), "{display:?}");
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
    Base64(String),
}

impl OutputBytes {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self::Utf8(text),
            Err(error) => Self::Base64(crate::handoff::encode_base64(&error.into_bytes())),
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
        /// The attempt's state name, so a reader can tell a clean success
        /// from an outcome that needs the chair.
        state: String,
        /// Why a terminal attempt ended as it did, when it recorded a reason.
        reason: Option<String>,
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
