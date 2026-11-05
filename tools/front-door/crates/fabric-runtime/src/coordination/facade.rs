//! Typed run/identity seam for F3. Runtime implements this trait; MCP consumes
//! it without a reverse dependency. F9 owns production composition.
use super::input::bounded_input;
use provenant_core::{AttemptId, CancelToken, DeliveryId, Digest, HostId, OperationId, WaitBudget};
use provenant_records::ArtifactRef;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An MCP run target is an attempt ID, optionally qualified by its owning host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptTarget {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub attempt: AttemptId,
    #[serde(default)]
    pub host: Option<HostId>,
}

/// One task in an MCP submission. Project, actor, authority and cwd come from
/// the service's authenticated identity rather than request supplied paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmittedTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<provenant_records::run::WorkContext>,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub build_heavy: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_skill_location: bool,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub access: crate::graph::AccessMode,
    /// Explicit capability set; absent inherits the project default for writers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Provenance for explicitly supplied capabilities.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub capability_sources: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub worktree: Option<String>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
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
pub struct NewWorktree {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
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
        Cancel {
            id: String,
            #[serde(default)]
            reason: Option<String>,
        },
        Resume {
            document: std::path::PathBuf,
            #[serde(default)]
            extend: Vec<String>,
        },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchRefusalFact {
    DetachedWriter,
    GraphSchema,
    GraphComposite,
    GraphPlan,
    GraphStructuralLimit(crate::graph::GraphStructuralLimit),
    GraphEnvelope,
    GraphOutputKind(crate::graph::GraphOutputField),
    Resource(provenant_records::diagnostic::ResourceFact),
}

/// Facts the input owner has explicitly approved for disclosure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidInputDetail {
    Schema,
    LandingLock,
    Field(String),
    FieldTooLarge { field: String, limit: usize },
    FieldTitle(String),
    InputUnreadable,
    InputTooLarge,
    InputInvalidUtf8,
}

impl InvalidInputDetail {
    fn safe_text(&self) -> Option<String> {
        match self {
            Self::Schema => Some("graph document schema_version is invalid".into()),
            Self::LandingLock => Some("landing lock required".into()),
            Self::InputUnreadable => {
                Some("input file is unreadable or is not a regular file".into())
            }
            Self::InputTooLarge => Some("input exceeds the 1 MiB limit".into()),
            Self::InputInvalidUtf8 => Some("input is not UTF-8".into()),
            Self::Field(path) => {
                safe_input_field(path).map(|path| format!("invalid input field `{path}`"))
            }
            Self::FieldTooLarge { field, limit } => safe_input_field(field)
                .map(|path| format!("invalid input field `{path}`: exceeds {limit} bytes")),
            Self::FieldTitle(field) => safe_input_field(field).map(|path| format!("invalid input field `{path}`: task title must be 1 to 256 bytes and contain no control characters")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FacadeError {
    #[error("run execution owner is unavailable")]
    OwnerUnavailable,
    /// `detail` names the field path or static reason when the refusing check
    /// has one. Absent on the wire when empty, so older readers still decode it.
    #[error("invalid input{}", invalid_detail(.detail))]
    InvalidInput {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialise_invalid_detail",
            deserialize_with = "deserialise_invalid_detail"
        )]
        detail: Option<InvalidInputDetail>,
    },
    #[error("record not found")]
    NotFound,
    #[error("request conflicts with current state")]
    Conflict,
    #[error("continuation generation conflict; current generation is {current_generation}")]
    GenerationConflict { current_generation: u64 },
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
    #[error("{}", crate::hosts::protocol_mismatch_message(peer, *remote, *expected))]
    PeerProtocolMismatch {
        #[serde(serialize_with = "serialise_peer_label")]
        peer: String,
        remote: u64,
        /// The requested protocol level.
        expected: u64,
    },
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
    ConfinementRefused {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource: Option<provenant_records::diagnostic::ResourceFact>,
    },
    #[error("writer lock or writer is busy")]
    WriterBusy,
    #[error("writer custody is unknown")]
    WriterUnknown,
    #[error("a requested credential-root grant was refused")]
    CredentialRoot {
        #[serde(serialize_with = "serialise_grant_access")]
        access: String,
    },
    #[error("{}", dispatch_message(reason, disk.as_ref()))]
    DispatchRefused {
        #[serde(serialize_with = "serialise_dispatch_reason")]
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disk: Option<DiskPressure>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fact: Option<DispatchRefusalFact>,
    },
    #[error("{reason}")]
    DispatchUnobserved {
        #[serde(serialize_with = "serialise_unobserved_reason")]
        reason: String,
    },
    #[serde(serialize_with = "delivery_stage_wire", skip_deserializing)]
    #[error("host writer delivery failed at {stage:?}")]
    HostDeliveryStage {
        stage: provenant_records::run::WriterDeliveryStage,
    },
    #[serde(serialize_with = "delivery_limit_wire", skip_deserializing)]
    #[error("{}", .fact.detail())]
    HostDeliveryLimit {
        fact: provenant_records::diagnostic::DeliveryLimit,
    },
    #[error("local worktree does not exist; create it with `provenant worktree create`")]
    WorktreeMissing,
    #[error(
        "worktree {path:?} is not registered with Provenant; create a writer with `provenant worktree create <new-branch> --base <branch>`"
    )]
    WorktreeUnregistered {
        #[serde(serialize_with = "serialise_unanchored_path")]
        path: String,
    },
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
    #[error(
        "upgrade required: reader release {reader_release}, writer release {writer_release}, {record} version {version}"
    )]
    UpgradeRequired {
        reader_release: String,
        writer_release: String,
        record: String,
        version: u64,
    },
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
        #[serde(serialize_with = "serialise_io_operation")]
        operation: String,
        #[serde(serialize_with = "serialise_unanchored_optional_path")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_limit: Option<provenant_records::diagnostic::DeliveryLimit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_stage: Option<provenant_records::run::WriterDeliveryStage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource: Option<Box<provenant_records::diagnostic::ResourceFact>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_failure: Option<provenant_records::diagnostic::IoFact>,
    },
}

fn delivery_limit_wire<S: serde::Serializer>(
    fact: &provenant_records::diagnostic::DeliveryLimit,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let mut wire = serializer.serialize_struct("FacadeError", 4)?;
    wire.serialize_field("code", "io")?;
    wire.serialize_field("operation", &fact.detail())?;
    wire.serialize_field("path", &None::<String>)?;
    wire.serialize_field("delivery_limit", fact)?;
    wire.end()
}

/// Safe numeric facts for a disk-floor refusal; the host path stays local.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskPressure {
    pub available: u64,
    pub floor: u64,
    pub reclaimed: u64,
    pub origin: provenant_records::diagnostic::SettingSource,
}

fn dispatch_message(reason: &str, disk: Option<&DiskPressure>) -> String {
    match disk {
        Some(disk) => format!(
            "insufficient disk space on the limiting volume: {} bytes available, floor {} bytes; gc.free_floor from {}; bounded GC reported {} bytes reclaimed; run `provenant gc --free-floor {}B` before dispatching a writer",
            disk.available,
            disk.floor,
            disk.origin.description(),
            disk.reclaimed,
            disk.floor
        ),
        None => provenant_records::diagnostic::safe_text(reason),
    }
}

fn serialise_unanchored_path<S: serde::Serializer>(
    _: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str("inspect worktree registration")
}

fn serialise_grant_access<S: serde::Serializer>(
    access: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(match access {
        "read" => "read",
        "write" => "write",
        _ => "requested",
    })
}

fn serialise_dispatch_reason<S: serde::Serializer>(
    _: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str("dispatch was refused; inspect the request and access policy")
}

fn serialise_unobserved_reason<S: serde::Serializer>(
    _: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str("disk observation failed")
}

fn serialise_unanchored_optional_path<S: serde::Serializer>(
    _: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    Option::<String>::None.serialize(serializer)
}

fn serialise_io_operation<S: serde::Serializer>(
    operation: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let operation = match operation {
        "access Fabric store"
        | "create peer worktree"
        | "submit: create peer worktree"
        | "read file"
        | "rename"
        | "resolve path"
        | "verify filesystem device"
        | "acquire lock"
        | "set lock deadline"
        | "verify filesystem entry" => operation,
        _ => "I/O operation",
    };
    serializer.serialize_str(operation)
}

fn serialise_peer_label<S: serde::Serializer>(
    peer: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match peer.parse::<provenant_core::HostId>() {
        Ok(host) => serializer.serialize_str(&host.to_string()),
        Err(_) => serializer.serialize_str("peer"),
    }
}

fn delivery_stage_wire<S: serde::Serializer>(
    stage: &provenant_records::run::WriterDeliveryStage,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let mut wire = serializer.serialize_struct("FacadeError", 4)?;
    wire.serialize_field("code", "io")?;
    wire.serialize_field(
        "operation",
        &format!("host writer delivery failed at {stage:?}"),
    )?;
    wire.serialize_field("path", &None::<String>)?;
    wire.serialize_field("delivery_stage", stage)?;
    wire.end()
}

impl FacadeError {
    /// Project a state directory permission refusal for the selected edge.
    pub fn state_directory_permissions(
        path: &std::path::Path,
        mode: u32,
        destination: provenant_records::diagnostic::Destination<'_>,
    ) -> provenant_records::diagnostic::Diagnostic {
        use provenant_records::diagnostic::{Diagnostic, safe_path, shell_path};
        let detail = safe_path(path, destination).map_or_else(
            || format!("Fabric state directory has mode {mode:04o}; other users can access it"),
            |path| {
                format!(
                    "Fabric state directory {path} has mode {mode:04o}; other users can access it"
                )
            },
        );
        let action = shell_path(path, destination).map_or_else(
            || "restrict the state directory to its owner, then retry".into(),
            |path| format!("run chmod 700 {path} and retry"),
        );
        Diagnostic::new("invalid_input")
            .category("invalid")
            .detail(detail)
            .next(action)
    }

    /// Retain the typed admission stage while keeping its original refusal text.
    pub fn graph_admission(error: crate::graph::AdmissionError) -> Self {
        use crate::graph::AdmissionError;
        let fact = match &error {
            AdmissionError::Schema(_) => DispatchRefusalFact::GraphSchema,
            AdmissionError::Composite(_) => DispatchRefusalFact::GraphComposite,
            AdmissionError::Plan(_) => DispatchRefusalFact::GraphPlan,
            AdmissionError::StructuralLimit(limit) => {
                DispatchRefusalFact::GraphStructuralLimit(*limit)
            }
            AdmissionError::Envelope(_) => DispatchRefusalFact::GraphEnvelope,
            AdmissionError::OutputKind { field, .. } => {
                DispatchRefusalFact::GraphOutputKind(*field)
            }
        };
        Self::DispatchRefused {
            reason: error.to_string(),
            disk: None,
            fact: Some(fact),
        }
    }

    /// Project only facts selected by the owning refusal, never its source text.
    pub fn diagnostic(&self) -> provenant_records::diagnostic::Diagnostic {
        use provenant_records::diagnostic::Diagnostic;
        let mut projected = Diagnostic::new(self.code());
        match self {
            Self::InvalidInput { detail: Some(detail) } => {
                if let Some(detail) = detail.safe_text() {
                    projected = projected.detail(detail);
                }
                projected = projected.next("check the named input field and retry");
            }
            Self::InvalidInput { detail: None } => projected = projected.detail("invalid input").next("check the input and retry"),
            Self::Busy => projected = projected.detail("service is busy").next("wait for host capacity before retrying"),
            Self::PeerProtocolMismatch { remote, expected, .. } => {
                projected = projected.detail(format!("peer protocol {remote} is incompatible with expected protocol {expected}"))
                    .next("upgrade both hosts to compatible protocol versions");
            }
            Self::DispatchUnobserved { .. } => projected = projected.detail("disk observation failed").next("restore the host disk observation before retrying"),
            Self::DispatchRefused { fact: Some(DispatchRefusalFact::DetachedWriter), .. } => {
                projected = projected.detail("writer needs a branch")
                    .next("create a branch for the worktree before retrying");
            }
            Self::DispatchRefused { fact: Some(DispatchRefusalFact::GraphStructuralLimit(limit)), .. } => {
                projected = projected.detail(limit.to_string()).next("reduce the graph or adjust the named graph limit");
            }
            Self::DispatchRefused { fact: Some(DispatchRefusalFact::Resource(fact)), .. } => {
                projected = fact.project(projected);
            }
            Self::DispatchRefused { fact: Some(fact), .. } => {
                let (detail, action) = match fact {
                    DispatchRefusalFact::DetachedWriter => unreachable!(),
                    DispatchRefusalFact::GraphSchema => ("graph document schema_version is invalid", "use a schema version that supports the document fields"),
                    DispatchRefusalFact::GraphComposite => ("graph composite or loop expansion is invalid", "inspect the composite or loop definition"),
                    DispatchRefusalFact::GraphPlan => ("graph plan is invalid", "inspect the graph plan and node dependencies"),
                    DispatchRefusalFact::GraphStructuralLimit(_) => unreachable!(),
                    DispatchRefusalFact::Resource(_) => unreachable!(),
                    DispatchRefusalFact::GraphEnvelope => ("graph checkpoint would exceed its size limit", "split the graph into smaller runs"),
                    DispatchRefusalFact::GraphOutputKind(crate::graph::GraphOutputField::Input) => ("graph inputs[].outputs selects an unavailable output kind", "select an output supplied by the source node"),
                    DispatchRefusalFact::GraphOutputKind(crate::graph::GraphOutputField::CompositeExport) => ("graph composite.exports[].outputs selects an unavailable output kind", "select an output supplied by the source node"),
                };
                projected = projected.detail(detail).next(action);
            }
            Self::DispatchRefused { disk: Some(disk), .. } => {
                projected = projected.detail(format!("insufficient disk space on the limiting volume: {} bytes available, floor {} bytes; gc.free_floor from {}; bounded GC reported {} bytes reclaimed", disk.available, disk.floor, disk.origin.description(), disk.reclaimed))
                    .next(format!("run `provenant gc --free-floor {}B` before dispatching a writer", disk.floor));
            }
            Self::HostDeliveryLimit { fact } | Self::Io { delivery_limit: Some(fact), .. } => projected = projected.detail(fact.detail()).next("inspect host delivery before retrying"),
            Self::HostDeliveryStage { stage } | Self::Io { delivery_stage: Some(stage), .. } => projected = projected.detail(format!("host writer delivery failed at {stage:?}")).next("inspect host delivery before retrying"),
            Self::Io { resource: Some(fact), .. } if matches!(fact.as_ref(), provenant_records::diagnostic::ResourceFact::Processes { .. }) => projected = fact.project(projected),
            Self::Io { input_failure: Some(fact), .. } => projected = fact.project(projected),
            Self::LaunchUnknown { operation_id } => projected = projected.next(format!("inspect the receipt for operation {operation_id} before retrying with the same operation ID")),
            Self::WriterBusy => projected = projected.next("wait for the current writer or lock holder to finish"),
            Self::WriterUnknown | Self::WorktreeUnregistered { .. } => projected = projected.next("inspect worktree registration and custody before selecting a checkout with --worktree"),
            Self::ConfinementRefused { resource: Some(fact) } => projected = fact.project(projected),
            Self::ConfinementRefused { resource: None } => projected = projected.next("inspect the requested access and confinement policy"),
            Self::CredentialRoot { access } => {
                let access = match access.as_str() { "read" => "read", "write" => "write", _ => "requested" };
                projected = projected.detail(format!("credential root refused for {access} grant"))
                    .next(format!("inspect the {access} grant and select a permitted root"));
            }
            _ => {}
        }
        if projected.next.is_none() {
            projected = projected
                .next("inspect the current state and request before taking further action");
        }
        projected
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::OwnerUnavailable => "owner_unavailable",
            Self::InvalidInput { .. } => "invalid_input",
            Self::NotFound => "not_found",
            Self::Conflict | Self::GenerationConflict { .. } => "conflict",
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
            Self::ConfinementRefused { .. } => "confinement_refused",
            Self::WriterBusy => "writer_busy",
            Self::WriterUnknown => "writer_unknown",
            Self::CredentialRoot { .. } => "credential_root",
            Self::DispatchRefused { .. } => "dispatch_refused",
            Self::DispatchUnobserved { .. } => "dispatch_unobserved",
            Self::HostDeliveryStage { .. } => "io",
            Self::HostDeliveryLimit { .. } => "io",
            Self::WorktreeMissing => "worktree_missing",
            Self::WorktreeUnregistered { .. } => "worktree_unregistered",
            Self::BaseUnreachable => "base_unreachable",
            Self::BranchExists => "branch_exists",
            Self::OutputTooLarge => "output_too_large",
            Self::StateCorrupt => "state_corrupt",
            Self::UpgradeRequired { .. } => "upgrade_required",
            Self::UpgradeRefused => "upgrade_refused",
            Self::PublicationFailed => "publication_failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Io { .. } => "io",
        }
    }
}

/// The ": reason" suffix of an invalid-input message, control characters
/// escaped so a detail echoing caller text cannot forge terminal output.
fn invalid_detail(detail: &Option<InvalidInputDetail>) -> String {
    detail
        .as_ref()
        .and_then(InvalidInputDetail::safe_text)
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default()
}

pub(super) fn safe_input_field(field: &str) -> Option<&str> {
    let allowed = [
        "scope",
        "paths",
        "path",
        "key",
        "tasks",
        "context",
        "title",
        "prompt",
        "base",
        "head",
        "account",
        "worktree",
        "deliver",
        "remote",
        "branch",
        "route",
        "adapter",
        "provider",
        "model",
        "effort",
        "timeout_seconds",
        "knobs",
        "session",
        "recipients",
        "after",
        "through",
        "state",
        "extend",
        "id",
        "reason",
        "concurrency",
        "targets",
        "max_bytes",
        "wait_seconds",
        "lease_seconds",
        "operation_id",
        "artifacts",
        "artifact",
        "limit",
        "document",
        "target",
        "condition",
        "proof",
        "reference",
        "body",
        "note",
        "name",
        "host",
        "attempt",
        "digest",
        "encoding",
        "generation",
        "expected",
    ];
    if field.len() <= 128
        && field.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'[' | b']')
        })
        && field
            .split(['.', '[', ']'])
            .filter(|part| !part.is_empty())
            .all(|part| part.bytes().all(|b| b.is_ascii_digit()) || allowed.contains(&part))
    {
        return Some(field);
    }
    None
}

fn serialise_invalid_detail<S: serde::Serializer>(
    detail: &Option<InvalidInputDetail>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    detail
        .as_ref()
        .and_then(InvalidInputDetail::safe_text)
        .serialize(serializer)
}

fn deserialise_invalid_detail<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<InvalidInputDetail>, D::Error> {
    let detail = Option::<String>::deserialize(deserializer)?;
    Ok(detail.and_then(|detail| match detail.as_str() {
        "graph document schema_version is invalid" => Some(InvalidInputDetail::Schema),
        "landing lock required" => Some(InvalidInputDetail::LandingLock),
        "input file is unreadable or is not a regular file" => {
            Some(InvalidInputDetail::InputUnreadable)
        }
        "input exceeds the 1 MiB limit" => Some(InvalidInputDetail::InputTooLarge),
        "input is not UTF-8" => Some(InvalidInputDetail::InputInvalidUtf8),
        _ => detail
            .strip_prefix("invalid input field `")
            .and_then(|rest| {
                let (field, reason) = rest.split_once('`')?;
                let field = safe_input_field(field)?.to_owned();
                match reason {
                    "" => Some(InvalidInputDetail::Field(field)),
                    ": task title must be 1 to 256 bytes and contain no control characters" => {
                        Some(InvalidInputDetail::FieldTitle(field))
                    }
                    _ => reason
                        .strip_prefix(": exceeds ")
                        .and_then(|value| value.strip_suffix(" bytes"))
                        .and_then(|value| value.parse::<usize>().ok())
                        .map(|limit| InvalidInputDetail::FieldTooLarge { field, limit }),
                }
            }),
    }))
}

impl FacadeError {
    /// An invalid-input refusal naming the field path or reason that failed.
    pub fn invalid(detail: InvalidInputDetail) -> Self {
        Self::InvalidInput {
            detail: Some(detail),
        }
    }

    pub fn io(operation: impl Into<String>) -> Self {
        Self::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
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
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
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
            PeerError::ProtocolMismatch {
                peer,
                remote,
                expected,
            } => Self::PeerProtocolMismatch {
                peer: peer.clone(),
                remote: *remote,
                expected: *expected,
            },
            PeerError::Project | PeerError::SourceHost | PeerError::RequestTooLarge => {
                Self::InvalidInput { detail: None }
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
            CoordinationError::InvalidInput(_) => Self::InvalidInput { detail: None },
            CoordinationError::LandingLockRequired => {
                Self::invalid(InvalidInputDetail::LandingLock)
            }
            CoordinationError::Input(error) => Self::InvalidInput {
                detail: error.diagnostic_detail(),
            },
            CoordinationError::Time(_) | CoordinationError::ResponseEncoding(_) => {
                Self::StateCorrupt
            }
            CoordinationError::Cancelled => Self::Cancelled,
            CoordinationError::Timeout => Self::Timeout,
            CoordinationError::Busy => Self::Busy,
            CoordinationError::Entropy(_) | CoordinationError::EntropyEncoding => {
                Self::io("generate entropy")
            }
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
            Error::Invalid(_) => Self::InvalidInput { detail: None },
            Error::PathEncoding | Error::ViewerLimit | Error::ViewerNotOpen => {
                Self::InvalidInput { detail: None }
            }
            Error::Decode(provenant_records::DecodeError::UnsupportedVersion { found, .. })
                if *found > 1 =>
            {
                Self::UpgradeRequired {
                    reader_release: env!("CARGO_PKG_VERSION").into(),
                    writer_release: "unknown".into(),
                    record: "record schema".into(),
                    version: *found,
                }
            }
            Error::SchemaVersion(version)
                if *version > provenant_fabric_store::CURRENT_SCHEMA_VERSION as i64 =>
            {
                Self::UpgradeRequired {
                    reader_release: env!("CARGO_PKG_VERSION").into(),
                    writer_release: "unknown".into(),
                    record: "database schema".into(),
                    version: *version as u64,
                }
            }
            Error::Decode(_)
            | Error::Encode(_)
            | Error::SchemaVersion(_)
            | Error::UnversionedDatabase
            | Error::JournalMode(_)
            | Error::Time(_) => Self::StateCorrupt,
            // checked_directory's device refusal names only the child component.
            Error::Filesystem(provenant_os::fs::FsError::CrossDevice { .. }) => {
                Self::io("verify Fabric filesystem device")
            }
            Error::Filesystem(error) => {
                // Fabric's held directories are traversed from `/`.
                Self::filesystem_anchored(error, Some(std::path::Path::new("/")))
            }
            Error::Io { path, .. } => Self::Io {
                delivery_limit: None,
                delivery_stage: None,
                resource: None,
                input_failure: None,
                operation: "access Fabric store".into(),
                path: Some(path.to_string_lossy().into_owned()),
            },
            Error::Sqlite(_) => Self::io("access Fabric database"),
            Error::UnsafePath(path) => Self::Io {
                delivery_limit: None,
                delivery_stage: None,
                resource: None,
                input_failure: None,
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
    fn future_schema_requires_upgrade_with_version_diagnostics() {
        use provenant_fabric_store::Error;
        let errors = [
            Error::SchemaVersion(99),
            Error::Decode(provenant_records::DecodeError::UnsupportedVersion {
                found: 2,
                line: 1,
                column: 1,
            }),
        ];
        for error in errors {
            let facade = FacadeError::from(&error);
            assert_eq!(facade.code(), "upgrade_required");
            let message = facade.to_string();
            assert!(message.contains(env!("CARGO_PKG_VERSION")));
            assert!(message.contains("writer release unknown"));
        }
        assert_eq!(
            FacadeError::from(&Error::SchemaVersion(1)),
            FacadeError::StateCorrupt
        );
    }

    #[test]
    fn graph_admission_projection_does_not_echo_source() {
        let error = FacadeError::graph_admission(crate::graph::AdmissionError::Schema(
            "TOKEN_SENTINEL\u{1b}".into(),
        ));
        let projection = error.diagnostic();
        assert_eq!(projection.code, "dispatch_refused");
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("schema_version")
        );
        assert!(projection.next.is_some());
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
        assert!(
            !serde_json::to_string(&projection)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
    }

    #[test]
    fn directory_permissions_projection_respects_destination() {
        use provenant_records::diagnostic::Destination;
        let path = std::path::Path::new("/private/a b");
        let local = FacadeError::state_directory_permissions(path, 0o755, Destination::Local);
        assert!(local.next.unwrap().contains("chmod 700 '/private/a b'"));
        let remote = FacadeError::state_directory_permissions(
            path,
            0o755,
            Destination::Remote {
                repository: None,
                home: None,
            },
        );
        let wire = serde_json::to_string(&remote).unwrap();
        assert!(!wire.contains("/private"));
        assert!(remote.next.is_some());
    }

    #[test]
    fn generic_io_has_safe_action_without_source_detail() {
        let error = FacadeError::Io {
            operation: "TOKEN_SENTINEL\u{1b}".into(),
            path: Some("/private/host".into()),
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
        };
        let diagnostic = error.diagnostic();
        assert_eq!(diagnostic.code, "io");
        assert!(diagnostic.detail.is_none());
        assert!(diagnostic.next.is_some());
        let wire = serde_json::to_string(&diagnostic).unwrap();
        assert!(!wire.contains("TOKEN_SENTINEL"));
        assert!(!wire.contains("/private/host"));
    }

    #[test]
    fn protocol_projection_omits_untrusted_peer_name() {
        let error = FacadeError::PeerProtocolMismatch {
            peer: "/private/TOKEN_SENTINEL\u{1b}".into(),
            remote: 3,
            expected: 6,
        };
        let projected = error.diagnostic();
        let wire = serde_json::to_string(&projected).unwrap();
        assert!(!wire.contains("TOKEN_SENTINEL"));
        assert!(!wire.contains("/private"));
        let transport = serde_json::to_string(&error).unwrap();
        assert!(!transport.contains("TOKEN_SENTINEL"));
        assert!(!transport.contains("/private"));
        let host = provenant_core::HostId::from_bytes([7; 16]).to_string();
        let safe = FacadeError::PeerProtocolMismatch {
            peer: host.clone(),
            remote: 3,
            expected: 6,
        };
        assert_eq!(serde_json::to_value(safe).unwrap()["peer"], host);
        assert!(projected.detail.unwrap().contains('3'));
        assert!(projected.next.is_some());
    }

    #[test]
    fn legacy_mismatch_frame_without_level_is_rejected() {
        let result = serde_json::from_value::<FacadeError>(serde_json::json!({
            "code": "peer_protocol_mismatch", "peer": "w", "remote": 1
        }));
        assert_eq!(
            result.unwrap_err().classify(),
            serde_json::error::Category::Data
        );
        let current = FacadeError::PeerProtocolMismatch {
            peer: "w".into(),
            remote: 1,
            expected: 2,
        };
        assert_eq!(current.code(), "peer_version");
    }

    #[test]
    fn resume_preserves_no_fallback_and_target() -> Result<(), Box<dyn std::error::Error>> {
        let input = serde_json::json!({
            "op": "resume",
            "target": { "attempt": "00000000000000000000000000000001" },
            "operation_id": "resume-fixture",
            "prompt": "Continue",
            "expected_generation": 0,
            "no_fallback": true
        });
        let mut legacy = input.clone();
        legacy.as_object_mut().unwrap().remove("no_fallback");
        assert!(matches!(
            serde_json::from_value::<super::RunInput>(legacy)?,
            super::RunInput::Resume {
                no_fallback: false,
                ..
            }
        ));
        let explicit: super::RunInput = serde_json::from_value(input)?;
        assert!(matches!(
            explicit,
            super::RunInput::Resume {
                no_fallback: true,
                ..
            }
        ));
        let wire = serde_json::to_value(explicit)?;
        assert_eq!(wire["no_fallback"], true);
        assert_eq!(
            wire["target"]["attempt"],
            "00000000000000000000000000000001"
        );
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
                delivery_limit: None,
                delivery_stage: None,
                resource: None,
                input_failure: None,
                operation: "openat read".into(),
                path: None,
            }
        );
    }

    #[test]
    fn typed_delivery_stage_survives_io_wire() {
        let stage = provenant_records::run::WriterDeliveryStage::Prepare;
        let wire = serde_json::to_value(FacadeError::HostDeliveryStage { stage }).unwrap();
        assert_eq!(wire["code"], "io");
        let received: FacadeError = serde_json::from_value(wire).unwrap();
        assert!(received.diagnostic().detail.unwrap().contains("Prepare"));
    }

    #[test]
    fn typed_delivery_limit_survives_peer_io_envelope_without_trusting_operation_text() {
        use provenant_records::{diagnostic::DeliveryLimit, run::WriterDeliveryStage};
        let fact =
            DeliveryLimit::new(WriterDeliveryStage::Prepare, "git", "worktree list", 4096).unwrap();
        let source = FacadeError::HostDeliveryLimit { fact: fact.clone() };
        let mut wire = serde_json::to_value(source).unwrap();
        assert_eq!(wire["code"], "io");
        wire["operation"] = "TOKEN_SENTINEL".into();
        let received: FacadeError = serde_json::from_value(wire).unwrap();
        let projected = received.diagnostic();
        assert_eq!(projected.code, "io");
        assert_eq!(projected.detail.as_deref(), Some(fact.detail().as_str()));
        assert!(
            !serde_json::to_string(&projected)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
    }

    #[test]
    fn detached_writer_refusal_keeps_its_typed_cause_across_transport() {
        let error = FacadeError::DispatchRefused {
            reason: "writer needs a branch".into(),
            disk: None,
            fact: Some(super::DispatchRefusalFact::DetachedWriter),
        };
        let wire = serde_json::to_string(&error).unwrap();
        let received: FacadeError = serde_json::from_str(&wire).unwrap();
        let projected = received.diagnostic();
        assert_eq!(projected.code, "dispatch_refused");
        assert_eq!(projected.detail.as_deref(), Some("writer needs a branch"));
        assert!(
            projected
                .next
                .as_deref()
                .is_some_and(|next| next.contains("branch"))
        );
    }

    #[test]
    fn disk_refusal_retains_numeric_facts_without_source_or_host_path() {
        let error = FacadeError::DispatchRefused {
            fact: None,
            reason: "TOKEN_SENTINEL /private/host".into(),
            disk: Some(super::DiskPressure {
                available: 42,
                floor: 100,
                reclaimed: 3,
                origin: provenant_records::diagnostic::SettingSource::Configuration,
            }),
        };
        let wire = serde_json::to_string(&error).unwrap();
        assert!(!wire.contains("TOKEN_SENTINEL"));
        assert!(!wire.contains("/private/host"));
        let received: FacadeError = serde_json::from_str(&wire).unwrap();
        let projection = received.diagnostic();
        assert_eq!(projection.code, "dispatch_refused");
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("42 bytes available, floor 100")
        );
        assert!(
            projection
                .detail
                .unwrap()
                .contains("gc.free_floor from host configuration")
        );
        assert!(projection.next.unwrap().contains("100B"));
    }

    #[test]
    fn memory_pressure_refusal_preserves_typed_source_over_transport() {
        use provenant_records::diagnostic::{
            PressureLevel, PressureThreshold, ResourceFact, SettingSource,
        };
        let error = FacadeError::ConfinementRefused {
            resource: Some(ResourceFact::MemoryPressure {
                level: PressureLevel::Critical,
                threshold: PressureThreshold::Warning,
                origin: SettingSource::Configuration,
            }),
        };
        let received: FacadeError =
            serde_json::from_value(serde_json::to_value(error).unwrap()).unwrap();
        let diagnostic = received.diagnostic();
        assert_eq!(diagnostic.code, "confinement_refused");
        let detail = diagnostic.detail.unwrap();
        assert!(detail.contains("critical"));
        assert!(detail.contains("warning"));
        assert!(detail.contains("host configuration"));
        assert!(diagnostic.next.unwrap().contains("memory"));
        let generic = FacadeError::ConfinementRefused { resource: None }.diagnostic();
        assert!(generic.detail.is_none());
    }

    #[test]
    fn invalid_input_projects_only_safe_fields() {
        let bare = FacadeError::InvalidInput { detail: None };
        assert_eq!(bare.to_string(), "invalid input");
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({"code": "invalid_input"})
        );
        let detailed = FacadeError::invalid(super::InvalidInputDetail::Schema);
        assert_eq!(detailed.code(), "invalid_input");
        assert_eq!(
            detailed.to_string(),
            "invalid input: graph document schema_version is invalid"
        );
        let wire = serde_json::to_value(&detailed).unwrap();
        assert_eq!(wire["detail"], "graph document schema_version is invalid");
        assert!(!wire.to_string().contains("31m"));
        let untrusted: FacadeError = serde_json::from_value(serde_json::json!({
            "code": "invalid_input", "detail": "schema version TOKEN_SENTINEL\u{1b}"
        }))
        .unwrap();
        assert!(
            !serde_json::to_string(&untrusted)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
        assert!(!untrusted.to_string().contains("TOKEN_SENTINEL"));
        for input in [
            format!("schema version TOKEN_SENTINEL\u{1b}{}", "é".repeat(2048)),
            "invalid input field `scope.paths` TOKEN_SENTINEL".to_owned(),
        ] {
            let received: FacadeError = serde_json::from_value(serde_json::json!({
                "code": "invalid_input", "detail": input
            }))
            .unwrap();
            let diagnostic = received.diagnostic();
            let wire = serde_json::to_string(&diagnostic).unwrap();
            assert!(!wire.contains("TOKEN_SENTINEL"));
            assert!(wire.len() < 4096);
            assert!(diagnostic.next.is_some());
        }
        let fake_field = FacadeError::InvalidInput {
            detail: Some(super::InvalidInputDetail::Field("secret_token".into())),
        };
        assert!(
            !serde_json::to_string(&fake_field)
                .unwrap()
                .contains("secret_token")
        );
        let from_field = FacadeError::from(&super::super::CoordinationError::Input(
            super::super::input::InputError::new("scope.paths"),
        ));
        assert!(
            from_field.to_string().contains("scope.paths"),
            "{from_field}"
        );
        let received: FacadeError =
            serde_json::from_value(serde_json::to_value(&from_field).unwrap()).unwrap();
        assert_eq!(received.diagnostic(), from_field.diagnostic());
        assert_eq!(
            FacadeError::from(&super::super::CoordinationError::LandingLockRequired).to_string(),
            "invalid input: landing lock required"
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
    fn inbox(
        &mut self,
        _input: crate::inbox::InboxInput,
        _budget: WaitBudget,
        _cancel: &CancelToken,
    ) -> Result<crate::inbox::InboxResult, crate::inbox::InboxError> {
        Err(crate::inbox::InboxError::OwnerUnavailable)
    }
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
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
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
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        target: AttemptTarget,
        artifact: ArtifactRef,
        digest: Digest,
        bytes: OutputBytes,
    },
}
