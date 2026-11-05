//! Fabric process composition: MCP, fixed peer ingress and host observations.
mod graph_human;
mod graph_inputs;
mod graph_status;
mod peer;
mod preset_graph;

#[cfg(test)]
#[allow(unused_imports)]
use graph_human::graph_ci_waiting;
pub(crate) use graph_human::graph_human;
use graph_human::graph_summary;
use graph_status::{graph_snapshot, graph_status_elapsed};
pub use peer::{hosts, peer};
#[cfg(test)]
#[allow(unused_imports)] // unused where integration tests include this file
pub(crate) use preset_graph::ScratchPreparationError;
#[allow(unused_imports)]
pub(crate) use preset_graph::publish_preset_graph;
pub(crate) use preset_graph::run_preset_graph;
#[cfg(test)]
#[allow(unused_imports)] // unused where integration tests include this file
pub(crate) use preset_graph::{PreparedScratch, retry_scratch_poll, scratch_prepare_error_message};

#[cfg(test)]
include!("fabric/facade_tests.rs");
#[cfg(test)]
include!("fabric/graph_tests.rs");

use provenant_cli::fabric::Context;

/// Repeat a list-then-read tail lookup until two consecutive passes name the
/// same attempt, and return the earlier pass's record. The later pass listed
/// after the earlier one read its state, so agreement proves no successor had
/// been committed with that state. Bounded by the policy chain ceiling.
fn confirm_stable_tail<R, K: PartialEq>(
    mut pass: impl FnMut() -> Result<R, FacadeError>,
    id: impl Fn(&R) -> K,
) -> Result<R, FacadeError> {
    let mut previous: Option<R> = None;
    for _ in 0..=provenant_run::POLICY_ATTEMPT_CEILING {
        let tail = pass()?;
        if let Some(earlier) = previous.take()
            && id(&earlier) == id(&tail)
        {
            return Ok(earlier);
        }
        previous = Some(tail);
    }
    Err(FacadeError::StateCorrupt)
}
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

pub(crate) fn scratch_prepare_error(error: impl std::fmt::Display) -> FacadeError {
    FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        operation: format!("prepare project scratch: {error}"),
        path: Some(".provenant/scratch".into()),
    }
}

fn graph_writer_initialisation_error(
    commit: &str,
    error: provenant_confinement::worktrees::WorktreeError,
) -> FacadeError {
    use provenant_confinement::worktrees::WorktreeError;
    match error {
        WorktreeError::RegistryLockTimeout | WorktreeError::LockBusy => FacadeError::WriterLockBusy,
        WorktreeError::UnknownWriter
        | WorktreeError::UnknownWriterAt { .. }
        | WorktreeError::CensusUnknown { .. }
        | WorktreeError::IncompleteCensus { .. } => FacadeError::WriterUnknown,
        error => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!("initialise graph writer at {commit}: {error}"),
        },
    }
}

fn lane_timeout(deadline_seconds: Option<u64>, graph_timeout_seconds: u64) -> u64 {
    deadline_seconds
        .unwrap_or(graph_timeout_seconds)
        .min(graph_timeout_seconds)
}

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
    backoff: ThreadPause,
    executor: SystemExecutor,
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    assets: PathBuf,
    host_launch: Option<provenant_run::HostLaunchContext>,
    mcp_project: Option<PathBuf>,
    workers: &'static dyn WorkerObserver,
}
impl Factory {
    /// Reopen a retained graph directory without borrowing the caller's checkout.
    fn for_graph_directory(&self, directory: &Path) -> Result<Self, FacadeError> {
        let context = Context::open(self.context.roots.clone(), &self.home, directory)
            .map_err(|_| FacadeError::StateCorrupt)?;
        if context.identity.project != self.context.identity.project
            || context.in_lane != self.context.in_lane
        {
            return Err(FacadeError::StateCorrupt);
        }
        let mut host_launch = self.host_launch.clone();
        if let Some(host) = &mut host_launch {
            host.cwd = directory.to_path_buf();
        }
        Ok(Self {
            context,
            project_attribution: self.project_attribution,
            clock: SystemClock::new(),
            backoff: ThreadPause,
            executor: SystemExecutor::new(4).map_err(|_| FacadeError::StateCorrupt)?,
            home: self.home.clone(),
            cwd: directory.to_path_buf(),
            assets: self.assets.clone(),
            host_launch,
            mcp_project: self.mcp_project.clone(),
            workers: self.workers,
        })
    }

    #[allow(clippy::disallowed_methods)] // Native read-only store open deadline.
    fn coordination(&self) -> Result<CoordinationService<'_>, CoordinationError> {
        let ctx = &self.context;
        let cancel = CancelToken::new();
        if ctx.in_lane {
            let store = provenant_fabric_store::Store::open_read_only(
                &ctx.roots.state,
                Instant::now() + std::time::Duration::from_secs(5),
            )?
            .ok_or(provenant_fabric_store::Error::NotFound)?
            .live_read_only()?;
            return Ok(CoordinationService::new(
                store,
                ctx.identity.project,
                ctx.identity.inbox.actor,
                &self.clock,
                &self.backoff,
            ));
        }
        CoordinationService::for_identity(
            &ctx.roots.state,
            &ctx.identity,
            &ctx.config,
            &self.home,
            &self.cwd,
            &self.executor,
            &self.clock,
            &self.backoff,
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
        use provenant_fabric_mcp::tools::ToolCall;
        if let provenant_fabric_mcp::tools::ToolCall::Inbox(input, _) = call {
            let ctx = &self.context;
            provenant_fabric_runtime::inbox::validate_request(
                &ctx.identity,
                &ctx.config,
                ctx.in_lane,
                input,
            )?;
        }
        if matches!(
            call,
            ToolCall::Submit(SubmitInput::Submit { host: None, .. })
                | ToolCall::SubmitWait(SubmitInput::Submit { host: None, .. }, ..)
        ) {
            provenant_cli::run::nested_dispatch_context(&self.context.roots).map_err(fail_error)?;
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
                .with_lane_restrictions(self.context.in_lane)
                .with_lane_relay(if self.context.in_lane {
                    std::env::var("PROVENANT_PARENT_ATTEMPT_ID")
                        .ok()
                        .and_then(|id| provenant_core::AttemptId::parse(&id).ok())
                        .map(provenant_providers::LaneRuntime::fabric_socket)
                } else {
                    None
                })
                .with_lane_relay_reads(self.context.in_lane && self.context.remote_home()),
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
            ConsoleInput::Detail { id } => {
                args.extend(["detail".into(), id]);
            }
            ConsoleInput::Search {
                query,
                part,
                session,
                limit,
                cursor,
            } => {
                args.extend([
                    "search".into(),
                    "--part".into(),
                    part,
                    "--limit".into(),
                    limit.to_string(),
                ]);
                if let Some(session) = session {
                    args.extend(["--session".into(), session]);
                }
                if let Some(cursor) = cursor {
                    args.extend(["--cursor".into(), cursor]);
                }
                args.extend(["--".into(), query]);
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
                operation_id,
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
                if let Some(id) = operation_id {
                    args.extend(["--operation-id".into(), id]);
                }
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

impl F3Facade for BoundFacade<'_> {
    fn inbox(
        &mut self,
        input: provenant_fabric_runtime::inbox::InboxInput,
        _budget: WaitBudget,
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
            cancel,
            &|work, call| {
                provenant_cli::inbox::resolve_work(
                    &ctx.roots.state,
                    ctx.identity.project,
                    work,
                    call.clock,
                    call.cancel,
                )
            },
            &|input, _| {
                provenant_cli::inbox::resolve_control(
                    &provenant_cli::run::Edge {
                        roots: &ctx.roots,
                        home: &self.factory.home,
                        assets: &self.factory.assets,
                        cwd: &self.factory.cwd,
                    },
                    &ctx.identity,
                    input,
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
                session: provenant_cli::fabric::current_session(),
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
                    features: Some(newest_feature_protocol()),
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
                    readiness: None,
                });
                continue;
            };
            let mut health = HostHealth {
                protocol: None,
                features: None,
                reason: None,
                host,
                available: false,
                execution: None,
                readiness: Some(provenant_fabric_runtime::hosts::PeerReadiness::not_probed()),
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
                let probe = transport.exchange_with_cap(
                    peer,
                    &request,
                    budget,
                    cancel,
                    std::time::Duration::from_micros(cap),
                );
                health.readiness = Some(
                    provenant_fabric_runtime::hosts::PeerReadiness::from_probe(&probe),
                );
                match probe {
                    Ok(PeerResponse {
                        result: PeerResult::Health { data: value, .. },
                        ..
                    }) => {
                        health.protocol = Some(PROTOCOL);
                        // The wire carries the peer's base fence, not its
                        // feature ceiling, so leave `features` absent rather
                        // than report this release's level for the peer.
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
        FacadeError::Io {
            operation,
            path,
            delivery_limit,
            delivery_stage,
        } => {
            let prefix = format!("{scope}: ");
            let operation = if operation.starts_with(&prefix) {
                operation
            } else {
                format!("{scope}: {operation}")
            };
            FacadeError::Io {
                operation,
                path,
                delivery_limit,
                delivery_stage,
            }
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
    let context = if provenant_cli::fabric::in_lane() {
        Context::open_read_only(resolved.roots, &resolved.home, &resolved.project)?
    } else {
        Context::open(resolved.roots, &resolved.home, &resolved.project)?
    };
    Ok(Factory {
        project_attribution: crate::usage::attribution_enabled(&context.roots),
        context,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: SystemExecutor::new(4)?,
        home: resolved.home,
        cwd: resolved.project.clone(),
        assets: resolved.assets,
        host_launch: None,
        mcp_project: globals.project.as_ref().map(|_| resolved.project),
        workers,
    })
}

/// Wake only expired journals. The snapshot is a discovery hint; the deadline
/// is checked again while holding the same journal lock as explicit advance.
pub(crate) fn tick_graphs(
    resolved: &crate::roots::Resolved,
    now: provenant_core::WallTime,
    workers: &'static dyn WorkerObserver,
) -> Result<crate::tick::Graphs, Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, advance_with_budget};
    let mut report = crate::tick::Graphs::default();
    for binding in provenant_config::registered_projects(&resolved.roots)? {
        let project = binding.project_id();
        let ids = match FileJournal::graph_ids(&resolved.roots.state, project) {
            Ok(ids) => ids,
            Err(error) => {
                report.errors.push(format!("{project}: {error}"));
                continue;
            }
        };
        for id in ids {
            let result = (|| -> Result<bool, Box<dyn std::error::Error>> {
                let Some(checkpoint) =
                    FileJournal::read_checkpoint_for_tick(&resolved.roots.state, project, &id)?
                else {
                    return Ok(false);
                };
                if !graph_wake_due(&checkpoint, now) {
                    return Ok(false);
                }
                let clock = SystemClock::new();
                let cancel = CancelToken::new();
                let budget = WaitBudget::cli(clock.mono_now(), 55)?;
                let mut journal = match FileJournal::open(
                    &resolved.roots.state,
                    project,
                    &id,
                    WaitBudget::nonblocking(clock.mono_now()),
                    &cancel,
                    &clock,
                ) {
                    Ok(journal) => journal,
                    Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
                        provenant_os::fs::FsError::LockTimeout { .. },
                    )) => return Ok(false),
                    Err(error) => return Err(error.into()),
                };
                let Some(checkpoint) = journal.load(&id)? else {
                    return Ok(false);
                };
                if !graph_wake_due(&checkpoint, clock.wall_now()?) {
                    return Ok(false);
                }
                // Execution can be rebound to another worktree by a manual
                // resume, so resolve it only after acquiring journal custody.
                let Some(execution) =
                    FileJournal::pending_execution(&resolved.roots.state, project, &id)?
                else {
                    return Ok(false);
                };
                let context =
                    Context::open(resolved.roots.clone(), &resolved.home, &execution.directory)?;
                if context.identity.project != project || context.in_lane {
                    return Err(EdgeError::Invalid.into());
                }
                let factory = Factory {
                    project_attribution: crate::usage::attribution_enabled(&context.roots),
                    context,
                    clock: SystemClock::new(),
                    backoff: ThreadPause,
                    executor: SystemExecutor::new(4)?,
                    home: resolved.home.clone(),
                    cwd: execution.directory,
                    assets: resolved.assets.clone(),
                    host_launch: None,
                    mcp_project: None,
                    workers,
                };
                let mut owner = BoundGraphOwner {
                    factory: &factory,
                    integration_base: Some(BoundGraphBases {
                        integration: execution.integration_base,
                        review: execution.review_base,
                    }),
                    integration_branch: execution.document.integration_branch.clone(),
                    network: graph_network(&execution.document)?,
                    exclude: graph_exclude(&execution.document)?,
                    budget,
                    cancel: &cancel,
                    prices: None,
                    tick: true,
                };
                let config = &factory.context.config.graph;
                advance_with_budget(
                    &execution.document,
                    &mut owner,
                    &mut journal,
                    &config.limits,
                    &config.budget,
                )?;
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    report.advanced += 1;
                    if let Err(error) =
                        FileJournal::set_advance_error(&resolved.roots.state, project, &id, None)
                    {
                        report.errors.push(format!("{project}/{id}: {error}"));
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    let reason = error.to_string();
                    if !error.is::<provenant_fabric_runtime::graph::UnreadableCheckpointNotice>() {
                        let _ = FileJournal::set_advance_error(
                            &resolved.roots.state,
                            project,
                            &id,
                            Some(&reason),
                        );
                    }
                    report.errors.push(format!("{project}/{id}: {reason}"));
                }
            }
        }
    }
    Ok(report)
}

fn graph_wake_due(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    now: provenant_core::WallTime,
) -> bool {
    if checkpoint.cancelled {
        checkpoint.nodes.iter().any(|node| {
            matches!(
                node.state,
                provenant_fabric_runtime::graph::NodeState::Pending
                    | provenant_fabric_runtime::graph::NodeState::Running
            )
        })
    } else {
        checkpoint.wake_at.is_some_and(|deadline| now >= deadline)
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
            E::Invalid(_) | E::InvalidRemote => FacadeError::invalid(error.to_string()),
            E::DirtyWorktree
            | E::IncompleteRepository
            | E::OidMismatch
            | E::NonFastForward
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
    clock: &SystemClock,
    cancel: &CancelToken,
) -> Result<provenant_fabric_store::PushOperation, Box<dyn std::error::Error>> {
    let request = provenant_fabric_store::Request { clock, cancel };
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
                diagnostic: None,
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
                diagnostic: None,
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
    use provenant_records::diagnostic::DiagnosticFact;
    let fact = fail
        .projection
        .as_ref()
        .and_then(|value| value.fact.as_ref());
    if fail.detail.is_none() {
        match fact {
            Some(DiagnosticFact::CredentialGrant { access }) => {
                return FacadeError::CredentialRoot {
                    access: (*access).into(),
                };
            }
            Some(DiagnosticFact::DeliveryLimit(fact)) => {
                return FacadeError::HostDeliveryLimit { fact: fact.clone() };
            }
            Some(DiagnosticFact::DeliveryStage(stage)) => {
                return FacadeError::HostDeliveryStage { stage: *stage };
            }
            Some(DiagnosticFact::GenerationConflict(current_generation)) => {
                return FacadeError::GenerationConflict {
                    current_generation: *current_generation,
                };
            }
            Some(DiagnosticFact::WorktreeUnregistered) => {
                return FacadeError::WorktreeUnregistered {
                    path: "inspect worktree registration".into(),
                };
            }
            Some(DiagnosticFact::Filesystem { operation }) => return FacadeError::io(*operation),
            Some(DiagnosticFact::DiskPressure {
                available,
                floor,
                reclaimed,
            }) => {
                return FacadeError::DispatchRefused {
                    fact: None,
                    reason: "insufficient disk space".into(),
                    disk: Some(
                        provenant_fabric_runtime::coordination::facade::DiskPressure {
                            available: *available,
                            floor: *floor,
                            reclaimed: *reclaimed,
                        },
                    ),
                };
            }
            Some(DiagnosticFact::DiskUnobserved) => {
                return FacadeError::DispatchUnobserved {
                    reason: "disk observation failed".into(),
                };
            }
            None => {}
        }
    }
    match fail.detail.map(|detail| *detail) {
        Some(FacadeError::WorktreeUnregistered { .. }) => FacadeError::WorktreeUnregistered {
            path: "inspect worktree registration".into(),
        },
        Some(FacadeError::Io {
            operation,
            delivery_limit,
            delivery_stage,
            ..
        }) => FacadeError::Io {
            delivery_limit,
            delivery_stage,
            operation,
            path: None,
        },
        Some(detail) => detail,
        None => facade_error_for_kind(fail.kind),
    }
}
/// Translate a submission's refusal at the facade edge. Dispatch access
/// failures keep their own message; rendering it by kind would read as
/// invalid input instead of a refusal.
fn dispatch_refused(fail: provenant_cli::run::Fail) -> FacadeError {
    if matches!(
        fail.projection
            .as_ref()
            .and_then(|projection| projection.fact.as_ref()),
        Some(provenant_records::diagnostic::DiagnosticFact::DiskPressure { .. })
    ) {
        return fail_error(fail);
    }
    match fail.detail.map(|detail| *detail) {
        Some(
            detail @ FacadeError::DispatchRefused {
                fact: None,
                disk: Some(_),
                ..
            },
        ) => detail,
        _ => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: "dispatch was refused; inspect the request and access policy".into(),
        },
    }
}
/// Translate a typed run category at the MCP edge. `ErrorKind` is the one
/// categorisation of run errors; this only renders it.
fn facade_error_for_kind(kind: provenant_run::ErrorKind) -> FacadeError {
    use provenant_run::ErrorKind as E;
    match kind {
        E::SecretInput
        | E::Refused
        | E::Unsupported
        | E::WriterScope
        | E::Confinement
        | E::DiskFloor
        | E::MemoryPressure
        | E::LaneCap
        | E::BuildSettings
        | E::GuardianIdentity => FacadeError::ConfinementRefused,
        E::WriterBusy => FacadeError::WriterBusy,
        E::WriterLockBusy => FacadeError::WriterLockBusy,
        E::WriterUnknown => FacadeError::WriterUnknown,
        E::CredentialRoot => FacadeError::CredentialRoot {
            access: "requested".into(),
        },
        E::LaneDepth => FacadeError::LaneDepth,
        E::LaneContext
        | E::ParentAttempt
        | E::Invalid
        | E::WorktreeRequired
        | E::WorktreeNotApplicable => FacadeError::InvalidInput { detail: None },
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
    fn call<'a>(&'a self, cancel: &'a CancelToken) -> provenant_run::Request<'a> {
        provenant_run::Request {
            clock: &self.factory.clock,
            cancel,
        }
    }
    fn session(&self, cancel: &CancelToken) -> Result<provenant_cli::run::Session, FacadeError> {
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
        let store = provenant_run::RunStore::open(&context, &self.call(cancel))
            .map_err(run_error)?
            .with_host(ctx.identity.host);
        Ok(provenant_cli::run::Session { store, context })
    }
    fn store(&self, cancel: &CancelToken) -> Result<provenant_run::RunStore, FacadeError> {
        Ok(self.session(cancel)?.store)
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
            let host = target
                .host
                .ok_or(FacadeError::InvalidInput { detail: None })?;
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
        provenant_run::Service::new(&self.store(cancel)?)
            .status(
                target.attempt,
                &provenant_run::Request {
                    clock: &self.factory.clock,
                    cancel,
                },
            )
            .map_err(run_error)
    }
    /// Read the current policy successor on its owning host without launching work.
    fn graph_policy_tail(
        &self,
        target: AttemptTarget,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_records::run::AttemptRecord, FacadeError> {
        let host = target.host.unwrap_or(self.factory.context.identity.host);
        if host != self.factory.context.identity.host {
            if !self.allow_remote {
                return Err(FacadeError::ConfinementRefused);
            }
            return self.graph_policy_tail_remote(target, budget, cancel);
        }
        let store = self.store(cancel)?;
        let call = self.call(cancel);
        let service = provenant_run::Service::new(&store);
        let mut tail = store
            .policy_tail(target.attempt, &call)
            .map_err(run_error)?
            .attempt_id;
        // Settlement terminates an attempt and queues its successor in one
        // transaction, so it can land between the tail lookup and the status
        // read. Accept a status only while its attempt is still the tail; a
        // terminal predecessor must never stand in for a live successor.
        loop {
            let record = service.status(tail, &call).map_err(run_error)?;
            let current = store
                .policy_tail(target.attempt, &call)
                .map_err(run_error)?
                .attempt_id;
            if current == tail {
                return Ok(record);
            }
            tail = current;
        }
    }

    /// Peers expose the host-authored chain through list and status.
    fn graph_policy_tail_remote(
        &self,
        target: AttemptTarget,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_records::run::AttemptRecord, FacadeError> {
        // One pass lists the run, then reads each candidate's state, so a
        // successor queued after the list read is invisible to that pass. Settlement
        // commits a terminal state and its successor together, so a later pass that
        // lists after the earlier pass's read and finds the same tail proves the
        // earlier record has no successor. Return that earlier record.
        confirm_stable_tail(
            || self.graph_policy_tail_remote_pass(target.clone(), budget, cancel),
            |record| record.attempt_id,
        )
    }

    fn graph_policy_tail_remote_pass(
        &self,
        target: AttemptTarget,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_records::run::AttemptRecord, FacadeError> {
        let host = target
            .host
            .ok_or(FacadeError::InvalidInput { detail: None })?;
        let mut current = self.snapshot(target.clone(), budget, cancel)?;
        let mut candidates = std::collections::BTreeMap::new();
        let mut after = None;
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let result = self.peer(
                host,
                PeerAction::Run {
                    extra: Default::default(),
                    input: RunInput::List {
                        extra: Default::default(),
                        after: after.clone(),
                        limit: 100,
                    },
                },
                budget,
                cancel,
            )?;
            let PeerResult::Run { data, .. } = result else {
                return Err(FacadeError::StateCorrupt);
            };
            let RunResult::List { page, .. } = *data else {
                return Err(FacadeError::StateCorrupt);
            };
            let mut reached = false;
            for item in page.items {
                if item.attempt_id == target.attempt {
                    reached = true;
                    break;
                }
                if item.run_id == current.run_id && item.task_id == current.task_id {
                    let candidate = self.snapshot(
                        AttemptTarget {
                            extra: Default::default(),
                            host: Some(host),
                            attempt: item.attempt_id,
                        },
                        budget,
                        cancel,
                    )?;
                    if candidates
                        .insert(candidate.attempt_number, candidate)
                        .is_some()
                    {
                        return Err(FacadeError::StateCorrupt);
                    }
                }
            }
            if reached {
                break;
            }
            let next = page.next_after.ok_or(FacadeError::StateCorrupt)?;
            if !seen.insert(next.attempt) {
                return Err(FacadeError::StateCorrupt);
            }
            after = Some(next);
        }
        for _ in 0..provenant_run::POLICY_ATTEMPT_CEILING {
            let Some(next) = candidates.remove(&current.attempt_number.saturating_add(1)) else {
                return Ok(current);
            };
            if next.retry_of != Some(current.attempt_id)
                || !matches!(
                    next.continuation.as_deref(),
                    Some("policy_continuation" | "policy_fallback")
                )
            {
                return Ok(current);
            }
            current = next;
        }
        Err(FacadeError::StateCorrupt)
    }

    fn snapshots_local(
        &self,
        targets: &[AttemptTarget],
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::AttemptRecord>, FacadeError> {
        let host = self.factory.context.identity.host;
        if targets
            .iter()
            .any(|target| target.host.is_some_and(|target_host| target_host != host))
        {
            return Err(FacadeError::InvalidInput { detail: None });
        }
        let session = self.session(cancel)?;
        let call = self.call(cancel);
        targets
            .iter()
            .map(|target| {
                let mut record = session
                    .service()
                    .status_for_wait(target.attempt, &call)
                    .map_err(run_error)?;
                let tail = session
                    .store
                    .policy_tail(target.attempt, &call)
                    .map_err(run_error)?;
                record.superseded_by =
                    (tail.attempt_id != target.attempt).then_some(tail.attempt_id);
                Ok(record)
            })
            .collect()
    }
    /// This host's lanes for `PeerAction::Lanes`: active ones oldest first,
    /// or with `all` the newest retained ones.
    fn lanes_local(
        &self,
        all: bool,
        limit: u16,
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::AttemptRecord>, FacadeError> {
        if !(1..=100).contains(&limit) {
            return Err(FacadeError::InvalidInput { detail: None });
        }
        let store = self.store(cancel)?;
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
                &self.call(cancel),
            )
            .map_err(run_error)?;
        let mut lanes = page.lanes;
        for lane in &mut lanes {
            let tail = store
                .policy_tail(lane.attempt_id, &self.call(cancel))
                .map_err(run_error)?;
            lane.superseded_by = (tail.attempt_id != lane.attempt_id).then_some(tail.attempt_id);
        }
        Ok(lanes)
    }
    /// The wait of each queued or starting lane among `targets`, for `PeerAction::LaneWaits`.
    fn waits_local(
        &self,
        targets: &[AttemptTarget],
        cancel: &CancelToken,
    ) -> Result<Vec<provenant_records::run::LaneWait>, FacadeError> {
        let lanes = self.snapshots_local(targets, cancel)?;
        let store = self.store(cancel)?;
        Ok(store.lane_waits(&lanes, &self.call(cancel)))
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
            None,
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
        settlement_retry_limit: Option<u8>,
    ) -> Result<SubmissionReceipt, FacadeError> {
        self.submit_inner(input, budget, cancel, access, settlement_retry_limit)
            .map_err(|error| scope_io(error, "submit"))
    }
    fn submit_inner(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        access: provenant_records::run::LaneAccess,
        settlement_retry_limit: Option<u8>,
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
                    return Err(FacadeError::InvalidInput { detail: None });
                }
                continue;
            };
            if !ingress
                || created.is_some()
                || task.worktree.is_some()
                || task.access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            {
                return Err(FacadeError::InvalidInput { detail: None });
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
                settlement_retry_limit,
                context: task.context,
                build_heavy: task.build_heavy,
                base: task.base,
                pinned_base: None,
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
        let session = self.session(cancel)?;
        let call = self.call(cancel);
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
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        provenant_confinement::repo::GitRevision::new(&spec.base)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        let replay = self
            .store(cancel)?
            .operation(operation_id, &self.call(cancel))
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
                    Some(info) => Err(WorktreeError::BranchExistsWithBase {
                        branch: spec.branch.clone(),
                        head: info.head,
                        base,
                    }
                    .into()),
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
            // The branch name is valid, so either refusal means it already exists.
            crate::worktrees::Error::Worktree(
                WorktreeError::InvalidRequest | WorktreeError::BranchExistsWithBase { .. },
            ) if !replay => FacadeError::BranchExists,
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
        let session = self.session(cancel)?;
        let call = self.call(cancel);
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
                    OutputPart::Events => return Err(FacadeError::InvalidInput { detail: None }),
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
                // The serving side reads its own graph, so `output --host` can
                // name the planner fallback it recorded.
                let mut extra = provenant_records::ExtraFields::new();
                extra.insert("length".into(), serde_json::Value::from(chunk.len));
                if matches!(part, OutputPart::Result)
                    && let Some(reason) = provenant_cli::run::planner_fallback_reason(
                        &self.factory.context.roots.state,
                        session.context.project_id,
                        &record,
                    )
                {
                    extra.insert(
                        provenant_cli::run::PLANNER_FALLBACK_KEY.into(),
                        serde_json::Value::String(reason),
                    );
                }
                Ok(OutputResult::Chunk {
                    extra,
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
                            .ok_or(FacadeError::InvalidInput { detail: None })?,
                    ),
                    provenant_core::WaitTransport::Cli,
                )
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                let budget =
                    if requested.deadline().as_mono_time() < budget.deadline().as_mono_time() {
                        requested
                    } else {
                        budget
                    };
                if !(1..=100).contains(&targets.len()) {
                    return Err(FacadeError::InvalidInput { detail: None });
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
                    Some(self.session(cancel)?)
                };
                let local_call = self.call(cancel);
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
                    provenant_core::Pause::wait(
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
                let store = self.store(cancel)?;
                let call = self.call(cancel);
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
                let session = self.session(cancel)?;
                let call = self.call(cancel);
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
                    return Err(FacadeError::InvalidInput { detail: None });
                }
                let host = self.factory.context.identity.host;
                if after
                    .as_ref()
                    .and_then(|target| target.host)
                    .is_some_and(|cursor_host| cursor_host != host)
                {
                    return Err(FacadeError::OwnerUnavailable);
                }
                let store = self.store(cancel)?;
                let call = self.call(cancel);
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
        cancel: &CancelToken,
    ) -> Result<RunEventsPage, FacadeError> {
        if !(1..=64).contains(&limit) {
            return Err(FacadeError::InvalidInput { detail: None });
        }
        let store = self.store(cancel)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
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

#[derive(Clone, Copy)]
struct BoundGraphBases {
    integration: Option<provenant_core::GitOid>,
    review: Option<provenant_core::GitOid>,
}

struct BoundGraphOwner<'a> {
    factory: &'a Factory,
    // Both immutable graph bases travel together through every host owner.
    integration_base: Option<BoundGraphBases>,
    integration_branch: Option<String>,
    /// The graph's explicit network reach for every node lane, such as
    /// `run --network off` bound before automatic preset selection.
    network: Option<provenant_cli::Switch>,
    /// The graph's route-exclusion selectors, passed to each node as the
    /// `routing.graph_exclude` knob.
    exclude: Vec<String>,
    budget: WaitBudget,
    cancel: &'a CancelToken,
    prices: Option<Result<provenant_telemetry::PriceTable, String>>,
    tick: bool,
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
                // The inbox needs a description; a decide option may leave it out.
                description: if option.description.is_empty() {
                    option.label.clone()
                } else {
                    option.description.clone()
                },
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
    cancel: &CancelToken,
) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    use provenant_records::inbox::RequestState;
    let inbox_error =
        |error: provenant_fabric_runtime::inbox::InboxError| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!("graph ask {}: {error}", scope.ask.ask_id),
        };
    // The immutable ask and its operation receipt commit together. A crash before
    // saving graph state replays this exact request, including its original ID.
    let InboxResult::Saved { receipt } = service
        .execute(graph_ask_input(scope)?, cancel)
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
        fact: None,
        disk: None,
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
    let service = InboxService::open(&context.roots.state, &identity, &factory.clock, cancel)
        .map_err(|error| refused(&error))?;
    if close {
        graph_ask_close(&service, &scope, identity.inbox.actor, cancel)?;
        Ok(None)
    } else {
        graph_ask_poll(&service, &scope, cancel)
    }
}

fn graph_ask_close(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    actor: provenant_core::ActorId,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    let InboxInput::Ask {
        operation_id: Some(operation),
        ..
    } = graph_ask_input(scope)?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    graph_inbox_close(
        service,
        &scope.ask.session,
        &operation,
        actor,
        &format!("close graph ask {}", scope.ask.ask_id),
        cancel,
    )
}

/// Supersede the open request raised under `operation` by `actor`, if any.
/// Closing never creates a request, and a closed one is left as it is.
fn graph_inbox_close(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    session: &str,
    operation: &provenant_core::OperationId,
    actor: provenant_core::ActorId,
    context: &str,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    use provenant_records::inbox::RequestState;
    let map = |e: provenant_fabric_runtime::inbox::InboxError| FacadeError::DispatchRefused {
        fact: None,
        disk: None,
        reason: format!("{context}: {e}"),
    };
    // Find the request through the normal bounded inbox list.
    let mut cursor = None;
    loop {
        let InboxResult::Listed {
            items, next_cursor, ..
        } = service
            .execute(
                InboxInput::List {
                    extra: Default::default(),
                    id: None,
                    session: Some(session.into()),
                    state: InboxFilter::All,
                    limit: 50,
                    cursor,
                },
                cancel,
            )
            .map_err(map)?
        else {
            return Err(FacadeError::StateCorrupt);
        };
        if let Some(item) = items
            .into_iter()
            .find(|item| &item.operation_id == operation && item.chair_actor == actor)
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

/// Close the loop requests a checkpoint from before node asks raised under
/// `actor`, by their original inbox operation. Only cancel does this; such a
/// checkpoint is never migrated.
fn graph_legacy_loop_close(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    actor: provenant_core::ActorId,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    let Some(instance_id) = checkpoint.instance_id.as_deref() else {
        return Ok(());
    };
    let asks = provenant_fabric_runtime::graph::legacy_loop_asks(checkpoint);
    for ask in asks.iter().filter(|ask| ask.actor == actor.to_string()) {
        let digest = provenant_records::CanonicalV1::record_digest(
            "graph-loop-inbox",
            1,
            &(
                checkpoint.graph_id.as_str(),
                instance_id,
                ask.node_id.as_str(),
                ask.rounds,
                ask.session.as_str(),
            ),
        )
        .map_err(|_| FacadeError::StateCorrupt)?;
        let operation = provenant_core::OperationId::parse(&format!(
            "loop-{}",
            provenant_core::hex(digest.as_bytes())
        ))
        .map_err(|_| FacadeError::StateCorrupt)?;
        graph_inbox_close(
            service,
            &ask.session,
            &operation,
            actor,
            &format!("close legacy loop ask {}", ask.node_id),
            cancel,
        )?;
    }
    Ok(())
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
    let value: serde_json::Value =
        provenant_records::decode(bytes, provenant_records::Limits::default())
            .map_err(|_| FacadeError::invalid("graph document is not a valid record"))?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(1 | 2) => serde_json::from_value(value)
            .map_err(|error| FacadeError::invalid(format!("graph document: {error}"))),
        Some(version) if version > 2 => Err(FacadeError::UpgradeRequired {
            reader_release: env!("CARGO_PKG_VERSION").into(),
            writer_release: "unknown".into(),
            record: "graph document schema".into(),
            version,
        }),
        _ => Err(FacadeError::invalid(
            "graph document schema_version must be 1 or 2",
        )),
    }
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
            fact: None,
            disk: None,
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
        provenant_run::RunError::ArtifactMismatch(_) => {
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

/// The one post-settlement seam. Every settlement, including a partial or
/// stopped-early one, passes through here once, and crash recovery drains the
/// same reactions through `settlement::drain_run`. A staged successor launches
/// around the reactions, and a delayed retry reacts before its wait. A failed
/// launch or reaction never stops the other, and the first error in time order
/// is reported. The reactions run in order: graphs advance, the chair is
/// notified, then the chair log is appended.
pub(crate) fn advance_settled_graphs(
    holder: &provenant_cli::run::HostHolder<'_>,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(settled) = holder.settled()? else {
        return Ok(());
    };
    crate::settlement::seam(
        |before| provenant_cli::run::launch_policy_successor(&settled, before),
        || {
            settlement_reactions(
                &crate::settlement::Subject {
                    home: settled.roots.home.clone(),
                    checkout: settled.roots.checkout.clone(),
                    state_root: settled.roots.state_root.clone(),
                    project: settled.project_id,
                    actor: settled.actor_id,
                    run: settled.run_id,
                },
                settled.launch.clone(),
                workers,
            )
        },
    )
}

/// Launch inputs for a graph advance when no live guardian supplies them,
/// derived the way a fresh host launch derives them: from this trusted process
/// and the recovery edge, never from the run store, which a lane can rewrite.
/// The graph's own directory comes from the host-owned graph journal.
pub(crate) fn recovery_launch(
    state_root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<provenant_run::HostLaunchContext, Box<dyn std::error::Error>> {
    Ok(provenant_run::HostLaunchContext::from_environment(
        state_root,
        cwd,
        provenant_os::git::git_executable().ok_or("Git unavailable")?,
    ))
}

/// Drain the reactions a settlement owes: the graph advance, then the notice
/// and the chair log. `live` is the launch context of the guardian that
/// settled, which only the host holds; recovery has none and derives its own.
/// If those cannot be derived the graph reaction stays pending.
pub(crate) fn settlement_reactions(
    subject: &crate::settlement::Subject,
    live: Option<provenant_run::HostLaunchContext>,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    crate::settlement::drain_run(subject, &mut |reaction| {
        let launch = match &live {
            Some(live) => live.clone(),
            None => recovery_launch(&subject.state_root, &subject.checkout)?,
        };
        let settled = provenant_run::HolderContext {
            schema_version: 1,
            roots: provenant_run::RootSeed {
                home: subject.home.clone(),
                checkout: subject.checkout.clone(),
                state_root: subject.state_root.clone(),
            },
            project_id: reaction.project,
            actor_id: subject.actor,
            run_id: reaction.run,
            attempt_id: reaction.attempt,
            host_delivery: None,
            launch: Some(launch),
            sealed: None,
        };
        advance_graphs_after_settlement(&settled, workers)
    })
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
                backoff: ThreadPause,
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
                integration_base: Some(BoundGraphBases {
                    integration: execution.integration_base,
                    review: execution.review_base,
                }),
                integration_branch: execution.document.integration_branch.clone(),
                network: graph_network(&execution.document)?,
                exclude: graph_exclude(&execution.document)?,
                budget,
                cancel: &cancel,
                prices: None,
                tick: false,
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
                                let guard =
                                    worktrees::cache_writer_guard(context.project_state, path)?;
                                if !terminal(path) || !no_live(path) {
                                    return Err(worktrees::WorkerState::Unknown);
                                }
                                Ok(Some(guard))
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
        cancel: &cancel,
    };
    Ok(store.worktree_settlement(id, &call)?)
}

impl BoundGraphOwner<'_> {
    fn prior_models(&self, attempts: &[String]) -> Result<Vec<String>, FacadeError> {
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let mut models = Vec::new();
        for reference in attempts {
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            if let Some(model) = owner
                .snapshot(target, self.budget, self.cancel)?
                .applied_route
                .and_then(|route| route.model)
            {
                models.push(model);
            }
        }
        Ok(models)
    }

    /// The graph exclusions plus, on an alias retry, the models that earlier
    /// attempts of this node already used. The runtime resolves those models
    /// from the attempt records, so they arrive as model ids, not references.
    fn route_exclusions(
        &self,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        prior_models: &[String],
        accepted: bool,
    ) -> Vec<String> {
        let mut exclude = self.exclude.clone();
        if !accepted && route.alias.is_some() {
            for model in prior_models {
                if !exclude.contains(model) {
                    exclude.push(model.clone());
                }
            }
        }
        exclude
    }

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
        failed_checks_max_bytes: usize,
        max_failed_checks: usize,
    ) -> provenant_fabric_runtime::graph::CiObservation {
        crate::pr::status::observe_ci(
            &self.factory.cwd,
            target,
            checks,
            failed_checks_max_bytes,
            max_failed_checks,
        )
    }
}

// Review roles come from the shared role library. The framing role
// `plan.review` prepares the subject and is not an independent review.
fn graph_review_node(document: &provenant_fabric_runtime::graph::GraphDocument, id: &str) -> bool {
    document.nodes.iter().any(|node| {
        node.id == id
            && node.access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
            && node.role.as_deref().is_some_and(|role| {
                role == "lens" || role.starts_with("lens.") || role.starts_with("review.")
            })
    })
}

/// What the host learned about a deferred review's pinned subject.
struct ReviewMeasure {
    routing: provenant_routing::RoutingConfig,
    directory: PathBuf,
    subject: provenant_core::GitOid,
    base: provenant_core::GitOid,
    measured: provenant_routing::Measured,
}

impl BoundGraphOwner<'_> {
    fn review_git(&self, directory: &Path, args: &[&str]) -> Result<String, String> {
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let git = provenant_os::git::git_executable()
            .ok_or_else(|| "Git executable unavailable".to_owned())?;
        let mut argv = vec![git.into_os_string()];
        argv.extend(args.iter().map(std::ffi::OsString::from));
        let output = self
            .factory
            .executor
            .execute(ExecOptions::new(
                &argv,
                directory,
                std::time::Duration::from_secs(30),
            ))
            .map_err(|error| error.to_string())?;
        if output.termination != Termination::Exit(0) || output.timed_out || output.stdout.truncated
        {
            return Err(format!(
                "Git failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes).trim()
            ));
        }
        String::from_utf8(output.stdout.bytes).map_err(|error| error.to_string())
    }

    fn measure_review(
        &self,
        node: &provenant_fabric_runtime::graph::GraphNode,
        subject: &str,
    ) -> Result<ReviewMeasure, provenant_fabric_runtime::graph::ReviewResolveError> {
        use provenant_fabric_runtime::graph::ReviewResolveError;
        let spec = node
            .review
            .as_ref()
            .ok_or_else(|| ReviewResolveError::Failed("missing review specification".into()))?;
        let subject = provenant_core::GitOid::parse(subject).map_err(|error| {
            ReviewResolveError::Failed(format!("invalid pinned review subject: {error}"))
        })?;
        let base = self
            .integration_base
            .and_then(|bases| bases.review.or(bases.integration))
            .unwrap_or(subject);
        let directory = spec.worktree.as_deref().map_or_else(
            || self.factory.cwd.clone(),
            |path| self.factory.cwd.join(path),
        );
        let routing: provenant_routing::RoutingConfig =
            provenant_config::read_domain(self.factory.context.roots.state.as_path(), "routing")
                .map_err(|error| {
                    ReviewResolveError::Failed(format!("read review configuration: {error}"))
                })?;
        let measured = routing.review.size.measure(
            spec.kind.as_deref(),
            &base.to_string(),
            &subject.to_string(),
            &mut |args| self.review_git(&directory, args),
        );
        Ok(ReviewMeasure {
            routing,
            directory,
            subject,
            base,
            measured,
        })
    }

    /// The lens members and composite root for a resolved panel, built from the
    /// lowered lenses the preset stored on the deferred node. Every selected
    /// lens runs: a family preference only labels it, and a lens the panel has
    /// no room for is refused by name rather than dropped.
    fn review_expansion(
        node: &provenant_fabric_runtime::graph::GraphNode,
        measure: &ReviewMeasure,
        panel: &provenant_routing::PanelResolution,
        source: String,
    ) -> Result<
        provenant_fabric_runtime::graph::Expansion,
        provenant_fabric_runtime::graph::ReviewResolveError,
    > {
        use provenant_fabric_runtime::graph::{
            Expansion, GraphNode, ReviewResolution, ReviewResolveError,
        };
        let failed = |message: String| ReviewResolveError::Failed(message);
        let spec = node
            .review
            .as_ref()
            .ok_or_else(|| failed("missing review specification".into()))?;
        let templates = node
            .extra
            .get("lens_templates")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failed("deferred review carries no lens templates".into()))?;
        let root_id = format!("{}--review", node.id);
        let subject = measure.subject.to_string();
        let mut members: Vec<GraphNode> = Vec::new();
        for slot in &panel.slots {
            let mut template = templates
                .get(&slot.lens)
                .cloned()
                .ok_or_else(|| failed(format!("no lowered lens for {}", slot.lens)))?;
            let map = template
                .as_object_mut()
                .ok_or_else(|| failed(format!("lens template {} is not a table", slot.lens)))?;
            let local = map
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| failed(format!("lens template {} has no id", slot.lens)))?
                .to_owned();
            let effort = map.remove("effort");
            let read_only = map.get("access").is_none_or(|access| access == "read_only");
            let (key, route) =
                crate::preset::tier_route(&local, &slot.tier, effort.as_ref(), read_only)
                    .map_err(failed)?;
            map.insert(key.into(), route);
            map.insert(
                "id".into(),
                serde_json::json!(format!("{root_id}--{local}")),
            );
            map.insert("depends_on".into(), serde_json::json!(node.depends_on));
            if !node.inputs.is_empty() {
                map.insert("inputs".into(), serde_json::json!(node.inputs));
            }
            if !node.read_roots.is_empty() {
                map.insert("read_roots".into(), serde_json::json!(node.read_roots));
            }
            map.insert(
                "source".into(),
                serde_json::json!({"type":"commit","commit":subject}),
            );
            if slot.distinct_families {
                map.insert(
                    "family_group".into(),
                    serde_json::json!(crate::preset::format_family_group(slot.group_index)),
                );
            }
            let member: GraphNode = serde_json::from_value(template)
                .map_err(|error| failed(format!("lens {} does not lower: {error}", slot.lens)))?;
            if members.iter().any(|existing| existing.id == member.id) {
                return Err(failed(format!("lens {} is selected twice", slot.lens)));
            }
            members.push(member);
        }
        if members.is_empty() {
            return Err(failed("the review panel selected no lenses".into()));
        }
        if members.len() + 1 > spec.max_nodes {
            return Err(failed(format!(
                "the review panel selected {} lenses; the review allows {}",
                members.len(),
                spec.max_nodes.saturating_sub(1)
            )));
        }
        let exports: Vec<_> = members
            .iter()
            .map(|member| serde_json::json!({"node": member.id, "outputs": ["result", "findings"]}))
            .collect();
        let root: GraphNode = serde_json::from_value(serde_json::json!({
            "id": root_id,
            "kind": "composite",
            "access": "read_only",
            "depends_on": node.depends_on,
            "composite": {
                "min_successful": spec.min_successful.map(|n| if n == 0 { members.len().saturating_sub(1).max(1) } else { n }),
                "nodes": [],
                "exports": exports,
            },
        }))
        .map_err(|error| failed(format!("review composite does not lower: {error}")))?;
        let mut nodes = vec![root];
        nodes.extend(members);
        Ok(Expansion {
            review: Some(ReviewResolution {
                subject: measure.subject,
                base: measure.base,
                measured_size: measure.measured.size.unwrap_or("unknown").to_owned(),
                shape: panel.shape.clone(),
                source,
            }),
            extra: Default::default(),
            source: node.id.clone(),
            continuation: false,
            nodes,
            patterns: Vec::new(),
        })
    }
}

impl provenant_fabric_runtime::graph::AttemptOwner for BoundGraphOwner<'_> {
    fn launch_retry_policy(&self) -> provenant_fabric_runtime::graph::LaunchRetryPolicy {
        use provenant_config::KnobSource;
        let mut policy = provenant_fabric_runtime::graph::LaunchRetryPolicy::default();
        let config = match provenant_config::read_with_domains(
            &self.factory.context.roots,
            &provenant_cli::domains::SECTIONS,
        ) {
            Ok(config) => config,
            Err(error) => {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: graph launch retry settings unavailable: {error}; using defaults"
                );
                return policy;
            }
        };
        if let Some(knob) = config.knobs.get("lanes.launch_retry_limit") {
            if let Some(value) = knob.value.as_u64().and_then(|n| u8::try_from(n).ok()) {
                policy.limit = value;
            }
            policy.limit_source = match knob.source {
                KnobSource::Default => "default",
                KnobSource::Profile => "profile",
                KnobSource::Override => "override",
            }
            .into();
        }
        if let Some(value) = config
            .knobs
            .get("lanes.launch_retry_first_seconds")
            .and_then(|knob| knob.value.as_u64())
        {
            policy.first_seconds = value;
        }
        if let Some(value) = config
            .knobs
            .get("lanes.launch_retry_max_seconds")
            .and_then(|knob| knob.value.as_u64())
        {
            policy.max_seconds = value;
        }
        policy
    }

    fn admission_timeout_seconds(&self) -> Option<u64> {
        let configured = provenant_confinement::run_config::RunConfig::read(
            self.factory.context.roots.state.as_path(),
        )
        .ok()
        .and_then(|config| {
            config.admission_seconds_for(&self.factory.context.identity.project.to_string())
        });
        let effective = provenant_run::AdmissionPolicy::resolve_timeout(None, configured);
        Some(effective.seconds)
    }
    type Error = FacadeError;
    fn resolve_review_base(
        &mut self,
        reference: &str,
        worktree: Option<&str>,
    ) -> Result<Option<String>, Self::Error> {
        let git = provenant_os::git::git_executable()
            .ok_or(FacadeError::InvalidInput { detail: None })?;
        let directory = worktree.map_or_else(
            || self.factory.cwd.clone(),
            |path| self.factory.cwd.join(path),
        );
        provenant_run::resolve_git_base(&git, &directory, reference)
            .map(Some)
            .map_err(run_error)
    }

    fn retry_allowance(
        &self,
        node: &provenant_records::ExtraFields,
        graph: &provenant_records::ExtraFields,
    ) -> Result<Option<(u8, String)>, Self::Error> {
        Ok(Some(graph_retry_allowance(
            node,
            graph,
            &self.factory.context.roots,
        )))
    }

    fn retry_successors(&mut self, reference: &str) -> Result<u64, FacadeError> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        if host != self.factory.context.identity.host.to_string() {
            // Unknown remote lineage must never grant fresh retry allowance.
            return Ok(u64::from(provenant_routing::MAX_SETTLEMENT_RETRIES));
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store(self.cancel)?;
        let attempt = attempt.parse().map_err(|_| FacadeError::StateCorrupt)?;
        let call = owner.call(self.cancel);
        let record = store.record(attempt, &call).map_err(run_error)?;
        Ok(store
            .policy_attempt_number(&record, &call)
            .map_err(run_error)?
            .saturating_sub(1))
    }

    fn preparation_fallback_evidence(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::PreparationFallbackEvidence>, Self::Error>
    {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        // Only this host can verify its writer fences and custody lock.
        if host != self.factory.context.identity.host.to_string() {
            return Ok(None);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store(self.cancel)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        if record.operation_id != graph_operation("graph-attempt-operation", key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            })
        {
            return Ok(None);
        }
        let custody_released = store.custody_released(&record, &call).map_err(run_error)?;
        Ok(record.preparation_failure.map(|failure| {
            provenant_fabric_runtime::graph::PreparationFallbackEvidence {
                failure,
                handoff: if record.provider_launched == Some(false) {
                    provenant_routing::FallbackHandoff::Local
                } else {
                    provenant_routing::FallbackHandoff::UncertainOrLaunched
                },
                custody_released,
            }
        }))
    }

    fn resolve_review(
        &mut self,
        node: &provenant_fabric_runtime::graph::GraphNode,
        subject: &str,
    ) -> Result<
        provenant_fabric_runtime::graph::Expansion,
        provenant_fabric_runtime::graph::ReviewResolveError,
    > {
        use provenant_fabric_runtime::graph::ReviewResolveError;
        let measure = self.measure_review(node, subject)?;
        let spec = node
            .review
            .as_ref()
            .ok_or_else(|| ReviewResolveError::Failed("missing review specification".into()))?;
        // Changed paths and test markers add lenses; a failed read only costs
        // those additions, and the reason is recorded with the resolution.
        let mut warnings: Vec<String> = measure.measured.warning.iter().cloned().collect();
        let (mut paths, mut marker) = (Vec::new(), false);
        let range = format!("{}...{}", measure.base, measure.subject);
        if measure.base != measure.subject {
            match self.review_git(
                &measure.directory,
                &["diff", "--no-renames", "--name-only", "-z", &range, "--"],
            ) {
                Ok(out) => {
                    paths = out
                        .split('\0')
                        .filter(|path| !path.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                Err(error) => warnings.push(format!("cannot read changed paths: {error}")),
            }
            match self.review_git(
                &measure.directory,
                &["diff", "--no-ext-diff", "--unified=0", &range, "--"],
            ) {
                Ok(out) => marker = provenant_routing::diff_has_test_markers(&out),
                Err(error) => warnings.push(format!("cannot scan test markers: {error}")),
            }
        }
        let request = provenant_routing::ReviewRequest {
            shape: spec.shape.clone(),
            risk_tier: spec.risk_tier.clone(),
            kind: spec.kind.clone(),
            size: spec.size.clone(),
            risk_tags: spec.risk_tags.clone(),
            lenses: spec
                .lenses
                .iter()
                .map(|lens| provenant_routing::ReviewLens {
                    name: lens.name.clone(),
                    when: lens.when.clone(),
                    tier: lens.tier.clone(),
                })
                .collect(),
        };
        let panel = measure
            .routing
            .review
            .resolve_panel(&request, measure.measured.size, &paths, marker)
            .map_err(ReviewResolveError::Failed)?;
        let mut source = format!(
            "{}; size={}; kind={}; risk={}",
            panel.source,
            spec.size
                .as_deref()
                .or(measure.measured.size)
                .unwrap_or("unknown"),
            spec.kind.as_deref().unwrap_or("unknown"),
            spec.risk_tier.as_deref().unwrap_or("unknown"),
        );
        if !panel.omitted.is_empty() {
            let omitted: Vec<String> = panel
                .omitted
                .iter()
                .map(|lens| format!("{} ({})", lens.lens, lens.reason))
                .collect();
            source.push_str(&format!("; omitted={}", omitted.join(", ")));
        }
        if !warnings.is_empty() {
            source.push_str(&format!("; warnings={}", warnings.join(" | ")));
        }
        Self::review_expansion(node, &measure, &panel, source)
    }

    fn attempt_models(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        self.prior_models(attempts)
    }

    fn equal_review_range(&mut self, reference: &str) -> Result<bool, Self::Error> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        if host != self.factory.context.identity.host.to_string() {
            return Ok(false);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store(self.cancel)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        let request = store.request(record.run_id, &call).map_err(run_error)?;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(FacadeError::StateCorrupt)?;
        Ok(task
            .base
            .as_ref()
            .zip(task.head.as_ref())
            .is_some_and(|(base, head)| base == head))
    }
    fn respect_ci_wake(&self) -> bool {
        self.tick
    }
    fn ci_poll_backoff(&self, node_id: &str) -> provenant_fabric_runtime::graph::CiPollBackoff {
        let config = &self.factory.context.config.graph;
        config
            .ci_poll_overrides
            .get(node_id)
            .copied()
            .unwrap_or(config.ci_poll_backoff)
    }
    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        let branch =
            self.integration_branch
                .as_deref()
                .ok_or_else(|| FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: "an integration source requires graph.integration_branch".into(),
                })?;
        let reference = match branch.strip_prefix("origin/") {
            Some(branch) => format!("refs/remotes/origin/{branch}"),
            None => format!("refs/heads/{branch}"),
        };
        let git = provenant_os::git::git_executable()
            .ok_or(FacadeError::InvalidInput { detail: None })?;
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
        graph_ask_edge(self.factory, self.cancel, scope, false)
    }
    fn close_node_ask(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<(), Self::Error> {
        graph_ask_edge(self.factory, self.cancel, scope, true).map(|_| ())
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        self.factory.clock.wall_now().ok()
    }
    fn land_policy(
        &self,
        tier: provenant_records::workflow::RiskTier,
    ) -> provenant_fabric_runtime::config::LandChoice {
        let project = self.factory.context.identity.project.to_string();
        self.factory
            .context
            .config
            .graph
            .land
            .choose(Some(&project), tier)
    }
    fn land(
        &mut self,
        request: provenant_fabric_runtime::graph::LandRequest<'_>,
    ) -> Result<provenant_fabric_runtime::graph::LandObservation, Self::Error> {
        Ok(crate::pr::land::graph_land(
            &self.factory.cwd,
            self.factory.context.roots.state.as_path(),
            request.branch,
            request.head,
            request.pull_request,
            request.authority.is_some(),
            self.factory.workers,
        ))
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
            .ok_or(FacadeError::InvalidInput { detail: None })?;
        let session = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        }
        .session(self.cancel)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            cancel: self.cancel,
        };
        let mut body = format!("Graph {}\n\n", document.graph_id);
        let mut reviews = Vec::new();
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
            if graph_review_node(document, &node.id) {
                // Retain each launched review route, including failed attempts
                // before a successful retry. These are evidence, not a verdict.
                for review in &node.attempts {
                    let Some(reference) = review.attempt_ref.as_deref() else {
                        continue;
                    };
                    let (_, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                    let id = id.parse().map_err(|_| FacadeError::StateCorrupt)?;
                    let record = session.service().status(id, &call).map_err(run_error)?;
                    let mut facts = provenant_cli::run::recorded_attempt_facts(&record);
                    facts.role = document
                        .nodes
                        .iter()
                        .find(|spec| spec.id == node.id)
                        .and_then(|spec| spec.role.clone())
                        .or(facts.role);
                    // Each row describes its own result, including failed
                    // routes before a retry. Never borrow a later verdict.
                    facts.verdict = record.result.as_ref().and_then(|_| {
                        let captured = session.store.stream_result(id, &call, None).ok()?;
                        (!captured.verdict_invalid)
                            .then_some(captured.verdict)?
                            .map(String::from)
                    });
                    facts.head = node
                        .source_commit
                        .as_deref()
                        .and_then(|head| provenant_core::GitOid::parse(head).ok())
                        .or(facts.head);
                    reviews.push(facts);
                }
            }
        }
        body.push_str(&provenant_cli::pr_body::independent_review_rows(&reviews));
        let url = provenant_run::HostDeliveryContext::capture()
            .deliver_graph_pr(
                &self.factory.cwd,
                branch,
                self.integration_base
                    .and_then(|bases| bases.integration)
                    .ok_or(FacadeError::Conflict)?,
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

    fn attempt_families(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        self.writer_families(attempts)
    }

    fn materialise_inputs(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        requests: &[provenant_fabric_runtime::graph::InputRequest],
        cap: usize,
    ) -> Option<provenant_fabric_runtime::graph::MaterialisedInputs> {
        Some(graph_inputs::materialise(
            self.factory,
            self.budget,
            self.cancel,
            key,
            requests,
            cap,
        ))
    }

    fn stop_cancelled_attempt(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<(), Self::Error> {
        let mut owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        self.factory
            .cancel_graph_attempt(&mut owner, attempt, self.budget, self.cancel)
    }

    fn cancelled_attempt_settled(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        if let Some(reference) = attempt.attempt_ref.as_deref() {
            let (host, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let host = host.parse().map_err(|_| FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host),
                attempt: id.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            let record = owner.graph_policy_tail(target, self.budget, self.cancel)?;
            return Ok(matches!(
                record.state,
                provenant_records::run::AttemptState::Terminal { .. }
            ) && record.custody_pending == Some(false));
        }
        let operation = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store(self.cancel)?;
        let call = owner.call(self.cancel);
        match store.operation(&operation, &call).map_err(run_error)? {
            None => Ok(true),
            Some(operation) => {
                use provenant_run::{AttemptFilter, AttemptReader};
                if !store
                    .pending_attempts(operation.run_id, &call)
                    .map_err(run_error)?
                    .is_empty()
                {
                    return Ok(false);
                }
                let attempts = store
                    .attempts(
                        AttemptFilter {
                            run_id: Some(operation.run_id),
                            task_id: None,
                        },
                        &call,
                    )
                    .map_err(run_error)?;
                let mut found = false;
                for fact in attempts {
                    let fact = fact.map_err(run_error)?;
                    let record = store.record(fact.attempt_id, &call).map_err(run_error)?;
                    found = true;
                    if !store.custody_released(&record, &call).map_err(run_error)? {
                        return Ok(false);
                    }
                }
                // A prepared operation with no attempt never admitted work;
                // an unknown launch without evidence must remain unresolved.
                Ok(found || operation.state == provenant_run::OperationState::Prepared)
            }
        }
    }

    fn launch_accepted(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        let operation_id = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store(self.cancel)?;
        let call = owner.call(self.cancel);
        Ok(store
            .operation(&operation_id, &call)
            .map_err(run_error)?
            .is_some_and(|operation| operation.state != provenant_run::OperationState::Prepared))
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
        let operation_id = graph_operation("graph-attempt-operation", key)?;
        let cancel = self.cancel;
        let blocked = |owner: &BinaryRunOwner<'_>| -> Result<Option<String>, FacadeError> {
            let store = owner.store(cancel)?;
            Ok(store
                .operation(&operation_id, &owner.call(cancel))
                .map_err(run_error)?
                .filter(|operation| operation.state == provenant_run::OperationState::Blocked)
                .map(|operation| {
                    operation
                        .blocked_reason
                        .unwrap_or_else(|| "custody release failed".into())
                }))
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
            if let Some(reason) = blocked(&owner)? {
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason,
                });
            }
            let operation_id = graph_operation("graph-attempt-operation", key)?;
            let store = owner.store(self.cancel)?;
            let call = owner.call(self.cancel);
            let operation = store.operation(&operation_id, &call).map_err(run_error)?;
            let accepted = operation.as_ref().is_some_and(|operation| {
                operation.state != provenant_run::OperationState::Prepared
            });
            // A crash can leave an accepted operation without a checkpoint
            // reference. Preserve its immutable labels, including legacy None.
            let mut prepared_without_request = false;
            let mut accepted_knobs = None;
            let mut accepted_route = None;
            let mut accepted_retry_limit = None;
            let context = match &operation {
                Some(operation) => match store.request(operation.run_id, &call) {
                    Ok(request) => {
                        let task = request.tasks.first().ok_or(FacadeError::StateCorrupt)?;
                        if accepted {
                            accepted_knobs = Some(task.profile.overrides.clone());
                            accepted_route = Some(task.route.clone());
                            accepted_retry_limit = Some(task.profile.settlement_retry_limit);
                        }
                        task.context.clone()
                    }
                    Err(provenant_run::RunError::NotFound)
                        if operation.state == provenant_run::OperationState::Prepared =>
                    {
                        prepared_without_request = true;
                        Some(node.context)
                    }
                    Err(error) => return Err(run_error(error)),
                },
                None => Some(node.context),
            };
            let excluded_families = self.writer_families(node.distinct_from_attempts)?;
            // An exact route cannot be substituted, so required separation is
            // decided here, before any worktree advance or submission. An
            // accepted operation is observed as it was launched.
            if !accepted
                && let Some(model) = &route.model
                && exact_route_refused(
                    &exact_model(route.adapter.as_deref(), model),
                    &excluded_families,
                    node.require_distinct,
                )
            {
                return Ok(AttemptObservation::Terminal {
                    attempt_ref: None,
                    outcome: AttemptOutcome::AdapterUnavailable,
                    launched: false,
                    result: None,
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: None,
                });
            }
            // Preserve the specific detached-writer refusal in node status.
            if !accepted
                && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                && let Some(path) = worktree
            {
                let writer = provenant_run::resolve_writer(
                    self.factory.context.roots.state.as_path(),
                    &self.factory.cwd,
                    Path::new(path),
                )
                .map_err(|error| match error {
                    provenant_run::RunError::Validation("writer needs a branch") => {
                        FacadeError::DispatchRefused {
                            fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                            disk: None,
                            reason: "writer needs a branch".into(),
                        }
                    }
                    other => run_error(other),
                })?;
                if let Err(provenant_run::RunError::Validation("writer needs a branch")) =
                    provenant_run::prepare_writer(
                        &writer,
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.identity.project,
                    )
                {
                    return Err(FacadeError::DispatchRefused {
                        fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                        disk: None,
                        reason: "writer needs a branch".into(),
                    });
                }
            }
            if !accepted && let (Some(path), Some(commit)) = (worktree, node.source_commit) {
                let head = commit
                    .parse()
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
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
                .map_err(|error| graph_writer_initialisation_error(commit, error))?;
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
                // A continuing writer pushes only to the branch it continues.
                if let Some(bound) = replay.branch
                    && bound != branch
                {
                    return Err(FacadeError::DispatchRefused {
                        fact: None,
                        disk: None,
                        reason: format!(
                            "graph writer worktree is on {branch}, not the continued branch {bound}"
                        ),
                    });
                }
                provenant_run::HostDeliveryContext::destination(mode.clone(), &branch)
                    .map_err(run_error)?
                    .map(|mut request| {
                        request.fast_forward_only = replay.fast_forward_only;
                        request
                    })
            } else {
                None
            };
            let review_base = if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly {
                provenant_fabric_runtime::graph::FileJournal::execution(
                    &self.factory.context.roots.state,
                    self.factory.context.identity.project,
                    &key.graph_id,
                )
                .map_err(graph_journal_error)?
                .and_then(|execution| execution.review_base)
            } else {
                None
            };
            // Writer separation is mandatory; sibling diversity remains a preference.
            let enforced = node.require_distinct && !excluded_families.is_empty();
            let mut avoided_families = excluded_families;
            let mut preferred_families = Vec::new();
            for family in node.family_peers {
                let target = if enforced {
                    &mut preferred_families
                } else {
                    &mut avoided_families
                };
                if !target.contains(family) {
                    target.push(family.clone());
                }
            }
            let exclude = self.route_exclusions(route, node.prior_attempts, accepted);
            let base = if accepted {
                store
                    .request(
                        operation.as_ref().ok_or(FacadeError::StateCorrupt)?.run_id,
                        &call,
                    )
                    .map_err(run_error)?
                    .tasks
                    .first()
                    .ok_or(FacadeError::StateCorrupt)?
                    .base_ref
                    .clone()
            } else if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                && node.source_commit.is_some()
            {
                node.review_base
                    .map(str::to_owned)
                    // A review base recorded with the graph execution comes next,
                    // then the integration base.
                    .or_else(|| review_base.map(|base| base.to_string()))
                    .or_else(|| {
                        self.integration_base
                            .and_then(|bases| bases.integration)
                            .map(|base| base.to_string())
                    })
                    // Preserve the prior fallback without requiring origin/HEAD.
                    .or_else(|| node.source_commit.map(str::to_owned))
            } else {
                node.source_commit.map(|head| {
                    self.integration_base
                        .and_then(|bases| bases.integration)
                        .map_or_else(|| head.to_owned(), |base| base.to_string())
                })
            };
            let input = SubmitInput::Submit {
                extra: Default::default(),
                operation_id,
                tasks: vec![SubmittedTask {
                    context,
                    extra: Default::default(),
                    build_heavy: false,
                    base,
                    head: node.source_commit.map(str::to_owned),
                    account: route.account.clone(),
                    access,
                    worktree: worktree.map(str::to_owned),
                    prompt: node.prompt.into(),
                    title: Some(node.title.to_owned()),
                    route: accepted_route.unwrap_or(match &route.model {
                        None if route.name.is_some() => {
                            provenant_records::run::RequestedRoute::Adapter {
                                extra: Default::default(),
                                provider: route
                                    .adapter
                                    .clone()
                                    .ok_or(FacadeError::InvalidInput { detail: None })?,
                                name: route
                                    .name
                                    .clone()
                                    .ok_or(FacadeError::InvalidInput { detail: None })?,
                            }
                        }
                        Some(model) => {
                            let adapter = route
                                .adapter
                                .as_ref()
                                .ok_or(FacadeError::InvalidInput { detail: None })?;
                            provenant_records::run::RequestedRoute::Model {
                                extra: Default::default(),
                                provider: adapter.clone(),
                                model: exact_model(Some(adapter), model),
                                effort: route.effort.clone(),
                            }
                        }
                        None if !avoided_families.is_empty() => {
                            provenant_records::run::RequestedRoute::Review {
                                extra: Default::default(),
                                name: route
                                    .alias
                                    .clone()
                                    .ok_or(FacadeError::InvalidInput { detail: None })?,
                                effort: route.effort.clone(),
                                excluded_families: avoided_families,
                                distinct_families: true,
                                preferred_families,
                                require_distinct: enforced,
                            }
                        }
                        None => provenant_records::run::RequestedRoute::Named {
                            extra: Default::default(),
                            name: route
                                .alias
                                .clone()
                                .ok_or(FacadeError::InvalidInput { detail: None })?,
                            effort: route.effort.clone(),
                        },
                    }),
                    timeout_seconds: lane_timeout(
                        node.deadline_seconds,
                        self.factory.context.config.limits.graph_timeout_seconds,
                    ),
                    delivery_id: None,
                    deliver: delivery,
                    new_worktree: None,
                }],
                concurrency: Some(1),
                knobs: if let Some(knobs) = accepted_knobs {
                    knobs
                } else {
                    let mut knobs = Vec::new();
                    if !exclude.is_empty() {
                        knobs.push(format!(
                            "routing.graph_exclude={}",
                            serde_json::Value::from(exclude)
                        ));
                    }
                    knobs
                },
                host: None,
            };
            // Operation replay is actor-scoped: submit under the recorded launch actor.
            owner.actor = Some(
                provenant_core::ActorId::parse(replay.launch_actor)
                    .map_err(|_| FacadeError::StateCorrupt)?,
            );
            let mut grants = provenant_cli::run::lane_access(
                &provenant_cli::Access {
                    mode: Some(provenant_cli::Mode::ReadOnly),
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
                    network: self.network,
                    capability: Vec::new(),
                },
                self.factory.context.roots.state.as_path(),
                &self.factory.home,
            )
            .map_err(dispatch_refused)?;
            grants.inputs_root = node.inputs_dir.map(Path::to_path_buf);
            let legacy_input = prepared_without_request.then(|| {
                let mut legacy = input.clone();
                let SubmitInput::Submit { tasks, .. } = &mut legacy;
                for task in tasks {
                    task.context = None;
                }
                legacy
            });
            let retry_limit = accepted_retry_limit.unwrap_or(node.settlement_retries);
            let receipt = match owner.submit_with_access(
                input,
                self.budget,
                self.cancel,
                grants.clone(),
                retry_limit,
            ) {
                Err(FacadeError::OperationConflict) if legacy_input.is_some() => {
                    // Store admission still requires the reserved digest to match
                    // exactly. Only legacy metadata is retried, once.
                    owner.submit_with_access(
                        legacy_input.ok_or(FacadeError::StateCorrupt)?,
                        self.budget,
                        self.cancel,
                        grants,
                        retry_limit,
                    )?
                }
                result => result?,
            };
            if let Some(reason) = blocked(&owner)? {
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason,
                });
            }
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
            let store = owner.store(self.cancel)?;
            attempt = store
                .policy_tail(attempt.attempt_id, &owner.call(self.cancel))
                .map_err(run_error)?;
            reference = Some(format!(
                "{}/{}",
                self.factory.context.identity.host, attempt.attempt_id
            ));
        }
        use provenant_records::run::{AttemptState, Outcome};
        match attempt.state {
            AttemptState::Terminal { outcome, .. } => {
                let store = owner.store(self.cancel)?;
                let call = owner.call(self.cancel);
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
                        Ok(result) => (Some(Box::new(result)), None),
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
                    } else if let Some(failure) = attempt.preparation_failure {
                        match failure {
                            provenant_records::run::PreparationFailure::AuthRequired => {
                                AttemptOutcome::AuthRequired
                            }
                            provenant_records::run::PreparationFailure::UsageLimited => {
                                AttemptOutcome::UsageLimited
                            }
                            provenant_records::run::PreparationFailure::AdapterUnavailable => {
                                AttemptOutcome::AdapterUnavailable
                            }
                        }
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
                            Outcome::TimedOut if node.deadline_seconds.is_some() => {
                                AttemptOutcome::DeadlineExceeded
                            }
                            Outcome::TimedOut => AttemptOutcome::OtherFailure,
                            // A route refused before launch leaves the next route free to try.
                            Outcome::ModelUnavailable => AttemptOutcome::AdapterUnavailable,
                            Outcome::Rejected if !launched => AttemptOutcome::AdapterUnavailable,
                            Outcome::ReviewRequired
                                if attempt.partial_signals.as_slice() == [provenant_records::run::PartialSignal::DeclaredStop] =>
                            {
                                AttemptOutcome::DeclaredStop
                            }
                            Outcome::Failed => AttemptOutcome::TaskFailed,
                            Outcome::ReviewRequired => AttemptOutcome::ReviewRequired,
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
/// Resolve the existing routing allowance, retaining explicit zero overrides.
fn graph_retry_allowance(
    node: &provenant_records::ExtraFields,
    graph: &provenant_records::ExtraFields,
    roots: &provenant_core::Roots,
) -> (u8, String) {
    let mut warnings = Vec::new();
    let mut read = |extra: &provenant_records::ExtraFields, key: &str| -> Option<u8> {
        let retry = extra.get(key)?;
        let value = retry
            .get("retries")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= provenant_routing::MAX_SETTLEMENT_RETRIES);
        if value.is_none() {
            warnings.push(format!("ignored invalid {key}.retries"));
        }
        value
    };
    let node_retry = read(node, "retry");
    let preset_retry = read(node, "preset_retry").or_else(|| read(graph, "retry"));
    let host = match provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS) {
        Ok(config) => matches!(
            config.origins.get("domains.routing.settlement_retries"),
            Some(provenant_config::Origin::User { .. })
        )
        .then_some(config.domains.routing.settlement_retries),
        Err(_) => {
            warnings.push("host retry setting unavailable".into());
            None
        }
    };
    let allowance = provenant_routing::resolve_retry_allowance(node_retry, preset_retry, host);
    let origin = match allowance.origin {
        provenant_routing::RetryAllowanceOrigin::Node => "node",
        provenant_routing::RetryAllowanceOrigin::Preset => "preset",
        provenant_routing::RetryAllowanceOrigin::HostConfig => "host_config",
        provenant_routing::RetryAllowanceOrigin::ShippedDefault => "shipped_default",
    };
    let origin = if warnings.is_empty() {
        origin.to_owned()
    } else {
        format!("{origin}; {}", warnings.join("; "))
    };
    (allowance.retries, origin)
}

/// A graph document's optional top-level `exclude` list of route-exclusion
/// selectors, inherited by every node it launches.
fn graph_exclude(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<Vec<String>, FacadeError> {
    match document.extra.get("exclude") {
        None => Ok(Vec::new()),
        Some(value) => value
            .as_array()
            .ok_or(FacadeError::InvalidInput { detail: None })?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or(FacadeError::InvalidInput { detail: None })
            })
            .collect(),
    }
}
/// A graph document's optional top-level `network` (`"on"` or `"off"`),
/// applied to every node lane it launches.
fn graph_network(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<Option<provenant_cli::Switch>, FacadeError> {
    match document.extra.get("network") {
        None => Ok(None),
        Some(value) => match value.as_str() {
            Some("on") => Ok(Some(provenant_cli::Switch::On)),
            Some("off") => Ok(Some(provenant_cli::Switch::Off)),
            _ => Err(FacadeError::InvalidInput { detail: None }),
        },
    }
}

/// A graph route's exact model in `ADAPTER/ID` form.
fn exact_model(adapter: Option<&str>, model: &str) -> String {
    match adapter {
        Some(adapter) if !model.contains('/') => format!("{adapter}/{model}"),
        _ => model.to_owned(),
    }
}

/// Whether required family separation refuses an exact route before launch.
/// As for routed reviews, a model whose family is unknown is treated as
/// excluded once any family is.
fn exact_route_refused(model: &str, excluded_families: &[String], require_distinct: bool) -> bool {
    require_distinct
        && !excluded_families.is_empty()
        && provenant_cli::run::model_family(model)
            .is_none_or(|family| excluded_families.contains(&family))
}

impl Factory {
    /// Read a project-local graph document through the confined, bounded helpers.
    fn read_graph_document(
        &self,
        path: &Path,
    ) -> Result<provenant_fabric_runtime::graph::GraphDocument, FacadeError> {
        let path = provenant_os::fs::canonicalize(path)
            .map_err(|_| FacadeError::io("read project-local graph document; a bare name is read only from .provenant/scratch/graphs, so pass the explicit path of a legacy .agent-run/graphs document"))?;
        if !path.starts_with(&self.cwd) {
            return Err(FacadeError::ConfinementRefused);
        }
        let bytes = provenant_os::fs::FileHandle::open_source(&path)
            .and_then(|mut file| file.read_bounded(8_388_608))
            .map_err(|_| FacadeError::io("access graph journal"))?;
        decode_graph_document(&bytes)
    }

    /// The one selector resolver for graph status, cancel and resume: a graph
    /// ID, a project-local document path or a document SHA-256 hash (prefix)
    /// becomes the graph ID and, when one is retained or named, its document.
    fn resolve_graph(
        &self,
        selector: &str,
    ) -> Result<
        (
            String,
            Option<provenant_fabric_runtime::graph::GraphDocument>,
        ),
        FacadeError,
    > {
        use provenant_fabric_runtime::graph::FileJournal;
        let refused = || FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: graph_selector_forms(),
        };
        // An exact graph ID wins over every other form, but a hex ID that is
        // also a hash prefix of a document declaring another graph is ambiguous.
        let hash = selector.strip_prefix("sha256:").unwrap_or(selector);
        let hex = !hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        match FileJournal::execution(
            &self.context.roots.state,
            self.context.identity.project,
            selector,
        ) {
            Ok(Some(execution)) => {
                if hex
                    && let Ok(found) = self.documents_by_hash(hash)
                    && found.iter().any(|document| document.graph_id != selector)
                {
                    return Err(refused());
                }
                return Ok((selector.to_owned(), Some(execution.document)));
            }
            Err(error) if error.is_unreadable() => return Ok((selector.to_owned(), None)),
            _ => {}
        }
        // Then a document path: any path-shaped selector, or a bare name of an
        // existing file in the working directory or the scratch graphs directory.
        let path = Path::new(selector);
        let scratch = self.cwd.join(".provenant/scratch/graphs").join(path);
        let local = self.cwd.join(path);
        if path.is_absolute()
            || selector.contains(['/', '\\'])
            || selector.ends_with(".json")
            || local.is_file()
            || scratch.is_file()
        {
            let path = if path.components().count() == 1 && !local.exists() {
                scratch
            } else {
                local
            };
            return match self.read_graph_document(&path) {
                Ok(document) => Ok((document.graph_id.clone(), Some(document))),
                Err(FacadeError::Io { .. }) => Err(refused()),
                Err(error) => Err(error),
            };
        }
        // An unknown ID, or a hash with no match, is not found; an ambiguous
        // prefix or an unsafe graphs directory is refused.
        if !hex {
            return Err(FacadeError::NotFound);
        }
        let Ok(found) = self.documents_by_hash(hash) else {
            return Err(refused());
        };
        match <[_; 1]>::try_from(found) {
            Ok([document]) => Ok((document.graph_id.clone(), Some(document))),
            Err(found) if found.is_empty() => Err(FacadeError::NotFound),
            Err(_) => Err(refused()),
        }
    }

    /// The scratch graph documents whose SHA-256 starts with `hash`. Symlinked
    /// and non-regular entries fail the no-follow bounded read; an unsafe
    /// graphs directory is an error.
    fn documents_by_hash(
        &self,
        hash: &str,
    ) -> Result<Vec<provenant_fabric_runtime::graph::GraphDocument>, provenant_os::fs::FsError>
    {
        let graphs = [".provenant", "scratch", "graphs"].iter().try_fold(
            provenant_os::fs::Directory::open_anchor(&self.cwd)?,
            |directory, name| directory.open_verified_directory(std::ffi::OsStr::new(name)),
        )?;
        let prefix = hash.to_ascii_lowercase();
        let mut found = Vec::new();
        for name in graphs.entries()?.flatten() {
            if let Ok(bytes) = graphs.read_bounded(&name, 8_388_608)
                && provenant_records::CanonicalV1::file_digest(&bytes)
                    .to_string()
                    .strip_prefix("sha256:")
                    .is_some_and(|digest| digest.starts_with(&prefix))
                && let Ok(document) = decode_graph_document(&bytes)
            {
                found.push(document);
            }
        }
        Ok(found)
    }

    fn graph(
        &self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        self.graph_with_scratch(input, budget, cancel, None)
    }

    fn graph_with_scratch(
        &self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
        prepared_scratch: Option<&Path>,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        use provenant_fabric_runtime::coordination::input::ValidateInput;
        use provenant_fabric_runtime::graph::{FileJournal, GraphJournal};
        let input = match input {
            GraphInput::Status { extra, id } => GraphInput::Status {
                extra,
                id: self.resolve_graph(&id)?.0,
            },
            GraphInput::Cancel { extra, id, reason } => GraphInput::Cancel {
                extra,
                id: self.resolve_graph(&id)?.0,
                reason,
            },
            input => input,
        };
        input
            .validate_input(provenant_core::WaitTransport::Cli)
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
        let resume = matches!(&input, GraphInput::Resume { .. });
        let document = match &input {
            GraphInput::Run { document, .. } | GraphInput::Resume { document, .. } => {
                let scratch = match prepared_scratch {
                    Some(scratch) => scratch.to_path_buf(),
                    None => self
                        .prepare_scratch(
                            cancel,
                            WaitBudget::cli(self.clock.mono_now(), 30)
                                .map_err(|_| FacadeError::InvalidInput { detail: None })?,
                        )
                        .map_err(scratch_prepare_error)?,
                };
                let document = if resume {
                    self.resolve_graph(&document.to_string_lossy())?
                        .1
                        .ok_or_else(|| FacadeError::DispatchRefused {
                            fact: None,
                            disk: None,
                            reason: graph_selector_forms(),
                        })?
                } else {
                    let path = if document.is_absolute() {
                        document.clone()
                    } else if document.components().count() == 1
                        && !self.cwd.join(document).exists()
                    {
                        scratch.join("graphs").join(document)
                    } else {
                        self.cwd.join(document)
                    };
                    self.read_graph_document(&path)?
                };
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
                    .map_err(|reason| FacadeError::DispatchRefused {
                        reason,
                        disk: None,
                        fact: None,
                    })?;
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
                        let path = node
                            .worktree
                            .as_deref()
                            .ok_or(FacadeError::InvalidInput { detail: None })?;
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
                    // A local target checked out in the writer's own worktree
                    // cannot be advanced by compare-and-swap, so refuse the
                    // graph before any attempt rather than at delivery.
                    if !admitted
                        && let Some(provenant_records::run::DeliveryMode::Integrate(target)) =
                            &node.delivery
                        && !target.starts_with("origin/")
                        && let Some(path) = node.worktree.as_deref()
                    {
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
                        if writer.branch.as_str().trim_start_matches("refs/heads/") == target {
                            return Err(FacadeError::DispatchRefused {
                                fact: None,
                                disk: None,
                                reason: format!(
                                    "worktree {path} is on the integration branch {target}; create it with `git worktree add -b batch/work PATH BASE`"
                                ),
                            });
                        }
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
                .ok_or(FacadeError::InvalidInput { detail: None })?
                .graph_id
                .as_str(),
        };
        // An unreadable record has no trustworthy node or attempt state.
        let unreadable_snapshot = |message: String| {
            use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphRun, GraphStatus};
            GraphRun {
                status: GraphStatus::Paused,
                checkpoint: GraphCheckpoint {
                    extra: Default::default(),
                    expansions: Vec::new(),
                    remaining: None,
                    instance_id: None,
                    delivery_required: false,
                    delivered: false,
                    pull_request: None,
                    integration_pin: None,
                    cancelled: false,
                    cancel_reason: None,
                    pause_reason: None,
                    pause_code: None,
                    unknown_usage: Vec::new(),
                    unknown_usage_omitted: 0,
                    usage_events: Vec::new(),
                    usage_events_omitted: 0,
                    settlements: 0,
                    extensions: Vec::new(),
                    asks: Vec::new(),
                    ask_actor: None,
                    wake_at: None,
                    pause_asks: 0,
                    limit_notes: Vec::new(),
                    schema_version: 1,
                    graph_id: id.to_owned(),
                    plan_digest: String::new(),
                    nodes: Vec::new(),
                },
                advance_error: Some(message),
            }
        };
        if matches!(input, GraphInput::Status { .. }) {
            let mut checkpoint = match FileJournal::read_checkpoint(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            ) {
                Ok(checkpoint) => checkpoint.ok_or(FacadeError::NotFound)?,
                Err(error) if error.is_unreadable() => {
                    return Ok(unreadable_snapshot(format!(
                        "Graph record is unreadable ({error}); cancel it to retire the record. Active attempts and writer fences are not released."
                    )));
                }
                Err(error) => return Err(graph_journal_error(error)),
            };
            if let (Ok(boot), Ok(now)) = (
                self.clock.boot_identity(),
                self.clock.host_monotonic_micros(),
            ) {
                graph_status_elapsed(&mut checkpoint, &boot, now);
            }
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
        if matches!(input, GraphInput::Cancel { .. })
            && FileJournal::retire_unreadable(
                &self.context.roots.state,
                self.context.identity.project,
                id,
                budget,
                cancel,
                &self.clock,
            )
            .map_err(graph_journal_error)?
        {
            return Ok(unreadable_snapshot(
                "Unreadable graph record retired. Active attempts were not cancelled and writer fences were not released.".into(),
            ));
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
            let existing = journal.load(id).map_err(graph_journal_error)?.is_some();
            let bound_execution = if existing {
                Some(
                    FileJournal::execution(
                        &self.context.roots.state,
                        self.context.identity.project,
                        id,
                    )
                    .map_err(graph_journal_error)?
                    .ok_or(FacadeError::StateCorrupt)?,
                )
            } else {
                None
            };
            let integration_base = if existing {
                journal.integration_base()
            } else if let Some(branch) = &document.integration_branch {
                let reference = match branch.strip_prefix("origin/") {
                    Some(branch) => format!("refs/remotes/origin/{branch}"),
                    None => format!("refs/heads/{branch}"),
                };
                let git = provenant_os::git::git_executable()
                    .ok_or(FacadeError::InvalidInput { detail: None })?;
                Some(
                    provenant_run::resolve_git_base(&git, &self.cwd, &reference)
                        .map_err(run_error)?
                        .parse()
                        .map_err(|_| FacadeError::StateCorrupt)?,
                )
            } else {
                None
            };
            let review_base = if existing {
                bound_execution
                    .as_ref()
                    .and_then(|execution| execution.review_base)
            } else if let Some(reference) = document
                .review_base
                .as_deref()
                .filter(|base| !base.is_empty())
            {
                let git = provenant_os::git::git_executable()
                    .ok_or(FacadeError::InvalidInput { detail: None })?;
                Some(
                    provenant_run::resolve_git_base(&git, &self.cwd, reference)
                        .map_err(run_error)?
                        .parse()
                        .map_err(|_| FacadeError::StateCorrupt)?,
                )
            } else {
                None
            };
            journal
                .bind(provenant_fabric_runtime::graph::GraphExecution {
                    review_base,
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
            // Manual replay must use the same host directory as the original
            // execution, just as background advancement does.
            let rebound = if let Some(execution) = bound_execution.as_ref()
                && execution.directory != self.cwd
            {
                Some(self.for_graph_directory(&execution.directory)?)
            } else {
                None
            };
            let mut owner = BoundGraphOwner {
                factory: rebound.as_ref().unwrap_or(self),
                integration_base: Some(BoundGraphBases {
                    integration: integration_base,
                    review: review_base,
                }),
                integration_branch: document.integration_branch.clone(),
                network: graph_network(document)?,
                exclude: graph_exclude(document)?,
                budget,
                cancel,
                prices: None,
                tick: false,
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
                provenant_fabric_runtime::graph::GraphError::Invalid(reason) => {
                    FacadeError::invalid(format!("graph {reason}"))
                }
                provenant_fabric_runtime::graph::GraphError::Extension(reason)
                | provenant_fabric_runtime::graph::GraphError::Refused(reason) => {
                    FacadeError::DispatchRefused {
                        reason,
                        disk: None,
                        fact: None,
                    }
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
        if let GraphInput::Cancel { reason, .. } = input {
            // Persist the admission fence before asking the run owner to stop live attempts.
            if !checkpoint.cancelled || reason.is_some() {
                checkpoint.cancel_reason = reason;
            }
            checkpoint.cancelled = true;
            provenant_fabric_runtime::graph::mark_cancelled_nodes_without_live_attempts(
                &mut checkpoint,
            );
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
                network: None,
                exclude: Vec::new(),
                budget,
                cancel,
                prices: None,
                tick: false,
            };
            if provenant_fabric_runtime::graph::settle_cancelled_nodes(
                &mut checkpoint,
                &mut graph_owner,
                &mut journal,
            )
            .is_err()
            {
                first_error.get_or_insert_with(|| FacadeError::io("settle cancelled graph nodes"));
            }
            if provenant_fabric_runtime::graph::close_resolved_asks(
                &mut checkpoint,
                &mut graph_owner,
                &mut journal,
            )
            .is_err()
            {
                first_error.get_or_insert_with(|| FacadeError::io("close cancelled graph asks"));
            }
            if let Err(error) = self.close_legacy_loop_asks(&checkpoint, cancel) {
                first_error.get_or_insert(error);
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        Ok(graph_snapshot(checkpoint))
    }

    fn prepare_scratch(
        &self,
        cancel: &CancelToken,
        budget: WaitBudget,
    ) -> Result<PathBuf, preset_graph::ScratchPreparationError> {
        provenant_confinement::worktrees::with_context(
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
                provenant_confinement::scratch::ensure_with_budget(
                    &repo,
                    &self.clock,
                    cancel,
                    budget,
                )
            },
        )
        .map_err(preset_graph::ScratchPreparationError)
    }
}

fn graph_selector_forms() -> String {
    "unknown or ambiguous graph selector; accepted forms are a graph ID, a project-local document path, or the document SHA-256 hash".to_owned()
}

impl Factory {
    /// Close the inbox requests of a checkpoint from before node asks, one
    /// inbox actor at a time.
    fn close_legacy_loop_asks(
        &self,
        checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
        cancel: &CancelToken,
    ) -> Result<(), FacadeError> {
        let actors: std::collections::BTreeSet<_> =
            provenant_fabric_runtime::graph::legacy_loop_asks(checkpoint)
                .into_iter()
                .map(|ask| ask.actor)
                .collect();
        for actor in actors {
            let mut identity = self.context.identity.clone();
            identity.inbox.actor = actor.parse().map_err(|_| FacadeError::StateCorrupt)?;
            let service = provenant_fabric_runtime::inbox::InboxService::open(
                &self.context.roots.state,
                &identity,
                &self.clock,
                cancel,
            )
            .map_err(|error| FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: format!("close legacy loop asks: {error}"),
            })?;
            graph_legacy_loop_close(&service, checkpoint, identity.inbox.actor, cancel)?;
        }
        Ok(())
    }

    /// Cancel one live graph attempt. A missing checkpoint reference (a crash
    /// between acceptance and the save) is recovered from the stable operation ID.
    fn cancel_graph_attempt(
        &self,
        owner: &mut BinaryRunOwner<'_>,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<(), FacadeError> {
        owner.actor = Some(
            attempt
                .launch_actor
                .parse()
                .map_err(|_| FacadeError::StateCorrupt)?,
        );
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
                let store = owner.store(cancel)?;
                let call = owner.call(cancel);
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
        let mut first_error = None;
        for mut target in targets {
            match owner.graph_policy_tail(target.clone(), budget, cancel) {
                Ok(tail) => target.attempt = tail.attempt_id,
                Err(error) => {
                    // Still ask the known attempt to stop if tail lookup fails.
                    first_error.get_or_insert(error);
                }
            }
            let operation_id =
                graph_operation(&format!("graph-cancel-{}", target.attempt), &attempt.key)?;
            if let Err(error) = owner.run(
                RunInput::Cancel {
                    extra: Default::default(),
                    target,
                    operation_id,
                },
                budget,
                cancel,
            ) {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
fn graph_operation(
    domain: &str,
    key: &provenant_fabric_runtime::graph::AttemptKey,
) -> Result<provenant_core::OperationId, FacadeError> {
    let digest = provenant_records::CanonicalV1::record_digest(domain, 1, key)
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
    provenant_core::OperationId::parse(&format!("graph-{}", provenant_core::hex(digest.as_bytes())))
        .map_err(|_| FacadeError::InvalidInput { detail: None })
}
fn graph_display(
    run: &provenant_fabric_runtime::graph::GraphRun,
    command: &str,
    globals: &provenant_cli::Globals,
) -> String {
    if command == "fabric graph status" && globals.full && !globals.json {
        graph_human::graph_human_full(run)
    } else {
        graph_human(run)
    }
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
        Graph::Cancel { id, reason } => (
            "fabric graph cancel",
            GraphInput::Cancel {
                extra: Default::default(),
                id,
                reason,
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
            // Only the launching commands answer for a blocked launch; status
            // and cancel report it as ordinary graph state.
            let launching = matches!(command, "fabric graph run" | "fabric graph resume");
            let refused = data.checkpoint.nodes.iter().find_map(|node| {
                (node.advance_error_code.as_deref() == Some("launch_refused"))
                    .then_some(node.advance_error.as_deref())
                    .flatten()
            });
            if let Some(reason) = refused.filter(|_| launching) {
                return crate::fail(
                    command,
                    provenant_records::Outcome::Refused,
                    "dispatch_refused",
                    reason,
                    globals,
                );
            }
            let human = graph_display(&data, command, globals);
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
            let mut message = error.to_string();
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
                message = graph_selector_forms();
                (provenant_records::Outcome::Invalid, "not_found")
            } else if let Some(error @ FacadeError::DispatchRefused { .. }) =
                error.downcast_ref::<FacadeError>()
            {
                (provenant_records::Outcome::Refused, error.code())
            } else {
                (provenant_records::Outcome::Error, "graph_owner")
            };
            crate::fail(command, outcome, code, &message, globals)
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
            cancel,
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
                owner.events(after, 64, cancel)
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
            return Err(FacadeError::InvalidInput { detail: None }.into());
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
                let input = EventsInput::Read {
                    extra: Default::default(),
                    after: page.after,
                    limit: page.limit,
                    wait_seconds: None,
                };
                let observation: Result<EventsResult, Box<dyn std::error::Error>> =
                    if factory.context.in_lane && factory.context.remote_home() {
                        provenant_fabric_mcp::tools::ToolCall::Events(input, None)
                            .relay_within(
                                &factory.context.lane_relay_socket()?,
                                &cancel,
                                std::time::Duration::from_micros(
                                    budget.remaining(factory.clock.mono_now()).as_micros(),
                                ),
                            )
                            .map_err(Into::into)
                            .and_then(|value| serde_json::from_value(value).map_err(Into::into))
                    } else {
                        service.events(input, budget, &cancel).map_err(Into::into)
                    };
                match observation {
                    Ok(value) => value,
                    Err(_)
                        if wait.is_some()
                            && budget.remaining(factory.clock.mono_now()).as_micros() == 0 =>
                    {
                        EventsResult::empty_timeout(page.after)
                    }
                    Err(error) => return Err(error),
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
            provenant_core::Pause::wait(
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
