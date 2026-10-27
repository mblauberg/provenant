//! Fabric process composition: MCP, fixed peer ingress and host observations.
use provenant_cli::fabric::Context;
use provenant_confinement::worktrees::WorkerObserver;
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
use provenant_os::{clock::SystemClock, exec::SystemExecutor};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::mpsc,
    time::Instant,
};

#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error(transparent)]
    Exec(#[from] provenant_os::exec::ExecError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] provenant_os::fs::FsError),
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
    project_attribution: bool,
    clock: SystemClock,
    backoff: ThreadWait,
    executor: SystemExecutor,
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    assets: PathBuf,
    host_launch: Option<provenant_run::HostLaunchContext>,
    workers: &'static dyn WorkerObserver,
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
        provenant_telemetry::usage::maintain_tools(&self.context.roots);
    }
    fn record_usage(&self, tool: &str, outcome: &str, duration_ms: u64) {
        let project_id = self
            .project_attribution
            .then_some(self.context.identity.project);
        provenant_telemetry::usage::record_tool(
            &self.context.roots,
            &self.clock,
            tool,
            outcome,
            duration_ms,
            project_id,
        );
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
        let coordination = self.coordination().map_err(|error| {
            let typed = FacadeError::from(&error);
            if matches!(typed, FacadeError::Io { .. }) {
                ServiceError::Facade(scope_io(typed, "open coordination store"))
            } else {
                ServiceError::Coordination(error)
            }
        })?;
        Ok(
            Service::new(coordination, BoundFacade { factory: self }, &self.clock)
                .with_limits(self.context.config.limits.clone())
                .with_lane_restrictions(self.context.in_lane),
        )
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
        let HostsInput::Hosts { after, limit } = input;
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
                    protocol: Some(PROTOCOL),
                    reason: None,
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
                protocol: None,
                reason: None,
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
                let request = PeerRequest::new(
                    host,
                    ctx.identity.host,
                    ctx.identity.inbox.agent.clone(),
                    project,
                    PeerAction::Health,
                );
                let transport = SshTransport {
                    executor: &self.factory.executor,
                    clock: &self.factory.clock,
                    home: &self.factory.home,
                    cwd: &self.factory.cwd,
                    executable,
                    seconds: ctx.config.limits.peer_seconds,
                };
                match transport.exchange_with_cap(
                    peer,
                    &request,
                    budget,
                    cancel,
                    std::time::Duration::from_micros(cap),
                ) {
                    Ok(PeerResponse {
                        result: PeerResult::Health(value),
                        ..
                    }) => {
                        health.protocol = Some(PROTOCOL);
                        health.reason = None;
                        health.host = value.host;
                        health.available = value.available;
                        health.execution = value.execution;
                    }
                    Err(PeerError::ProtocolMismatch { remote, .. }) => {
                        health.protocol = Some(remote);
                        health.reason = Some("peer_version".into());
                    }
                    _ => {}
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

/// Retain lower-level diagnostics while naming the request that failed.
fn scope_io(error: FacadeError, scope: &str) -> FacadeError {
    match error {
        FacadeError::Io { operation, path } => {
            let prefix = format!("{scope}: ");
            let operation = if operation.starts_with(&prefix) {
                operation
            } else {
                format!("{scope}: {operation}")
            };
            FacadeError::Io { operation, path }
        }
        other => other,
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
    // The stdin bridge waits against the transport's real-time deadline.
    #[allow(clippy::disallowed_methods)]
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
        provenant_os::sys::hook_io::write_stdout(bytes, deadline)
    }
}

pub fn mcp(
    client: Option<provenant_records::Harness>,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut factory = factory(globals, workers)?;
        let context = &mut factory.context;
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

fn factory(
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Result<Factory, Box<dyn std::error::Error>> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let context = Context::open(resolved.roots, &resolved.home, &resolved.project)?;
    Ok(Factory {
        project_attribution: crate::usage::attribution_enabled(&context.roots),
        context,
        clock: SystemClock::new(),
        backoff: ThreadWait,
        executor: SystemExecutor::new(4)?,
        home: resolved.home,
        cwd: resolved.project,
        assets: resolved.assets,
        host_launch: None,
        workers,
    })
}
pub fn hosts(globals: &provenant_cli::Globals, workers: &'static dyn WorkerObserver) -> u8 {
    let result = (|| -> Result<Vec<HostHealth>, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        Ok(BoundFacade { factory: &factory }.hosts(
            HostsInput::Hosts {
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
// Peer frames use bounded real-time stdin and stdout transport deadlines.
#[allow(clippy::disallowed_methods)]
pub fn peer(
    globals: &provenant_cli::Globals,
    in_process: bool,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // A request names its own agent, so a lane could act as any actor.
        if provenant_cli::fabric::in_lane() {
            return Err(CoordinationError::LaneMutation.into());
        }
        let bytes = provenant_os::sys::hook_io::read_stdin(
            MAX_FRAME,
            Instant::now() + std::time::Duration::from_secs(30),
        )?;
        if let Some(refusal) = protocol_refusal(&bytes)? {
            let mut reply = serde_json::to_vec(&refusal)?;
            reply.push(b'\n');
            provenant_os::sys::hook_io::write_stdout(
                &reply,
                Instant::now() + std::time::Duration::from_secs(30),
            )?;
            return Ok(());
        }
        let host = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Home,
            crate::roots::Product::Required,
        )?;
        let home = host.home;
        if !in_process {
            let state = host.roots.state;
            // Only an unreachable agent falls back; a sent request is never repeated.
            let reply =
                match provenant_fabric_runtime::peer_agent::forward(state.as_path(), &bytes)? {
                    provenant_fabric_runtime::peer_agent::Forward::Replied(reply) => Some(reply),
                    // Refused unread at the agent's cap: nothing ran, so say busy.
                    provenant_fabric_runtime::peer_agent::Forward::Busy => {
                        let request = decode_request(&bytes)?;
                        let mut busy = serde_json::to_vec(&PeerResponse {
                            version: 1,
                            protocol: PROTOCOL,
                            host: request.target_host,
                            project: request.project.key,
                            result: PeerResult::Error {
                                error: FacadeError::Busy,
                            },
                        })?;
                        busy.push(b'\n');
                        Some(busy)
                    }
                    provenant_fabric_runtime::peer_agent::Forward::Unreachable => None,
                };
            if let Some(reply) = reply {
                provenant_os::sys::hook_io::write_stdout(
                    &reply,
                    Instant::now() + std::time::Duration::from_secs(30),
                )?;
                return Ok(());
            }
        }
        let request = decode_request(&bytes)?;
        let common = request.project.resolve(&home)?;
        if provenant_os::fs::canonicalize(&common)? != common {
            return Err(PeerError::Project.into());
        }
        let cwd = if common.file_name() == Some(std::ffi::OsStr::new(".git")) {
            common.parent().ok_or(PeerError::Project)?.to_path_buf()
        } else {
            common
        };
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::At(&cwd),
            crate::roots::Product::Required,
        )?;
        let assets = resolved.assets;
        let mut context = Context::open(resolved.roots, &home, &cwd)?;
        request.validate(context.identity.host, &context.config, &home)?;
        if context.identity.federation.as_ref().map(|p| &p.key) != Some(&request.project.key) {
            return Err(PeerError::Project.into());
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let seconds = if let PeerAction::SessionHandoff { budget_seconds, .. } = &request.action {
            provenant_fabric_runtime::handoff::session::peer_seconds(
                *budget_seconds,
                context.config.limits.peer_seconds,
            )
        } else {
            30
        };
        let budget = WaitBudget::cli(clock.mono_now(), seconds)?;
        let backoff = ThreadWait;
        context.identity.inbox =
            provenant_fabric_runtime::identity::Address::new(request.source_host, &request.agent)?;
        let peer_factory = Factory {
            project_attribution: crate::usage::attribution_enabled(&context.roots),
            context,
            clock: SystemClock::new(),
            backoff: ThreadWait,
            executor: SystemExecutor::new(4)?,
            home: home.clone(),
            cwd: cwd.clone(),
            assets,
            host_launch: None,
            workers,
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
                        error: FacadeError::InvalidInput,
                    }
                } else {
                    peer_result(
                        owner.snapshots_local(targets, budget, &cancel),
                        PeerResult::RunStatuses,
                    )
                }
            }
            PeerAction::Run { input } => {
                peer_result(owner.run(input.clone(), budget, &cancel), |value| {
                    PeerResult::Run(Box::new(value))
                })
            }
            PeerAction::Submit { input } => peer_result(
                owner.submit(input.clone(), budget, &cancel),
                PeerResult::Submitted,
            ),
            PeerAction::Output { input } => peer_result(
                owner.output(input.clone(), budget, &cancel),
                PeerResult::Output,
            ),
            PeerAction::RunEvents { after, limit } => peer_result(
                owner.events(*after, *limit, budget, &cancel),
                PeerResult::RunEvents,
            ),
            PeerAction::Lanes { all, limit } => peer_result(
                owner.lanes_local(*all, *limit, budget, &cancel),
                PeerResult::RunStatuses,
            ),
            PeerAction::LaneWaits { targets } => {
                if !(1..=100).contains(&targets.len()) {
                    PeerResult::Error {
                        error: FacadeError::InvalidInput,
                    }
                } else {
                    peer_result(
                        owner.waits_local(targets, budget, &cancel),
                        PeerResult::LaneWaits,
                    )
                }
            }
            PeerAction::SessionHandoff { input, .. } => {
                PeerResult::SessionHandoff(crate::handoff::receive(
                    &crate::handoff::Target {
                        context,
                        home: &home,
                        cwd: &cwd,
                        workers,
                        source: request.source_host,
                        clock: &clock,
                        budget,
                    },
                    input,
                    &provenant_fabric_runtime::handoff::session::Terminal,
                ))
            }
            PeerAction::Health => PeerResult::Health(provenant_fabric_runtime::hosts::PeerHealth {
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
                        error: FacadeError::HomeUnavailable,
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
                        Err(error) => PeerResult::Error {
                            error: landing_error(error.as_ref()),
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
                        error: FacadeError::LaunchUnknown {
                            operation_id: operation_id.clone(),
                        },
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
                        error: FacadeError::HomeUnavailable,
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
                                    error: FacadeError::from(&error),
                                },
                            }
                        }
                        Err(error) => PeerResult::Error { error },
                    }
                }
            }
        };
        let response = PeerResponse {
            version: 1,
            protocol: PROTOCOL,
            host: context.identity.host,
            project: request.project.key,
            result,
        };
        let mut bytes = serde_json::to_vec(&response)?;
        if bytes.len() >= MAX_FRAME {
            return Err(PeerError::Frame.into());
        }
        bytes.push(b'\n');
        provenant_os::sys::hook_io::write_stdout(
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

fn landing_error(error: &(dyn std::error::Error + 'static)) -> FacadeError {
    use provenant_fabric_runtime::git_transport::GitTransportError as E;
    if let Some(error) = error.downcast_ref::<E>() {
        return match error {
            E::PushUnknown { operation } | E::PushUnresolved { operation } => {
                FacadeError::LaunchUnknown {
                    operation_id: operation.clone(),
                }
            }
            E::Store(error) => FacadeError::from(error),
            E::PushNotFound { .. } => FacadeError::NotFound,
            E::Invalid(_) | E::InvalidRemote | E::BundleTooLarge => FacadeError::InvalidInput,
            E::DirtyWorktree
            | E::IncompleteRepository
            | E::OidMismatch
            | E::NonFastForward
            | E::BundleHeadMismatch
            | E::TransportConflict
            | E::RemoteMoved => FacadeError::Conflict,
            _ => FacadeError::io("landing"),
        };
    }
    error
        .downcast_ref::<provenant_fabric_store::Error>()
        .map_or(FacadeError::io("landing"), FacadeError::from)
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
pub fn landing(
    input: provenant_cli::fabric::Landing,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
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
            let factory = factory(globals, workers)?;
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
                let request = PeerRequest::new(
                    *host,
                    ctx.identity.host,
                    ctx.identity.inbox.agent.clone(),
                    ProjectLocator::new(&ctx.identity, &factory.home)?,
                    PeerAction::LandingPush {
                        proof,
                        operation_id: operation.operation_id,
                        reconcile,
                    },
                );
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
                    PeerResult::Error { error } => {
                        if let FacadeError::LaunchUnknown { operation_id } = &error
                            && operation_id != &recovery_id
                        {
                            return Err(PeerError::Frame.into());
                        }
                        if matches!(error, FacadeError::LaunchUnknown { .. }) {
                            Err(PeerError::Unknown {
                                operation_id: recovery_id.clone(),
                            }
                            .into())
                        } else {
                            Err(PeerError::Refused { error }.into())
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
        Err(error)
            if matches!(
                error.downcast_ref::<PeerError>(),
                Some(PeerError::Refused {
                    error: FacadeError::Busy
                })
            ) =>
        {
            let message = error.to_string();
            let mut result = provenant_records::CommandResult::new(
                command,
                provenant_records::Outcome::Refused,
                serde_json::json!({"operation_id":recovery_id}),
            );
            result.error = Some(provenant_records::CommandError {
                code: "QUEUE_FULL".into(),
                message: message.clone(),
                cause: None,
                repair: Some(format!(
                    "repeat {command} with the same fence and operation id"
                )),
            });
            crate::emit(&result, globals, &message, false)
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
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
    let git = provenant_os::git::git_executable().ok_or(FacadeError::OwnerUnavailable)?;
    let environment: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| !key.to_str().is_some_and(|key| GIT_REDIRECTS.contains(&key)))
        .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
        .collect();
    let executor = SystemExecutor::new(1).map_err(|_| FacadeError::io("create Git executor"))?;
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
    wrap: impl FnOnce(T) -> PeerResult,
) -> PeerResult {
    match result {
        Ok(value) => wrap(value),
        Err(error) => PeerResult::Error { error },
    }
}
/// How many leading events fit in a JSON array of at most `max_bytes`.
fn events_within<T: serde::Serialize>(events: &[T], max_bytes: u64) -> Result<usize, FacadeError> {
    let mut size = 2;
    for (index, event) in events.iter().enumerate() {
        let len = serde_json::to_vec(event)
            .map_err(|_| FacadeError::StateCorrupt)?
            .len() as u64;
        size += len + u64::from(index > 0);
        if size > max_bytes {
            return Ok(index);
        }
    }
    Ok(events.len())
}
fn run_error(error: provenant_run::RunError) -> FacadeError {
    fail_error(error.into())
}
fn fail_error(fail: provenant_cli::run::Fail) -> FacadeError {
    fail.detail.unwrap_or_else(|| kind_error(fail.kind))
}
/// Translate a typed run category at the MCP edge. `ErrorKind` is the one
/// categorisation of run errors; this only renders it.
fn kind_error(kind: provenant_run::ErrorKind) -> FacadeError {
    use provenant_run::ErrorKind as E;
    match kind {
        E::SecretInput | E::Refused | E::Unsupported => FacadeError::ConfinementRefused,
        E::LaneDepth => FacadeError::LaneDepth,
        E::LaneContext
        | E::ParentAttempt
        | E::Invalid
        | E::WorktreeRequired
        | E::WorktreeNotApplicable => FacadeError::InvalidInput,
        E::WorktreeMissing => FacadeError::WorktreeMissing,
        E::NotFound => FacadeError::NotFound,
        E::Conflict => FacadeError::Conflict,
        E::OperationConflict => FacadeError::OperationConflict,
        E::Nonterminal => FacadeError::NotTerminal,
        E::Cancelled => FacadeError::Cancelled,
        E::Timeout => FacadeError::Timeout,
        E::QueueFull | E::Contention => FacadeError::Busy,
        E::ContinuationUnsupported => FacadeError::ContinuationUnsupported,
        E::ProviderStateExpired => FacadeError::ProviderStateExpired,
        E::OwnerUnavailable => FacadeError::OwnerUnavailable,
        // An unregistered worktree must arrive with typed detail and its path.
        E::StateCorrupt | E::WorktreeUnregistered => FacadeError::StateCorrupt,
        E::BaseUnreachable => FacadeError::BaseUnreachable,
        E::BranchExists => FacadeError::BranchExists,
        E::LaneCoordinationRefused => FacadeError::LaneCoordinationRefused,
        E::UpgradeRefused => FacadeError::UpgradeRefused,
        E::PeerVersion => FacadeError::PeerVersion,
        _ => FacadeError::io("run operation"),
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
                &self.factory.assets,
            )
            .map_err(fail_error)?,
            project_id: ctx.identity.project,
            actor_id: self.actor.unwrap_or(ctx.identity.inbox.actor),
            read_prompts: true,
        };
        let store = provenant_run::RunStore::open(&context, &self.call(budget, cancel))
            .map_err(run_error)?
            .with_host(ctx.identity.host);
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
        action(&provenant_cli::run::Edge {
            roots: &self.factory.context.roots,
            home: &self.factory.home,
            assets: &self.factory.assets,
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
            .service()
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
        provenant_run::Service::new(&self.store(budget, cancel)?)
            .status(
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
                    .service()
                    .status(target.attempt, &call)
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
        let page = provenant_run::Service::new(&store)
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
    /// The wait of each queued or starting lane among `targets`, for `PeerAction::LaneWaits`.
    fn waits_local(
        &self,
        targets: &[AttemptTarget],
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::LaneWait>, FacadeError> {
        let lanes = self.snapshots_local(targets, budget, cancel)?;
        let store = self.store(budget, cancel)?;
        Ok(store.lane_waits(&lanes, &self.call(budget, cancel)))
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
        self.submit_inner(input, budget, cancel)
            .map_err(|error| scope_io(error, "submit"))
    }
    fn submit_inner(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<SubmissionReceipt, FacadeError> {
        use provenant_cli::run::{Submission, Submitted, derived_task, worktree_access};
        let SubmitInput::Submit {
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
            let input = SubmitInput::Submit {
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
            submitted.push(Submitted {
                base: task.base,
                task_id: derived_task(&operation_id, index).map_err(fail_error)?,
                prompt: task.prompt,
                account: task.account,
                route: task.route,
                fallback: Vec::new(),
                provider_timeout: task.timeout_seconds,
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
            provenant_cli::run::submit_with_host(
                &session,
                edge,
                Submission {
                    operation_id: operation_id.clone(),
                    tasks: submitted,
                    concurrency: concurrency.map(u64::from),
                    knobs,
                },
                &call,
                self.factory.host_launch.as_ref(),
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
            &self.factory.home,
            project,
            half(),
            self.factory.workers,
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
            _ => FacadeError::io("create peer worktree"),
        })?;
        Ok(provenant_fabric_runtime::runs::PeerWorktree {
            path: info
                .path
                .to_str()
                .ok_or(FacadeError::io("create peer worktree"))?
                .to_owned(),
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
            .service()
            .status(target.attempt, &call)
            .map_err(run_error)?;
        let (state, reason) = provenant_cli::run::state_and_reason(&record.state);
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
                if matches!(part, OutputPart::Events) {
                    let mut events = session
                        .service()
                        .events(offset, 64, Some(record.attempt_id), &call)
                        .map_err(run_error)?;
                    let eof = events.len() < 64;
                    // Trim the page, never an event, so a caller can page on.
                    let kept = events_within(&events, max_bytes)?;
                    if kept == 0 && !events.is_empty() {
                        return Err(FacadeError::OutputTooLarge);
                    }
                    let eof = eof && kept == events.len();
                    events.truncate(kept);
                    let next_offset = events.last().map_or(offset, |event| event.sequence);
                    let bytes =
                        serde_json::to_vec(&events).map_err(|_| FacadeError::StateCorrupt)?;
                    if bytes.len() as u64 > max_bytes {
                        return Err(FacadeError::OutputTooLarge);
                    }
                    return Ok(OutputResult::Chunk {
                        target,
                        part,
                        state,
                        reason,
                        offset,
                        next_offset,
                        eof,
                        digest: provenant_core::Digest::from_bytes(
                            *provenant_records::CanonicalV1::file_digest(&bytes).bytes(),
                        ),
                        bytes: OutputBytes::from_bytes(bytes),
                    });
                }
                let artifact = match part {
                    OutputPart::Result => provenant_run::ArtifactPart::Result,
                    OutputPart::Prompt => provenant_run::ArtifactPart::Prompt,
                    OutputPart::Stderr => provenant_run::ArtifactPart::Stderr,
                    OutputPart::Events => return Err(FacadeError::InvalidInput),
                };
                let mut chunk = session
                    .service()
                    .output(
                        record.attempt_id,
                        artifact,
                        provenant_run::ByteRange { offset, max_bytes },
                        &call,
                    )
                    .map_err(run_error)?;
                provenant_cli::run::align_utf8_tail(&mut chunk);
                Ok(OutputResult::Chunk {
                    target,
                    part,
                    state,
                    reason,
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
                        .service()
                        .output(
                            record.attempt_id,
                            provenant_run::ArtifactPart::Result,
                            provenant_run::ByteRange {
                                offset: bytes.len() as u64,
                                max_bytes: provenant_records::run::OUTPUT_LIMIT,
                            },
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
                wait_seconds,
            } => {
                let requested = WaitBudget::new(
                    self.factory.clock.mono_now(),
                    provenant_core::DurationMicros::from_micros(
                        wait_seconds
                            .checked_mul(1_000_000)
                            .ok_or(FacadeError::InvalidInput)?,
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
                                    .service()
                                    .status(targets[*index].attempt, &local_call)
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
                let reason = format!("cancelled by Fabric operation {operation_id}");
                let service = provenant_run::Service::new(&store);
                service
                    .bind_cancel(operation_id, &target.attempt.to_string(), &call)
                    .map_err(run_error)?;
                let receipt = service
                    .cancel(target.attempt, &reason, &call)
                    .map_err(run_error)?;
                // Without a launch summary nothing was asked to stop.
                if let provenant_run::CancelReceipt::Unobservable { .. } = receipt {
                    return Err(FacadeError::LaunchUnknown {
                        operation_id: operation_id.clone(),
                    });
                }
                Ok(RunResult::Cancel {
                    attempt: service.status(target.attempt, &call).map_err(run_error)?,
                })
            }
            RunInput::Resume {
                ref target,
                ref operation_id,
                ref prompt,
                expected_generation,
                no_fallback,
            } => {
                if self.factory.context.in_lane {
                    if target
                        .host
                        .is_some_and(|host| host != self.factory.context.identity.host)
                    {
                        return Err(FacadeError::LaneCoordinationRefused);
                    }
                    return Err(FacadeError::ContinuationUnsupported);
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
                    provenant_cli::run::continue_attempt_mode(
                        &session,
                        edge,
                        target.attempt,
                        operation_id.clone(),
                        prompt.clone(),
                        expected_generation,
                        &call,
                        !no_fallback,
                    )
                })?;
                let receipt =
                    self.receipt(&session, operation_id.clone(), &run, &started, &call)?;
                let attempt = receipt.targets.first().ok_or(FacadeError::StateCorrupt)?;
                Ok(RunResult::Resume {
                    attempt: session
                        .service()
                        .status(attempt.attempt, &call)
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
                let page = provenant_run::Service::new(&store)
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
        let mut items = provenant_run::Service::new(&store)
            .events(after, requested, None, &call)
            .map_err(run_error)?;
        let truncated = if limit < 64 {
            items.len() > usize::from(limit)
        } else if items.len() == 64 {
            let cursor = items.last().map_or(after, |event| event.sequence);
            !provenant_run::Service::new(&store)
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
    integration_base: Option<provenant_core::GitOid>,
    budget: WaitBudget,
    cancel: &'a CancelToken,
}

pub(crate) fn advance_settled_graphs(
    holder: &provenant_cli::run::HostHolder<'_>,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(settled) = holder.settled()? else {
        return Ok(());
    };
    advance_graphs_after_settlement(&settled, workers)
}

fn advance_graphs_after_settlement(
    settled: &provenant_run::HolderContext,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{FileJournal, advance};

    let Some(host) = &settled.launch else {
        return Ok(());
    };
    let roots = crate::roots::child(
        &settled.roots.home,
        &settled.roots.checkout,
        host.configuration_root.clone(),
    )?;
    // A lane's settlement must not stop unrelated graphs from advancing.
    // Settlement records its own retention reason on the attempt.
    let settlement_error = settle_owned_worktree(settled, &roots, workers, None).err();
    let settlement_reason = settlement_error.as_ref().map(|error| error.to_string());
    let mut first_error = settlement_error;
    for id in FileJournal::graph_ids(&roots.state, settled.project_id)? {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let Some(execution) =
                FileJournal::pending_execution(&roots.state, settled.project_id, &id)?
            else {
                return Ok(());
            };
            let context = Context::open(roots.clone(), &settled.roots.home, &execution.directory)?;
            if context.identity.project != settled.project_id {
                return Err(EdgeError::Invalid.into());
            }
            // The settled attempt's launch inputs serve the graph, whatever that attempt was.
            let mut host = host.clone();
            host.cwd = execution.directory.clone();
            let factory = Factory {
                project_attribution: crate::usage::attribution_enabled(&context.roots),
                context,
                clock: SystemClock::new(),
                backoff: ThreadWait,
                executor: SystemExecutor::new(4)?,
                home: settled.roots.home.clone(),
                cwd: execution.directory,
                assets: settled.roots.checkout.clone(),
                host_launch: Some(host),
                workers,
            };
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
            let mut journal = FileJournal::open(
                &roots.state,
                settled.project_id,
                &id,
                budget,
                &cancel,
                &factory.clock,
            )?;
            let mut owner = BoundGraphOwner {
                factory: &factory,
                integration_base: execution.integration_base,
                budget,
                cancel: &cancel,
            };
            advance(&execution.document, &mut owner, &mut journal)?;
            Ok(())
        })();
        // If the attempt store itself refused the retention receipt, retain
        // the error on the graph containing this lane, without affecting
        // the status or advancement of any other graph.
        let settlement_reason = settlement_reason.as_deref().filter(|_| {
            FileJournal::read_checkpoint(&roots.state, settled.project_id, &id)
                .ok()
                .flatten()
                .is_some_and(|checkpoint| {
                    checkpoint.nodes.iter().any(|node| {
                        node.attempts.iter().any(|attempt| {
                            attempt
                                .attempt_ref
                                .as_deref()
                                .and_then(|reference| reference.rsplit_once('/'))
                                .and_then(|(_, attempt)| attempt.parse().ok())
                                == Some(settled.attempt_id)
                        })
                    })
                })
        });
        // The guardian's output is discarded, so `graph status` shows a failure.
        let advance_reason = result.as_ref().err().map(|error| error.to_string());
        let recorded = FileJournal::set_advance_error(
            &roots.state,
            settled.project_id,
            &id,
            advance_reason.as_deref().or(settlement_reason),
        );
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
        if let Err(error) = recorded {
            first_error.get_or_insert(error.into());
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn settle_owned_worktree(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    workers: &dyn WorkerObserver,
    delivery_override: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{self, WorktreeGcPolicy, WorktreeOrigin};
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: settled.project_id,
        actor_id: settled.actor_id,
        read_prompts: true,
    };
    let store =
        provenant_run::RunStore::open(&context, &call)?.with_writer_registry(roots.state.as_path());
    let record = store.record(settled.attempt_id, &call)?;
    if !matches!(record.state, provenant_run::AttemptState::Terminal { .. }) {
        return Ok(());
    }
    let task = store
        .request(record.run_id, &call)?
        .tasks
        .into_iter()
        .find(|task| task.task_id == record.task_id)
        .ok_or("settled task missing")?;
    let Some(writer) = task.writer else {
        return Ok(());
    };
    if let Some(receipt) = replay_worktree_settlement(
        store
            .worktree_settlement(record.attempt_id, &call)?
            .as_ref(),
        &writer.worktree,
    )? {
        record_worktree_settlement(&store, record.attempt_id, &receipt)?;
        return Ok(());
    }
    let pickup = store.delivered(record.attempt_id, &call)?;
    let delivery_request = pickup
        .as_ref()
        .map(|(request, _)| request)
        .or(task.deliver.as_ref());
    let delivery = delivery_override
        .or(pickup.as_ref().map(|(_, receipt)| receipt))
        .or(record.delivery.as_ref());
    if let Some(reason) =
        settlement_retention_reason(&record.state, task.deliver.is_some(), delivery)
    {
        let receipt = serde_json::json!({
            "schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":reason
        });
        record_worktree_settlement(&store, record.attempt_id, &receipt)?;
        return Ok(());
    }
    let mut pruning_intent = None;
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let config = provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)?;
        let settings = config
            .gc
            .projects
            .get(&settled.project_id.to_string())
            .cloned()
            .unwrap_or_default();
        Ok(crate::worktrees::with_context(
            roots,
            &settled.roots.home,
            &writer.project,
            std::time::Duration::from_secs(120),
            workers,
            |context, cancel| {
                let tree = provenant_confinement::verify_worktree(context.repo, &writer.worktree)
                    .map_err(worktrees::WorktreeError::from)?;
                let Some(owner) = worktrees::load_owner(&tree)? else {
                    return Ok(None);
                };
                if owner.origin != WorktreeOrigin::Lane {
                    return Ok(None);
                }
                // Resumed attempts share the original owner's lane. Apply the
                // continuation proof before pruning against that owner's ID.
                if store
                    .check_continuation_writer(record.attempt_id, &call)
                    .map_err(|_| worktrees::WorktreeError::UnknownWriter)?
                    != Some(owner.clone())
                {
                    return Err(worktrees::WorktreeError::UnknownWriter.into());
                }
                let owner_attempt = owner
                    .attempt_id
                    .ok_or(worktrees::WorktreeError::UnknownWriter)?;
                let states = || {
                    crate::gc_support::lane_states(
                        roots,
                        &settled.roots.home,
                        &settled.roots.checkout,
                        settled.project_id,
                    )
                    .ok()
                };
                let terminal = |path: &Path| {
                    states().as_ref().and_then(|snapshot| snapshot.get(path)) == Some(&true)
                };
                let no_live =
                    |path: &Path| states().is_some_and(|snapshot| snapshot.no_live_lanes(path));
                let upstream = delivery
                    .filter(|receipt| {
                        matches!(
                            receipt.state,
                            provenant_run::WriterDeliveryState::Delivered { .. }
                        )
                    })
                    .map(|receipt| {
                        if receipt.remote.is_empty() {
                            format!("refs/heads/{}", receipt.branch)
                        } else {
                            format!("refs/remotes/{}/{}", receipt.remote, receipt.branch)
                        }
                    });
                let upstream = upstream.as_deref().unwrap_or(&owner.base);
                let git = provenant_os::git::git_executable()
                    .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                let pinned = provenant_run::resolve_git_base(&git, &writer.project, upstream)
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                let Some(head) = record.head.or_else(|| {
                    (record.provider_launched == Some(false))
                        .then(|| owner.base.parse().ok())
                        .flatten()
                }) else {
                    return Err(worktrees::WorktreeError::InvalidOutput.into());
                };
                let policy = WorktreeGcPolicy {
                    // A push or open PR has not landed the branch. Only an
                    // integration receipt grants local branch cleanup.
                    upstream: delivery_request
                        .filter(|request| {
                            matches!(
                                request.mode,
                                provenant_records::run::DeliveryMode::Integrate(_)
                            )
                        })
                        .map(|_| pinned),
                    terminal: &terminal,
                    no_live_lanes: &no_live,
                    regenerable: &settings.regenerable,
                    keep: &settings.keep,
                    written_before_ns: i128::MAX,
                    dry_run: false,
                };
                let intent = serde_json::json!({
                    "schema_version":1, "state":"pruning", "worktree":writer.worktree,
                    "branch":format!("refs/heads/{}", owner.branch)
                });
                record_worktree_settlement(&store, record.attempt_id, &intent)
                    .map_err(|_| worktrees::WorktreeError::RegistrationUnconfirmed)?;
                pruning_intent = Some(intent);
                Ok(Some(worktrees::prune_owned(
                    context,
                    &writer.worktree,
                    owner_attempt,
                    head,
                    pinned,
                    &policy,
                    cancel,
                )?))
            },
        )?)
    })();
    let receipt = match result {
        Ok(None) => return Ok(()),
        Ok(Some(result)) => {
            serde_json::json!({"schema_version":1, "state":"pruned", "worktree": writer.worktree, "result":result})
        }
        Err(error) => {
            if let Some(mut pruned) =
                replay_worktree_settlement(pruning_intent.as_ref(), &writer.worktree)?
            {
                pruned["cleanup_warning"] = serde_json::json!(error.to_string());
                pruned
            } else {
                serde_json::json!({"schema_version":1, "state":"retained", "worktree": writer.worktree, "reason":error.to_string()})
            }
        }
    };
    // The Git mutation has its own 120-second bound. Start a fresh storage
    // budget after it so a completed prune can always publish its receipt.
    record_worktree_settlement(&store, record.attempt_id, &receipt)?;
    Ok(())
}

fn record_worktree_settlement(
    store: &provenant_run::RunStore,
    id: provenant_core::AttemptId,
    receipt: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    store.record_worktree_settlement(id, receipt, &call)?;
    Ok(())
}

fn settlement_retention_reason(
    state: &provenant_run::AttemptState,
    delivery_requested: bool,
    delivery: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Option<&'static str> {
    if !matches!(
        state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Succeeded,
            ..
        }
    ) {
        return Some("lane did not succeed; worktree retained for resume");
    }
    if (delivery_requested || delivery.is_some())
        && !delivery.is_some_and(|receipt| {
            matches!(
                receipt.state,
                provenant_run::WriterDeliveryState::Delivered { .. }
            )
        })
    {
        return Some("delivery has not succeeded; worktree retained for pickup");
    }
    None
}

fn replay_worktree_settlement(
    receipt: Option<&serde_json::Value>,
    worktree: &Path,
) -> Result<Option<serde_json::Value>, std::io::Error> {
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    if receipt["state"] == "pruned" {
        return Ok(Some(receipt.clone()));
    }
    if receipt["state"] != "pruning" || receipt["worktree"] != serde_json::json!(worktree) {
        return Ok(None);
    }
    match std::fs::symlink_metadata(worktree) {
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut receipt = receipt.clone();
            receipt["state"] = serde_json::json!("pruned");
            receipt["recovered_from_intent"] = serde_json::json!(true);
            Ok(Some(receipt))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn settle_delivered_attempt(
    edge: &provenant_cli::run::Edge<'_>,
    reply: &mut provenant_cli::run::Reply,
    workers: &dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    if reply.result.status != provenant_records::Outcome::Ok {
        return Ok(());
    }
    let Some(id) = reply.result.data["attempt_id"].as_str() else {
        return Ok(());
    };
    // `lane deliver --deliver none` has no delivery receipt to settle.
    if reply.result.data["delivery"].is_null() {
        return Ok(());
    }
    let id = id.parse()?;
    let delivery = serde_json::from_value::<provenant_run::WriterDeliveryReceipt>(
        reply.result.data["delivery"].clone(),
    )?;
    if matches!(
        delivery.state,
        provenant_run::WriterDeliveryState::Delivered { .. }
    ) {
        let context = Context::open(edge.roots.clone(), edge.home, edge.cwd)?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        provenant_fabric_runtime::graph::FileJournal::retry_delivered_attempt(
            &edge.roots.state,
            context.identity.project,
            id,
            budget,
            &cancel,
            &clock,
        )?;
    }
    if let Some(receipt) = settle_attempt(edge, id, Some(&delivery), workers)? {
        reply.human.push_str(&format!(
            "worktree {}{}\n",
            receipt["state"].as_str().unwrap_or("unknown"),
            receipt["reason"]
                .as_str()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ));
        reply.result.data["worktree_settlement"] = receipt;
    }
    Ok(())
}

pub(crate) fn settle_dispatched_attempts(
    edge: &provenant_cli::run::Edge<'_>,
    reply: &mut provenant_cli::run::Reply,
    workers: &dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    if reply.result.data["worktree"]["owned"] != true {
        return Ok(());
    }
    let ids = reply.result.data["lanes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|lane| lane["attempt_id"].as_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut receipts = Vec::new();
    for id in ids {
        if let Some(receipt) = settle_attempt(edge, id.parse()?, None, workers)? {
            reply.human.push_str(&format!(
                "worktree {}{}\n",
                receipt["state"].as_str().unwrap_or("unknown"),
                receipt["reason"]
                    .as_str()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            ));
            receipts.push(receipt);
        }
    }
    if !receipts.is_empty() {
        reply.result.data["worktree_settlements"] = serde_json::json!(receipts);
    }
    Ok(())
}

fn settle_attempt(
    edge: &provenant_cli::run::Edge<'_>,
    id: provenant_core::AttemptId,
    delivery: Option<&provenant_run::WriterDeliveryReceipt>,
    workers: &dyn WorkerObserver,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error>> {
    let context = Context::open(edge.roots.clone(), edge.home, edge.cwd)?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let execution = provenant_run::ExecutionContext {
        state_root: edge.roots.state.clone(),
        project_id: context.identity.project,
        actor_id: context.identity.inbox.actor,
        read_prompts: true,
    };
    let store = provenant_run::RunStore::open(&execution, &call)?;
    let record = store.record(id, &call)?;
    if let Some(receipt) = store.worktree_settlement(id, &call)?
        && receipt["state"] == "pruned"
    {
        return Ok(Some(receipt));
    }
    let settled = provenant_run::HolderContext {
        schema_version: 1,
        roots: provenant_run::RootSeed {
            home: edge.home.into(),
            checkout: edge.assets.into(),
            state_root: edge.roots.state.as_path().into(),
        },
        project_id: record.project_id,
        actor_id: execution.actor_id,
        run_id: record.run_id,
        attempt_id: id,
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    settle_owned_worktree(&settled, edge.roots, workers, delivery)?;
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    Ok(store.worktree_settlement(id, &call)?)
}

impl provenant_fabric_runtime::graph::AttemptOwner for BoundGraphOwner<'_> {
    type Error = FacadeError;
    fn actor(&self) -> String {
        self.factory.context.identity.inbox.actor.to_string()
    }
    fn deliver_graph(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<Option<String>, Self::Error> {
        let branch = document
            .integration_branch
            .as_deref()
            .ok_or(FacadeError::InvalidInput)?;
        let session = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
        }
        .session(self.budget, self.cancel)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            budget: self.budget,
            cancel: self.cancel,
        };
        let mut body = format!("Graph {}\n\n", document.graph_id);
        for node in &checkpoint.nodes {
            let reference = node
                .attempts
                .last()
                .and_then(|attempt| attempt.attempt_ref.as_deref())
                .ok_or(FacadeError::StateCorrupt)?;
            let (_, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let id = id.parse().map_err(|_| FacadeError::StateCorrupt)?;
            let attempt = session.service().status(id, &call).map_err(run_error)?;
            body.push_str(&format!(
                "Node {}: attempt {id}, run {}\n\n",
                node.id, attempt.run_id
            ));
        }
        let url = provenant_run::HostDeliveryContext::capture()
            .deliver_graph_pr(
                &self.factory.cwd,
                branch,
                self.integration_base.ok_or(FacadeError::Conflict)?,
                &body,
            )
            .map_err(run_error)?;
        Ok(Some(url))
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
            let delivery = if let Some(mode) = replay.delivery {
                let branch = if let Some(path) = worktree {
                    let writer = provenant_run::resolve_writer(
                        self.factory.context.roots.state.as_path(),
                        &self.factory.cwd,
                        Path::new(path),
                    )
                    .map_err(run_error)?;
                    provenant_run::prepare_writer(
                        &writer,
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.identity.project,
                    )
                    .map_err(run_error)?
                    .branch
                    .as_str()
                    .trim_start_matches("refs/heads/")
                    .to_owned()
                } else {
                    String::new()
                };
                provenant_run::HostDeliveryContext::destination(mode.clone(), &branch)
                    .map_err(run_error)?
            } else {
                None
            };
            let operation_id = graph_operation("graph-attempt-operation", key)?;
            let input = SubmitInput::Submit {
                operation_id,
                tasks: vec![SubmittedTask {
                    base: None,
                    account: route.account.clone(),
                    access,
                    worktree: worktree.map(str::to_owned),
                    prompt: prompt.into(),
                    route: match &route.model {
                        Some(model) => provenant_records::run::RequestedRoute::Model {
                            provider: route.adapter.clone(),
                            model: if model.contains('/') {
                                model.clone()
                            } else {
                                format!("{}/{model}", route.adapter)
                            },
                            effort: route.effort.clone(),
                        },
                        None => provenant_records::run::RequestedRoute::Named {
                            name: route.alias.clone().ok_or(FacadeError::InvalidInput)?,
                            effort: route.effort.clone(),
                        },
                    },
                    timeout_seconds: self.factory.context.config.limits.graph_timeout_seconds,
                    delivery_id: None,
                    deliver: delivery,
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
                let store = owner.store(self.budget, self.cancel)?;
                let call = owner.call(self.budget, self.cancel);
                match store.custody_released(&attempt, &call) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Ok(AttemptObservation::Pending {
                            attempt_ref: reference,
                        });
                    }
                    Err(_) => {
                        return Ok(AttemptObservation::Unknown {
                            attempt_ref: reference,
                        });
                    }
                }
                let pickup = store
                    .delivered(attempt.attempt_id, &call)
                    .map_err(run_error)?;
                let effective_delivery = pickup
                    .as_ref()
                    .map(|(_, receipt)| receipt)
                    .or(attempt.delivery.as_ref());
                let launched = attempt.provider_launched.unwrap_or(true);
                Ok(AttemptObservation::Terminal {
                    launched,
                    attempt_ref: reference,
                    outcome: match outcome {
                        Outcome::Succeeded
                            if effective_delivery.is_some_and(|delivery| {
                                matches!(
                                    delivery.state,
                                    provenant_records::run::WriterDeliveryState::Failed { .. }
                                        | provenant_records::run::WriterDeliveryState::NotAttempted
                                )
                            }) =>
                        {
                            AttemptOutcome::OtherFailure
                        }
                        Outcome::Succeeded => AttemptOutcome::Succeeded,
                        Outcome::AuthRequired => AttemptOutcome::AuthRequired,
                        Outcome::RateLimited | Outcome::QuotaLimited => {
                            AttemptOutcome::UsageLimited
                        }
                        // A route refused before launch leaves the next route free to try.
                        Outcome::ModelUnavailable => AttemptOutcome::AdapterUnavailable,
                        Outcome::Rejected if !launched => AttemptOutcome::AdapterUnavailable,
                        Outcome::Failed => AttemptOutcome::TaskFailed,
                        // Uncommitted edits need the chair, so the node fails without fallback.
                        Outcome::Uncommitted => AttemptOutcome::OtherFailure,
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
                let scratch = provenant_confinement::worktrees::with_context(
                    &self.context.roots,
                    &self.home,
                    &self.cwd,
                    &self.clock,
                    cancel,
                    |context| {
                        let repo = context.utility.open_repo(
                            context.executor,
                            &self.cwd,
                            context.utility_budget,
                            cancel,
                        )?;
                        provenant_confinement::scratch::ensure(&repo, &self.clock, cancel)
                    },
                )
                .map_err(|_| {
                    FacadeError::io(
                        "prepare project scratch .provenant/scratch/graphs or .agent-run/graphs",
                    )
                })?;
                let path = if document.is_absolute() {
                    document.clone()
                } else if document.components().count() == 1 && !self.cwd.join(document).exists() {
                    scratch.join("graphs").join(document)
                } else {
                    self.cwd.join(document)
                };
                let path = provenant_os::fs::canonicalize(&path)
                    .map_err(|_| FacadeError::io("read project-local graph document; use .provenant/scratch/graphs or .agent-run/graphs"))?;
                if !path.starts_with(&self.cwd) {
                    return Err(FacadeError::ConfinementRefused);
                }
                let bytes = provenant_os::fs::FileHandle::open_source(&path)
                    .and_then(|mut file| file.read_bounded(1_048_576))
                    .map_err(|_| FacadeError::io("access graph journal"))?;
                let document: GraphDocument = provenant_records::decode_v1(
                    bytes.as_slice(),
                    provenant_records::Limits::CONFIG,
                )
                .map_err(|_| FacadeError::InvalidInput)?;
                if let Some(branch) = &document.integration_branch {
                    provenant_run::HostDeliveryContext::validate_integration_target(
                        &self.cwd, branch,
                    )
                    .map_err(run_error)?;
                }
                for node in &document.nodes {
                    if let Some(provenant_records::run::DeliveryMode::Integrate(target)) =
                        &node.delivery
                    {
                        provenant_run::HostDeliveryContext::validate_integration_target(
                            &self.cwd, target,
                        )
                        .map_err(run_error)?;
                    }
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
            )
            .map_err(|error| {
                use provenant_fabric_runtime::graph::JournalError;
                match error {
                    // Unreadable is not corrupt: a denied or vanished read stays Io.
                    JournalError::Filesystem(_)
                    | JournalError::Io(_)
                    | JournalError::DurabilityUnconfirmed(_) => {
                        FacadeError::io("access graph journal")
                    }
                    _ => FacadeError::StateCorrupt,
                }
            })?
            .ok_or(FacadeError::NotFound)?;
            let mut run = graph_snapshot(checkpoint);
            run.advance_error = FileJournal::read_advance_error(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            )
            .map_err(|_| FacadeError::io("access graph journal"))?;
            return Ok(run);
        }
        let mut journal = FileJournal::open(
            &self.context.roots.state,
            self.context.identity.project,
            id,
            budget,
            cancel,
            &self.clock,
        )
        .map_err(|_| FacadeError::io("access graph journal"))?;
        if let Some(document) = &document {
            let integration_base = if journal
                .load(id)
                .map_err(|_| FacadeError::StateCorrupt)?
                .is_some()
            {
                journal.integration_base()
            } else if let Some(branch) = &document.integration_branch {
                let reference = match branch.strip_prefix("origin/") {
                    Some(branch) => format!("refs/remotes/origin/{branch}"),
                    None => format!("refs/heads/{branch}"),
                };
                let git = provenant_os::git::git_executable().ok_or(FacadeError::InvalidInput)?;
                Some(
                    provenant_run::resolve_git_base(&git, &self.cwd, &reference)
                        .map_err(run_error)?
                        .parse()
                        .map_err(|_| FacadeError::StateCorrupt)?,
                )
            } else {
                None
            };
            journal
                .bind(provenant_fabric_runtime::graph::GraphExecution {
                    integration_base,
                    directory: self.cwd.clone(),
                    document: document.clone(),
                })
                .map_err(|_| FacadeError::Conflict)?;
            let mut owner = BoundGraphOwner {
                factory: self,
                integration_base,
                budget,
                cancel,
            };
            let run = if resume {
                provenant_fabric_runtime::graph::resume(document, &mut owner, &mut journal)
            } else {
                provenant_fabric_runtime::graph::advance(document, &mut owner, &mut journal)
            };
            let run = run.map_err(|error| match error {
                provenant_fabric_runtime::graph::GraphError::Run(error) => error,
                provenant_fabric_runtime::graph::GraphError::Invalid(_) => {
                    FacadeError::InvalidInput
                }
                provenant_fabric_runtime::graph::GraphError::PlanChanged => FacadeError::Conflict,
                _ => FacadeError::StateCorrupt,
            })?;
            // A manual advance that succeeded supersedes an earlier failure.
            FileJournal::set_advance_error(
                &self.context.roots.state,
                self.context.identity.project,
                id,
                None,
            )
            .map_err(|_| FacadeError::io("access graph journal"))?;
            return Ok(run);
        }
        let mut checkpoint = journal
            .load(id)
            .map_err(|_| FacadeError::StateCorrupt)?
            .ok_or(FacadeError::NotFound)?;
        if matches!(input, GraphInput::Cancel { .. }) {
            // Persist the admission fence before asking the run owner to stop live attempts.
            checkpoint.cancelled = true;
            journal
                .save(&checkpoint)
                .map_err(|_| FacadeError::io("access graph journal"))?;
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
    } else if (checkpoint.delivery_required
        && !checkpoint.delivered
        && checkpoint
            .nodes
            .iter()
            .all(|node| node.state == NodeState::Succeeded))
        || checkpoint
            .nodes
            .iter()
            .any(|node| matches!(node.state, NodeState::Pending | NodeState::Running))
    {
        GraphStatus::Pending
    } else {
        GraphStatus::Complete
    };
    GraphRun {
        status,
        checkpoint,
        advance_error: None,
    }
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
pub fn graph(
    input: provenant_cli::fabric::Graph,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    use provenant_cli::fabric::Graph;
    let (command, input) = match input {
        Graph::Run { document } => ("fabric graph run", GraphInput::Run { document }),
        Graph::Status { id } => ("fabric graph status", GraphInput::Status { id }),
        Graph::Cancel { id } => ("fabric graph cancel", GraphInput::Cancel { id }),
        Graph::Resume { document } => ("fabric graph resume", GraphInput::Resume { document }),
    };
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
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
            } else if let Some(error @ FacadeError::DispatchRefused { .. }) =
                error.downcast_ref::<FacadeError>()
            {
                (provenant_records::Outcome::Refused, error.code())
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
            .map_err(|error| FacadeError::from(&error))?;
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
                .map_err(|_| FacadeError::io("read run event cursor"))?;
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
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<(EventsResult, bool), Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
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
                        wait_seconds: None,
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
            let typed = error
                .downcast_ref::<CoordinationError>()
                .map(FacadeError::from)
                .or_else(|| error.downcast_ref::<FacadeError>().cloned());
            crate::fail(
                "fabric events read",
                typed
                    .as_ref()
                    .map(provenant_cli::fabric::coordination_outcome)
                    .unwrap_or(provenant_records::Outcome::Error),
                typed.as_ref().map_or("events_read", FacadeError::code),
                &error.to_string(),
                globals,
            )
        }
    }
}
#[cfg(test)]
mod peer_tests {
    use super::*;
    use provenant_core::{AttemptId, RunId, TaskId};
    use provenant_run::{AttemptState, Outcome, RunStore};

    fn host_test(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        if !provenant_cli::fabric::in_lane() {
            return Ok(false);
        }
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            name.into(),
            "--nocapture".into(),
        ];
        let removed = [
            provenant_core::RUN_STATE_DIRECTORY_ENV,
            provenant_run::LANE_DEPTH_ENV,
            provenant_run::PARENT_ATTEMPT_ENV,
        ];
        let environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| !removed.iter().any(|name| key == name))
            .collect();
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, std::time::Duration::from_secs(30));
        options.env = Some(&environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(true)
    }

    struct FreeWorkers;
    impl WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState>
        {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
                .collect()
        }
    }

    struct SettlementFixture {
        rig: crate::worktree_tests::support::TestRig,
        roots: provenant_core::Roots,
        store: RunStore,
        settled: provenant_run::HolderContext,
        worktree: PathBuf,
    }

    fn publish_success(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let attempt = rig
            .state
            .join("runs")
            .join(record.run_id.to_string())
            .join("attempts")
            .join(record.attempt_id.to_string());
        let directory = provenant_os::fs::Directory::open_anchor(&attempt)?;
        use provenant_supervisor::custody::{
            ExitObservation, PublicationState, Scope, SettlementEvidence,
        };
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope: Scope {
                run: record.run_id,
                attempt: record.attempt_id,
                generation: record.generation,
            },
            termination: Some(provenant_os::exec::Termination::Exit(0)),
            never_spawned: false,
            stop_reason: None,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: false,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
            stdout: Default::default(),
            stderr: Default::default(),
        };
        let mut exit_wire = serde_json::to_value(&exit)?;
        let object = exit_wire.as_object_mut().ok_or("exit object")?;
        object.remove("publication");
        object.insert("schema_version".into(), 1.into());
        for (name, wire) in [
            ("process-exit.json", exit_wire),
            (
                "writer-terminal.json",
                serde_json::json!({"schema_version":1,"head":head,"protected_paths":[]}),
            ),
        ] {
            directory
                .atomic_write(
                    std::ffi::OsStr::new(name),
                    ".fixture-",
                    &serde_json::to_vec(&wire)?,
                    provenant_os::fs::PublishMode::NoReplace,
                )?
                .require_durable()?;
        }
        drop(directory.lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
            call.budget,
            call.cancel,
            call.clock,
        )?);
        let route = provenant_run::RouteRecord {
            provider: Some("fixture".into()),
            model: Some("fixture/model".into()),
            ..Default::default()
        };
        store.publish_terminal(
            record.attempt_id,
            record.revision,
            &provenant_run::TerminalPublication {
                outcome: Outcome::Succeeded,
                finished_at: call.clock.wall_now()?,
                started_at: None,
                provider_ms: None,
                reason: None,
                result: "OK",
                complete: true,
                redacted: false,
                resolved_route: Some(route.clone()),
                applied_route: Some(route.clone()),
                observed_route: Some(route),
                usage: Default::default(),
                session: None,
            },
            &exit,
            call,
        )?;
        Ok(())
    }

    impl SettlementFixture {
        fn resumed() -> Result<Self, Box<dyn std::error::Error>> {
            use provenant_confinement::worktrees::{
                self, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
            };
            let rig = crate::worktree_tests::support::TestRig::new()?;
            let git = provenant_os::git::git_executable().ok_or("git")?;
            for args in [
                vec!["config", "user.name", "Fixture"],
                vec!["config", "user.email", "fixture@example.invalid"],
                vec!["commit", "--allow-empty", "-qm", "base"],
            ] {
                let output = rig.execute(&git, &args, None)?;
                assert_eq!(crate::worktree_tests::support::code(&output), 0);
            }
            let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
            let identity = Context::open(roots.clone(), &rig.home, &rig.project)?.identity;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = provenant_run::Request {
                clock: &clock,
                cancel: &cancel,
                budget: WaitBudget::cli(clock.mono_now(), 30)?,
            };
            let context = provenant_run::ExecutionContext {
                state_root: roots.state.clone(),
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                read_prompts: true,
            };
            let store =
                RunStore::open(&context, &call)?.with_writer_registry(roots.state.as_path());
            let info = worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    worktrees::create(
                        &worktrees::WorktreeContext {
                            workers: &FreeWorkers,
                            ..*context
                        },
                        WorktreeCreate {
                            branch: "resumed-lane".into(),
                            base: Some("HEAD".into()),
                        },
                        &cancel,
                    )
                },
            )?;
            let run = provenant_run::Service::new(&store).dispatch(
                provenant_run::DispatchRequest {
                    operation_id: "settlement-original".parse()?,
                    concurrency: 1,
                    lane_depth: 1,
                    parent_attempt: None,
                    tasks: vec![provenant_run::DispatchTask {
                        task_id: TaskId::from_bytes([3; 16]),
                        prompt: "reply OK".into(),
                        base: Some(info.head.to_string()),
                        profile: Default::default(),
                        route: provenant_run::RequestedRoute::Model {
                            provider: "fixture".into(),
                            model: "fixture/model".into(),
                            effort: None,
                        },
                        account: None,
                        fallback: vec![],
                        timeout_seconds: 30,
                        delivery_id: None,
                        deliver: None,
                        worktree: Some(info.path.clone()),
                        access: Default::default(),
                    }],
                },
                &rig.project,
                &call,
            )?;
            let prior = store
                .pending_attempts(run.run_id, &call)?
                .first()
                .ok_or("attempt")?
                .attempt_id;
            worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    worktrees::save_owner(
                        &tree,
                        &WorktreeOwner {
                            attempt_id: Some(prior),
                            operation_id: Some(run.operation_id.clone()),
                            branch: "resumed-lane".into(),
                            base: info.head.to_string(),
                            host: identity.host,
                            created_at: clock
                                .wall_now()
                                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                            origin: WorktreeOrigin::Lane,
                            label: None,
                        },
                    )?;
                    Ok(())
                },
            )?;
            store.withdraw(
                prior,
                AttemptState::Queued,
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
            let resume = provenant_run::Resume {
                prior,
                expected_generation: 0,
                operation_id: "settlement-resume".parse()?,
                run_id: RunId::from_bytes([30; 16]),
                attempt_id: AttemptId::from_bytes([31; 16]),
                prompt: "continue".into(),
                created_at: clock.wall_now()?,
            };
            store.resume_fresh(&resume, "fixture", &call)?;
            let record = store.record(resume.attempt_id, &call)?;
            publish_success(&rig, &store, &record, info.head, &call)?;
            let settled = provenant_run::HolderContext {
                schema_version: 1,
                roots: provenant_run::RootSeed {
                    home: rig.home.clone(),
                    checkout: rig.project.clone(),
                    state_root: rig.state.clone(),
                },
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                run_id: record.run_id,
                attempt_id: record.attempt_id,
                host_delivery: None,
                launch: None,
                sealed: None,
            };
            Ok(Self {
                rig,
                roots,
                store,
                settled,
                worktree: info.path,
            })
        }
    }

    #[test]
    fn settlement_failure_in_one_graph_does_not_stop_another_graph()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::settlement_failure_in_one_graph_does_not_stop_another_graph",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{
            self, AttemptKey, AttemptObservation, AttemptOwner, FileJournal, GraphDocument,
            GraphExecution, GraphRoute, NodeState, Replay,
        };
        struct SeedAttempt {
            reference: String,
            actor: String,
        }
        impl AttemptOwner for SeedAttempt {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                self.actor.clone()
            }
            fn observe_or_start(
                &mut self,
                _: &AttemptKey,
                _: &str,
                _: &GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: Replay<'_>,
            ) -> Result<AttemptObservation, Self::Error> {
                Ok(AttemptObservation::Pending {
                    attempt_ref: Some(self.reference.clone()),
                })
            }
        }
        let mut fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        let identity = Context::open(
            fixture.roots.clone(),
            &fixture.rig.home,
            &fixture.rig.project,
        )?
        .identity;
        // Two different terminal attempts let the settlement error be attributed
        // to A alone. B's journal still needs its running node to be observed.
        let continuation = provenant_run::Resume {
            prior: fixture.settled.attempt_id,
            expected_generation: 0,
            operation_id: "graph-b-attempt".parse()?,
            run_id: RunId::from_bytes([40; 16]),
            attempt_id: AttemptId::from_bytes([41; 16]),
            prompt: "continue".into(),
            created_at: clock.wall_now()?,
        };
        fixture
            .store
            .resume_fresh(&continuation, "fixture", &call)?;
        let second = fixture.store.record(continuation.attempt_id, &call)?;
        let head = fixture
            .store
            .record(fixture.settled.attempt_id, &call)?
            .head
            .ok_or("head")?;
        publish_success(&fixture.rig, &fixture.store, &second, head, &call)?;
        for (id, attempt) in [
            ("a-settlement-error", fixture.settled.attempt_id),
            ("b-completes", continuation.attempt_id),
        ] {
            let document: GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write","worktree":fixture.worktree,"route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?;
            let mut journal = FileJournal::open(
                &fixture.roots.state,
                fixture.settled.project_id,
                id,
                call.budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                integration_base: None,
                directory: fixture.rig.project.clone(),
                document: document.clone(),
            })?;
            let mut seed = SeedAttempt {
                reference: format!("{}/{attempt}", identity.host),
                actor: identity.inbox.actor.to_string(),
            };
            let seeded = graph::advance(&document, &mut seed, &mut journal)?;
            assert_eq!(seeded.checkpoint.nodes[0].state, NodeState::Running);
        }
        // An unsafe receipt path makes settlement itself fail before the graph
        // loop. It must leave A observable and still allow B to complete.
        let receipt_path = fixture
            .rig
            .state
            .join("runs")
            .join(fixture.settled.run_id.to_string())
            .join("attempts")
            .join(fixture.settled.attempt_id.to_string())
            .join("worktree-settlement.json");
        std::fs::create_dir(&receipt_path)?;
        let settlement_error =
            settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)
                .err()
                .ok_or("settlement should fail")?
                .to_string();
        fixture.settled.launch = Some(provenant_run::HostLaunchContext {
            configuration_root: fixture.rig.state.clone(),
            cwd: fixture.rig.project.clone(),
            executables: Default::default(),
            git: provenant_os::git::git_executable().ok_or("git")?,
            codex_home: None,
            fixture_state_write: false,
        });
        let error = advance_graphs_after_settlement(&fixture.settled, &FreeWorkers)
            .err()
            .ok_or("settlement failure must propagate")?;
        assert_eq!(error.to_string(), settlement_error);
        assert_eq!(
            FileJournal::read_advance_error(
                &fixture.roots.state,
                fixture.settled.project_id,
                "a-settlement-error"
            )?
            .as_deref(),
            Some(settlement_error.as_str())
        );
        let completed = FileJournal::read_checkpoint(
            &fixture.roots.state,
            fixture.settled.project_id,
            "b-completes",
        )?
        .ok_or("B checkpoint")?;
        assert_eq!(completed.nodes[0].state, NodeState::Succeeded);
        assert!(
            FileJournal::pending_execution(
                &fixture.roots.state,
                fixture.settled.project_id,
                "b-completes"
            )?
            .is_none()
        );
        assert!(
            FileJournal::read_advance_error(
                &fixture.roots.state,
                fixture.settled.project_id,
                "b-completes"
            )?
            .is_none()
        );
        assert!(fixture.worktree.exists());
        Ok(())
    }

    #[test]
    fn graph_submission_pins_the_integration_base_across_replay()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_submission_pins_the_integration_base_across_replay",
        )? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::FileJournal;
        let fixture = SettlementFixture::resumed()?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture
            .rig
            .execute(&git, &["branch", "integration"], None)?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadWait,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join("base-graph.json");
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":"pinned-base", "integration_branch":"integration", "delivery":"pr",
                "nodes":[{"id":"one","prompt":"reply OK","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?,
        )?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        // The fixture adapter is unavailable, so no provider launches. The
        // first graph checkpoint must already contain its integration base.
        let run = factory.graph(
            GraphInput::Run {
                document: document.clone(),
            },
            budget,
            &cancel,
        )?;
        assert_eq!(
            run.checkpoint.nodes[0].state,
            provenant_fabric_runtime::graph::NodeState::Failed
        );
        assert!(!run.checkpoint.nodes[0].attempts[0].launched);
        let base = {
            let journal = FileJournal::open(
                &fixture.roots.state,
                fixture.settled.project_id,
                "pinned-base",
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.integration_base().ok_or("graph base missing")?
        };
        assert_eq!(
            Some(base),
            fixture
                .store
                .record(
                    fixture.settled.attempt_id,
                    &provenant_run::Request {
                        clock: &factory.clock,
                        cancel: &cancel,
                        budget
                    }
                )?
                .head
        );
        for args in [
            vec![
                "commit",
                "--allow-empty",
                "-qm",
                "Unrelated primary checkout",
            ],
            vec!["branch", "-f", "integration", "HEAD"],
        ] {
            let output = fixture.rig.execute(&git, &args, None)?;
            assert_eq!(crate::worktree_tests::support::code(&output), 0);
        }
        let replay = factory.graph(GraphInput::Resume { document }, budget, &cancel)?;
        assert_eq!(replay.checkpoint, run.checkpoint);
        let journal = FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            "pinned-base",
            budget,
            &cancel,
            &factory.clock,
        )?;
        assert_eq!(journal.integration_base(), Some(base));
        Ok(())
    }

    #[test]
    fn resumed_lane_settlement_prunes_and_records_its_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::resumed_lane_settlement_prunes_and_records_its_receipt")?
        {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        let receipt = fixture
            .store
            .worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("missing resumed settlement receipt")?;
        assert_eq!(receipt["state"], "pruned", "{receipt}");
        assert!(!fixture.worktree.exists());
        assert!(
            fixture
                .rig
                .state
                .join("runs")
                .join(fixture.settled.run_id.to_string())
                .join("attempts")
                .join(fixture.settled.attempt_id.to_string())
                .join("worktree-settlement.json")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn deliver_none_does_not_try_to_settle_a_missing_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        struct NoWorkers;
        impl provenant_confinement::worktrees::WorkerObserver for NoWorkers {
            fn observe_roots(
                &self,
                _: &[std::path::PathBuf],
            ) -> std::collections::BTreeMap<
                std::path::PathBuf,
                provenant_confinement::worktrees::WorkerState,
            > {
                std::collections::BTreeMap::new()
            }
        }
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let assets = root.join("assets");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&assets)?;
        let roots = crate::roots::child(&home, &assets, root.join("state"))?;
        let edge = provenant_cli::run::Edge {
            roots: &roots,
            home: &home,
            assets: &assets,
            cwd: &assets,
        };
        let mut reply = provenant_cli::run::Reply {
            result: provenant_records::CommandResult::new(
                "lane deliver",
                provenant_records::Outcome::Ok,
                serde_json::json!({"attempt_id":"unused", "deliver":"none"}),
            ),
            human: "delivery disabled\n".into(),
        };
        super::settle_delivered_attempt(&edge, &mut reply, &NoWorkers)?;
        assert_eq!(reply.human, "delivery disabled\n");
        assert!(reply.result.data["worktree_settlement"].is_null());
        Ok(())
    }

    #[test]
    fn unsuccessful_lanes_retain_the_worktree_even_after_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_run::{AttemptState, Outcome, WriterDeliveryReceipt, WriterDeliveryState};
        let delivered = WriterDeliveryReceipt {
            schema_version: 1,
            remote: "origin".into(),
            branch: "lane".into(),
            head: None,
            state: WriterDeliveryState::Delivered { pull_request: None },
        };
        for outcome in [
            Outcome::Failed,
            Outcome::RateLimited,
            Outcome::Cancelled,
            Outcome::TimedOut,
            Outcome::Rejected,
        ] {
            let state = AttemptState::Terminal {
                outcome,
                exit_code: Some(1),
                finished_at: provenant_core::WallTime::from_micros(0)?,
                reason: None,
            };
            assert!(super::settlement_retention_reason(&state, false, None).is_some());
            assert!(super::settlement_retention_reason(&state, true, Some(&delivered)).is_some());
        }
        let succeeded = AttemptState::Terminal {
            outcome: Outcome::Succeeded,
            exit_code: Some(0),
            finished_at: provenant_core::WallTime::from_micros(0)?,
            reason: None,
        };
        assert!(super::settlement_retention_reason(&succeeded, false, None).is_none());
        assert!(super::settlement_retention_reason(&succeeded, true, Some(&delivered)).is_none());
        assert!(super::settlement_retention_reason(&succeeded, true, None).is_some());
        let failed_delivery = WriterDeliveryReceipt {
            state: WriterDeliveryState::NotAttempted,
            ..delivered
        };
        assert!(
            super::settlement_retention_reason(&succeeded, true, Some(&failed_delivery)).is_some()
        );
        Ok(())
    }

    #[test]
    fn settlement_intent_replays_a_prune_after_a_crash() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let worktree = temp.path().join("lane");
        std::fs::create_dir(&worktree)?;
        let intent = serde_json::json!({
            "schema_version":1, "state":"pruning", "worktree":worktree
        });
        assert!(super::replay_worktree_settlement(Some(&intent), &worktree)?.is_none());
        std::fs::remove_dir(&worktree)?;
        let receipt = super::replay_worktree_settlement(Some(&intent), &worktree)?
            .ok_or("missing recovered prune receipt")?;
        assert_eq!(receipt["state"], "pruned");
        assert_eq!(receipt["recovered_from_intent"], true);
        assert_eq!(
            super::replay_worktree_settlement(Some(&receipt), &worktree)?,
            Some(receipt)
        );
        assert!(
            super::replay_worktree_settlement(Some(&intent), &temp.path().join("other"))?.is_none()
        );
        let retained = serde_json::json!({"state":"retained", "worktree":worktree});
        assert!(super::replay_worktree_settlement(Some(&retained), &worktree)?.is_none());
        Ok(())
    }

    #[test]
    fn submit_io_keeps_the_inner_operation_without_inventing_a_path() {
        let error = super::scope_io(super::FacadeError::io("access Fabric database"), "submit");
        assert_eq!(
            error,
            super::FacadeError::Io {
                operation: "submit: access Fabric database".into(),
                path: None,
            }
        );
    }

    #[test]
    fn submit_io_retains_single_prefix_for_peer_error_and_scopes_local_error() {
        let peer_error = super::FacadeError::Io {
            operation: "submit: open writer repository".into(),
            path: Some("/repo".into()),
        };
        let rescoped = super::scope_io(peer_error.clone(), "submit");
        assert_eq!(
            rescoped,
            super::FacadeError::Io {
                operation: "submit: open writer repository".into(),
                path: Some("/repo".into()),
            }
        );

        let local_error = super::FacadeError::Io {
            operation: "open writer repository".into(),
            path: Some("/repo".into()),
        };
        let scoped = super::scope_io(local_error, "submit");
        assert_eq!(
            scoped,
            super::FacadeError::Io {
                operation: "submit: open writer repository".into(),
                path: Some("/repo".into()),
            }
        );
    }

    #[test]
    fn writer_repository_open_failure_keeps_its_operation_and_project_path()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let request = provenant_run::WriterRequest {
            project: root.clone(),
            worktree: root.join("writer"),
        };
        let project = provenant_core::ProjectId::from_bytes([1; 16]);
        let error = match provenant_run::prepare_writer(&request, &root, project) {
            Ok(_) => return Err("non-repository unexpectedly opened".into()),
            Err(error) => super::run_error(error),
        };
        assert_eq!(
            serde_json::to_value(error)?,
            serde_json::json!({
                "code": "io",
                "operation": "open writer repository",
                "path": root,
            })
        );
        Ok(())
    }

    #[test]
    fn typed_run_errors_keep_their_codes() {
        for (error, expected) in [
            (provenant_run::RunError::Nonterminal, "not_terminal"),
            (
                provenant_run::RunError::OperationConflict,
                "operation_conflict",
            ),
            (provenant_run::RunError::Conflict, "conflict"),
            (
                provenant_run::RunError::ProviderStateExpired,
                "provider_state_expired",
            ),
            // The CLI and MCP share one categorisation.
            (
                provenant_run::RunError::Validation("input"),
                "invalid_input",
            ),
            (provenant_run::RunError::ArtifactMismatch, "state_corrupt"),
            (provenant_run::RunError::Refused, "confinement_refused"),
            (provenant_run::RunError::Capacity, "busy"),
        ] {
            assert_eq!(super::run_error(error).code(), expected);
        }
    }
    #[test]
    fn peer_run_errors_stay_typed_through_cli_rendering() -> Result<(), Box<dyn std::error::Error>>
    {
        for error in [
            super::FacadeError::NotTerminal,
            super::FacadeError::OperationConflict,
            super::FacadeError::OwnerUnavailable,
            super::FacadeError::Io {
                operation: "openat directory".into(),
                path: Some("/state/worktrees".into()),
            },
            super::FacadeError::WorktreeUnregistered {
                path: "/repo/.worktrees/writer".into(),
            },
        ] {
            let expected = error.clone();
            let response = super::peer_result::<()>(Err(error), |()| super::PeerResult::Error {
                error: super::FacadeError::io("test"),
            });
            let response: super::PeerResult =
                serde_json::from_slice(&serde_json::to_vec(&response)?)?;
            let super::PeerResult::Error { error } = response else {
                return Err("expected error".into());
            };
            let (status, kind) = provenant_cli::fabric::facade_outcome(&error);
            let fail = provenant_cli::run::Fail {
                status,
                kind,
                message: error.to_string(),
                detail: Some(error),
            };
            assert_eq!(super::fail_error(fail), expected);
        }
        Ok(())
    }
    #[test]
    fn events_within_trims_whole_events_and_counts_separators() {
        // Serialised sizes are 4, 12 and 3 bytes; the array adds brackets and commas.
        let events = ["aa", "bbbbbbbbbb", "c"];
        for (max_bytes, kept) in [
            (65536, 3),
            // Two events are 2 + 4 + 1 + 12 = 19 bytes; the comma counts.
            (19, 2),
            (18, 1),
            // One event is 2 + 4 bytes.
            (6, 1),
            (5, 0),
            (0, 0),
        ] {
            assert_eq!(
                super::events_within(&events, max_bytes).unwrap(),
                kept,
                "{max_bytes}"
            );
        }
        // A large event stops the page even when a later small one would fit.
        assert_eq!(super::events_within(&events, 10).unwrap(), 1);
        assert_eq!(super::events_within::<&str>(&[], 2).unwrap(), 0);
    }
    #[test]
    fn host_probe_budget_is_shared_fairly() {
        assert_eq!(super::fair_probe_micros(10_000_000, 4), Some(2_500_000));
        assert_eq!(super::fair_probe_micros(55_000_000, 100), Some(550_000));
    }
    #[test]
    fn forwarded_unobservable_cancel_keeps_its_operation() -> Result<(), Box<dyn std::error::Error>>
    {
        let error = super::FacadeError::LaunchUnknown {
            operation_id: provenant_core::OperationId::parse("stop")?,
        };
        let reply = super::peer_result::<()>(Err(error), |()| super::PeerResult::Error {
            error: super::FacadeError::io("test"),
        });
        let super::PeerResult::Error { error } = reply else {
            return Err("expected error".into());
        };
        assert!(
            matches!(error, super::FacadeError::LaunchUnknown { operation_id } if operation_id.as_str() == "stop")
        );
        Ok(())
    }
}
