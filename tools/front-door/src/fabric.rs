//! Fabric process composition: MCP, fixed peer ingress and host observations.
mod context;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_mcp::{
    stdio,
    tools::{Service, ServiceError},
};
use provenant_fabric_runtime::runs::{
    RunCapabilities, RunEventsPage, RunOwner, RunResult, SubmissionReceipt,
};
use provenant_fabric_runtime::{
    coordination::{facade::*, *},
    hosts::*,
    identity::Identity,
};
use provenant_supervisor::{clock::SystemClock, exec::SystemExecutor};
use std::{
    io::{self, Read},
    path::Path,
    sync::mpsc,
    time::Instant,
};

#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error(transparent)]
    Exec(#[from] provenant_supervisor::exec::ExecError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] provenant_supervisor::fs::FsError),
    #[error(transparent)]
    Identity(#[from] provenant_fabric_runtime::identity::IdentityError),
    #[error(transparent)]
    Utility(#[from] provenant_confinement::utility::UtilityError),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
    #[error(transparent)]
    Peer(#[from] PeerError),
    #[error(transparent)]
    Time(#[from] provenant_core::TimeError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid Fabric setup")]
    Invalid,
}

struct Factory {
    context: context::Context,
    clock: SystemClock,
    backoff: ThreadWait,
    executor: SystemExecutor,
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
}
impl Factory {
    fn coordination(&self) -> Result<CoordinationService<'_>, CoordinationError> {
        let ctx = &self.context;
        let cancel = CancelToken::new();
        let budget = WaitBudget::mcp(self.clock.mono_now(), 55)?;
        CoordinationService::for_identity(
            &ctx.roots.state,
            &ctx.identity,
            &ctx.config,
            &self.home,
            &self.cwd,
            &self.executor,
            &self.clock,
            &self.backoff,
            budget,
            &cancel,
        )
    }
}
fn system_ssh() -> Option<&'static Path> {
    ["/usr/bin/ssh", "/bin/ssh"]
        .into_iter()
        .map(Path::new)
        .find(|p| provenant_supervisor::sys::is_executable(p))
}
impl stdio::Factory for Factory {
    fn maintain_usage(&self) {
        crate::usage::maintain_tools(&self.context.roots);
    }
    fn record_usage(&self, tool: &str, outcome: &str, duration_ms: u64) {
        crate::usage::record_tool(&self.context.roots, tool, outcome, duration_ms);
    }
    type Facade<'a> = BoundFacade<'a>;
    fn input_limits(&self) -> provenant_records::Limits {
        provenant_records::Limits {
            max_bytes: self.context.config.limits.input_frame_bytes,
            max_depth: usize::try_from(self.context.config.limits.json_depth).unwrap_or(64),
            max_members: 100_000,
        }
    }
    fn open(&self) -> Result<Service<'_, Self::Facade<'_>>, ServiceError> {
        Ok(Service::new(
            self.coordination()?,
            BoundFacade { factory: self },
            &self.clock,
        )
        .with_limits(self.context.config.limits.clone()))
    }
}
struct BoundFacade<'a> {
    factory: &'a Factory,
}
impl F3Facade for BoundFacade<'_> {
    fn refresh_events(
        &mut self,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<EventCoverage, FacadeError> {
        self.factory.pull_events(budget, cancel)
    }
    type Identity = Identity;
    type Submit = SubmissionReceipt;
    type Run = RunResult;
    type Hosts = Vec<HostHealth>;
    fn identity(
        &mut self,
        _: IdentityInput,
        _: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Identity, FacadeError> {
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled);
        }
        Ok(self.factory.context.identity.clone())
    }
    // F3 binds run submit/query/controls once the run owner lands on main.
    fn submit(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<SubmissionReceipt, FacadeError> {
        BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
        }
        .submit(input, budget, cancel)
    }
    fn run(
        &mut self,
        input: RunInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<RunResult, FacadeError> {
        BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
        }
        .run(input, budget, cancel)
    }
    fn graph(
        &mut self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        self.factory.graph(input, budget, cancel)
    }
    fn output(
        &mut self,
        input: OutputInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<OutputResult, FacadeError> {
        BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
        }
        .output(input, budget, cancel)
    }
    fn hosts(
        &mut self,
        input: HostsInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<HostHealth>, FacadeError> {
        let HostsInput::List { after, limit } = input;
        let ctx = &self.factory.context;
        let mut hosts = std::collections::BTreeMap::new();
        hosts.insert(
            ctx.identity.host,
            HostHealth {
                host: ctx.identity.host,
                available: true,
                execution: Some(
                    BinaryRunOwner {
                        factory: self.factory,
                        allow_remote: true,
                    }
                    .capabilities()
                    .submit,
                ),
            },
        );
        for peer in ctx.config.peers.values() {
            if hosts.contains_key(&peer.host) {
                continue;
            }
            let mut health = HostHealth {
                host: peer.host,
                available: false,
                execution: None,
            };
            if let (Ok(project), Some(executable)) = (
                ProjectLocator::new(&ctx.identity, &self.factory.home),
                system_ssh(),
            ) {
                let request = PeerRequest {
                    version: 1,
                    target_host: peer.host,
                    source_host: ctx.identity.host,
                    agent: ctx.identity.inbox.agent.clone(),
                    project,
                    action: PeerAction::Health,
                };
                let transport = SshTransport {
                    executor: &self.factory.executor,
                    clock: &self.factory.clock,
                    home: &self.factory.home,
                    cwd: &self.factory.cwd,
                    executable,
                    seconds: ctx.config.limits.peer_seconds,
                };
                if let Ok(PeerResponse {
                    result: PeerResult::Health(value),
                    ..
                }) = transport.exchange(peer, &request, budget, cancel)
                {
                    health = value;
                }
            }
            hosts.insert(peer.host, health);
        }
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled);
        }
        Ok(hosts
            .into_values()
            .filter(|h| after.is_none_or(|a| h.host > a))
            .take(limit as usize)
            .collect())
    }
}

/// The OS reader owns stdin for this process. Its one-slot queue bounds memory;
/// callers observe deadlines without changing inherited descriptor flags.
struct Stdin {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    eof: bool,
}
impl Stdin {
    fn new() -> Self {
        let (send, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                let mut bytes = vec![0; 8192];
                match input.read(&mut bytes) {
                    Ok(n) => {
                        bytes.truncate(n);
                        if send.send(Ok(bytes)).is_err() || n == 0 {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        let _ = send.send(Err(e));
                        return;
                    }
                }
            }
        });
        Self {
            receiver,
            pending: Vec::new(),
            eof: false,
        }
    }
}
impl stdio::Input for Stdin {
    fn read(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }
        if self.pending.is_empty() {
            self.pending = match self
                .receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(value) => value?,
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
                Err(_) => return Err(io::ErrorKind::BrokenPipe.into()),
            };
            if self.pending.is_empty() {
                self.eof = true;
                return Ok(0);
            }
        }
        let size = bytes.len().min(self.pending.len());
        bytes[..size].copy_from_slice(&self.pending[..size]);
        self.pending.drain(..size);
        Ok(size)
    }
}
struct Stdout;
impl stdio::Output for Stdout {
    fn write_frame(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()> {
        provenant_supervisor::sys::hook_io::write_stdout(bytes, deadline)
    }
}

pub fn mcp(client: Option<provenant_records::Harness>, globals: &provenant_cli::Globals) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let assets = crate::roots::product_root()?;
        let cwd = crate::roots::selected_project(globals.project.as_deref(), false)?;
        let roots = crate::roots::roots(globals, &assets, &cwd)?;
        let home = crate::roots::home()?;
        let mut context = context::Context::open(roots, &home, &cwd)?;
        if let Some(client) = client {
            context.config.client_label = serde_json::to_value(client)?
                .as_str()
                .ok_or(EdgeError::Invalid)?
                .to_owned();
            context.identity.client = context.config.client_label.clone();
            if context.config.agent_label.is_empty()
                && std::env::var_os("PROVENANT_AGENT_LABEL").is_none_or(|v| v.is_empty())
            {
                context.identity.inbox = provenant_fabric_runtime::identity::Address::new(
                    context.identity.host,
                    &context.config.client_label,
                )?;
            }
        }
        let factory = Factory {
            context,
            clock: SystemClock::new(),
            backoff: ThreadWait,
            executor: SystemExecutor::new(4)?,
            home,
            cwd,
        };
        stdio::serve(&mut Stdin::new(), &mut Stdout, factory)?;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = std::io::Write::write_all(
                &mut io::stderr().lock(),
                format!("provenant: fabric mcp: {error}\n").as_bytes(),
            );
            3
        }
    }
}

fn factory(globals: &provenant_cli::Globals) -> Result<Factory, Box<dyn std::error::Error>> {
    let assets = crate::roots::product_root()?;
    let cwd = crate::roots::selected_project(globals.project.as_deref(), false)?;
    let roots = crate::roots::roots(globals, &assets, &cwd)?;
    let home = crate::roots::home()?;
    let context = context::Context::open(roots, &home, &cwd)?;
    Ok(Factory {
        context,
        clock: SystemClock::new(),
        backoff: ThreadWait,
        executor: SystemExecutor::new(4)?,
        home,
        cwd,
    })
}
pub fn hosts(globals: &provenant_cli::Globals) -> u8 {
    let result = (|| -> Result<Vec<HostHealth>, Box<dyn std::error::Error>> {
        let factory = factory(globals)?;
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        Ok(BoundFacade { factory: &factory }.hosts(
            HostsInput::List {
                after: None,
                limit: 100,
            },
            budget,
            &CancelToken::new(),
        )?)
    })();
    match result {
        Ok(data) => {
            let human = serde_json::to_string_pretty(&data).unwrap_or_default();
            crate::emit(
                &provenant_records::CommandResult::new(
                    "fabric host list",
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                true,
            )
        }
        Err(error) => crate::fail(
            "fabric host list",
            provenant_records::Outcome::Error,
            "io",
            &error.to_string(),
            globals,
        ),
    }
}
pub fn peer(globals: &provenant_cli::Globals) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let bytes = provenant_supervisor::sys::hook_io::read_stdin(
            MAX_FRAME,
            Instant::now() + std::time::Duration::from_secs(30),
        )?;
        let request = decode_request(&bytes)?;
        let home = crate::roots::home()?;
        let common = request.project.resolve(&home)?;
        if provenant_supervisor::fs::canonicalize(&common)? != common {
            return Err(PeerError::Project.into());
        }
        let cwd = if common.file_name() == Some(std::ffi::OsStr::new(".git")) {
            common.parent().ok_or(PeerError::Project)?.to_path_buf()
        } else {
            common
        };
        let assets = crate::roots::product_root()?;
        let roots = crate::roots::roots(globals, &assets, &cwd)?;
        let mut context = context::Context::open(roots, &home, &cwd)?;
        request.validate(context.identity.host, &context.config, &home)?;
        if context.identity.federation.as_ref().map(|p| &p.key) != Some(&request.project.key) {
            return Err(PeerError::Project.into());
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let backoff = ThreadWait;
        context.identity.inbox =
            provenant_fabric_runtime::identity::Address::new(request.source_host, &request.agent)?;
        let peer_factory = Factory {
            context,
            clock: SystemClock::new(),
            backoff: ThreadWait,
            executor: SystemExecutor::new(4)?,
            home: home.clone(),
            cwd: cwd.clone(),
        };
        let context = &peer_factory.context;
        let mut owner = BinaryRunOwner {
            factory: &peer_factory,
            allow_remote: false,
        };
        let result = match &request.action {
            PeerAction::Run { input } => peer_result(
                owner.run(input.clone(), budget, &cancel),
                request.operation(),
                |value| PeerResult::Run(Box::new(value)),
            ),
            PeerAction::Submit { input } => peer_result(
                owner.submit(input.clone(), budget, &cancel),
                request.operation(),
                PeerResult::Submitted,
            ),
            PeerAction::Output { input } => peer_result(
                owner.output(input.clone(), budget, &cancel),
                None,
                PeerResult::Output,
            ),
            PeerAction::RunEvents { after, limit } => peer_result(
                owner.events(*after, *limit, budget, &cancel),
                None,
                PeerResult::RunEvents,
            ),
            PeerAction::Health => PeerResult::Health(HostHealth {
                host: context.identity.host,
                available: true,
                execution: Some(owner.capabilities().submit),
            }),
            PeerAction::LandingPush {
                proof,
                operation_id,
                reconcile,
            } => {
                if context
                    .config
                    .home_hosts
                    .get(request.project.key.as_str())
                    .is_some_and(|h| *h != context.identity.host)
                {
                    PeerResult::Error {
                        code: "home_unavailable".into(),
                        operation_id: Some(operation_id.clone()),
                    }
                } else {
                    let executor = SystemExecutor::new(1)?;
                    match landing_local(
                        context,
                        &executor,
                        proof.clone(),
                        operation_id,
                        *reconcile,
                        request.actor()?,
                        budget,
                        &clock,
                        &cancel,
                    ) {
                        Ok(value) => PeerResult::Push(value),
                        Err(_) => PeerResult::Error {
                            code: "launch_unknown".into(),
                            operation_id: Some(operation_id.clone()),
                        },
                    }
                }
            }
            PeerAction::Receipt { operation_id } => {
                let store_request = provenant_fabric_store::Request {
                    clock: &clock,
                    budget,
                    cancel: &cancel,
                    backoff: &backoff,
                };
                let store =
                    provenant_fabric_store::Store::open(&context.roots.state, &store_request)?;
                match store.coordination_receipt::<MutationResult>(
                    context.identity.project,
                    request.actor()?,
                    operation_id,
                    &store_request,
                )? {
                    Some(value) => PeerResult::Coordination(value.into()),
                    None => PeerResult::Error {
                        code: "launch_unknown".into(),
                        operation_id: Some(operation_id.clone()),
                    },
                }
            }
            PeerAction::Coordination {
                input,
                operation_id,
            } => {
                // A peer never recursively forwards or elects another home.
                if context
                    .config
                    .home_hosts
                    .get(request.project.key.as_str())
                    .is_some_and(|h| *h != context.identity.host)
                {
                    PeerResult::Error {
                        code: "home_unavailable".into(),
                        operation_id: operation_id.clone(),
                    }
                } else {
                    let coverage =
                        if matches!(input, CoordinationInput::Events(EventsInput::Read { .. })) {
                            peer_factory.pull_events_from(false, budget, &cancel)?
                        } else {
                            EventCoverage::default()
                        };
                    let service = CoordinationService::open(
                        &context.roots.state,
                        context.identity.project,
                        request.actor()?,
                        &clock,
                        &backoff,
                        budget,
                        &cancel,
                    )?;
                    match input.execute(&service, operation_id.clone(), budget, &cancel) {
                        Ok(mut value) => {
                            if let CoordinationResult::Events(EventsResult::Read {
                                unknown_hosts,
                                run_events_pending,
                                ..
                            }) = &mut value
                            {
                                unknown_hosts.extend(coverage.unknown_hosts);
                                *run_events_pending |= coverage.more_events;
                            }
                            PeerResult::Coordination(value)
                        }
                        Err(error) => PeerResult::Error {
                            code: error.code().into(),
                            operation_id: operation_id.clone(),
                        },
                    }
                }
            }
        };
        let response = PeerResponse {
            version: 1,
            host: context.identity.host,
            project: request.project.key,
            result,
        };
        let mut bytes = serde_json::to_vec(&response)?;
        if bytes.len() >= MAX_FRAME {
            return Err(PeerError::Frame.into());
        }
        bytes.push(b'\n');
        provenant_supervisor::sys::hook_io::write_stdout(
            &bytes,
            Instant::now() + std::time::Duration::from_secs(30),
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = std::io::Write::write_all(
                &mut io::stderr().lock(),
                format!("provenant: fabric peer: {error}\n").as_bytes(),
            );
            3
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn landing_local(
    context: &context::Context,
    executor: &SystemExecutor,
    proof: LandingProof,
    operation_id: &provenant_core::OperationId,
    reconcile: bool,
    actor: provenant_core::ActorId,
    budget: WaitBudget,
    clock: &SystemClock,
    cancel: &CancelToken,
) -> Result<provenant_fabric_store::PushOperation, Box<dyn std::error::Error>> {
    let backoff = ThreadWait;
    let request = provenant_fabric_store::Request {
        clock,
        budget,
        cancel,
        backoff: &backoff,
    };
    let store = provenant_fabric_store::Store::open(&context.roots.state, &request)?;
    let guard = store.lock_landing(context.identity.project, &proof.reference, &request)?;
    let fence = provenant_fabric_store::ClaimFence {
        id: proof.id,
        actor,
        generation: proof.generation,
        token: proof.token,
    };
    let git = provenant_fabric_runtime::git_transport::GitTransport::discover(
        executor,
        std::env::var_os("SSH_AUTH_SOCK").map(Into::into),
    )?;
    let cwd = context.identity.cwd.to_path_buf()?;
    if reconcile {
        Ok(git.reconcile_landing(
            &store,
            &guard,
            &fence,
            context.identity.project,
            operation_id,
            &request,
            &cwd,
            cancel,
        )?)
    } else {
        Ok(git.push_landing(&store, &guard, &fence, &cwd, operation_id, &request, cancel)?)
    }
}
pub fn landing(input: provenant_cli::fabric::Landing, globals: &provenant_cli::Globals) -> u8 {
    use provenant_cli::fabric::Landing;
    let (fence, operation, reconcile) = match input {
        Landing::Push { fence, operation } => (fence, operation, false),
        Landing::Reconcile { fence, operation } => (fence, operation, true),
        _ => return 2,
    };
    let recovery_id = operation.operation_id.clone();
    let command = if reconcile {
        "fabric landing reconcile"
    } else {
        "fabric landing push"
    };
    let result =
        (|| -> Result<provenant_fabric_store::PushOperation, Box<dyn std::error::Error>> {
            let proof = fence.proof()?;
            let factory = factory(globals)?;
            let ctx = &factory.context;
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(factory.clock.mono_now(), 300)?;
            if let Some(project) = &ctx.identity.federation
                && let Some(host) = ctx
                    .config
                    .home_hosts
                    .get(project.key.as_str())
                    .filter(|h| **h != ctx.identity.host)
            {
                let peer = ctx
                    .config
                    .peers
                    .values()
                    .find(|p| p.host == *host)
                    .ok_or(PeerError::Unavailable)?;
                let executable = system_ssh().ok_or(PeerError::Unavailable)?;
                let request = PeerRequest {
                    version: 1,
                    target_host: *host,
                    source_host: ctx.identity.host,
                    agent: ctx.identity.inbox.agent.clone(),
                    project: ProjectLocator::new(&ctx.identity, &factory.home)?,
                    action: PeerAction::LandingPush {
                        proof,
                        operation_id: operation.operation_id,
                        reconcile,
                    },
                };
                let transport = SshTransport {
                    executor: &factory.executor,
                    clock: &factory.clock,
                    home: &factory.home,
                    cwd: &factory.cwd,
                    executable,
                    seconds: ctx.config.limits.peer_seconds,
                };
                return match transport.exchange(peer, &request, budget, &cancel)?.result {
                    PeerResult::Push(value) => Ok(value),
                    PeerResult::Error {
                        code,
                        operation_id: Some(operation_id),
                    } if code == "launch_unknown" => {
                        Err(PeerError::Unknown { operation_id }.into())
                    }
                    PeerResult::Error { code, .. } => Err(PeerError::Refused { code }.into()),
                    _ => Err(PeerError::Frame.into()),
                };
            }
            landing_local(
                ctx,
                &factory.executor,
                proof,
                &operation.operation_id,
                reconcile,
                ctx.identity.inbox.actor,
                budget,
                &factory.clock,
                &cancel,
            )
        })();
    match result {
        Ok(data) => {
            let human = serde_json::to_string_pretty(&data).unwrap_or_default();
            crate::emit(
                &provenant_records::CommandResult::new(
                    command,
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                false,
            )
        }
        Err(error)
            if matches!(
                error.downcast_ref::<PeerError>(),
                Some(PeerError::Unknown { .. })
            ) || error
                .downcast_ref::<provenant_fabric_runtime::git_transport::GitTransportError>()
                .is_some_and(|error| error.is_unknown()) =>
        {
            let mut result = provenant_records::CommandResult::new(
                command,
                provenant_records::Outcome::Error,
                serde_json::json!({"operation_id":recovery_id,"state":"unknown"}),
            );
            result.error = Some(provenant_records::CommandError {
                code: "launch_unknown".into(),
                message: "push outcome requires remote reconciliation".into(),
                cause: None,
                repair: Some(
                    "repeat fabric landing reconcile with the same fence and operation id".into(),
                ),
            });
            crate::emit(
                &result,
                globals,
                "push outcome requires remote reconciliation",
                false,
            )
        }
        Err(error) => crate::fail(
            command,
            provenant_records::Outcome::Refused,
            "landing_refused",
            &error.to_string(),
            globals,
        ),
    }
}

struct BinaryRunOwner<'a> {
    factory: &'a Factory,
    allow_remote: bool,
}
fn peer_result<T>(
    result: Result<T, FacadeError>,
    operation_id: Option<provenant_core::OperationId>,
    wrap: impl FnOnce(T) -> PeerResult,
) -> PeerResult {
    match result {
        Ok(value) => wrap(value),
        Err(error) => PeerResult::Error {
            code: error.code().into(),
            operation_id,
        },
    }
}
fn run_error(error: provenant_run::RunError) -> FacadeError {
    use provenant_run::RunError;
    match error {
        RunError::NotFound => FacadeError::NotFound,
        RunError::Refused => FacadeError::ConfinementRefused,
        RunError::Contention | RunError::Capacity => FacadeError::Busy,
        RunError::Conflict | RunError::OperationConflict => FacadeError::Conflict,
        RunError::Cancelled => FacadeError::Cancelled,
        RunError::Validation(_) | RunError::Decode(_) | RunError::ArtifactMismatch => {
            FacadeError::StateCorrupt
        }
        _ => FacadeError::Io,
    }
}
/// Map a shared run-edge refusal to its stable facade code.
fn fail_error(fail: provenant_cli::run::Fail) -> FacadeError {
    match fail.code {
        "SECRET_INPUT" | "REFUSED" => FacadeError::ConfinementRefused,
        "INVALID" | "WORKTREE_REQUIRED" | "WORKTREE_NOT_APPLICABLE" => FacadeError::InvalidInput,
        "WORKTREE_MISSING" => FacadeError::WorktreeMissing,
        "NOT_FOUND" => FacadeError::NotFound,
        "CONFLICT" | "OPERATION_CONFLICT" | "NONTERMINAL" => FacadeError::Conflict,
        "CANCELLED" => FacadeError::Cancelled,
        "QUEUE_FULL" | "CONTENTION" => FacadeError::Busy,
        "UNSUPPORTED" => FacadeError::OwnerUnavailable,
        _ => FacadeError::Io,
    }
}
impl BinaryRunOwner<'_> {
    fn call<'a>(
        &'a self,
        budget: WaitBudget,
        cancel: &'a CancelToken,
    ) -> provenant_run::Request<'a> {
        provenant_run::Request {
            clock: &self.factory.clock,
            budget,
            cancel,
        }
    }
    fn session(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_cli::run::Session, FacadeError> {
        let ctx = &self.factory.context;
        let context = provenant_run::ExecutionContext {
            state_root: ctx.roots.state.clone(),
            project_id: ctx.identity.project,
            actor_id: ctx.identity.inbox.actor,
            read_prompts: false,
        };
        let store = provenant_run::RunStore::open(&context, &self.call(budget, cancel))
            .map_err(run_error)?;
        Ok(provenant_cli::run::Session { store, context })
    }
    fn store(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_run::RunStore, FacadeError> {
        Ok(self.session(budget, cancel)?.store)
    }
    /// Run the shared launch path with this process's resolved roots.
    fn with_edge<T>(
        &self,
        action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> Result<T, provenant_cli::run::Fail>,
    ) -> Result<T, FacadeError> {
        let assets = crate::roots::product_root().map_err(|_| FacadeError::OwnerUnavailable)?;
        action(&provenant_cli::run::Edge {
            roots: &self.factory.context.roots,
            home: &self.factory.home,
            assets: &assets,
            cwd: &self.factory.cwd,
        })
        .map_err(fail_error)
    }
    /// The attempts of a run this owner just accepted or continued.
    fn receipt(
        &self,
        session: &provenant_cli::run::Session,
        operation_id: provenant_core::OperationId,
        run: &provenant_run::RunRecord,
        started: &[provenant_run::Started],
        call: &provenant_run::Request<'_>,
    ) -> Result<SubmissionReceipt, FacadeError> {
        // An uncertain handoff is reconciled by receipt lookup, never relaunched.
        if started
            .iter()
            .any(|s| matches!(s, provenant_run::Started::Unknown { .. }))
        {
            return Err(FacadeError::LaunchUnknown { operation_id });
        }
        let page = session
            .store
            .lanes(
                &provenant_run::LaneQuery {
                    targets: vec![provenant_run::LaneTarget {
                        run: run.run_id,
                        task: None,
                        attempt: None,
                    }],
                    all: true,
                    limit: 100,
                    after: 0,
                },
                call,
            )
            .map_err(run_error)?;
        Ok(SubmissionReceipt {
            operation_id,
            targets: page
                .lanes
                .iter()
                .map(|lane| AttemptTarget {
                    attempt: lane.attempt_id,
                    host: Some(self.factory.context.identity.host),
                })
                .collect(),
        })
    }
    /// A local attempt, or the forwarded answer for another host's attempt.
    fn remote(
        &self,
        target: &AttemptTarget,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Option<PeerResult>, FacadeError> {
        match target.host {
            Some(host) if host != self.factory.context.identity.host => {
                if !self.allow_remote {
                    return Err(FacadeError::ConfinementRefused);
                }
                self.peer(host, action, budget, cancel).map(Some)
            }
            _ => Ok(None),
        }
    }
    fn snapshot(
        &self,
        target: AttemptTarget,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_records::run::AttemptRecord, FacadeError> {
        let identity = &self.factory.context.identity;
        if target.host.is_some_and(|h| h != identity.host) {
            if !self.allow_remote {
                return Err(FacadeError::ConfinementRefused);
            }
            let host = target.host.ok_or(FacadeError::InvalidInput)?;
            return match self.peer(
                host,
                PeerAction::Run {
                    input: RunInput::Status { target },
                },
                budget,
                cancel,
            )? {
                PeerResult::Run(value) => match *value {
                    RunResult::Status { attempt } => Ok(attempt),
                    _ => Err(FacadeError::StateCorrupt),
                },
                _ => Err(FacadeError::StateCorrupt),
            };
        }
        self.store(budget, cancel)?
            .record(
                target.attempt,
                &provenant_run::Request {
                    clock: &self.factory.clock,
                    budget,
                    cancel,
                },
            )
            .map_err(run_error)
    }
    fn peer(
        &self,
        host: provenant_core::HostId,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResult, FacadeError> {
        let ctx = &self.factory.context;
        let peer = ctx
            .config
            .peers
            .values()
            .find(|p| p.host == host)
            .ok_or(FacadeError::HomeUnavailable)?;
        let project = ProjectLocator::new(&ctx.identity, &self.factory.home)
            .map_err(|_| FacadeError::ConfinementRefused)?;
        let request = PeerRequest {
            version: 1,
            target_host: host,
            source_host: ctx.identity.host,
            agent: ctx.identity.inbox.agent.clone(),
            project,
            action,
        };
        let transport = SshTransport {
            executor: &self.factory.executor,
            clock: &self.factory.clock,
            home: &self.factory.home,
            cwd: &self.factory.cwd,
            executable: system_ssh().ok_or(FacadeError::HomeUnavailable)?,
            seconds: ctx.config.limits.peer_seconds,
        };
        match transport
            .exchange(peer, &request, budget, cancel)
            .map_err(|e| match e {
                PeerError::Unknown { operation_id } => FacadeError::LaunchUnknown { operation_id },
                PeerError::Version => FacadeError::PeerVersion,
                _ => FacadeError::HomeUnavailable,
            })?
            .result
        {
            PeerResult::Error { code, operation_id } => Err(peer_error(&code, operation_id)),
            value => Ok(value),
        }
    }
}
/// Decode a peer's refusal, keeping the operation an uncertain mutation names.
fn peer_error(code: &str, operation_id: Option<provenant_core::OperationId>) -> FacadeError {
    match (code, operation_id) {
        ("launch_unknown", Some(operation_id)) => FacadeError::LaunchUnknown { operation_id },
        ("not_found", _) => FacadeError::NotFound,
        ("worktree_missing", _) => FacadeError::WorktreeMissing,
        ("output_too_large", _) => FacadeError::OutputTooLarge,
        ("invalid_input", _) => FacadeError::InvalidInput,
        ("confinement_refused", _) => FacadeError::ConfinementRefused,
        ("continuation_unsupported", _) => FacadeError::ContinuationUnsupported,
        ("owner_unavailable", _) => FacadeError::OwnerUnavailable,
        ("cancelled", _) => FacadeError::Cancelled,
        ("conflict", _) => FacadeError::Conflict,
        ("busy", _) => FacadeError::Busy,
        _ => FacadeError::HomeUnavailable,
    }
}
impl RunOwner for BinaryRunOwner<'_> {
    fn capabilities(&self) -> RunCapabilities {
        RunCapabilities {
            submit: true,
            list: false,
            status: true,
            wait: true,
            cancel: true,
            resume: provenant_cli::run::Registry { roots: None }.continues_sessions(),
            output: true,
            events: true,
        }
    }
    /// Submissions take the CLI's scan, record and launch path. Fabric never
    /// creates queued rows itself or reconstructs a provider invocation.
    fn submit(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<SubmissionReceipt, FacadeError> {
        use provenant_cli::run::{Submission, Submitted, derived_task, requested, worktree_access};
        let SubmitInput::Submission {
            operation_id,
            tasks,
            concurrency,
            knobs,
        } = input;
        let mut submitted = Vec::new();
        for (index, task) in tasks.into_iter().enumerate() {
            worktree_access(
                task.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite,
                task.worktree.as_deref().map(Path::new),
                &self.factory.cwd,
            )
            .map_err(fail_error)?;
            if !task.inputs.is_empty() {
                return Err(FacadeError::InvalidInput);
            }
            let route = &task.route;
            let model = match (&route.adapter, &route.model) {
                (_, Some(model)) if model.contains('/') => Some(model.clone()),
                (Some(adapter), Some(model)) => Some(format!("{adapter}/{model}")),
                (None, Some(_)) => return Err(FacadeError::InvalidInput),
                (_, None) => None,
            };
            let name = match model {
                Some(_) => None,
                None => route.role.as_deref().or(route.task_class.as_deref()),
            };
            submitted.push(Submitted {
                task_class: route.task_class.clone(),
                task_id: derived_task(&operation_id, index).map_err(fail_error)?,
                prompt: task.prompt,
                route: requested(model.as_deref(), name, route.effort.clone())
                    .map_err(fail_error)?,
                fallback: Vec::new(),
                provider_timeout: task.timeout_ms.div_ceil(1000).max(1),
                delivery_id: task.delivery_id,
            });
        }
        let session = self.session(budget, cancel)?;
        let call = self.call(budget, cancel);
        let (run, started) = self.with_edge(|edge| {
            provenant_cli::run::submit(
                &session,
                edge,
                Submission {
                    operation_id: operation_id.clone(),
                    tasks: submitted,
                    concurrency: concurrency.map(u64::from),
                    knobs,
                },
                &call,
            )
        })?;
        self.receipt(&session, operation_id, &run, &started, &call)
    }
    fn output(
        &mut self,
        input: OutputInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<OutputResult, FacadeError> {
        let target = match &input {
            OutputInput::Read { target, .. } | OutputInput::Fetch { target, .. } => target.clone(),
        };
        if let Some(answer) = self.remote(
            &target,
            PeerAction::Output {
                input: input.clone(),
            },
            budget,
            cancel,
        )? {
            return match answer {
                PeerResult::Output(value) => Ok(value),
                _ => Err(FacadeError::StateCorrupt),
            };
        }
        let session = self.session(budget, cancel)?;
        let call = self.call(budget, cancel);
        let record = session
            .store
            .record(target.attempt, &call)
            .map_err(run_error)?;
        let digest = |chunk: &provenant_run::OutputChunk| {
            provenant_core::Digest::from_bytes(*chunk.digest.bytes())
        };
        match input {
            OutputInput::Read {
                part,
                offset,
                max_bytes,
                ..
            } => {
                let artifact = match part {
                    OutputPart::Stdout => provenant_run::ArtifactPart::Result,
                    OutputPart::Stderr => provenant_run::ArtifactPart::Stderr,
                    OutputPart::Combined => return Err(FacadeError::InvalidInput),
                };
                let chunk = session
                    .store
                    .read_output(
                        record.attempt_id,
                        artifact,
                        provenant_run::ByteRange { offset, max_bytes },
                        &session.context,
                        &call,
                    )
                    .map_err(run_error)?;
                Ok(OutputResult::Chunk {
                    target,
                    part,
                    offset: chunk.offset,
                    next_offset: chunk.next_offset,
                    eof: chunk.eof,
                    digest: digest(&chunk),
                    bytes: OutputBytes::from_bytes(chunk.bytes),
                })
            }
            OutputInput::Fetch {
                artifact,
                max_bytes,
                ..
            } => {
                // Only the attempt's own bound result is fetchable.
                let bound = record.result.as_ref().ok_or(FacadeError::NotFound)?;
                if artifact.path != bound.path || artifact.digest.is_some_and(|d| d != bound.digest)
                {
                    return Err(FacadeError::NotFound);
                }
                // A fetch returns the whole artifact or refuses; the reader
                // pages at 64 KiB, below Fabric's fetch limit.
                let mut bytes = Vec::new();
                loop {
                    let chunk = session
                        .store
                        .read_output(
                            record.attempt_id,
                            provenant_run::ArtifactPart::Result,
                            provenant_run::ByteRange {
                                offset: bytes.len() as u64,
                                max_bytes: provenant_records::run::OUTPUT_LIMIT,
                            },
                            &session.context,
                            &call,
                        )
                        .map_err(run_error)?;
                    if chunk.len > max_bytes {
                        return Err(FacadeError::OutputTooLarge);
                    }
                    bytes.extend_from_slice(&chunk.bytes);
                    if chunk.eof {
                        return Ok(OutputResult::Artifact {
                            target,
                            artifact,
                            digest: digest(&chunk),
                            bytes: OutputBytes::from_bytes(bytes),
                        });
                    }
                }
            }
        }
    }
    fn run(
        &mut self,
        input: RunInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<RunResult, FacadeError> {
        match input {
            RunInput::Status { target } => Ok(RunResult::Status {
                attempt: self.snapshot(target, budget, cancel)?,
            }),
            RunInput::Wait {
                targets,
                condition,
                wait_ms,
            } => {
                let requested = WaitBudget::new(
                    self.factory.clock.mono_now(),
                    provenant_core::DurationMicros::from_micros(
                        wait_ms.checked_mul(1000).ok_or(FacadeError::InvalidInput)?,
                    ),
                    provenant_core::WaitTransport::Cli,
                )
                .map_err(|_| FacadeError::InvalidInput)?;
                let budget =
                    if requested.deadline().as_mono_time() < budget.deadline().as_mono_time() {
                        requested
                    } else {
                        budget
                    };
                let mut last_attempts = None;
                loop {
                    if cancel.is_cancelled() {
                        return Err(FacadeError::Cancelled);
                    }
                    if budget.remaining(self.factory.clock.mono_now()).as_micros() == 0 {
                        if let Some(attempts) = last_attempts {
                            return Ok(RunResult::Wait {
                                condition,
                                attempts,
                                satisfied: false,
                                timed_out: true,
                            });
                        }
                        return Err(FacadeError::Timeout);
                    }
                    let attempts = targets
                        .iter()
                        .map(|target| self.snapshot(target.clone(), budget, cancel))
                        .collect::<Result<Vec<_>, _>>()?;
                    let satisfied = provenant_fabric_runtime::runs::wait_satisfied(
                        condition,
                        attempts.iter().map(|r| &r.state),
                    );
                    let timed_out =
                        budget.remaining(self.factory.clock.mono_now()).as_micros() == 0;
                    if satisfied || timed_out {
                        return Ok(RunResult::Wait {
                            condition,
                            attempts,
                            satisfied,
                            timed_out: !satisfied && timed_out,
                        });
                    }
                    last_attempts = Some(attempts);
                    provenant_fabric_store::RetryWait::wait(
                        &self.factory.backoff,
                        provenant_core::DurationMicros::from_micros(
                            50_000.min(budget.remaining(self.factory.clock.mono_now()).as_micros()),
                        ),
                        cancel,
                    );
                }
            }
            RunInput::Cancel {
                ref target,
                ref operation_id,
            } => {
                if let Some(answer) = self.remote(
                    target,
                    PeerAction::Run {
                        input: input.clone(),
                    },
                    budget,
                    cancel,
                )? {
                    return match answer {
                        PeerResult::Run(value) => Ok(*value),
                        _ => Err(FacadeError::StateCorrupt),
                    };
                }
                let store = self.store(budget, cancel)?;
                let call = self.call(budget, cancel);
                store
                    .bind_cancel(operation_id, &target.attempt.to_string(), &call)
                    .map_err(run_error)?;
                let reason = format!("cancelled by Fabric operation {operation_id}");
                let receipt = store
                    .cancel(target.attempt, &reason, &call)
                    .map_err(run_error)?;
                // Without a launch summary nothing was asked to stop.
                if let provenant_run::CancelReceipt::Unobservable { .. } = receipt {
                    return Err(FacadeError::LaunchUnknown {
                        operation_id: operation_id.clone(),
                    });
                }
                Ok(RunResult::Cancel {
                    attempt: store.record(target.attempt, &call).map_err(run_error)?,
                })
            }
            RunInput::Resume {
                ref target,
                ref operation_id,
                ref prompt,
                expected_generation,
            } => {
                if let Some(answer) = self.remote(
                    target,
                    PeerAction::Run {
                        input: input.clone(),
                    },
                    budget,
                    cancel,
                )? {
                    return match answer {
                        PeerResult::Run(value) => Ok(*value),
                        _ => Err(FacadeError::StateCorrupt),
                    };
                }
                if !self.capabilities().resume {
                    return Err(FacadeError::ContinuationUnsupported);
                }
                let session = self.session(budget, cancel)?;
                let call = self.call(budget, cancel);
                let (run, started) = self.with_edge(|edge| {
                    provenant_cli::run::continue_attempt(
                        &session,
                        edge,
                        target.attempt,
                        operation_id.clone(),
                        prompt.clone(),
                        expected_generation,
                        &call,
                    )
                })?;
                let receipt =
                    self.receipt(&session, operation_id.clone(), &run, &started, &call)?;
                let attempt = receipt.targets.first().ok_or(FacadeError::StateCorrupt)?;
                Ok(RunResult::Resume {
                    attempt: session
                        .store
                        .record(attempt.attempt, &call)
                        .map_err(run_error)?,
                })
            }
            RunInput::List { .. } => Err(FacadeError::OwnerUnavailable),
        }
    }
    fn events(
        &mut self,
        after: u64,
        limit: u16,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<RunEventsPage, FacadeError> {
        if !(1..=64).contains(&limit) {
            return Err(FacadeError::InvalidInput);
        }
        let items = self
            .store(budget, cancel)?
            .events(
                after,
                usize::from(limit),
                None,
                &provenant_run::Request {
                    clock: &self.factory.clock,
                    budget,
                    cancel,
                },
            )
            .map_err(run_error)?;
        let cursor = items.last().map_or(after, |event| event.sequence);
        let truncated = items.len() == usize::from(limit);
        Ok(RunEventsPage {
            items,
            cursor,
            truncated,
        })
    }
}

struct BoundGraphOwner<'a> {
    factory: &'a Factory,
    budget: WaitBudget,
    cancel: &'a CancelToken,
}
impl provenant_fabric_runtime::graph::AttemptOwner for BoundGraphOwner<'_> {
    type Error = FacadeError;
    fn observe_or_start(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        prompt: &str,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
        use provenant_fabric_runtime::graph::{AttemptObservation, AttemptOutcome};
        let operation_id = graph_operation("graph-attempt-operation", key)?;
        let input = SubmitInput::Submission {
            operation_id,
            tasks: vec![SubmittedTask {
                access,
                worktree: worktree.map(str::to_owned),
                prompt: prompt.into(),
                inputs: vec![],
                route: RoutePreference {
                    adapter: Some(route.adapter.clone()),
                    model: route.model.clone(),
                    route: route.alias.clone(),
                    role: None,
                    task_class: None,
                    effort: route.effort.clone(),
                },
                timeout_ms: self.factory.context.config.limits.graph_timeout_ms,
                delivery_id: None,
            }],
            concurrency: Some(1),
            knobs: vec![],
        };
        let mut owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
        };
        let receipt = owner.submit(input, self.budget, self.cancel)?;
        let target = receipt
            .targets
            .into_iter()
            .next()
            .ok_or(FacadeError::StateCorrupt)?;
        let attempt = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        let reference = Some(format!(
            "{}/{}",
            target.host.unwrap_or(self.factory.context.identity.host),
            target.attempt
        ));
        use provenant_records::run::{AttemptState, Outcome};
        match attempt.state {
            AttemptState::Terminal { outcome, .. } => {
                let launched = attempt.provider_launched.unwrap_or(true);
                Ok(AttemptObservation::Terminal {
                    launched,
                    attempt_ref: reference,
                    outcome: match outcome {
                        Outcome::Succeeded => AttemptOutcome::Succeeded,
                        Outcome::AuthRequired => AttemptOutcome::AuthRequired,
                        Outcome::RateLimited | Outcome::QuotaLimited => {
                            AttemptOutcome::UsageLimited
                        }
                        // A route refused before launch leaves the next route free to try.
                        Outcome::ModelUnavailable => AttemptOutcome::AdapterUnavailable,
                        Outcome::Rejected if !launched => AttemptOutcome::AdapterUnavailable,
                        Outcome::Failed => AttemptOutcome::TaskFailed,
                        _ => AttemptOutcome::OtherFailure,
                    },
                })
            }
            _ => Ok(AttemptObservation::Pending {
                attempt_ref: reference,
            }),
        }
    }
}
impl Factory {
    fn graph(
        &self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        use provenant_fabric_runtime::coordination::input::ValidateInput;
        use provenant_fabric_runtime::graph::{
            FileJournal, GraphDocument, GraphJournal, GraphRun, GraphStatus,
        };
        input
            .validate_input(provenant_core::WaitTransport::Cli)
            .map_err(|_| FacadeError::InvalidInput)?;
        let resume = matches!(&input, GraphInput::Resume { .. });
        let document = match &input {
            GraphInput::Run { document } | GraphInput::Resume { document } => {
                let path = if document.is_absolute() {
                    document.clone()
                } else {
                    self.cwd.join(document)
                };
                let path =
                    provenant_supervisor::fs::canonicalize(&path).map_err(|_| FacadeError::Io)?;
                if !path.starts_with(&self.cwd) {
                    return Err(FacadeError::ConfinementRefused);
                }
                let bytes = provenant_supervisor::fs::FileHandle::open_source(&path)
                    .and_then(|mut file| file.read_bounded(1_048_576))
                    .map_err(|_| FacadeError::Io)?;
                let document: GraphDocument = provenant_records::decode_v1(
                    bytes.as_slice(),
                    provenant_records::Limits::CONFIG,
                )
                .map_err(|_| FacadeError::InvalidInput)?;
                for node in &document.nodes {
                    if node.prompt.len() as u64 > self.context.config.limits.body_bytes {
                        return Err(FacadeError::InvalidInput);
                    }
                }
                Some(document)
            }
            _ => None,
        };
        let id = match &input {
            GraphInput::Status { id } | GraphInput::Cancel { id } => id.as_str(),
            _ => document
                .as_ref()
                .ok_or(FacadeError::InvalidInput)?
                .graph_id
                .as_str(),
        };
        let mut journal = FileJournal::open(
            &self.context.roots.state,
            self.context.identity.project,
            id,
            &provenant_supervisor::fs::LocalFs,
            budget,
            cancel,
            &self.clock,
        )
        .map_err(|_| FacadeError::Io)?;
        if let Some(document) = document {
            let mut owner = BoundGraphOwner {
                factory: self,
                budget,
                cancel,
            };
            let run = if resume {
                provenant_fabric_runtime::graph::resume(&document, &mut owner, &mut journal)
            } else {
                provenant_fabric_runtime::graph::advance(&document, &mut owner, &mut journal)
            };
            return run.map_err(|error| match error {
                provenant_fabric_runtime::graph::GraphError::Run(error) => error,
                provenant_fabric_runtime::graph::GraphError::Invalid(_) => {
                    FacadeError::InvalidInput
                }
                provenant_fabric_runtime::graph::GraphError::PlanChanged => FacadeError::Conflict,
                _ => FacadeError::StateCorrupt,
            });
        }
        let mut checkpoint = journal
            .load(id)
            .map_err(|_| FacadeError::StateCorrupt)?
            .ok_or(FacadeError::NotFound)?;
        if matches!(input, GraphInput::Cancel { .. }) {
            // Persist the admission fence before asking the run owner to stop live attempts.
            checkpoint.cancelled = true;
            journal.save(&checkpoint).map_err(|_| FacadeError::Io)?;
            let mut owner = BinaryRunOwner {
                factory: self,
                allow_remote: true,
            };
            for node in &checkpoint.nodes {
                for attempt in &node.attempts {
                    if attempt.state == provenant_fabric_runtime::graph::AttemptState::Terminal {
                        continue;
                    }
                    let reference = match attempt.attempt_ref.as_deref() {
                        Some(reference) => reference,
                        None => {
                            return Err(FacadeError::LaunchUnknown {
                                operation_id: graph_operation(
                                    "graph-attempt-operation",
                                    &attempt.key,
                                )?,
                            });
                        }
                    };
                    let (host, attempt_id) =
                        reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                    let operation_id = graph_operation("graph-cancel", &attempt.key)?;
                    owner.run(
                        RunInput::Cancel {
                            target: AttemptTarget {
                                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                                attempt: attempt_id
                                    .parse()
                                    .map_err(|_| FacadeError::StateCorrupt)?,
                            },
                            operation_id,
                        },
                        budget,
                        cancel,
                    )?;
                }
            }
        }
        let status = if checkpoint.cancelled {
            GraphStatus::Cancelled
        } else if checkpoint.nodes.iter().any(|node| {
            matches!(
                node.state,
                provenant_fabric_runtime::graph::NodeState::Pending
                    | provenant_fabric_runtime::graph::NodeState::Running
            )
        }) {
            GraphStatus::Pending
        } else {
            GraphStatus::Complete
        };
        Ok(GraphRun { status, checkpoint })
    }
}
fn graph_operation(
    domain: &str,
    key: &provenant_fabric_runtime::graph::AttemptKey,
) -> Result<provenant_core::OperationId, FacadeError> {
    let digest = provenant_records::CanonicalV1::record_digest(domain, 1, key)
        .map_err(|_| FacadeError::InvalidInput)?;
    provenant_core::OperationId::parse(&format!("graph-{}", provenant_core::hex(digest.as_bytes())))
        .map_err(|_| FacadeError::InvalidInput)
}
pub fn graph(input: provenant_cli::fabric::Graph, globals: &provenant_cli::Globals) -> u8 {
    use provenant_cli::fabric::Graph;
    let (command, input) = match input {
        Graph::Run { document } => ("fabric graph run", GraphInput::Run { document }),
        Graph::Status { id } => ("fabric graph status", GraphInput::Status { id }),
        Graph::Cancel { id } => ("fabric graph cancel", GraphInput::Cancel { id }),
        Graph::Resume { document } => ("fabric graph resume", GraphInput::Resume { document }),
    };
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let factory = factory(globals)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
        Ok(factory.graph(input, budget, &cancel)?)
    })();
    match result {
        Ok(data) => {
            let human = serde_json::to_string_pretty(&data).unwrap_or_default();
            crate::emit(
                &provenant_records::CommandResult::new(
                    command,
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                false,
            )
        }
        Err(error) => crate::fail(
            command,
            provenant_records::Outcome::Error,
            "graph_owner",
            &error.to_string(),
            globals,
        ),
    }
}

impl Factory {
    fn pull_events(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<EventCoverage, FacadeError> {
        self.pull_events_from(true, budget, cancel)
    }
    fn pull_events_from(
        &self,
        allow_peers: bool,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<EventCoverage, FacadeError> {
        let ctx = &self.context;
        if ctx.identity.federation.as_ref().is_some_and(|p| {
            ctx.config
                .home_hosts
                .get(p.key.as_str())
                .is_some_and(|h| *h != ctx.identity.host)
        }) {
            return Ok(EventCoverage::default());
        }
        let request = provenant_fabric_store::Request {
            clock: &self.clock,
            budget,
            cancel,
            backoff: &self.backoff,
        };
        let store = provenant_fabric_store::Store::open(&ctx.roots.state, &request)
            .map_err(|_| FacadeError::Io)?;
        let mut owner = BinaryRunOwner {
            factory: self,
            allow_remote: true,
        };
        let mut hosts = std::collections::BTreeSet::new();
        hosts.insert(ctx.identity.host);
        hosts.extend(ctx.config.peers.values().map(|p| p.host));
        let mut coverage = EventCoverage::default();
        for host in hosts {
            if !allow_peers && host != ctx.identity.host {
                coverage.unknown_hosts.push(host);
                continue;
            }
            if cancel.is_cancelled() {
                return Err(FacadeError::Cancelled);
            }
            let after = store
                .run_event_cursor(ctx.identity.project, host, &request)
                .map_err(|_| FacadeError::Io)?;
            let result = if host == ctx.identity.host {
                owner.events(after, 64, budget, cancel)
            } else {
                owner
                    .peer(
                        host,
                        PeerAction::RunEvents { after, limit: 64 },
                        budget,
                        cancel,
                    )
                    .and_then(|result| match result {
                        PeerResult::RunEvents(page) => Ok(page),
                        _ => Err(FacadeError::StateCorrupt),
                    })
            };
            match result {
                Ok(page) => {
                    if page.cursor != page.items.last().map_or(after, |event| event.sequence) {
                        return Err(FacadeError::StateCorrupt);
                    }
                    store
                        .ingest_run_events(ctx.identity.project, host, &page.items, &request)
                        .map_err(|_| FacadeError::StateCorrupt)?;
                    coverage.more_events |= page.truncated;
                }
                Err(_) => coverage.unknown_hosts.push(host),
            }
        }
        Ok(coverage)
    }
}

/// Pull bounded execution pages before each short coordination observation.
/// No database transaction spans a peer call or a wait.
pub fn events(
    page: provenant_cli::fabric::SequencePage,
    wait: Option<u64>,
    globals: &provenant_cli::Globals,
) -> u8 {
    let result = (|| -> Result<(EventsResult, bool), Box<dyn std::error::Error>> {
        let factory = factory(globals)?;
        if page.limit > factory.context.config.limits.page_rows {
            return Err(FacadeError::InvalidInput.into());
        }
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), wait.unwrap_or(30))?;
        let service = factory.coordination()?;
        let mut unavailable = std::collections::BTreeSet::new();
        loop {
            let expired = budget.remaining(factory.clock.mono_now()).as_micros() == 0;
            let coverage = if expired {
                EventCoverage::default()
            } else {
                match factory.pull_events(budget, &cancel) {
                    Ok(value) => value,
                    Err(_)
                        if wait.is_some()
                            && budget.remaining(factory.clock.mono_now()).as_micros() == 0 =>
                    {
                        EventCoverage::default()
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            unavailable.extend(coverage.unknown_hosts);
            let result = if budget.remaining(factory.clock.mono_now()).as_micros() == 0 {
                EventsResult::empty_timeout(page.after)
            } else {
                match service.events(
                    EventsInput::Read {
                        after: page.after,
                        limit: page.limit,
                        wait_ms: None,
                    },
                    budget,
                    &cancel,
                ) {
                    Ok(value) => value,
                    Err(_)
                        if wait.is_some()
                            && budget.remaining(factory.clock.mono_now()).as_micros() == 0 =>
                    {
                        EventsResult::empty_timeout(page.after)
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let EventsResult::Read {
                page: records,
                cursor,
                unknown_hosts,
                run_events_pending,
                timed_out: owner_timeout,
            } = result
            else {
                return Err(FacadeError::StateCorrupt.into());
            };
            unavailable.extend(unknown_hosts);
            let timed_out = owner_timeout
                || (wait.is_some() && budget.remaining(factory.clock.mono_now()).as_micros() == 0);
            if !records.items.is_empty() || wait.is_none() || timed_out {
                return Ok((
                    EventsResult::Read {
                        page: records,
                        cursor,
                        timed_out,
                        unknown_hosts: unavailable.into_iter().collect(),
                        run_events_pending: coverage.more_events || run_events_pending,
                    },
                    timed_out,
                ));
            }
            provenant_fabric_store::RetryWait::wait(
                &factory.backoff,
                provenant_core::DurationMicros::from_micros(
                    500_000.min(budget.remaining(factory.clock.mono_now()).as_micros()),
                ),
                &cancel,
            );
        }
    })();
    match result {
        Ok((data, timed_out)) => {
            let human = serde_json::to_string_pretty(&data).unwrap_or_default();
            crate::emit(
                &provenant_records::CommandResult::new(
                    "fabric events read",
                    if timed_out {
                        provenant_records::Outcome::Timeout
                    } else {
                        provenant_records::Outcome::Ok
                    },
                    data,
                ),
                globals,
                &human,
                true,
            )
        }
        Err(error) => crate::fail(
            "fabric events read",
            provenant_records::Outcome::Error,
            "events_read",
            &error.to_string(),
            globals,
        ),
    }
}
#[cfg(test)]
mod peer_tests {
    #[test]
    fn forwarded_unobservable_cancel_keeps_its_operation() -> Result<(), Box<dyn std::error::Error>>
    {
        // The home host answers an unobservable cancel through `peer_result`.
        let id = provenant_core::OperationId::parse("stop")?;
        let unknown = super::FacadeError::LaunchUnknown {
            operation_id: id.clone(),
        };
        let reply =
            super::peer_result::<()>(Err(unknown), Some(id), |()| super::PeerResult::Error {
                code: String::new(),
                operation_id: None,
            });
        let super::PeerResult::Error { code, operation_id } = reply else {
            return Err("expected a peer refusal".into());
        };
        assert!(matches!(
            super::peer_error(&code, operation_id),
            super::FacadeError::LaunchUnknown { operation_id } if operation_id.as_str() == "stop"
        ));
        Ok(())
    }
}
