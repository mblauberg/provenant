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
    #[error("peer project is outside the selected home")]
    Project,
    #[error("peer is unavailable")]
    Unavailable,
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
            Self::Version => "peer_version",
            Self::Project => "invalid_input",
            Self::Unavailable => "home_unavailable",
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
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRequest {
    pub version: u64,
    pub target_host: HostId,
    pub source_host: HostId,
    pub agent: String,
    pub project: ProjectLocator,
    pub action: PeerAction,
}
impl PeerRequest {
    pub fn operation(&self) -> Option<OperationId> {
        match &self.action {
            PeerAction::Coordination { operation_id, .. } => operation_id.clone(),
            PeerAction::LandingPush { operation_id, .. } => Some(operation_id.clone()),
            PeerAction::SessionHandoff { input } => Some(input.operation_id.clone()),
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
        if self.source_host != local
            && !config
                .peers
                .values()
                .any(|peer| peer.host == self.source_host)
        {
            return Err(PeerError::Project);
        }
        self.project.resolve(home)?;
        self.actor()?;
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostHealth {
    pub host: HostId,
    pub available: bool,
    /// Readiness requires the run owner; config presence alone proves nothing.
    pub execution: Option<bool>,
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
pub struct PeerResponse {
    pub version: u64,
    pub host: HostId,
    pub project: CrossHostProjectKey,
    pub result: PeerResult,
}
pub fn decode_request(bytes: &[u8]) -> Result<PeerRequest, PeerError> {
    provenant_records::decode(bytes, Limits::CONFIG).map_err(|_| PeerError::Frame)
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
        let outcome = self.executor.execute(options).map_err(|_| {
            request
                .operation()
                .map_or(PeerError::Unavailable, |operation_id| PeerError::Unknown {
                    operation_id,
                })
        })?;
        let uncertain = || {
            request
                .operation()
                .map_or(PeerError::Unavailable, |operation_id| PeerError::Unknown {
                    operation_id,
                })
        };
        if outcome.termination != Termination::Exit(0)
            || outcome.timed_out
            || outcome.cancelled
            || outcome.stdout.truncated
            || outcome.stdout.read_failed
            || outcome.drain_truncated
        {
            return Err(uncertain());
        }
        let response: PeerResponse =
            provenant_records::decode(outcome.stdout.bytes.as_slice(), Limits::CONFIG)
                .map_err(|_| uncertain())?;
        if response.version != 1
            || response.host != peer.host
            || response.project != request.project.key
        {
            return Err(PeerError::Version);
        }
        Ok(response)
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
                host: request.target_host,
                project: request.project.key.clone(),
                result: PeerResult::Health(HostHealth {
                    host: request.target_host,
                    available: true,
                    execution: None,
                }),
            };
            *self.request.lock().unwrap() = Some(request);
            Ok(ExecOutcome {
                termination: Termination::Exit(if self.lose_reply { 255 } else { 0 }),
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
            },
        };
        let frame = serde_json::to_vec(&request).unwrap();
        assert!(frame.len() < MAX_FRAME, "{} bytes", frame.len());
        let decoded = decode_request(&frame).unwrap();
        assert!(
            matches!(&decoded.action, PeerAction::SessionHandoff { input: got } if **got == input)
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
