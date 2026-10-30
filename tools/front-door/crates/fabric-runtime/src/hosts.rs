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

/// The newest protocol level this release speaks. Health is the host-level
/// probe, so it carries this level and `hosts` reports it for each host.
/// A release that adds a newer action raises this too.
pub const PROTOCOL: u64 = 10;
/// Levels at which each action family last changed; below `PROTOCOL` once a
/// newer action lands.
pub const CONSOLE_PROTOCOL: u64 = 7;
pub const INBOX_PROTOCOL: u64 = 8;
pub const DOCTOR_PROTOCOL: u64 = 9;
pub const CONSOLE_DETAIL_PROTOCOL: u64 = 10;
pub const MAX_FRAME: usize = 1_048_576;

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
    #[error("peer {peer} speaks protocol {remote}, this host speaks {PROTOCOL}; upgrade both")]
    ProtocolMismatch { peer: String, remote: u64 },
    #[error("peer project is outside the selected home")]
    Project,
    #[error("peer source host must be a configured peer other than this host")]
    SourceHost,
    #[error("peer is unavailable")]
    Unavailable,
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
    pub fn code(&self) -> &'static str {
        facade::FacadeError::from(self).code()
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
    Task {
        input: TaskInput,
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
    Landing {
        input: LandingInput,
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
            | Self::Task {
                input: TaskInput::List { .. },
                ..
            }
            | Self::Events {
                input: EventsInput::Read { .. },
                ..
            }
            | Self::Claim {
                input: ClaimInput::Verify { .. } | ClaimInput::List { .. },
                ..
            }
            | Self::Landing {
                input: LandingInput::Verify { .. } | LandingInput::Wait { .. },
                ..
            } => None,
            Self::Mail { input: v, .. } => Some(Mutation::Mail(v.clone())),
            Self::Task { input: v, .. } => Some(Mutation::Task(v.clone())),
            Self::Events { input: v, .. } => Some(Mutation::Events(v.clone())),
            Self::Claim { input: v, .. } => Some(Mutation::Claim(v.clone())),
            Self::Landing { input: v, .. } => Some(Mutation::Landing(v.clone())),
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
            Self::Task { input: v, .. } => {
                service
                    .task(v.clone(), budget, cancel)
                    .map(|data| CoordinationResult::Task {
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
            Self::Landing { input: v, .. } => {
                service
                    .landing(v.clone(), budget, cancel)
                    .map(|data| CoordinationResult::Landing {
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
    Task {
        result: TaskResult,
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
    Landing {
        result: LandingResult,
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
            Mutation::Task(_) => decode!(Task),
            Mutation::Events(_) => decode!(Events),
            Mutation::Claim(_) => decode!(Claim),
            Mutation::Landing(_) => decode!(Landing),
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
            MutationResult::Task(v) => Self::Task {
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
            MutationResult::Landing(v) => Self::Landing {
                result: v,
                extra: Default::default(),
            },
        }
    }
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
    RunStatuses {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        targets: Vec<crate::coordination::facade::AttemptTarget>,
    },
    Run {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        input: crate::coordination::facade::RunInput,
    },
    Submit {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        input: crate::coordination::facade::SubmitInput,
    },
    Output {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
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
        operation_id: OperationId,
    },
    LandingPush {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        proof: LandingProof,
        operation_id: OperationId,
        reconcile: bool,
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
        /// The source's wait, so the peer finishes inside it. Absent from an
        /// older source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_seconds: Option<u64>,
    },
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
            protocol: if matches!(action, PeerAction::ConsoleDetail { .. }) {
                CONSOLE_DETAIL_PROTOCOL
            } else if matches!(action, PeerAction::Inbox { .. }) {
                INBOX_PROTOCOL
            } else if matches!(
                action,
                PeerAction::ConsoleBoard { .. } | PeerAction::ConsoleTree { .. }
            ) {
                CONSOLE_PROTOCOL
            } else if matches!(action, PeerAction::Doctor { .. }) {
                DOCTOR_PROTOCOL
            } else {
                PROTOCOL
            },
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
            PeerAction::LandingPush { operation_id, .. } => Some(operation_id.clone()),
            PeerAction::SessionHandoff { input, .. } => Some(input.operation_id.clone()),
            PeerAction::Submit {
                input: facade::SubmitInput::Submit { operation_id, .. },
                ..
            }
            | PeerAction::Run {
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
    pub host: HostId,
    pub available: bool,
    /// Readiness requires the run owner; config presence alone proves nothing.
    pub execution: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHealth {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub host: HostId,
    pub available: bool,
    /// Readiness requires the run owner; config presence alone proves nothing.
    pub execution: Option<bool>,
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
        data: crate::coordination::facade::OutputResult,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    RunEvents {
        data: crate::runs::RunEventsPage,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Push {
        data: provenant_fabric_store::PushOperation,
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
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerResponse<R = PeerResult> {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub version: u64,
    #[serde(default = "baseline_protocol")]
    pub protocol: u64,
    pub host: HostId,
    pub project: CrossHostProjectKey,
    pub result: R,
}
fn baseline_protocol() -> u64 {
    1
}
/// Inspect the envelope before decoding an action that another protocol may
/// have changed. A refusal always names this host's protocol.
pub fn protocol_refusal(bytes: &[u8]) -> Result<Option<PeerResponse>, PeerError> {
    let request: PeerRequest<serde_json::Value> =
        provenant_records::decode(bytes, Limits::CONFIG).map_err(|_| PeerError::Frame)?;
    let console = matches!(
        request.action["action"].as_str(),
        Some("console_board" | "console_tree")
    );
    let expected = if request.action["action"] == "console_detail" {
        CONSOLE_DETAIL_PROTOCOL
    } else if request.action["action"] == "inbox" {
        INBOX_PROTOCOL
    } else if console {
        CONSOLE_PROTOCOL
    } else if request.action["action"] == "doctor" {
        DOCTOR_PROTOCOL
    } else {
        PROTOCOL
    };
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
/// Whether stderr contains an exact trimmed line, after removing controls.
fn stderr_has_line(bytes: &[u8], expected: &str) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .any(|line| plain_text(line, 200) == expected)
}
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
        let mut input = serde_json::to_vec(request).map_err(|_| PeerError::Frame)?;
        if input.len() >= MAX_FRAME {
            return Err(PeerError::RequestTooLarge);
        }
        input.push(b'\n');
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
            return Err(PeerError::Unavailable);
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
                if matches!(
                    request.action,
                    PeerAction::ConsoleBoard { .. }
                        | PeerAction::ConsoleTree { .. }
                        | PeerAction::ConsoleDetail { .. }
                        | PeerAction::Inbox { .. }
                        | PeerAction::Doctor { .. }
                ) && stderr_has_line(
                    &outcome.stderr.bytes,
                    "provenant: fabric peer: invalid peer frame",
                ) {
                    return Err(PeerError::ProtocolMismatch {
                        peer: peer.destination.clone(),
                        remote: 1,
                    });
                }
                if code == 3
                    && stderr_has_line(
                        &outcome.stderr.bytes,
                        "provenant: fabric peer: invalid peer frame",
                    )
                {
                    return Err(PeerError::ProtocolMismatch {
                        peer: peer.destination.clone(),
                        remote: 1,
                    });
                }
                if code == 3
                    && !matches!(
                        request.action,
                        PeerAction::ConsoleBoard { .. }
                            | PeerAction::ConsoleTree { .. }
                            | PeerAction::ConsoleDetail { .. }
                            | PeerAction::Inbox { .. }
                            | PeerAction::Doctor { .. }
                    )
                    && String::from_utf8_lossy(&outcome.stderr.bytes)
                        .lines()
                        .any(|line| {
                            line.trim().starts_with(
                                "provenant: fabric peer: peer agent closed without a reply; see ",
                            )
                        })
                {
                    // The v1 agent hides decode refusals, but this can also be
                    // a lost mutation reply. Probe health, never repeat the
                    // operation, and reclassify only a proven version mismatch.
                    // Omit protocol and trace so v1 can decode the probe.
                    let probe_value = serde_json::json!({
                        "version": 1,
                        "target_host": request.target_host,
                        "source_host": request.source_host,
                        "agent": request.agent,
                        "project": request.project,
                        "action": {"action": "health"},
                    });
                    let mut probe =
                        serde_json::to_vec(&probe_value).map_err(|_| PeerError::Frame)?;
                    probe.push(b'\n');
                    let remaining = timeout
                        .saturating_sub(Duration::from_micros(outcome.elapsed_micros))
                        .min(Duration::from_micros(
                            budget.remaining(self.clock.mono_now()).as_micros(),
                        ));
                    if !remaining.is_zero() && !cancel.is_cancelled() {
                        let mut options = ExecOptions::new(&argv, self.cwd, remaining);
                        options.env = Some(&env);
                        options.stdin = Some(&probe);
                        options.stdout = StreamPolicy::Capture { limit: MAX_FRAME };
                        options.stderr = StreamPolicy::Capture { limit: 4096 };
                        options.cancel = Some(cancel);
                        options.stop_on_output_limit = true;
                        if let Some((terminate_grace, reap_timeout)) = cleanup {
                            options.terminate_grace = terminate_grace;
                            options.reap_timeout = reap_timeout;
                        }
                        if let Ok(reply) = self.executor.execute(options)
                            && reply.termination == Termination::Exit(0)
                            && !reply.timed_out
                            && !reply.cancelled
                            && !reply.stdout.truncated
                            && !reply.stdout.read_failed
                            && !reply.drain_truncated
                            && let Ok(response) =
                                provenant_records::decode::<PeerResponse<serde_json::Value>, _>(
                                    reply.stdout.bytes.as_slice(),
                                    Limits::CONFIG,
                                )
                            && response.version == 1
                            && response.host == peer.host
                            && response.project == request.project.key
                            && response.protocol != PROTOCOL
                        {
                            return Err(PeerError::ProtocolMismatch {
                                peer: peer.destination.clone(),
                                remote: response.protocol,
                            });
                        }
                    }
                }
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
            project: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            result: serde_json::json!({"result": "error", "data": {"error": {"code": "busy"}}}),
        };
        assert!(matches!(
            decode_recorded_response(&serde_json::to_vec(&response).unwrap()),
            Err(PeerError::Version)
        ));
    }

    #[test]
    fn the_host_protocol_tracks_the_newest_action_level() {
        // `hosts` reports this number, so a new action that raises the level
        // must raise the host-level probe too.
        let newest = [
            PROTOCOL,
            CONSOLE_PROTOCOL,
            INBOX_PROTOCOL,
            DOCTOR_PROTOCOL,
            CONSOLE_DETAIL_PROTOCOL,
        ]
        .into_iter()
        .max()
        .unwrap();
        assert_eq!(PROTOCOL, newest);
    }

    #[test]
    fn protocol_fence_precedes_action_decoding() {
        let request = PeerRequest {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL - 1,
            target_host: HostId::from_bytes([3; 16]),
            source_host: HostId::from_bytes([4; 16]),
            agent: "fixture".into(),
            project: ProjectLocator {
                extra: Default::default(),
                key: CrossHostProjectKey::from_home(
                    Path::new("/home/a"),
                    Path::new("/home/a/repo/.git"),
                )
                .unwrap(),
                common_dir: "repo/.git".into(),
            },
            action: serde_json::json!({"action": "future_action", "build_heavy": true}),
            trace: Trace::default(),
        };
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
            pipelines: Default::default(),
            limits: crate::config::Limits {
                body_bytes: 65536,
                input_frame_bytes: 1048576,
                json_depth: 64,
                page_rows: 100,
                output_lines: 500,
                output_bytes: 65536,
                result_bytes: 1048576,
                lease_seconds: 3600,
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
                key: CrossHostProjectKey::from_home(home, &home.join("repo/.git")).unwrap(),
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

    #[test]
    fn new_requests_trace_the_sender_and_the_operation() {
        let (local, remote) = (HostId::from_bytes([1; 16]), HostId::from_bytes([2; 16]));
        let project = ProjectLocator {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(Path::new("/h"), Path::new("/h/repo/.git"))
                .unwrap(),
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
                        execution: None,
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
    #[test]
    fn fixed_peer_transport_preserves_mutation_uncertainty_without_retry() {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let local = HostId::from_bytes([1; 16]);
        let host = HostId::from_bytes([2; 16]);
        let operation = OperationId::parse("mail-once").unwrap();
        let project = ProjectLocator {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
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
            Some(operation)
        );
        assert!(request.project.resolve(Path::new("/home/b")).is_ok());
    }

    struct Canned(Mutex<Option<Result<ExecOutcome, ExecError>>>);
    impl Executor for Canned {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let wire: serde_json::Value = serde_json::from_slice(options.stdin.unwrap()).unwrap();
            if wire.get("protocol").is_none() {
                assert_eq!(wire["action"], serde_json::json!({"action":"health"}));
                return Err(ExecError::Busy);
            }
            let request = decode_request(options.stdin.unwrap()).unwrap();
            assert_eq!(
                request.protocol,
                if matches!(
                    request.action,
                    PeerAction::ConsoleBoard { .. } | PeerAction::ConsoleTree { .. }
                ) {
                    CONSOLE_PROTOCOL
                } else {
                    PROTOCOL
                }
            );
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
            protocol: if matches!(
                action,
                PeerAction::ConsoleBoard { .. } | PeerAction::ConsoleTree { .. }
            ) {
                CONSOLE_PROTOCOL
            } else {
                PROTOCOL
            },
            target_host: host,
            source_host: HostId::from_bytes([1; 16]),
            trace: Default::default(),
            agent: "chair".into(),
            project: ProjectLocator {
                extra: Default::default(),
                key: CrossHostProjectKey::from_home(
                    Path::new("/home/a"),
                    Path::new("/home/a/repo/.git"),
                )
                .unwrap(),
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
    fn console_peer_unsupported_and_unreachable() {
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
                PeerError::ProtocolMismatch { .. }
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
    fn console_protocol_refused_before_action_decode() {
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest::new(
            host,
            HostId::from_bytes([1; 16]),
            "chair".into(),
            project,
            PeerAction::ConsoleBoard {
                extra: Default::default(),
                all: false,
                limit: 50,
                selected: false,
            },
        );
        assert_eq!(request.protocol, CONSOLE_PROTOCOL);
        assert!(
            protocol_refusal(&serde_json::to_vec(&request).unwrap())
                .unwrap()
                .is_none()
        );
        let mut old = serde_json::to_value(&request).unwrap();
        old["protocol"] = PROTOCOL.into();
        let refusal = protocol_refusal(&serde_json::to_vec(&old).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(refusal.protocol, CONSOLE_PROTOCOL);
    }
    #[test]
    fn doctor_action_uses_its_protocol_and_is_refused_before_action_decode() {
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest::new(
            host,
            HostId::from_bytes([1; 16]),
            "chair".into(),
            project,
            PeerAction::Doctor {
                extra: Default::default(),
            },
        );
        assert_eq!(request.protocol, DOCTOR_PROTOCOL);
        assert!(
            protocol_refusal(&serde_json::to_vec(&request).unwrap())
                .unwrap()
                .is_none()
        );
        let mut old = serde_json::to_value(&request).unwrap();
        old["protocol"] = PROTOCOL.into();
        let refusal = protocol_refusal(&serde_json::to_vec(&old).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(refusal.protocol, DOCTOR_PROTOCOL);
        assert!(matches!(
            refusal.result,
            PeerResult::Error {
                data: crate::hosts::PeerResultError {
                    error: facade::FacadeError::PeerVersion,
                    ..
                },
                ..
            }
        ));
    }
    #[test]
    fn console_detail_uses_its_own_protocol() {
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest::new(
            host,
            HostId::from_bytes([1; 16]),
            "chair".into(),
            project,
            PeerAction::ConsoleDetail {
                extra: Default::default(),
                id: "attempt".into(),
            },
        );
        assert_eq!(request.protocol, CONSOLE_DETAIL_PROTOCOL);
        assert!(request.operation().is_none());
        assert!(
            protocol_refusal(&serde_json::to_vec(&request).unwrap())
                .unwrap()
                .is_none()
        );
        let mut old = serde_json::to_value(request).unwrap();
        old["protocol"] = CONSOLE_PROTOCOL.into();
        assert_eq!(
            protocol_refusal(&serde_json::to_vec(&old).unwrap())
                .unwrap()
                .unwrap()
                .protocol,
            CONSOLE_DETAIL_PROTOCOL
        );
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
    fn opaque_agent_failure_preserves_mutation_uncertainty() {
        for mutation in [false, true] {
            // The released v1 relay hides a decode refusal this way, but the
            // same diagnostic can also follow execution and a lost reply.
            let error = exchange_failure(
                Ok(outcome(
                    3,
                    "provenant: fabric peer: peer agent closed without a reply; see /home/a/peer-agent/agent.log\n",
                    false,
                )),
                mutation,
            );
            if mutation {
                assert!(matches!(error, PeerError::Unknown { .. }), "{error}");
            } else {
                assert!(matches!(error, PeerError::Failed(_)), "{error}");
            }
        }
    }

    #[test]
    fn agent_socket_closure_requires_a_confirmed_protocol_mismatch() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        // SSH's remote entrypoint forwards to the agent socket. The
        // protocol-1 wire fixture reproduces the old relay dropping a child's
        // decode refusal, then answering a legacy health request.
        struct AgentSsh(Mutex<std::collections::VecDeque<UnixStream>>);
        impl Executor for AgentSsh {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let request: serde_json::Value =
                    serde_json::from_slice(options.stdin.unwrap()).unwrap();
                let probe = request.get("protocol").is_none();
                assert_eq!(
                    options.timeout,
                    Duration::from_secs(if probe { 3 } else { 5 })
                );
                let mut stream = self.0.lock().unwrap().pop_front().unwrap();
                stream.write_all(options.stdin.unwrap()).unwrap();
                stream.shutdown(std::net::Shutdown::Write).unwrap();
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).unwrap();
                Ok(if bytes.is_empty() {
                    let mut reply = outcome(
                        3,
                        "provenant: fabric peer: peer agent closed without a reply; see /home/a/peer-agent/agent.log\n",
                        false,
                    );
                    reply.elapsed_micros = 2_000_000;
                    reply
                } else {
                    let mut reply = outcome(0, "", false);
                    reply.stdout.bytes = bytes;
                    reply
                })
            }
        }
        for (protocol, wrong_identity) in [
            (None, false),
            (Some(4), false),
            (Some(PROTOCOL), false),
            (None, true),
        ] {
            for mutation in [false, true] {
                let (client, server) = UnixStream::pair().unwrap();
                let (probe, health) = UnixStream::pair().unwrap();
                let server = std::thread::spawn(move || {
                    for (index, mut stream) in [server, health].into_iter().enumerate() {
                        let mut bytes = Vec::new();
                        stream.read_to_end(&mut bytes).unwrap();
                        if bytes.is_empty() {
                            return;
                        }
                        let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                        if index == 0 {
                            assert_eq!(request["protocol"], PROTOCOL);
                        } else {
                            assert!(request.get("protocol").is_none());
                            assert!(request.get("trace").is_none());
                            assert_eq!(request["action"], serde_json::json!({"action":"health"}));
                            let mut reply = serde_json::json!({
                                "version":1, "host":request["target_host"],
                                "project":request["project"]["key"],
                                "result":{"result":"health","data":{
                                    "host":request["target_host"],"available":true,"execution":true,
                                }},
                            });
                            if let Some(protocol) = protocol {
                                reply["protocol"] = protocol.into();
                            }
                            if wrong_identity {
                                reply["host"] =
                                    serde_json::to_value(HostId::from_bytes([9; 16])).unwrap();
                            }
                            stream
                                .write_all(&serde_json::to_vec(&reply).unwrap())
                                .unwrap();
                        }
                    }
                });
                let error = exchange_error(&AgentSsh(Mutex::new([client, probe].into())), mutation);
                if protocol != Some(PROTOCOL) && !wrong_identity {
                    assert!(
                        matches!(error, PeerError::ProtocolMismatch { remote, .. }
                    if remote == protocol.unwrap_or(1)),
                        "{error}"
                    );
                } else if mutation {
                    assert!(matches!(error, PeerError::Unknown { .. }), "{error}");
                } else {
                    assert!(matches!(error, PeerError::Failed(_)), "{error}");
                }
                server.join().unwrap();
            }
        }
    }

    #[test]
    fn released_v1_health_and_decode_refusal_report_protocol_one() {
        // v1 omits protocol from both the envelope and the health data.
        let host = HostId::from_bytes([2; 16]);
        let project =
            CrossHostProjectKey::from_home(Path::new("/home/a"), Path::new("/home/a/repo/.git"))
                .unwrap();
        let mut reply = outcome(0, "", false);
        reply.stdout.bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1, "host": host, "project": project,
            "result": {"result": "health", "data": {
                "host": host, "available": true, "execution": true,
            }},
        }))
        .unwrap();
        for mutation in [false, true] {
            for reply in [
                reply.clone(),
                outcome(
                    3,
                    "SSH login banner\nprovenant: fabric peer: invalid peer frame\n",
                    false,
                ),
            ] {
                let error = exchange_failure(Ok(reply), mutation);
                assert!(
                    matches!(error, PeerError::ProtocolMismatch { remote: 1, .. }),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn protocol_two_frame_errors_use_a_distinct_diagnostic() {
        assert_eq!(PeerError::Frame.to_string(), "malformed peer frame");
    }

    #[test]
    fn peer_health_wire_data_omits_local_diagnostics() {
        let health = PeerHealth {
            extra: Default::default(),
            host: HostId::from_bytes([2; 16]),
            available: true,
            execution: Some(true),
        };
        let value = serde_json::to_value(health).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert!(value.get("protocol").is_none());
        assert!(value.get("reason").is_none());
        let result: PeerResult = serde_json::from_value(serde_json::json!({
            "result": "health",
            "data": value,
        }))
        .unwrap();
        assert!(matches!(result, PeerResult::Health { .. }));
    }

    #[test]
    fn protocol_mismatch_names_both_versions_without_decoding_payload() {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        for action in [
            PeerAction::RunStatuses {
                extra: Default::default(),
                targets: vec![],
            },
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
                    protocol: PROTOCOL,
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
                        "peer workshop speaks protocol {remote}, this host speaks {PROTOCOL}; upgrade both"
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
        assert!(
            serde_json::to_value(unprobed)
                .unwrap()
                .get("protocol")
                .is_none()
        );
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
    fn plain_text_drops_format_and_separator_characters() {
        let text = "a\u{202e}b\u{2066}c\u{200b}d\u{2028}e\u{feff}f\u{1b}g";
        assert_eq!(plain_text(text, 100), "abcdefg");
        assert_eq!(plain_text("abcdef", 3), "abc");
    }

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
                base64: encode_base64(&vec![0xff; ATTACHMENT_LIMIT]),
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
                key: CrossHostProjectKey::from_home(
                    Path::new("/home/a"),
                    Path::new("/home/a/repo/.git"),
                )
                .unwrap(),
                common_dir: "repo/.git".into(),
            },
            action: PeerAction::SessionHandoff {
                extra: Default::default(),
                input: Box::new(input.clone()),
                budget_seconds: Some(30),
            },
        };
        let frame = serde_json::to_vec(&request).unwrap();
        assert!(frame.len() < MAX_FRAME, "{} bytes", frame.len());
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
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
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
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
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
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
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
            Err(PeerError::Unavailable)
        ));
        assert_eq!(executor.0.lock().unwrap().len(), 3);
    }
}
