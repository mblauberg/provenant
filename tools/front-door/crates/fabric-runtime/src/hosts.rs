//! Fixed, bounded SSH peer protocol. No command strings or placement fallback.
use crate::{
    config::{FabricConfig, Peer},
    coordination::*,
    identity::{CrossHostProjectKey, Identity},
};
use provenant_core::{ActorId, CancelToken, Clock, HostId, OperationId, WaitBudget};
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use provenant_records::{CanonicalV1, Limits, Trace};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Component, Path},
    time::Duration,
};

pub const PROTOCOL: u64 = 14;
pub const MAX_FRAME: usize = 1_048_576;

macro_rules! peer_feature_protocols {
    (
        $(
            $constant:ident = $protocol:literal => [
                $(($variant:ident, $wire_name:literal, $sample:expr)),+ $(,)?
            ]
        ),+ $(,)?
    ) => {
        $(pub const $constant: u64 = $protocol;)+

        /// Highest feature fence declared by this peer release.
        pub fn newest_feature_protocol() -> u64 {
            [$($protocol),+].into_iter().fold(0, u64::max)
        }

        fn action_fence(action: &PeerAction) -> u64 {
            match action {
                $($(PeerAction::$variant { .. } => $protocol,)+)+
                _ => PROTOCOL,
            }
        }

        fn feature_protocol_for_wire_name(action: &str) -> u64 {
            match action {
                $($($wire_name => $protocol,)+)+
                _ => PROTOCOL,
            }
        }

        #[cfg(test)]
        fn declared_feature_actions() -> Vec<(&'static str, u64, PeerAction)> {
            vec![
                $(
                    $(($wire_name, $protocol, $sample),)+
                )+
            ]
        }
    };
}

/// Render a protocol mismatch with the requested protocol level.
pub fn protocol_mismatch_message(peer: &str, remote: u64, expected: u64) -> String {
    format!(
        "peer {peer} speaks protocol {remote}, this host expects protocol {expected}; upgrade both"
    )
}

pub fn discover_ssh_executable(
    override_path: Option<&Path>,
) -> Result<std::path::PathBuf, PeerError> {
    if let Some(path) = override_path {
        if !crate::config::valid_executable_override(path)
            || !provenant_os::sys::is_executable(path)
        {
            return Err(PeerError::Unavailable);
        }
        return Ok(path.to_path_buf());
    }
    ["/usr/bin/ssh", "/bin/ssh"]
        .into_iter()
        .map(Path::new)
        .find(|path| provenant_os::sys::is_executable(path))
        .map(Path::to_path_buf)
        .ok_or(PeerError::Unavailable)
}

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("peer protocol version or identity mismatch")]
    Version,
    #[error(
        "peer {peer} speaks protocol {remote}, this host expects protocol {expected}; upgrade both"
    )]
    ProtocolMismatch {
        peer: String,
        remote: u64,
        /// The requested protocol level, which a feature action raises above
        /// the base fence.
        expected: u64,
    },
    #[error("peer project is outside the selected home")]
    Project,
    #[error("peer source host must be a configured peer other than this host")]
    SourceHost,
    #[error("peer is unavailable")]
    Unavailable,
    /// The caller's wait ran out before the request reached the peer, so
    /// nothing was sent and nothing is known about the peer's home.
    #[error("the wait deadline passed before the peer was contacted")]
    Deadline,
    /// A read-only exchange failed for a known cause. Same wire code as
    /// [`PeerError::Unavailable`]; only the message carries the detail.
    #[error("peer is unavailable: {0}")]
    Failed(String),
    #[error("peer mutation outcome is unknown for operation {operation_id}")]
    Unknown { operation_id: OperationId },
    #[error("peer request was cancelled")]
    Cancelled,
    #[error("malformed peer frame")]
    Frame,
    #[error("encoded peer request exceeds 1 MiB; shorten the prompt or split the batch")]
    RequestTooLarge,
    #[error("peer refused the operation: {error}")]
    Refused { error: facade::FacadeError },
}
impl PeerError {
    pub fn code(&self) -> &str {
        if let Self::Refused { error } = self {
            return error.code();
        }
        facade::FacadeError::from(self).static_code()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectLocator {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub key: CrossHostProjectKey,
    pub common_dir: String,
}
impl ProjectLocator {
    pub fn new(identity: &Identity, home: &Path) -> Result<Self, PeerError> {
        let common = identity
            .common_dir
            .to_path_buf()
            .map_err(|_| PeerError::Project)?;
        let relative = common.strip_prefix(home).map_err(|_| PeerError::Project)?;
        let common_dir = relative.to_str().ok_or(PeerError::Project)?.to_owned();
        let locator = Self {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(home, &common).map_err(|_| PeerError::Project)?,
            common_dir,
        };
        locator.resolve(home)?;
        Ok(locator)
    }
    pub fn resolve(&self, home: &Path) -> Result<std::path::PathBuf, PeerError> {
        let relative = Path::new(&self.common_dir);
        if self.common_dir.len() > 4096
            || relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(PeerError::Project);
        }
        let path = home.join(relative);
        if CrossHostProjectKey::from_home(home, &path).map_err(|_| PeerError::Project)? != self.key
        {
            return Err(PeerError::Project);
        }
        Ok(path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "domain", rename_all = "snake_case")]
pub enum CoordinationInput {
    Mail {
        input: MailInput,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Events {
        input: EventsInput,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Claim {
        input: ClaimInput,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
}
impl CoordinationInput {
    pub fn mutation(&self) -> Option<Mutation> {
        match self {
            Self::Mail {
                input: MailInput::Read { .. },
                ..
            }
            | Self::Events {
                input: EventsInput::Read { .. },
                ..
            }
            | Self::Claim {
                input: ClaimInput::Verify { .. } | ClaimInput::List { .. },
                ..
            } => None,
            Self::Mail { input: v, .. } => Some(Mutation::Mail(v.clone())),
            Self::Events { input: v, .. } => Some(Mutation::Events(v.clone())),
            Self::Claim { input: v, .. } => Some(Mutation::Claim(v.clone())),
        }
    }
    pub fn execute(
        &self,
        service: &CoordinationService<'_>,
        operation: Option<OperationId>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<CoordinationResult, CoordinationError> {
        if let Some(input) = self.mutation() {
            let id = operation.ok_or(CoordinationError::InvalidInput(
                "peer mutations require operation_id",
            ))?;
            return service.mutate(input, &id, budget, cancel).map(Into::into);
        }
        if operation.is_some() {
            return Err(CoordinationError::InvalidInput(
                "operation_id on observation",
            ));
        }
        match self {
            Self::Mail { input: v, .. } => {
                service
                    .mail(v.clone(), budget, cancel)
                    .map(|data| CoordinationResult::Mail {
                        result: data,
                        extra: Default::default(),
                    })
            }
            Self::Events { input: v, .. } => {
                service
                    .events(v.clone(), budget, cancel)
                    .map(|data| CoordinationResult::Events {
                        result: data,
                        extra: Default::default(),
                    })
            }
            Self::Claim { input: v, .. } => {
                service
                    .claim(v.clone(), budget, cancel)
                    .map(|data| CoordinationResult::Claim {
                        result: data,
                        extra: Default::default(),
                    })
            }
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
// Struct variants retain extensions without changing the domain/result wire shape.
#[serde(tag = "domain", rename_all = "snake_case")]
pub enum CoordinationResult {
    Mail {
        result: MailResult,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Events {
        result: EventsResult,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Claim {
        result: ClaimResult,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
}
impl CoordinationResult {
    /// Stored mutation results have no domain tag; use the frozen request
    /// rather than guessing between compatible, extensible result objects.
    pub fn from_stored(
        input: &Mutation,
        result: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        macro_rules! decode {
            ($variant:ident) => {
                serde_json::from_value(result).map(|result| Self::$variant {
                    result,
                    extra: Default::default(),
                })
            };
        }
        match input {
            Mutation::Mail(_) => decode!(Mail),
            Mutation::Events(_) => decode!(Events),
            Mutation::Claim(_) => decode!(Claim),
        }
    }
}
impl From<MutationResult> for CoordinationResult {
    fn from(value: MutationResult) -> Self {
        match value {
            MutationResult::Mail(v) => Self::Mail {
                result: v,
                extra: Default::default(),
            },
            MutationResult::Events(v) => Self::Events {
                result: v,
                extra: Default::default(),
            },
            MutationResult::Claim(v) => Self::Claim {
                result: v,
                extra: Default::default(),
            },
        }
    }
}
/// Typed launch facts for a graph node submitted to a peer host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSubmitContext {
    pub launch_actor: ActorId,
    pub attempt: crate::graph::AttemptSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_limit: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<provenant_records::run::NetworkAccess>,
}

/// Graph identity for a read of its accepted peer receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphReceiptContext {
    pub launch_actor: ActorId,
    pub origin_work_key: provenant_records::WorkKey,
}

/// Graph identity for a receiver-owned cancellation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCancelContext {
    pub launch_actor: ActorId,
}

/// Claimed output audience. Graph execution is authorised only
/// after the receiver validates this context against the authenticated actor and
/// the retained launch receipt and verifies that the requested attempt belongs
/// to that launch. A claim alone never grants that authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputAudience {
    PeerPublication,
    GraphExecution {
        launch_actor: ActorId,
        launch_operation: OperationId,
        origin_work_key: provenant_records::WorkKey,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PeerAction {
    Health {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    /// The peer's own sign-in and managed-install report, read-only.
    Doctor {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Inbox {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        input: crate::inbox::InboxInput,
    },
    ConsoleBoard {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        all: bool,
        limit: u64,
        selected: bool,
    },
    ConsoleTree {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        max_depth: u64,
        max_nodes: u64,
        detail: bool,
        selected: bool,
    },
    ConsoleDetail {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        id: String,
    },
    /// The first page of this host's requests across all its projects,
    /// read-only. `state` is `open` or `all`; `limit` is at most `page_rows`.
    ConsoleInbox {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        state: crate::inbox::InboxFilter,
        limit: u64,
    },
    RunStatuses {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        targets: Vec<crate::coordination::facade::AttemptTarget>,
    },
    Run {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        input: crate::coordination::facade::RunInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_subtree_after: Option<usize>,
    },
    Submit {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        graph_context: Box<GraphSubmitContext>,
        input: Box<crate::coordination::facade::SubmitInput>,
    },
    Output {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        audience: OutputAudience,
        input: crate::coordination::facade::OutputInput,
    },
    RunEvents {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        after: u64,
        limit: u16,
    },
    Receipt {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graph_context: Option<GraphReceiptContext>,
        operation_id: OperationId,
    },
    /// Cancel or inspect cancellation of a receiver-owned graph by its stable submission.
    CancelSubmission {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        graph_context: GraphCancelContext,
        submission: OperationId,
        operation_id: OperationId,
        stop: bool,
    },
    Coordination {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        input: CoordinationInput,
        operation_id: Option<OperationId>,
    },
    /// The peer's own lanes, active only unless `all`; newest first with `all`.
    Lanes {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        all: bool,
        limit: u16,
    },
    /// What each named queued or starting lane waits for. An older peer does
    /// not know it and refuses, which a caller reads as no waits to show.
    LaneWaits {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        targets: Vec<crate::coordination::facade::AttemptTarget>,
    },
    /// Start a fresh session on the peer from a hand-off prompt.
    SessionHandoff {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        input: Box<crate::handoff::SessionHandoff>,
        /// The source's wait, so the peer finishes inside it.
        budget_seconds: u64,
    },
}

peer_feature_protocols! {
    OUTPUT_AUDIENCE_PROTOCOL = 15 => [
        (Output, "output", PeerAction::Output {
            extra: Default::default(),
            audience: OutputAudience::PeerPublication,
            input: facade::OutputInput::Read {
                extra: Default::default(),
                target: facade::AttemptTarget { extra: Default::default(), attempt: provenant_core::AttemptId::from_bytes([1; 16]), host: None },
                part: facade::OutputPart::Result,
                offset: 0,
                max_bytes: 1,
            },
        }),
    ],
    CONSOLE_PROTOCOL = 7 => [
        (ConsoleBoard, "console_board", PeerAction::ConsoleBoard {
            extra: Default::default(), all: false, limit: 50, selected: false,
        }),
        (ConsoleTree, "console_tree", PeerAction::ConsoleTree {
            extra: Default::default(), max_depth: 8, max_nodes: 100, detail: false, selected: false,
        }),
    ],
    INBOX_PROTOCOL = 14 => [
        (Inbox, "inbox", PeerAction::Inbox {
            extra: Default::default(),
            input: crate::inbox::InboxInput::List {
                extra: Default::default(), id: None, session: None,
                state: Default::default(), limit: 50, cursor: None,
            },
        }),
    ],
    DOCTOR_PROTOCOL = 12 => [
        (Doctor, "doctor", PeerAction::Doctor { extra: Default::default() }),
    ],
    CONSOLE_DETAIL_PROTOCOL = 10 => [
        (ConsoleDetail, "console_detail", PeerAction::ConsoleDetail {
            extra: Default::default(), id: "attempt".into(),
        }),
    ],
    CONSOLE_INBOX_PROTOCOL = 11 => [
        (ConsoleInbox, "console_inbox", PeerAction::ConsoleInbox {
            extra: Default::default(), state: crate::inbox::InboxFilter::Open, limit: 50,
        }),
    ],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRequest<A = PeerAction> {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub version: u64,
    pub protocol: u64,
    pub target_host: HostId,
    pub source_host: HostId,
    pub agent: String,
    pub project: ProjectLocator,
    pub action: A,
    /// Who sent this and for which operation, for correlation on the peer.
    #[serde(default)]
    pub trace: Trace,
}
impl PeerRequest {
    /// A request from this host: the trace names the sending host, the acting
    /// agent and the operation the action carries, and nothing else.
    pub fn new(
        target_host: HostId,
        source_host: HostId,
        agent: String,
        project: ProjectLocator,
        action: PeerAction,
    ) -> Self {
        let mut request = Self {
            extra: Default::default(),
            version: 1,
            protocol: action_fence(&action),
            target_host,
            source_host,
            agent,
            project,
            action,
            trace: Trace::default(),
        };
        request.trace = Trace {
            host: Some(source_host),
            actor: request.actor().ok(),
            operation: request.operation(),
            ..Trace::default()
        };
        request
    }
    pub fn operation(&self) -> Option<OperationId> {
        match &self.action {
            PeerAction::Inbox { input, .. } => input.operation_id().cloned(),
            PeerAction::Coordination { operation_id, .. } => operation_id.clone(),
            PeerAction::CancelSubmission { operation_id, .. } => Some(operation_id.clone()),
            PeerAction::SessionHandoff { input, .. } => Some(input.operation_id.clone()),
            PeerAction::Submit { input, .. } => {
                let facade::SubmitInput::Submit { operation_id, .. } = input.as_ref();
                Some(operation_id.clone())
            }
            PeerAction::Run {
                input:
                    facade::RunInput::Cancel { operation_id, .. }
                    | facade::RunInput::Resume { operation_id, .. },
                ..
            } => Some(operation_id.clone()),
            PeerAction::RunStatuses { .. } => None,
            _ => None,
        }
    }
    pub fn digest(&self) -> Result<provenant_core::Digest, PeerError> {
        CanonicalV1::record_digest("fabric-peer", 1, self).map_err(|_| PeerError::Frame)
    }
    pub fn actor(&self) -> Result<ActorId, PeerError> {
        crate::identity::Address::new(self.source_host, &self.agent)
            .map(|a| a.actor)
            .map_err(|_| PeerError::Project)
    }
    pub fn validate(
        &self,
        local: HostId,
        config: &FabricConfig,
        home: &Path,
    ) -> Result<(), PeerError> {
        if self.version != 1 || self.target_host != local {
            return Err(PeerError::Version);
        }
        if self.trace.host.is_some_and(|host| host != self.source_host) {
            return Err(PeerError::SourceHost);
        }
        if self.source_host == local
            || !config
                .peers
                .values()
                .any(|peer| peer.host == self.source_host)
        {
            return Err(PeerError::SourceHost);
        }
        self.project.resolve(home)?;
        self.actor()?;
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostHealth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u64>,
    /// The newest feature level this release speaks. Only the local row
    /// reports it: the wire carries a peer's base fence, not its ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<u64>,
    pub host: HostId,
    pub available: bool,
    /// Readiness requires the run owner; config presence alone proves nothing.
    pub execution: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the host's route readiness reads as from here. Absent on the local
    /// row and on rows nobody probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<PeerReadiness>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHealth {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub host: HostId,
    pub available: bool,
    /// Readiness requires the run owner; config presence alone proves nothing.
    pub execution: bool,
    /// Every format-2 peer reports its release and supported run operations.
    pub release: String,
    pub run_capabilities: crate::runs::RunCapabilities,
    /// Every format-2 peer reports route readiness, even before a route probe.
    pub readiness: PeerReadiness,
}

/// The state of a host's route readiness, with the quota record's wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerReadinessState {
    Ready,
    Unavailable,
    Unknown,
}

/// Reason a peer reads as unknown: quota's `unreachable_peer` wire name.
pub const READINESS_UNREACHABLE_PEER: &str = "unreachable_peer";
/// Reason for a peer too old to report readiness: quota's
/// `unsupported_peer_version` wire name. The protocol refusal itself keeps its
/// own `peer_version` code.
pub const READINESS_UNSUPPORTED_PEER_VERSION: &str = "unsupported_peer_version";
/// Reason nothing was observed.
pub const READINESS_NOT_PROBED: &str = "not_probed";

/// A host's route readiness as it crosses the wire. It is additive: fields
/// the reader does not know are kept, and a missing record is never ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerReadiness {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub state: PeerReadinessState,
    /// A quota readiness reason wire name. Kept as text so a newer peer's
    /// reason still decodes; bounded and cleaned when read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PeerReadiness {
    fn unknown(reason: &str) -> Self {
        Self {
            extra: Default::default(),
            state: PeerReadinessState::Unknown,
            reason: Some(reason.into()),
        }
    }
    /// Nothing observed: the host was not asked.
    pub fn not_probed() -> Self {
        Self::unknown(READINESS_NOT_PROBED)
    }
    /// A peer that could not be asked reads as unknown, never as ready.
    pub fn unreachable() -> Self {
        Self::unknown(READINESS_UNREACHABLE_PEER)
    }
    /// A peer whose build predates readiness reporting.
    pub fn unsupported_version() -> Self {
        Self::unknown(READINESS_UNSUPPORTED_PEER_VERSION)
    }
    /// What this build reports of itself. It carries no per-route
    /// observations yet, so it states only that it can report readiness.
    pub fn reporting() -> Self {
        Self::not_probed()
    }
    /// What a probe of a peer shows. A protocol mismatch is unsupported; any
    /// other failure, or a reply that is not a health reply, is unreachable.
    pub fn from_probe(probe: &Result<PeerResponse, PeerError>) -> Self {
        match probe {
            Ok(PeerResponse {
                result: PeerResult::Health { data, .. },
                ..
            }) => Self {
                extra: Default::default(),
                state: data.readiness.state,
                reason: data.readiness.reason.as_deref().map(|r| plain_text(r, 64)),
            },
            Err(PeerError::ProtocolMismatch { .. }) => Self::unsupported_version(),
            _ => Self::unreachable(),
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
// Internally tagged struct variants keep the existing result/data wire shape
// while retaining extensions on the result envelope as well as its payload.
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PeerResult {
    Inbox {
        data: crate::inbox::InboxReply,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Run {
        data: Box<crate::runs::RunResult>,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    SubmissionCancelled {
        settled: bool,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    RunStatuses {
        #[serde(with = "crate::runs::attempts_wire")]
        data: Vec<provenant_records::run::AttemptRecord>,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    LaneWaits {
        data: Vec<provenant_records::run::LaneWait>,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Submitted {
        data: crate::runs::SubmissionReceipt,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Output {
        data: Box<crate::coordination::facade::OutputResult>,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    RunEvents {
        data: crate::runs::RunEventsPage,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Health {
        data: PeerHealth,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Doctor {
        data: serde_json::Value,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Console {
        data: serde_json::Value,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Coordination {
        data: CoordinationResult,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    SessionHandoff {
        data: crate::handoff::HandoffReceipt,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    // Error keeps its data wrapper, including extensions inside it.
    Error {
        data: PeerResultError,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PeerResultError {
    pub error: facade::FacadeError,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
}

impl PeerResultError {
    /// Preserve the refusal code supplied by the peer.
    pub fn code(&self) -> &str {
        self.error.code()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerResponse<R = PeerResult> {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub version: u64,
    pub protocol: u64,
    pub host: HostId,
    pub project: CrossHostProjectKey,
    pub result: R,
}
/// Inspect the envelope before decoding an action that another protocol may
/// have changed. A refusal always names this host's protocol.
pub fn protocol_refusal(bytes: &[u8]) -> Result<Option<PeerResponse>, PeerError> {
    let request: PeerRequest<serde_json::Value> =
        provenant_records::decode(bytes, Limits::CONFIG).map_err(|_| PeerError::Frame)?;
    let expected =
        feature_protocol_for_wire_name(request.action["action"].as_str().unwrap_or_default());
    Ok((request.protocol != expected).then_some(PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: expected,
        host: request.target_host,
        project: request.project.key,
        result: PeerResult::Error {
            data: crate::hosts::PeerResultError {
                extra: Default::default(),
                error: facade::FacadeError::PeerVersion,
            },
            extra: Default::default(),
        },
    }))
}
pub fn decode_request(bytes: &[u8]) -> Result<PeerRequest, PeerError> {
    provenant_records::decode(bytes, Limits::CONFIG).map_err(|_| PeerError::Frame)
}

/// Recorded replies keep their protocol fence across upgrades.
pub fn decode_recorded_response(bytes: &[u8]) -> Result<PeerResponse, PeerError> {
    let envelope: PeerResponse<serde_json::Value> =
        provenant_records::decode(bytes, Limits::CONFIG).map_err(|_| PeerError::Frame)?;
    if envelope.protocol != PROTOCOL {
        return Err(PeerError::Version);
    }
    provenant_records::decode(bytes, Limits::CONFIG).map_err(|_| PeerError::Frame)
}
/// Control (Cc), format (Cf: zero-width, bidi override and isolate marks) and
/// line or paragraph separator characters, none of which belong in peer text.
fn unprintable(c: char) -> bool {
    c.is_control()
        || matches!(c as u32,
            0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2
            | 0x180E | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064
            | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD
            | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A
            | 0xE0001 | 0xE0020..=0xE007F)
}
/// `text` with unprintable characters removed, cut to `limit` characters.
/// Peer text passes through this before it is printed.
pub fn plain_text(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !unprintable(*c))
        .take(limit)
        .collect()
}
/// First non-empty stderr line, control characters removed, bounded.
fn stderr_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| plain_text(line, 200))
        .unwrap_or_default()
}
/// The one federation port: how a request reaches a peer host and its reply
/// comes back. SSH is the only production implementation; the trait is the seam where a
/// different carrier would plug in, and federation is always in the build.
pub trait PeerTransport {
    fn exchange(
        &self,
        peer: &Peer,
        request: &PeerRequest,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResponse, PeerError>;
}
/// The executor's usual termination and reap allowances, which a deadline
/// shares out of its own time.
const TERMINATE_GRACE: Duration = Duration::from_millis(250);
const REAP_TIMEOUT: Duration = Duration::from_millis(250);
pub struct SshTransport<'a> {
    pub executor: &'a dyn Executor,
    pub clock: &'a dyn Clock,
    pub home: &'a Path,
    pub cwd: &'a Path,
    /// Absolute, resolved system SSH executable, chosen by composition.
    pub executable: std::path::PathBuf,
    pub seconds: u64,
}
fn request_draft_texts<'a>(
    request: &'a mut provenant_records::inbox::RequestDraft,
    texts: &mut Vec<&'a mut String>,
) {
    texts.push(&mut request.title);
    texts.push(&mut request.body);
    texts.extend(request.proceeding_with.iter_mut());
    for option in &mut request.options {
        texts.push(&mut option.label);
        texts.push(&mut option.description);
    }
}

fn human_request_texts<'a>(
    request: &'a mut provenant_records::inbox::HumanRequest,
    texts: &mut Vec<&'a mut String>,
) {
    request_draft_texts(&mut request.request, texts);
    texts.extend(request.closure_reason.iter_mut());
}

fn attempt_state_texts<'a>(
    state: &'a mut provenant_records::run::AttemptState,
    texts: &mut Vec<&'a mut String>,
) {
    if let provenant_records::run::AttemptState::Terminal {
        reason,
        terminal_note,
        ..
    } = state
    {
        texts.extend(reason.iter_mut());
        texts.extend(terminal_note.iter_mut());
    }
}

fn route_texts<'a>(
    route: &'a mut provenant_records::run::RouteRecord,
    texts: &mut Vec<&'a mut String>,
) {
    texts.extend(route.availability_warning.iter_mut());
    texts.extend(route.launch_warnings.iter_mut());
    if let Some(warnings) = &mut route.discovery_warnings {
        texts.extend(warnings.iter_mut());
    }
    if let Some(sandbox) = &mut route.sandbox {
        texts.extend(sandbox.warning.iter_mut());
    }
    if let Some(preparation) = &mut route.dependency_preparation {
        texts.extend(preparation.reason.iter_mut());
    }
    if let Some(paths) = &mut route.project_paths {
        for warning in &mut paths.warnings {
            texts.extend(warning.reason.iter_mut());
            texts.extend(warning.policy_message.iter_mut());
        }
    }
    if let Some(skills) = &mut route.user_skills {
        for warning in &mut skills.warnings {
            texts.push(&mut warning.reason);
        }
    }
    if let Some(tools) = &mut route.tool_closure {
        for tool in tools {
            texts.extend(tool.reason.iter_mut());
            texts.extend(tool.reasons.iter_mut());
        }
    }
    if let Some(access) = &mut route.applied_access {
        for omitted in &mut access.omitted_grants {
            texts.push(&mut omitted.reason);
        }
    }
}

fn attempt_texts<'a>(
    attempt: &'a mut provenant_records::run::AttemptRecord,
    texts: &mut Vec<&'a mut String>,
) {
    texts.extend(attempt.title.iter_mut());
    texts.extend(attempt.publication_error.iter_mut());
    attempt_state_texts(&mut attempt.state, texts);
    for route in [
        &mut attempt.resolved_route,
        &mut attempt.applied_route,
        &mut attempt.observed_route,
    ]
    .into_iter()
    .flatten()
    {
        route_texts(route, texts);
    }
    if let Some(delivery) = &mut attempt.delivery {
        if let provenant_records::run::WriterDeliveryState::Failed { reason, .. } =
            &mut delivery.state
        {
            texts.extend(reason.iter_mut());
        }
    }
    for skipped in &mut attempt.skipped_routes {
        texts.push(&mut skipped.detail);
    }
}

// Doctor and console are JSON views. Select their declared prose by path;
// a coincidentally named field in an extension is still opaque metadata.
fn report_texts<'a>(
    value: &'a mut serde_json::Value,
    paths: &[&str],
    texts: &mut Vec<&'a mut String>,
) {
    match value {
        serde_json::Value::String(text) if paths.contains(&"") => texts.push(text),
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let paths: Vec<_> = paths
                    .iter()
                    .filter_map(|path| {
                        let (first, rest) = path.split_once('/').unwrap_or((path, ""));
                        (first == key || first == "*").then_some(rest)
                    })
                    .collect();
                if !paths.is_empty() {
                    report_texts(value, &paths, texts);
                }
            }
        }
        serde_json::Value::Array(items) => {
            let paths: Vec<_> = paths
                .iter()
                .filter_map(|path| {
                    let (first, rest) = path.split_once('/').unwrap_or((path, ""));
                    (first == "*").then_some(rest)
                })
                .collect();
            if !paths.is_empty() {
                for item in items {
                    report_texts(item, &paths, texts);
                }
            }
        }
        _ => {}
    }
}

/// Encode a response after projecting its declared prose. Output is already
/// projected and digest-bound before paging; it must never be inspected again.
pub fn encode_peer_response(
    mut response: PeerResponse,
    projector: &provenant_confinement::publication::Projector,
) -> Result<Vec<u8>, PeerError> {
    use crate::inbox::{InboxReply, InboxResult};
    use crate::runs::RunResult;
    let mut texts = Vec::new();
    match &mut response.result {
        PeerResult::Inbox { data, .. } => match data {
            InboxReply::Error { message, .. } => texts.push(message),
            InboxReply::Ok { result, .. } => match result.as_mut() {
                InboxResult::Item { item, .. } => {
                    human_request_texts(&mut item.request, &mut texts);
                    for request in &mut item.history {
                        human_request_texts(request, &mut texts);
                    }
                    if let Some(answer) = &mut item.answer {
                        texts.extend(answer.text.iter_mut());
                    }
                    if let Some(delivery) = &mut item.delivery {
                        texts.extend(delivery.error.iter_mut());
                    }
                }
                InboxResult::Listed { items, .. } => {
                    for request in items {
                        human_request_texts(request, &mut texts);
                    }
                }
                InboxResult::Saved { receipt } => {
                    if let Some(delivery) = &mut receipt.delivery {
                        texts.extend(delivery.error.iter_mut());
                    }
                }
                InboxResult::Control { receipt } => texts.push(&mut receipt.reason),
            },
        },
        PeerResult::Run { data, .. } => match data.as_mut() {
            RunResult::List { page, .. } => {
                for item in &mut page.items {
                    texts.extend(item.reason.iter_mut());
                    if let Some(route) = &mut item.applied_route {
                        route_texts(route, &mut texts);
                    }
                }
            }
            RunResult::Status { attempt, .. }
            | RunResult::Cancel { attempt, .. }
            | RunResult::Resume { attempt, .. } => attempt_texts(attempt, &mut texts),
            RunResult::Wait {
                observation,
                attempts,
                ..
            } => {
                texts.extend(observation.iter_mut());
                for attempt in attempts {
                    attempt_texts(attempt, &mut texts);
                }
            }
        },
        PeerResult::RunStatuses { data, .. } => {
            for attempt in data {
                attempt_texts(attempt, &mut texts);
            }
        }
        PeerResult::RunEvents { data, .. } => {
            for event in &mut data.items {
                attempt_state_texts(&mut event.change, &mut texts);
            }
        }
        PeerResult::Coordination { data, .. } => match data {
            CoordinationResult::Mail {
                result: MailResult::Read { page, .. },
                ..
            } => {
                for delivery in &mut page.items {
                    texts.push(&mut delivery.message.body);
                }
            }
            CoordinationResult::Events {
                result:
                    EventsResult::Read {
                        page, observation, ..
                    },
                ..
            } => {
                for event in &mut page.items {
                    texts.push(&mut event.record.detail);
                }
                if let Some(observation) = observation {
                    if let crate::coordination::EventObservation::PacedReread { reason, .. } =
                        observation.as_mut()
                    {
                        texts.push(reason);
                    }
                }
            }
            _ => {}
        },
        PeerResult::SessionHandoff { data, .. } => match data {
            crate::handoff::HandoffReceipt::Refused { message, .. } => texts.push(message),
            crate::handoff::HandoffReceipt::Delivered {
                launch_error,
                worktrees,
                ..
            } => {
                texts.extend(launch_error.iter_mut());
                for worktree in worktrees {
                    texts.extend(worktree.error.iter_mut());
                }
            }
        },
        PeerResult::Error { data, .. } => match &mut data.error {
            facade::FacadeError::RecordedRefusal { diagnostic, .. } => {
                texts.extend(diagnostic.detail.iter_mut());
                texts.extend(diagnostic.next.iter_mut());
            }
            facade::FacadeError::StateCorruptDetail { detail } => texts.push(detail),
            _ => {}
        },
        PeerResult::Doctor { data, .. } => report_texts(
            data,
            &[
                "managed_error",
                "warnings/*",
                "tick_agent/last_exit_reason",
                "tick_agent/warning",
                "tick_agent/repair",
                "login_accounts/*/repair",
            ],
            &mut texts,
        ),
        PeerResult::Console { data, .. } => {
            let mut paths: Vec<String> = [
                "reason",
                "lanes/*/metadata/attempt/title",
                "lanes/*/metadata/attempt/state/reason",
                "lanes/*/metadata/attempt/state/terminal_note",
                "lanes/*/metadata/attempt/publication_error",
                "lanes/*/metadata/attempt/skipped_routes/*/detail",
                "lanes/*/metadata/operation/blocked_reason",
                "lanes/*/metadata/operation/refusal/detail",
                "lanes/*/metadata/operation/refusal/next",
                "nodes/*/metadata/attempt/title",
                "nodes/*/metadata/attempt/state/reason",
                "nodes/*/metadata/attempt/state/terminal_note",
                "nodes/*/metadata/attempt/publication_error",
                "nodes/*/metadata/attempt/skipped_routes/*/detail",
                "nodes/*/metadata/operation/blocked_reason",
                "nodes/*/metadata/operation/refusal/detail",
                "nodes/*/metadata/operation/refusal/next",
                "unfetched/*/detail",
                "delivery/state/reason",
                "linked_inbox_requests/*/title",
                "items/*/request/title",
                "items/*/request/body",
                "items/*/request/proceeding_with",
                "items/*/request/options/*/label",
                "items/*/request/options/*/description",
                "items/*/closure_reason",
            ]
            .into_iter()
            .map(String::from)
            .collect();
            for prefix in ["lanes/*/metadata/attempt", "nodes/*/metadata/attempt"] {
                paths.push(format!("{prefix}/delivery/state/reason"));
                for route in ["resolved_route", "applied_route", "observed_route"] {
                    for field in [
                        "availability_warning",
                        "launch_warnings/*",
                        "discovery_warnings/*",
                        "sandbox/warning",
                        "dependency_preparation/reason",
                        "project_paths/warnings/*/reason",
                        "project_paths/warnings/*/policy_message",
                        "user_skills/warnings/*/reason",
                        "tool_closure/*/reason",
                        "tool_closure/*/reasons/*",
                        "applied_access/omitted_grants/*/reason",
                    ] {
                        paths.push(format!("{prefix}/{route}/{field}"));
                    }
                }
            }
            let paths: Vec<_> = paths.iter().map(String::as_str).collect();
            report_texts(data, &paths, &mut texts);
        }
        PeerResult::Output { .. }
        | PeerResult::Health { .. }
        | PeerResult::Submitted { .. }
        | PeerResult::SubmissionCancelled { .. }
        | PeerResult::LaneWaits { .. } => {}
    }
    provenant_confinement::publication::CrossHostTransport::new(projector)
        .texts(&mut texts)
        .map_err(|_| PeerError::Frame)?;
    let bytes = serde_json::to_vec(&response).map_err(|_| PeerError::Frame)?;
    if bytes.len() >= MAX_FRAME {
        return Err(PeerError::Frame);
    }
    Ok(bytes)
}

/// Encode and validate the request before any peer contact.
pub fn encode_peer_request(request: &PeerRequest) -> Result<Vec<u8>, PeerError> {
    let projector =
        provenant_confinement::publication::Projector::ambient().map_err(|_| PeerError::Frame)?;
    let boundary = provenant_confinement::publication::CrossHostTransport::new(&projector);
    let mut outbound = request.clone();
    let mut texts = Vec::new();
    match &mut outbound.action {
        PeerAction::Submit { input, .. } => {
            let facade::SubmitInput::Submit { task, .. } = input.as_mut();
            texts.push(&mut task.prompt);
            texts.extend(task.title.iter_mut());
        }
        PeerAction::Run {
            input: facade::RunInput::Resume { prompt, .. },
            ..
        } => texts.push(prompt),
        PeerAction::Inbox { input, .. } => match input {
            crate::inbox::InboxInput::Ask { request, .. } => {
                request_draft_texts(request, &mut texts);
            }
            crate::inbox::InboxInput::Answer { text, .. } => texts.extend(text.iter_mut()),
            crate::inbox::InboxInput::Close { reason, .. } => texts.push(reason),
            crate::inbox::InboxInput::Steer { text, .. }
            | crate::inbox::InboxInput::ChairMessage { text, .. } => texts.push(text),
            _ => {}
        },
        PeerAction::Coordination { input, .. } => match input {
            CoordinationInput::Mail {
                input: MailInput::Send { body, .. },
                ..
            } => texts.push(body),
            CoordinationInput::Events {
                input: EventsInput::Note { note, .. },
                ..
            } => texts.push(note),
            _ => {}
        },
        PeerAction::SessionHandoff { input, .. } => {
            texts.push(&mut input.prompt);
            // Decode before collecting typed secrets shared with the prompt.
            for attachment in &mut input.attachments {
                let bytes =
                    crate::handoff::decode_base64(&attachment.base64).ok_or(PeerError::Frame)?;
                attachment.base64 = boundary
                    .inspect_text(&bytes)
                    .map_err(|_| PeerError::Frame)?
                    .to_owned();
                texts.push(&mut attachment.base64);
            }
        }
        _ => {}
    }
    boundary.texts(&mut texts).map_err(|_| PeerError::Frame)?;
    if let PeerAction::SessionHandoff { input, .. } = &mut outbound.action {
        for attachment in &mut input.attachments {
            attachment.base64 = crate::handoff::encode_base64(attachment.base64.as_bytes());
        }
    }
    let mut input = serde_json::to_vec(&outbound).map_err(|_| PeerError::Frame)?;
    if input.len() >= MAX_FRAME {
        return Err(PeerError::RequestTooLarge);
    }
    input.push(b'\n');
    Ok(input)
}

impl SshTransport<'_> {
    pub fn exchange_with_cap(
        &self,
        peer: &Peer,
        request: &PeerRequest,
        budget: WaitBudget,
        cancel: &CancelToken,
        cap: Duration,
    ) -> Result<PeerResponse, PeerError> {
        self.exchange_bounded(peer, request, budget, cancel, cap, None)
    }
    /// One exchange that is over, SSH termination and reap included, within
    /// `within` of starting, however the whole-second `budget` rounds. The
    /// cleanup grace is carved out of `within` rather than added to it.
    pub fn exchange_within(
        &self,
        peer: &Peer,
        request: &PeerRequest,
        budget: WaitBudget,
        cancel: &CancelToken,
        within: Duration,
    ) -> Result<PeerResponse, PeerError> {
        let cap = Duration::from_secs(self.seconds);
        self.exchange_bounded(peer, request, budget, cancel, cap, Some(within))
    }
    fn exchange_bounded(
        &self,
        peer: &Peer,
        request: &PeerRequest,
        budget: WaitBudget,
        cancel: &CancelToken,
        cap: Duration,
        within: Option<Duration>,
    ) -> Result<PeerResponse, PeerError> {
        if cancel.is_cancelled() {
            return Err(PeerError::Cancelled);
        }
        // Config validation is repeated at this public boundary before argv.
        if peer.host != request.target_host
            || peer.destination.is_empty()
            || peer.destination.starts_with('-')
            || !peer
                .destination
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
            || !self.executable.is_absolute()
            || peer
                .remote_executable
                .as_deref()
                .is_some_and(|path| !crate::config::valid_executable_override(path))
            || !(1..=30).contains(&peer.connect_timeout_seconds)
        {
            return Err(PeerError::Project);
        }
        let input = encode_peer_request(request)?;
        let mut timeout =
            Duration::from_micros(budget.remaining(self.clock.mono_now()).as_micros())
                .min(Duration::from_secs(self.seconds))
                .min(cap);
        // Termination and reap follow the timeout, so a deadline leaves them
        // their share (at most the usual grace, a quarter of it each when short).
        let cleanup = within.map(|within| {
            let quarter = within / 4;
            let grace = (TERMINATE_GRACE.min(quarter), REAP_TIMEOUT.min(quarter));
            timeout = timeout.min(within.saturating_sub(grace.0 + grace.1));
            grace
        });
        if timeout.is_zero() {
            // An exhausted budget is a deadline, not an unavailable home.
            return Err(PeerError::Deadline);
        }
        let remote_executable = peer
            .remote_executable
            .as_deref()
            .and_then(Path::to_str)
            .unwrap_or("provenant");
        let remote_command = format!("{remote_executable} fabric peer");
        let argv: Vec<OsString> = vec![
            self.executable.as_os_str().to_owned(),
            "-T".into(),
            "-oBatchMode=yes".into(),
            format!("-oConnectTimeout={}", peer.connect_timeout_seconds).into(),
            "--".into(),
            peer.destination.clone().into(),
            remote_command.into(),
        ];
        let mut env = vec![
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("HOME".into(), self.home.into()),
            ("LANG".into(), "C".into()),
        ];
        if let Some(socket) = std::env::var_os("SSH_AUTH_SOCK") {
            env.push(("SSH_AUTH_SOCK".into(), socket));
        }
        let mut options = ExecOptions::new(&argv, self.cwd, timeout);
        options.env = Some(&env);
        options.stdin = Some(&input);
        options.stdout = StreamPolicy::Capture { limit: MAX_FRAME };
        options.stderr = StreamPolicy::Capture { limit: 4096 };
        options.cancel = Some(cancel);
        options.stop_on_output_limit = true;
        if let Some((terminate_grace, reap_timeout)) = cleanup {
            options.terminate_grace = terminate_grace;
            options.reap_timeout = reap_timeout;
        }
        let uncertain = |cause: String| {
            request
                .operation()
                .map_or(PeerError::Failed(cause), |operation_id| {
                    PeerError::Unknown { operation_id }
                })
        };
        let outcome = self
            .executor
            .execute(options)
            .map_err(|error| uncertain(format!("ssh could not be run: {error}")))?;
        if outcome.cancelled {
            return Err(uncertain("the request was cancelled".to_owned()));
        }
        if outcome.timed_out {
            return Err(uncertain(format!(
                "ssh timed out after {}s",
                timeout.as_secs().max(1)
            )));
        }
        if let Termination::Exit(code) = outcome.termination {
            if code != 0 {
                let line = stderr_line(&outcome.stderr.bytes);
                return Err(uncertain(if line.is_empty() {
                    format!("ssh exited with status {code}")
                } else {
                    format!("ssh exited with status {code}: {line}")
                }));
            }
        } else if let Termination::Signal(signal) = outcome.termination {
            return Err(uncertain(format!("ssh was killed by signal {signal}")));
        }
        if outcome.stdout.truncated || outcome.stdout.read_failed || outcome.drain_truncated {
            return Err(uncertain("peer reply was truncated".to_owned()));
        }
        let response: PeerResponse<serde_json::Value> =
            provenant_records::decode(outcome.stdout.bytes.as_slice(), Limits::CONFIG)
                .map_err(|_| uncertain("peer reply is not a valid frame".to_owned()))?;
        if response.version != 1
            || response.host != peer.host
            || response.project != request.project.key
        {
            return Err(PeerError::Version);
        }
        if response.protocol != request.protocol {
            return Err(PeerError::ProtocolMismatch {
                peer: peer.destination.clone(),
                remote: response.protocol,
                expected: request.protocol,
            });
        }
        let result = serde_json::from_value(response.result)
            .map_err(|_| uncertain("peer reply is not a valid frame".to_owned()))?;
        if let PeerResult::Health { data: health, .. } = &result
            && health.host != peer.host
        {
            return Err(PeerError::Version);
        }
        Ok(PeerResponse {
            extra: response.extra,
            version: response.version,
            protocol: response.protocol,
            host: response.host,
            project: response.project,
            result,
        })
    }
}
impl PeerTransport for SshTransport<'_> {
    fn exchange(
        &self,
        peer: &Peer,
        request: &PeerRequest,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResponse, PeerError> {
        self.exchange_with_cap(
            peer,
            request,
            budget,
            cancel,
            Duration::from_secs(self.seconds),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{MonoTime, TestClock, WallTime};
    use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    fn project_key(home: &Path) -> CrossHostProjectKey {
        CrossHostProjectKey::from_home(home, &home.join("repo/.git")).unwrap()
    }

    #[test]
    fn typed_output_frame_round_trips_graph_and_extension_fields() {
        let host = HostId::from_bytes([3; 16]);
        let origin = provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([4; 16]),
            "graph",
            "review",
            2,
        )
        .unwrap();
        let output = facade::OutputResult::Chunk {
            peer: provenant_records::run::PeerGraphFields {
                peer_graph: Some(provenant_records::run::PeerGraph {
                    host,
                    graph_id: "graph".into(),
                }),
                peer_graph_collected: Some(true),
                peer_origin_work_key: Some(origin.clone()),
                ..Default::default()
            },
            length: Some(42),
            planner_fallback: Some("bounded planner failure".into()),
            extra: [(
                "future_output_field".into(),
                serde_json::json!({"kept":true}),
            )]
            .into(),
            target: facade::AttemptTarget {
                extra: Default::default(),
                attempt: provenant_core::AttemptId::from_bytes([5; 16]),
                host: Some(host),
            },
            part: facade::OutputPart::Result,
            state: "succeeded".into(),
            reason: None,
            offset: 0,
            next_offset: 2,
            eof: true,
            digest: provenant_core::Digest::from_bytes([6; 32]),
            bytes: facade::OutputBytes::Utf8("ok".into()),
        };
        let response = PeerResponse {
            extra: [("future_response_field".into(), serde_json::json!("kept"))].into(),
            version: 1,
            protocol: PROTOCOL,
            host,
            project: project_key(Path::new("/home/a")),
            result: PeerResult::Output {
                data: Box::new(output),
                extra: [("future_result_field".into(), serde_json::json!(true))].into(),
            },
        };

        // Production writes this typed response directly; the strict frame
        // decoder must see every field once and retain each layer's owner.
        let frame = serde_json::to_vec(&response).unwrap();
        let decoded: PeerResponse =
            provenant_records::decode(frame.as_slice(), Limits::CONFIG).unwrap();
        assert_eq!(decoded.extra["future_response_field"], "kept");
        let PeerResult::Output { data, extra } = decoded.result else {
            panic!("output response expected");
        };
        assert_eq!(extra["future_result_field"], true);
        let facade::OutputResult::Chunk {
            peer,
            length,
            planner_fallback,
            extra,
            ..
        } = *data
        else {
            panic!("output chunk expected");
        };
        assert_eq!(peer.peer_origin_work_key, Some(origin));
        assert_eq!(peer.peer_graph.unwrap().graph_id, "graph");
        assert_eq!(peer.peer_graph_collected, Some(true));
        assert_eq!(length, Some(42));
        assert_eq!(planner_fallback.as_deref(), Some("bounded planner failure"));
        assert_eq!(
            extra["future_output_field"],
            serde_json::json!({"kept":true})
        );
    }

    #[test]
    fn peer_run_subtree_cursor_round_trips_and_rejects_wrong_types() {
        let action = PeerAction::Run {
            extra: [("future_action_fact".into(), serde_json::json!(true))].into(),
            input: facade::RunInput::Status {
                extra: Default::default(),
                target: facade::AttemptTarget {
                    extra: Default::default(),
                    attempt: provenant_core::AttemptId::from_bytes([9; 16]),
                    host: None,
                },
            },
            peer_subtree_after: Some(3),
        };
        let encoded = serde_json::to_value(&action).unwrap();
        assert_eq!(encoded["peer_subtree_after"], 3);
        let decoded: PeerAction = serde_json::from_value(encoded.clone()).unwrap();
        let round_trip = serde_json::to_value(decoded).unwrap();
        assert_eq!(round_trip["peer_subtree_after"], 3);
        assert_eq!(round_trip["future_action_fact"], true);
        let mut malformed = encoded;
        malformed["peer_subtree_after"] = serde_json::json!("three");
        assert!(serde_json::from_value::<PeerAction>(malformed).is_err());
    }

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn output_audience_requires_graph_evidence_and_new_protocol() {
        let action = declared_feature_actions()
            .into_iter()
            .find(|(name, _, _)| *name == "output")
            .unwrap()
            .2;
        let mut wire = serde_json::to_value(&action).unwrap();
        assert_eq!(action_fence(&action), OUTPUT_AUDIENCE_PROTOCOL);
        assert_eq!(
            feature_protocol_for_wire_name("output"),
            OUTPUT_AUDIENCE_PROTOCOL
        );
        assert!(serde_json::from_value::<PeerAction>(wire.clone()).is_ok());
        wire.as_object_mut().unwrap().remove("audience");
        assert!(serde_json::from_value::<PeerAction>(wire.clone()).is_err());
        wire["audience"] = serde_json::json!({"kind":"graph_execution"});
        assert!(serde_json::from_value::<PeerAction>(wire).is_err());
    }

    #[test]
    fn graph_peer_contexts_round_trip_and_reject_malformed_values() {
        let submit = GraphSubmitContext {
            launch_actor: ActorId::from_bytes([1; 16]),
            attempt: crate::graph::AttemptSpec::default(),
            retry_limit: Some(2),
            network: Some(provenant_records::run::NetworkAccess::Local),
        };
        let encoded = serde_json::to_value(&submit).unwrap();
        assert_eq!(
            serde_json::from_value::<GraphSubmitContext>(encoded.clone()).unwrap(),
            submit
        );
        for (field, bad) in [
            ("launch_actor", serde_json::json!(false)),
            ("attempt", serde_json::json!(false)),
            ("retry_limit", serde_json::json!("many")),
            ("network", serde_json::json!("unlisted")),
        ] {
            let mut malformed = encoded.clone();
            malformed[field] = bad;
            assert!(serde_json::from_value::<GraphSubmitContext>(malformed).is_err());
        }

        let receipt = GraphReceiptContext {
            launch_actor: ActorId::from_bytes([2; 16]),
            origin_work_key: provenant_records::WorkKey::new(
                provenant_core::ProjectId::from_bytes([5; 16]),
                "G",
                "N",
                1,
            )
            .unwrap(),
        };
        let encoded = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serde_json::from_value::<GraphReceiptContext>(encoded.clone()).unwrap(),
            receipt
        );
        let mut malformed = encoded;
        malformed["origin_work_key"] = serde_json::json!(false);
        assert!(serde_json::from_value::<GraphReceiptContext>(malformed).is_err());

        let cancel = GraphCancelContext {
            launch_actor: ActorId::from_bytes([3; 16]),
        };
        let encoded = serde_json::to_value(&cancel).unwrap();
        assert_eq!(
            serde_json::from_value::<GraphCancelContext>(encoded.clone()).unwrap(),
            cancel
        );
        let mut malformed = encoded;
        malformed["launch_actor"] = serde_json::json!(false);
        assert!(serde_json::from_value::<GraphCancelContext>(malformed).is_err());
    }

    #[test]
    fn peer_refusals_preserve_typed_errors_and_reject_unknown_codes() {
        let error = facade::FacadeError::LaunchUnknown {
            operation_id: OperationId::parse("typed-error-operation").unwrap(),
        };
        let value = serde_json::to_value(PeerResult::Error {
            data: crate::hosts::PeerResultError {
                extra: Default::default(),
                error: error.clone(),
            },
            extra: Default::default(),
        })
        .unwrap();
        // The peer wire keeps its own tag; only the code a caller sees changed spelling.
        assert_eq!(value["data"]["error"]["code"], "launch_unknown");
        assert!(matches!(
            serde_json::from_value::<PeerResult>(value).unwrap(),
            PeerResult::Error { data: crate::hosts::PeerResultError { error: actual, .. }, .. } if actual == error
        ));
        for error in [
            serde_json::json!({"code": "unknown_error"}),
            serde_json::json!({"code": "launch_unknown"}),
        ] {
            assert!(serde_json::from_value::<facade::FacadeError>(error).is_err());
        }
    }

    #[test]
    fn recorded_peer_responses_refuse_retired_protocols() {
        let response = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL - 1,
            host: HostId::from_bytes([3; 16]),
            project: project_key(Path::new("/home/a")),
            result: serde_json::json!({"result": "error", "data": {"error": {"code": "busy"}}}),
        };
        assert!(matches!(
            decode_recorded_response(&serde_json::to_vec(&response).unwrap()),
            Err(PeerError::Version)
        ));
    }

    #[test]
    fn protocol_fence_precedes_action_decoding() {
        let mut request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL - 1,
            target_host: HostId::from_bytes([3; 16]),
            source_host: HostId::from_bytes([4; 16]),
            agent: "fixture".into(),
            project: ProjectLocator {
                extra: Default::default(),
                key: project_key(Path::new("/home/a")),
                common_dir: "repo/.git".into(),
            },
            action: serde_json::json!({"action": "future_action", "build_heavy": true}),
            trace: Trace::default(),
        };
        for protocol in [6, PROTOCOL - 1] {
            for action in [
                serde_json::json!({"action":"future_action"}),
                serde_json::json!({"action":"submit", "input":{"op":"submit", "task":"invalid shape", "origin_work_key":"new payload"}}),
            ] {
                request.protocol = protocol;
                request.action = action;
                let bytes = serde_json::to_vec(&request).unwrap();
                assert!(decode_request(&bytes).is_err());
                let response = protocol_refusal(&bytes).unwrap().unwrap();
                assert_eq!(response.protocol, PROTOCOL);
                assert!(matches!(
                    response.result,
                    PeerResult::Error {
                        data: crate::hosts::PeerResultError {
                            error: facade::FacadeError::PeerVersion,
                            ..
                        },
                        ..
                    }
                ));
            }
        }
    }

    #[test]
    fn validate_refuses_the_target_as_its_own_source() {
        let temp = crate::test_support::tempdir().unwrap();
        let home = temp.path();
        let local = HostId::from_bytes([1; 16]);
        let remote = HostId::from_bytes([2; 16]);
        let mut config = FabricConfig {
            agent_label: "fixture".into(),
            client_label: "fixture".into(),
            commands: Default::default(),
            host_native: Default::default(),
            ssh_executable: None,
            home_hosts: Default::default(),
            peers: Default::default(),
            graph: Default::default(),
            lanes: Default::default(),
            plan: Default::default(),
            implement: Default::default(),
            steer: Default::default(),
            limits: crate::config::Limits {
                body_bytes: 65536,
                input_frame_bytes: 1048576,
                json_depth: 64,
                page_rows: 100,
                output_lines: 500,
                output_bytes: 65536,
                result_bytes: 1048576,
                peer_seconds: 30,
                graph_timeout_seconds: 14400,
                store_busy_timeout_ms: 30_000,
            },
        };
        for (name, host) in [("self", local), ("fixture", remote)] {
            config.peers.insert(
                name.into(),
                Peer {
                    host,
                    destination: name.into(),
                    remote_executable: None,
                    connect_timeout_seconds: 10,
                },
            );
        }
        let mut request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            target_host: local,
            source_host: local,
            trace: Default::default(),
            agent: "fixture".into(),
            project: ProjectLocator {
                extra: Default::default(),
                key: project_key(home),
                common_dir: "repo/.git".into(),
            },
            action: PeerAction::Health {
                extra: Default::default(),
            },
        };
        assert!(matches!(
            request.validate(local, &config, home),
            Err(PeerError::SourceHost)
        ));
        assert_eq!(
            request.validate(local, &config, home).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            request
                .validate(local, &config, home)
                .unwrap_err()
                .to_string(),
            "peer source host must be a configured peer other than this host"
        );
        request.source_host = remote;
        assert!(request.validate(local, &config, home).is_ok());
        // A trace that names another host than the sender is refused.
        request.trace.host = Some(local);
        assert!(matches!(
            request.validate(local, &config, home),
            Err(PeerError::SourceHost)
        ));
        request.trace.host = Some(remote);
        assert!(request.validate(local, &config, home).is_ok());
        request.source_host = HostId::from_bytes([3; 16]);
        assert!(matches!(
            request.validate(local, &config, home),
            Err(PeerError::SourceHost)
        ));
    }

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn cross_host_submit_projects_credentials_before_encoding() {
        let local = HostId::from_bytes([1; 16]);
        let remote = HostId::from_bytes([2; 16]);
        let actor = crate::identity::Address::new(local, "origin")
            .unwrap()
            .actor;
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/h")),
            common_dir: "repo/.git".into(),
        };
        let secret = "submit-private-value";
        let input = serde_json::from_value(serde_json::json!({
            "op":"submit", "operation_id":"submit-test",
            "task":{"prompt":format!("token={secret}"), "access":"read_only",
                "route":{"kind":"model","provider":"fixture","model":"fixture/model"}, "timeout_seconds":30}
        })).unwrap();
        let request = PeerRequest::new(
            remote,
            local,
            "worker".into(),
            project,
            PeerAction::Submit {
                extra: Default::default(),
                graph_context: Box::new(GraphSubmitContext {
                    launch_actor: actor,
                    attempt: Default::default(),
                    retry_limit: None,
                    network: None,
                }),
                input: Box::new(input),
            },
        );
        let encoded = encode_peer_request(&request).unwrap();
        let wire = std::str::from_utf8(&encoded).unwrap();
        assert!(!wire.contains(secret));
        assert!(wire.contains("[REDACTED]"));
        assert!(serde_json::to_string(&request).unwrap().contains(secret));

        let mut prose = request.clone();
        if let PeerAction::Submit { input, .. } = &mut prose.action {
            let facade::SubmitInput::Submit { task, .. } = input.as_mut();
            task.prompt = "token: it; it stays outside; token: ordinary; ordinary stays".into();
            task.title = Some("it ordinary submit".into());
        }
        let wire: serde_json::Value =
            serde_json::from_slice(&encode_peer_request(&prose).unwrap()).unwrap();
        let mut expected = serde_json::to_value(&prose).unwrap();
        expected["action"]["input"]["task"]["prompt"] = serde_json::json!(
            "token: [REDACTED]; it stays outside; token: [REDACTED]; ordinary stays"
        );
        assert_eq!(wire, expected);

        // A typed value shared with an operation ID is masked only in text.
        if let PeerAction::Submit { input, .. } = &mut prose.action {
            let facade::SubmitInput::Submit { task, .. } = input.as_mut();
            task.prompt = r#"{"token":"submit-test"} copy submit-test"#.into();
            task.title = Some("submit-test".into());
        }
        let wire = encode_peer_request(&prose).unwrap();
        let decoded = decode_request(&wire).unwrap();
        let actual: serde_json::Value = serde_json::to_value(decoded).unwrap();
        let mut expected = serde_json::to_value(prose).unwrap();
        expected["action"]["input"]["task"]["prompt"] =
            serde_json::json!(r#"{"token":"[REDACTED]"} copy [REDACTED]"#);
        expected["action"]["input"]["task"]["title"] = serde_json::json!("[REDACTED]");
        assert_eq!(actual, expected);
    }

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn cross_host_response_projects_only_declared_text_fields() {
        let remote = HostId::from_bytes([2; 16]);
        let projector = provenant_confinement::publication::Projector::new([]).unwrap();
        for (message, projected) in [
            ("token: it; it remains", "token: [REDACTED]; it remains"),
            (
                r#"{"token":"submit-test"} copy submit-test"#,
                r#"{"token":"[REDACTED]"} copy [REDACTED]"#,
            ),
        ] {
            let response = PeerResponse {
                extra: [("submit-test".into(), serde_json::json!("submit-test"))].into(),
                version: 1,
                protocol: PROTOCOL,
                host: remote,
                project: project_key(Path::new("/h")),
                result: PeerResult::Inbox {
                    extra: Default::default(),
                    data: crate::inbox::InboxReply::Error {
                        extra: Default::default(),
                        code: "submit-test".into(),
                        message: message.into(),
                    },
                },
            };
            let mut expected = serde_json::to_value(&response).unwrap();
            expected["result"]["data"]["message"] = serde_json::json!(projected);
            let wire = encode_peer_response(response, &projector).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
                expected
            );
            serde_json::from_slice::<PeerResponse>(&wire).unwrap();
        }
        let data = serde_json::json!({
            "warnings": [r#"{"token":"submit-test"}"#],
            "managed_error":"submit-test token: it; it stays",
            "extension":{"message":"submit-test", "id":"it"},
            "message":"submit-test"
        });
        let response = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            host: remote,
            project: project_key(Path::new("/h")),
            result: PeerResult::Doctor {
                data,
                extra: Default::default(),
            },
        };
        let mut expected = serde_json::to_value(&response).unwrap();
        expected["result"]["data"]["warnings"][0] = serde_json::json!(r#"{"token":"[REDACTED]"}"#);
        expected["result"]["data"]["managed_error"] =
            serde_json::json!("[REDACTED] token: [REDACTED]; it stays");
        let wire = encode_peer_response(response, &projector).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
            expected
        );
        let data = serde_json::json!({
            "lanes":[{"metadata":{"attempt":{
                "title":r#"{"token":"submit-test"}"#,
                "state":{"state":"terminal", "reason":"submit-test token: it; it remains"},
                "applied_route":{"account_key":"submit-test", "launch_warnings":["submit-test"]},
                "extension":{"title":"submit-test"}
            }}}],
            "delivery":{"state":{"state":"failed", "reason":"submit-test"}},
            "extension":{"reason":"submit-test"}
        });
        let response = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            host: remote,
            project: project_key(Path::new("/h")),
            result: PeerResult::Console {
                data,
                extra: Default::default(),
            },
        };
        let mut expected = serde_json::to_value(&response).unwrap();
        let attempt = &mut expected["result"]["data"]["lanes"][0]["metadata"]["attempt"];
        attempt["title"] = serde_json::json!(r#"{"token":"[REDACTED]"}"#);
        attempt["state"]["reason"] = serde_json::json!("[REDACTED] token: [REDACTED]; it remains");
        attempt["applied_route"]["launch_warnings"][0] = serde_json::json!("[REDACTED]");
        expected["result"]["data"]["delivery"]["state"]["reason"] = serde_json::json!("[REDACTED]");
        let wire = encode_peer_response(response, &projector).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
            expected
        );
    }

    #[test]
    fn new_requests_trace_the_sender_and_the_operation() {
        let (local, remote) = (HostId::from_bytes([1; 16]), HostId::from_bytes([2; 16]));
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/h")),
            common_dir: "repo/.git".into(),
        };
        let operation = OperationId::parse("note-once").unwrap();
        let note: PeerAction = serde_json::from_value(serde_json::json!({
            "action": "coordination",
            "input": {"domain": "events", "input": {"op": "note", "note": "n", "dedup_key": null}},
            "operation_id": "note-once",
        }))
        .unwrap();
        let request = PeerRequest::new(remote, local, "worker".into(), project.clone(), note);
        assert_eq!(request.trace.host, Some(local));
        assert_eq!(request.trace.actor, Some(request.actor().unwrap()));
        assert_eq!(request.trace.operation, Some(operation));
        let health = PeerRequest::new(
            remote,
            local,
            "worker".into(),
            project,
            PeerAction::Health {
                extra: Default::default(),
            },
        );
        assert_eq!(health.trace.operation, None);
        assert_eq!((health.version, health.protocol), (1, PROTOCOL));
        assert_eq!(health.target_host, remote);
    }

    struct Ssh {
        request: Mutex<Option<PeerRequest>>,
        lose_reply: bool,
        calls: AtomicUsize,
        expected_executable: OsString,
        expected_connect_timeout: OsString,
        expected_remote_command: OsString,
        expected_timeout: Option<Duration>,
    }
    impl Executor for Ssh {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(options.argv.first(), Some(&self.expected_executable));
            assert!(options.argv.iter().any(|a| a == "-oBatchMode=yes"));
            assert!(
                options
                    .argv
                    .iter()
                    .any(|a| a == &self.expected_connect_timeout)
            );
            assert_eq!(options.argv.last(), Some(&self.expected_remote_command));
            if let Some(expected_timeout) = self.expected_timeout {
                assert_eq!(options.timeout, expected_timeout);
            }
            let request = decode_request(options.stdin.unwrap()).unwrap();
            let response = PeerResponse {
                extra: Default::default(),
                version: 1,
                protocol: PROTOCOL,
                host: request.target_host,
                project: request.project.key.clone(),
                result: PeerResult::Health {
                    data: PeerHealth {
                        extra: Default::default(),
                        host: request.target_host,
                        available: true,
                        execution: false,
                        release: "2.0.0".into(),
                        run_capabilities: crate::runs::RunCapabilities {
                            submit: false,
                            list: true,
                            status: true,
                            wait: true,
                            cancel: true,
                            resume: true,
                            output: true,
                            events: true,
                        },
                        readiness: PeerReadiness::reporting(),
                    },
                    extra: Default::default(),
                },
            };
            let lose_reply =
                self.lose_reply && !matches!(request.action, PeerAction::Health { .. });
            *self.request.lock().unwrap() = Some(request);
            Ok(ExecOutcome {
                termination: Termination::Exit(if lose_reply { 255 } else { 0 }),
                stdout: CapturedStream {
                    bytes: serde_json::to_vec(&response).unwrap(),
                    ..Default::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }
    /// The `PeerTransport` contract every implementation honours: a health
    /// exchange answers as the addressed host for the requested project, a
    /// cancelled token is refused before any peer contact, and a request
    /// addressed to another host than the peer is refused as a project error.
    fn assert_peer_transport_conforms(
        transport: &dyn PeerTransport,
        peer: &Peer,
        health: &PeerRequest,
        budget: WaitBudget,
    ) {
        let response = transport
            .exchange(peer, health, budget, &CancelToken::new())
            .unwrap();
        assert_eq!(response.host, peer.host);
        assert_eq!(response.project, health.project.key);
        assert!(matches!(response.result, PeerResult::Health { .. }));
        let cancelled = CancelToken::new();
        cancelled.cancel();
        assert!(matches!(
            transport.exchange(peer, health, budget, &cancelled),
            Err(PeerError::Cancelled)
        ));
        let mut misaddressed = health.clone();
        misaddressed.target_host = HostId::from_bytes([9; 16]);
        assert!(matches!(
            transport.exchange(peer, &misaddressed, budget, &CancelToken::new()),
            Err(PeerError::Project)
        ));
    }

    #[test]
    fn ssh_transport_conforms_to_the_peer_transport_port() {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let (local, host) = (HostId::from_bytes([1; 16]), HostId::from_bytes([2; 16]));
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        };
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        };
        let health = PeerRequest::new(
            host,
            local,
            "chair".into(),
            project,
            PeerAction::Health {
                extra: Default::default(),
            },
        );
        let ssh = Ssh {
            request: Mutex::new(None),
            lose_reply: false,
            calls: AtomicUsize::new(0),
            expected_executable: "/usr/bin/ssh".into(),
            expected_connect_timeout: "-oConnectTimeout=10".into(),
            expected_remote_command: "provenant fabric peer".into(),
            expected_timeout: None,
        };
        let transport = SshTransport {
            executor: &ssh,
            clock: &clock,
            home: Path::new("/home/a"),
            cwd: Path::new("/home/a/repo"),
            executable: "/usr/bin/ssh".into(),
            seconds: 30,
        };
        assert_peer_transport_conforms(&transport, &peer, &health, budget);
        // Only the first exchange reached the executor.
        assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fixed_peer_transport_preserves_mutation_uncertainty_without_retry() {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let local = HostId::from_bytes([1; 16]);
        let host = HostId::from_bytes([2; 16]);
        let operation = OperationId::parse("mail-once").unwrap();
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            trace: Default::default(),
            agent: "chair".into(),
            project,
            action: PeerAction::Coordination {
                extra: Default::default(),
                input: CoordinationInput::Events {
                    input: EventsInput::Note {
                        extra: Default::default(),
                        note: "hello".into(),
                        dedup_key: None,
                    },
                    extra: Default::default(),
                },
                operation_id: Some(operation.clone()),
            },
        };
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        };
        let ssh = Ssh {
            request: Mutex::new(None),
            lose_reply: true,
            calls: AtomicUsize::new(0),
            expected_executable: "/usr/bin/ssh".into(),
            expected_connect_timeout: "-oConnectTimeout=10".into(),
            expected_remote_command: "provenant fabric peer".into(),
            expected_timeout: None,
        };
        let transport = SshTransport {
            executor: &ssh,
            clock: &clock,
            home: Path::new("/home/a"),
            cwd: Path::new("/home/a/repo"),
            executable: "/usr/bin/ssh".into(),
            seconds: 30,
        };
        assert!(
            matches!(transport.exchange(&peer,&request,budget,&CancelToken::new()),Err(PeerError::Unknown{operation_id}) if operation_id==operation)
        );
        assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            ssh.request.lock().unwrap().as_ref().unwrap().operation(),
            Some(operation.clone())
        );
        assert!(request.project.resolve(Path::new("/home/b")).is_ok());

        // Submit has its own operation-id path. An expired deadline must not
        // contact SSH; once contacted, a lost reply is an unknown launch.
        let mut submit = request.clone();
        let task = serde_json::from_value(serde_json::json!({
            "prompt": "inspect the project",
            "route": {"kind": "model", "provider": "fixture", "model": "fixture/model"},
            "timeout_seconds": 30
        }))
        .unwrap();
        submit.action = PeerAction::Submit {
            extra: Default::default(),
            graph_context: Box::new(GraphSubmitContext {
                launch_actor: ActorId::from_bytes([9; 16]),
                attempt: crate::graph::AttemptSpec::default(),
                retry_limit: None,
                network: None,
            }),
            input: Box::new(facade::SubmitInput::Submit {
                extra: Default::default(),
                origin_work_key: None,
                operation_id: operation.clone(),
                task,
                knobs: Vec::new(),
                host: None,
            }),
        };
        assert!(matches!(
            transport.exchange_within(&peer, &submit, budget, &CancelToken::new(), Duration::ZERO),
            Err(PeerError::Deadline)
        ));
        assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            transport.exchange(&peer, &submit, budget, &CancelToken::new()),
            Err(PeerError::Unknown { operation_id }) if operation_id == operation
        ));
        assert_eq!(ssh.calls.load(Ordering::SeqCst), 2);
    }

    struct Canned(Mutex<Option<Result<ExecOutcome, ExecError>>>);
    impl Executor for Canned {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let request = decode_request(options.stdin.unwrap()).unwrap();
            let expected = action_fence(&request.action);
            assert_eq!(request.protocol, expected);
            self.0.lock().unwrap().take().unwrap()
        }
    }
    fn outcome(exit: i64, stderr: &str, timed_out: bool) -> ExecOutcome {
        ExecOutcome {
            termination: Termination::Exit(exit),
            stdout: CapturedStream::default(),
            stderr: CapturedStream {
                bytes: stderr.as_bytes().to_vec(),
                ..Default::default()
            },
            timed_out,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        }
    }
    fn read_only_failure(result: Result<ExecOutcome, ExecError>) -> PeerError {
        exchange_failure(result, false)
    }
    fn exchange_failure(result: Result<ExecOutcome, ExecError>, mutation: bool) -> PeerError {
        let executor = Canned(Mutex::new(Some(result)));
        exchange_error(&executor, mutation)
    }
    fn exchange_error(executor: &dyn Executor, mutation: bool) -> PeerError {
        let action = if mutation {
            PeerAction::Coordination {
                extra: Default::default(),
                input: CoordinationInput::Events {
                    input: EventsInput::Note {
                        extra: Default::default(),
                        note: "must not run".into(),
                        dedup_key: None,
                    },
                    extra: Default::default(),
                },
                operation_id: Some(OperationId::parse("wrong-version").unwrap()),
            }
        } else {
            PeerAction::RunStatuses {
                extra: Default::default(),
                targets: vec![],
            }
        };
        exchange_action_error(executor, action)
    }
    fn exchange_action_error(executor: &dyn Executor, action: PeerAction) -> PeerError {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let host = HostId::from_bytes([2; 16]);
        let request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: action_fence(&action),
            target_host: host,
            source_host: HostId::from_bytes([1; 16]),
            trace: Default::default(),
            agent: "chair".into(),
            project: ProjectLocator {
                extra: Default::default(),
                key: project_key(Path::new("/home/a")),
                common_dir: "repo/.git".into(),
            },
            action,
        };
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        };
        let transport = SshTransport {
            executor,
            clock: &clock,
            home: Path::new("/home/a"),
            cwd: Path::new("/home/a/repo"),
            executable: "/usr/bin/ssh".into(),
            seconds: 30,
        };
        transport
            .exchange(&peer, &request, budget, &CancelToken::new())
            .unwrap_err()
    }
    #[test]
    fn console_peer_invalid_frames_fail_and_unreachable() {
        for action in [
            PeerAction::ConsoleBoard {
                extra: Default::default(),
                all: false,
                limit: 50,
                selected: false,
            },
            PeerAction::ConsoleTree {
                extra: Default::default(),
                max_depth: 8,
                max_nodes: 100,
                detail: false,
                selected: false,
            },
        ] {
            let old = Canned(Mutex::new(Some(Ok(outcome(
                3,
                "provenant: fabric peer: invalid peer frame\n",
                false,
            )))));
            assert!(matches!(
                exchange_action_error(&old, action.clone()),
                PeerError::Failed(_)
            ));
            let offline = Canned(Mutex::new(Some(Ok(outcome(
                255,
                "connection refused",
                false,
            )))));
            assert!(matches!(
                exchange_action_error(&offline, action),
                PeerError::Failed(_)
            ));
        }
    }
    #[test]
    fn console_inbox_invalid_frame_fails_closed() {
        let action = PeerAction::ConsoleInbox {
            extra: Default::default(),
            state: crate::inbox::InboxFilter::Open,
            limit: 50,
        };
        let old = Canned(Mutex::new(Some(Ok(outcome(
            3,
            "provenant: fabric peer: invalid peer frame\n",
            false,
        )))));
        assert!(matches!(
            exchange_action_error(&old, action.clone()),
            PeerError::Failed(_)
        ));
        let offline = Canned(Mutex::new(Some(Ok(outcome(
            255,
            "connection refused",
            false,
        )))));
        assert!(matches!(
            exchange_action_error(&offline, action),
            PeerError::Failed(_)
        ));
    }
    #[test]
    fn peer_feature_declaration_drives_every_protocol_fence() {
        let features = declared_feature_actions();
        let newest = features
            .iter()
            .map(|(_, protocol, _)| *protocol)
            .max()
            .unwrap();
        assert_eq!(newest_feature_protocol(), newest);

        for (wire_name, protocol, action) in features {
            assert_eq!(action_fence(&action), protocol, "{wire_name}");
            let request = PeerRequest::new(
                HostId::from_bytes([2; 16]),
                HostId::from_bytes([1; 16]),
                "chair".into(),
                ProjectLocator {
                    extra: Default::default(),
                    key: project_key(Path::new("/home/a")),
                    common_dir: "repo/.git".into(),
                },
                action,
            );
            assert_eq!(request.protocol, protocol, "{wire_name}");
            let mut value = serde_json::to_value(request).unwrap();
            assert_eq!(value["action"]["action"], wire_name);
            assert!(
                protocol_refusal(&serde_json::to_vec(&value).unwrap())
                    .unwrap()
                    .is_none(),
                "{wire_name} at its declared fence"
            );

            value["protocol"] = (protocol + 1).into();
            let refusal = protocol_refusal(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(refusal.protocol, protocol, "{wire_name}");
        }
        assert_eq!(feature_protocol_for_wire_name("health"), PROTOCOL);
    }
    #[test]
    fn read_only_failures_name_their_cause_on_the_same_wire_code() {
        let error = read_only_failure(Ok(outcome(
            255,
            "\nssh: connect to host w port 22: Connection refused\nmore",
            false,
        )));
        assert_eq!(error.code(), "home_unavailable");
        let text = error.to_string();
        assert!(text.contains("status 255"), "{text}");
        assert!(text.contains("Connection refused"), "{text}");
        assert!(!text.contains("more"), "{text}");
        assert!(
            read_only_failure(Ok(outcome(0, "", true)))
                .to_string()
                .contains("timed out")
        );
        assert!(
            read_only_failure(Err(ExecError::Busy))
                .to_string()
                .contains("could not be run")
        );
        let bad = read_only_failure(Ok(outcome(0, "", false)));
        assert!(bad.to_string().contains("valid frame"), "{bad}");
    }

    #[test]
    fn missing_peer_protocol_fields_are_rejected() {
        let host = HostId::from_bytes([2; 16]);
        let mut request = serde_json::to_value(PeerRequest::new(
            host,
            HostId::from_bytes([1; 16]),
            "chair".into(),
            ProjectLocator {
                extra: Default::default(),
                key: project_key(Path::new("/home/a")),
                common_dir: "repo/.git".into(),
            },
            PeerAction::Health {
                extra: Default::default(),
            },
        ))
        .unwrap();
        request.as_object_mut().unwrap().remove("protocol");
        assert!(matches!(
            decode_request(&serde_json::to_vec(&request).unwrap()),
            Err(PeerError::Frame)
        ));

        let mut response = serde_json::json!({
            "version": 1,
            "protocol": PROTOCOL,
            "host": host,
            "project": project_key(Path::new("/home/a")),
            "result": {"result": "health", "data": {"host": host}},
        });
        response.as_object_mut().unwrap().remove("protocol");
        assert!(
            provenant_records::decode::<PeerResponse<serde_json::Value>, _>(
                serde_json::to_vec(&response).unwrap().as_slice(),
                Limits::CONFIG,
            )
            .is_err()
        );
    }

    #[test]
    fn closed_peer_reply_is_uncertain_and_never_retried() {
        struct ClosedPeer(AtomicUsize);
        impl Executor for ClosedPeer {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                let request = decode_request(options.stdin.unwrap()).unwrap();
                assert_eq!(request.protocol, PROTOCOL);
                Ok(outcome(
                    3,
                    "provenant: fabric peer: peer agent closed without a reply; see peer.log\n",
                    false,
                ))
            }
        }

        for mutation in [false, true] {
            let peer = ClosedPeer(AtomicUsize::new(0));
            let error = exchange_error(&peer, mutation);
            if mutation {
                assert!(matches!(error, PeerError::Unknown { .. }), "{error}");
            } else {
                assert!(matches!(error, PeerError::Failed(_)), "{error}");
            }
            assert_eq!(peer.0.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn frame_errors_have_a_distinct_diagnostic() {
        let frame = PeerError::Frame.to_string();
        assert!(!frame.is_empty());
        assert_ne!(frame, PeerError::Version.to_string());
    }

    #[test]
    fn peer_health_requires_every_format_two_field() {
        let host = HostId::from_bytes([2; 16]);
        let complete = serde_json::json!({
            "host": host,
            "available": true,
            "execution": true,
            "release": "2.0.0",
            "run_capabilities": {
                "submit": true, "list": true, "status": true, "wait": true,
                "cancel": true, "resume": true, "output": true, "events": true
            },
            "readiness": {"state": "unknown", "reason": "not_probed"}
        });
        let decoded: PeerHealth = serde_json::from_value(complete.clone()).unwrap();
        assert_eq!(decoded.release, "2.0.0");
        assert!(decoded.run_capabilities.submit);
        assert_eq!(decoded.readiness.state, PeerReadinessState::Unknown);
        for field in [
            "host",
            "available",
            "execution",
            "release",
            "run_capabilities",
            "readiness",
        ] {
            let mut omitted = complete.clone();
            omitted.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<PeerHealth>(omitted).is_err(),
                "format-2 health accepted a missing {field}"
            );
        }
        let encoded = serde_json::to_value(decoded).unwrap();
        assert!(encoded.get("protocol").is_none());
        assert!(encoded.get("reason").is_none());
    }

    #[test]
    fn protocol_mismatch_names_both_versions_without_decoding_payload() {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        };
        for (requested, action) in [
            (
                PROTOCOL,
                PeerAction::RunStatuses {
                    extra: Default::default(),
                    targets: vec![],
                },
            ),
            (
                PROTOCOL,
                PeerAction::Coordination {
                    extra: Default::default(),
                    input: CoordinationInput::Events {
                        input: EventsInput::Note {
                            extra: Default::default(),
                            note: "hello".into(),
                            dedup_key: None,
                        },
                        extra: Default::default(),
                    },
                    operation_id: Some(OperationId::parse("note").unwrap()),
                },
            ),
            // A feature action raises both the requested fence and the prose
            // beyond the base level.
            (
                DOCTOR_PROTOCOL,
                PeerAction::Doctor {
                    extra: Default::default(),
                },
            ),
        ] {
            for remote in [1, PROTOCOL + 1] {
                let response = PeerResponse {
                    extra: Default::default(),
                    version: 1,
                    protocol: remote,
                    host,
                    project: project.key.clone(),
                    result: serde_json::json!({"result": "future_protocol_payload"}),
                };
                let mut reply = outcome(0, "", false);
                reply.stdout.bytes = serde_json::to_vec(&response).unwrap();
                let executor = Canned(Mutex::new(Some(Ok(reply))));
                let transport = SshTransport {
                    executor: &executor,
                    clock: &clock,
                    home: Path::new("/home/a"),
                    cwd: Path::new("/home/a/repo"),
                    executable: "/usr/bin/ssh".into(),
                    seconds: 30,
                };
                let request = PeerRequest {
                    extra: Default::default(),
                    version: 1,
                    protocol: requested,
                    target_host: host,
                    source_host: HostId::from_bytes([1; 16]),
                    trace: Default::default(),
                    agent: "chair".into(),
                    project: project.clone(),
                    action: action.clone(),
                };
                let peer = Peer {
                    host,
                    destination: "workshop".into(),
                    remote_executable: None,
                    connect_timeout_seconds: 10,
                };
                let error = transport
                    .exchange(
                        &peer,
                        &request,
                        WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                        &CancelToken::new(),
                    )
                    .unwrap_err();
                assert_eq!(error.code(), "peer_version");
                assert_eq!(
                    error.to_string(),
                    format!(
                        "peer workshop speaks protocol {remote}, this host expects protocol {requested}; upgrade both"
                    )
                );
                assert!(executor.0.lock().unwrap().is_none());
            }
        }
    }

    #[test]
    fn health_reports_only_observed_protocols() {
        let mut health = serde_json::json!({
            "host": HostId::from_bytes([2; 16]),
            "available": true, "execution": true,
        });
        let unprobed = serde_json::from_value::<HostHealth>(health.clone()).unwrap();
        assert_eq!(unprobed.protocol, None);
        let unprobed = serde_json::to_value(unprobed).unwrap();
        assert!(unprobed.get("protocol").is_none());
        assert!(unprobed.get("features").is_none());
        health["protocol"] = serde_json::json!("2");
        assert!(serde_json::from_value::<HostHealth>(health.clone()).is_err());
        health["protocol"] = serde_json::json!(PROTOCOL);
        assert_eq!(
            serde_json::from_value::<HostHealth>(health)
                .unwrap()
                .protocol,
            Some(PROTOCOL)
        );
    }

    #[test]
    fn peer_readiness_reads_protocol_mismatch_and_unreachable_peers_as_unknown() {
        let host = HostId::from_bytes([2; 16]);
        let reason = |probe: &Result<PeerResponse, PeerError>| {
            let read = PeerReadiness::from_probe(probe);
            (read.state, read.reason)
        };
        let unknown = |reason: &str| (PeerReadinessState::Unknown, Some(reason.to_owned()));
        // A protocol mismatch remains distinct from a transport failure.
        let mismatch = Err(PeerError::ProtocolMismatch {
            peer: "workshop".into(),
            remote: PROTOCOL - 1,
            expected: PROTOCOL,
        });
        assert_eq!(reason(&mismatch), unknown("unsupported_peer_version"));
        // An unreachable peer, or any failed exchange, reads unknown, never ready.
        for failed in [
            PeerError::Unavailable,
            PeerError::Failed("ssh".into()),
            PeerError::Frame,
        ] {
            assert_eq!(reason(&Err(failed)), unknown("unreachable_peer"));
        }
        // A current peer's report is kept, with its reason bounded and cleaned.
        let response = serde_json::json!({
            "version": 1, "protocol": PROTOCOL, "host": host,
            "project": project_key(Path::new("/home/a")),
            "result": {"result": "health", "data": {
                "host": host, "available": true, "execution": true,
                "release": "2.0.0",
                "run_capabilities": {
                    "submit": true, "list": true, "status": true, "wait": true,
                    "cancel": true, "resume": true, "output": true, "events": true
                },
                "readiness": {"state": "unavailable", "reason": "exhausted\u{202e}", "future": 1}
            }}
        });
        let current: PeerResponse = serde_json::from_value(response).unwrap();
        assert_eq!(
            reason(&Ok(current)),
            (
                PeerReadinessState::Unavailable,
                Some("exhausted".to_owned())
            )
        );
        // The current peer reports its readiness record even before probing routes.
        let sent = PeerHealth {
            extra: Default::default(),
            host,
            available: true,
            execution: true,
            release: "2.0.0".into(),
            run_capabilities: crate::runs::RunCapabilities {
                submit: true,
                list: true,
                status: true,
                wait: true,
                cancel: true,
                resume: true,
                output: true,
                events: true,
            },
            readiness: PeerReadiness::reporting(),
        };
        let value = serde_json::to_value(sent).unwrap();
        assert_eq!(value["readiness"]["state"], "unknown");
        assert_eq!(value["readiness"]["reason"], "not_probed");
        assert!(value.get("extra").is_none());
    }

    #[test]
    fn plain_text_drops_format_and_separator_characters() {
        let text = "a\u{202e}b\u{2066}c\u{200b}d\u{2028}e\u{feff}f\u{1b}g";
        assert_eq!(plain_text(text, 100), "abcdefg");
        assert_eq!(plain_text("abcdef", 3), "abc");
    }

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn session_handoff_fits_one_frame_and_keeps_its_operation_on_a_lost_reply() {
        use crate::handoff::{
            ATTACHMENT_LIMIT, Attachment, PROMPT_HEADER, PROMPT_LIMIT, SessionHandoff,
            encode_base64,
        };
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let local = HostId::from_bytes([1; 16]);
        let host = HostId::from_bytes([2; 16]);
        let operation = OperationId::parse("handoff-1").unwrap();
        // The largest hand-off: a full prompt of characters JSON escapes and
        // the whole attachment allowance.
        let input = SessionHandoff {
            extra: Default::default(),
            operation_id: operation.clone(),
            account: "work".into(),
            model: None,
            prompt: format!(
                "{PROMPT_HEADER}{}",
                "\"".repeat(PROMPT_LIMIT - PROMPT_HEADER.len())
            ),
            branches: (0..64).map(|i| format!("feat/branch-{i}")).collect(),
            attachments: vec![Attachment {
                extra: Default::default(),
                name: "bundle.bin".into(),
                base64: encode_base64(&vec![b'x'; ATTACHMENT_LIMIT]),
            }],
            launch: false,
        };
        input.validate().unwrap();
        let request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            trace: Default::default(),
            agent: "chair".into(),
            project: ProjectLocator {
                extra: Default::default(),
                key: project_key(Path::new("/home/a")),
                common_dir: "repo/.git".into(),
            },
            action: PeerAction::SessionHandoff {
                extra: Default::default(),
                input: Box::new(input.clone()),
                budget_seconds: 30,
            },
        };
        let mut secret_request = request.clone();
        let PeerAction::SessionHandoff {
            input: secret_input,
            ..
        } = &mut secret_request.action
        else {
            unreachable!()
        };
        secret_input.attachments[0].base64 =
            encode_base64(br#"{"token":"attachment-private-value"}"#);
        secret_input.prompt = format!("{PROMPT_HEADER} attachment-private-value");
        let wire = encode_peer_request(&secret_request).unwrap();
        let decoded = decode_request(&wire).unwrap();
        let PeerAction::SessionHandoff {
            input: projected, ..
        } = decoded.action
        else {
            unreachable!()
        };
        assert!(!projected.prompt.contains("attachment-private-value"));
        let content = crate::handoff::decode_base64(&projected.attachments[0].base64).unwrap();
        assert!(
            !String::from_utf8(content.clone())
                .unwrap()
                .contains("attachment-private-value")
        );
        assert!(String::from_utf8(content).unwrap().contains("[REDACTED]"));
        let PeerAction::SessionHandoff {
            input: binary_input,
            ..
        } = &mut secret_request.action
        else {
            unreachable!()
        };
        binary_input.attachments[0].base64 = encode_base64(&[0xff]);
        assert!(encode_peer_request(&secret_request).is_err());
        let frame = serde_json::to_vec(&request).unwrap();
        assert!(frame.len() < MAX_FRAME, "{} bytes", frame.len());
        let mut without_budget = serde_json::to_value(&request).unwrap();
        without_budget["action"]
            .as_object_mut()
            .unwrap()
            .remove("budget_seconds");
        assert!(decode_request(&serde_json::to_vec(&without_budget).unwrap()).is_err());
        let decoded = decode_request(&frame).unwrap();
        assert!(
            matches!(&decoded.action, PeerAction::SessionHandoff { input: got, .. } if **got == input)
        );
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        };
        let ssh = Ssh {
            request: Mutex::new(None),
            lose_reply: true,
            calls: AtomicUsize::new(0),
            expected_executable: "/usr/bin/ssh".into(),
            expected_connect_timeout: "-oConnectTimeout=10".into(),
            expected_remote_command: "provenant fabric peer".into(),
            expected_timeout: None,
        };
        let transport = SshTransport {
            executor: &ssh,
            clock: &clock,
            home: Path::new("/home/a"),
            cwd: Path::new("/home/a/repo"),
            executable: "/usr/bin/ssh".into(),
            seconds: 30,
        };
        assert!(
            matches!(transport.exchange(&peer, &request, budget, &CancelToken::new()), Err(PeerError::Unknown { operation_id }) if operation_id == operation)
        );
        assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn peer_policy_overrides_remote_executable_and_connect_timeout() {
        let ssh_path = Path::new("/bin/sh");
        assert_eq!(
            discover_ssh_executable(Some(ssh_path)).unwrap(),
            ssh_path.to_path_buf()
        );

        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let local = HostId::from_bytes([1; 16]);
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            trace: Default::default(),
            agent: "chair".into(),
            project,
            action: PeerAction::Health {
                extra: Default::default(),
            },
        };
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: Some("/opt/provenant/bin/provenant".into()),
            connect_timeout_seconds: 7,
        };
        let ssh = Ssh {
            request: Mutex::new(None),
            lose_reply: false,
            calls: AtomicUsize::new(0),
            expected_executable: ssh_path.into(),
            expected_connect_timeout: "-oConnectTimeout=7".into(),
            expected_remote_command: "/opt/provenant/bin/provenant fabric peer".into(),
            expected_timeout: None,
        };
        let transport = SshTransport {
            executor: &ssh,
            clock: &clock,
            home: Path::new("/home/a"),
            cwd: Path::new("/home/a/repo"),
            executable: ssh_path.to_path_buf(),
            seconds: 30,
        };

        transport
            .exchange(&peer, &request, budget, &CancelToken::new())
            .unwrap();
        assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn peer_exchange_respects_a_fair_probe_cap() {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let local = HostId::from_bytes([1; 16]);
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            trace: Default::default(),
            agent: "chair".into(),
            project,
            action: PeerAction::Health {
                extra: Default::default(),
            },
        };
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        };
        let ssh = Ssh {
            request: Mutex::new(None),
            lose_reply: false,
            calls: AtomicUsize::new(0),
            expected_executable: "/usr/bin/ssh".into(),
            expected_connect_timeout: "-oConnectTimeout=10".into(),
            expected_remote_command: "provenant fabric peer".into(),
            expected_timeout: Some(Duration::from_millis(250)),
        };
        let transport = SshTransport {
            executor: &ssh,
            clock: &clock,
            home: Path::new("/home/a"),
            cwd: Path::new("/home/a/repo"),
            executable: "/usr/bin/ssh".into(),
            seconds: 30,
        };

        transport
            .exchange_with_cap(
                &peer,
                &request,
                budget,
                &CancelToken::new(),
                Duration::from_millis(250),
            )
            .unwrap();
        assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
    }

    // The deadline is shared out between the SSH timeout and the executor's
    // termination and reap allowances, so their sum never exceeds it.
    #[test]
    fn peer_exchange_within_a_deadline_includes_termination_and_reap() {
        struct Budgets(Mutex<Vec<(Duration, Duration, Duration)>>);
        impl Executor for Budgets {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                self.0.lock().unwrap().push((
                    options.timeout,
                    options.terminate_grace,
                    options.reap_timeout,
                ));
                Ok(ExecOutcome {
                    termination: Termination::Signal(9),
                    stdout: CapturedStream::default(),
                    stderr: CapturedStream::default(),
                    timed_out: true,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        // The whole-second budget has far longer left than the deadline.
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let local = HostId::from_bytes([1; 16]);
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            trace: Default::default(),
            agent: "chair".into(),
            project,
            action: PeerAction::Health {
                extra: Default::default(),
            },
        };
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        };
        let executor = Budgets(Mutex::new(Vec::new()));
        let transport = SshTransport {
            executor: &executor,
            clock: &clock,
            home: Path::new("/home/a"),
            cwd: Path::new("/home/a/repo"),
            executable: "/usr/bin/ssh".into(),
            seconds: 30,
        };
        let ms = Duration::from_millis;
        let us = Duration::from_micros;
        for within in [ms(10), ms(2000)] {
            let result =
                transport.exchange_within(&peer, &request, budget, &CancelToken::new(), within);
            assert!(matches!(result, Err(PeerError::Failed(_))), "{result:?}");
        }
        // A short deadline gives each allowance a quarter; a long one keeps the
        // usual grace. Either way the child's whole lifetime fits the deadline.
        // A cap alone keeps the executor's own allowances on top of the cap.
        transport
            .exchange_with_cap(&peer, &request, budget, &CancelToken::new(), ms(250))
            .unwrap_err();
        let recorded = executor.0.lock().unwrap().clone();
        assert_eq!(
            recorded,
            [
                (ms(5), us(2500), us(2500)),
                (ms(1500), TERMINATE_GRACE, REAP_TIMEOUT),
                (ms(250), ms(250), ms(250)),
            ]
        );
        for ((timeout, grace, reap), within) in recorded.iter().zip([ms(10), ms(2000)]) {
            assert!(*timeout + *grace + *reap <= within);
        }
        // A deadline too short to leave any timeout fails before SSH starts.
        assert!(matches!(
            transport.exchange_within(&peer, &request, budget, &CancelToken::new(), Duration::ZERO),
            Err(PeerError::Deadline)
        ));
        assert_eq!(executor.0.lock().unwrap().len(), 3);
    }
}
