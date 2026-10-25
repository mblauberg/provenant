//! Read-only peer placement. `dispatch --host` forwards one batch to the
//! peer's fixed `fabric peer` entrypoint; `lanes`, `output` and `cancel`
//! forward by ID. Routing, confinement and execution stay on the peer. This
//! host keeps only a placement record under `fabric/remote-runs/`, never a
//! copy of the peer's run state.
use super::{Condition, Edge, Fail, Reply, Status, condition_met, render_lanes, reply, state_name};
use crate::{
    Cancel, Dispatch, Lanes, Output, Part,
    fabric::{Context, PeerFailure},
};
use provenant_core::{
    AttemptId, CancelToken, Clock, DeliveryId, HostId, OperationId, Roots, RunId, TaskId,
    WaitBudget, WallTime,
};
use provenant_fabric_runtime::{
    coordination::facade::{
        AttemptTarget, FacadeError, NewWorktree, OutputBytes, OutputInput, OutputPart,
        OutputResult, RoutePreference, RunInput, SubmitInput, SubmittedTask,
    },
    graph::AccessMode,
    hosts::{PeerAction, PeerResult},
    identity::CrossHostProjectKey,
    runs::RunResult,
};
use provenant_run as run;
use provenant_supervisor::{
    clock::SystemClock,
    fs::{Directory, Publication, PublishMode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    time::{Duration, Instant},
};

const PLACEMENTS: &str = "fabric/remote-runs";
const PLACEMENT_LIMIT: u64 = 64 * 1024;
/// Each poll is one SSH round trip, so waits poll less often than local ones.
const POLL: Duration = Duration::from_secs(2);

/// Route text exactly as given, so the peer resolves it as the CLI would.
pub(super) struct RouteText {
    pub(super) model: Option<String>,
    pub(super) route: Option<String>,
    pub(super) effort: Option<String>,
}
pub(super) struct Task {
    pub(super) prompt: String,
    pub(super) route: RouteText,
    pub(super) account: Option<String>,
    pub(super) delivery_id: Option<DeliveryId>,
    pub(super) deliver: Option<run::WriterDeliveryRequest>,
}
pub(super) struct Batch {
    pub(super) operation_id: OperationId,
    pub(super) tasks: Vec<Task>,
    pub(super) concurrency: Option<u64>,
    pub(super) knobs: Vec<String>,
    pub(super) provider_timeout: u64,
    /// A writer's worktree, created by the peer in its own clone.
    pub(super) new_worktree: Option<NewWorktree>,
}

/// Where a run lives. The peer owns everything else about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Placement {
    schema_version: u64,
    host: HostId,
    project: CrossHostProjectKey,
    operation_id: OperationId,
    run_id: RunId,
    attempts: Vec<Placed>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Placed {
    task_id: TaskId,
    attempt_id: AttemptId,
}
impl Placement {
    /// Attempts the target names, in dispatch order.
    fn select(
        &self,
        task: Option<TaskId>,
        attempt: Option<AttemptId>,
    ) -> Result<Vec<(HostId, AttemptId)>, Fail> {
        let selected: Vec<_> = self
            .attempts
            .iter()
            .filter(|p| task.is_none_or(|t| t == p.task_id))
            .filter(|p| attempt.is_none_or(|a| a == p.attempt_id))
            .map(|p| (self.host, p.attempt_id))
            .collect();
        if attempt.is_some() && selected.is_empty() {
            return Err(Fail::new(
                Status::Invalid,
                "NOT_FOUND",
                "attempt is not in that run/task",
            ));
        }
        Ok(selected)
    }
}

fn io_fail(error: impl std::fmt::Display) -> Fail {
    Fail::new(Status::Error, "IO", error.to_string())
}
fn corrupt() -> Fail {
    Fail::new(
        Status::Error,
        "STATE_CORRUPT",
        "peer reply does not match the request",
    )
}
fn unsupported(what: &str) -> Fail {
    Fail::new(
        Status::Refused,
        "UNSUPPORTED",
        format!("{what} is not available for a peer run in this build"),
    )
}
/// Map a peer refusal to the CLI's stable codes. Unknown launches keep their
/// operation ID: replaying the same command recovers the receipt.
/// A transport failure names the peer and cause in the message; the stable
/// code is unchanged.
fn peer_failure_fail(failure: PeerFailure) -> Fail {
    let PeerFailure { error, detail } = failure;
    let message = match detail {
        Some(detail) => format!("{error} ({detail})"),
        None => error.to_string(),
    };
    if let FacadeError::LaunchUnknown { operation_id } = &error {
        return Fail::new(
            Status::Error,
            "LAUNCH_UNKNOWN",
            format!(
                "{message}; rerun with --operation-id {operation_id} to recover the \
                 peer's receipt before submitting new work"
            ),
        );
    }
    let (status, code) = crate::fabric::facade_outcome(&error);
    Fail::new(status, code, format!("peer: {message}"))
}

fn directory(roots: &Roots) -> Result<Directory, Fail> {
    let root = roots.state.as_path();
    crate::fabric::private_dir(root, &root.join(PLACEMENTS)).map_err(io_fail)
}
fn read(directory: &Directory, name: &str, run: RunId) -> Result<Placement, Fail> {
    let bytes = directory
        .read_bounded(OsStr::new(name), PLACEMENT_LIMIT)
        .map_err(io_fail)?;
    let placement: Placement = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
    if placement.schema_version != 1 || placement.run_id != run {
        return Err(corrupt());
    }
    Ok(placement)
}
/// Record a placement. Replaying the operation refreshes its attempts; any
/// other existing record for the run is a conflict.
fn save(roots: &Roots, placement: &Placement) -> Result<(), Fail> {
    let directory = directory(roots)?;
    let name = format!("{}.json", placement.run_id);
    if directory.metadata_at(OsStr::new(&name), false).is_ok() {
        let existing = read(&directory, &name, placement.run_id)?;
        if (existing.host, &existing.project, &existing.operation_id)
            != (placement.host, &placement.project, &placement.operation_id)
        {
            return Err(Fail::new(
                Status::Refused,
                "CONFLICT",
                "another placement already names this run",
            ));
        }
    }
    let bytes = serde_json::to_vec(placement).map_err(io_fail)?;
    match directory
        .atomic_write(
            OsStr::new(&name),
            ".placement-",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(io_fail)?
    {
        Publication::Durable => Ok(()),
        Publication::VisibleDurabilityUnconfirmed { sync_error } => Err(io_fail(sync_error)),
    }
}
/// The placement of a run dispatched to a peer from this host, if any.
pub(super) fn find(roots: &Roots, run: RunId) -> Result<Option<Placement>, Fail> {
    let name = format!("{run}.json");
    let path = roots.state.as_path().join(PLACEMENTS).join(&name);
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    read(&directory(roots)?, &name, run).map(Some)
}

/// One Fabric identity and a bounded call budget per peer request.
struct Peer {
    context: Context,
    clock: SystemClock,
    cancel: CancelToken,
}
impl Peer {
    fn open(edge: &Edge<'_>) -> Result<Self, Fail> {
        let context = Context::open(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, "FABRIC", error.to_string()))?;
        Ok(Self {
            context,
            clock: SystemClock::new(),
            cancel: CancelToken::new(),
        })
    }
    fn project(&self) -> Result<CrossHostProjectKey, Fail> {
        self.context
            .identity
            .federation
            .as_ref()
            .map(|project| project.key.clone())
            .ok_or_else(|| {
                Fail::new(
                    Status::Refused,
                    "REFUSED",
                    "peer placement needs a Git project inside the home directory",
                )
            })
    }
    /// Open for placements made from this project only.
    fn open_for<'p>(
        edge: &Edge<'_>,
        placements: impl IntoIterator<Item = &'p Placement>,
    ) -> Result<Self, Fail> {
        let peer = Self::open(edge)?;
        let project = peer.project()?;
        if placements.into_iter().any(|p| p.project != project) {
            return Err(Fail::new(
                Status::Invalid,
                "NOT_FOUND",
                "that run was placed from another project",
            ));
        }
        Ok(peer)
    }
    fn call(&self, host: HostId, action: PeerAction) -> Result<PeerResult, Fail> {
        let budget = WaitBudget::cli(
            self.clock.mono_now(),
            self.context.config.limits.peer_seconds,
        )
        .map_err(io_fail)?;
        self.context
            .peer(&self.clock, host, action, budget, &self.cancel)
            .map_err(peer_failure_fail)
    }
    /// Current owner records, one batched status read per host.
    fn records(&self, selected: &[(HostId, AttemptId)]) -> Result<Vec<run::AttemptRecord>, Fail> {
        let mut by_host: BTreeMap<HostId, Vec<usize>> = BTreeMap::new();
        for (index, (host, _)) in selected.iter().enumerate() {
            by_host.entry(*host).or_default().push(index);
        }
        let mut records: Vec<Option<run::AttemptRecord>> = std::iter::repeat_with(|| None)
            .take(selected.len())
            .collect();
        for (host, indices) in by_host {
            for chunk in indices.chunks(100) {
                let targets = chunk
                    .iter()
                    .map(|i| AttemptTarget {
                        attempt: selected[*i].1,
                        host: Some(host),
                    })
                    .collect();
                let PeerResult::RunStatuses(page) =
                    self.call(host, PeerAction::RunStatuses { targets })?
                else {
                    return Err(corrupt());
                };
                if page.len() != chunk.len() {
                    return Err(corrupt());
                }
                for (index, record) in chunk.iter().zip(page) {
                    if record.attempt_id != selected[*index].1 {
                        return Err(corrupt());
                    }
                    records[*index] = Some(record);
                }
            }
        }
        records
            .into_iter()
            .collect::<Option<_>>()
            .ok_or_else(corrupt)
    }
    /// Poll the owner until the condition holds or the reader deadline passes.
    /// Expiry never stops a provider. A transient peer failure is retried within
    /// the budget; the last cause is reported only if the budget runs out on it.
    fn wait(
        &self,
        selected: &[(HostId, AttemptId)],
        condition: Condition,
        seconds: u64,
    ) -> Result<(Vec<run::AttemptRecord>, bool), Fail> {
        poll_until(
            || self.records(selected),
            condition,
            Duration::from_secs(seconds),
            POLL,
        )
    }
}

/// Poll `fetch` until the condition holds or `budget` passes.
fn poll_until(
    mut fetch: impl FnMut() -> Result<Vec<run::AttemptRecord>, Fail>,
    condition: Condition,
    budget: Duration,
    poll: Duration,
) -> Result<(Vec<run::AttemptRecord>, bool), Fail> {
    let deadline = Instant::now() + budget;
    loop {
        match fetch() {
            Ok(lanes) => {
                let met = condition_met(&lanes, condition);
                if met || Instant::now() >= deadline {
                    return Ok((lanes, met));
                }
            }
            Err(fail) if fail.code == "PEER_UNAVAILABLE" => {
                if Instant::now() >= deadline {
                    return Err(fail);
                }
            }
            Err(fail) => return Err(fail),
        }
        std::thread::park_timeout(poll.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Local lane rendering plus each row's owning host. Result paths are the
/// peer's; read them with `output`.
fn render(
    lanes: &[run::AttemptRecord],
    selected: &[(HostId, AttemptId)],
    edge: &Edge<'_>,
) -> (Value, String) {
    let (mut data, human) = render_lanes(lanes, None, edge.roots.state.as_path());
    if let Some(rows) = data["lanes"].as_array_mut() {
        for (row, lane) in rows.iter_mut().zip(lanes) {
            let host = selected
                .iter()
                .find(|(_, attempt)| *attempt == lane.attempt_id)
                .map(|(host, _)| *host);
            row["host"] = json!(host);
            row["result_path"] = Value::Null;
        }
    }
    let mut hosts: Vec<_> = selected.iter().map(|(host, _)| host.to_string()).collect();
    hosts.dedup();
    (data, format!("host {}\n{human}", hosts.join(",")))
}

/// Whether `host`, a peer label or host ID, names this machine.
/// `local`, `localhost`, this host's ID, or a peer label resolving to it.
pub(super) fn names_local_host(edge: &Edge<'_>, host: &str) -> Result<bool, Fail> {
    if matches!(host, "local" | "localhost") {
        return Ok(true);
    }
    let peer = Peer::open(edge)?;
    let local = peer.context.identity.host;
    Ok(HostId::parse(host).is_ok_and(|id| id == local)
        || peer.context.peer_host(host) == Some(local))
}

pub(super) fn names_peer(edge: &Edge<'_>, host: &str) -> Result<bool, Fail> {
    Ok(Peer::open(edge)?.context.peer_host(host).is_some())
}

pub(super) fn unknown_host(host: &str) -> Fail {
    Fail::new(
        Status::Invalid,
        "UNKNOWN_HOST",
        format!(
            "no configured peer named {host}; use `local` for this host or see `provenant hosts`"
        ),
    )
}

/// The worktree a peer writer asks the peer to create. One writer per
/// worktree, so a manifest cannot share it.
pub(super) fn new_worktree(command: &Dispatch) -> Result<NewWorktree, Fail> {
    if command.input.manifest.is_some() {
        return Err(unsupported("a manifest with a peer writer"));
    }
    let (Some(branch), Some(base)) = (&command.branch, &command.base) else {
        return Err(Fail::new(
            Status::Invalid,
            "WORKTREE_NOT_APPLICABLE",
            "--worktree names a path on this host; a peer writer takes --branch and --base",
        ));
    };
    if provenant_confinement::repo::GitRefName::branch(branch).is_err()
        || provenant_confinement::repo::GitRevision::new(base).is_err()
    {
        return Err(Fail::new(
            Status::Invalid,
            "INVALID",
            "invalid --branch or --base",
        ));
    }
    Ok(NewWorktree {
        base: base.clone(),
        branch: branch.clone(),
    })
}

pub(super) fn dispatch(
    edge: &Edge<'_>,
    host: &str,
    batch: Batch,
    wait: Option<u64>,
) -> Result<(Value, String, bool), Fail> {
    let peer = Peer::open(edge)?;
    let project = peer.project()?;
    let host = peer
        .context
        .peer_host(host)
        .ok_or_else(|| unknown_host(host))?;
    let timeout_ms = batch
        .provider_timeout
        .checked_mul(1000)
        .ok_or_else(|| Fail::new(Status::Invalid, "INVALID", "provider timeout"))?;
    let access = match batch.new_worktree {
        Some(_) => AccessMode::WorktreeWrite,
        None => AccessMode::ReadOnly,
    };
    let tasks = batch
        .tasks
        .into_iter()
        .map(|task| SubmittedTask {
            account: task.account,
            access,
            worktree: None,
            new_worktree: batch.new_worktree.clone(),
            prompt: task.prompt,
            route: RoutePreference {
                adapter: None,
                model: task.route.model,
                route: task.route.route,
                role: None,
                task_class: None,
                effort: task.route.effort,
            },
            timeout_ms,
            delivery_id: task.delivery_id,
            deliver: task.deliver,
        })
        .collect();
    let concurrency = batch
        .concurrency
        .map(u16::try_from)
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, "INVALID", "concurrency"))?;
    let input = SubmitInput::Submission {
        operation_id: batch.operation_id.clone(),
        tasks,
        concurrency,
        knobs: batch.knobs,
        host: None,
    };
    let PeerResult::Submitted(receipt) = peer.call(host, PeerAction::Submit { input })? else {
        return Err(corrupt());
    };
    if receipt.operation_id != batch.operation_id
        || receipt.targets.is_empty()
        || receipt.targets.iter().any(|t| t.host != Some(host))
        || receipt.worktree.is_some() != (access == AccessMode::WorktreeWrite)
    {
        return Err(corrupt());
    }
    let selected: Vec<_> = receipt.targets.iter().map(|t| (host, t.attempt)).collect();
    let lanes = peer.records(&selected)?;
    let run_id = lanes.first().ok_or_else(corrupt)?.run_id;
    if lanes.iter().any(|lane| lane.run_id != run_id) {
        return Err(corrupt());
    }
    save(
        edge.roots,
        &Placement {
            schema_version: 1,
            host,
            project,
            operation_id: batch.operation_id.clone(),
            run_id,
            attempts: lanes
                .iter()
                .map(|lane| Placed {
                    task_id: lane.task_id,
                    attempt_id: lane.attempt_id,
                })
                .collect(),
        },
    )?;
    let (lanes, met) = match wait {
        Some(seconds) => {
            let (lanes, met) = peer.wait(&selected, Condition::All, seconds)?;
            (lanes, Some(met))
        }
        None => (lanes, None),
    };
    let (mut data, human) = render(&lanes, &selected, edge);
    data["run_id"] = json!(run_id);
    data["operation_id"] = json!(batch.operation_id);
    data["host"] = json!(host);
    if let Some(met) = met {
        data["condition_met"] = json!(met);
    }
    // Undelivered commits stay in the peer's worktree; say where.
    let human = match &receipt.worktree {
        Some(worktree) => {
            data["worktree"] =
                json!({ "host": host, "path": worktree.path, "branch": worktree.branch });
            format!(
                "worktree {} {} (on the peer)\n{human}",
                worktree.branch, worktree.path
            )
        }
        None => human,
    };
    Ok((data, format!("run {run_id}\n{human}"), met == Some(false)))
}

/// Forward `lanes --id` when every target names a peer run.
pub(super) fn lanes(
    command: &Lanes,
    edge: &Edge<'_>,
) -> Result<Option<(Value, String, bool)>, Fail> {
    let placements = command
        .id
        .iter()
        .map(|target| find(edge.roots, target.run))
        .collect::<Result<Vec<_>, _>>()?;
    let placed: Vec<&Placement> = placements.iter().flatten().collect();
    if placed.is_empty() {
        return Ok(None);
    }
    if placed.len() != placements.len() {
        return Err(Fail::new(
            Status::Invalid,
            "INVALID",
            "list peer runs and local runs in separate lanes calls",
        ));
    }
    if command.cursor.is_some() {
        return Err(unsupported("--cursor"));
    }
    let mut selected = Vec::new();
    for (target, placement) in command.id.iter().zip(&placed) {
        for pair in placement.select(target.task, target.attempt)? {
            if !selected.contains(&pair) {
                selected.push(pair);
            }
        }
    }
    let peer = Peer::open_for(edge, placed)?;
    if let Some(seconds) = command.wait {
        let (lanes, met) = peer.wait(&selected, command.condition, seconds)?;
        let (mut data, human) = render(&lanes, &selected, edge);
        data["condition_met"] = json!(met);
        return Ok(Some((data, human, !met)));
    }
    let since = command
        .since
        .map(|since| WallTime::from_micros(since.as_microsecond()))
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, "INVALID", "invalid --since value"))?;
    let lanes: Vec<_> = peer
        .records(&selected)?
        .into_iter()
        .filter(|lane| {
            command.states.is_empty()
                || command
                    .states
                    .iter()
                    .any(|state| state.as_str() == state_name(&lane.state))
        })
        .filter(|lane| since.is_none_or(|since| lane.queued_at >= since))
        .take(usize::try_from(command.limit).unwrap_or(usize::MAX))
        .collect();
    let (data, human) = render(&lanes, &selected, edge);
    Ok(Some((data, human, false)))
}

/// `lanes --host`: the peer's own lanes, read over the peer channel. The
/// peer applies `--all` and `--limit`; state and time filters apply here to
/// the page it returns.
pub(super) fn host_lanes(
    command: &Lanes,
    host: &str,
    edge: &Edge<'_>,
) -> Result<(Value, String, bool), Fail> {
    let peer = Peer::open(edge)?;
    peer.project()?;
    let id = peer
        .context
        .peer_host(host)
        .ok_or_else(|| unknown_host(host))?;
    let limit = u16::try_from(command.limit)
        .map_err(|_| Fail::new(Status::Invalid, "INVALID", "invalid --limit"))?;
    let PeerResult::RunStatuses(records) = peer.call(
        id,
        PeerAction::Lanes {
            all: command.all,
            limit,
        },
    )?
    else {
        return Err(corrupt());
    };
    if records.len() > usize::from(limit) {
        return Err(corrupt());
    }
    let since = command
        .since
        .map(|since| WallTime::from_micros(since.as_microsecond()))
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, "INVALID", "invalid --since value"))?;
    let lanes: Vec<_> = records
        .into_iter()
        .filter(|lane| {
            command.states.is_empty()
                || command
                    .states
                    .iter()
                    .any(|state| state.as_str() == state_name(&lane.state))
        })
        .filter(|lane| since.is_none_or(|since| lane.queued_at >= since))
        .collect();
    let selected: Vec<_> = lanes.iter().map(|lane| (id, lane.attempt_id)).collect();
    let (data, human) = if selected.is_empty() {
        let (mut data, human) = render_lanes(&[], None, edge.roots.state.as_path());
        data["host"] = json!(id);
        (data, format!("host {id}\n{human}"))
    } else {
        render(&lanes, &selected, edge)
    };
    Ok((data, human, false))
}

pub(super) fn output(
    command: &Output,
    placement: &Placement,
    edge: &Edge<'_>,
) -> Result<(Value, String), Fail> {
    let target = &command.attempt.0;
    let [(host, attempt)] = placement.select(target.task, target.attempt)?[..] else {
        return Err(Fail::new(Status::Invalid, "INVALID", "attempt required"));
    };
    let part = match command.part {
        Part::Result => OutputPart::Stdout,
        Part::Stderr => OutputPart::Stderr,
        Part::Prompt | Part::Events => return Err(unsupported("--part prompt or events")),
    };
    if command.tail.is_some() {
        return Err(unsupported("--tail"));
    }
    let peer = Peer::open_for(edge, [placement])?;
    let input = OutputInput::Read {
        target: AttemptTarget {
            attempt,
            host: Some(host),
        },
        part,
        offset: command.offset,
        max_bytes: command.max_bytes,
    };
    let PeerResult::Output(OutputResult::Chunk {
        offset,
        next_offset,
        eof,
        digest,
        bytes,
        ..
    }) = peer.call(host, PeerAction::Output { input })?
    else {
        return Err(corrupt());
    };
    let text = match bytes {
        OutputBytes::Utf8(text) => text,
        OutputBytes::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
    };
    Ok((
        json!({
            "attempt_id": attempt,
            "host": host,
            "part": format!("{:?}", command.part).to_lowercase(),
            "offset": offset,
            "next_offset": next_offset,
            "eof": eof,
            "digest": digest,
            "text": text,
        }),
        text,
    ))
}

/// Ask the owning peer to cancel each named attempt. The peer records its own
/// reason; one cancel operation is bound per attempt.
pub(super) fn cancel(command: &Cancel, placement: &Placement, edge: &Edge<'_>) -> Reply {
    let mut status = Status::Ok;
    let mut out = reply(
        "cancel",
        (|| {
            let target = &command.target;
            let selected = placement.select(target.task, target.attempt)?;
            if selected.is_empty() {
                return Err(Fail::new(
                    Status::Invalid,
                    "NOT_FOUND",
                    "no matching attempts",
                ));
            }
            let peer = Peer::open_for(edge, [placement])?;
            let mut receipts = Vec::new();
            let mut human = String::new();
            for (index, (host, attempt)) in selected.iter().enumerate() {
                let operation_id = if selected.len() == 1 {
                    command.operation_id.clone()
                } else {
                    OperationId::parse(&format!("{}.{index}", command.operation_id)).map_err(
                        |_| {
                            Fail::new(
                                Status::Invalid,
                                "INVALID",
                                "operation ID is too long to derive one per attempt",
                            )
                        },
                    )?
                };
                let input = RunInput::Cancel {
                    target: AttemptTarget {
                        attempt: *attempt,
                        host: Some(*host),
                    },
                    operation_id,
                };
                let row = match peer.call(*host, PeerAction::Run { input }) {
                    Ok(PeerResult::Run(result)) => match *result {
                        RunResult::Cancel { attempt: record } if record.attempt_id == *attempt => {
                            let state = state_name(&record.state);
                            // A live attempt has a stop request, not an exit.
                            let tag = if super::settled(&record.state).0 {
                                state.as_str()
                            } else {
                                "requested"
                            };
                            human.push_str(&format!("{attempt} {tag}\n"));
                            json!({ "attempt_id": attempt, "host": host, "state": state })
                        }
                        _ => return Err(corrupt()),
                    },
                    Ok(_) => return Err(corrupt()),
                    Err(fail) => {
                        status = Status::Failed;
                        human.push_str(&format!("{attempt} error: {}\n", fail.message));
                        json!({ "attempt_id": attempt, "host": host, "error": { "code": fail.code, "message": fail.message } })
                    }
                };
                receipts.push(row);
            }
            let mut data = json!({
                "operation_id": command.operation_id,
                "host": placement.host,
                "receipts": receipts,
            });
            if let Some(seconds) = command.wait {
                let (lanes, met) = peer.wait(&selected, Condition::All, seconds)?;
                if !met && status == Status::Ok {
                    status = Status::Timeout;
                }
                let (rendered, text) = render(&lanes, &selected, edge);
                data["lanes"] = rendered["lanes"].clone();
                data["condition_met"] = json!(met);
                human.push_str(&text);
            }
            Ok((data, human))
        })(),
    );
    if out.result.status == Status::Ok {
        out.result.status = status;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::RootInputs;
    use provenant_supervisor::fs::LocalFs;

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn roots() -> Result<(Scratch, Roots)> {
        let base = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
            "provenant-remote-{}-{}",
            std::process::id(),
            SystemClock::new().wall_now()?.as_micros()
        ));
        let home = base.join("home");
        std::fs::create_dir_all(&home)?;
        let mut inputs = RootInputs::new(home, base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&LocalFs)?;
        Ok((Scratch(base), roots))
    }
    fn placement(operation: &str) -> Result<Placement> {
        Ok(Placement {
            schema_version: 1,
            host: HostId::from_bytes([7; 16]),
            project: serde_json::from_value(json!("project-key"))?,
            operation_id: OperationId::parse(operation)?,
            run_id: RunId::from_bytes([1; 16]),
            attempts: vec![
                Placed {
                    task_id: TaskId::from_bytes([2; 16]),
                    attempt_id: AttemptId::from_bytes([3; 16]),
                },
                Placed {
                    task_id: TaskId::from_bytes([4; 16]),
                    attempt_id: AttemptId::from_bytes([5; 16]),
                },
            ],
        })
    }

    #[test]
    fn placement_replays_its_operation_and_refuses_another() -> Result {
        let (_scratch, roots) = roots()?;
        let run = RunId::from_bytes([1; 16]);
        assert_eq!(find(&roots, run).map_err(|f| f.message)?, None);
        let first = placement("place")?;
        save(&roots, &first).map_err(|f| f.message)?;
        save(&roots, &first).map_err(|f| f.message)?;
        assert_eq!(find(&roots, run).map_err(|f| f.message)?, Some(first));
        let other = save(&roots, &placement("other")?).err().ok_or("conflict")?;
        assert_eq!(other.code, "CONFLICT");
        Ok(())
    }

    #[test]
    fn selection_narrows_by_task_and_attempt() -> Result {
        let placed = placement("place")?;
        let host = HostId::from_bytes([7; 16]);
        assert_eq!(placed.select(None, None).map_err(|f| f.message)?.len(), 2);
        assert_eq!(
            placed
                .select(Some(TaskId::from_bytes([4; 16])), None)
                .map_err(|f| f.message)?,
            vec![(host, AttemptId::from_bytes([5; 16]))]
        );
        let missing = placed
            .select(
                Some(TaskId::from_bytes([2; 16])),
                Some(AttemptId::from_bytes([5; 16])),
            )
            .err()
            .ok_or("mismatch")?;
        assert_eq!(missing.code, "NOT_FOUND");
        Ok(())
    }

    #[test]
    fn unknown_launch_names_the_operation_to_replay() -> Result {
        let fail = peer_failure_fail(
            FacadeError::LaunchUnknown {
                operation_id: OperationId::parse("place")?,
            }
            .into(),
        );
        assert_eq!(fail.code, "LAUNCH_UNKNOWN");
        assert!(fail.message.contains("--operation-id place"));
        assert_eq!(
            peer_failure_fail(FacadeError::PeerVersion.into()).code,
            "PEER_VERSION"
        );
        Ok(())
    }
    fn unavailable(detail: &str) -> Fail {
        peer_failure_fail(PeerFailure {
            error: FacadeError::HomeUnavailable,
            detail: Some(detail.to_owned()),
        })
    }

    #[test]
    fn peer_failure_names_the_peer_and_cause_on_the_stable_code() {
        let fail = unavailable("workshop: ssh exited with status 255: Connection refused");
        assert_eq!(fail.code, "PEER_UNAVAILABLE");
        assert!(fail.message.contains("workshop"), "{}", fail.message);
        assert!(
            fail.message.contains("Connection refused"),
            "{}",
            fail.message
        );
    }

    #[test]
    fn wait_retries_a_transient_peer_failure_within_budget() {
        let mut calls = 0;
        let out = poll_until(
            || {
                calls += 1;
                if calls < 3 {
                    Err(unavailable("workshop: ssh timed out"))
                } else {
                    Ok(Vec::new())
                }
            },
            Condition::All,
            Duration::from_secs(30),
            Duration::from_millis(1),
        );
        assert!(matches!(out, Ok((_, true))));
        assert_eq!(calls, 3);
    }

    #[test]
    fn wait_reports_the_last_cause_when_the_budget_runs_out() {
        let mut calls = 0;
        let out = poll_until(
            || {
                calls += 1;
                Err(unavailable(&format!("workshop: failure {calls}")))
            },
            Condition::All,
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        let fail = out.expect_err("budget exhausted");
        assert_eq!(fail.code, "PEER_UNAVAILABLE");
        assert!(calls > 1);
        assert!(fail.message.contains(&format!("failure {calls}")));
    }

    #[test]
    fn wait_does_not_retry_a_non_transient_failure() {
        let mut calls = 0;
        let out = poll_until(
            || {
                calls += 1;
                Err(corrupt())
            },
            Condition::All,
            Duration::from_secs(30),
            Duration::from_millis(1),
        );
        assert_eq!(out.err().map(|f| f.code), Some("STATE_CORRUPT"));
        assert_eq!(calls, 1);
    }
}
