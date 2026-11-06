//! Fabric process composition: MCP, fixed peer ingress and host observations.
mod graph_human;
mod graph_inputs;
mod graph_status;
mod interface;
mod unified;
pub(crate) use unified::{
    GraphCommand, append_graph_status, read_status_rows, target_text, unified_graph_command,
    unified_graph_output, unified_graph_steer,
};
mod graph_document;
pub(crate) mod landing;
pub(crate) mod owner;
mod peer;
pub(crate) mod run_owner;
pub(crate) mod settlement;

pub use landing::*;
pub(crate) use owner::*;
pub(crate) use run_owner::*;
pub(crate) use settlement::*;

pub(crate) use graph_document::publish_graph_document;
#[cfg(test)]
#[allow(unused_imports)]
use graph_human::graph_ci_waiting;
#[cfg(test)]
pub(crate) use graph_human::graph_human;
use graph_human::graph_summary;
use graph_status::{graph_snapshot, graph_status_elapsed, redact_checkout_paths};
pub use peer::{hosts, peer};
pub(crate) use settlement::advance_live_graph;

#[cfg(test)]
include!("fabric/facade_tests.rs");
#[cfg(test)]
include!("fabric/graph_tests.rs");

use provenant_cli::fabric::Context;

use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_mcp::{
    stdio,
    tools::{Service, ServiceError},
};
use provenant_fabric_runtime::runs::{AttemptPage, RunCapabilities, RunEventsPage, RunResult};
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
        resource: None,
        input_failure: None,
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
        WorktreeError::UnknownWriter => FacadeError::WriterUnknown,
        error => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!(
                "initialise graph writer at {commit}: {}",
                crate::worktrees::local_worktree_message(&error)
            ),
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

pub(crate) struct Factory {
    pub(crate) context: Context,
    pub(crate) project_attribution: bool,
    pub(crate) clock: SystemClock,
    pub(crate) backoff: ThreadPause,
    pub(crate) executor: SystemExecutor,
    pub(crate) home: std::path::PathBuf,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) assets: PathBuf,
    pub(crate) host_launch: Option<provenant_run::HostLaunchContext>,
    pub(crate) mcp_project: Option<PathBuf>,
    pub(crate) workers: &'static dyn WorkerObserver,
}
impl Factory {
    /// Reopen a retained graph directory without borrowing the caller's checkout.
    pub(crate) fn for_graph_directory(&self, directory: &Path) -> Result<Self, FacadeError> {
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
    pub(crate) fn coordination(&self) -> Result<CoordinationService<'_>, CoordinationError> {
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
pub struct BoundFacade<'a> {
    factory: &'a Factory,
}
impl provenant_fabric_mcp::tools::McpFacade for BoundFacade<'_> {
    fn relay_document(
        &mut self,
        input: &serde_json::Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ServiceError> {
        self.factory
            .relay_document(input, budget, cancel)
            .map_err(ServiceError::from)
    }
    fn compile_lane_run(
        &self,
        input: &serde_json::Value,
    ) -> Result<provenant_fabric_runtime::graph::relay::RelayOp, ServiceError> {
        self.factory
            .child_request(input)
            .map(provenant_fabric_runtime::graph::relay::RelayOp::Add)
            .map_err(ServiceError::from)
    }
    fn graph_document(
        &mut self,
        input: serde_json::Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ServiceError> {
        self.factory
            .graph_document(input, budget, cancel)
            .map_err(ServiceError::from)
    }
    fn add_from_lane(
        &mut self,
        op: &provenant_fabric_runtime::graph::relay::RelayOp,
        cancel: &CancelToken,
        within: std::time::Duration,
    ) -> Result<serde_json::Value, ServiceError> {
        provenant_cli::fabric::graph_relay::from_lane(op, cancel, within)
    }

    fn wait_for_children(
        &mut self,
        cancel: &CancelToken,
        within: std::time::Duration,
    ) -> Result<serde_json::Value, ServiceError> {
        provenant_cli::fabric::graph_relay::wait_for_children(cancel, within)
    }

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
                "OWNER_LOCAL_ONLY",
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
                "UNSUPPORTED_OWNER",
                "peer does not support inbox operations".into(),
            )),
            Err(failure) => {
                let code = match failure.error {
                    FacadeError::LaunchUnknown { .. } => "ambiguous_outcome",
                    FacadeError::PeerVersion | FacadeError::PeerProtocolMismatch { .. } => {
                        "UNSUPPORTED_OWNER"
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
        code: "CLI_FAILED".into(),
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
            resource,
            input_failure,
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
                resource,
                input_failure,
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

pub(crate) fn factory(
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Result<Factory, Box<dyn std::error::Error>> {
    factory_mode(globals, workers, false)
}

pub(crate) fn read_factory(
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Result<Factory, Box<dyn std::error::Error>> {
    factory_mode(globals, workers, true)
}

fn factory_mode(
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
    read_only: bool,
) -> Result<Factory, Box<dyn std::error::Error>> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let context = if read_only {
        Context::open_inspection(resolved.roots, &resolved.home, &resolved.project)?
    } else if provenant_cli::fabric::in_lane() {
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

/// Discover pending journals and check deadlines under the same lock as explicit advance.
pub(crate) fn tick_graphs(
    resolved: &crate::roots::Resolved,
    now: provenant_core::WallTime,
    workers: &'static dyn WorkerObserver,
) -> Result<crate::tick::Graphs, Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, advance_with_budget};
    let mut report = crate::tick::Graphs::default();
    for binding in provenant_config::registered_projects(&resolved.roots)? {
        let project = binding.project_id();
        let ids = match FileJournal::pending_graph_ids(&resolved.roots.state, project) {
            Ok(ids) => ids,
            Err(error) => {
                report.errors.push(format!("{project}: {error}"));
                continue;
            }
        };
        for id in ids {
            let result = (|| -> Result<bool, Box<dyn std::error::Error>> {
                let checkpoint =
                    FileJournal::read_checkpoint_for_tick(&resolved.roots.state, project, &id)?;
                let Some(checkpoint) = checkpoint else {
                    // An already reported unreadable checkpoint must stay marked.
                    if matches!(
                        FileJournal::read_checkpoint(&resolved.roots.state, project, &id),
                        Ok(None)
                    ) {
                        let clock = SystemClock::new();
                        let cancel = CancelToken::new();
                        match FileJournal::open(
                            &resolved.roots.state,
                            project,
                            &id,
                            WaitBudget::nonblocking(clock.mono_now()),
                            &cancel,
                            &clock,
                        ) {
                            Ok(mut journal) => journal.clear_stale_pending()?,
                            Err(provenant_fabric_runtime::graph::JournalError::Filesystem(
                                provenant_os::fs::FsError::LockTimeout { .. },
                            )) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    return Ok(false);
                };
                let due_at_start = checkpoint.cancelled || graph_wake_due(&checkpoint, now);
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
                // Execution can be rebound to another worktree by a manual
                // resume, so resolve it only after acquiring journal custody.
                let Some(execution) =
                    FileJournal::pending_execution(&resolved.roots.state, project, &id)?
                else {
                    journal.clear_stale_pending()?;
                    return Ok(false);
                };
                match std::fs::metadata(&execution.directory) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let reason = format!(
                            "graph {id}: worktree is missing; restore it or cancel this graph"
                        );
                        if FileJournal::read_advance_error(&resolved.roots.state, project, &id)?
                            .as_deref()
                            != Some(reason.as_str())
                        {
                            FileJournal::set_advance_error(
                                &resolved.roots.state,
                                project,
                                &id,
                                Some(&reason),
                            )?;
                            report.warnings.push(reason);
                        }
                        return Ok(false);
                    }
                    Err(error) => return Err(error.into()),
                    Ok(_) => {}
                }
                if !due_at_start || !graph_wake_due(&checkpoint, clock.wall_now()?) {
                    return Ok(false);
                }
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
                    capability_overrides: graph_capability_overrides(&execution.document)?,
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
        checkpoint.wake_at.is_none_or(|deadline| now >= deadline)
            || checkpoint.nodes.iter().any(|node| {
                use provenant_fabric_runtime::graph::NodeState;
                node.state == NodeState::Running
                    && !node.attempts.is_empty()
                    && node
                        .launch_retry
                        .as_ref()
                        .and_then(|retry| retry.wake_at)
                        .is_none()
            })
    }
}
impl Factory {
    /// Read a project-local graph document through the confined, bounded helpers.
    pub(crate) fn read_graph_document(
        &self,
        path: &Path,
    ) -> Result<provenant_fabric_runtime::graph::GraphDocument, FacadeError> {
        Ok(self.read_graph_document_bytes(path)?.0)
    }

    pub(crate) fn read_graph_document_bytes(
        &self,
        path: &Path,
    ) -> Result<
        (
            provenant_fabric_runtime::graph::GraphDocument,
            PathBuf,
            Vec<u8>,
        ),
        FacadeError,
    > {
        let path = provenant_os::fs::canonicalize(path)
            .map_err(|_| FacadeError::io("read project-local graph document; a bare name is read only from .provenant/scratch/graphs, so pass the explicit path of a legacy .agent-run/graphs document"))?;
        if !path.starts_with(&self.cwd) {
            return Err(FacadeError::ConfinementRefused { resource: None });
        }
        let bytes = provenant_os::fs::FileHandle::open_source(&path)
            .and_then(|mut file| {
                file.read_bounded(
                    provenant_fabric_runtime::config::GraphLimits::document_bytes_bound()
                        .maximum_u64(),
                )
            })
            .map_err(|error| match error {
                provenant_os::fs::FsError::TooLarge { .. } => {
                    FacadeError::invalid(InvalidInputDetail::GraphDocumentTooLarge)
                }
                _ => FacadeError::io("access graph journal"),
            })?;
        let document = decode_graph_document(&bytes)?;
        Ok((document, path, bytes))
    }

    /// The one selector resolver for graph status, cancel and resume: a graph
    /// ID, a project-local document path or a document SHA-256 hash (prefix)
    /// becomes the graph ID and, when one is retained or named, its document.
    pub(crate) fn resolve_graph(
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
    pub(crate) fn documents_by_hash(
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
            if let Ok(bytes) = graphs.read_bounded(
                &name,
                provenant_fabric_runtime::config::GraphLimits::document_bytes_bound().maximum_u64(),
            ) && provenant_records::CanonicalV1::file_digest(&bytes)
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

    pub(crate) fn graph(
        &self,
        input: GraphInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        self.graph_with_scratch(input, budget, cancel, None)
    }

    pub(crate) fn graph_with_scratch(
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
            .map_err(|error| {
                FacadeError::from(
                    &provenant_fabric_runtime::coordination::CoordinationError::Input(error),
                )
            })?;
        let resume = matches!(&input, GraphInput::Resume { .. });
        let mut scratch_source = None;
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
                let mut document = if resume {
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
                    let (document, source, bytes) = self.read_graph_document_bytes(&path)?;
                    if let Ok(graphs) = provenant_os::fs::canonicalize(&scratch.join("graphs"))
                        && source.parent() == Some(graphs.as_path())
                        && let Some(name) = source.file_name()
                    {
                        scratch_source = Some((graphs, name.to_owned(), bytes));
                    }
                    document
                };
                if !document.extra.contains_key("submission_patch") {
                    let submission_key = document
                        .extra
                        .get("provenance")
                        .and_then(|value| value.get("operation_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("submit:{}", document.graph_id));
                    provenant_fabric_runtime::graph::patch::submission(
                        &mut document,
                        &submission_key,
                        &self.context.config.graph.limits,
                    )
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                }
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
                document
                    .admit(&self.context.config.graph.limits)
                    .map_err(FacadeError::graph_admission)?;
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
                        .map_err(|_| FacadeError::ConfinementRefused { resource: None })?;
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
                outcome: None,
                checkpoint: GraphCheckpoint {
                    bound_review_base: None,
                    bound_integration_base: None,
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
            let mut checkpoint = match FileJournal::read_status_checkpoint(
                &self.context.roots.state,
                self.context.identity.project,
                id,
            ) {
                Ok(checkpoint) => checkpoint.ok_or(FacadeError::NotFound)?,
                Err(error) if error.is_unreadable() => {
                    return Ok(unreadable_snapshot(format!(
                        "Graph {id} record is unreadable ({error}); use `provenant stop {id}` to retire the record. Inspect active attempts and writer fences."
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
                |checkpoint| {
                    let Some(checkpoint) = checkpoint else {
                        return Ok(());
                    };
                    let mut first_error = None;
                    let mut owner = BinaryRunOwner {
                        factory: self,
                        allow_remote: true,
                        actor: None,
                        chair: None,
                    };
                    for attempt in checkpoint.nodes.iter().flat_map(|node| &node.attempts) {
                        if attempt.state != provenant_fabric_runtime::graph::AttemptState::Terminal
                            && let Err(error) =
                                self.cancel_graph_attempt(&mut owner, attempt, budget, cancel)
                        {
                            first_error.get_or_insert(error);
                        }
                    }
                    if let (Some(instance_id), Some(actor)) =
                        (&checkpoint.instance_id, &checkpoint.ask_actor)
                    {
                        for ask in checkpoint.asks.iter().filter(|ask| !ask.closed) {
                            let scope = provenant_fabric_runtime::graph::AskScope {
                                graph_id: &checkpoint.graph_id,
                                instance_id,
                                actor,
                                ask,
                            };
                            if let Err(error) = graph_ask_edge(self, cancel, scope, true) {
                                first_error.get_or_insert(error);
                            }
                        }
                    } else if checkpoint.asks.iter().any(|ask| !ask.closed) {
                        first_error.get_or_insert(FacadeError::StateCorrupt);
                    }
                    if let Some(error) = first_error {
                        return Err(Box::new(error));
                    }
                    Ok(())
                },
            )
            .map_err(|error| FacadeError::DispatchRefused {
                reason: format!("cancel graph {id}: {error}"),
                fact: None,
                disk: None,
            })?
        {
            return Ok(unreadable_snapshot(
                "Unreadable graph record retired. Identifiable attempts and inbox requests were cancelled; unidentifiable work and writer fences require inspection.".into(),
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
            // GC and admission hold this graph's lock. If collection removed
            // the source after it was read, restore the exact bytes before
            // publishing a new pending checkpoint or launching a node.
            if let Some((directory, name, bytes)) = &scratch_source {
                restore_scratch_source(directory, name, bytes)?;
            }
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
                // A missing local integration branch starts where the batch is
                // measured from, rather than refusing the graph.
                if !branch.starts_with("origin/") {
                    provenant_run::ensure_local_branch(
                        &git,
                        &self.cwd,
                        branch,
                        document
                            .review_base
                            .as_deref()
                            .filter(|base| !base.is_empty()),
                    )
                    .map_err(run_error)?;
                }
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
                        .map_err(|error| match error {
                            provenant_run::RunError::InvalidBase(_) => {
                                FacadeError::DispatchRefused {
                                    fact: None,
                                    disk: None,
                                    reason: error.to_string(),
                                }
                            }
                            error => run_error(error),
                        })?
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
                capability_overrides: graph_capability_overrides(document)?,
                exclude: graph_exclude(document)?,
                budget,
                cancel,
                prices: None,
                tick: false,
            };
            let graph_config = &owner.factory.context.config.graph;
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
                    FacadeError::InvalidInput { detail: None }
                }
                provenant_fabric_runtime::graph::GraphError::Admission(reason) => match reason {
                    provenant_fabric_runtime::graph::AdmissionError::Schema(_) => {
                        FacadeError::invalid(provenant_fabric_runtime::coordination::facade::InvalidInputDetail::Schema)
                    }
                    _ => FacadeError::InvalidInput { detail: None },
                },
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
            let mut run = run;
            redact_checkout_paths(&mut run.checkpoint);
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
        if let GraphInput::Cancel { reason, .. } = &input {
            // Persist the admission fence before asking the run owner to stop live attempts.
            if !checkpoint.cancelled || reason.is_some() {
                checkpoint.cancel_reason = reason.clone();
            }
            checkpoint.cancelled = true;
            let detached = provenant_fabric_runtime::graph::relay::fence_detached_cancellation(
                &FileJournal::execution(
                    &self.context.roots.state,
                    self.context.identity.project,
                    id,
                )
                .map_err(graph_journal_error)?
                .ok_or(FacadeError::NotFound)?
                .document,
                &mut checkpoint,
            )
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
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
                if detached.contains(&node.id) {
                    continue;
                }
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
                capability_overrides: Default::default(),
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
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        Ok(graph_snapshot(checkpoint))
    }

    pub(crate) fn prepare_scratch(
        &self,
        cancel: &CancelToken,
        budget: WaitBudget,
    ) -> Result<PathBuf, provenant_confinement::worktrees::WorktreeError> {
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
    }
}

fn graph_selector_forms() -> String {
    "unknown or ambiguous graph selector; accepted forms are a graph ID, a project-local document path, or the document SHA-256 hash".to_owned()
}

impl Factory {
    /// Cancel one live graph attempt. A missing checkpoint reference (a crash
    /// between acceptance and the save) is recovered from the stable operation ID.
    pub(crate) fn cancel_graph_attempt(
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
        for target in targets {
            if target
                .host
                .is_none_or(|host| host == self.context.identity.host)
            {
                let store = owner.store(cancel)?;
                let call = owner.call(cancel);
                let reference = format!("{}/{}", self.context.identity.host, target.attempt);
                match provenant_cli::fabric::graph_cloud::installed(
                    &store,
                    target.attempt,
                    "",
                    &reference,
                    &call,
                    &self.cwd,
                    std::time::Duration::from_micros(
                        budget.remaining(self.clock.mono_now()).as_micros(),
                    ),
                    true,
                ) {
                    Ok(Some(_)) => continue,
                    Ok(None) => {}
                    Err(_error) => {
                        first_error.get_or_insert(FacadeError::InvalidInput { detail: None });
                        continue;
                    }
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
#[cfg(test)]
#[allow(
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
use test_presentation::*;

impl Factory {
    pub(crate) fn pull_events(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<EventCoverage, FacadeError> {
        self.pull_events_from(true, budget, cancel)
    }
    pub(crate) fn pull_events_from(
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

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the custom worktree driver omits libtest functions"
)]
mod test_presentation {
    use super::*;
    fn shell_path(path: &std::path::Path) -> String {
        let value = path.to_string_lossy();
        if value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_./-".contains(character))
        {
            value.into_owned()
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    }
    pub(super) fn graph_display(
        run: &provenant_fabric_runtime::graph::GraphRun,
        command: &str,
        globals: &provenant_cli::Globals,
    ) -> String {
        if command == "status" && globals.full && !globals.json {
            graph_human::graph_human_full(run)
        } else {
            graph_human(run)
        }
    }

    pub(super) fn graph_waiting_human(
        run: &provenant_fabric_runtime::graph::GraphRun,
        globals: &provenant_cli::Globals,
    ) -> String {
        use provenant_fabric_runtime::graph::NodeState;
        let in_flight = run
            .checkpoint
            .nodes
            .iter()
            .filter(|node| matches!(node.state, NodeState::Running | NodeState::Pending))
            .count();
        let mut status = String::from("provenant");
        if let Some(state_dir) = &globals.state_dir {
            status.push_str(&format!(" --state-dir {}", shell_path(state_dir)));
        }
        if let Some(project) = &globals.project {
            status.push_str(&format!(" --project {}", shell_path(project)));
        }
        status.push_str(&format!(" status {}", run.checkpoint.graph_id));
        format!(
            "Graph {}: waiting ({in_flight} nodes in flight); status: {status}\n{}",
            run.checkpoint.graph_id,
            graph_human(run)
        )
    }

    pub(super) fn graph_command_result(
        command: &str,
        run: provenant_fabric_runtime::graph::GraphRun,
    ) -> provenant_records::CommandResult<provenant_fabric_runtime::graph::GraphRun> {
        let outcome = if command == "status wait"
            && run.status == provenant_fabric_runtime::graph::GraphStatus::Pending
        {
            provenant_records::Outcome::Waiting
        } else {
            provenant_records::Outcome::Ok
        };
        provenant_records::CommandResult::new(command, outcome, run)
    }

    pub(super) fn wait_graph(
        clock: &dyn Clock,
        pause: &dyn provenant_core::Pause,
        seconds: u64,
        cancel: &CancelToken,
        mut observe: impl FnMut() -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, Box<dyn std::error::Error>> {
        let budget = WaitBudget::cli(clock.mono_now(), seconds)?;
        loop {
            let run = observe()?;
            let remaining = budget.remaining(clock.mono_now());
            if run.status != provenant_fabric_runtime::graph::GraphStatus::Pending
                || remaining.as_micros() == 0
            {
                return Ok(run);
            }
            pause.wait(
                provenant_core::DurationMicros::from_micros(250_000.min(remaining.as_micros())),
                cancel,
            );
        }
    }
}
