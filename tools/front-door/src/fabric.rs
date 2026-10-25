//! Fabric process composition: MCP, fixed peer ingress and host observations.
use provenant_cli::fabric::Context;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_mcp::{
    stdio,
    tools::{Service, ServiceError},
};
use provenant_fabric_runtime::runs::{
    AttemptPage, RunCapabilities, RunEventsPage, RunResult, SubmissionReceipt,
};
use provenant_fabric_runtime::{
    coordination::{facade::*, *},
    hosts::*,
    identity::Identity,
};
use provenant_supervisor::{clock::SystemClock, exec::SystemExecutor};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
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
    context: Context,
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
fn fair_probe_micros(remaining_micros: u64, probes_remaining: usize) -> Option<u64> {
    let probes_remaining = u64::try_from(probes_remaining).ok()?;
    let share = remaining_micros.checked_div(probes_remaining)?;
    (share > 0).then_some(share)
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
        .with_limits(self.context.config.limits.clone())
        .with_lane_restrictions(self.context.in_lane))
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
            actor: None,
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
            actor: None,
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
            actor: None,
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
        let mut candidates: std::collections::BTreeMap<
            provenant_core::HostId,
            Option<&provenant_fabric_runtime::config::Peer>,
        > = std::collections::BTreeMap::new();
        candidates.insert(ctx.identity.host, None);
        for peer in ctx.config.peers.values() {
            candidates.entry(peer.host).or_insert(Some(peer));
        }
        let selected = candidates
            .into_iter()
            .filter(|(host, _)| after.is_none_or(|cursor| *host > cursor))
            .take(limit as usize)
            .collect::<Vec<_>>();
        let mut probes_remaining = selected.iter().filter(|(_, peer)| peer.is_some()).count();
        let locator = (probes_remaining > 0)
            .then(|| ProjectLocator::new(&ctx.identity, &self.factory.home).ok())
            .flatten();
        let executable = (probes_remaining > 0)
            .then(|| {
                provenant_fabric_runtime::hosts::discover_ssh_executable(
                    ctx.config.ssh_executable.as_deref(),
                )
                .ok()
            })
            .flatten();
        let mut page = Vec::with_capacity(selected.len());
        for (host, peer) in selected {
            if cancel.is_cancelled() {
                return Err(FacadeError::Cancelled);
            }
            let Some(peer) = peer else {
                page.push(HostHealth {
                    host,
                    available: true,
                    execution: Some(
                        BinaryRunOwner {
                            factory: self.factory,
                            allow_remote: true,
                            actor: None,
                        }
                        .capabilities()
                        .submit,
                    ),
                });
                continue;
            };
            let mut health = HostHealth {
                host,
                available: false,
                execution: None,
            };
            let cap = fair_probe_micros(
                budget.remaining(self.factory.clock.mono_now()).as_micros(),
                probes_remaining,
            );
            probes_remaining = probes_remaining.saturating_sub(1);
            if let (Some(project), Some(executable), Some(cap)) =
                (locator.clone(), executable.clone(), cap)
            {
                let request = PeerRequest {
                    version: 1,
                    target_host: host,
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
                }) = transport.exchange_with_cap(
                    peer,
                    &request,
                    budget,
                    cancel,
                    std::time::Duration::from_micros(cap),
                ) {
                    health = value;
                }
            }
            page.push(health);
        }
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled);
        }
        Ok(page)
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
        let mut context = Context::open(roots, &home, &cwd)?;
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
    let context = Context::open(roots, &home, &cwd)?;
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
            let human = provenant_cli::terse(&serde_json::to_value(&data).unwrap_or_default());
            crate::emit(
                &provenant_records::CommandResult::new(
                    "hosts",
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                true,
            )
        }
        Err(error) => crate::fail(
            "hosts",
            provenant_records::Outcome::Error,
            "io",
            &error.to_string(),
            globals,
        ),
    }
}
pub fn peer(globals: &provenant_cli::Globals, in_process: bool) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // A request names its own agent, so a lane could act as any actor.
        if provenant_cli::fabric::in_lane() {
            return Err(CoordinationError::LaneMutation.into());
        }
        let bytes = provenant_supervisor::sys::hook_io::read_stdin(
            MAX_FRAME,
            Instant::now() + std::time::Duration::from_secs(30),
        )?;
        let home = crate::roots::home()?;
        if !in_process {
            let assets = crate::roots::product_root()?;
            let state = crate::roots::roots(globals, &assets, &home)?.state;
            // Only an unreachable agent falls back; a sent request is never repeated.
            let reply = match crate::peer_agent::forward(state.as_path(), &bytes)? {
                crate::peer_agent::Forward::Replied(reply) => Some(reply),
                // Refused unread at the agent's cap: nothing ran, so say busy.
                crate::peer_agent::Forward::Busy => {
                    let request = decode_request(&bytes)?;
                    let mut busy = serde_json::to_vec(&PeerResponse {
                        version: 1,
                        host: request.target_host,
                        project: request.project.key,
                        result: PeerResult::Error {
                            code: "busy".into(),
                            operation_id: None,
                        },
                    })?;
                    busy.push(b'\n');
                    Some(busy)
                }
                crate::peer_agent::Forward::Unreachable => None,
            };
            if let Some(reply) = reply {
                provenant_supervisor::sys::hook_io::write_stdout(
                    &reply,
                    Instant::now() + std::time::Duration::from_secs(30),
                )?;
                return Ok(());
            }
        }
        let request = decode_request(&bytes)?;
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
        let mut context = Context::open(roots, &home, &cwd)?;
        request.validate(context.identity.host, &context.config, &home)?;
        if context.identity.federation.as_ref().map(|p| &p.key) != Some(&request.project.key) {
            return Err(PeerError::Project.into());
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let seconds = if let PeerAction::SessionHandoff { budget_seconds, .. } = &request.action {
            crate::handoff::peer_seconds(*budget_seconds, context.config.limits.peer_seconds)
        } else {
            30
        };
        let budget = WaitBudget::cli(clock.mono_now(), seconds)?;
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
            actor: None,
        };
        let result = match &request.action {
            PeerAction::RunStatuses { targets } => {
                if !(1..=100).contains(&targets.len()) {
                    PeerResult::Error {
                        code: "invalid_input".into(),
                        operation_id: None,
                    }
                } else {
                    peer_result(
                        owner.snapshots_local(targets, budget, &cancel),
                        None,
                        PeerResult::RunStatuses,
                    )
                }
            }
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
            PeerAction::Lanes { all, limit } => peer_result(
                owner.lanes_local(*all, *limit, budget, &cancel),
                None,
                PeerResult::RunStatuses,
            ),
            PeerAction::SessionHandoff { input, .. } => {
                PeerResult::SessionHandoff(crate::handoff::receive(
                    &crate::handoff::Target {
                        context,
                        cwd: &cwd,
                        source: request.source_host,
                        clock: &clock,
                        budget,
                    },
                    input,
                    &crate::handoff::Terminal,
                ))
            }
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
                        Err(error) => {
                            let code = landing_error_code(error.as_ref());
                            PeerResult::Error {
                                code: code.into(),
                                operation_id: (code == "launch_unknown")
                                    .then(|| operation_id.clone()),
                            }
                        }
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
                            peer_factory.pull_events_from(false, budget, &cancel)
                        } else {
                            Ok(EventCoverage::default())
                        };
                    match coverage {
                        Ok(coverage) => {
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

fn store_error_code(error: &provenant_fabric_store::Error) -> &'static str {
    use provenant_fabric_store::Error as StoreError;
    match error {
        StoreError::NotFound => "not_found",
        StoreError::StaleFence => "stale_fence",
        StoreError::Conflict
        | StoreError::PushUnresolved { .. }
        | StoreError::PushTransition { .. }
        | StoreError::Overlap { .. } => "conflict",
        StoreError::Busy => "busy",
        StoreError::Cancelled => "cancelled",
        StoreError::Invalid(_) | StoreError::PathEncoding => "invalid_input",
        StoreError::Decode(_)
        | StoreError::Encode(_)
        | StoreError::SchemaVersion(_)
        | StoreError::UnversionedDatabase
        | StoreError::JournalMode(_) => "state_corrupt",
        StoreError::LegacyImportData { .. } => "upgrade_refused",
        _ => "io",
    }
}

fn landing_error_code(error: &(dyn std::error::Error + 'static)) -> &'static str {
    use provenant_fabric_runtime::git_transport::GitTransportError as GitError;
    if let Some(error) = error.downcast_ref::<GitError>() {
        if error.is_unknown() {
            return "launch_unknown";
        }
        return match error {
            GitError::Store(error) => store_error_code(error),
            GitError::PushNotFound { .. } => "not_found",
            GitError::Invalid(_) | GitError::InvalidRemote | GitError::BundleTooLarge => {
                "invalid_input"
            }
            GitError::DirtyWorktree
            | GitError::IncompleteRepository
            | GitError::OidMismatch
            | GitError::NonFastForward
            | GitError::BundleHeadMismatch
            | GitError::TransportConflict
            | GitError::RemoteMoved => "conflict",
            _ => "io",
        };
    }
    error
        .downcast_ref::<provenant_fabric_store::Error>()
        .map_or("io", store_error_code)
}

#[allow(clippy::too_many_arguments)]
fn landing_local(
    context: &Context,
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
    let git = provenant_fabric_runtime::git_transport::GitTransport::discover_with_policy(
        executor,
        std::env::var_os("SSH_AUTH_SOCK").map(Into::into),
        context.config.ssh_executable.as_deref(),
        context.config.limits.bundle_bytes,
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
            if ctx.in_lane {
                return Err(CoordinationError::LaneMutation.into());
            }
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
                let executable = provenant_fabric_runtime::hosts::discover_ssh_executable(
                    ctx.config.ssh_executable.as_deref(),
                )?;
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
                    executable: executable.clone(),
                    seconds: ctx.config.limits.peer_seconds,
                };
                return match transport.exchange(peer, &request, budget, &cancel)?.result {
                    PeerResult::Push(value) if value.id == recovery_id => Ok(value),
                    PeerResult::Error { code, operation_id } => {
                        if operation_id.as_ref().is_some_and(|id| id != &recovery_id) {
                            return Err(PeerError::Frame.into());
                        }
                        if code == "launch_unknown" {
                            Err(PeerError::Unknown {
                                operation_id: recovery_id.clone(),
                            }
                            .into())
                        } else {
                            Err(PeerError::Refused { code }.into())
                        }
                    }
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
            let human = provenant_cli::terse(&serde_json::to_value(&data).unwrap_or_default());
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
            if matches!(
                error.downcast_ref::<CoordinationError>(),
                Some(CoordinationError::LaneMutation)
            ) {
                "lane_coordination_refused"
            } else if matches!(error.downcast_ref::<PeerError>(), Some(PeerError::Frame)) {
                "state_corrupt"
            } else {
                "landing_refused"
            },
            &error.to_string(),
            globals,
        ),
    }
}

/// Git variables that would redirect a fetch away from the peer's clone or
/// let it prompt.
pub(crate) const GIT_REDIRECTS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_TERMINAL_PROMPT",
];
/// Fetch every remote with this host's own Git authority, then require `base`
/// on a remote-tracking ref, so a peer writer never starts from a commit only
/// this clone has. Returns the pinned base commit.
fn fetch_remote_base(
    project: &Path,
    base: &str,
    timeout: std::time::Duration,
) -> Result<provenant_core::GitOid, FacadeError> {
    remote_base(project, base, timeout, true)
}
/// [`fetch_remote_base`], skipping the fetch when `fetch` is false so a caller
/// pinning several bases fetches once.
pub(crate) fn remote_base(
    project: &Path,
    base: &str,
    timeout: std::time::Duration,
    fetch: bool,
) -> Result<provenant_core::GitOid, FacadeError> {
    use provenant_supervisor::exec::{ExecOptions, Executor, StreamPolicy, Termination};
    let git = crate::roots::git_binary().ok_or(FacadeError::OwnerUnavailable)?;
    let environment: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| !key.to_str().is_some_and(|key| GIT_REDIRECTS.contains(&key)))
        .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
        .collect();
    let executor = SystemExecutor::new(1).map_err(|_| FacadeError::Io)?;
    let git_output = |args: &[&str]| -> Result<Vec<u8>, FacadeError> {
        let argv: Vec<std::ffi::OsString> = std::iter::once(git.as_os_str().to_owned())
            .chain(
                [
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                ]
                .iter()
                .chain(args)
                .map(std::ffi::OsString::from),
            )
            .collect();
        let mut options = ExecOptions::new(&argv, project, timeout);
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 4096 };
        options.stderr = StreamPolicy::Discard;
        let output = executor
            .execute(options)
            .map_err(|_| FacadeError::BaseUnreachable)?;
        if output.termination != Termination::Exit(0) || output.timed_out {
            return Err(FacadeError::BaseUnreachable);
        }
        Ok(output.stdout.bytes)
    };
    if fetch {
        git_output(&["fetch", "--all", "--no-tags", "--no-recurse-submodules"])?;
    }
    let contains = format!("--contains={base}");
    let refs = git_output(&[
        "for-each-ref",
        "--count=1",
        "--format=%(refname)",
        &contains,
        "refs/remotes",
    ])?;
    if refs.iter().all(u8::is_ascii_whitespace) {
        return Err(FacadeError::BaseUnreachable);
    }
    let commit = format!("{base}^{{commit}}");
    let oid = git_output(&["rev-parse", "--verify", "--end-of-options", &commit])?;
    std::str::from_utf8(&oid)
        .ok()
        .and_then(|oid| provenant_core::GitOid::parse(oid.trim()).ok())
        .ok_or(FacadeError::BaseUnreachable)
}

struct BinaryRunOwner<'a> {
    factory: &'a Factory,
    /// False only at peer ingress: no forwarding, and writers name a new
    /// worktree instead of a path.
    allow_remote: bool,
    /// Replay submissions under this actor instead of the process's own.
    actor: Option<provenant_core::ActorId>,
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
        RunError::DepthExceeded { .. } | RunError::DepthOverflow => FacadeError::LaneDepth,
        RunError::Writer(_) | RunError::Confinement(_) => FacadeError::ConfinementRefused,
        RunError::WorktreeMissing(_) => FacadeError::WorktreeMissing,
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
        "LANE_DEPTH" => FacadeError::LaneDepth,
        "LANE_CONTEXT" | "PARENT_ATTEMPT" => FacadeError::InvalidInput,
        "INVALID" | "WORKTREE_REQUIRED" | "WORKTREE_NOT_APPLICABLE" => FacadeError::InvalidInput,
        "WORKTREE_MISSING" => FacadeError::WorktreeMissing,
        "NOT_FOUND" => FacadeError::NotFound,
        "CONFLICT" | "OPERATION_CONFLICT" | "NONTERMINAL" => FacadeError::Conflict,
        "CANCELLED" => FacadeError::Cancelled,
        "QUEUE_FULL" | "CONTENTION" => FacadeError::Busy,
        "UNSUPPORTED" => FacadeError::ContinuationUnsupported,
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
            state_root: provenant_cli::run::execution_state(
                &ctx.roots,
                &self.factory.home,
                &crate::roots::product_root().map_err(|_| FacadeError::OwnerUnavailable)?,
            )
            .map_err(fail_error)?,
            project_id: ctx.identity.project,
            actor_id: self.actor.unwrap_or(ctx.identity.inbox.actor),
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
                    newest_first: false,
                    owner: None,
                    unseen_after: None,
                },
                call,
            )
            .map_err(run_error)?;
        Ok(SubmissionReceipt {
            worktree: None,
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
            let attempt_id = target.attempt;
            return match self.peer(
                host,
                PeerAction::Run {
                    input: RunInput::Status { target },
                },
                budget,
                cancel,
            )? {
                PeerResult::Run(value) => match *value {
                    RunResult::Status { attempt } if attempt.attempt_id == attempt_id => {
                        Ok(attempt)
                    }
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
    fn snapshots_local(
        &self,
        targets: &[AttemptTarget],
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::AttemptRecord>, FacadeError> {
        let host = self.factory.context.identity.host;
        if targets
            .iter()
            .any(|target| target.host.is_some_and(|target_host| target_host != host))
        {
            return Err(FacadeError::InvalidInput);
        }
        let session = self.session(budget, cancel)?;
        let call = self.call(budget, cancel);
        targets
            .iter()
            .map(|target| {
                session
                    .store
                    .record(target.attempt, &call)
                    .map_err(run_error)
            })
            .collect()
    }
    /// This host's lanes for `PeerAction::Lanes`: active ones oldest first,
    /// or with `all` the newest retained ones.
    fn lanes_local(
        &self,
        all: bool,
        limit: u16,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::AttemptRecord>, FacadeError> {
        if !(1..=100).contains(&limit) {
            return Err(FacadeError::InvalidInput);
        }
        let store = self.store(budget, cancel)?;
        let page = store
            .lanes(
                &provenant_run::LaneQuery {
                    targets: Vec::new(),
                    all,
                    limit: u64::from(limit),
                    after: 0,
                    newest_first: all,
                    owner: None,
                    unseen_after: None,
                },
                &self.call(budget, cancel),
            )
            .map_err(run_error)?;
        Ok(page.lanes)
    }
    fn peer(
        &self,
        host: provenant_core::HostId,
        action: PeerAction,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<PeerResult, FacadeError> {
        self.factory
            .context
            .peer_with(
                &self.factory.executor,
                &self.factory.clock,
                host,
                action,
                budget,
                cancel,
            )
            .map_err(|failure| failure.error)
    }
}
impl BinaryRunOwner<'_> {
    fn capabilities(&self) -> RunCapabilities {
        RunCapabilities {
            submit: true,
            list: true,
            status: true,
            wait: true,
            cancel: true,
            resume: provenant_cli::run::Registry::default().continues_sessions(),
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
            host,
        } = input;
        // Explicit placement forwards the whole batch; the peer confines it.
        if let Some(host) = host.filter(|h| *h != self.factory.context.identity.host) {
            if !self.allow_remote {
                return Err(FacadeError::ConfinementRefused);
            }
            // MCP forwards read-only work only; a peer writer comes from `dispatch`.
            if tasks
                .iter()
                .any(|t| t.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly)
            {
                return Err(FacadeError::OwnerUnavailable);
            }
            let input = SubmitInput::Submission {
                operation_id: operation_id.clone(),
                tasks,
                concurrency,
                knobs,
                host: None,
            };
            return match self.peer(host, PeerAction::Submit { input }, budget, cancel)? {
                PeerResult::Submitted(receipt)
                    if receipt.operation_id == operation_id
                        && receipt.worktree.is_none()
                        && !receipt.targets.is_empty()
                        && receipt.targets.iter().all(|t| t.host == Some(host)) =>
                {
                    Ok(receipt)
                }
                _ => Err(FacadeError::StateCorrupt),
            };
        }
        // Peer ingress creates a writer's worktree in this host's clone; a
        // caller never names a path here, and a batch creates at most one.
        let ingress = !self.allow_remote;
        let mut tasks = tasks;
        let mut created = None;
        for task in &mut tasks {
            let Some(spec) = task.new_worktree.take() else {
                if ingress && task.worktree.is_some() {
                    return Err(FacadeError::InvalidInput);
                }
                continue;
            };
            if !ingress
                || created.is_some()
                || task.worktree.is_some()
                || task.access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            {
                return Err(FacadeError::InvalidInput);
            }
            let worktree = self.peer_worktree(&operation_id, spec, budget, cancel)?;
            task.worktree = Some(worktree.path.clone());
            created = Some(worktree);
        }
        let mut submitted = Vec::new();
        for (index, task) in tasks.into_iter().enumerate() {
            worktree_access(
                task.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite,
                task.worktree.as_deref().map(Path::new),
            )
            .map_err(fail_error)?;
            let route = &task.route;
            let model = match (&route.adapter, &route.model) {
                (_, Some(model)) if model.contains('/') => Some(model.clone()),
                (Some(adapter), Some(model)) => Some(format!("{adapter}/{model}")),
                (None, Some(_)) => return Err(FacadeError::InvalidInput),
                (_, None) => None,
            };
            let name = match model {
                Some(_) => None,
                None => route
                    .route
                    .as_deref()
                    .or(route.role.as_deref())
                    .or(route.task_class.as_deref()),
            };
            submitted.push(Submitted {
                task_class: route.task_class.clone(),
                task_id: derived_task(&operation_id, index).map_err(fail_error)?,
                prompt: task.prompt,
                account: task.account,
                route: requested(model.as_deref(), name, route.effort.clone())
                    .map_err(fail_error)?,
                fallback: Vec::new(),
                provider_timeout: task.timeout_ms.div_ceil(1000).max(1),
                delivery_id: task.delivery_id,
                deliver: task.deliver,
                worktree: task.worktree.map(PathBuf::from),
                // Fabric lanes take the mode defaults; overrides are CLI-only.
                access: provenant_records::run::LaneAccess::default(),
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
        let mut receipt = self.receipt(&session, operation_id, &run, &started, &call)?;
        receipt.worktree = created;
        Ok(receipt)
    }
    /// The writer worktree a peer request names. A first request fetches every
    /// remote, requires the base on a remote-tracking ref and creates the
    /// branch at it. A retry adopts the branch's worktree while it is still
    /// clean at that base; a replayed operation finds the one it created.
    fn peer_worktree(
        &self,
        operation_id: &provenant_core::OperationId,
        spec: NewWorktree,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::runs::PeerWorktree, FacadeError> {
        use provenant_confinement::worktrees::{
            self, WorktreeCondition, WorktreeCreate, WorktreeError, WriterState,
        };
        let branch = provenant_confinement::repo::GitRefName::branch(&spec.branch)
            .map_err(|_| FacadeError::InvalidInput)?;
        provenant_confinement::repo::GitRevision::new(&spec.base)
            .map_err(|_| FacadeError::InvalidInput)?;
        let replay = self
            .store(budget, cancel)?
            .operation(operation_id, &self.call(budget, cancel))
            .map_err(run_error)?
            .is_some();
        // Fetch and create each take at most half of what remains, so the
        // submission keeps a reserve; a retry adopts a create that ran late.
        let half = || {
            let micros = budget.remaining(self.factory.clock.mono_now()).as_micros() / 2;
            std::time::Duration::from_micros(micros)
        };
        let project = &self.factory.cwd;
        let base = if replay {
            None
        } else {
            Some(fetch_remote_base(project, &spec.base, half())?)
        };
        let info = crate::worktrees::with_context(
            &self.factory.context.roots,
            project,
            half(),
            |context, cancel| {
                let existing = worktrees::list(context, cancel)?
                    .into_iter()
                    .find(|w| w.branch.as_deref() == Some(branch.as_str()));
                let Some(base) = base else {
                    return existing.ok_or(WorktreeError::NotFound.into());
                };
                match existing {
                    Some(info)
                        if info.head == base
                            && info.dirty == WorktreeCondition::Clean
                            && info.writer != WriterState::Live =>
                    {
                        Ok(info)
                    }
                    Some(_) => Err(WorktreeError::InvalidRequest.into()),
                    None => {
                        let request = WorktreeCreate {
                            branch: spec.branch.clone(),
                            base: Some(base.to_string()),
                        };
                        Ok(worktrees::create(context, request, cancel)?)
                    }
                }
            },
        )
        .map_err(|error| match error {
            // The branch name is valid, so an invalid create means it exists.
            crate::worktrees::Error::Worktree(WorktreeError::InvalidRequest) if !replay => {
                FacadeError::BranchExists
            }
            crate::worktrees::Error::Worktree(WorktreeError::NotFound) if replay => {
                FacadeError::WorktreeMissing
            }
            crate::worktrees::Error::Worktree(WorktreeError::NotFound) => {
                FacadeError::BaseUnreachable
            }
            crate::worktrees::Error::Worktree(WorktreeError::Cancelled) => FacadeError::Cancelled,
            _ => FacadeError::Io,
        })?;
        Ok(provenant_fabric_runtime::runs::PeerWorktree {
            path: info.path.to_str().ok_or(FacadeError::Io)?.to_owned(),
            branch: spec.branch,
        })
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
                if !(1..=100).contains(&targets.len()) {
                    return Err(FacadeError::InvalidInput);
                }
                let local_host = self.factory.context.identity.host;
                let mut local_indices = Vec::new();
                let mut remote_batches = std::collections::BTreeMap::new();
                for (index, target) in targets.iter().enumerate() {
                    match target.host {
                        Some(host) if host != local_host => {
                            remote_batches
                                .entry(host)
                                .or_insert_with(Vec::new)
                                .push((index, target.clone()));
                        }
                        _ => local_indices.push(index),
                    }
                }
                if !self.allow_remote && !remote_batches.is_empty() {
                    return Err(FacadeError::ConfinementRefused);
                }
                let local_session = if local_indices.is_empty() {
                    None
                } else {
                    Some(self.session(budget, cancel)?)
                };
                let local_call = self.call(budget, cancel);
                let mut last_attempts = None;
                let mut backoff_micros = 50_000_u64;
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
                    let mut attempts: Vec<Option<provenant_records::run::AttemptRecord>> =
                        std::iter::repeat_with(|| None)
                            .take(targets.len())
                            .collect();
                    if let Some(session) = &local_session {
                        for index in &local_indices {
                            attempts[*index] = Some(
                                session
                                    .store
                                    .record(targets[*index].attempt, &local_call)
                                    .map_err(run_error)?,
                            );
                        }
                    }
                    for (host, batch) in &remote_batches {
                        let requested = batch
                            .iter()
                            .map(|(_, target)| target.clone())
                            .collect::<Vec<_>>();
                        let result = match self.peer(
                            *host,
                            PeerAction::RunStatuses { targets: requested },
                            budget,
                            cancel,
                        ) {
                            Ok(result) => result,
                            // A poll cut short by the wait's own deadline is a
                            // timed-out wait, not an unavailable peer.
                            Err(error) => {
                                let expired =
                                    budget.remaining(self.factory.clock.mono_now()).as_micros()
                                        == 0;
                                return match last_attempts {
                                    Some(attempts) if expired => Ok(RunResult::Wait {
                                        condition,
                                        attempts,
                                        satisfied: false,
                                        timed_out: true,
                                    }),
                                    _ => Err(error),
                                };
                            }
                        };
                        let PeerResult::RunStatuses(records) = result else {
                            return Err(FacadeError::StateCorrupt);
                        };
                        if records.len() != batch.len() {
                            return Err(FacadeError::StateCorrupt);
                        }
                        for ((index, target), record) in batch.iter().zip(records) {
                            if record.attempt_id != target.attempt {
                                return Err(FacadeError::StateCorrupt);
                            }
                            attempts[*index] = Some(record);
                        }
                    }
                    let attempts = attempts
                        .into_iter()
                        .collect::<Option<Vec<_>>>()
                        .ok_or(FacadeError::StateCorrupt)?;
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
                            backoff_micros
                                .min(budget.remaining(self.factory.clock.mono_now()).as_micros()),
                        ),
                        cancel,
                    );
                    backoff_micros = backoff_micros.saturating_mul(2).min(500_000);
                }
            }
            RunInput::Cancel {
                ref target,
                ref operation_id,
            } => {
                if self.factory.context.in_lane
                    && target
                        .host
                        .is_some_and(|host| host != self.factory.context.identity.host)
                {
                    return Err(FacadeError::LaneCoordinationRefused);
                }
                if let Some(answer) = self.remote(
                    target,
                    PeerAction::Run {
                        input: input.clone(),
                    },
                    budget,
                    cancel,
                )? {
                    return match answer {
                        PeerResult::Run(value)
                            if matches!(&*value, RunResult::Cancel { attempt }
                                if attempt.attempt_id == target.attempt) =>
                        {
                            Ok(*value)
                        }
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
                if self.factory.context.in_lane
                    && target
                        .host
                        .is_some_and(|host| host != self.factory.context.identity.host)
                {
                    return Err(FacadeError::LaneCoordinationRefused);
                }
                if let Some(answer) = self.remote(
                    target,
                    PeerAction::Run {
                        input: input.clone(),
                    },
                    budget,
                    cancel,
                )? {
                    return match answer {
                        PeerResult::Run(value)
                            if matches!(&*value, RunResult::Resume { attempt }
                                if attempt.operation_id == *operation_id) =>
                        {
                            Ok(*value)
                        }
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
            RunInput::List { after, limit } => {
                if !(1..=100).contains(&limit) {
                    return Err(FacadeError::InvalidInput);
                }
                let host = self.factory.context.identity.host;
                if after
                    .as_ref()
                    .and_then(|target| target.host)
                    .is_some_and(|cursor_host| cursor_host != host)
                {
                    return Err(FacadeError::OwnerUnavailable);
                }
                let store = self.store(budget, cancel)?;
                let call = self.call(budget, cancel);
                let after_sequence = after
                    .as_ref()
                    .map(|target| {
                        store
                            .attempt_sequence(target.attempt, &call)
                            .map_err(run_error)
                    })
                    .transpose()?
                    .unwrap_or(0);
                let page = store
                    .lanes(
                        &provenant_run::LaneQuery {
                            targets: Vec::new(),
                            all: true,
                            limit: u64::from(limit),
                            after: after_sequence,
                            newest_first: false,
                            owner: None,
                            unseen_after: None,
                        },
                        &call,
                    )
                    .map_err(run_error)?;
                let truncated = page.next.is_some();
                let next_after = if truncated {
                    page.lanes.last().map(|lane| AttemptTarget {
                        attempt: lane.attempt_id,
                        host: Some(host),
                    })
                } else {
                    None
                };
                Ok(RunResult::List {
                    page: AttemptPage {
                        items: page.lanes,
                        next_after,
                        truncated,
                    },
                })
            }
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
        let store = self.store(budget, cancel)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            budget,
            cancel,
        };
        let requested = usize::from(limit).saturating_add(1).min(64);
        let mut items = store
            .events(after, requested, None, &call)
            .map_err(run_error)?;
        let truncated = if limit < 64 {
            items.len() > usize::from(limit)
        } else if items.len() == 64 {
            let cursor = items.last().map_or(after, |event| event.sequence);
            !store
                .events(cursor, 1, None, &call)
                .map_err(run_error)?
                .is_empty()
        } else {
            false
        };
        items.truncate(usize::from(limit));
        let cursor = items.last().map_or(after, |event| event.sequence);
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
    fn actor(&self) -> String {
        self.factory.context.identity.inbox.actor.to_string()
    }
    fn observe_or_start(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        prompt: &str,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
        use provenant_fabric_runtime::graph::{AttemptObservation, AttemptOutcome};
        let mut owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
        };
        let (target, reference) = if let Some(reference) = replay.stored_ref {
            // An accepted attempt is observed by reference: operation replay is
            // actor-scoped, so resubmitting under another actor would launch again.
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            (target, Some(reference.to_owned()))
        } else {
            let operation_id = graph_operation("graph-attempt-operation", key)?;
            let input = SubmitInput::Submission {
                operation_id,
                tasks: vec![SubmittedTask {
                    account: None,
                    access,
                    worktree: worktree.map(str::to_owned),
                    prompt: prompt.into(),
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
                    deliver: None,
                    new_worktree: None,
                }],
                concurrency: Some(1),
                knobs: vec![],
                host: None,
            };
            // Operation replay is actor-scoped: submit under the recorded launch actor.
            owner.actor = Some(
                provenant_core::ActorId::parse(replay.launch_actor)
                    .map_err(|_| FacadeError::StateCorrupt)?,
            );
            let receipt = owner.submit(input, self.budget, self.cancel)?;
            owner.actor = None;
            let target = receipt
                .targets
                .into_iter()
                .next()
                .ok_or(FacadeError::StateCorrupt)?;
            let reference = Some(format!(
                "{}/{}",
                target.host.unwrap_or(self.factory.context.identity.host),
                target.attempt
            ));
            (target, reference)
        };
        let attempt = owner.snapshot(target, self.budget, self.cancel)?;
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
        use provenant_fabric_runtime::graph::{FileJournal, GraphDocument, GraphJournal};
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
        if matches!(input, GraphInput::Status { .. }) {
            let checkpoint = FileJournal::read_checkpoint(
                &self.context.roots.state,
                self.context.identity.project,
                id,
                &provenant_supervisor::fs::LocalFs,
            )
            .map_err(|error| {
                use provenant_fabric_runtime::graph::JournalError;
                match error {
                    // Unreadable is not corrupt: a denied or vanished read stays Io.
                    JournalError::Filesystem(_)
                    | JournalError::Io(_)
                    | JournalError::DurabilityUnconfirmed(_) => FacadeError::Io,
                    _ => FacadeError::StateCorrupt,
                }
            })?
            .ok_or(FacadeError::NotFound)?;
            return Ok(graph_snapshot(checkpoint));
        }
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
                actor: None,
            };
            // One unresolvable target must not strand the others; report the first failure last.
            let mut first_error = None;
            for node in &checkpoint.nodes {
                for attempt in &node.attempts {
                    if attempt.state == provenant_fabric_runtime::graph::AttemptState::Terminal {
                        continue;
                    }
                    if let Err(error) =
                        self.cancel_graph_attempt(&mut owner, attempt, budget, cancel)
                    {
                        first_error.get_or_insert(error);
                    }
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        Ok(graph_snapshot(checkpoint))
    }
}

fn graph_snapshot(
    checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
) -> provenant_fabric_runtime::graph::GraphRun {
    use provenant_fabric_runtime::graph::{GraphRun, GraphStatus, NodeState};
    let status = if checkpoint.cancelled {
        GraphStatus::Cancelled
    } else if checkpoint
        .nodes
        .iter()
        .any(|node| matches!(node.state, NodeState::Pending | NodeState::Running))
    {
        GraphStatus::Pending
    } else {
        GraphStatus::Complete
    };
    GraphRun { status, checkpoint }
}

impl Factory {
    /// Cancel one live graph attempt. A missing checkpoint reference (a crash
    /// between acceptance and the save) is recovered from the stable operation ID.
    fn cancel_graph_attempt(
        &self,
        owner: &mut BinaryRunOwner<'_>,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<(), FacadeError> {
        let operation_id = graph_operation("graph-cancel", &attempt.key)?;
        let targets = match attempt.attempt_ref.as_deref() {
            Some(reference) => {
                let (host, attempt_id) =
                    reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                vec![AttemptTarget {
                    host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                    attempt: attempt_id.parse().map_err(|_| FacadeError::StateCorrupt)?,
                }]
            }
            None => {
                let accepted = graph_operation("graph-attempt-operation", &attempt.key)?;
                let store = owner.store(budget, cancel)?;
                let call = owner.call(budget, cancel);
                // An operation this actor never accepted launched nothing to stop.
                match store.operation(&accepted, &call).map_err(run_error)? {
                    Some(record) => store
                        .pending_attempts(record.run_id, &call)
                        .map_err(run_error)?
                        .into_iter()
                        .map(|record| AttemptTarget {
                            host: Some(self.context.identity.host),
                            attempt: record.attempt_id,
                        })
                        .collect(),
                    None => vec![],
                }
            }
        };
        for target in targets {
            owner.run(
                RunInput::Cancel {
                    target,
                    operation_id: operation_id.clone(),
                },
                budget,
                cancel,
            )?;
        }
        Ok(())
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
        if factory.context.in_lane && !matches!(input, GraphInput::Status { .. }) {
            return Err(CoordinationError::LaneMutation.into());
        }
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
        Ok(factory.graph(input, budget, &cancel)?)
    })();
    match result {
        Ok(data) => {
            let human = provenant_cli::terse(&serde_json::to_value(&data).unwrap_or_default());
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
        Err(error) => {
            let (outcome, code) = if matches!(
                error.downcast_ref::<CoordinationError>(),
                Some(CoordinationError::LaneMutation)
            ) {
                (
                    provenant_records::Outcome::Refused,
                    "lane_coordination_refused",
                )
            } else if matches!(
                error.downcast_ref::<FacadeError>(),
                Some(FacadeError::NotFound)
            ) {
                (provenant_records::Outcome::Invalid, "not_found")
            } else {
                (provenant_records::Outcome::Error, "graph_owner")
            };
            crate::fail(command, outcome, code, &error.to_string(), globals)
        }
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
        if ctx.in_lane {
            return Ok(EventCoverage {
                unknown_hosts: std::iter::once(ctx.identity.host)
                    .chain(ctx.config.peers.values().map(|peer| peer.host))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                more_events: false,
            });
        }
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
            .map_err(|error| FacadeError::from_code(store_error_code(&error), None))?;
        let mut owner = BinaryRunOwner {
            factory: self,
            allow_remote: true,
            actor: None,
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
                Err(error) if host == ctx.identity.host => return Err(error),
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
            let human = provenant_cli::terse(&serde_json::to_value(&data).unwrap_or_default());
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
        Err(error) => {
            let code = error
                .downcast_ref::<CoordinationError>()
                .map(CoordinationError::code)
                .or_else(|| error.downcast_ref::<FacadeError>().map(FacadeError::code));
            crate::fail(
                "fabric events read",
                code.map(provenant_cli::fabric::coordination_outcome)
                    .unwrap_or(provenant_records::Outcome::Error),
                code.unwrap_or("events_read"),
                &error.to_string(),
                globals,
            )
        }
    }
}
#[cfg(test)]
mod peer_tests {
    #[test]
    fn peer_refusal_codes_keep_their_facade_meaning() {
        for code in [
            "stale_fence",
            "timeout",
            "state_corrupt",
            "upgrade_refused",
            "peer_version",
            "publication_failed",
            "home_unavailable",
        ] {
            assert_eq!(
                super::FacadeError::from_code(code, None).code(),
                code,
                "{code}"
            );
        }
    }

    #[test]
    fn host_probe_budget_is_shared_fairly() {
        assert_eq!(super::fair_probe_micros(10_000_000, 4), Some(2_500_000));
        assert_eq!(super::fair_probe_micros(55_000_000, 100), Some(550_000));
    }

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
            super::FacadeError::from_code(&code, operation_id),
            super::FacadeError::LaunchUnknown { operation_id } if operation_id.as_str() == "stop"
        ));
        Ok(())
    }
}
