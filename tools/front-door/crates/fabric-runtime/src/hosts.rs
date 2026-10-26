//! Fixed, bounded SSH peer protocol. No command strings or placement fallback.
use crate::{
    config::{FabricConfig, Peer},
    coordination::*,
    identity::{CrossHostProjectKey, Identity},
};
use provenant_core::{ActorId, CancelToken, Clock, HostId, OperationId, WaitBudget};
use provenant_records::{CanonicalV1, Limits};
use provenant_supervisor::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Component, Path},
    time::Duration,
};

pub const PROTOCOL: u64 = 2;
pub const MAX_FRAME: usize = 1_048_576;

pub fn discover_ssh_executable(
    override_path: Option<&Path>,
) -> Result<std::path::PathBuf, PeerError> {
    if let Some(path) = override_path {
        if !crate::config::valid_executable_override(path)
            || !provenant_supervisor::sys::is_executable(path)
        {
            return Err(PeerError::Unavailable);
        }
        return Ok(path.to_path_buf());
    }
    ["/usr/bin/ssh", "/bin/ssh"]
        .into_iter()
        .map(Path::new)
        .find(|path| provenant_supervisor::sys::is_executable(path))
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
    #[error("invalid peer frame")]
    Frame,
    #[error("peer refused the operation: {code}")]
    Refused { code: String },
}
impl PeerError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Version | Self::ProtocolMismatch { .. } => "peer_version",
            Self::Project | Self::SourceHost => "invalid_input",
            Self::Unavailable | Self::Failed(_) => "home_unavailable",
            Self::Unknown { .. } => "launch_unknown",
            Self::Cancelled => "cancelled",
            Self::Frame => "state_corrupt",
            Self::Refused { code } => {
                crate::coordination::facade::FacadeError::from_code(code, None).code()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLocator {
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
#[serde(
    tag = "domain",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CoordinationInput {
    Mail(MailInput),
    Task(TaskInput),
    Events(EventsInput),
    Claim(ClaimInput),
    Landing(LandingInput),
}
impl CoordinationInput {
    pub fn mutation(&self) -> Option<Mutation> {
        match self {
            Self::Mail(MailInput::Read { .. })
            | Self::Task(TaskInput::List { .. })
            | Self::Events(EventsInput::Read { .. })
            | Self::Claim(ClaimInput::Verify { .. } | ClaimInput::List { .. })
            | Self::Landing(LandingInput::Verify { .. } | LandingInput::Wait { .. }) => None,
            Self::Mail(v) => Some(Mutation::Mail(v.clone())),
            Self::Task(v) => Some(Mutation::Task(v.clone())),
            Self::Events(v) => Some(Mutation::Events(v.clone())),
            Self::Claim(v) => Some(Mutation::Claim(v.clone())),
            Self::Landing(v) => Some(Mutation::Landing(v.clone())),
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
            Self::Mail(v) => service
                .mail(v.clone(), budget, cancel)
                .map(CoordinationResult::Mail),
            Self::Task(v) => service
                .task(v.clone(), budget, cancel)
                .map(CoordinationResult::Task),
            Self::Events(v) => service
                .events(v.clone(), budget, cancel)
                .map(CoordinationResult::Events),
            Self::Claim(v) => service
                .claim(v.clone(), budget, cancel)
                .map(CoordinationResult::Claim),
            Self::Landing(v) => service
                .landing(v.clone(), budget, cancel)
                .map(CoordinationResult::Landing),
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "domain",
    content = "result",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CoordinationResult {
    Mail(MailResult),
    Task(TaskResult),
    Events(EventsResult),
    Claim(ClaimResult),
    Landing(LandingResult),
}
impl From<MutationResult> for CoordinationResult {
    fn from(value: MutationResult) -> Self {
        match value {
            MutationResult::Mail(v) => Self::Mail(v),
            MutationResult::Task(v) => Self::Task(v),
            MutationResult::Events(v) => Self::Events(v),
            MutationResult::Claim(v) => Self::Claim(v),
            MutationResult::Landing(v) => Self::Landing(v),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PeerAction {
    Health,
    RunStatuses {
        targets: Vec<crate::coordination::facade::AttemptTarget>,
    },
    Run {
        input: crate::coordination::facade::RunInput,
    },
    Submit {
        input: crate::coordination::facade::SubmitInput,
    },
    Output {
        input: crate::coordination::facade::OutputInput,
    },
    RunEvents {
        after: u64,
        limit: u16,
    },
    Receipt {
        operation_id: OperationId,
    },
    LandingPush {
        proof: LandingProof,
        operation_id: OperationId,
        reconcile: bool,
    },
    Coordination {
        input: CoordinationInput,
        operation_id: Option<OperationId>,
    },
    /// The peer's own lanes, active only unless `all`; newest first with `all`.
    Lanes {
        all: bool,
        limit: u16,
    },
    /// Start a fresh session on the peer from a hand-off prompt.
    SessionHandoff {
        input: Box<crate::handoff::SessionHandoff>,
        /// The source's wait, so the peer finishes inside it. Absent from an
        /// older source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_seconds: Option<u64>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRequest<A = PeerAction> {
    pub version: u64,
    pub protocol: u64,
    pub target_host: HostId,
    pub source_host: HostId,
    pub agent: String,
    pub project: ProjectLocator,
    pub action: A,
}
impl PeerRequest {
    pub fn operation(&self) -> Option<OperationId> {
        match &self.action {
            PeerAction::Coordination { operation_id, .. } => operation_id.clone(),
            PeerAction::LandingPush { operation_id, .. } => Some(operation_id.clone()),
            PeerAction::SessionHandoff { input, .. } => Some(input.operation_id.clone()),
            PeerAction::Submit {
                input: facade::SubmitInput::Submission { operation_id, .. },
            }
            | PeerAction::Run {
                input:
                    facade::RunInput::Cancel { operation_id, .. }
                    | facade::RunInput::Resume { operation_id, .. },
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
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "result",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PeerResult {
    Run(Box<crate::runs::RunResult>),
    RunStatuses(Vec<provenant_records::run::AttemptRecord>),
    Submitted(crate::runs::SubmissionReceipt),
    Output(crate::coordination::facade::OutputResult),
    RunEvents(crate::runs::RunEventsPage),
    Push(provenant_fabric_store::PushOperation),
    Health(HostHealth),
    Coordination(CoordinationResult),
    SessionHandoff(crate::handoff::HandoffReceipt),
    Error {
        code: String,
        operation_id: Option<OperationId>,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerResponse<R = PeerResult> {
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
    Ok((request.protocol != PROTOCOL).then(|| PeerResponse {
        version: 1,
        protocol: PROTOCOL,
        host: request.target_host,
        project: request.project.key,
        result: PeerResult::Error {
            code: "peer_version".into(),
            operation_id: None,
        },
    }))
}
pub fn decode_request(bytes: &[u8]) -> Result<PeerRequest, PeerError> {
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
pub trait PeerTransport {
    fn exchange(
        &self,
        peer: &Peer,
        request: &PeerRequest,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResponse, PeerError>;
}
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
            return Err(PeerError::Frame);
        }
        input.push(b'\n');
        let timeout = Duration::from_micros(budget.remaining(self.clock.mono_now()).as_micros())
            .min(Duration::from_secs(self.seconds))
            .min(cap);
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
                if code == 3 && line == "provenant: fabric peer: invalid peer frame" {
                    return Err(PeerError::ProtocolMismatch {
                        peer: peer.destination.clone(),
                        remote: 1,
                    });
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
        if response.protocol != PROTOCOL {
            return Err(PeerError::ProtocolMismatch {
                peer: peer.destination.clone(),
                remote: response.protocol,
            });
        }
        let result = serde_json::from_value(response.result)
            .map_err(|_| uncertain("peer reply is not a valid frame".to_owned()))?;
        if let PeerResult::Health(health) = &result
            && health.host != peer.host
        {
            return Err(PeerError::Version);
        }
        Ok(PeerResponse {
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
    use provenant_supervisor::exec::{CapturedStream, ExecError, ExecOutcome};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn validate_refuses_the_target_as_its_own_source() {
        let temp = crate::test_support::tempdir().unwrap();
        let home = temp.path();
        let local = HostId::from_bytes([1; 16]);
        let remote = HostId::from_bytes([2; 16]);
        let mut config = FabricConfig {
            agent_label: "fixture".into(),
            client_label: "fixture".into(),
            ssh_executable: None,
            home_hosts: Default::default(),
            peers: Default::default(),
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
                graph_timeout_ms: 14400000,
                bundle_bytes: 67108864,
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
            version: 1,
            protocol: PROTOCOL,
            target_host: local,
            source_host: local,
            agent: "fixture".into(),
            project: ProjectLocator {
                key: CrossHostProjectKey::from_home(home, &home.join("repo/.git")).unwrap(),
                common_dir: "repo/.git".into(),
            },
            action: PeerAction::Health,
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
        request.source_host = HostId::from_bytes([3; 16]);
        assert!(matches!(
            request.validate(local, &config, home),
            Err(PeerError::SourceHost)
        ));
    }

    #[test]
    fn peer_responses_read_only_null_retired_members() {
        use provenant_core::{MessageId, ProjectId, TaskId};
        use provenant_fabric_store::{Delivery, Message, MessageKind, Page, Task, TaskState};
        use provenant_records::RecordHeader;

        fn old_frame(response: &PeerResponse, record: &str, member: &str, value: &str) -> Vec<u8> {
            let old = format!(
                "{},\"{member}\":{value}}}",
                record.strip_suffix('}').unwrap()
            );
            let frame = serde_json::to_string(response).unwrap();
            assert!(frame.contains(record));
            frame.replace(record, &old).into_bytes()
        }

        let now = WallTime::from_micros(1).unwrap();
        let task = Task {
            header: RecordHeader::V1,
            project: ProjectId::from_bytes([1; 16]),
            id: TaskId::from_bytes([2; 16]),
            objective: "Review".into(),
            note: None,
            state: TaskState::Open,
            owner: None,
            generation: 0,
            expires_at: None,
            created_at: now,
            updated_at: now,
            legacy: (),
        };
        let mut response = PeerResponse {
            version: 1,
            protocol: PROTOCOL,
            host: HostId::from_bytes([3; 16]),
            project: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            result: PeerResult::Coordination(CoordinationResult::Task(TaskResult::Created {
                task: task.clone(),
            })),
        };
        let task_json = serde_json::to_string(&task).unwrap();
        let frame = old_frame(&response, &task_json, "legacy", "null");
        let decoded =
            provenant_records::decode::<PeerResponse, _>(frame.as_slice(), Limits::CONFIG).unwrap();
        assert!(matches!(
            decoded.result,
            PeerResult::Coordination(CoordinationResult::Task(TaskResult::Created { task: got }))
                if got == task
        ));
        let frame = old_frame(&response, &task_json, "legacy", "{}");
        assert!(
            provenant_records::decode::<PeerResponse, _>(frame.as_slice(), Limits::CONFIG).is_err()
        );

        let message = Message {
            header: RecordHeader::V1,
            project: task.project,
            id: MessageId::from_bytes([4; 16]),
            sender: ActorId::from_bytes([5; 16]),
            recipients: vec![ActorId::from_bytes([6; 16])],
            conversation: None,
            kind: MessageKind::Note,
            body: "Review".into(),
            artifacts: vec![],
            reply_to: None,
            task: None,
            created_at: now,
            legacy_output_path: (),
            legacy: (),
        };
        let mail = MailResult::Read {
            page: Page {
                items: vec![Delivery {
                    seq: 1,
                    message: message.clone(),
                }],
                truncated: false,
            },
            cursor: 1,
        };
        response.result = PeerResult::Coordination(CoordinationResult::Mail(mail.clone()));
        let message_json = serde_json::to_string(&message).unwrap();
        for member in ["legacy", "legacy_output_path"] {
            let frame = old_frame(&response, &message_json, member, "null");
            let decoded =
                provenant_records::decode::<PeerResponse, _>(frame.as_slice(), Limits::CONFIG)
                    .unwrap();
            assert!(matches!(
                decoded.result,
                PeerResult::Coordination(CoordinationResult::Mail(got)) if got == mail
            ));
            let frame = old_frame(&response, &message_json, member, "{}");
            assert!(
                provenant_records::decode::<PeerResponse, _>(frame.as_slice(), Limits::CONFIG)
                    .is_err()
            );
        }
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
                version: 1,
                protocol: PROTOCOL,
                host: request.target_host,
                project: request.project.key.clone(),
                result: PeerResult::Health(HostHealth {
                    protocol: Some(PROTOCOL),
                    reason: None,
                    host: request.target_host,
                    available: true,
                    execution: None,
                }),
            };
            let lose_reply = self.lose_reply && !matches!(request.action, PeerAction::Health);
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
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest {
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            agent: "chair".into(),
            project,
            action: PeerAction::Coordination {
                input: CoordinationInput::Events(EventsInput::Note {
                    note: "hello".into(),
                    dedup_key: None,
                }),
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
            let request = decode_request(options.stdin.unwrap()).unwrap();
            assert_eq!(request.protocol, PROTOCOL);
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
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let host = HostId::from_bytes([2; 16]);
        let request = PeerRequest {
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: HostId::from_bytes([1; 16]),
            agent: "chair".into(),
            project: ProjectLocator {
                key: CrossHostProjectKey::from_home(
                    Path::new("/home/a"),
                    Path::new("/home/a/repo/.git"),
                )
                .unwrap(),
                common_dir: "repo/.git".into(),
            },
            action: if mutation {
                PeerAction::Coordination {
                    input: CoordinationInput::Events(EventsInput::Note {
                        note: "must not run".into(),
                        dedup_key: None,
                    }),
                    operation_id: Some(OperationId::parse("wrong-version").unwrap()),
                }
            } else {
                PeerAction::RunStatuses { targets: vec![] }
            },
        };
        let peer = Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        };
        let executor = Canned(Mutex::new(Some(result)));
        let transport = SshTransport {
            executor: &executor,
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
                outcome(3, "provenant: fabric peer: invalid peer frame\n", false),
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
    fn protocol_mismatch_names_both_versions_without_decoding_payload() {
        let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
        let host = HostId::from_bytes([2; 16]);
        let project = ProjectLocator {
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        for action in [
            PeerAction::RunStatuses { targets: vec![] },
            PeerAction::Coordination {
                input: CoordinationInput::Events(EventsInput::Note {
                    note: "hello".into(),
                    dedup_key: None,
                }),
                operation_id: Some(OperationId::parse("note").unwrap()),
            },
        ] {
            for remote in [1, PROTOCOL + 1] {
                let response = PeerResponse {
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
                    version: 1,
                    protocol: PROTOCOL,
                    target_host: host,
                    source_host: HostId::from_bytes([1; 16]),
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
            operation_id: operation.clone(),
            account: "work".into(),
            model: None,
            prompt: format!(
                "{PROMPT_HEADER}{}",
                "\"".repeat(PROMPT_LIMIT - PROMPT_HEADER.len())
            ),
            branches: (0..64).map(|i| format!("feat/branch-{i}")).collect(),
            attachments: vec![Attachment {
                name: "bundle.bin".into(),
                base64: encode_base64(&vec![0xff; ATTACHMENT_LIMIT]),
            }],
            launch: false,
        };
        input.validate().unwrap();
        let request = PeerRequest {
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            agent: "chair".into(),
            project: ProjectLocator {
                key: CrossHostProjectKey::from_home(
                    Path::new("/home/a"),
                    Path::new("/home/a/repo/.git"),
                )
                .unwrap(),
                common_dir: "repo/.git".into(),
            },
            action: PeerAction::SessionHandoff {
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
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest {
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            agent: "chair".into(),
            project,
            action: PeerAction::Health,
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
            key: CrossHostProjectKey::from_home(
                Path::new("/home/a"),
                Path::new("/home/a/repo/.git"),
            )
            .unwrap(),
            common_dir: "repo/.git".into(),
        };
        let request = PeerRequest {
            version: 1,
            protocol: PROTOCOL,
            target_host: host,
            source_host: local,
            agent: "chair".into(),
            project,
            action: PeerAction::Health,
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
}
