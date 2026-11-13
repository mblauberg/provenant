//! Read-only peer placement. `dispatch --host` forwards one batch to the
//! peer's fixed `fabric peer` entrypoint; `lanes`, `output` and `cancel`
//! forward by ID. Routing, confinement and execution stay on the peer. This
//! host keeps only a placement record under `fabric/remote-runs/`, never a
//! copy of the peer's run state.
use super::watch::{self, Scope};
use super::{Condition, Edge, Fail, Reply, Status, condition_met, reply, state_and_reason};
use crate::{
    Cancel, Lanes, Output, Part,
    fabric::{Context, PeerFailure},
};
use provenant_core::{
    AttemptId, CancelToken, Clock, HostId, OperationId, Roots, RunId, WaitBudget, WallTime,
};
use provenant_fabric_runtime::{
    coordination::facade::{
        AttemptTarget, FacadeError, OutputBytes, OutputInput, OutputPart, OutputResult, RunInput,
    },
    hosts::{PeerAction, PeerResult},
    identity::{Address, CrossHostProjectKey},
    runs::RunResult,
};
use provenant_os::{clock::SystemClock, fs::Directory};
use provenant_records::WorkKey;
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

/// Where a graph lives. The peer owns everything else about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Placement {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    schema_version: u64,
    host: HostId,
    project: CrossHostProjectKey,
    operation_id: OperationId,
    graph: String,
    attempts: Vec<Placed>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Placed {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    key: WorkKey,
    attempt_id: AttemptId,
}
impl Placement {
    fn select_target(&self, target: &crate::Target) -> Result<Vec<(HostId, AttemptId)>, Fail> {
        let selection = target.selection.as_deref().ok_or_else(corrupt)?;
        let target = crate::target::Target::parse(selection).map_err(|_| corrupt())?;
        self.select(
            &target.name,
            target.node.as_deref(),
            target.attempt.map(std::num::NonZeroU32::get),
        )
    }
    /// Attempts the target names, in dispatch order.
    fn select(
        &self,
        graph: &str,
        node: Option<&str>,
        attempt: Option<u32>,
    ) -> Result<Vec<(HostId, AttemptId)>, Fail> {
        let selected: Vec<_> = self
            .attempts
            .iter()
            .filter(|p| p.key.graph() == graph)
            .filter(|p| node.is_none_or(|node| node == p.key.node()))
            .filter(|p| attempt.is_none_or(|number| number == p.key.attempt()))
            .map(|p| (self.host, p.attempt_id))
            .collect();
        if (node.is_some() || attempt.is_some()) && selected.is_empty() {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::NotFound,
                "attempt is not in that graph/node",
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
    let projection = error.diagnostic();
    Fail {
        detail: Some(Box::new(error)),
        projection: Some(Box::new(projection)),
        ..Fail::new(status, kind, format!("peer: {message}"))
    }
}

fn directory(roots: &Roots) -> Result<Directory, Fail> {
    let root = roots.state.as_path();
    crate::fabric::private_dir(root, &root.join(PLACEMENTS)).map_err(io_fail)
}
fn read(
    directory: &Directory,
    name: &str,
    project: &CrossHostProjectKey,
    graph: &str,
) -> Result<Placement, Fail> {
    let bytes = directory
        .read_bounded(OsStr::new(name), PLACEMENT_LIMIT)
        .map_err(io_fail)?;
    let placement = decode_placement(&bytes)?;
    if placement.project != *project || placement.graph != graph {
        return Err(corrupt());
    }
    Ok(placement)
}
fn decode_placement(bytes: &[u8]) -> Result<Placement, Fail> {
    let placement: Placement = serde_json::from_slice(bytes).map_err(|_| corrupt())?;
    if placement.schema_version != 2
        || placement.attempts.is_empty()
        || placement
            .attempts
            .iter()
            .any(|placed| placed.key.graph() != placement.graph)
        || placement
            .attempts
            .iter()
            .any(|placed| placed.key.project() != placement.attempts[0].key.project())
    {
        return Err(corrupt());
    }
    Ok(placement)
}
fn placement_name(project: &CrossHostProjectKey, graph: &str) -> String {
    format!("{}-{graph}.json", project.as_str())
}
/// Record a placement. Replaying the operation refreshes its attempts; any
/// other existing record for the run is a conflict.
#[cfg(test)]
fn save(roots: &Roots, placement: &Placement) -> Result<(), Fail> {
    let directory = directory(roots)?;
    let name = placement_name(&placement.project, &placement.graph);
    let mut placement = placement.clone();
    if directory.metadata_at(OsStr::new(&name), false).is_ok() {
        let existing = read(&directory, &name, &placement.project, &placement.graph)?;
        if (
            existing.host,
            &existing.project,
            &existing.graph,
            &existing.operation_id,
        ) != (
            placement.host,
            &placement.project,
            &placement.graph,
            &placement.operation_id,
        ) {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Conflict,
                "another placement already names this graph",
            ));
        }
        for (key, value) in existing.extra {
            placement.extra.entry(key).or_insert(value);
        }
        for placed in &mut placement.attempts {
            if let Some(previous) = existing.attempts.iter().find(|previous| {
                previous.key == placed.key && previous.attempt_id == placed.attempt_id
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
            provenant_os::fs::PublishMode::Replace,
        )
        .map_err(io_fail)?
        .require_durable()
        .map_err(io_fail)
}
/// The placement of a graph dispatched to a peer from this host, if any.
pub(super) fn find(
    roots: &Roots,
    project: &CrossHostProjectKey,
    graph: &str,
) -> Result<Option<Placement>, Fail> {
    let name = placement_name(project, graph);
    let path = roots.state.as_path().join(PLACEMENTS).join(&name);
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    read(&directory(roots)?, &name, project, graph).map(Some)
}
/// Graph targets are resolved from the host's durable peer placements.
pub(super) fn find_target(
    edge: &Edge<'_>,
    target: &crate::Target,
    allow_run: bool,
) -> Result<Option<Placement>, Fail> {
    if !edge.roots.state.as_path().join(PLACEMENTS).exists() || target.selection.is_none() {
        return Ok(None);
    }
    let peer = Peer::open(edge)?;
    let Some(project) = peer
        .context
        .identity
        .federation
        .as_ref()
        .map(|project| project.key.clone())
    else {
        return Ok(None);
    };
    find_target_in_project(edge.roots, &project, target, allow_run)
}

fn find_target_in_project(
    roots: &Roots,
    project: &CrossHostProjectKey,
    target: &crate::Target,
    allow_run: bool,
) -> Result<Option<Placement>, Fail> {
    let Some(selection) = target.selection.as_deref() else {
        return Ok(None);
    };
    let Ok(target) = crate::target::Target::parse(selection) else {
        return Ok(None);
    };
    if target.node.is_none() && target.attempt.is_none() && !allow_run {
        return Ok(None);
    }
    find(roots, project, &target.name)
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
        let window = Window::new(remaining);
        let mut call = |host, action, left| self.call_within(host, action, left);
        let mut lanes = read_records(selected, &window, &mut call)?;
        let waits = fetch_waits(&lanes, selected, |host, action| {
            call(host, action, window.next()?)
        });
        super::apply_lane_wait_suppression(&mut lanes, &waits);
        Ok(lanes)
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
        let window = Window::new(remaining);
        let mut call = |host, action, left| self.call_within(host, action, left);
        let mut lanes = read_newest(host, &window, tracked, keep, &mut call)?;
        let selected: Vec<_> = lanes.iter().map(|lane| (host, lane.attempt_id)).collect();
        lanes.retain(|lane| keep(lane));
        let waits = fetch_waits(&lanes, &selected, |owner, action| {
            call(owner, action, window.next()?)
        });
        super::apply_lane_wait_suppression(&mut lanes, &waits);
        Ok(lanes)
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
struct Window<'a> {
    /// Unbounded when the budget is too large to represent.
    deadline: Option<Instant>,
    called: Cell<bool>,
    now: &'a dyn Fn() -> Instant,
}
impl<'a> Window<'a> {
    // A reader deadline is a real transport boundary.
    #[allow(clippy::disallowed_methods)]
    fn new(remaining: Duration) -> Self {
        Self::with_clock(remaining, &Instant::now)
    }
    fn with_clock(remaining: Duration, now: &'a dyn Fn() -> Instant) -> Self {
        Self {
            deadline: now().checked_add(remaining),
            called: Cell::new(false),
            now,
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
        let left = deadline.saturating_duration_since((self.now)());
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
    window: &Window<'_>,
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
    window: &Window<'_>,
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
            run::AttemptState::Queued { .. }
                | run::AttemptState::Starting { .. }
                | run::AttemptState::Running { .. }
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
                if met {
                    return Ok((lanes, true));
                }
                if super::silent_lane(&lanes).is_some() {
                    return Ok((lanes, false));
                }
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
    super::render_lanes_with_view(lanes, waits, next, state, None, false)
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
                .map(|(host, _)| *host)
                .or(lane.trace.host);
            row["host"] = json!(host);
            row["result_path"] = Value::Null;
            row["stale"] = Value::Null;
            if row["transcript"].is_object() {
                row["transcript"]["path"] = Value::Null;
            }
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

pub(super) fn unknown_host(host: &str) -> Fail {
    Fail::new(
        Status::Invalid,
        ErrorKind::UnknownHost,
        format!(
            "no configured peer named {host}; use `local` for this host or see `provenant hosts`"
        ),
    )
}

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
                .map_or_else(|| lane.trace.host.unwrap_or(first), |(host, _)| *host)
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
                    .any(|state| state.as_str() == state_and_reason(&lane.state).0)
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
                    watch::json_line_with_successor(lane, host, lane.superseded_by)
                } else {
                    watch::line_with_successor(lane, host, lane.superseded_by)
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
        if let Some(id) = watched.silent_lane {
            data["timed_out"] = json!(false);
            data["reason"] = json!(format!("provider silent; lane {id} is still running"));
        }
        let human = if json {
            String::new()
        } else {
            watch::end_line(&watched)
        };
        return Ok((
            data,
            human,
            !watched.settled && watched.silent_lane.is_none(),
        ));
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
    if let Some(id) = waited.silent_lane {
        data["timed_out"] = json!(false);
        data["reason"] = json!(format!("provider silent; lane {id} is still running"));
    }
    if waited.ignored > 0 {
        data["ignored_settled"] = json!(waited.ignored);
    }
    Ok((data, human, !waited.met && waited.silent_lane.is_none()))
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
                    .any(|state| state.as_str() == state_and_reason(&lane.state).0)
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
    let peer = Peer::open_for(edge, [placement])?;
    read_output(command, host, attempt, peer)
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
            "peer history exceeds its enumeration limit; use a full attempt ID from the peer's `provenant status --all`",
        ));
    }
    let candidates = records
        .into_iter()
        .map(|record| {
            let key = record.key.ok_or_else(corrupt)?;
            Ok(super::targets::Candidate {
                target: crate::Target {
                    selection: Some(key.to_string()),
                    run: None,
                    task: None,
                    attempt: None,
                },
                project: None,
                host: Some(host),
            })
        })
        .collect::<Result<Vec<_>, Fail>>()?;
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
    let window = Window::new(Duration::from_secs(30));
    read_output_with(command, host, attempt, |input| {
        match peer.call_within(
            host,
            PeerAction::Output {
                extra: Default::default(),
                input,
            },
            window.next()?,
        )? {
            PeerResult::Output { data, .. } => Ok(data),
            _ => Err(corrupt()),
        }
    })
}

pub(super) fn read_output_with(
    command: &Output,
    host: HostId,
    attempt: AttemptId,
    mut read: impl FnMut(OutputInput) -> Result<OutputResult, Fail>,
) -> Result<(Value, String), Fail> {
    if command.path {
        return Err(unsupported("--path"));
    }
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
    let chunk = read(OutputInput::Read {
        extra: Default::default(),
        target: AttemptTarget {
            extra: Default::default(),
            attempt,
            host: Some(host),
        },
        part,
        offset: command.offset,
        max_bytes: command.max_bytes,
    })?;
    let fallback = match &chunk {
        OutputResult::Chunk { extra, .. } if matches!(command.part, Part::Result) => extra
            .get(super::output::PLANNER_FALLBACK_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
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
            read(input)
        })?
    } else {
        chunk
    };
    let (mut data, mut text) = chunk_response(attempt, host, &command.part, chunk)?;
    if let Some(reason) = fallback {
        super::output::add_planner_fallback(&mut data, &mut text, reason);
    }
    Ok((data, text))
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
        extra,
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
    if extra.get("attempt_id").and_then(Value::as_str) != Some(attempt.to_string().as_str()) {
        return Err(corrupt());
    }
    let text = match &bytes {
        OutputBytes::Utf8(text) => text.clone(),
        OutputBytes::Base64(data) => data.clone(),
    };
    let mut data = json!({
        "attempt_id": attempt, "host": host, "state": state, "reason": reason,
        "part": format!("{part:?}").to_lowercase(), "offset": offset,
        "next_offset": next_offset, "length": null, "eof": eof, "clipped": null, "complete": null, "digest": digest, "bytes": bytes,
    });
    if let Some(object) = data.as_object_mut() {
        object.extend(extra);
    }
    Ok((data, text))
}

/// Ask the owning peer to cancel each named attempt. The peer records its own
/// reason; one cancel operation is bound per attempt.
pub(super) fn cancel(command: &Cancel, placement: &Placement, edge: &Edge<'_>) -> Reply {
    let mut status = Status::Ok;
    let mut out = reply(
        "stop",
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
            let mut observed = Vec::new();
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
                            let state = state_and_reason(&record.state).0;
                            observed.push(record.clone());
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
                        json!({ "attempt_id": attempt, "host": host, "error": super::command_error(&fail) })
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
                    status = if super::silent_lane(&lanes).is_some() {
                        Status::Failed
                    } else {
                        Status::Timeout
                    };
                }
                // No peer read after the wait's deadline.
                let (rendered, text) =
                    render_with_waits(&lanes, &[], &selected, edge.roots.state.as_path());
                data["lanes"] = rendered["lanes"].clone();
                data["condition_met"] = json!(met);
                if !met && let Some(id) = super::silent_lane(&lanes) {
                    data["timed_out"] = json!(false);
                    data["reason"] = json!(format!("provider silent; lane {id} is still running"));
                }
                human.push_str(&text);
            } else {
                let (rendered, _) =
                    render_with_waits(&observed, &[], &selected, edge.roots.state.as_path());
                data["lanes"] = rendered["lanes"].clone();
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

/// The terminal attempts of every configured peer, optionally narrowed to a
/// graph, read as `lanes --host` reads them. `None` when this is not a
/// federated project or no peer is configured.
pub(super) fn peer_facts(
    edge: &Edge<'_>,
    target: Option<&crate::target::Target>,
) -> Result<Option<PeerFacts>, Fail> {
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
    let (facts, hosts) = collect(&names, target, |host, action| peer.call(host, action));
    Ok(Some(PeerFacts {
        local: peer.context.identity.host,
        names,
        facts,
        hosts,
    }))
}

/// The most lanes one peer returns; the peer rejects a larger page.
const PEER_LANES: u16 = 100;

/// Ask each host for its lanes and keep the terminal ones of `graph`, or of
/// any graph. A host that fails is listed with its error and the rest still count.
fn collect(
    names: &BTreeMap<HostId, String>,
    target: Option<&crate::target::Target>,
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
                        .filter(|r| {
                            target.is_none_or(|target| {
                                r.key
                                    .as_ref()
                                    .is_some_and(|key| super::work_key_matches(key, target))
                            })
                        })
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

/// Work keys from peer receipts, used to check graph target uniqueness against
/// local identities before routing a command.
pub(super) fn target_candidates(edge: &Edge<'_>) -> Result<Vec<super::targets::Candidate>, Fail> {
    let roots = edge.roots;
    if !roots.state.as_path().join(PLACEMENTS).exists() {
        return Ok(Vec::new());
    }
    let peer = Peer::open(edge)?;
    let Some(project) = peer
        .context
        .identity
        .federation
        .as_ref()
        .map(|project| project.key.clone())
    else {
        return Ok(Vec::new());
    };
    let directory = directory(roots)?;
    let mut candidates = Vec::new();
    for name in directory.entries().map_err(io_fail)? {
        let name = name.map_err(io_fail)?;
        let prefix = format!("{}-", project.as_str());
        let Some(file_name) = name
            .to_str()
            .filter(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        else {
            continue;
        };
        let bytes = directory
            .read_bounded(OsStr::new(file_name), PLACEMENT_LIMIT)
            .map_err(io_fail)?;
        let placement = decode_placement(&bytes)?;
        if placement.project != project {
            return Err(corrupt());
        }
        for attempt in placement.attempts {
            candidates.push(super::targets::Candidate {
                target: crate::Target {
                    selection: Some(attempt.key.to_string()),
                    run: None,
                    task: None,
                    attempt: None,
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
    fn remote_output_carries_the_attempt_state_and_reason() -> Result {
        let attempt = AttemptId::from_bytes([3; 16]);
        let host = HostId::from_bytes([7; 16]);
        let chunk = OutputResult::Chunk {
            extra: [("attempt_id".into(), json!(attempt))].into(),
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
            schema_version: 2,
            host: HostId::from_bytes([7; 16]),
            project: serde_json::from_value(json!("project-key"))?,
            operation_id: OperationId::parse(operation)?,
            graph: "graph-one".into(),
            attempts: vec![
                Placed {
                    extra: Default::default(),
                    key: WorkKey::new(
                        provenant_core::ProjectId::from_bytes([3; 16]),
                        "graph-one",
                        "plan",
                        1,
                    )?,
                    attempt_id: AttemptId::from_bytes([3; 16]),
                },
                Placed {
                    extra: Default::default(),
                    key: WorkKey::new(
                        provenant_core::ProjectId::from_bytes([3; 16]),
                        "graph-one",
                        "implement",
                        2,
                    )?,
                    attempt_id: AttemptId::from_bytes([5; 16]),
                },
            ],
        })
    }

    #[test]
    fn peer_placement_round_trip_preserves_work_keys() -> Result {
        let original = placement("place")?;
        let bytes = serde_json::to_vec(&original)?;
        let decoded = decode_placement(&bytes).map_err(|fail| fail.message)?;
        assert_eq!(decoded, original);
        assert_eq!(
            decoded
                .attempts
                .iter()
                .map(|placed| placed.key.clone())
                .collect::<Vec<_>>(),
            original
                .attempts
                .iter()
                .map(|placed| placed.key.clone())
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn placement_replays_its_operation_and_refuses_another() -> Result {
        let (_scratch, roots) = roots()?;
        let first = placement("place")?;
        assert_eq!(
            find(&roots, &first.project, &first.graph).map_err(|f| f.message)?,
            None
        );
        save(&roots, &first).map_err(|f| f.message)?;
        save(&roots, &first).map_err(|f| f.message)?;
        assert_eq!(
            find(&roots, &first.project, &first.graph).map_err(|f| f.message)?,
            Some(first)
        );
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
        let refreshed = find(&roots, &original.project, &original.graph)
            .map_err(|f| f.message)?
            .ok_or("missing placement")?;
        assert_eq!(serde_json::to_value(refreshed)?, newer);
        Ok(())
    }

    #[test]
    fn graph_targets_resolve_peer_nodes_and_attempts() -> Result {
        let (_scratch, roots) = roots()?;
        let placed = placement("place")?;
        save(&roots, &placed).map_err(|f| f.message)?;
        let graph: crate::Target = "graph-one".parse()?;
        assert_eq!(
            find_target_in_project(&roots, &placed.project, &graph, false).map_err(|f| f.message)?,
            None
        );
        let found = find_target_in_project(&roots, &placed.project, &graph, true)
            .map_err(|f| f.message)?
            .ok_or("missing graph")?;
        assert_eq!(found.select_target(&graph).map_err(|f| f.message)?.len(), 2);
        let attempt: crate::Target = "graph-one/plan#1".parse()?;
        assert_eq!(
            find_target_in_project(&roots, &placed.project, &attempt, false)
                .map_err(|f| f.message)?
                .ok_or("missing attempt")?
                .select_target(&attempt)
                .map_err(|f| f.message)?,
            vec![(placed.host, placed.attempts[0].attempt_id)]
        );
        Ok(())
    }

    #[test]
    fn work_key_selection_narrows_by_node_and_attempt() -> Result {
        let placed = placement("place")?;
        let host = HostId::from_bytes([7; 16]);
        assert_eq!(
            placed
                .select("graph-one", None, None)
                .map_err(|f| f.message)?
                .len(),
            2
        );
        assert_eq!(
            placed
                .select("graph-one", Some("implement"), None)
                .map_err(|f| f.message)?,
            vec![(host, AttemptId::from_bytes([5; 16]))]
        );
        let missing = placed
            .select("graph-one", Some("plan"), Some(2))
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
    fn peer_refusal_keeps_owner_code_and_safe_facts() {
        use provenant_fabric_runtime::coordination::facade::DispatchRefusalFact;
        let fail = peer_failure_fail(
            FacadeError::DispatchRefused {
                fact: Some(DispatchRefusalFact::DetachedWriter),
                disk: None,
                reason: "writer refused".into(),
            }
            .into(),
        );
        let error = json!(super::super::command_error(&fail));
        assert_eq!(error["code"], "REFUSED");
        assert_eq!(error["detail"], "writer needs a branch");
        assert!(error["next"].as_str().is_some_and(|next| !next.is_empty()));
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
    fn peer_wait_returns_early_for_an_already_silent_lane() {
        let mut lane = running(1);
        lane.provider_silent_for_ms.0 = Some(1_800_000);
        let mut calls = 0;
        let (lanes, met) = poll_until(
            |_| {
                calls += 1;
                Ok(vec![lane.clone()])
            },
            Condition::All,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .unwrap();
        assert!(!met);
        assert_eq!(super::super::silent_lane(&lanes), Some(lane.attempt_id));
        assert_eq!(lanes[0].state, lane.state);
        assert_eq!(calls, 1);
    }

    #[test]
    fn peer_any_wait_returns_met_before_a_silent_sibling() {
        let mut silent = running(2);
        silent.provider_silent_for_ms.0 = Some(1_800_000);
        let (lanes, met) = poll_until(
            |_| Ok(vec![done(1, 10), silent.clone()]),
            Condition::Any,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .unwrap();
        assert!(met);
        assert_eq!(super::super::silent_lane(&lanes), Some(silent.attempt_id));
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
            "run_id": "09090909090909090909090909090909",
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": "04040404040404040404040404040404",
            "key": {"project": provenant_core::ProjectId::from_bytes([3; 16]), "graph": "graph-one", "node": "implement", "attempt": 1},
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
    fn done(id: u8, at: i64) -> run::AttemptRecord {
        let mut lane = running(id);
        lane.state = run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::Succeeded,
            exit_code: Some(0),
            finished_at: WallTime::from_micros(at).unwrap(),
            reason: None,
        };
        lane
    }
    fn attempt(id: u8) -> AttemptId {
        AttemptId::from_bytes([id; 16])
    }
    /// What a peer answers: a status record for each target, and an empty page.
    fn answer(action: PeerAction) -> std::result::Result<PeerResult, Fail> {
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

    #[allow(clippy::disallowed_methods)] // Only the origin is sampled; tests advance the clock explicitly.
    fn fixture_instant() -> Instant {
        Instant::now()
    }

    #[test]
    fn every_read_of_a_fetch_gets_only_the_time_the_earlier_ones_left() {
        // One host with 101 lanes reads in two chunks, then a second host.
        let mut selected: Vec<_> = (1..=101)
            .map(|id| (HostId::from_bytes([1; 16]), attempt(id)))
            .collect();
        selected.push((HostId::from_bytes([2; 16]), attempt(102)));
        let mut left = Vec::new();
        let time = Cell::new(fixture_instant());
        let now = || time.get();
        let window = Window::with_clock(Duration::from_millis(900), &now);
        let records = read_records(&selected, &window, &mut |_, action, budget| {
            left.push(budget);
            time.set(time.get() + Duration::from_millis(250));
            answer(action)
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
        let time = Cell::new(fixture_instant());
        let now = || time.get();
        let window = Window::with_clock(Duration::from_millis(100), &now);
        let failure = read_records(&selected, &window, &mut |_, action, _| {
            starts += 1;
            time.set(time.get() + Duration::from_millis(150));
            answer(action)
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
            answer(action)
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
        let time = Cell::new(fixture_instant());
        let now = || time.get();
        let window = Window::with_clock(Duration::from_millis(800), &now);
        let lanes = read_newest(
            host,
            &window,
            &mut tracking_one(),
            &keep,
            &mut |_, action, budget| {
                left.push(budget);
                time.set(time.get() + Duration::from_millis(300));
                answer(action)
            },
        )
        .expect("both reads fit");
        assert_eq!((lanes.len(), left.len()), (1, 2));
        assert!(left[1] <= Duration::from_millis(500), "{left:?}");

        // The first read used everything: the lookup must not start, even
        // though a zero remainder would round up to a one-second budget.
        let mut starts = 0;
        let window = Window::with_clock(Duration::from_millis(100), &now);
        let failure = read_newest(
            host,
            &window,
            &mut tracking_one(),
            &keep,
            &mut |_, action, _| {
                starts += 1;
                time.set(time.get() + Duration::from_millis(150));
                answer(action)
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
