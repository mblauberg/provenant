//! Fixed, bounded SSH peer protocol. No command strings or placement fallback.
pub mod mirror;
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

pub const PROTOCOL: u64 = 15;
pub const MAX_FRAME: usize = 1_048_576;
pub const PEER_UPGRADE_PENDING: &str = "PEER_UPGRADE_PENDING";

fn upgrade_pending() -> facade::FacadeError {
    facade::FacadeError::RunKind {
        run_code: PEER_UPGRADE_PENDING.into(),
        next: "wait 5 seconds for both peers to finish upgrading, then retry".into(),
        detail: None,
        resource: None,
    }
}

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
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum HostReadInput {
    Status {
        targets: Vec<String>,
        all: bool,
        limit: u64,
        cursor: Option<u64>,
        full: bool,
        mine: bool,
        adoptable: bool,
        custody_session: Option<String>,
        session_stream: bool,
        custody_stream: bool,
        chair: Option<provenant_records::custody::Chair>,
    },
    Output {
        target: String,
        part: String,
        offset: u64,
        tail: Option<u64>,
        tail_lines: Option<u64>,
        max_bytes: u64,
        path: bool,
        final_result: bool,
        to: Option<String>,
        full: bool,
    },
    Result {
        target: String,
        full: bool,
    },
    Events {
        from: Option<i64>,
        follow: bool,
        consumer: Option<String>,
        /// Internal page bound for aggregation; the public events verb has no
        /// row limit and drains these pages before returning.
        limit: u64,
    },
    EventsAck {
        consumer: String,
        through_cursor: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PeerAction {
    SmokeFixture {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        op: SmokeFixtureOp,
    },
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
        /// Optional for older senders; older receivers retain an unknown field
        /// in `extra` and return the whole tree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subtree: Option<String>,
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
    /// Read command data from this host for a multi-host envelope. Each
    /// variant carries the verb's actual options, never a shell command.
    HostRead {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        input: HostReadInput,
    },
    /// Collect this host's lane caches and worktrees. `gc --host` sends this
    /// to a peer; the peer applies the same flags it would apply locally.
    Gc {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        dry_run: bool,
        apply: bool,
        scheduled: bool,
        pressure: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        free_floor: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finished_branch: Option<String>,
        #[serde(default)]
        full: bool,
        #[serde(default)]
        detailed: bool,
        timeout_seconds: u64,
    },
}

/// Fixture operations are fenced independently of the base peer protocol.
/// `recipe` versions the deterministic fixture identity; peers echo the value
/// they actually built so recipe drift cannot masquerade as the same fixture.
/// `root` is the initiator's home-relative fixture root; the peer validates it
/// and uses it for both creation and removal so the two hosts build one fixture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SmokeFixtureOp {
    Create {
        nonce: String,
        recipe: u32,
        root: String,
    },
    Remove {
        nonce: String,
    },
}

peer_feature_protocols! {
    SMOKE_FIXTURE_PROTOCOL = 16 => [
        (SmokeFixture, "smoke_fixture", PeerAction::SmokeFixture {
            extra: Default::default(), op: SmokeFixtureOp::Create { nonce: "0".repeat(12), recipe: 1, root: ".local/state/provenant-smoke".into() },
        }),
    ],
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
            subtree: None,
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
    GC_PROTOCOL = 20 => [
        (Gc, "gc", PeerAction::Gc {
            extra: Default::default(), dry_run: true, apply: false,
            scheduled: false, pressure: true, free_floor: None, idle: None,
            finished_branch: None, full: false, detailed: false, timeout_seconds: 300,
        }),
    ],
    HOST_READ_PROTOCOL = 18 => [
        (HostRead, "host_read", PeerAction::HostRead {
            extra: Default::default(),
            input: HostReadInput::Events { from: Some(0), consumer: None, follow: false, limit: 1 },
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
            extra: [("release".into(), env!("CARGO_PKG_VERSION").into())].into(),
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
    SmokeFixture {
        data: SmokeFixtureReceipt,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
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
    /// One typed host read, retaining the command's own outcome and data.
    HostRead {
        data: Box<provenant_records::CommandResult<serde_json::Value>>,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    /// The bounded per-class aggregates of a peer `gc` sweep.
    Gc {
        data: Box<provenant_records::CommandResult<serde_json::Value>>,
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
/// Fixture creation returns its commits; removal may report a reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmokeFixtureReceipt {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Create: the fixture's base commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<provenant_core::GitOid>,
    /// Create: the fixture's head commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<provenant_core::GitOid>,
    /// Create: the fixture recipe the peer actually built, echoed for comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    let error = if request.protocol != expected {
        Some(facade::FacadeError::PeerVersion)
    } else if request
        .extra
        .get("release")
        .and_then(|value| value.as_str())
        != Some(env!("CARGO_PKG_VERSION"))
    {
        Some(upgrade_pending())
    } else {
        None
    };
    Ok(error.map(|error| PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: expected,
        host: request.target_host,
        project: request.project.key,
        result: PeerResult::Error {
            data: crate::hosts::PeerResultError {
                extra: Default::default(),
                error,
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

type StopViews<'a> = Vec<(
    &'a mut provenant_records::Stop,
    provenant_records::handoff::HandoffResult,
)>;

fn handoff_texts<'a>(
    handoff: &'a mut provenant_records::handoff::HandoffResult,
    texts: &mut Vec<&'a mut String>,
) {
    match handoff {
        Ok(Some(handoff)) => {
            texts.push(&mut handoff.note);
            texts.extend(handoff.remaining.iter_mut());
            texts.extend(handoff.decisions.iter_mut());
            texts.extend(handoff.verify.iter_mut());
            for concern in &mut handoff.spec_concerns {
                texts.extend([
                    &mut concern.criterion,
                    &mut concern.claim,
                    &mut concern.evidence,
                    &mut concern.proposed_change,
                ]);
            }
        }
        Err(error) => {
            for dropped in &mut error.dropped {
                texts.push(&mut dropped.error);
            }
            for (key, value) in &mut error.retained {
                report_texts(value, handoff_field_paths(key).unwrap_or(&[]), texts);
            }
        }
        Ok(None) => {}
    }
}

fn attempt_state_texts<'a, 's: 'a>(
    state: &'s mut provenant_records::run::AttemptState,
    texts: &mut Vec<&'a mut String>,
    stops: &mut StopViews<'s>,
) {
    if let provenant_records::run::AttemptState::Terminal {
        reason,
        terminal_note,
        stop,
        ..
    } = state
    {
        texts.extend(reason.iter_mut());
        texts.extend(terminal_note.iter_mut());
        if let Some(stop) = stop {
            let handoff = stop.handoff_result().clone();
            stops.push((stop, handoff));
        }
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

fn attempt_texts<'a, 's: 'a>(
    attempt: &'s mut provenant_records::run::AttemptRecord,
    texts: &mut Vec<&'a mut String>,
    stops: &mut StopViews<'s>,
) {
    texts.extend(attempt.title.iter_mut());
    texts.extend(attempt.publication_error.iter_mut());
    attempt_state_texts(&mut attempt.state, texts, stops);
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
    if let Some(delivery) = &mut attempt.delivery
        && let provenant_records::run::WriterDeliveryState::Failed { reason, .. } =
            &mut delivery.state
    {
        texts.extend(reason.iter_mut());
    }
    for skipped in &mut attempt.skipped_routes {
        texts.push(&mut skipped.detail);
    }
}

pub(crate) fn handoff_field_paths(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "note" => Some(&[""]),
        "dropped" => Some(&["*/error"]),
        "remaining" | "decisions" | "verify" => Some(&["*"]),
        "spec_concerns" => Some(&["*/criterion", "*/claim", "*/evidence", "*/proposed_change"]),
        _ => None,
    }
}

// Doctor and console are JSON views. Select their declared prose by path;
// a coincidentally named field in an extension is still opaque metadata.
pub(crate) fn report_texts<'a>(
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

/// Collect the declared prose in a flattened output lane row.
pub fn output_lane_texts<'a>(
    fields: &'a mut provenant_records::ExtraFields,
    texts: &mut Vec<&'a mut String>,
) {
    for (key, value) in fields {
        let paths: &[&str] = match key.as_str() {
            "title" | "waiting_for" | "provider_error" => &[""],
            "settlement" | "git_import_failure" => &["reason"],
            "budget_usage" => &["unit_detail", "notice"],
            "delivery" => &["state/reason"],
            "skipped_routes" => &["*/detail", "*/next"],
            "applied_route" | "observed_route" | "requested_route" => &[
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
            ],
            _ => &[],
        };
        if key == "handoff" {
            handoff_row_texts(value, texts);
        } else {
            report_texts(value, paths, texts);
        }
    }
}

fn handoff_row_texts<'a>(value: &'a mut serde_json::Value, texts: &mut Vec<&'a mut String>) {
    fn fields<'a>(value: &'a mut serde_json::Value, texts: &mut Vec<&'a mut String>) {
        if let serde_json::Value::Object(map) = value {
            for (key, value) in map {
                report_texts(value, handoff_field_paths(key).unwrap_or(&[]), texts);
            }
        }
    }
    if let serde_json::Value::Object(map) = value {
        for (key, value) in map {
            match key.as_str() {
                "Ok" => fields(value, texts),
                "Err" => {
                    if let serde_json::Value::Object(error) = value {
                        for (key, value) in error {
                            match key.as_str() {
                                "retained" => fields(value, texts),
                                "dropped" => report_texts(
                                    value,
                                    handoff_field_paths(key).unwrap_or(&[]),
                                    texts,
                                ),
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
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
    let mut stops = Vec::new();
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
            | RunResult::Resume { attempt, .. } => attempt_texts(attempt, &mut texts, &mut stops),
            RunResult::Wait {
                observation,
                attempts,
                ..
            } => {
                texts.extend(observation.iter_mut());
                for attempt in attempts {
                    attempt_texts(attempt, &mut texts, &mut stops);
                }
            }
        },
        PeerResult::RunStatuses { data, .. } => {
            for attempt in data {
                attempt_texts(attempt, &mut texts, &mut stops);
            }
        }
        PeerResult::RunEvents { data, .. } => {
            for event in &mut data.items {
                attempt_state_texts(&mut event.change, &mut texts, &mut stops);
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
                if let Some(observation) = observation
                    && let crate::coordination::EventObservation::PacedReread { reason, .. } =
                        observation.as_mut()
                {
                    texts.push(reason);
                }
            }
            _ => {}
        },
        PeerResult::SmokeFixture { data, .. } => texts.extend(data.reason.iter_mut()),
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
        PeerResult::HostRead { data: result, .. } => {
            let mut value = serde_json::to_value(result.as_ref()).map_err(|_| PeerError::Frame)?;
            provenant_confinement::publication::CrossHostTransport::new(projector)
                .json(&mut value)
                .map_err(|_| PeerError::Frame)?;
            **result = serde_json::from_value(value).map_err(|_| PeerError::Frame)?;
        }
        PeerResult::Gc { data: result, .. } => {
            let mut value = serde_json::to_value(result.as_ref()).map_err(|_| PeerError::Frame)?;
            provenant_confinement::publication::CrossHostTransport::new(projector)
                .json(&mut value)
                .map_err(|_| PeerError::Frame)?;
            **result = serde_json::from_value(value).map_err(|_| PeerError::Frame)?;
        }
        PeerResult::Error { data, .. } => match &mut data.error {
            facade::FacadeError::RecordedRefusal { diagnostic, .. } => {
                texts.extend(diagnostic.detail.iter_mut());
                texts.extend(diagnostic.next.iter_mut());
            }
            facade::FacadeError::DispatchRefused { reason, .. } => texts.push(reason),
            facade::FacadeError::InvalidInput {
                detail: Some(detail),
            }
            | facade::FacadeError::GraphRelayRefused {
                detail: Some(detail),
                ..
            } => texts.extend(detail.strings_mut()),
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
                "lanes/*/metadata/attempt/reason",
                "lanes/*/metadata/attempt/settlement/reason",
                "lanes/*/metadata/attempt/git_import_failure/reason",
                "lanes/*/metadata/attempt/waiting_for",
                "lanes/*/metadata/attempt/provider_error",
                "lanes/*/metadata/attempt/skipped_routes/*/detail",
                "lanes/*/metadata/operation/blocked_reason",
                "lanes/*/metadata/operation/refusal/detail",
                "lanes/*/metadata/operation/refusal/next",
                "nodes/*/metadata/attempt/title",
                "nodes/*/metadata/attempt/reason",
                "nodes/*/metadata/attempt/settlement/reason",
                "nodes/*/metadata/attempt/git_import_failure/reason",
                "nodes/*/metadata/attempt/waiting_for",
                "nodes/*/metadata/attempt/provider_error",
                "nodes/*/metadata/attempt/skipped_routes/*/detail",
                "nodes/*/metadata/operation/blocked_reason",
                "nodes/*/metadata/operation/refusal/detail",
                "nodes/*/metadata/operation/refusal/next",
                "unfetched_children/*/detail",
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
                paths.push(format!("{prefix}/handoff/Err/dropped/*/error"));
                for handoff in ["Ok", "Err/retained"] {
                    for field in [
                        "note",
                        "remaining/*",
                        "decisions/*",
                        "verify/*",
                        "spec_concerns/*/criterion",
                        "spec_concerns/*/claim",
                        "spec_concerns/*/evidence",
                        "spec_concerns/*/proposed_change",
                    ] {
                        paths.push(format!("{prefix}/handoff/{handoff}/{field}"));
                    }
                }
                for route in ["applied_route", "observed_route"] {
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
    for (_, handoff) in &mut stops {
        handoff_texts(handoff, &mut texts);
    }
    provenant_confinement::publication::CrossHostTransport::new(projector)
        .texts(&mut texts)
        .map_err(|_| PeerError::Frame)?;
    drop(texts);
    for (stop, handoff) in stops {
        *stop = stop.with_handoff(handoff).map_err(|_| PeerError::Frame)?;
    }
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
    outbound
        .extra
        .insert("release".into(), env!("CARGO_PKG_VERSION").into());
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
        // Reject invalid or oversized requests before even the health probe.
        encode_peer_request(request)?;
        if !matches!(request.action, PeerAction::Health { .. }) {
            let mut probe = request.clone();
            probe.action = PeerAction::Health {
                extra: Default::default(),
            };
            if matches!(
                request.action,
                PeerAction::SmokeFixture {
                    op: SmokeFixtureOp::Create { .. },
                    ..
                }
            ) {
                probe.extra.insert("release_probe".into(), true.into());
            }
            probe.protocol = action_fence(&probe.action);
            let started = self.clock.mono_now();
            let response = self.exchange_bounded(peer, &probe, budget, cancel, cap, within)?;
            if !matches!(response.result, PeerResult::Health { .. }) {
                return Err(PeerError::Frame);
            }
            let elapsed = Duration::from_micros(
                self.clock
                    .mono_now()
                    .as_micros()
                    .saturating_sub(started.as_micros()),
            );
            return self.exchange_after_probe(
                peer,
                request,
                budget,
                cancel,
                cap.saturating_sub(elapsed),
                within.map(|limit| limit.saturating_sub(elapsed)),
            );
        }
        self.exchange_after_probe(peer, request, budget, cancel, cap, within)
    }

    fn exchange_after_probe(
        &self,
        peer: &Peer,
        request: &PeerRequest,
        budget: WaitBudget,
        cancel: &CancelToken,
        cap: Duration,
        within: Option<Duration>,
    ) -> Result<PeerResponse, PeerError> {
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
        if let PeerResult::Health { data: health, .. } = &result
            && health.release != env!("CARGO_PKG_VERSION")
        {
            return Err(PeerError::Refused {
                error: upgrade_pending(),
            });
        }
        if let PeerResult::Error { data, .. } = &result
            && data.error.code() == PEER_UPGRADE_PENDING
        {
            return Err(PeerError::Refused {
                error: data.error.clone(),
            });
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
#[path = "hosts/tests.rs"]
mod tests;
