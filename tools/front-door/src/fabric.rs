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
    mcp_project: Option<PathBuf>,
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
    fn preflight(&self, call: &provenant_fabric_mcp::tools::ToolCall) -> Result<(), ServiceError> {
        if let provenant_fabric_mcp::tools::ToolCall::Inbox(input, _) = call {
            let ctx = &self.context;
            provenant_fabric_runtime::inbox::validate_request(
                &ctx.identity,
                &ctx.config,
                ctx.in_lane,
                input,
            )?;
        }
        Ok(())
    }
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
impl provenant_fabric_mcp::tools::McpFacade for BoundFacade<'_> {
    fn console(
        &mut self,
        input: provenant_fabric_mcp::tools::ConsoleInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ServiceError> {
        use provenant_fabric_mcp::tools::ConsoleInput;
        let mut args = vec!["console".to_owned()];
        match input {
            ConsoleInput::Projects => args.push("projects".into()),
            ConsoleInput::Overview => args.push("overview".into()),
            ConsoleInput::Sessions {
                session,
                limit,
                cursor,
            } => {
                args.push("sessions".into());
                if let Some(session) = session {
                    args.push(session);
                }
                args.extend(["--limit".into(), limit.to_string()]);
                if let Some(cursor) = cursor {
                    args.extend(["--cursor".into(), cursor]);
                }
            }
            ConsoleInput::Board {
                all,
                limit,
                cursor,
                hosts,
            } => {
                args.push("board".into());
                if all {
                    args.push("--all".into());
                }
                args.extend(["--limit".into(), limit.to_string()]);
                if let Some(cursor) = cursor {
                    args.extend(["--cursor".into(), cursor]);
                }
                if let Some(hosts) = hosts {
                    args.extend(["--hosts".into(), hosts]);
                }
            }
            ConsoleInput::Tree {
                max_depth,
                max_nodes,
                detail,
                hosts,
            } => {
                args.extend([
                    "tree".into(),
                    "--max-depth".into(),
                    max_depth.to_string(),
                    "--max-nodes".into(),
                    max_nodes.to_string(),
                ]);
                if detail {
                    args.push("--detail".into());
                }
                if let Some(hosts) = hosts {
                    args.extend(["--hosts".into(), hosts]);
                }
            }
        }
        self.cli_service(args, None, budget, cancel)
    }
    fn chair(
        &mut self,
        input: provenant_fabric_mcp::tools::ChairInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ServiceError> {
        use provenant_fabric_mcp::tools::ChairInput;
        let mut args = vec!["chair".to_owned()];
        let mut body = None;
        match input {
            ChairInput::SummaryShow { session } => {
                args.extend(["summary".into(), "show".into(), session])
            }
            ChairInput::SummaryDiff { session, from, to } => {
                args.extend([
                    "summary".into(),
                    "diff".into(),
                    session,
                    "--from".into(),
                    from.to_string(),
                ]);
                if let Some(to) = to {
                    args.extend(["--to".into(), to.to_string()]);
                }
            }
            ChairInput::SummaryUpdate {
                session,
                patch,
                expect_revision,
            } => {
                args.extend([
                    "summary".into(),
                    "update".into(),
                    session,
                    "--file".into(),
                    "-".into(),
                    "--expect-revision".into(),
                    expect_revision.to_string(),
                ]);
                body = Some(serde_json::to_vec(&patch)?);
            }
            ChairInput::LogList {
                session,
                after,
                limit,
            } => {
                args.extend([
                    "log".into(),
                    "list".into(),
                    "--session".into(),
                    session,
                    "--limit".into(),
                    limit.to_string(),
                ]);
                if let Some(after) = after {
                    args.extend(["--after".into(), after.to_string()]);
                }
            }
            ChairInput::LogAppend {
                session,
                entry,
                operation_id,
                issued_at,
            } => {
                args.extend([
                    "log".into(),
                    "append".into(),
                    "--session".into(),
                    session,
                    "--file".into(),
                    "-".into(),
                ]);
                if let Some(id) = operation_id {
                    args.extend(["--operation-id".into(), id]);
                }
                if let Some(time) = issued_at {
                    args.extend(["--issued-at".into(), time.to_string()]);
                }
                body = Some(serde_json::to_vec(&entry)?);
            }
        }
        self.cli_service(args, body, budget, cancel)
    }
    fn inbox_owner(
        &mut self,
        input: provenant_fabric_runtime::inbox::InboxInput,
        host: Option<String>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::inbox::InboxResult, ServiceError> {
        use provenant_fabric_runtime::inbox::{InboxInput, InboxReply};

        let ctx = &self.factory.context;
        let Some(host) =
            host.filter(|host| host != "local" && host != &ctx.identity.host.to_string())
        else {
            return self.inbox(input, budget, cancel).map_err(Into::into);
        };
        let error = |code: &str, message: String| ServiceError::InboxOwner {
            code: code.into(),
            message,
        };
        let remote =
            Context::open_read_only(ctx.roots.clone(), &self.factory.home, &self.factory.cwd)
                .map_err(|failure| error("inbox_unavailable", failure.to_string()))?;
        if matches!(
            input,
            InboxInput::Close { .. } | InboxInput::Acknowledge { .. }
        ) {
            return Err(error(
                "owner_local_only",
                format!(
                    "inbox owner {host}: run this operation on the owner host or from the owning chair session"
                ),
            ));
        }
        let target = remote.peer_host(&host).ok_or_else(|| {
            error(
                "owner_unavailable",
                format!("inbox owner {host} is not configured"),
            )
        })?;
        match remote.peer(
            &self.factory.clock,
            target,
            PeerAction::Inbox {
                extra: Default::default(),
                input,
            },
            budget,
            cancel,
        ) {
            Ok(PeerResult::Inbox {
                data: InboxReply::Ok { result, .. },
                ..
            }) => Ok(*result),
            Ok(PeerResult::Inbox {
                data: InboxReply::Error { code, message, .. },
                ..
            }) => Err(error(&code, message)),
            Ok(_) => Err(error(
                "unsupported_owner",
                "peer does not support inbox operations".into(),
            )),
            Err(failure) => {
                let code = match failure.error {
                    FacadeError::LaunchUnknown { .. } => "ambiguous_outcome",
                    FacadeError::PeerVersion | FacadeError::PeerProtocolMismatch { .. } => {
                        "unsupported_owner"
                    }
                    _ => "owner_unavailable",
                };
                Err(error(
                    code,
                    failure.detail.unwrap_or_else(|| failure.error.to_string()),
                ))
            }
        }
    }
}
impl BoundFacade<'_> {
    // Reuse the process-edge composition so peer coverage and chair custody
    // follow exactly the same path as the CLI, with fixed argv and bounded IO.
    fn cli_service(
        &self,
        args: Vec<String>,
        body: Option<Vec<u8>>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ServiceError> {
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
        let executable = std::env::current_exe().map_err(|_| FacadeError::OwnerUnavailable)?;
        let mut argv = vec![
            executable.into_os_string(),
            "--json".into(),
            "--state-dir".into(),
            self.factory.context.roots.state.as_path().into(),
        ];
        if let Some(project) = &self.factory.mcp_project {
            argv.extend(["--project".into(), project.as_os_str().into()]);
        }
        argv.extend(args.into_iter().map(Into::into));
        let timeout = std::time::Duration::from_micros(
            budget.remaining(self.factory.clock.mono_now()).as_micros(),
        );
        if timeout.is_zero() {
            return Err(FacadeError::Timeout.into());
        }
        let mut options = ExecOptions::new(&argv, &self.factory.cwd, timeout);
        options.stdin = body.as_deref();
        options.cancel = Some(cancel);
        options.process_group = true;
        options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
        options.stderr = StreamPolicy::Capture { limit: 16 * 1024 };
        let output = self
            .factory
            .executor
            .execute(options)
            .map_err(|_| FacadeError::OwnerUnavailable)?;
        if output.timed_out {
            return Err(FacadeError::Timeout.into());
        }
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled.into());
        }
        if output.termination != Termination::Exit(0) {
            return Err(cli_failure(&output.stdout.bytes, &output.stderr.bytes));
        }
        if output.stdout.truncated {
            return Err(ServiceError::ResultTooLarge);
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)
            .map_err(|_| FacadeError::OwnerUnavailable)?;
        Ok(value["data"].clone())
    }
}

fn cli_failure(stdout: &[u8], stderr: &[u8]) -> ServiceError {
    let parsed = serde_json::from_slice::<serde_json::Value>(stdout).ok();
    if let Some(value) = parsed.as_ref()
        && let (Some(code), Some(message)) = (
            value["error"]["code"].as_str(),
            value["error"]["message"].as_str(),
        )
    {
        return ServiceError::Cli {
            code: code.to_owned(),
            message: message.to_owned(),
            data: value
                .get("data")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        };
    }
    let stderr = String::from_utf8_lossy(stderr);
    ServiceError::Cli {
        code: "cli_failed".into(),
        message: stderr
            .lines()
            .next()
            .filter(|line| !line.is_empty())
            .unwrap_or("CLI service failed without an error message")
            .to_owned(),
        data: serde_json::Value::Null,
    }
}

#[cfg(test)]
#[test]
fn cli_failure_uses_stderr_without_structured_json() {
    let error = cli_failure(b"not json", b"specific failure\nadditional detail\n");
    assert!(matches!(error, ServiceError::Cli { code, message, data }
        if code == "cli_failed" && message == "specific failure" && data.is_null()));
}
impl F3Facade for BoundFacade<'_> {
    fn inbox(
        &mut self,
        input: provenant_fabric_runtime::inbox::InboxInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<
        provenant_fabric_runtime::inbox::InboxResult,
        provenant_fabric_runtime::inbox::InboxError,
    > {
        let ctx = &self.factory.context;
        provenant_fabric_runtime::inbox::execute(
            &ctx.roots.state,
            &ctx.identity,
            &ctx.config,
            ctx.in_lane,
            &self.factory.clock,
            input,
            budget,
            cancel,
            &|work, call| {
                provenant_cli::inbox::resolve_work(
                    &ctx.roots.state,
                    ctx.identity.project,
                    work,
                    call.clock,
                    call.budget,
                    call.cancel,
                )
            },
        )
    }
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
            chair: Some(provenant_run::ChairBinding {
                extra: Default::default(),
                host: self.factory.context.identity.inbox.host,
                actor: self.factory.context.identity.inbox.actor,
            }),
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
            chair: None,
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
            chair: None,
        }
        .output(input, budget, cancel)
    }
    fn hosts(
        &mut self,
        input: HostsInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<HostHealth>, FacadeError> {
        let HostsInput::Hosts { after, limit, .. } = input;
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
                            chair: None,
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
                    PeerAction::Health {
                        extra: Default::default(),
                    },
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
                        result: PeerResult::Health { data: value, .. },
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
        }
        context.identity.inbox = context.inbox_address(client)?;
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
        cwd: resolved.project.clone(),
        assets: resolved.assets,
        host_launch: None,
        mcp_project: globals.project.as_ref().map(|_| resolved.project),
        workers,
    })
}
pub fn hosts(globals: &provenant_cli::Globals, workers: &'static dyn WorkerObserver) -> u8 {
    let result = (|| -> Result<Vec<HostHealth>, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        Ok(BoundFacade { factory: &factory }.hosts(
            HostsInput::Hosts {
                extra: Default::default(),
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
fn peer_read_only(action: &PeerAction) -> bool {
    use provenant_fabric_runtime::inbox::InboxInput;
    match action {
        PeerAction::ConsoleBoard { .. } | PeerAction::ConsoleTree { .. } => true,
        PeerAction::Inbox { input, .. } => !matches!(
            input,
            InboxInput::Answer {
                operation_id: Some(_),
                ..
            }
        ),
        _ => false,
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
                            extra: Default::default(),
                            version: 1,
                            protocol: request.protocol,
                            host: request.target_host,
                            project: request.project.key,
                            result: PeerResult::Error {
                                data: provenant_fabric_runtime::hosts::PeerResultError {
                                    extra: Default::default(),
                                    error: FacadeError::Busy,
                                },
                                extra: Default::default(),
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
        // An answer without an operation ID is refused below, after peer validation,
        // and must not initialise writable project state on its way there.
        let mut context = if peer_read_only(&request.action) {
            Context::open_read_only(resolved.roots, &home, &cwd)?
        } else {
            Context::open(resolved.roots, &home, &cwd)?
        };
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
            mcp_project: None,
            workers,
        };
        let context = &peer_factory.context;
        let mut owner = BinaryRunOwner {
            factory: &peer_factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let result = match &request.action {
            PeerAction::Inbox { input, .. } => {
                use provenant_fabric_runtime::inbox::{InboxInput, InboxReply};
                if matches!(
                    input,
                    InboxInput::Close { .. } | InboxInput::Acknowledge { .. }
                ) {
                    PeerResult::Inbox {
                        data: InboxReply::Error {
                            extra: Default::default(),
                            code: "owner_local_only".into(),
                            message: format!(
                                "inbox owner {}: run this operation on the owner host or from the owning chair session",
                                context.identity.host
                            ),
                        },
                        extra: Default::default(),
                    }
                } else {
                    let result = if matches!(input, InboxInput::Ask { .. })
                        || (input.is_mutation() && input.operation_id().is_none())
                    {
                        Err(provenant_fabric_runtime::inbox::InboxError::Invalid(
                            "peer operation",
                        ))
                    } else {
                        provenant_fabric_runtime::inbox::execute(
                            &context.roots.state,
                            &context.identity,
                            &context.config,
                            false,
                            &clock,
                            input.clone(),
                            budget,
                            &cancel,
                            &|work, call| {
                                provenant_cli::inbox::resolve_work(
                                    &context.roots.state,
                                    context.identity.project,
                                    work,
                                    call.clock,
                                    call.budget,
                                    call.cancel,
                                )
                            },
                        )
                    };
                    PeerResult::Inbox {
                        data: match result {
                            Ok(result) => InboxReply::Ok {
                                extra: Default::default(),
                                result: Box::new(result),
                            },
                            Err(error) => InboxReply::Error {
                                extra: Default::default(),
                                code: error.code().into(),
                                message: error.to_string(),
                            },
                        },
                        extra: Default::default(),
                    }
                }
            }
            PeerAction::ConsoleBoard {
                all,
                limit,
                selected,
                ..
            } => {
                let command = provenant_cli::console::Console::Board {
                    all: *all,
                    limit: *limit,
                    cursor: None,
                    hosts: None,
                };
                if *limit > context.config.limits.page_rows {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput,
                        },
                        extra: Default::default(),
                    }
                } else {
                    match provenant_cli::console::read(
                        &context.roots,
                        selected.then_some(context.identity.project),
                        &command,
                    ) {
                        Ok(value) => PeerResult::Console {
                            data: value,
                            extra: Default::default(),
                        },
                        Err(_) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error: FacadeError::StateCorrupt,
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
            PeerAction::ConsoleTree {
                max_depth,
                max_nodes,
                detail,
                selected,
                ..
            } => {
                let command = provenant_cli::console::Console::Tree {
                    max_depth: *max_depth,
                    max_nodes: *max_nodes,
                    detail: *detail,
                    hosts: None,
                };
                if *max_nodes > context.config.limits.page_rows {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput,
                        },
                        extra: Default::default(),
                    }
                } else {
                    match provenant_cli::console::read(
                        &context.roots,
                        selected.then_some(context.identity.project),
                        &command,
                    ) {
                        Ok(value) => PeerResult::Console {
                            data: value,
                            extra: Default::default(),
                        },
                        Err(_) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error: FacadeError::StateCorrupt,
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
            PeerAction::RunStatuses { targets, .. } => {
                if !(1..=100).contains(&targets.len()) {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput,
                        },
                        extra: Default::default(),
                    }
                } else {
                    peer_result(owner.snapshots_local(targets, budget, &cancel), |data| {
                        PeerResult::RunStatuses {
                            data,
                            extra: Default::default(),
                        }
                    })
                }
            }
            PeerAction::Run { input, .. } => {
                peer_result(owner.run(input.clone(), budget, &cancel), |value| {
                    PeerResult::Run {
                        data: Box::new(value),
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::Submit { input, .. } => {
                peer_result(owner.submit(input.clone(), budget, &cancel), |data| {
                    PeerResult::Submitted {
                        data,
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::Output { input, .. } => {
                peer_result(owner.output(input.clone(), budget, &cancel), |data| {
                    PeerResult::Output {
                        data,
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::RunEvents { after, limit, .. } => {
                peer_result(owner.events(*after, *limit, budget, &cancel), |data| {
                    PeerResult::RunEvents {
                        data,
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::Lanes { all, limit, .. } => {
                peer_result(owner.lanes_local(*all, *limit, budget, &cancel), |data| {
                    PeerResult::RunStatuses {
                        data,
                        extra: Default::default(),
                    }
                })
            }
            PeerAction::LaneWaits { targets, .. } => {
                if !(1..=100).contains(&targets.len()) {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::InvalidInput,
                        },
                        extra: Default::default(),
                    }
                } else {
                    peer_result(owner.waits_local(targets, budget, &cancel), |data| {
                        PeerResult::LaneWaits {
                            data,
                            extra: Default::default(),
                        }
                    })
                }
            }
            PeerAction::SessionHandoff { input, .. } => PeerResult::SessionHandoff {
                data: crate::handoff::receive(
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
                ),
                extra: Default::default(),
            },
            PeerAction::Health { .. } => PeerResult::Health {
                data: provenant_fabric_runtime::hosts::PeerHealth {
                    extra: Default::default(),
                    host: context.identity.host,
                    available: true,
                    execution: Some(owner.capabilities().submit),
                },
                extra: Default::default(),
            },
            PeerAction::LandingPush {
                proof,
                operation_id,
                reconcile,
                ..
            } => {
                if context
                    .config
                    .home_hosts
                    .get(request.project.key.as_str())
                    .is_some_and(|h| *h != context.identity.host)
                {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::HomeUnavailable,
                        },
                        extra: Default::default(),
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
                        Ok(value) => PeerResult::Push {
                            data: value,
                            extra: Default::default(),
                        },
                        Err(error) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error: landing_error(error.as_ref()),
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
            PeerAction::Receipt { operation_id, .. } => {
                let store_request = provenant_fabric_store::Request {
                    clock: &clock,
                    budget,
                    cancel: &cancel,
                    backoff: &backoff,
                };
                let store =
                    provenant_fabric_store::Store::open(&context.roots.state, &store_request)?;
                match store.coordination_receipt_with_input::<Mutation, serde_json::Value>(
                    context.identity.project,
                    request.actor()?,
                    operation_id,
                    &store_request,
                )? {
                    Some((input, value)) => peer_result(
                        CoordinationResult::from_stored(&input, value)
                            .map_err(|_| FacadeError::StateCorrupt),
                        |data| PeerResult::Coordination {
                            data,
                            extra: Default::default(),
                        },
                    ),
                    None => PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::LaunchUnknown {
                                operation_id: operation_id.clone(),
                            },
                        },
                        extra: Default::default(),
                    },
                }
            }
            PeerAction::Coordination {
                input,
                operation_id,
                ..
            } => {
                // A peer never recursively forwards or elects another home.
                if context
                    .config
                    .home_hosts
                    .get(request.project.key.as_str())
                    .is_some_and(|h| *h != context.identity.host)
                {
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError {
                            extra: Default::default(),
                            error: FacadeError::HomeUnavailable,
                        },
                        extra: Default::default(),
                    }
                } else {
                    let coverage = if matches!(
                        input,
                        CoordinationInput::Events {
                            input: EventsInput::Read { .. },
                            extra: _
                        }
                    ) {
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
                                    if let CoordinationResult::Events {
                                        result:
                                            EventsResult::Read {
                                                unknown_hosts,
                                                run_events_pending,
                                                ..
                                            },
                                        ..
                                    } = &mut value
                                    {
                                        unknown_hosts.extend(coverage.unknown_hosts);
                                        *run_events_pending |= coverage.more_events;
                                    }
                                    PeerResult::Coordination {
                                        data: value,
                                        extra: Default::default(),
                                    }
                                }
                                Err(error) => PeerResult::Error {
                                    data: provenant_fabric_runtime::hosts::PeerResultError {
                                        extra: Default::default(),
                                        error: FacadeError::from(&error),
                                    },
                                    extra: Default::default(),
                                },
                            }
                        }
                        Err(error) => PeerResult::Error {
                            data: provenant_fabric_runtime::hosts::PeerResultError {
                                extra: Default::default(),
                                error,
                            },
                            extra: Default::default(),
                        },
                    }
                }
            }
        };
        let response = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: request.protocol,
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
        extra: Default::default(),
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
                        extra: Default::default(),
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
                    PeerResult::Push { data: value, .. } if value.id == recovery_id => Ok(value),
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
                        ..
                    } => {
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
            let value = serde_json::to_value(&data).unwrap_or_default();
            let human = provenant_cli::terse(&value);
            let summary = graph_summary(&value);
            crate::emit_summarised(
                &provenant_records::CommandResult::new(
                    command,
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                matches!(command, "fabric graph status"),
                Some(&summary),
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
    /// Only the direct chair entry point supplies a verified origin.
    chair: Option<provenant_run::ChairBinding>,
}
fn peer_result<T>(
    result: Result<T, FacadeError>,
    wrap: impl FnOnce(T) -> PeerResult,
) -> PeerResult {
    match result {
        Ok(value) => wrap(value),
        Err(error) => PeerResult::Error {
            data: provenant_fabric_runtime::hosts::PeerResultError {
                extra: Default::default(),
                error,
            },
            extra: Default::default(),
        },
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
            extra: Default::default(),
            worktree: None,
            operation_id,
            targets: page
                .lanes
                .iter()
                .map(|lane| AttemptTarget {
                    extra: Default::default(),
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
                    extra: Default::default(),
                    input: RunInput::Status {
                        extra: Default::default(),
                        target,
                    },
                },
                budget,
                cancel,
            )? {
                PeerResult::Run { data: value, .. } => match *value {
                    RunResult::Status { attempt, .. } if attempt.attempt_id == attempt_id => {
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
                    .status_for_wait(target.attempt, &call)
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
            .lanes_for_wait(
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
        self.submit_inner(
            input,
            budget,
            cancel,
            provenant_records::run::LaneAccess::default(),
        )
        .map_err(|error| scope_io(error, "submit"))
    }
    /// Graph nodes use the same validated, sealed access grants as CLI lanes.
    fn submit_with_access(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        access: provenant_records::run::LaneAccess,
    ) -> Result<SubmissionReceipt, FacadeError> {
        self.submit_inner(input, budget, cancel, access)
            .map_err(|error| scope_io(error, "submit"))
    }
    fn submit_inner(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        access: provenant_records::run::LaneAccess,
    ) -> Result<SubmissionReceipt, FacadeError> {
        use provenant_cli::run::{Submission, Submitted, derived_task, worktree_access};
        let SubmitInput::Submit {
            operation_id,
            tasks,
            concurrency,
            knobs,
            host,
            ..
        } = input;
        // Explicit placement forwards the whole batch; the peer confines it.
        if let Some(host) = host.filter(|h| *h != self.factory.context.identity.host) {
            if !access.is_default() {
                return Err(FacadeError::ConfinementRefused);
            }
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
                extra: Default::default(),
                operation_id: operation_id.clone(),
                tasks,
                concurrency,
                knobs,
                host: None,
            };
            return match self.peer(
                host,
                PeerAction::Submit {
                    extra: Default::default(),
                    input,
                },
                budget,
                cancel,
            )? {
                PeerResult::Submitted { data: receipt, .. }
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
                build_heavy: task.build_heavy,
                base: task.base,
                head: task.head,
                task_id: derived_task(&operation_id, index).map_err(fail_error)?,
                prompt: task.prompt,
                title: task.title,
                account: task.account,
                route: task.route,
                fallback: Vec::new(),
                provider_timeout: task.timeout_seconds,
                delivery_id: task.delivery_id,
                deliver: task.deliver,
                worktree: task.worktree.map(PathBuf::from),
                access: access.clone(),
            });
        }
        let session = self.session(budget, cancel)?;
        let call = self.call(budget, cancel);
        let (run, started) = self.with_edge(|edge| {
            provenant_cli::run::submit_with_host(
                &session,
                edge,
                Submission {
                    chair: self.chair.clone(),
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
            extra: Default::default(),
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
                extra: Default::default(),
                input: input.clone(),
            },
            budget,
            cancel,
        )? {
            return match answer {
                PeerResult::Output { data: value, .. } => Ok(value),
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
                        extra: Default::default(),
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
                    extra: Default::default(),
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
                            extra: Default::default(),
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
            RunInput::Status { target, .. } => Ok(RunResult::Status {
                extra: Default::default(),
                attempt: self.snapshot(target, budget, cancel)?,
            }),
            RunInput::Wait {
                targets,
                condition,
                wait_seconds,
                ..
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
                                extra: Default::default(),
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
                                    .policy_tail(targets[*index].attempt, &local_call)
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
                            PeerAction::RunStatuses {
                                extra: Default::default(),
                                targets: requested,
                            },
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
                                        extra: Default::default(),
                                        condition,
                                        attempts,
                                        satisfied: false,
                                        timed_out: true,
                                    }),
                                    _ => Err(error),
                                };
                            }
                        };
                        let PeerResult::RunStatuses { data: records, .. } = result else {
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
                            extra: Default::default(),
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
                ..
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
                        extra: Default::default(),
                        input: input.clone(),
                    },
                    budget,
                    cancel,
                )? {
                    return match answer {
                        PeerResult::Run { data: value, .. }
                            if matches!(&*value, RunResult::Cancel { attempt, .. }
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
                    extra: Default::default(),
                    attempt: service.status(target.attempt, &call).map_err(run_error)?,
                })
            }
            RunInput::Resume {
                ref target,
                ref operation_id,
                ref prompt,
                expected_generation,
                no_fallback,
                ..
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
                        extra: Default::default(),
                        input: input.clone(),
                    },
                    budget,
                    cancel,
                )? {
                    return match answer {
                        PeerResult::Run { data: value, .. }
                            if matches!(&*value, RunResult::Resume { attempt, .. }
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
                    extra: Default::default(),
                    attempt: session
                        .service()
                        .status(attempt.attempt, &call)
                        .map_err(run_error)?,
                })
            }
            RunInput::List { after, limit, .. } => {
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
                            newest_first: true,
                            owner: None,
                            unseen_after: None,
                        },
                        &call,
                    )
                    .map_err(run_error)?;
                let truncated = page.next.is_some();
                let next_after = if truncated {
                    page.lanes.last().map(|lane| AttemptTarget {
                        extra: Default::default(),
                        attempt: lane.attempt_id,
                        host: Some(host),
                    })
                } else {
                    None
                };
                Ok(RunResult::List {
                    extra: Default::default(),
                    page: AttemptPage {
                        extra: Default::default(),
                        items: page.lanes.into_iter().map(Into::into).collect(),
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
            extra: Default::default(),
            items,
            cursor,
            truncated,
        })
    }
}

struct BoundGraphOwner<'a> {
    factory: &'a Factory,
    integration_base: Option<provenant_core::GitOid>,
    integration_branch: Option<String>,
    budget: WaitBudget,
    cancel: &'a CancelToken,
    prices: Option<Result<provenant_telemetry::PriceTable, String>>,
}

/// The inbox request for a stored graph ask. The checkpoint keeps the rendered
/// text, so a replay sends the identical request under the same operation.
fn graph_ask_input(
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
) -> Result<provenant_fabric_runtime::inbox::InboxInput, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    use provenant_records::inbox::{RequestDraft, RequestKind, RequestOption};
    let ask = scope.ask;
    let operation = provenant_records::CanonicalV1::record_digest(
        "graph-node-ask",
        1,
        &(scope.graph_id, scope.instance_id, &ask.ask_id, &ask.session),
    )
    .map_err(|_| FacadeError::StateCorrupt)?;
    let operation_id = provenant_core::OperationId::parse(&format!(
        "ask-{}",
        provenant_core::hex(operation.as_bytes())
    ))
    .map_err(|_| FacadeError::StateCorrupt)?;
    let request = RequestDraft {
        extra: Default::default(),
        supersedes: None,
        deadline: ask.wake_at,
        work: None,
        kind: RequestKind::Decision,
        title: ask.title.clone(),
        body: ask.body.clone(),
        blocking: true,
        options: ask
            .options
            .iter()
            .map(|option| RequestOption {
                extra: Default::default(),
                id: option.id.clone(),
                label: option.label.clone(),
                description: option.description.clone(),
            })
            .collect(),
        recommended_option_id: ask.default.clone(),
        proceeding_with: None,
    };
    Ok(InboxInput::Ask {
        extra: Default::default(),
        session: ask.session.clone(),
        request: Box::new(request),
        operation_id: Some(operation_id),
    })
}

fn graph_ask_poll(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    use provenant_records::inbox::RequestState;
    let inbox_error =
        |error: provenant_fabric_runtime::inbox::InboxError| FacadeError::DispatchRefused {
            reason: format!("graph ask {}: {error}", scope.ask.ask_id),
        };
    // The immutable ask and its operation receipt commit together. A crash before
    // saving graph state replays this exact request, including its original ID.
    let InboxResult::Saved { receipt } = service
        .execute(graph_ask_input(scope)?, budget, cancel)
        .map_err(inbox_error)?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    let InboxResult::Item { item, .. } = service
        .execute(
            InboxInput::List {
                extra: Default::default(),
                id: Some(receipt.request_id),
                session: None,
                state: InboxFilter::All,
                limit: 1,
                cursor: None,
            },
            budget,
            cancel,
        )
        .map_err(inbox_error)?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    if item.request.state != RequestState::Answered {
        return Ok(None);
    }
    let Some(answer) = item.answer else {
        return Ok(None);
    };
    // Acknowledgement precedes action. The immutable answer stays readable after
    // acknowledgement, so a failed graph checkpoint save loses no decision.
    service
        .execute(
            InboxInput::Acknowledge {
                extra: Default::default(),
                id: receipt.request_id,
                answer_id: answer.id,
                session: scope.ask.session.clone(),
                operation_id: Some(
                    provenant_core::OperationId::parse(&format!("ask-ack-{}", answer.id))
                        .map_err(|_| FacadeError::StateCorrupt)?,
                ),
            },
            budget,
            cancel,
        )
        .map_err(inbox_error)?;
    // The graph runtime reads the option or reply text against the ask it stored.
    Ok(Some(provenant_fabric_runtime::graph::AskReply {
        option: answer.option_id,
        text: answer.text,
        actor: answer.actor.to_string(),
        at: Some(answer.answered_at),
    }))
}

fn graph_ask_edge(
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
    scope: provenant_fabric_runtime::graph::AskScope<'_>,
    close: bool,
) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxService;
    let context = &factory.context;
    let mut identity = context.identity.clone();
    // Settlement can run under another client label. Keep the actor saved
    // before the first ask, just as an attempt retains its launch actor.
    identity.inbox.actor = scope.actor.parse().map_err(|_| FacadeError::StateCorrupt)?;
    let refused = |error: &dyn std::fmt::Display| FacadeError::DispatchRefused {
        reason: format!("graph ask {}: {error}", scope.ask.ask_id),
    };
    // Validate the ordinary inbox edge before opening local state. Graph
    // asks obey lane and configured home-host restrictions too.
    provenant_fabric_runtime::inbox::validate_request(
        &identity,
        &context.config,
        context.in_lane,
        &graph_ask_input(&scope)?,
    )
    .map_err(|error| refused(&error))?;
    let service = InboxService::open(
        &context.roots.state,
        &identity,
        &factory.clock,
        budget,
        cancel,
    )
    .map_err(|error| refused(&error))?;
    if close {
        graph_ask_close(&service, &scope, identity.inbox.actor, budget, cancel)?;
        Ok(None)
    } else {
        graph_ask_poll(&service, &scope, budget, cancel)
    }
}

fn graph_ask_close(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    actor: provenant_core::ActorId,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    use provenant_records::inbox::RequestState;
    let map = |e: provenant_fabric_runtime::inbox::InboxError| FacadeError::DispatchRefused {
        reason: format!("close graph ask {}: {e}", scope.ask.ask_id),
    };
    let InboxInput::Ask {
        operation_id: Some(operation),
        ..
    } = graph_ask_input(scope)?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    // Find the request through the normal bounded inbox list. Closing never creates an ask.
    let mut cursor = None;
    loop {
        let InboxResult::Listed {
            items, next_cursor, ..
        } = service
            .execute(
                InboxInput::List {
                    extra: Default::default(),
                    id: None,
                    session: Some(scope.ask.session.clone()),
                    state: InboxFilter::All,
                    limit: 50,
                    cursor,
                },
                budget,
                cancel,
            )
            .map_err(map)?
        else {
            return Err(FacadeError::StateCorrupt);
        };
        if let Some(item) = items
            .into_iter()
            .find(|item| item.operation_id == operation && item.chair_actor == actor)
        {
            if matches!(
                item.state,
                RequestState::Withdrawn | RequestState::Superseded | RequestState::Expired
            ) {
                return Ok(());
            }
            service
                .execute(
                    InboxInput::Close {
                        extra: Default::default(),
                        id: item.id,
                        state: RequestState::Superseded,
                        reason: "Graph decision resolved or cancelled".into(),
                        expect_revision: item.revision,
                        operation_id: Some(
                            provenant_core::OperationId::parse(&format!("ask-close-{}", item.id))
                                .map_err(|_| FacadeError::StateCorrupt)?,
                        ),
                    },
                    budget,
                    cancel,
                )
                .map_err(map)?;
            return Ok(());
        }
        let Some(next) = next_cursor else {
            return Ok(());
        };
        cursor = Some(next);
    }
}

fn load_graph_prices(assets: &std::path::Path) -> Result<provenant_telemetry::PriceTable, String> {
    let path = assets.join("config/telemetry-prices.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let config = toml_edit::de::from_str::<provenant_telemetry::ConfiguredPrices>(&text)
        .map_err(|error| format!("{}: invalid price table: {error}", path.display()))?;
    provenant_telemetry::validate_prices(&config)
        .map_err(|error| format!("{}: invalid price table: {error}", path.display()))
}

fn graph_attempt_usage(
    record: provenant_records::run::AttemptRecord,
    prices: Option<&provenant_telemetry::PriceTable>,
    cost_unknown_reason: Option<String>,
) -> provenant_fabric_runtime::graph::AttemptUsage {
    let usage = &record.usage;
    let categories = [
        usage.uncached_input,
        usage.cache_read,
        usage.cache_write,
        usage.output_excluding_reasoning,
        usage.reasoning,
    ];
    // Reasoning is counted inside output, so providers never report it apart.
    let tokens_partial = categories[..4].iter().any(Option::is_none);
    let tokens = categories.iter().any(Option::is_some).then(|| {
        categories
            .into_iter()
            .flatten()
            .fold(0u64, u64::saturating_add)
    });
    let cost = prices
        .and_then(|prices| {
            provenant_run::fact(record.clone()).ok().and_then(|fact| {
                provenant_telemetry::stats([fact], prices, Default::default()).ok()
            })
        })
        .map(|report| report.totals.cost);
    let cost_partial = cost.as_ref().is_none_or(|cost| cost.complete.is_none());
    let cost_micro_usd =
        cost.and_then(|cost| cost.known_lower_bound.map(|money| money.microdollars));
    provenant_fabric_runtime::graph::AttemptUsage {
        extra: Default::default(),
        tokens,
        cost_micro_usd,
        wall_time_seconds: record.provider_ms.map(|ms| ms.saturating_add(999) / 1000),
        tokens_partial,
        cost_partial,
        cost_unknown_reason,
    }
}

fn decode_graph_document(
    bytes: &[u8],
) -> Result<provenant_fabric_runtime::graph::GraphDocument, FacadeError> {
    provenant_records::decode_v1(bytes, provenant_records::Limits::default()).map_err(|error| {
        match error {
            provenant_records::DecodeError::UnsupportedVersion { found, .. } if found > 1 => {
                FacadeError::UpgradeRequired {
                    reader_release: env!("CARGO_PKG_VERSION").into(),
                    writer_release: "unknown".into(),
                    record: "graph document schema".into(),
                    version: found,
                }
            }
            _ => FacadeError::InvalidInput,
        }
    })
}

fn graph_journal_error(error: provenant_fabric_runtime::graph::JournalError) -> FacadeError {
    use provenant_fabric_runtime::graph::JournalError;
    match error {
        JournalError::Decode(provenant_records::DecodeError::UnsupportedVersion {
            found, ..
        }) if found > 1 => FacadeError::UpgradeRequired {
            reader_release: env!("CARGO_PKG_VERSION").into(),
            writer_release: "unknown".into(),
            record: "graph checkpoint schema".into(),
            version: found,
        },
        JournalError::TooLarge => FacadeError::DispatchRefused {
            reason: "graph checkpoint exceeds its byte limit".into(),
        },
        JournalError::Filesystem(_)
        | JournalError::Io(_)
        | JournalError::DurabilityUnconfirmed(_) => FacadeError::io("access graph journal"),
        _ => FacadeError::StateCorrupt,
    }
}

fn graph_result_warning(error: &provenant_run::RunError) -> &'static str {
    match error {
        provenant_run::RunError::ArtifactMismatch => {
            "artifact digest or length does not match its record"
        }
        provenant_run::RunError::NotFound => "result artifact is missing",
        provenant_run::RunError::Filesystem(source) if source.is_not_found() => {
            "result artifact is missing"
        }
        provenant_run::RunError::TooLarge { .. } => "result artifact exceeds the byte limit",
        _ => "result artifact could not be verified",
    }
}

pub(crate) fn advance_settled_graphs(
    holder: &provenant_cli::run::HostHolder<'_>,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(settled) = holder.settled()? else {
        return Ok(());
    };
    provenant_cli::run::launch_policy_successor(&settled).map_err(std::io::Error::other)?;
    advance_graphs_after_settlement(&settled, workers)
}

fn advance_graphs_after_settlement(
    settled: &provenant_run::HolderContext,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{FileJournal, advance_with_budget};

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
    let pressure_error = crate::disk_guard::settlement_gc(settled).err();
    let settlement_error = settle_owned_worktree(settled, &roots, workers, None).err();
    let settlement_reason = settlement_error.as_ref().map(|error| error.to_string());
    let mut first_error = settlement_error.or_else(|| pressure_error.map(Into::into));
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
                mcp_project: None,
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
                integration_branch: execution.document.integration_branch.clone(),
                budget,
                cancel: &cancel,
                prices: None,
            };
            let graph_config = &factory.context.config.graph;
            advance_with_budget(
                &execution.document,
                &mut owner,
                &mut journal,
                &graph_config.limits,
                &graph_config.budget,
            )?;
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
    if store.policy_tail(record.attempt_id, &call)?.attempt_id != record.attempt_id {
        // The successor owns this writer's remaining work and settlement.
        return Ok(());
    }
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
    if task.deliver.is_none()
        && delivery_override.is_none()
        && record.delivery.is_none()
        && store.delivered(record.attempt_id, &call)?.is_none()
    {
        let mut ownership_unknown = false;
        let ids = match provenant_fabric_runtime::graph::FileJournal::graph_ids(
            &roots.state,
            settled.project_id,
        ) {
            Ok(ids) => ids,
            Err(_) => {
                ownership_unknown = true;
                Vec::new()
            }
        };
        let mut graph_member = false;
        for id in ids {
            let checkpoint = match provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
                &roots.state,
                settled.project_id,
                &id,
            ) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => continue,
                Err(_) => {
                    ownership_unknown = true;
                    continue;
                }
            };
            for node in &checkpoint.nodes {
                for attempt in &node.attempts {
                    if attempt
                        .attempt_ref
                        .as_deref()
                        .and_then(|reference| reference.rsplit('/').next())
                        == Some(record.attempt_id.to_string().as_str())
                        || graph_operation("graph-attempt-operation", &attempt.key)
                            .is_ok_and(|operation| operation == record.operation_id)
                    {
                        graph_member = true;
                    }
                }
            }
            if graph_member {
                break;
            }
        }
        // Unknown graph custody retains graph operations conservatively; unrelated lanes still settle.
        if graph_member || ownership_unknown && record.operation_id.as_str().starts_with("graph-") {
            let reason = if graph_member {
                "graph writer worktree retained for continuation, later rounds and explicit delivery"
            } else {
                "graph custody could not be read; worktree retained for explicit delivery"
            };
            record_worktree_settlement(
                &store,
                record.attempt_id,
                &serde_json::json!({"schema_version":1,"state":"retained","worktree":writer.worktree,"reason":reason}),
            )?;
            return Ok(());
        }
    }
    // Refresh before taking the collection lock and before the prune budget starts.
    // Explicit worktrees have no owner record, so use their verified HEAD.
    let mut tracking_ref_warning = None;
    let mut lane_owned = false;
    let _ = crate::worktrees::with_context(
        roots,
        &settled.roots.home,
        &writer.project,
        std::time::Duration::from_secs(15),
        workers,
        |context, _| {
            use std::io::Read;
            let tree = provenant_confinement::verify_worktree(context.repo, &writer.worktree)
                .map_err(worktrees::WorktreeError::from)?;
            let owner = worktrees::load_owner(&tree)?;
            lane_owned = owner
                .as_ref()
                .is_some_and(|owner| owner.origin == WorktreeOrigin::Lane);
            let branch = if let Some(owner) = owner {
                Some(owner.branch)
            } else {
                let mut head = Vec::new();
                tree.repo()
                    .private_dir()
                    .open_read(std::ffi::OsStr::new("HEAD"))?
                    .take(4097)
                    .read_to_end(&mut head)
                    .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
                if head.len() > 4096 {
                    None
                } else {
                    std::str::from_utf8(&head)
                        .ok()
                        .and_then(|head| head.trim().strip_prefix("ref: refs/heads/"))
                        .map(str::to_owned)
                }
            };
            if let Some(branch) = branch
                && let Err(error) = crate::gc_support::refresh_writer_tracking_ref(context, &branch)
            {
                tracking_ref_warning = Some(error.to_string());
            }
            Ok(())
        },
    );
    if !lane_owned {
        tracking_ref_warning = None;
    }
    let _collection = match crate::worktrees::event_collection_lock(roots) {
        Ok(lock) => lock,
        // A long-running collector still owns the pass. Leave a visible
        // receipt rather than skipping silently; replay ignores retained ones.
        Err(crate::worktrees::Error::Fs(provenant_os::fs::FsError::LockTimeout { .. })) => {
            let receipt = serde_json::json!({"schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":crate::worktrees::COLLECTION_BUSY});
            // Never replace a receipt that already settled or began pruning.
            if store
                .worktree_settlement(record.attempt_id, &call)?
                .is_none()
            {
                record_worktree_settlement(&store, record.attempt_id, &receipt)?;
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
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
    let retention_reason =
        settlement_retention_reason(&record.state, task.deliver.is_some(), delivery);
    let mut pruning_intent = None;
    let collection_error = std::cell::RefCell::new(None::<String>);
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
                // Resumed attempts share the original owner's lane. Apply the
                // continuation proof before pruning against that owner's ID.
                if owner.origin == WorktreeOrigin::Lane
                    && store
                        .check_continuation_writer(record.attempt_id, &call)
                        .map_err(|_| worktrees::WorktreeError::UnknownWriter)?
                        != Some(owner.clone())
                {
                    return Err(worktrees::WorktreeError::UnknownWriter.into());
                }
                if owner.origin != WorktreeOrigin::Lane {
                    return Ok(None);
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
                let output_policy = WorktreeGcPolicy {
                    upstream: None,
                    terminal: &terminal,
                    no_live_lanes: &no_live,
                    regenerable: &settings.regenerable,
                    keep: &settings.keep,
                    written_before_ns: i128::MAX,
                    foreign_merged_clean: false,
                    foreign_idle_before_ns: None,
                    dry_run: false,
                };
                // A failed pre-prune step must not replace a retained lane's
                // real reason, so record it and carry on.
                let pre_prune = || -> Result<(), Box<dyn std::error::Error>> {
                    let lane = worktrees::list(context, cancel)?
                        .into_iter()
                        .find(|lane| lane.path == writer.worktree)
                        .ok_or(worktrees::WorktreeError::NotFound)?;
                    worktrees::collect(
                        context,
                        std::slice::from_ref(&lane),
                        &output_policy,
                        cancel,
                    )?;
                    // Retained lanes can resume with fresh caches. Restrict this
                    // pass to their owner, including detached remnants, and prove
                    // terminal state again while acquiring writer exclusion.
                    if lane.writer == worktrees::WriterState::Free {
                        provenant_confinement::cache::gc_worktree_cargo_caches_with_writer_guard(
                            &settled.roots.home,
                            &provenant_confinement::cache::CacheGcPolicy {
                                idle_before_ns: Some(i128::MAX),
                                project_owners: [writer.worktree.clone()].into(),
                                selected_only: true,
                                ..Default::default()
                            },
                            |path| {
                                let guard = worktrees::cache_writer_guard(
                                    context.binding,
                                    context.project_state,
                                    path,
                                    lane.branch.as_deref(),
                                )?;
                                if !terminal(path) || !no_live(path) {
                                    return Err(worktrees::WorkerState::Unknown);
                                }
                                Ok(guard)
                            },
                        )
                        .map_err(|error| crate::worktrees::Error::Permissions(*error.source))?;
                    }
                    Ok(())
                };
                if let Err(error) = pre_prune() {
                    *collection_error.borrow_mut() = Some(error.to_string());
                }
                if retention_reason.is_some() {
                    return Ok(None);
                }
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
                    ..output_policy
                };
                let mut intent = serde_json::json!({
                    "schema_version":1, "state":"pruning", "worktree":writer.worktree,
                    "branch":format!("refs/heads/{}", owner.branch)
                });
                if let Some(warning) = tracking_ref_warning.as_ref() {
                    intent["tracking_ref_warning"] = serde_json::json!(warning);
                }
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
    let mut receipt = match result {
        Ok(None) => {
            let reason = match retention_reason {
                Some(reason) => reason,
                None => return Ok(()),
            };
            let reason = match collection_error.take() {
                Some(error) => format!("{reason}; collection failed: {error}"),
                None => reason.to_owned(),
            };
            serde_json::json!({"schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":reason})
        }
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
    if let Some(warning) = tracking_ref_warning {
        receipt["tracking_ref_warning"] = serde_json::json!(warning);
    }
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
    // The tree is kept on purpose without a Delivered receipt (e.g. NoNewCommits); reclaimed via gc or disk reclaim.
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

impl BoundGraphOwner<'_> {
    /// Model families that served the writer attempts a reviewer must differ from.
    fn writer_families(&self, attempts: &[String]) -> Result<Vec<String>, FacadeError> {
        if attempts.is_empty() {
            return Ok(Vec::new());
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let mut families = Vec::new();
        for reference in attempts {
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            let record = owner.snapshot(target, self.budget, self.cancel)?;
            let family = record
                .applied_route
                .and_then(|route| route.model)
                .and_then(|model| provenant_cli::run::model_family(&model));
            if let Some(family) = family
                && !families.contains(&family)
            {
                families.push(family);
            }
        }
        Ok(families)
    }
}

impl provenant_fabric_runtime::graph::CiStatus for BoundGraphOwner<'_> {
    fn host_stamp(&self) -> Option<provenant_fabric_runtime::graph::CiHostStamp> {
        Some(provenant_fabric_runtime::graph::CiHostStamp {
            extra: Default::default(),
            boot_identity: self.factory.clock.boot_identity().ok()?,
            host_monotonic_micros: self.factory.clock.host_monotonic_micros().ok()?,
        })
    }
    fn observe_ci(
        &mut self,
        target: &str,
        checks: &[String],
    ) -> provenant_fabric_runtime::graph::CiObservation {
        crate::pr::status::observe_ci(&self.factory.cwd, target, checks)
    }
}

impl provenant_fabric_runtime::graph::AttemptOwner for BoundGraphOwner<'_> {
    type Error = FacadeError;
    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        let branch =
            self.integration_branch
                .as_deref()
                .ok_or_else(|| FacadeError::DispatchRefused {
                    reason: "an integration source requires graph.integration_branch".into(),
                })?;
        let reference = match branch.strip_prefix("origin/") {
            Some(branch) => format!("refs/remotes/origin/{branch}"),
            None => format!("refs/heads/{branch}"),
        };
        let git = provenant_os::git::git_executable().ok_or(FacadeError::InvalidInput)?;
        provenant_run::resolve_git_base(&git, &self.factory.cwd, &reference)
            .map(Some)
            .map_err(run_error)
    }
    fn ci_status(&mut self) -> Option<&mut dyn provenant_fabric_runtime::graph::CiStatus> {
        Some(self)
    }
    fn actor(&self) -> String {
        self.factory.context.identity.inbox.actor.to_string()
    }
    fn validate_expansion(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        _checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<(), Self::Error> {
        for node in &document.nodes {
            if let Some(provenant_records::run::DeliveryMode::Integrate(target)) = &node.delivery {
                provenant_run::HostDeliveryContext::validate_integration_target(
                    &self.factory.cwd,
                    target,
                )
                .map_err(run_error)?;
            }
        }
        Ok(())
    }
    fn node_ask(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, Self::Error> {
        graph_ask_edge(self.factory, self.budget, self.cancel, scope, false)
    }
    fn close_node_ask(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<(), Self::Error> {
        graph_ask_edge(self.factory, self.budget, self.cancel, scope, true).map(|_| ())
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        self.factory.clock.wall_now().ok()
    }
    fn cost_unknown_reason(&mut self) -> Option<String> {
        let graph_budget = &self.factory.context.config.graph.budget;
        if graph_budget.per_graph.cost_usd.is_none() && graph_budget.per_node.cost_usd.is_none() {
            return None;
        }
        self.prices
            .get_or_insert_with(|| load_graph_prices(&self.factory.assets))
            .as_ref()
            .err()
            .cloned()
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
            chair: None,
        }
        .session(self.budget, self.cancel)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            budget: self.budget,
            cancel: self.cancel,
        };
        let mut body = format!("Graph {}\n\n", document.graph_id);
        for node in &checkpoint.nodes {
            if let Some(summary) = node.deterministic_summary() {
                body.push_str(&summary);
                continue;
            }
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
    fn usage(
        &mut self,
        attempt_ref: &str,
    ) -> Result<provenant_fabric_runtime::graph::AttemptUsage, Self::Error> {
        let (host, attempt) = attempt_ref
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
        };
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let record = owner.snapshot(target, self.budget, self.cancel)?;
        let cost_unknown_reason = self.cost_unknown_reason();
        let prices = self
            .prices
            .get_or_insert_with(|| load_graph_prices(&self.factory.assets));
        Ok(graph_attempt_usage(
            record,
            prices.as_ref().ok(),
            cost_unknown_reason,
        ))
    }

    fn observe_or_start(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
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
            chair: None,
        };
        let (target, reference) = if let Some(reference) = replay.stored_ref {
            // An accepted attempt is observed by reference: operation replay is
            // actor-scoped, so resubmitting under another actor would launch again.
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            (target, Some(reference.to_owned()))
        } else {
            // Replaying an accepted operation must observe its existing writer,
            // even after that writer has advanced beyond the pinned start.
            owner.actor = Some(
                replay
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            );
            let operation_id = graph_operation("graph-attempt-operation", key)?;
            let accepted = owner
                .store(self.budget, self.cancel)?
                .operation(&operation_id, &owner.call(self.budget, self.cancel))
                .map_err(run_error)?
                .is_some_and(|operation| {
                    operation.state != provenant_run::OperationState::Prepared
                });
            if !accepted && let (Some(path), Some(commit)) = (worktree, node.source_commit) {
                let head = commit.parse().map_err(|_| FacadeError::InvalidInput)?;
                provenant_confinement::worktrees::with_context(
                    &self.factory.context.roots,
                    &self.factory.home,
                    &self.factory.cwd,
                    &self.factory.clock,
                    self.cancel,
                    |context| {
                        provenant_confinement::worktrees::fast_forward(
                            context,
                            Path::new(path),
                            head,
                            self.cancel,
                        )
                    },
                )
                .map_err(|error| FacadeError::DispatchRefused {
                    reason: format!("initialise graph writer at {commit}: {error}"),
                })?;
            }
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
            let excluded_families = self.writer_families(node.distinct_from_attempts)?;
            let input = SubmitInput::Submit {
                extra: Default::default(),
                operation_id,
                tasks: vec![SubmittedTask {
                    extra: Default::default(),
                    build_heavy: false,
                    base: node.source_commit.map(|head| {
                        self.integration_base
                            .map_or_else(|| head.to_owned(), |base| base.to_string())
                    }),
                    head: node.source_commit.map(str::to_owned),
                    account: route.account.clone(),
                    access,
                    worktree: worktree.map(str::to_owned),
                    prompt: node.prompt.into(),
                    title: Some(node.title.to_owned()),
                    route: match &route.model {
                        Some(model) => {
                            let adapter =
                                route.adapter.as_ref().ok_or(FacadeError::InvalidInput)?;
                            provenant_records::run::RequestedRoute::Model {
                                extra: Default::default(),
                                provider: adapter.clone(),
                                model: if model.contains('/') {
                                    model.clone()
                                } else {
                                    format!("{adapter}/{model}")
                                },
                                effort: route.effort.clone(),
                            }
                        }
                        None if !excluded_families.is_empty() => {
                            provenant_records::run::RequestedRoute::Review {
                                extra: Default::default(),
                                name: route.alias.clone().ok_or(FacadeError::InvalidInput)?,
                                effort: route.effort.clone(),
                                excluded_families,
                                distinct_families: true,
                            }
                        }
                        None => provenant_records::run::RequestedRoute::Named {
                            extra: Default::default(),
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
            let grants = provenant_cli::run::lane_access(
                &provenant_cli::Access {
                    mode: provenant_cli::Mode::ReadOnly,
                    worktree: None,
                    read_root: node
                        .read_roots
                        .iter()
                        .map(|root| {
                            let path = Path::new(root);
                            if path.is_absolute() {
                                path.to_path_buf()
                            } else {
                                self.factory.cwd.join(path)
                            }
                        })
                        .collect(),
                    write_root: Vec::new(),
                    network: None,
                    capability: Vec::new(),
                },
                self.factory.context.roots.state.as_path(),
            )
            .map_err(fail_error)?;
            let receipt = owner.submit_with_access(input, self.budget, self.cancel, grants)?;
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
        let mut attempt = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        let mut reference = reference;
        if target
            .host
            .is_none_or(|host| host == self.factory.context.identity.host)
        {
            let store = owner.store(self.budget, self.cancel)?;
            attempt = store
                .policy_tail(attempt.attempt_id, &owner.call(self.budget, self.cancel))
                .map_err(run_error)?;
            reference = Some(format!(
                "{}/{}",
                self.factory.context.identity.host, attempt.attempt_id
            ));
        }
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
                let (result, result_warning) = if attempt.result.is_some()
                    || outcome == Outcome::Succeeded
                {
                    match store.stream_result(attempt.attempt_id, &call, node.required_verdict) {
                        Ok(result) => (Some(result), None),
                        Err(error) => (None, Some(graph_result_warning(&error).to_owned())),
                    }
                } else {
                    (None, None)
                };
                // An unverifiable artifact is an integrity failure regardless
                // of the provider outcome. In particular, it must not trigger
                // route fallback for authentication or quota failures.
                Ok(AttemptObservation::Terminal {
                    launched,
                    attempt_ref: reference,
                    outcome: if result_warning.is_some() {
                        AttemptOutcome::OtherFailure
                    } else {
                        match outcome {
                            Outcome::Succeeded
                                if effective_delivery.is_some_and(|delivery| {
                                    matches!(
                                    delivery.state,
                                    provenant_records::run::WriterDeliveryState::Failed { .. }
                                        | provenant_records::run::WriterDeliveryState::NotAttempted { .. }
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
                        }
                    },
                    result,
                    result_warning,
                    result_path: attempt.result.as_ref().and_then(|_| {
                        (target.host.unwrap_or(self.factory.context.identity.host)
                            == self.factory.context.identity.host)
                            .then(|| {
                                self.factory
                                    .context
                                    .roots
                                    .state
                                    .as_path()
                                    .join(format!(
                                        "runs/{}/attempts/{}/result.md",
                                        attempt.run_id, attempt.attempt_id
                                    ))
                                    .to_string_lossy()
                                    .into_owned()
                            })
                    }),
                    branch: effective_delivery.map(|receipt| receipt.branch.clone()),
                    commit: if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                    {
                        attempt.head.map(|head| head.to_string())
                    } else {
                        node.source_commit.map(str::to_owned)
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
        use provenant_fabric_runtime::graph::{FileJournal, GraphJournal};
        input
            .validate_input(provenant_core::WaitTransport::Cli)
            .map_err(|_| FacadeError::InvalidInput)?;
        let resume = matches!(&input, GraphInput::Resume { .. });
        let document = match &input {
            GraphInput::Run { document, .. } | GraphInput::Resume { document, .. } => {
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
                    .and_then(|mut file| file.read_bounded(8_388_608))
                    .map_err(|_| FacadeError::io("access graph journal"))?;
                let document = decode_graph_document(&bytes)?;
                if let Some(branch) = &document.integration_branch {
                    provenant_run::HostDeliveryContext::validate_integration_target(
                        &self.cwd, branch,
                    )
                    .map_err(run_error)?;
                }
                let checkpoint = FileJournal::read_checkpoint(
                    &self.context.roots.state,
                    self.context.identity.project,
                    &document.graph_id,
                )
                .map_err(graph_journal_error)?;
                let admitted = checkpoint.is_some();
                let expanded = document
                    .expanded(&self.context.config.graph.limits)
                    .map_err(|reason| FacadeError::DispatchRefused { reason })?;
                for node in &expanded.nodes {
                    if let Some(provenant_records::run::DeliveryMode::Integrate(target)) =
                        &node.delivery
                    {
                        provenant_run::HostDeliveryContext::validate_integration_target(
                            &self.cwd, target,
                        )
                        .map_err(run_error)?;
                    }
                    if !admitted
                        && node.delivery == Some(provenant_records::run::DeliveryMode::Push)
                    {
                        let path = node.worktree.as_deref().ok_or(FacadeError::InvalidInput)?;
                        let writer = provenant_run::resolve_writer(
                            self.context.roots.state.as_path(),
                            &self.cwd,
                            Path::new(path),
                        )
                        .map_err(run_error)?;
                        let writer = provenant_run::prepare_writer(
                            &writer,
                            self.context.roots.state.as_path(),
                            self.context.identity.project,
                        )
                        .map_err(run_error)?;
                        provenant_run::HostDeliveryContext::check_delivery_branch(
                            &self.cwd,
                            writer.branch.as_str().trim_start_matches("refs/heads/"),
                        )
                        .map_err(|_| FacadeError::ConfinementRefused)?;
                    }
                }
                Some(document)
            }
            _ => None,
        };
        let id = match &input {
            GraphInput::Status { id, .. } | GraphInput::Cancel { id, .. } => id.as_str(),
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
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
            let mut run = graph_snapshot(checkpoint);
            let sidecar_error = FileJournal::read_advance_error(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            )
            .map_err(|_| FacadeError::io("access graph journal"))?;
            run.advance_error = sidecar_error.or(run.advance_error);
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
            let integration_base = if journal.load(id).map_err(graph_journal_error)?.is_some() {
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
                    extra: Default::default(),
                    integration_base,
                    directory: self.cwd.clone(),
                    document: document.clone(),
                })
                .map_err(|error| match error {
                    provenant_fabric_runtime::graph::JournalError::TooLarge => {
                        graph_journal_error(error)
                    }
                    _ => FacadeError::Conflict,
                })?;
            let mut owner = BoundGraphOwner {
                factory: self,
                integration_base,
                integration_branch: document.integration_branch.clone(),
                budget,
                cancel,
                prices: None,
            };
            let graph_config = &self.context.config.graph;
            let run = if resume {
                let extensions = match &input {
                    GraphInput::Resume { extend, .. } => extend.as_slice(),
                    _ => &[],
                };
                provenant_fabric_runtime::graph::resume_with_budget(
                    document,
                    &mut owner,
                    &mut journal,
                    &graph_config.limits,
                    &graph_config.budget,
                    extensions,
                )
            } else {
                provenant_fabric_runtime::graph::advance_with_budget(
                    document,
                    &mut owner,
                    &mut journal,
                    &graph_config.limits,
                    &graph_config.budget,
                )
            };
            let run = run.map_err(|error| match error {
                provenant_fabric_runtime::graph::GraphError::Run(error) => error,
                provenant_fabric_runtime::graph::GraphError::Invalid(_) => {
                    FacadeError::InvalidInput
                }
                provenant_fabric_runtime::graph::GraphError::Extension(reason) => {
                    FacadeError::DispatchRefused { reason }
                }
                provenant_fabric_runtime::graph::GraphError::PlanChanged => FacadeError::Conflict,
                provenant_fabric_runtime::graph::GraphError::Journal(error) => {
                    graph_journal_error(error)
                }
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
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
        if matches!(input, GraphInput::Cancel { .. }) {
            // Persist the admission fence before asking the run owner to stop live attempts.
            checkpoint.cancelled = true;
            journal.save(&checkpoint).map_err(graph_journal_error)?;
            let mut owner = BinaryRunOwner {
                factory: self,
                allow_remote: true,
                actor: None,
                chair: None,
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
            let mut graph_owner = BoundGraphOwner {
                factory: self,
                integration_base: None,
                integration_branch: None,
                budget,
                cancel,
                prices: None,
            };
            if provenant_fabric_runtime::graph::close_resolved_asks(
                &mut checkpoint,
                &mut graph_owner,
                &mut journal,
            )
            .is_err()
            {
                first_error.get_or_insert_with(|| FacadeError::io("close cancelled graph asks"));
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        Ok(graph_snapshot(checkpoint))
    }
}

fn graph_snapshot(
    mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
) -> provenant_fabric_runtime::graph::GraphRun {
    use provenant_fabric_runtime::graph::{GraphRun, GraphStatus, NodeState};
    let unfinished = checkpoint
        .nodes
        .iter()
        .any(|node| node.expansion_pause.is_some())
        || (checkpoint.delivery_required && !checkpoint.delivered && checkpoint.successful())
        || checkpoint
            .nodes
            .iter()
            .any(|node| matches!(node.state, NodeState::Pending | NodeState::Running));
    let status = if checkpoint.cancelled {
        GraphStatus::Cancelled
    } else if !unfinished {
        // A crash can leave a pause behind after the final node was saved.
        checkpoint.pause_reason = None;
        GraphStatus::Complete
    } else if checkpoint.pause_reason.is_some() {
        GraphStatus::Paused
    } else {
        GraphStatus::Pending
    };
    let advance_error = checkpoint
        .nodes
        .iter()
        .find_map(|node| node.advance_error.clone());
    GraphRun {
        status,
        checkpoint,
        advance_error,
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
                    extra: Default::default(),
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
                            extra: Default::default(),
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
                    extra: Default::default(),
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
/// Retain a content-addressed preset graph in the same verified scratch area
/// consumed by the graph owner. Repeated publication must match byte for byte.
pub(crate) fn retain_preset_graph(
    document: &serde_json::Value,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let factory = factory(globals, workers)?;
    if factory.context.in_lane {
        return Err(CoordinationError::LaneMutation.into());
    }
    let cancel = CancelToken::new();
    let scratch = provenant_confinement::worktrees::with_context(
        &factory.context.roots,
        &factory.home,
        &factory.cwd,
        &factory.clock,
        &cancel,
        |context| {
            let repo = context.utility.open_repo(
                context.executor,
                &factory.cwd,
                context.utility_budget,
                &cancel,
            )?;
            provenant_confinement::scratch::ensure(&repo, &factory.clock, &cancel)
        },
    )?;
    publish_preset_graph(&scratch, document)
}

pub(crate) fn publish_preset_graph(
    scratch: &std::path::Path,
    document: &serde_json::Value,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use provenant_os::fs::{Directory, PublishMode};
    use std::ffi::OsStr;
    let bytes = serde_json::to_vec(document)?;
    let digest = provenant_records::CanonicalV1::file_digest(&bytes).to_string();
    let filename = format!("preset-{}.json", &digest[7..]);
    let dir = Directory::open_anchor(&scratch.join("graphs"))?;
    let directory_metadata = dir.metadata()?;
    if directory_metadata.uid != provenant_os::fs::effective_uid()
        || directory_metadata.mode & 0o077 != 0
    {
        return Err("preset graph directory must be private to the current user".into());
    }
    match dir.atomic_write(
        OsStr::new(&filename),
        ".preset-",
        &bytes,
        PublishMode::NoReplace,
    ) {
        Ok(publication) => publication.require_durable()?,
        Err(error) if error.is_exists() => {
            let metadata = dir.metadata_at(OsStr::new(&filename), false)?;
            if metadata.uid != provenant_os::fs::effective_uid()
                || metadata.mode & 0o077 != 0
                || metadata.links != 1
                || dir.read_stable(OsStr::new(&filename), 8_388_608)? != bytes
            {
                return Err(
                    "existing preset graph is not private or does not match its content hash"
                        .into(),
                );
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(scratch.join("graphs").join(filename))
}

fn graph_human(run: &provenant_fabric_runtime::graph::GraphRun) -> String {
    let members: std::collections::BTreeSet<_> = run
        .checkpoint
        .nodes
        .iter()
        .flat_map(|node| node.children.iter())
        .collect();
    let mut human = format!("Graph {}: {:?}\n", run.checkpoint.graph_id, run.status);
    for node in &run.checkpoint.nodes {
        if members.contains(&node.id) || node.round.is_some() {
            continue;
        }
        human.push_str(&format!("  {}: {:?}", node.id, node.state));
        if !node.children.is_empty() {
            human.push_str(&format!(" ({} members)", node.children.len()));
        }
        if let Some(head) = &node.source_commit {
            human.push_str(&format!(" source={head}"));
        }
        human.push('\n');
    }
    if let Some(error) = &run.advance_error {
        human.push_str(&format!("Advance error: {error}\n"));
    }
    human
}

/// Preset waiting advances the retained immutable graph, including CI gates.
pub(crate) fn run_preset_graph(
    document: PathBuf,
    wait_seconds: u64,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    use provenant_fabric_runtime::graph::GraphStatus;
    use provenant_records::Outcome;
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        if factory.context.in_lane {
            return Err(CoordinationError::LaneMutation.into());
        }
        let cancel = CancelToken::new();
        let waiting = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
        loop {
            let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
            let run = factory.graph(
                GraphInput::Run {
                    extra: Default::default(),
                    document: document.clone(),
                },
                budget,
                &cancel,
            )?;
            if run.status != GraphStatus::Pending
                || waiting.remaining(factory.clock.mono_now()).as_micros() == 0
            {
                return Ok(run);
            }
            provenant_fabric_store::RetryWait::wait(
                &factory.backoff,
                provenant_core::DurationMicros::from_micros(
                    250_000.min(waiting.remaining(factory.clock.mono_now()).as_micros()),
                ),
                &cancel,
            );
        }
    })();
    match result {
        Ok(run) => {
            let outcome = match run.status {
                GraphStatus::Complete if run.checkpoint.successful() => Outcome::Ok,
                GraphStatus::Complete => Outcome::Failed,
                GraphStatus::Cancelled => Outcome::Cancelled,
                GraphStatus::Paused => Outcome::Refused,
                GraphStatus::Pending => Outcome::Timeout,
            };
            let continuation = format!(
                "provenant fabric graph resume --document {}",
                document.display()
            );
            let mut human = graph_human(&run);
            if matches!(run.status, GraphStatus::Pending | GraphStatus::Paused) {
                human.push_str(&format!("Continue: {continuation}\n"));
            } else if let Some(result) = run
                .checkpoint
                .nodes
                .iter()
                .rev()
                .filter_map(|node| {
                    node.outputs
                        .as_ref()
                        .and_then(|outputs| outputs.result.as_ref())
                })
                .next()
            {
                human.push_str(&result.excerpt);
                human.push('\n');
            }
            crate::emit(
                &provenant_records::CommandResult::new(
                    "run",
                    outcome,
                    serde_json::json!({"graph":run,"document":document,"continue":continuation}),
                ),
                globals,
                &human,
                false,
            )
        }
        Err(error) => crate::fail(
            "run",
            Outcome::Error,
            "preset_graph",
            &error.to_string(),
            globals,
        ),
    }
}

/// Counts of nodes by state, then the nodes that need attention (failed,
/// paused, running), then the command that shows the rest.
fn graph_summary(run: &serde_json::Value) -> provenant_cli::Summary {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let checkpoint = run.get("checkpoint").unwrap_or(&serde_json::Value::Null);
    let id = text(checkpoint, "graph_id").unwrap_or_else(|| "unknown".into());
    let nodes: Vec<(String, String, Option<String>)> = checkpoint
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .map(|node| {
                    (
                        text(node, "id").unwrap_or_default(),
                        text(node, "state").unwrap_or_else(|| "unknown".into()),
                        text(node, "advance_error"),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let rank = |state: &str| match state {
        "failed" | "refused" | "cancelled" => 0,
        "paused" | "blocked" | "input_required" => 1,
        "running" | "launched" | "pending" => 2,
        _ => 3,
    };
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (_, state, _) in &nodes {
        match counts.iter_mut().find(|(name, _)| name == state) {
            Some((_, n)) => *n += 1,
            None => counts.push((state.clone(), 1)),
        }
    }
    counts.sort_by_key(|(state, _)| rank(state));
    let tally = provenant_cli::Summary::tally(
        &counts
            .iter()
            .map(|(state, n)| (state.as_str(), *n))
            .collect::<Vec<_>>(),
    );
    let status = text(run, "status").unwrap_or_else(|| "unknown".into());
    let mut action: Vec<&(String, String, Option<String>)> = nodes
        .iter()
        .filter(|(_, state, _)| rank(state) < 3)
        .collect();
    action.sort_by_key(|(_, state, _)| rank(state));
    let mut rows: Vec<String> = Vec::new();
    if let Some(reason) = text(checkpoint, "pause_reason") {
        rows.push(format!("paused: {reason}"));
    }
    if let Some(error) = text(run, "advance_error") {
        rows.push(format!("advance error: {error}"));
    }
    rows.extend(action.iter().map(|(node, state, error)| match error {
        Some(error) => format!("{node} {state}: {error}"),
        None => format!("{node} {state}"),
    }));
    let next = match status.as_str() {
        "complete" | "cancelled" => None,
        "paused" => Some("provenant fabric graph resume --document DOCUMENT".to_owned()),
        _ => Some(format!("provenant fabric graph status {id}")),
    };
    provenant_cli::Summary::new(
        format!("graph {id} {status}: {} nodes ({tally})", nodes.len()),
        rows,
        next,
    )
}

pub fn graph(
    input: provenant_cli::fabric::Graph,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    use provenant_cli::fabric::Graph;
    let (command, input) = match input {
        Graph::Run { document } => (
            "fabric graph run",
            GraphInput::Run {
                extra: Default::default(),
                document,
            },
        ),
        Graph::Status { id } => (
            "fabric graph status",
            GraphInput::Status {
                extra: Default::default(),
                id,
            },
        ),
        Graph::Cancel { id } => (
            "fabric graph cancel",
            GraphInput::Cancel {
                extra: Default::default(),
                id,
            },
        ),
        Graph::Resume { document, extend } => (
            "fabric graph resume",
            GraphInput::Resume {
                extra: Default::default(),
                document,
                extend,
            },
        ),
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
            let human = graph_human(&data);
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
            chair: None,
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
                        PeerAction::RunEvents {
                            extra: Default::default(),
                            after,
                            limit: 64,
                        },
                        budget,
                        cancel,
                    )
                    .and_then(|result| match result {
                        PeerResult::RunEvents { data: page, .. } => Ok(page),
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
                        extra: Default::default(),
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
                ..
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
                        extra: Default::default(),
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
#[allow(unused_imports)] // The custom worktree driver compiles without a test harness.
mod graph_ask_inbox_tests {
    use super::*;
    use provenant_fabric_runtime::{
        graph::{AskScope, LoopDecision, loop_ask},
        inbox::{InboxFilter, InboxInput, InboxResult, InboxService},
    };
    use provenant_records::inbox::{
        AnswerAction, AnswerDeliveryState, InteractionChannel, RequestState,
    };

    #[test]
    fn graph_ask_inbox_replays_requests_and_acknowledged_answers()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let host = provenant_core::HostId::from_bytes([1; 16]);
        let identity = Identity {
            project: provenant_core::ProjectId::from_bytes([2; 16]),
            host,
            inbox: provenant_fabric_runtime::identity::Address::new(host, "chair")?,
            client: "test".into(),
            cwd: provenant_records::AbsolutePath::new(&base)?,
            common_dir: provenant_records::AbsolutePath::new(&base)?,
            federation: None,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = || WaitBudget::cli(clock.mono_now(), 5);
        let service = InboxService::open(&roots.state, &identity, &clock, budget()?, &cancel)?;
        for (index, (option, text, expected)) in [
            (Some("stop"), None, Some(LoopDecision::Stop)),
            (Some("accept"), None, Some(LoopDecision::Accept)),
            (
                Some("extend"),
                None,
                Some(LoopDecision::Extend { rounds: 1 }),
            ),
            (
                None,
                Some("{\"decision\":\"extend\",\"rounds\":3}"),
                Some(LoopDecision::Extend { rounds: 3 }),
            ),
            (None, Some("extend by two"), None),
        ]
        .into_iter()
        .enumerate()
        {
            let mut ask = loop_ask("fixture", "review", index + 1, "chair-session");
            if index == 2 {
                // A deadline and default reach the inbox as its deadline and recommendation.
                ask.default = Some("stop".into());
                ask.wake_at = Some(
                    clock
                        .wall_now()?
                        .checked_add(provenant_core::DurationMicros::from_seconds(3600)?)?,
                );
            }
            let actor = identity.inbox.actor.to_string();
            let scope = AskScope {
                graph_id: "fixture",
                instance_id: "instance-one",
                actor: &actor,
                ask: &ask,
            };
            assert_eq!(graph_ask_poll(&service, &scope, budget()?, &cancel)?, None);
            let InboxResult::Saved { receipt } =
                service.execute(graph_ask_input(&scope)?, budget()?, &cancel)?
            else {
                return Err("missing ask receipt".into());
            };
            service.execute(
                InboxInput::Answer {
                    extra: Default::default(),
                    id: receipt.request_id,
                    action: if option.is_some() {
                        AnswerAction::Choose
                    } else {
                        AnswerAction::Reply
                    },
                    option_id: option.map(String::from),
                    text: text.map(String::from),
                    expect_revision: receipt.revision,
                    channel: InteractionChannel::Cli,
                    operation_id: None,
                },
                budget()?,
                &cancel,
            )?;
            for _ in 0..2 {
                let reply = graph_ask_poll(&service, &scope, budget()?, &cancel)?
                    .ok_or("missing answer")?;
                assert_eq!(
                    (reply.option.as_deref(), reply.text.as_deref()),
                    (option, text)
                );
                assert_eq!(reply.actor, actor);
                assert!(reply.at.is_some());
                assert_eq!(
                    LoopDecision::parse(reply.option.as_deref(), reply.text.as_deref()).ok(),
                    expected
                );
            }
            let InboxResult::Item { item, .. } = service.execute(
                InboxInput::List {
                    extra: Default::default(),
                    id: Some(receipt.request_id),
                    session: None,
                    state: InboxFilter::All,
                    limit: 1,
                    cursor: None,
                },
                budget()?,
                &cancel,
            )?
            else {
                return Err("missing answered request".into());
            };
            assert_eq!(
                item.delivery.ok_or("missing delivery")?.state,
                AnswerDeliveryState::Acknowledged
            );
            assert_eq!(item.request.chair_actor, identity.inbox.actor);
            assert_eq!(item.request.chair_session_id, ask.session);
            assert_eq!(item.request.request.title, ask.title);
            assert_eq!(item.request.request.recommended_option_id, ask.default);
            assert_eq!(item.request.request.deadline, ask.wake_at);
            if index == 1 {
                let fresh = AskScope {
                    instance_id: "instance-two",
                    ..scope
                };
                assert_eq!(
                    graph_ask_poll(&service, &fresh, budget()?, &cancel)?,
                    None,
                    "same document recreated state must ask again"
                );
                graph_ask_close(&service, &fresh, identity.inbox.actor, budget()?, &cancel)?;
            }
            if index == 0 {
                service.execute(
                    InboxInput::Close {
                        extra: Default::default(),
                        id: receipt.request_id,
                        state: RequestState::Superseded,
                        reason: "Replace the decision".into(),
                        expect_revision: item.request.revision,
                        operation_id: None,
                    },
                    budget()?,
                    &cancel,
                )?;
                assert_eq!(
                    graph_ask_poll(&service, &scope, budget()?, &cancel)?,
                    None,
                    "a superseded request reused its historical answer"
                );
            }
            graph_ask_close(&service, &scope, identity.inbox.actor, budget()?, &cancel)?;
            graph_ask_close(&service, &scope, identity.inbox.actor, budget()?, &cancel)?;
            assert_eq!(graph_ask_poll(&service, &scope, budget()?, &cancel)?, None);
        }
        let InboxResult::Listed { items, .. } = service.execute(
            InboxInput::List {
                extra: Default::default(),
                id: None,
                session: Some("chair-session".into()),
                state: InboxFilter::All,
                limit: 50,
                cursor: None,
            },
            budget()?,
            &cancel,
        )?
        else {
            return Err("missing inbox listing".into());
        };
        assert_eq!(
            items.len(),
            6,
            "replay or cleanup created duplicate decisions"
        );
        assert!(
            items
                .iter()
                .all(|item| item.state == RequestState::Superseded)
        );
        Ok(())
    }
}

#[cfg(test)]
mod result_tests {
    #[test]
    fn graph_usage_maps_attempt_telemetry_and_prices() {
        fn usage_record(
            model: &str,
            usage: serde_json::Value,
            provider_ms: Option<u64>,
        ) -> provenant_records::run::AttemptRecord {
            use provenant_core::{AttemptId, Digest, ProjectId, RunId, TaskId, WallTime};
            use serde_json::json;
            let now = WallTime::from_micros(1).unwrap();
            serde_json::from_value(json!({
                "schema_version": 1,
                "attempt_id": AttemptId::from_bytes([1; 16]),
                "run_id": RunId::from_bytes([2; 16]),
                "project_id": ProjectId::from_bytes([3; 16]),
                "task_id": TaskId::from_bytes([4; 16]),
                "operation_id": "graph-usage-test",
                "attempt_number": 1, "revision": 1, "generation": 0,
                "requested_route": {"kind":"model", "provider":"p", "model":model},
                "resolved_route": {"provider":"p", "model":model, "effort":null, "account_label":null},
                "authority_digest": Digest::from_bytes([5; 32]), "queued_at": now,
                "state": {"state":"terminal", "outcome":"lost", "exit_code":null, "finished_at":now, "reason":null},
                "provider_ms": provider_ms, "usage": usage,
                "prompt": {"path":{"encoding":"utf8", "path":"prompt.txt"},
                    "digest": provenant_records::CanonicalV1::file_digest(b""), "bytes":0, "complete":true, "redacted":false}
            })).unwrap()
        }

        fn usage_prices() -> provenant_telemetry::PriceTable {
            let config = provenant_telemetry::ConfiguredPrices {
                schema_version: 1,
                id: "test".into(),
                reviewed_at: "2026-10-22".into(),
                units: "usd_microdollars_per_million_tokens".into(),
                models: vec![provenant_telemetry::ConfiguredModel {
                    provider: "p".into(),
                    model: "priced".into(),
                    sources: vec!["test".into()],
                    billing: provenant_telemetry::BillingKind::Api,
                    zero_rate_verified: false,
                    rates: provenant_telemetry::Rates {
                        uncached_input: Some(1_000_000),
                        cache_read: Some(2_000_000),
                        cache_write: Some(3_000_000),
                        output_excluding_reasoning: Some(4_000_000),
                    },
                }],
            };
            provenant_telemetry::validate_prices(&config).unwrap()
        }

        use serde_json::json;
        let prices = usage_prices();
        let full = json!({"uncached_input":1_000_000, "cache_read":1_000_000,
            "cache_write":1_000_000, "output_excluding_reasoning":1_000_000, "reasoning":1_000_000});
        let result = super::graph_attempt_usage(
            usage_record("priced", full.clone(), Some(1001)),
            Some(&prices),
            None,
        );
        assert_eq!(result.tokens, Some(5_000_000));
        assert!(!result.tokens_partial);
        assert_eq!(result.cost_micro_usd, Some(14_000_000));
        assert!(!result.cost_partial);
        assert_eq!(result.wall_time_seconds, Some(2));
        assert_eq!(result.cost_unknown_reason, None);

        // No provider reports reasoning apart from output, so its absence is
        // not partial usage.
        let without_reasoning = super::graph_attempt_usage(
            usage_record(
                "priced",
                json!({"uncached_input":1_000_000, "cache_read":1_000_000,
                    "cache_write":1_000_000, "output_excluding_reasoning":1_000_000}),
                Some(1000),
            ),
            Some(&prices),
            None,
        );
        assert!(!without_reasoning.tokens_partial);
        assert!(!without_reasoning.cost_partial);
        assert_eq!(without_reasoning.cost_micro_usd, Some(10_000_000));

        let partial = super::graph_attempt_usage(
            usage_record("priced", json!({"uncached_input":1_000_000}), Some(1000)),
            Some(&prices),
            None,
        );
        assert_eq!(partial.tokens, Some(1_000_000));
        assert!(partial.tokens_partial);
        assert_eq!(partial.cost_micro_usd, Some(1_000_000));
        assert!(partial.cost_partial);
        assert_eq!(partial.wall_time_seconds, Some(1));

        let absent = super::graph_attempt_usage(
            usage_record("priced", json!({}), None),
            Some(&prices),
            None,
        );
        assert_eq!(absent.tokens, None);
        assert!(absent.tokens_partial);
        assert_eq!(absent.cost_micro_usd, None);
        assert!(absent.cost_partial);
        assert_eq!(absent.wall_time_seconds, None);

        let unpriced = super::graph_attempt_usage(
            usage_record("unpriced", full.clone(), Some(0)),
            Some(&prices),
            None,
        );
        assert_eq!(unpriced.tokens, Some(5_000_000));
        assert!(!unpriced.tokens_partial);
        assert_eq!(unpriced.cost_micro_usd, None);
        assert!(unpriced.cost_partial);
        assert_eq!(unpriced.wall_time_seconds, Some(0));

        let unavailable = super::graph_attempt_usage(
            usage_record("priced", full, None),
            None,
            Some("price table unavailable".into()),
        );
        assert_eq!(unavailable.cost_micro_usd, None);
        assert!(unavailable.cost_partial);
        assert_eq!(
            unavailable.cost_unknown_reason.as_deref(),
            Some("price table unavailable")
        );
    }

    #[test]
    fn graph_status_handles_pause_left_after_terminal_checkpoint() {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphStatus, NodeState};

        // This is the durable state if the process stops immediately after saving
        // the final terminal node, before advance clears the structural pause.
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": "graph",
            "plan_digest": "digest",
            "pause_reason": "node limit exceeded",
            "nodes": [{"id": "one", "state": "succeeded", "worktree": null, "attempts": []}]
        }))
        .unwrap();
        let run = super::graph_snapshot(checkpoint.clone());
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.pause_reason.is_none());

        checkpoint.nodes[0].state = NodeState::Running;
        assert_eq!(
            super::graph_snapshot(checkpoint.clone()).status,
            GraphStatus::Paused
        );
        checkpoint.nodes[0].state = NodeState::Succeeded;
        checkpoint.nodes[0].expansion_pause = Some("fragment exceeds node bound".into());
        assert_eq!(
            super::graph_snapshot(checkpoint.clone()).status,
            GraphStatus::Paused
        );
        checkpoint.nodes[0].expansion_pause = None;
        checkpoint.delivery_required = true;
        assert_eq!(
            super::graph_snapshot(checkpoint.clone()).status,
            GraphStatus::Paused
        );
        checkpoint.cancelled = true;
        assert_eq!(
            super::graph_snapshot(checkpoint).status,
            GraphStatus::Cancelled
        );
    }

    #[test]
    fn graph_result_errors_produce_safe_specific_warnings() {
        use provenant_run::RunError;
        for (error, warning) in [
            (
                RunError::ArtifactMismatch,
                "artifact digest or length does not match its record",
            ),
            (RunError::NotFound, "result artifact is missing"),
            (
                RunError::TooLarge { limit: 1 },
                "result artifact exceeds the byte limit",
            ),
            (
                RunError::Validation("output redaction failed"),
                "result artifact could not be verified",
            ),
        ] {
            assert_eq!(super::graph_result_warning(&error), warning);
        }
    }
    #[test]
    fn missing_graph_price_table_reports_its_path_and_failure() {
        let assets = tempfile::tempdir().unwrap();
        let reason = super::load_graph_prices(assets.path()).unwrap_err();
        assert!(
            reason.contains(
                &assets
                    .path()
                    .join("config/telemetry-prices.toml")
                    .display()
                    .to_string()
            )
        );
        assert!(reason.contains("No such file") || reason.contains("not found"));
    }

    #[test]
    fn newer_graph_schemas_report_upgrade_required() {
        let document_error =
            super::decode_graph_document(br#"{"schema_version":2,"future_shape":true}"#)
                .unwrap_err();
        let decode_error = provenant_records::decode_v1::<serde_json::Value, _>(
            br#"{"schema_version":2,"future_shape":true}"#.as_slice(),
            provenant_records::Limits::default(),
        )
        .unwrap_err();
        let checkpoint_error = super::graph_journal_error(
            provenant_fabric_runtime::graph::JournalError::Decode(decode_error),
        );
        for (error, record_kind) in [
            (document_error, "graph document schema"),
            (checkpoint_error, "graph checkpoint schema"),
        ] {
            assert_eq!(error.code(), "upgrade_required");
            let super::FacadeError::UpgradeRequired {
                reader_release,
                writer_release,
                record,
                version,
            } = error
            else {
                unreachable!()
            };
            assert_eq!(reader_release, env!("CARGO_PKG_VERSION"));
            assert_eq!(writer_release, "unknown");
            assert_eq!(record, record_kind);
            assert_eq!(version, 2);
        }
        assert_eq!(
            super::decode_graph_document(br#"{"schema_version":0}"#).unwrap_err(),
            super::FacadeError::InvalidInput
        );
    }

    #[test]
    fn oversized_graph_checkpoint_is_refused_with_size_reason() {
        match super::graph_journal_error(provenant_fabric_runtime::graph::JournalError::TooLarge) {
            super::FacadeError::DispatchRefused { reason } => {
                assert!(reason.contains("byte limit"))
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes these helpers without libtest.
mod peer_tests {
    use super::*;
    #[allow(unused_imports)]
    // The worktree driver includes this module without libtest functions.
    use provenant_confinement::worktrees;
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
        let fixture_home = tempfile::tempdir()?;
        let isolate_home = name
            .ends_with("explicit_writer_refreshes_tracking_ref_while_collection_is_busy")
            || name.ends_with("adopted_writer_refresh_failure_does_not_change_receipt");
        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !removed.iter().any(|name| key == name) && !(isolate_home && key == "HOME")
            })
            .collect();
        if isolate_home {
            environment.push(("HOME".into(), fixture_home.path().as_os_str().to_owned()));
        }
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

    mod loop_repair_tests {
        include!("fabric/loop_repair_tests.rs");
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
        publish_outcome(rig, store, record, head, Outcome::Succeeded, call)
    }

    fn publish_outcome(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        outcome: Outcome,
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
                outcome,
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
                partial_signals: Vec::new(),
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
                    chair: None,
                    operation_id: "settlement-original".parse()?,
                    concurrency: 1,
                    lane_depth: 1,
                    parent_attempt: None,
                    tasks: vec![provenant_run::DispatchTask {
                        build_heavy: false,
                        title: None,
                        task_id: TaskId::from_bytes([3; 16]),
                        prompt: "reply OK".into(),
                        base: Some(info.head.to_string()),
                        head: None,
                        profile: Default::default(),
                        route: provenant_run::RequestedRoute::Model {
                            extra: Default::default(),
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
                AttemptState::Queued {
                    extra: Default::default(),
                },
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
    fn run_list_is_compact_and_pages_newest_first() -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::run_list_is_compact_and_pages_newest_first")? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
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
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let first = owner.run(
            RunInput::List {
                extra: Default::default(),
                after: None,
                limit: 1,
            },
            budget,
            &cancel,
        )?;
        let wire = serde_json::to_value(&first)?;
        let row = &wire["page"]["items"][0];
        assert!(
            row["attempt_id"] == fixture.settled.attempt_id.to_string()
                && row.get("usage").is_none()
                && row.get("prompt").is_none()
                && row.get("trace").is_none(),
            "expected newest compact row, got {row}"
        );
        let RunResult::List { page, .. } = first else {
            return Err("list result".into());
        };
        assert!(page.truncated);
        let cursor = page.next_after.ok_or("next cursor")?;
        let second = owner.run(
            RunInput::List {
                extra: Default::default(),
                after: Some(cursor),
                limit: 1,
            },
            budget,
            &cancel,
        )?;
        let RunResult::List { page, .. } = second else {
            return Err("list result".into());
        };
        assert!(!page.truncated);
        assert!(page.next_after.is_none());
        let second = serde_json::to_value(&page.items[0])?;
        assert_ne!(second["attempt_id"], row["attempt_id"]);
        assert_eq!(second["state"]["outcome"], "cancelled");
        assert_eq!(second["state"]["reason"], "fixture");
        assert!(second["state"]["finished_at"].is_string());
        let status = owner.run(
            RunInput::Status {
                extra: Default::default(),
                target: AttemptTarget {
                    extra: Default::default(),
                    attempt: fixture.settled.attempt_id,
                    host: None,
                },
            },
            budget,
            &cancel,
        )?;
        let RunResult::Status { mut attempt, .. } = status else {
            return Err("status result".into());
        };
        let status = serde_json::to_value(RunResult::Status {
            extra: Default::default(),
            attempt: attempt.clone(),
        })?;
        assert!(status["attempt"].get("usage").is_some());
        assert!(status["attempt"].get("prompt").is_some());
        // Pool routing carries an audit receipt in full records. It must not
        // enlarge the compact route returned by list.
        attempt.applied_route = Some(serde_json::from_value(serde_json::json!({
            "provider":"fixture", "model":"fixture/model", "effort":"high",
            "account_label":"fixture", "account_key":"private-key", "source":"pool",
            "pool_selection": {
                "pool":"senior", "candidates":[], "roll":0, "total_weight":1,
                "pick":"fixture/model"
            }
        }))?);
        let summary = provenant_fabric_runtime::runs::AttemptSummary::from(attempt);
        let summary = serde_json::to_value(summary)?;
        assert_eq!(
            summary["applied_route"],
            serde_json::json!({
                "provider":"fixture", "model":"fixture/model", "effort":"high",
                "account_label":"fixture"
            })
        );
        Ok(())
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
                _: graph::NodeInput<'_>,
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
                extra: Default::default(),
                integration_base: None,
                directory: fixture.rig.project.clone(),
                document: document.clone(),
            })?;
            let mut seed = SeedAttempt {
                reference: format!("{}/{attempt}", identity.host),
                actor: identity.inbox.actor.to_string(),
            };
            let seeded = graph::advance_with_budget(
                &document,
                &mut seed,
                &mut journal,
                &Default::default(),
                &Default::default(),
            )?;
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
            executable_search_path: Vec::new(),
            git: provenant_os::git::git_executable().ok_or("git")?,
            codex_home: None,
            cargo_build_jobs: None,
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
    fn graph_owner_fails_node_when_result_integrity_is_lost()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::graph_owner_fails_node_when_result_integrity_is_lost")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner, GraphJournal};
        struct Seed(String);
        impl AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.0.clone()),
                })
            }
        }
        let fixture = SettlementFixture::resumed()?;
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
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let artifact = fixture.rig.state.join(format!(
            "runs/{}/attempts/{}/result.md",
            fixture.settled.run_id, fixture.settled.attempt_id
        ));
        std::fs::write(&artifact, b"XX")?;
        for required in [None, Some("VERDICT: PASS")] {
            let id = if required.is_some() {
                "corrupt-required"
            } else {
                "corrupt-result"
            };
            let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version":1,"graph_id":id,"nodes":[
                    {"id":"parent","prompt":"work","access":"worktree_write","worktree":fixture.worktree,"required_verdict":required,"route":{"adapter":"fixture","model":"fixture/model"}},
                    {"id":"child","prompt":"review","access":"read_only","depends_on":["parent"],"inputs":[{"node":"parent","outputs":["result"]}],"route":{"adapter":"fixture","model":"fixture/model"}}
                ]
            }))?;
            let mut journal = graph::FileJournal::open(
                &fixture.roots.state,
                fixture.settled.project_id,
                id,
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.bind(graph::GraphExecution {
                extra: Default::default(),
                integration_base: None,
                directory: fixture.rig.project.clone(),
                document: document.clone(),
            })?;
            let mut seed = Seed(format!(
                "{}/{}",
                factory.context.identity.host, fixture.settled.attempt_id
            ));
            graph::advance_with_budget(
                &document,
                &mut seed,
                &mut journal,
                &Default::default(),
                &Default::default(),
            )?;
            let mut owner = BoundGraphOwner {
                factory: &factory,
                integration_base: None,
                integration_branch: None,
                budget,
                cancel: &cancel,
                prices: None,
            };
            let run = graph::advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &Default::default(),
                &Default::default(),
            )?;
            let parent = &run.checkpoint.nodes[0];
            assert_eq!(parent.state, graph::NodeState::Failed);
            assert_eq!(
                parent.attempts[0].outcome,
                Some(graph::AttemptOutcome::OtherFailure)
            );
            assert!(
                parent
                    .outputs
                    .as_ref()
                    .ok_or("outputs")?
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("digest or length"))
            );
            assert_eq!(run.checkpoint.nodes[1].state, graph::NodeState::Blocked);
            assert_eq!(
                journal.load(id)?.ok_or("checkpoint")?.nodes[0].state,
                graph::NodeState::Failed
            );
        }

        // A corrupt authentication failure must not consume a fallback route,
        // even for a read-only node where that provider outcome normally would.
        let call = provenant_run::Request {
            clock: &factory.clock,
            cancel: &cancel,
            budget,
        };
        let continuation = provenant_run::Resume {
            prior: fixture.settled.attempt_id,
            expected_generation: 0,
            operation_id: "graph-corrupt-auth".parse()?,
            run_id: RunId::from_bytes([43; 16]),
            attempt_id: AttemptId::from_bytes([44; 16]),
            prompt: "continue".into(),
            created_at: factory.clock.wall_now()?,
        };
        fixture
            .store
            .resume_fresh(&continuation, "fixture", &call)?;
        let record = fixture.store.record(continuation.attempt_id, &call)?;
        let head = fixture
            .store
            .record(fixture.settled.attempt_id, &call)?
            .head
            .ok_or("head")?;
        publish_outcome(
            &fixture.rig,
            &fixture.store,
            &record,
            head,
            Outcome::AuthRequired,
            &call,
        )?;
        let artifact = fixture.rig.state.join(format!(
            "runs/{}/attempts/{}/result.md",
            continuation.run_id, continuation.attempt_id
        ));
        std::fs::write(&artifact, b"XX")?;
        let id = "corrupt-auth";
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":id,"nodes":[
                {"id":"parent","prompt":"work","access":"read_only","routes":[
                    {"adapter":"fixture","model":"fixture/model"},
                    {"adapter":"fixture","model":"fixture/other"}
                ]}
            ]
        }))?;
        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {
            extra: Default::default(),
            integration_base: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;
        let mut seed = Seed(format!(
            "{}/{}",
            factory.context.identity.host, continuation.attempt_id
        ));
        graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let mut owner = BoundGraphOwner {
            factory: &factory,
            integration_base: None,
            integration_branch: None,
            budget,
            cancel: &cancel,
            prices: None,
        };
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let parent = &run.checkpoint.nodes[0];
        assert_eq!(parent.state, graph::NodeState::Failed);
        assert_eq!(parent.attempts.len(), 1);
        assert_eq!(
            parent.attempts[0].outcome,
            Some(graph::AttemptOutcome::OtherFailure)
        );
        assert!(
            parent
                .outputs
                .as_ref()
                .ok_or("outputs")?
                .warnings
                .iter()
                .any(|warning| warning.contains("digest or length"))
        );
        std::fs::remove_file(&artifact)?;
        let mut missing_document = document.clone();
        missing_document.graph_id = "missing-auth-result".into();
        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            &missing_document.graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {
            extra: Default::default(),
            integration_base: None,
            directory: fixture.rig.project.clone(),
            document: missing_document.clone(),
        })?;
        let mut seed = Seed(format!(
            "{}/{}",
            factory.context.identity.host, continuation.attempt_id
        ));
        graph::advance_with_budget(
            &missing_document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let run = graph::advance_with_budget(
            &missing_document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        let parent = &run.checkpoint.nodes[0];
        assert_eq!(parent.state, graph::NodeState::Failed);
        assert_eq!(parent.attempts.len(), 1);
        assert_eq!(
            parent.attempts[0].outcome,
            Some(graph::AttemptOutcome::OtherFailure)
        );
        assert!(
            parent
                .outputs
                .as_ref()
                .ok_or("outputs")?
                .warnings
                .iter()
                .any(|warning| warning == "result artifact is missing")
        );
        Ok(())
    }

    #[test]
    fn graph_submission_refuses_a_push_to_main_before_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_submission_refuses_a_push_to_main_before_admission",
        )? {
            return Ok(());
        }
        use provenant_confinement::worktrees::{self, WorktreeCreate};
        use provenant_fabric_runtime::graph::FileJournal;
        let fixture = SettlementFixture::resumed()?;
        let cancel = CancelToken::new();
        let info = worktrees::with_context(
            &fixture.roots,
            &fixture.rig.home,
            &fixture.rig.project,
            &SystemClock,
            &cancel,
            |context| {
                worktrees::create(
                    &worktrees::WorktreeContext {
                        workers: &FreeWorkers,
                        ..*context
                    },
                    WorktreeCreate {
                        branch: "main".into(),
                        base: Some("HEAD".into()),
                    },
                    &cancel,
                )
            },
        )?;
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
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join("protected-push-graph.json");
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":"protected-push",
                "nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write",
                    "worktree":info.path,"delivery":"push",
                    "route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?,
        )?;
        let result = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document,
            },
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
        );
        assert!(
            FileJournal::read_checkpoint(
                &fixture.roots.state,
                fixture.settled.project_id,
                "protected-push"
            )?
            .is_none(),
            "protected push was admitted: {result:?}"
        );
        assert!(
            matches!(result, Err(FacadeError::ConfinementRefused)),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn graph_submission_replays_an_accepted_push_after_protection_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::graph_submission_replays_an_accepted_push_after_protection_changes",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
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
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join("replay-push-graph.json");
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":"replay-push",
                "nodes":[{"id":"one","prompt":"reply OK","access":"worktree_write",
                    "worktree":fixture.worktree,"delivery":"push",
                    "route":{"adapter":"fixture","model":"fixture/model"}}]
            }))?,
        )?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let run = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
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
        let git = provenant_os::git::git_executable().ok_or("git")?;
        for args in [
            vec![
                "remote",
                "add",
                "origin",
                fixture.rig.project.to_str().ok_or("project")?,
            ],
            vec![
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/resumed-lane",
            ],
        ] {
            let output = fixture.rig.execute(&git, &args, None)?;
            assert_eq!(crate::worktree_tests::support::code(&output), 0);
        }
        for input in [
            GraphInput::Run {
                extra: Default::default(),
                document: document.clone(),
            },
            GraphInput::Resume {
                extra: Default::default(),
                document,
                extend: vec![],
            },
        ] {
            let replay = factory.graph(input, budget, &cancel)?;
            assert_eq!(replay.checkpoint, run.checkpoint);
        }
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
            mcp_project: None,
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
                extra: Default::default(),
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
        let owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store(budget, &cancel)?;
        let operation = graph_operation(
            "graph-attempt-operation",
            &run.checkpoint.nodes[0].attempts[0].key,
        )?;
        let operation = store
            .operation(&operation, &owner.call(budget, &cancel))?
            .ok_or("graph run not admitted")?;
        let index_call = provenant_fabric_runtime::chairs::Request {
            clock: &factory.clock,
            budget,
            cancel: &cancel,
            backoff: &ThreadWait,
        };
        let binding = provenant_fabric_runtime::chairs::ChairIndex::open(
            &factory.context.roots.state,
            &index_call,
        )?
        .read(
            factory.context.identity.project,
            operation.run_id,
            &index_call,
        )?
        .ok_or("graph index missing")?;
        assert!(binding.chair.is_none(), "graph created a chair association");
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
        let replay = factory.graph(
            GraphInput::Resume {
                extra: Default::default(),
                document,
                extend: vec![],
            },
            budget,
            &cancel,
        )?;
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
    fn retained_lane_settlement_collects_only_its_output_and_cache()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::retained_lane_settlement_collects_only_its_output_and_cache",
        )? {
            return Ok(());
        }
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let fixture = SettlementFixture::resumed()?;
        std::fs::write(
            fixture.rig.project.join(".git/info/exclude"),
            "build/\nsaved/\n",
        )?;
        std::fs::write(
            fixture.rig.state.join("config.toml"),
            format!(
                "schema_version=1\n[gc.projects.\"{}\"]\nregenerable=['build','saved']\nkeep=['saved']\nforeign_worktrees='merged-clean'\n",
                fixture.settled.project_id
            ),
        )?;
        std::fs::create_dir(fixture.worktree.join("build"))?;
        std::fs::write(fixture.worktree.join("build/artifact"), "regenerable")?;
        std::fs::create_dir(fixture.worktree.join("saved"))?;
        std::fs::write(fixture.worktree.join("saved/keep"), "keep")?;
        let other = fixture.rig.project.join(".worktrees/foreign");
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture.rig.execute(
            &git,
            &[
                "worktree",
                "add",
                "-b",
                "foreign",
                other.to_str().ok_or("path")?,
            ],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        std::fs::create_dir(other.join("build"))?;
        std::fs::write(other.join("build/artifact"), "foreign")?;
        let cache_root = fixture.rig.home.join("Library/Caches/provenant-lane-cargo");
        // Use valid cache ownership markers, including an unrelated orphan.
        let cache = cache_root.join("a".repeat(64));
        let orphan = cache_root.join("b".repeat(64));
        let identity = std::fs::metadata(&fixture.worktree)?;
        for (path, owner) in [
            (&cache, fixture.worktree.clone()),
            (&orphan, fixture.rig.root.join("missing")),
        ] {
            std::fs::create_dir_all(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            let mut marker = b"provenant-lane-cargo-owner-v2\n".to_vec();
            marker.extend_from_slice(&identity.dev().to_le_bytes());
            marker.extend_from_slice(&identity.ino().to_le_bytes());
            marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
            std::fs::write(path.join(".provenant-worktree-owner"), marker)?;
            std::fs::write(path.join("artifact"), "cached")?;
        }
        let delivery = provenant_run::WriterDeliveryReceipt {
            extra: Default::default(),
            schema_version: 1,
            remote: "origin".into(),
            branch: "resumed-lane".into(),
            head: None,
            review_acceptance: None,
            state: provenant_run::WriterDeliveryState::NotAttempted {
                extra: Default::default(),
            },
        };
        let collection = crate::worktrees::collection_lock(&fixture.roots)?;
        settle_owned_worktree(
            &fixture.settled,
            &fixture.roots,
            &FreeWorkers,
            Some(&delivery),
        )?;
        assert!(fixture.worktree.join("build/artifact").exists());
        assert!(cache.exists());
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        let busy = fixture
            .store
            .worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("missing busy settlement receipt")?;
        assert_eq!(busy["state"], "retained", "{busy}");
        assert_eq!(busy["reason"], crate::worktrees::COLLECTION_BUSY);
        drop(collection);
        let state = provenant_os::fs::Directory::open_anchor(
            &fixture
                .rig
                .state
                .join("worktrees")
                .join(fixture.settled.project_id.to_string())
                .join(provenant_confinement::worktrees::state_dir_name(
                    &fixture.worktree,
                )),
        )?;
        let writer = state.lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
            WaitBudget::nonblocking(SystemClock.mono_now()),
            &CancelToken::new(),
            &SystemClock,
        )?;
        settle_owned_worktree(
            &fixture.settled,
            &fixture.roots,
            &FreeWorkers,
            Some(&delivery),
        )?;
        assert!(fixture.worktree.join("build/artifact").exists());
        assert!(cache.exists());
        drop(writer);
        for _ in 0..2 {
            settle_owned_worktree(
                &fixture.settled,
                &fixture.roots,
                &FreeWorkers,
                Some(&delivery),
            )?;
            assert!(fixture.worktree.exists());
            assert!(!fixture.worktree.join("build/artifact").exists());
            assert!(!cache.exists(), "settled lane cache survived");
            assert_eq!(
                std::fs::read_to_string(fixture.worktree.join("saved/keep"))?,
                "keep"
            );
            assert_eq!(
                std::fs::read_to_string(other.join("build/artifact"))?,
                "foreign"
            );
            assert!(orphan.exists(), "unrelated cache collected");
        }
        Ok(())
    }

    #[test]
    fn explicit_writer_refreshes_tracking_ref_while_collection_is_busy()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::explicit_writer_refreshes_tracking_ref_while_collection_is_busy",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let owner_path = worktrees::with_context(
            &fixture.roots,
            &fixture.rig.home,
            &fixture.rig.project,
            &clock,
            &cancel,
            |context| {
                let tree = provenant_confinement::verify_worktree(context.repo, &fixture.worktree)?;
                Ok(tree.repo().private_git_dir().join("provenant-owner.json"))
            },
        )?;
        std::fs::remove_file(owner_path)?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let remote = fixture.rig.root.join("remote.git");
        for args in [
            vec!["init", "--bare", remote.to_str().ok_or("remote path")?],
            vec![
                "remote",
                "add",
                "origin",
                remote.to_str().ok_or("remote path")?,
            ],
            vec![
                "push",
                "origin",
                "refs/heads/resumed-lane:refs/heads/resumed-lane",
            ],
            vec!["update-ref", "-d", "refs/remotes/origin/resumed-lane"],
        ] {
            let output = fixture.rig.execute(&git, &args, None)?;
            assert_eq!(
                crate::worktree_tests::support::code(&output),
                0,
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
        }
        let collection = crate::worktrees::collection_lock(&fixture.roots)?;
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let output = fixture.rig.execute(
            &git,
            &["rev-parse", "--verify", "refs/remotes/origin/resumed-lane"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        drop(collection);
        Ok(())
    }

    #[test]
    fn adopted_writer_refresh_failure_does_not_change_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("fabric::peer_tests::adopted_writer_refresh_failure_does_not_change_receipt")?
        {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        worktrees::with_context(
            &fixture.roots,
            &fixture.rig.home,
            &fixture.rig.project,
            &clock,
            &cancel,
            |context| {
                let tree = provenant_confinement::verify_worktree(context.repo, &fixture.worktree)?;
                let mut owner = provenant_confinement::worktrees::load_owner(&tree)?
                    .ok_or(provenant_confinement::worktrees::WorktreeError::InvalidOutput)?;
                owner.origin = provenant_confinement::worktrees::WorktreeOrigin::Adopted;
                provenant_confinement::worktrees::save_owner(&tree, &owner)?;
                Ok(())
            },
        )?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture.rig.execute(
            &git,
            &["remote", "add", "origin", "/nonexistent/provenant-remote"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        assert!(
            fixture
                .store
                .worktree_settlement(fixture.settled.attempt_id, &call)?
                .is_none()
        );
        let receipt = serde_json::json!({"schema_version":1, "state":"retained", "worktree":fixture.worktree, "reason":"existing reason"});
        record_worktree_settlement(&fixture.store, fixture.settled.attempt_id, &receipt)?;
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        assert_eq!(
            fixture
                .store
                .worktree_settlement(fixture.settled.attempt_id, &call)?,
            Some(receipt)
        );
        Ok(())
    }

    #[test]
    fn settlement_records_tracking_ref_failure_without_failing_lane()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test(
            "fabric::peer_tests::settlement_records_tracking_ref_failure_without_failing_lane",
        )? {
            return Ok(());
        }
        let fixture = SettlementFixture::resumed()?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let output = fixture.rig.execute(
            &git,
            &["remote", "add", "origin", "/nonexistent/provenant-remote"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
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
            .ok_or("settlement")?;
        assert!(receipt["tracking_ref_warning"].is_string(), "{receipt}");
        assert_eq!(receipt["state"], "pruned", "{receipt}");
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
            summary: None,
            read_only: false,
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
            extra: Default::default(),
            schema_version: 1,
            remote: "origin".into(),
            branch: "lane".into(),
            head: None,
            review_acceptance: None,
            state: WriterDeliveryState::Delivered {
                extra: Default::default(),
                pull_request: None,
            },
        };
        for outcome in [
            Outcome::Failed,
            Outcome::RateLimited,
            Outcome::Cancelled,
            Outcome::TimedOut,
            Outcome::Rejected,
        ] {
            let state = AttemptState::Terminal {
                extra: Default::default(),
                outcome,
                exit_code: Some(1),
                finished_at: provenant_core::WallTime::from_micros(0)?,
                reason: None,
            };
            assert!(super::settlement_retention_reason(&state, false, None).is_some());
            assert!(super::settlement_retention_reason(&state, true, Some(&delivered)).is_some());
        }
        let succeeded = AttemptState::Terminal {
            extra: Default::default(),
            outcome: Outcome::Succeeded,
            exit_code: Some(0),
            finished_at: provenant_core::WallTime::from_micros(0)?,
            reason: None,
        };
        assert!(super::settlement_retention_reason(&succeeded, false, None).is_none());
        assert!(super::settlement_retention_reason(&succeeded, true, Some(&delivered)).is_none());
        assert!(super::settlement_retention_reason(&succeeded, true, None).is_some());
        let failed_delivery = WriterDeliveryReceipt {
            state: WriterDeliveryState::NotAttempted {
                extra: Default::default(),
            },
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
            extra: Default::default(),
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
                data: provenant_fabric_runtime::hosts::PeerResultError {
                    extra: Default::default(),
                    error: super::FacadeError::io("test"),
                },
                extra: Default::default(),
            });
            let response: super::PeerResult =
                serde_json::from_slice(&serde_json::to_vec(&response)?)?;
            let super::PeerResult::Error {
                data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
                ..
            } = response
            else {
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
            data: provenant_fabric_runtime::hosts::PeerResultError {
                extra: Default::default(),
                error: super::FacadeError::io("test"),
            },
            extra: Default::default(),
        });
        let super::PeerResult::Error {
            data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
            ..
        } = reply
        else {
            return Err("expected error".into());
        };
        assert!(
            matches!(error, super::FacadeError::LaunchUnknown { operation_id } if operation_id.as_str() == "stop")
        );
        Ok(())
    }
}
