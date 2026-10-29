//! Read-only peer placement. `dispatch --host` forwards one batch to the
//! peer's fixed `fabric peer` entrypoint; `lanes`, `output` and `cancel`
//! forward by ID. Routing, confinement and execution stay on the peer. This
//! host keeps only a placement record under `fabric/remote-runs/`, never a
//! copy of the peer's run state.
use super::watch::{self, Scope};
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
        OutputResult, RunInput, SubmitInput, SubmittedTask,
    },
    graph::AccessMode,
    hosts::{PeerAction, PeerResult},
    identity::{Address, CrossHostProjectKey},
    runs::RunResult,
};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, PublishMode},
};
use provenant_run as run;
use run::ErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    time::{Duration, Instant},
};

const PLACEMENTS: &str = "fabric/remote-runs";
const PLACEMENT_LIMIT: u64 = 64 * 1024;
/// Each poll is one SSH round trip, so waits poll less often than local ones.
const POLL: Duration = Duration::from_secs(2);

pub(super) struct Task {
    pub(super) context: Option<provenant_records::run::WorkContext>,
    pub(super) build_heavy: bool,
    pub(super) title: Option<String>,
    pub(super) base: Option<String>,
    pub(super) head: Option<String>,
    pub(super) prompt: String,
    pub(super) route: run::RequestedRoute,
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
pub(super) struct Placement {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    schema_version: u64,
    host: HostId,
    project: CrossHostProjectKey,
    operation_id: OperationId,
    run_id: RunId,
    attempts: Vec<Placed>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Placed {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    task_id: TaskId,
    attempt_id: AttemptId,
}
impl Placement {
    fn select_target(&self, target: &crate::Target) -> Result<Vec<(HostId, AttemptId)>, Fail> {
        let (mut task, mut attempt) = (target.task, target.attempt);
        // `RUN/ATTEMPT`: the second part names a placed attempt, not a task.
        if let (Some(second), None) = (task, attempt) {
            let candidate = AttemptId::from_bytes(*second.as_bytes());
            if self
                .attempts
                .iter()
                .any(|placed| placed.attempt_id == candidate)
            {
                (task, attempt) = (None, Some(candidate));
            }
        }
        let attempt = attempt.filter(|id| {
            target.run.is_some() || self.attempts.iter().any(|placed| placed.attempt_id == *id)
        });
        self.select(task, attempt)
    }
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
                ErrorKind::NotFound,
                "attempt is not in that run/task",
            ));
        }
        Ok(selected)
    }
}

fn io_fail(error: impl std::fmt::Display) -> Fail {
    Fail::new(Status::Error, ErrorKind::Io, error.to_string())
}
fn corrupt() -> Fail {
    Fail::new(
        Status::Error,
        ErrorKind::StateCorrupt,
        "peer reply does not match the request",
    )
}
fn unsupported(what: &str) -> Fail {
    Fail::new(
        Status::Refused,
        ErrorKind::Unsupported,
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
            ErrorKind::LaunchUnknown,
            format!(
                "{message}; rerun with --operation-id {operation_id} to recover the \
                 peer's receipt before submitting new work"
            ),
        );
    }
    let (status, kind) = crate::fabric::facade_outcome(&error);
    Fail {
        detail: Some(error),
        ..Fail::new(status, kind, format!("peer: {message}"))
    }
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
    let mut placement = placement.clone();
    if directory.metadata_at(OsStr::new(&name), false).is_ok() {
        let existing = read(&directory, &name, placement.run_id)?;
        if (existing.host, &existing.project, &existing.operation_id)
            != (placement.host, &placement.project, &placement.operation_id)
        {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Conflict,
                "another placement already names this run",
            ));
        }
        for (key, value) in existing.extra {
            placement.extra.entry(key).or_insert(value);
        }
        for placed in &mut placement.attempts {
            if let Some(previous) = existing.attempts.iter().find(|previous| {
                previous.task_id == placed.task_id && previous.attempt_id == placed.attempt_id
            }) {
                for (key, value) in &previous.extra {
                    placed
                        .extra
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
            }
        }
    }
    let bytes = serde_json::to_vec(&placement).map_err(io_fail)?;
    directory
        .atomic_write(
            OsStr::new(&name),
            ".placement-",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(io_fail)?
        .require_durable()
        .map_err(io_fail)
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

/// Bare attempt IDs are resolved from the host's durable peer placements.
pub(super) fn find_target(
    roots: &Roots,
    target: &crate::Target,
    allow_run: bool,
) -> Result<Option<Placement>, Fail> {
    if let Some(run) = target.run {
        return find(roots, run);
    }
    let Some(id) = target.attempt else {
        return Ok(None);
    };
    if !roots.state.as_path().join(PLACEMENTS).exists() {
        return Ok(None);
    }
    let directory = directory(roots)?;
    let mut selected = None;
    for name in directory.entries().map_err(io_fail)? {
        let name = name.map_err(io_fail)?;
        let Some(run) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|id| RunId::parse(id).ok())
        else {
            continue;
        };
        let placement = read(&directory, &name.to_string_lossy(), run)?;
        let attempt = placement
            .attempts
            .iter()
            .any(|placed| placed.attempt_id == id);
        let run = allow_run && placement.run_id.as_bytes() == id.as_bytes();
        if attempt && run || (attempt || run) && selected.is_some() {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::Conflict,
                "bare ID is ambiguous; use RUN/TASK/ATTEMPT",
            ));
        }
        if attempt || run {
            selected = Some(placement);
        }
    }
    Ok(selected)
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
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Fabric, error.to_string()))?;
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
                    ErrorKind::Refused,
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
                ErrorKind::NotFound,
                "that run was placed from another project",
            ));
        }
        Ok(peer)
    }
    fn call(&self, host: HostId, action: PeerAction) -> Result<PeerResult, Fail> {
        self.call_within(host, action, Duration::MAX)
    }
    /// One peer request that takes no longer than `remaining`, the time a wait
    /// or watch has left, nor than the configured peer limit. The SSH child's
    /// termination and reap come out of `remaining` too. Only the first read of
    /// a zero-second wait has nothing left, and it keeps the one-second minimum.
    fn call_within(
        &self,
        host: HostId,
        action: PeerAction,
        remaining: Duration,
    ) -> Result<PeerResult, Fail> {
        let budget = WaitBudget::cli(
            self.clock.mono_now(),
            call_seconds(remaining, self.context.config.limits.peer_seconds),
        )
        .map_err(io_fail)?;
        if remaining.is_zero() || remaining == Duration::MAX {
            return self
                .context
                .peer(&self.clock, host, action, budget, &self.cancel)
                .map_err(peer_failure_fail);
        }
        self.context
            .peer_within(&self.clock, host, action, budget, &self.cancel, remaining)
            .map_err(peer_failure_fail)
    }
    /// Current owner records, one batched status read per host.
    fn records(&self, selected: &[(HostId, AttemptId)]) -> Result<Vec<run::AttemptRecord>, Fail> {
        self.records_within(selected, Duration::MAX)
    }
    /// Every read together takes no longer than `remaining`.
    fn records_within(
        &self,
        selected: &[(HostId, AttemptId)],
        remaining: Duration,
    ) -> Result<Vec<run::AttemptRecord>, Fail> {
        read_records(
            selected,
            &Window::new(remaining),
            &mut |host, action, left| self.call_within(host, action, left),
        )
    }
    /// The peer's newest lanes that `keep` accepts, plus every lane `tracked`
    /// has seen running that those no longer include. Both reads together take
    /// no longer than `remaining`.
    fn newest(
        &self,
        host: HostId,
        remaining: Duration,
        tracked: &mut watch::Tracked,
        keep: &dyn Fn(&run::AttemptRecord) -> bool,
    ) -> Result<Vec<run::AttemptRecord>, Fail> {
        read_newest(
            host,
            &Window::new(remaining),
            tracked,
            keep,
            &mut |host, action, left| self.call_within(host, action, left),
        )
    }
    /// What each queued or starting lane waits for, read from its owner. A
    /// peer that cannot say, or does not know the question, leaves a lane with
    /// no wait rather than failing the listing.
    fn waits(
        &self,
        lanes: &[run::AttemptRecord],
        selected: &[(HostId, AttemptId)],
    ) -> Vec<run::LaneWait> {
        fetch_waits(lanes, selected, |host, action| self.call(host, action))
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
            |remaining| self.records_within(selected, remaining),
            condition,
            Duration::from_secs(seconds),
            POLL,
        )
    }
}

/// The one deadline every transport call of a fetch shares. It is fixed when
/// the fetch starts, so each call sees what the earlier ones left, not the
/// fetch's whole budget.
struct Window {
    /// Unbounded when the budget is too large to represent.
    deadline: Option<Instant>,
    called: Cell<bool>,
}
impl Window {
    // A reader deadline is a real transport boundary.
    #[allow(clippy::disallowed_methods)]
    fn new(remaining: Duration) -> Self {
        Self {
            deadline: Instant::now().checked_add(remaining),
            called: Cell::new(false),
        }
    }
    /// The time the next transport call may take. The first call of a fetch
    /// always runs, because a wait of zero seconds is one read. Every later
    /// call refuses to start at or after the deadline, as a transient failure
    /// the wait reports like any other that outlasts its budget.
    // A reader deadline is a real transport boundary.
    #[allow(clippy::disallowed_methods)]
    fn next(&self) -> Result<Duration, Fail> {
        let first = !self.called.replace(true);
        let Some(deadline) = self.deadline else {
            return Ok(Duration::MAX);
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() && !first {
            return Err(Fail::new(
                Status::Error,
                ErrorKind::PeerUnavailable,
                "the wait's time ran out before the next peer read",
            ));
        }
        Ok(left)
    }
}

/// One peer request: the host, the action, and the time it may take.
type Call<'a> = dyn FnMut(HostId, PeerAction, Duration) -> Result<PeerResult, Fail> + 'a;

/// Current owner records, one batched status read per host and per 100 lanes.
fn read_records(
    selected: &[(HostId, AttemptId)],
    window: &Window,
    call: &mut Call<'_>,
) -> Result<Vec<run::AttemptRecord>, Fail> {
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
                    extra: Default::default(),
                    attempt: selected[*i].1,
                    host: Some(host),
                })
                .collect();
            let PeerResult::RunStatuses {
                data: page,
                extra: _,
            } = call(
                host,
                PeerAction::RunStatuses {
                    extra: Default::default(),
                    targets,
                },
                window.next()?,
            )?
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

/// A peer's newest lanes that `keep` accepts, then the tracked lanes they
/// dropped. The lookup of those draws on the same window as the first read.
fn read_newest(
    host: HostId,
    window: &Window,
    tracked: &mut watch::Tracked,
    keep: &dyn Fn(&run::AttemptRecord) -> bool,
    call: &mut Call<'_>,
) -> Result<Vec<run::AttemptRecord>, Fail> {
    let PeerResult::RunStatuses {
        data: records,
        extra: _,
    } = call(
        host,
        PeerAction::Lanes {
            extra: Default::default(),
            all: true,
            limit: PEER_LANES,
        },
        window.next()?,
    )?
    else {
        return Err(corrupt());
    };
    if records.len() > usize::from(PEER_LANES) {
        return Err(corrupt());
    }
    let page = records.into_iter().filter(|lane| keep(lane)).collect();
    tracked.complete(page, |missing| {
        let targets: Vec<_> = missing.iter().map(|attempt| (host, *attempt)).collect();
        read_records(&targets, window, call)
    })
}

/// Whole seconds one peer request may take: what is left of the wait, rounded
/// up to the one-second minimum a budget has, and never more than the peer
/// limit.
fn call_seconds(remaining: Duration, peer_seconds: u64) -> u64 {
    remaining
        .as_secs()
        .saturating_add(u64::from(remaining.subsec_nanos() > 0))
        .clamp(1, peer_seconds.max(1))
}

/// Ask each lane's host what its queued or starting lanes wait for.
fn fetch_waits(
    lanes: &[run::AttemptRecord],
    selected: &[(HostId, AttemptId)],
    mut call: impl FnMut(HostId, PeerAction) -> Result<PeerResult, Fail>,
) -> Vec<run::LaneWait> {
    let mut by_host: BTreeMap<HostId, Vec<AttemptId>> = BTreeMap::new();
    for lane in lanes.iter().filter(|lane| {
        matches!(
            lane.state,
            run::AttemptState::Queued { .. } | run::AttemptState::Starting { .. }
        )
    }) {
        if let Some((host, _)) = selected.iter().find(|(_, id)| *id == lane.attempt_id) {
            by_host.entry(*host).or_default().push(lane.attempt_id);
        }
    }
    let mut waits = Vec::new();
    for (host, attempts) in by_host {
        for chunk in attempts.chunks(100) {
            let targets = chunk
                .iter()
                .map(|attempt| AttemptTarget {
                    extra: Default::default(),
                    attempt: *attempt,
                    host: Some(host),
                })
                .collect();
            if let Ok(PeerResult::LaneWaits {
                data: found,
                extra: _,
            }) = call(
                host,
                PeerAction::LaneWaits {
                    extra: Default::default(),
                    targets,
                },
            ) {
                // Only a wait for a lane that was asked about is believed.
                waits.extend(
                    found
                        .into_iter()
                        .filter(|wait| chunk.contains(&wait.attempt_id)),
                );
            }
        }
    }
    waits
}

/// Poll `fetch`, given the time left, until the condition holds or `budget`
/// passes. No fetch starts after the deadline.
// Remote peer polling uses a hard real-time budget.
#[allow(clippy::disallowed_methods)]
fn poll_until(
    mut fetch: impl FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail>,
    condition: Condition,
    budget: Duration,
    poll: Duration,
) -> Result<(Vec<run::AttemptRecord>, bool), Fail> {
    let deadline = Instant::now() + budget;
    loop {
        let last = match watch::tick(&mut fetch, deadline)? {
            watch::Tick::Lanes(lanes) => {
                let met = condition_met(&lanes, condition);
                if met || Instant::now() >= deadline {
                    return Ok((lanes, met));
                }
                Ok(lanes)
            }
            watch::Tick::Retry(fail) => Err(fail),
        };
        watch::pause(poll, deadline);
        if Instant::now() >= deadline {
            return last.map(|lanes| (lanes, false));
        }
    }
}

/// Peer views cannot attribute this hosts cooldowns to the peer.
fn render_peer_lanes(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    next: Option<u64>,
    state: &std::path::Path,
) -> (Value, String) {
    render_lanes(lanes, waits, next, state, None)
}

/// Local lane rendering plus each row's owning host. Result paths are the
/// peer's; read them with `output`.
fn render(
    peer: &Peer,
    lanes: &[run::AttemptRecord],
    selected: &[(HostId, AttemptId)],
    edge: &Edge<'_>,
) -> (Value, String) {
    let waits = peer.waits(lanes, selected);
    render_with_waits(lanes, &waits, selected, edge.roots.state.as_path())
}
fn render_with_waits(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    selected: &[(HostId, AttemptId)],
    state: &std::path::Path,
) -> (Value, String) {
    let (mut data, human) = render_peer_lanes(lanes, waits, None, state);
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

/// Where a host named on the command line runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Site {
    Local,
    Peer(HostId),
}

/// Resolve `--host`: this machine, or a configured peer by label or host ID.
pub(super) fn site(edge: &Edge<'_>, host: &str) -> Result<Site, Fail> {
    if names_local_host(edge, host)? {
        return Ok(Site::Local);
    }
    let peer = Peer::open(edge)?;
    peer.project()?;
    peer.context
        .peer_host(host)
        .map(Site::Peer)
        .ok_or_else(|| unknown_host(host))
}

/// The host a two-part `HOST/ATTEMPT` qualifier names, when it is a host ID
/// of this machine or a configured peer rather than a run. A qualifier that
/// cannot be checked (no Fabric identity here) is a run.
pub(super) fn qualifier_site(edge: &Edge<'_>, qualifier: RunId) -> Option<Site> {
    let id = HostId::from_bytes(*qualifier.as_bytes());
    let peer = Peer::open(edge).ok()?;
    if peer.context.identity.host == id {
        Some(Site::Local)
    } else {
        peer.context
            .config
            .peers
            .values()
            .any(|configured| configured.host == id)
            .then_some(Site::Peer(id))
    }
}

pub(super) fn names_peer(edge: &Edge<'_>, host: &str) -> Result<bool, Fail> {
    Ok(Peer::open(edge)?.context.peer_host(host).is_some())
}

pub(super) fn unknown_host(host: &str) -> Fail {
    Fail::new(
        Status::Invalid,
        ErrorKind::UnknownHost,
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
            ErrorKind::WorktreeNotApplicable,
            "--worktree names a path on this host; a peer writer takes --branch and --base",
        ));
    };
    if provenant_confinement::repo::GitRefName::branch(branch).is_err()
        || provenant_confinement::repo::GitRevision::new(base).is_err()
    {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "invalid --branch or --base",
        ));
    }
    Ok(NewWorktree {
        extra: Default::default(),
        base: base.clone(),
        branch: branch.clone(),
    })
}

fn submitted_task(
    task: Task,
    new_worktree: Option<NewWorktree>,
    timeout_seconds: u64,
) -> SubmittedTask {
    SubmittedTask {
        context: task.context,
        extra: Default::default(),
        build_heavy: task.build_heavy,
        title: task.title,
        // A writer's base belongs to its new worktree request.
        base: if new_worktree.is_none() || task.head.is_some() {
            task.base
        } else {
            None
        },
        head: task.head,
        account: task.account,
        access: if new_worktree.is_some() {
            AccessMode::WorktreeWrite
        } else {
            AccessMode::ReadOnly
        },
        worktree: None,
        new_worktree,
        prompt: task.prompt,
        route: task.route,
        timeout_seconds,
        delivery_id: task.delivery_id,
        deliver: task.deliver,
    }
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
    let timeout_seconds = batch.provider_timeout;
    let access = if batch.new_worktree.is_some() {
        AccessMode::WorktreeWrite
    } else {
        AccessMode::ReadOnly
    };
    let tasks = batch
        .tasks
        .into_iter()
        .map(|task| submitted_task(task, batch.new_worktree.clone(), timeout_seconds))
        .collect();
    let concurrency = batch
        .concurrency
        .map(u16::try_from)
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "concurrency"))?;
    let input = SubmitInput::Submit {
        extra: Default::default(),
        operation_id: batch.operation_id.clone(),
        tasks,
        concurrency,
        knobs: batch.knobs,
        host: None,
    };
    let PeerResult::Submitted {
        data: receipt,
        extra: _,
    } = peer.call(
        host,
        PeerAction::Submit {
            extra: Default::default(),
            input,
        },
    )?
    else {
        return Err(corrupt());
    };
    if receipt.operation_id != batch.operation_id
        || receipt.targets.is_empty()
        || receipt.targets.iter().any(|t| t.host != Some(host))
        || receipt.worktree.is_some() != (access == AccessMode::WorktreeWrite)
        || receipt.worktree.as_ref().map(|w| &w.branch)
            != batch.new_worktree.as_ref().map(|w| &w.branch)
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
            extra: Default::default(),
            schema_version: 1,
            host,
            project,
            operation_id: batch.operation_id.clone(),
            run_id,
            attempts: lanes
                .iter()
                .map(|lane| Placed {
                    extra: Default::default(),
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
    // A waited report makes no peer read after the wait's deadline.
    let (mut data, human) = if met.is_some() {
        render_with_waits(&lanes, &[], &selected, edge.roots.state.as_path())
    } else {
        render(&peer, &lanes, &selected, edge)
    };
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
                provenant_fabric_runtime::hosts::plain_text(&worktree.branch, usize::MAX),
                provenant_fabric_runtime::hosts::plain_text(&worktree.path, usize::MAX)
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
    json: bool,
    emit: &mut dyn FnMut(&str) -> Result<(), Fail>,
) -> Result<Option<(Value, String, bool)>, Fail> {
    let placements = command
        .id
        .iter()
        .map(|target| super::targets::placement(edge, target, true))
        .collect::<Result<Vec<_>, _>>()?;
    let placed: Vec<&Placement> = placements.iter().flatten().collect();
    if placed.is_empty() {
        return Ok(None);
    }
    if placed.len() != placements.len() {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "list peer runs and local runs in separate lanes calls",
        ));
    }
    if command.cursor.is_some() {
        return Err(unsupported("--cursor"));
    }
    let mut selected = Vec::new();
    for (target, placement) in command.id.iter().zip(&placed) {
        for pair in placement.select_target(target)? {
            if !selected.contains(&pair) {
                selected.push(pair);
            }
        }
    }
    if command.session.is_some() {
        return Err(unsupported("--session"));
    }
    let peer = Peer::open_for(edge, placed)?;
    let since = command
        .since
        .map(|since| WallTime::from_micros(since.as_microsecond()))
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "invalid --since value"))?;
    if command.wait.is_some() || command.watch.is_some() {
        let scope = Scope {
            exact: command
                .id
                .iter()
                .filter_map(|target| target.attempt)
                .collect(),
            since,
            entered: watch::entered()?,
            explicit: true,
            snapshot: true,
        };
        let fetch = |remaining| peer.records_within(&selected, remaining);
        let first = selected
            .first()
            .map(|(host, _)| *host)
            .ok_or_else(corrupt)?;
        let host_of = |lane: &run::AttemptRecord| {
            selected
                .iter()
                .find(|(_, attempt)| *attempt == lane.attempt_id)
                .map_or(first, |(host, _)| *host)
        };
        return armed(command, scope, fetch, &host_of, edge, json, emit).map(Some);
    }
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
    let (data, human) = render(&peer, &lanes, &selected, edge);
    Ok(Some((data, human, false)))
}

/// A wait or watch over peer lanes. `host_of` names the host each runs on.
fn armed(
    command: &Lanes,
    scope: Scope,
    fetch: impl FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail>,
    host_of: &dyn Fn(&run::AttemptRecord) -> HostId,
    edge: &Edge<'_>,
    json: bool,
    emit: &mut dyn FnMut(&str) -> Result<(), Fail>,
) -> Result<(Value, String, bool), Fail> {
    let hosts = |lanes: &[run::AttemptRecord]| -> Vec<(HostId, AttemptId)> {
        lanes
            .iter()
            .map(|lane| (host_of(lane), lane.attempt_id))
            .collect()
    };
    if let Some(seconds) = command.watch {
        let watched = watch::watch(
            fetch,
            scope,
            Duration::from_secs(seconds),
            POLL,
            &mut |lane| {
                let host = Some(host_of(lane));
                let text = if json {
                    watch::json_line(lane, host)
                } else {
                    watch::line(lane, host)
                };
                emit(&format!("{text}\n"))
            },
        )?;
        let (mut data, _) = render_with_waits(
            &watched.lanes,
            &[],
            &hosts(&watched.lanes),
            edge.roots.state.as_path(),
        );
        data["condition_met"] = json!(watched.settled);
        let human = if json {
            String::new()
        } else {
            watch::end_line(&watched)
        };
        return Ok((data, human, !watched.settled));
    }
    let seconds = command.wait.ok_or_else(corrupt)?;
    let waited = watch::wait(
        fetch,
        scope,
        command.condition,
        Duration::from_secs(seconds),
        POLL,
    )?;
    let (mut data, human) = render_with_waits(
        &waited.lanes,
        &[],
        &hosts(&waited.lanes),
        edge.roots.state.as_path(),
    );
    data["condition_met"] = json!(waited.met);
    if waited.ignored > 0 {
        data["ignored_settled"] = json!(waited.ignored);
    }
    Ok((data, human, !waited.met))
}

/// `lanes --host`: the peer's own lanes, read over the peer channel. The
/// peer applies `--all` and `--limit`; state and time filters apply here to
/// the page it returns.
pub(super) fn host_lanes(
    command: &Lanes,
    host: &str,
    edge: &Edge<'_>,
    json: bool,
    emit: &mut dyn FnMut(&str) -> Result<(), Fail>,
) -> Result<(Value, String, bool), Fail> {
    let peer = Peer::open(edge)?;
    peer.project()?;
    let id = peer
        .context
        .peer_host(host)
        .ok_or_else(|| unknown_host(host))?;
    let limit = u16::try_from(command.limit)
        .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "invalid --limit"))?;
    // A session label names an agent on the peer, which stamps its own lanes.
    let owner = command
        .session
        .as_deref()
        .map(|label| {
            Address::new(id, label)
                .map(|address| address.actor)
                .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))
        })
        .transpose()?;
    let by_session =
        |lane: &run::AttemptRecord| owner.is_none_or(|owner| lane.verified_owner == Some(owner));
    let since = command
        .since
        .map(|since| WallTime::from_micros(since.as_microsecond()))
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "invalid --since value"))?;
    if command.wait.is_some() || command.watch.is_some() {
        // The peer's newest lanes show what is new; a lane seen running stays
        // tracked by ID after later dispatches push it off that page.
        let mut tracked = watch::Tracked::default();
        let fetch = |remaining| peer.newest(id, remaining, &mut tracked, &by_session);
        let scope = Scope {
            exact: BTreeSet::new(),
            since,
            entered: watch::entered()?,
            explicit: false,
            snapshot: true,
        };
        return armed(command, scope, fetch, &|_| id, edge, json, emit);
    }
    let PeerResult::RunStatuses {
        data: records,
        extra: _,
    } = peer.call(
        id,
        PeerAction::Lanes {
            extra: Default::default(),
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
    let lanes: Vec<_> = records
        .into_iter()
        .filter(&by_session)
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
        let (mut data, human) = render_peer_lanes(&[], &[], None, edge.roots.state.as_path());
        data["host"] = json!(id);
        (data, format!("host {id}\n{human}"))
    } else {
        render(&peer, &lanes, &selected, edge)
    };
    Ok((data, human, false))
}

pub(super) fn output(
    command: &Output,
    placement: &Placement,
    edge: &Edge<'_>,
) -> Result<(Value, String), Fail> {
    let target = &command.attempt.0;
    let [(host, attempt)] = placement.select_target(target)?[..] else {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "attempt required",
        ));
    };
    read_output(command, host, attempt, Peer::open_for(edge, [placement])?)
}

/// `output --host PEER` or `PEER/ATTEMPT`: the peer's own attempt, with no
/// local placement. The peer resolves the attempt and applies its own
/// confinement and access rules; any run or task in the target is not checked.
pub(super) fn resolve_prefix(
    edge: &Edge<'_>,
    host: HostId,
    target: &crate::Target,
) -> Result<crate::Target, Fail> {
    let selection = target.selection.as_deref().ok_or_else(corrupt)?;
    let peer = Peer::open(edge)?;
    peer.project()?;
    let PeerResult::RunStatuses {
        data: records,
        extra: _,
    } = peer.call(
        host,
        PeerAction::Lanes {
            extra: Default::default(),
            all: true,
            limit: PEER_LANES,
        },
    )?
    else {
        return Err(corrupt());
    };
    if records.len() >= usize::from(PEER_LANES) {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "peer history exceeds its enumeration limit; run `provenant lanes --host PEER --all` and use a full attempt ID",
        ));
    }
    let candidates = records
        .into_iter()
        .map(|record| super::targets::Candidate {
            target: crate::Target {
                selection: None,
                run: Some(record.run_id),
                task: Some(record.task_id),
                attempt: Some(record.attempt_id),
            },
            project: None,
            host: Some(host),
        })
        .collect();
    Ok(super::targets::choose(selection, candidates, false)?.target)
}

pub(super) fn output_on_peer(
    command: &Output,
    host: HostId,
    edge: &Edge<'_>,
) -> Result<(Value, String), Fail> {
    let attempt = command
        .attempt
        .0
        .attempt
        .ok_or_else(|| Fail::new(Status::Invalid, ErrorKind::Invalid, "attempt required"))?;
    let peer = Peer::open(edge)?;
    peer.project()?;
    read_output(command, host, attempt, peer)
}

fn read_output(
    command: &Output,
    host: HostId,
    attempt: AttemptId,
    peer: Peer,
) -> Result<(Value, String), Fail> {
    let part = match command.part {
        Part::Result => OutputPart::Result,
        Part::Stderr => OutputPart::Stderr,
        Part::Prompt => OutputPart::Prompt,
        Part::Events => OutputPart::Events,
        Part::Transcript => return Err(unsupported("--part transcript")),
        Part::Receipt => return Err(unsupported("--part receipt")),
    };
    if command.trace {
        return Err(unsupported("--trace"));
    }
    let input = OutputInput::Read {
        extra: Default::default(),
        target: AttemptTarget {
            extra: Default::default(),
            attempt,
            host: Some(host),
        },
        part,
        offset: command.offset,
        max_bytes: command.max_bytes,
    };
    let window = Window::new(Duration::from_secs(30));
    let PeerResult::Output {
        data: chunk,
        extra: _,
    } = peer.call_within(
        host,
        PeerAction::Output {
            extra: Default::default(),
            input,
        },
        window.next()?,
    )?
    else {
        return Err(corrupt());
    };
    let chunk = if let Some(lines) = command.tail {
        collect_tail(chunk, lines, command.max_bytes, |offset| {
            let input = OutputInput::Read {
                extra: Default::default(),
                target: AttemptTarget {
                    extra: Default::default(),
                    attempt,
                    host: Some(host),
                },
                part,
                offset,
                max_bytes: command.max_bytes,
            };
            match peer.call_within(
                host,
                PeerAction::Output {
                    extra: Default::default(),
                    input,
                },
                window.next()?,
            )? {
                PeerResult::Output {
                    data: chunk,
                    extra: _,
                } => Ok(chunk),
                _ => Err(corrupt()),
            }
        })?
    } else {
        chunk
    };
    chunk_response(attempt, host, &command.part, chunk)
}

/// Older peers expose paged reads. Retain only the bounded tail while paging,
/// and refuse a changing artifact rather than combining different digests.
fn collect_tail(
    mut chunk: OutputResult,
    lines: u64,
    max_bytes: u64,
    mut read: impl FnMut(u64) -> Result<OutputResult, Fail>,
) -> Result<OutputResult, Fail> {
    let mut retained = Vec::new();
    let mut retained_events = Vec::new();
    let mut expected_digest = None;
    for _ in 0..1024 {
        let OutputResult::Chunk {
            bytes,
            next_offset,
            eof,
            digest,
            offset,
            part,
            ..
        } = &chunk
        else {
            return Err(corrupt());
        };
        // Event offsets are sequence cursors; their content digest changes per page.
        if !matches!(part, OutputPart::Events) {
            if expected_digest.is_some_and(|previous| previous != *digest) {
                return Err(corrupt());
            }
            expected_digest = Some(*digest);
        }
        let raw = match bytes {
            OutputBytes::Utf8(text) => text.as_bytes().to_vec(),
            OutputBytes::Base64(text) => {
                provenant_fabric_runtime::handoff::decode_base64(text).ok_or_else(corrupt)?
            }
        };
        if matches!(part, OutputPart::Events) {
            let page: Vec<Value> = serde_json::from_slice(&raw).map_err(|_| corrupt())?;
            retained_events.extend(page);
            let excess = retained_events
                .len()
                .saturating_sub(lines.min(super::TAIL_MAX) as usize);
            retained_events.drain(..excess);
            retained = serde_json::to_vec(&retained_events).map_err(io_fail)?;
            while retained.len() as u64 > max_bytes && !retained_events.is_empty() {
                retained_events.remove(0);
                retained = serde_json::to_vec(&retained_events).map_err(io_fail)?;
            }
        } else {
            retained.extend_from_slice(&raw);
            let excess = retained.len().saturating_sub(max_bytes as usize);
            retained.drain(..excess);
        }
        if *eof {
            let start = super::tail_start(&retained, lines.min(super::TAIL_MAX));
            retained.drain(..start);
            while !retained.is_empty() && retained[0] & 0xc0 == 0x80 {
                retained.remove(0);
            }
            let mut result = chunk;
            if let OutputResult::Chunk {
                bytes,
                offset,
                next_offset,
                part,
                ..
            } = &mut result
            {
                if !matches!(part, OutputPart::Events) {
                    *offset = next_offset.saturating_sub(retained.len() as u64);
                }
                *bytes = OutputBytes::from_bytes(retained);
            }
            return Ok(result);
        }
        if next_offset <= offset {
            return Err(corrupt());
        }
        chunk = read(*next_offset)?;
    }
    Err(Fail::new(
        Status::Error,
        ErrorKind::Limit,
        "peer output exceeds the bounded tail scan",
    ))
}

fn chunk_response(
    attempt: AttemptId,
    host: HostId,
    part: &Part,
    chunk: OutputResult,
) -> Result<(Value, String), Fail> {
    let OutputResult::Chunk {
        state,
        reason,
        offset,
        next_offset,
        eof,
        digest,
        bytes,
        ..
    } = chunk
    else {
        return Err(corrupt());
    };
    let text = match &bytes {
        OutputBytes::Utf8(text) => text.clone(),
        OutputBytes::Base64(data) => data.clone(),
    };
    Ok((
        json!({
            "attempt_id": attempt,
            "host": host,
            "state": state,
            "reason": reason,
            "part": format!("{part:?}").to_lowercase(),
            "offset": offset,
            "next_offset": next_offset,
            "eof": eof,
            "digest": digest,
            "bytes": bytes,
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
            let selected = placement.select_target(target)?;
            if selected.is_empty() {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::NotFound,
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
                                ErrorKind::Invalid,
                                "operation ID is too long to derive one per attempt",
                            )
                        },
                    )?
                };
                let input = RunInput::Cancel {
                    extra: Default::default(),
                    target: AttemptTarget {
                        extra: Default::default(),
                        attempt: *attempt,
                        host: Some(*host),
                    },
                    operation_id,
                };
                let row = match peer.call(
                    *host,
                    PeerAction::Run {
                        extra: Default::default(),
                        input,
                    },
                ) {
                    Ok(PeerResult::Run {
                        data: result,
                        extra: _,
                    }) => match *result {
                        RunResult::Cancel {
                            attempt: record, ..
                        } if record.attempt_id == *attempt => {
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
                        json!({ "attempt_id": attempt, "host": host, "error": { "code": fail.kind.code(), "message": fail.message } })
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
                // No peer read after the wait's deadline.
                let (rendered, text) =
                    render_with_waits(&lanes, &[], &selected, edge.roots.state.as_path());
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

/// What a trace export learned from peers. A peer that cannot be read is
/// listed with its error, never fatal: the rest of the export still stands.
pub(super) struct PeerFacts {
    pub(super) local: HostId,
    pub(super) names: BTreeMap<HostId, String>,
    pub(super) facts: Vec<provenant_records::AttemptFact>,
    pub(super) hosts: Vec<Value>,
}

/// The terminal attempts of every configured peer, however the run got there,
/// read as `lanes --host` reads them: the peer's newest lanes over the peer
/// channel. `None` when this is not a federated project or no peer is
/// configured.
pub(super) fn peer_facts(edge: &Edge<'_>, run: Option<RunId>) -> Result<Option<PeerFacts>, Fail> {
    let peer = Peer::open(edge)?;
    // Another project's lanes are not this export's to read.
    if peer.project().is_err() || peer.context.config.peers.is_empty() {
        return Ok(None);
    }
    let names: BTreeMap<HostId, String> = peer
        .context
        .config
        .peers
        .iter()
        .map(|(name, p)| (p.host, name.clone()))
        .collect();
    let (facts, hosts) = collect(&names, run, |host, action| peer.call(host, action));
    Ok(Some(PeerFacts {
        local: peer.context.identity.host,
        names,
        facts,
        hosts,
    }))
}

/// The most lanes one peer returns; the peer rejects a larger page.
const PEER_LANES: u16 = 100;

/// Ask each host for its lanes and keep the terminal ones of `run`, or of any
/// run. A host that fails is listed with its error and the rest still count.
fn collect(
    names: &BTreeMap<HostId, String>,
    run: Option<RunId>,
    mut call: impl FnMut(HostId, PeerAction) -> Result<PeerResult, Fail>,
) -> (Vec<provenant_records::AttemptFact>, Vec<Value>) {
    let (mut facts, mut hosts) = (Vec::new(), Vec::new());
    for (host, name) in names {
        let mut row = json!({ "host": host, "name": name, "attempts": 0 });
        let action = PeerAction::Lanes {
            extra: Default::default(),
            all: true,
            limit: PEER_LANES,
        };
        match call(*host, action) {
            Ok(PeerResult::RunStatuses {
                data: records,
                extra: _,
            }) if records.len() <= usize::from(PEER_LANES) => {
                let before = facts.len();
                facts.extend(
                    records
                        .into_iter()
                        .filter(|r| run.is_none_or(|run| r.run_id == run))
                        .filter_map(|r| run::fact(r).ok()),
                );
                row["attempts"] = json!(facts.len() - before);
            }
            Ok(_) => row["error"] = json!(corrupt().message),
            Err(fail) => row["error"] = json!(fail.message),
        }
        hosts.push(row);
    }
    (facts, hosts)
}

/// Full identities from peer receipts, used to check prefix uniqueness against
/// local identities before routing a command.
pub(super) fn target_candidates(roots: &Roots) -> Result<Vec<super::targets::Candidate>, Fail> {
    if !roots.state.as_path().join(PLACEMENTS).exists() {
        return Ok(Vec::new());
    }
    let directory = directory(roots)?;
    let mut candidates = Vec::new();
    for name in directory.entries().map_err(io_fail)? {
        let name = name.map_err(io_fail)?;
        let Some(run) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|id| RunId::parse(id).ok())
        else {
            continue;
        };
        let placement = read(&directory, &name.to_string_lossy(), run)?;
        candidates.push(super::targets::Candidate {
            target: crate::Target {
                selection: None,
                run: Some(run),
                task: None,
                attempt: None,
            },
            project: None,
            host: Some(placement.host),
        });
        for attempt in placement.attempts {
            candidates.push(super::targets::Candidate {
                target: crate::Target {
                    selection: None,
                    run: Some(run),
                    task: Some(attempt.task_id),
                    attempt: Some(attempt.attempt_id),
                },
                project: None,
                host: Some(placement.host),
            });
        }
    }
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::RootInputs;

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
    #[test]
    fn peer_tail_pages_to_eof_and_rejects_a_changed_artifact() -> Result {
        let target = AttemptTarget {
            extra: Default::default(),
            attempt: AttemptId::from_bytes([1; 16]),
            host: None,
        };
        let chunk = |text: &str, offset, eof, byte| OutputResult::Chunk {
            extra: Default::default(),
            target: target.clone(),
            part: OutputPart::Result,
            state: "succeeded".into(),
            reason: None,
            offset,
            next_offset: offset + text.len() as u64,
            eof,
            digest: provenant_core::Digest::from_bytes([byte; 32]),
            bytes: OutputBytes::Utf8(text.into()),
        };
        let first = chunk("before\n", 0, false, 1);
        let last = collect_tail(first.clone(), 1, 64, |offset| {
            assert_eq!(offset, 7);
            Ok(chunk("verdict\n", offset, true, 1))
        })
        .map_err(|fail| fail.message)?;
        let (_, text) = chunk_response(
            target.attempt,
            HostId::from_bytes([2; 16]),
            &Part::Result,
            last,
        )
        .map_err(|fail| fail.message)?;
        assert_eq!(text, "verdict\n");
        assert!(
            collect_tail(first, 1, 64, |offset| Ok(chunk(
                "changed\n",
                offset,
                true,
                2
            )))
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn peer_event_tail_preserves_whole_records_across_pages() -> Result {
        let target = AttemptTarget {
            extra: Default::default(),
            attempt: AttemptId::from_bytes([1; 16]),
            host: None,
        };
        let chunk = |sequence, eof| OutputResult::Chunk {
            extra: Default::default(),
            target: target.clone(),
            part: OutputPart::Events,
            state: "succeeded".into(),
            reason: None,
            offset: sequence - 1,
            next_offset: sequence,
            eof,
            digest: provenant_core::Digest::from_bytes([sequence as u8; 32]),
            bytes: OutputBytes::Utf8(json!([{"sequence":sequence}]).to_string()),
        };
        let last = collect_tail(chunk(1, false), 1, 1024, |offset| {
            assert_eq!(offset, 1);
            Ok(chunk(2, true))
        })
        .map_err(|fail| fail.message)?;
        let (_, text) = chunk_response(
            target.attempt,
            HostId::from_bytes([2; 16]),
            &Part::Events,
            last,
        )
        .map_err(|fail| fail.message)?;
        assert_eq!(
            serde_json::from_str::<Value>(&text)?,
            json!([{"sequence":2}])
        );
        Ok(())
    }

    fn lane(seed: u8, state: &str) -> Result<run::AttemptRecord> {
        Ok(serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([seed; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([seed; 16]),
            "operation_id": "peer-wait",
            "attempt_number": 1,
            "revision": 1,
            "generation": 1,
            "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
            "applied_route": null,
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": WallTime::from_micros(100)?,
            "state": {"state": state},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": provenant_records::CanonicalV1::file_digest(b""),
                "bytes": 1,
                "complete": true,
                "redacted": false
            }
        }))?)
    }

    #[test]
    fn peer_lanes_do_not_show_local_capacity_cooldowns() -> Result {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let route = run::RouteRecord {
            extra: Default::default(),
            provider: Some("codex".into()),
            model: Some("codex/model".into()),
            effort: None,
            account_label: Some("work".into()),
            account_key: Some("key".into()),
            source: None,
            pool_selection: None,
        };
        let now = SystemClock::new().wall_now()?.as_timestamp().as_second();
        crate::quota::persist_capacity_cooldown(&state, &route, now, 600)?;
        let (data, human) = render_with_waits(&[], &[], &[], &state);
        assert_eq!(data["capacity_cooldowns"], json!([]));
        assert!(!human.contains("capacity cooldown"));
        Ok(())
    }

    #[test]
    fn a_peers_unstarted_lanes_show_their_class_and_wait_like_local_ones() -> Result {
        let host = HostId::from_bytes([9; 16]);
        let (queued, starting) = (lane(1, "queued")?, lane(2, "starting")?);
        let lanes = [queued.clone(), starting.clone()];
        let selected: Vec<_> = lanes.iter().map(|l| (host, l.attempt_id)).collect();
        let wait = |lane: &run::AttemptRecord, text: &str| run::LaneWait {
            extra: Default::default(),
            attempt_id: lane.attempt_id,
            lane_class: run::LaneClass::ReadOnly,
            waiting_for: text.into(),
        };
        let mut asked = Vec::new();
        let waits = fetch_waits(&lanes, &selected, |host, action| {
            let PeerAction::LaneWaits { targets, .. } = action else {
                return Err(corrupt());
            };
            asked.push((host, targets.iter().map(|t| t.attempt).collect::<Vec<_>>()));
            Ok(PeerResult::LaneWaits {
                data: vec![
                    wait(&queued, "launch: not yet handed to a guardian"),
                    wait(&starting, "read-only startup spacing: 1.2 s left of 2.0 s"),
                    // Not asked about, so not believed.
                    run::LaneWait {
                        extra: Default::default(),
                        attempt_id: AttemptId::from_bytes([77; 16]),
                        lane_class: run::LaneClass::Writer,
                        waiting_for: "stray".into(),
                    },
                ],
                extra: Default::default(),
            })
        });
        assert_eq!(
            asked,
            vec![(host, vec![queued.attempt_id, starting.attempt_id])]
        );
        assert_eq!(waits.len(), 2);
        let (data, human) =
            render_with_waits(&lanes, &waits, &selected, std::path::Path::new("/state"));
        assert_eq!(data["lanes"][0]["lane_class"], "read_only");
        assert_eq!(
            data["lanes"][0]["waiting_for"],
            "launch: not yet handed to a guardian"
        );
        assert_eq!(
            data["lanes"][1]["waiting_for"],
            "read-only startup spacing: 1.2 s left of 2.0 s"
        );
        assert_eq!(data["lanes"][1]["host"], json!(host));
        assert!(human.contains("(read-only startup spacing: 1.2 s left of 2.0 s)"));
        Ok(())
    }

    #[test]
    fn a_peer_that_cannot_say_leaves_a_lane_without_a_wait_and_running_lanes_are_not_asked()
    -> Result {
        let host = HostId::from_bytes([9; 16]);
        let (running, starting) = (lane(3, "running")?, lane(4, "starting")?);
        let lanes = [running.clone(), starting.clone()];
        let selected: Vec<_> = lanes.iter().map(|l| (host, l.attempt_id)).collect();
        let mut asked = Vec::new();
        // An older peer refuses the action it does not know.
        let waits = fetch_waits(&lanes, &selected, |_, action| {
            if let PeerAction::LaneWaits { targets, .. } = action {
                asked.extend(targets.iter().map(|t| t.attempt));
            }
            Err(corrupt())
        });
        assert!(waits.is_empty());
        assert_eq!(asked, vec![starting.attempt_id]);
        let (data, _) =
            render_with_waits(&lanes, &waits, &selected, std::path::Path::new("/state"));
        assert_eq!(data["lanes"][1]["lane_class"], Value::Null);
        assert_eq!(data["lanes"][1]["waiting_for"], Value::Null);
        Ok(())
    }

    #[test]
    fn peer_writer_moves_its_base_into_the_new_worktree() -> Result {
        let route = run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "fixture".into(),
            model: "fixture/model".into(),
            effort: None,
        };
        let task = submitted_task(
            Task {
                context: None,
                build_heavy: true,
                title: Some("Make the change".into()),
                base: Some("main".into()),
                head: None,
                prompt: "write the change".into(),
                route: route.clone(),
                account: None,
                delivery_id: None,
                deliver: None,
            },
            Some(NewWorktree {
                extra: Default::default(),
                base: "main".into(),
                branch: "writer/change".into(),
            }),
            600,
        );
        let actual = serde_json::to_value(SubmitInput::Submit {
            extra: Default::default(),
            operation_id: OperationId::parse("peer-writer")?,
            tasks: vec![task],
            concurrency: Some(1),
            knobs: vec![],
            host: None,
        })?;
        assert_eq!(actual["op"], "submit");
        // The whole task, so a stray field or a rewritten prompt fails.
        assert_eq!(
            actual["tasks"][0],
            json!({
                "access": "worktree_write",
                "build_heavy": true,
                "account": null,
                "deliver": null,
                "delivery_id": null,
                "new_worktree": {"base": "main", "branch": "writer/change"},
                "prompt": "write the change",
                "title": "Make the change",
                "route": serde_json::to_value(route)?,
                "timeout_seconds": 600,
                "worktree": null
            })
        );
        Ok(())
    }

    #[test]
    fn read_only_peer_forwards_the_context_ref_and_original_prompt() {
        let task = submitted_task(
            Task {
                context: Some(provenant_records::run::WorkContext {
                    graph_id: Some("graph".into()),
                    node_id: Some("inspect".into()),
                    pattern: Some("council".into()),
                    ..Default::default()
                }),
                build_heavy: false,
                title: None,
                base: Some("main".into()),
                head: None,
                prompt: "review".into(),
                route: run::RequestedRoute::Named {
                    extra: Default::default(),
                    name: "utility".into(),
                    effort: None,
                },
                account: None,
                delivery_id: None,
                deliver: None,
            },
            None,
            600,
        );
        assert_eq!(task.base.as_deref(), Some("main"));
        assert_eq!(task.prompt, "review");
        assert_eq!(
            task.context
                .as_ref()
                .and_then(|context| context.pattern.as_deref()),
            Some("council")
        );
        assert_eq!(task.access, AccessMode::ReadOnly);
    }

    #[test]
    fn remote_output_carries_the_attempt_state_and_reason() -> Result {
        let attempt = AttemptId::from_bytes([3; 16]);
        let host = HostId::from_bytes([7; 16]);
        let chunk = OutputResult::Chunk {
            extra: Default::default(),
            target: AttemptTarget {
                extra: Default::default(),
                attempt,
                host: Some(host),
            },
            part: OutputPart::Result,
            state: "uncommitted".into(),
            reason: Some("uncommitted paths: src/a.rs".into()),
            offset: 0,
            next_offset: 2,
            eof: true,
            digest: provenant_core::Digest::from_bytes([0; 32]),
            bytes: OutputBytes::Utf8("ok".into()),
        };
        let (data, text) =
            chunk_response(attempt, host, &Part::Result, chunk).map_err(|f| f.message)?;
        assert_eq!(data["state"], "uncommitted");
        assert_eq!(data["reason"], "uncommitted paths: src/a.rs");
        assert_eq!(text, "ok");
        Ok(())
    }

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
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        Ok((Scratch(base), roots))
    }
    fn placement(operation: &str) -> Result<Placement> {
        Ok(Placement {
            extra: Default::default(),
            schema_version: 1,
            host: HostId::from_bytes([7; 16]),
            project: serde_json::from_value(json!("project-key"))?,
            operation_id: OperationId::parse(operation)?,
            run_id: RunId::from_bytes([1; 16]),
            attempts: vec![
                Placed {
                    extra: Default::default(),
                    task_id: TaskId::from_bytes([2; 16]),
                    attempt_id: AttemptId::from_bytes([3; 16]),
                },
                Placed {
                    extra: Default::default(),
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
        assert_eq!(other.kind.code(), "CONFLICT");
        Ok(())
    }

    #[test]
    fn placement_refresh_preserves_unknown_envelope_and_attempt_fields() -> Result {
        let (_scratch, roots) = roots()?;
        let original = placement("place")?;
        let mut newer = serde_json::to_value(&original)?;
        newer["future"] = json!({"nested": [null, true, u64::MAX]});
        newer["attempts"][0]["future_attempt"] = json!(["keep"]);
        let decoded: Placement = serde_json::from_value(newer.clone())?;
        save(&roots, &decoded).map_err(|f| f.message)?;
        save(&roots, &original).map_err(|f| f.message)?;
        let refreshed = find(&roots, original.run_id)
            .map_err(|f| f.message)?
            .ok_or("missing placement")?;
        assert_eq!(serde_json::to_value(refreshed)?, newer);
        Ok(())
    }

    #[test]
    fn bare_ids_resolve_peer_attempts_and_runs() -> Result {
        let (_scratch, roots) = roots()?;
        let placed = placement("place")?;
        save(&roots, &placed).map_err(|f| f.message)?;
        let attempt: crate::Target = placed.attempts[0].attempt_id.to_string().parse()?;
        let found = find_target(&roots, &attempt, false)
            .map_err(|f| f.message)?
            .ok_or("missing attempt")?;
        assert_eq!(
            found.select_target(&attempt).map_err(|f| f.message)?,
            vec![(placed.host, placed.attempts[0].attempt_id)]
        );
        let run: crate::Target = placed.run_id.to_string().parse()?;
        assert_eq!(
            find_target(&roots, &run, false).map_err(|f| f.message)?,
            None
        );
        let found = find_target(&roots, &run, true)
            .map_err(|f| f.message)?
            .ok_or("missing run")?;
        assert_eq!(found.select_target(&run).map_err(|f| f.message)?.len(), 2);
        let mut colliding = placement("colliding")?;
        colliding.run_id = RunId::from_bytes(*placed.attempts[0].attempt_id.as_bytes());
        colliding.attempts[0].attempt_id = AttemptId::from_bytes([8; 16]);
        colliding.attempts[1].attempt_id = AttemptId::from_bytes([9; 16]);
        save(&roots, &colliding).map_err(|f| f.message)?;
        let failure = find_target(&roots, &attempt, true)
            .err()
            .ok_or("expected ambiguity")?;
        assert_eq!(failure.kind, ErrorKind::Conflict);
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
        assert_eq!(missing.kind.code(), "NOT_FOUND");
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
        assert_eq!(fail.kind.code(), "LAUNCH_UNKNOWN");
        assert!(fail.message.contains("--operation-id place"));
        assert_eq!(
            peer_failure_fail(FacadeError::PeerVersion.into())
                .kind
                .code(),
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
        assert_eq!(fail.kind.code(), "PEER_UNAVAILABLE");
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
            |_| {
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
            |_| {
                calls += 1;
                Err(unavailable(&format!("workshop: failure {calls}")))
            },
            Condition::All,
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        let fail = out.expect_err("budget exhausted");
        assert_eq!(fail.kind.code(), "PEER_UNAVAILABLE");
        assert!(calls > 1);
        assert!(fail.message.contains(&format!("failure {calls}")));
    }

    #[test]
    fn an_armed_peer_wait_reports_the_last_failed_read() -> Result {
        let first = running(1);
        let mut calls = 0;
        let out = watch::wait(
            |_| {
                calls += 1;
                if calls == 1 {
                    Ok(vec![first.clone()])
                } else {
                    Err(unavailable("workshop: ssh timed out"))
                }
            },
            Scope {
                exact: BTreeSet::new(),
                since: None,
                entered: WallTime::from_micros(1)?,
                explicit: false,
                snapshot: false,
            },
            Condition::All,
            Duration::from_millis(30),
            Duration::from_millis(1),
        );
        let fail = out.err().expect("the final peer read failed");
        assert_eq!(fail.kind.code(), "PEER_UNAVAILABLE");
        assert!(fail.message.contains("ssh timed out"));
        Ok(())
    }

    #[test]
    fn an_armed_peer_watch_reports_the_last_failed_read() -> Result {
        let first = running(1);
        let mut calls = 0;
        let out = watch::watch(
            |_| {
                calls += 1;
                if calls == 1 {
                    Ok(vec![first.clone()])
                } else {
                    Err(unavailable("workshop: ssh timed out"))
                }
            },
            Scope {
                exact: BTreeSet::new(),
                since: None,
                entered: WallTime::from_micros(1)?,
                explicit: false,
                snapshot: false,
            },
            Duration::from_millis(30),
            Duration::from_millis(1),
            &mut |_| Ok(()),
        );
        let fail = out.err().expect("the final peer read failed");
        assert_eq!(fail.kind.code(), "PEER_UNAVAILABLE");
        assert!(fail.message.contains("ssh timed out"));
        Ok(())
    }

    #[test]
    fn a_peer_call_is_capped_by_the_time_left_and_the_peer_limit() {
        let secs = Duration::from_secs;
        assert_eq!(call_seconds(secs(1), 30), 1);
        assert_eq!(call_seconds(Duration::from_millis(1500), 30), 2);
        assert_eq!(call_seconds(secs(25), 30), 25);
        assert_eq!(call_seconds(secs(3600), 30), 30);
        assert_eq!(call_seconds(Duration::MAX, 30), 30);
        // An exhausted budget still asks for the one-second minimum.
        assert_eq!(call_seconds(Duration::ZERO, 30), 1);
    }

    #[test]
    // The deadline under test is a real-time reader boundary.
    #[allow(clippy::disallowed_methods)]
    fn wait_starts_no_fetch_after_its_deadline() {
        let mut starts = Vec::new();
        let begun = Instant::now();
        let out = poll_until(
            |left| {
                starts.push((Instant::now(), left));
                Err(unavailable("workshop: ssh timed out"))
            },
            Condition::All,
            Duration::from_millis(40),
            Duration::from_secs(30),
        );
        assert_eq!(
            out.err().map(|fail| fail.kind.code()),
            Some("PEER_UNAVAILABLE")
        );
        assert_eq!(starts.len(), 1);
        assert!(starts[0].1 <= Duration::from_millis(40), "{starts:?}");
        assert!(starts[0].0 < begun + Duration::from_millis(40));
    }

    fn running(id: u8) -> run::AttemptRecord {
        serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([id; 16]),
            "run_id": RunId::from_bytes([9; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "remote-fixture",
            "attempt_number": u64::from(id),
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": WallTime::from_micros(1).expect("time"),
            "state": {"state":"running"},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": provenant_records::CanonicalV1::file_digest(b""),
                "bytes": 0,
                "complete": true,
                "redacted": false
            }
        }))
        .expect("attempt record")
    }
    fn attempt(id: u8) -> AttemptId {
        AttemptId::from_bytes([id; 16])
    }
    /// What a peer answers: a status record for each target, and an empty
    /// newest page. Each call first takes `delay`, as a slow transport would.
    #[allow(clippy::disallowed_methods)]
    fn answer(action: PeerAction, delay: Duration) -> std::result::Result<PeerResult, Fail> {
        std::thread::sleep(delay);
        Ok(PeerResult::RunStatuses {
            data: match action {
                PeerAction::RunStatuses { targets, .. } => targets
                    .iter()
                    .map(|target| running(target.attempt.as_bytes()[0]))
                    .collect(),
                _ => Vec::new(),
            },
            extra: Default::default(),
        })
    }

    #[test]
    fn every_read_of_a_fetch_gets_only_the_time_the_earlier_ones_left() {
        // One host with 101 lanes reads in two chunks, then a second host.
        let mut selected: Vec<_> = (1..=101)
            .map(|id| (HostId::from_bytes([1; 16]), attempt(id)))
            .collect();
        selected.push((HostId::from_bytes([2; 16]), attempt(102)));
        let mut left = Vec::new();
        let window = Window::new(Duration::from_millis(900));
        let records = read_records(&selected, &window, &mut |_, action, budget| {
            left.push(budget);
            answer(action, Duration::from_millis(250))
        })
        .expect("three reads fit the budget");
        assert_eq!(records.len(), 102);
        assert_eq!(left.len(), 3);
        // Each read is bounded by what the earlier ones left, not by 900 ms.
        assert!(left[1] <= Duration::from_millis(700), "{left:?}");
        assert!(left[2] <= Duration::from_millis(450), "{left:?}");
        assert!(left[2] < left[1] && left[1] < left[0], "{left:?}");
    }

    #[test]
    fn no_read_starts_at_or_after_the_deadline() {
        let selected = [
            (HostId::from_bytes([1; 16]), attempt(1)),
            (HostId::from_bytes([2; 16]), attempt(2)),
            (HostId::from_bytes([3; 16]), attempt(3)),
        ];
        let mut starts = 0;
        let window = Window::new(Duration::from_millis(100));
        let failure = read_records(&selected, &window, &mut |_, action, _| {
            starts += 1;
            answer(action, Duration::from_millis(150))
        })
        .expect_err("the first read used the whole budget");
        assert_eq!(failure.kind.code(), "PEER_UNAVAILABLE");
        assert_eq!(starts, 1, "later hosts are never asked");
    }

    #[test]
    fn a_zero_second_budget_still_makes_its_one_read() {
        let selected = [(HostId::from_bytes([1; 16]), attempt(1))];
        let mut starts = 0;
        let window = Window::new(Duration::ZERO);
        let records = read_records(&selected, &window, &mut |_, action, _| {
            starts += 1;
            answer(action, Duration::ZERO)
        })
        .expect("the initial read runs");
        assert_eq!((records.len(), starts), (1, 1));
    }

    /// A tracker that has seen lane 1 running, so a page without it needs a
    /// lookup.
    fn tracking_one() -> watch::Tracked {
        let mut tracked = watch::Tracked::default();
        tracked
            .complete(vec![running(1)], |_| Err(corrupt()))
            .expect("a page that holds the lane needs no lookup");
        tracked
    }

    #[test]
    fn the_missing_lane_lookup_shares_the_first_reads_deadline() {
        let host = HostId::from_bytes([1; 16]);
        let keep = |_: &run::AttemptRecord| true;
        // The first read leaves time for the lookup, but only what it left.
        let mut left = Vec::new();
        let window = Window::new(Duration::from_millis(800));
        let lanes = read_newest(
            host,
            &window,
            &mut tracking_one(),
            &keep,
            &mut |_, action, budget| {
                left.push(budget);
                answer(action, Duration::from_millis(300))
            },
        )
        .expect("both reads fit");
        assert_eq!((lanes.len(), left.len()), (1, 2));
        assert!(left[1] <= Duration::from_millis(500), "{left:?}");

        // The first read used everything: the lookup must not start, even
        // though a zero remainder would round up to a one-second budget.
        let mut starts = 0;
        let window = Window::new(Duration::from_millis(100));
        let failure = read_newest(
            host,
            &window,
            &mut tracking_one(),
            &keep,
            &mut |_, action, _| {
                starts += 1;
                answer(action, Duration::from_millis(150))
            },
        )
        .expect_err("the budget ran out before the lookup");
        assert_eq!(failure.kind.code(), "PEER_UNAVAILABLE");
        assert_eq!(starts, 1);
    }

    #[test]
    fn wait_does_not_retry_a_non_transient_failure() {
        let mut calls = 0;
        let out = poll_until(
            |_| {
                calls += 1;
                Err(corrupt())
            },
            Condition::All,
            Duration::from_secs(30),
            Duration::from_millis(1),
        );
        assert_eq!(out.err().map(|f| f.kind.code()), Some("STATE_CORRUPT"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn peer_export_asks_every_configured_peer_for_its_lanes() {
        let names = BTreeMap::from([
            (HostId::from_bytes([1; 16]), "alpha".to_owned()),
            (HostId::from_bytes([2; 16]), "beta".to_owned()),
            (HostId::from_bytes([3; 16]), "gamma".to_owned()),
        ]);
        let mut asked = Vec::new();
        let (facts, hosts) = collect(&names, None, |host, action| {
            let PeerAction::Lanes { all, limit, .. } = action else {
                panic!("expected a lanes request");
            };
            asked.push((host, all, limit));
            match names[&host].as_str() {
                "alpha" => Ok(PeerResult::RunStatuses {
                    data: Vec::new(),
                    extra: Default::default(),
                }),
                "beta" => Err(unavailable("beta: ssh timed out")),
                _ => Ok(PeerResult::Error {
                    extra: Default::default(),
                    data: provenant_fabric_runtime::hosts::PeerResultError {
                        extra: Default::default(),
                        error:
                            provenant_fabric_runtime::coordination::facade::FacadeError::Conflict,
                    },
                }),
            }
        });
        let expected: Vec<_> = names.keys().map(|h| (*h, true, PEER_LANES)).collect();
        assert_eq!(asked, expected);
        assert!(facts.is_empty());
        assert_eq!(hosts[0]["name"], "alpha");
        assert_eq!(hosts[0]["attempts"], 0);
        assert!(hosts[0].get("error").is_none());
        assert!(
            hosts[1]["error"]
                .as_str()
                .is_some_and(|e| e.contains("beta"))
        );
        assert_eq!(hosts[2]["error"], corrupt().message);
    }
}
