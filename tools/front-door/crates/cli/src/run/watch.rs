//! Armed lane waits and the streaming `lanes --watch`.
//!
//! A wait counts a lane only when it was unsettled as the wait started or has
//! settled since then (or since `--since`). A lane named by its exact attempt
//! ID always counts, whenever it settled. A wholly settled named selection
//! returns its terminal rows immediately. Both modes poll a `fetch` of the
//! candidate lanes, so the local store and peers share one rule.
use super::{Condition, ErrorKind, Fail, Status, condition_met, settled, state_name};
use provenant_core::{AttemptId, Clock, HostId, WallTime};
use provenant_os::clock::SystemClock;
use provenant_run as run;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

/// Which lanes a wait or watch may count.
pub(super) struct Scope {
    /// Attempts the caller named exactly; they count whenever they settled.
    pub(super) exact: BTreeSet<AttemptId>,
    /// Count lanes that settled at or after this time, even before arming.
    pub(super) since: Option<WallTime>,
    /// When the command started, before any fetch. A lane that settled at or
    /// after this time is a new settlement even if the first successful fetch
    /// comes later, such as after peer failures.
    pub(super) entered: WallTime,
    /// The caller named runs or attempts, so an empty selection is an error.
    pub(super) explicit: bool,
    /// Ignore lanes already settled when the wait starts. An unnamed selection
    /// clears this because its lanes are already limited to unseen settlements.
    pub(super) snapshot: bool,
}

fn finished(lane: &run::AttemptRecord) -> Option<WallTime> {
    match lane.state {
        run::AttemptState::Terminal { finished_at, .. } => Some(finished_at),
        _ => None,
    }
}

/// The time a command starts, which fixes what it counts as already settled.
pub(super) fn entered() -> Result<WallTime, Fail> {
    SystemClock::new()
        .wall_now()
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))
}

/// The lanes already settled when a wait started.
pub(super) struct Armed {
    scope: Scope,
    baseline: BTreeSet<AttemptId>,
}
impl Armed {
    pub(super) fn arm(scope: Scope, first: &[run::AttemptRecord]) -> Self {
        let baseline = first
            .iter()
            .filter(|lane| {
                scope.snapshot
                    && !scope.exact.contains(&lane.attempt_id)
                    && finished(lane).is_some_and(|at| at < scope.since.unwrap_or(scope.entered))
            })
            .map(|lane| lane.attempt_id)
            .collect();
        Self { scope, baseline }
    }
    pub(super) fn ignored(&self) -> usize {
        self.baseline.len()
    }
    fn counts(&self, lane: &run::AttemptRecord) -> bool {
        self.scope.exact.contains(&lane.attempt_id)
            || (!self.baseline.contains(&lane.attempt_id)
                && finished(lane).is_none_or(|at| self.scope.since.is_none_or(|since| at >= since)))
    }
    pub(super) fn counted(&self, lanes: Vec<run::AttemptRecord>) -> Vec<run::AttemptRecord> {
        lanes.into_iter().filter(|lane| self.counts(lane)).collect()
    }
}

/// Keeps every lane a bounded page once showed unsettled in view until it
/// settles. A peer's page holds only its newest lanes, so new dispatches can
/// push a running lane off it; reading that lane's page absence as completion
/// would end a wait or watch while it still runs.
#[derive(Default)]
pub(super) struct Tracked {
    open: BTreeSet<AttemptId>,
    /// Lanes that settled after falling off the page, as last read.
    closed: BTreeMap<AttemptId, run::AttemptRecord>,
}
impl Tracked {
    /// `page`, plus a current record from `lookup` for each tracked lane the
    /// page no longer holds, and the last record of those that settled since.
    pub(super) fn complete(
        &mut self,
        mut page: Vec<run::AttemptRecord>,
        lookup: impl FnOnce(&[AttemptId]) -> Result<Vec<run::AttemptRecord>, Fail>,
    ) -> Result<Vec<run::AttemptRecord>, Fail> {
        let on_page: BTreeSet<AttemptId> = page.iter().map(|lane| lane.attempt_id).collect();
        let missing: Vec<AttemptId> = self.open.difference(&on_page).copied().collect();
        if !missing.is_empty() {
            page.extend(lookup(&missing)?);
        }
        page.extend(
            self.closed
                .iter()
                .filter(|(id, _)| !on_page.contains(id))
                .map(|(_, lane)| lane.clone()),
        );
        for lane in &page {
            if !settled(&lane.state).0 {
                self.open.insert(lane.attempt_id);
            } else if self.open.remove(&lane.attempt_id) && !on_page.contains(&lane.attempt_id) {
                self.closed.insert(lane.attempt_id, lane.clone());
            }
        }
        Ok(page)
    }
}

/// One page of lanes, newest queued first, and the cursor of the next page.
pub(super) type Page = (Vec<run::AttemptRecord>, Option<u64>);

/// Add to `ids` every lane `pages` yields that is unsettled or settled at or
/// after `since`, whenever it was queued. Queue order says nothing about when a
/// lane settled, so the scan covers up to 1000 lanes and refuses past that
/// instead of silently missing one.
pub(super) fn settled_since(
    mut ids: Vec<AttemptId>,
    mut pages: impl FnMut(u64) -> Result<Page, Fail>,
    since: WallTime,
) -> Result<Vec<AttemptId>, Fail> {
    const SCAN: usize = 1000;
    let mut scanned = 0;
    let mut after = 0;
    loop {
        let (lanes, next) = pages(after)?;
        for lane in &lanes {
            let recent = finished(lane).is_none_or(|at| at >= since);
            if recent && !ids.contains(&lane.attempt_id) {
                ids.push(lane.attempt_id);
            }
        }
        scanned += lanes.len();
        match next {
            Some(next) if scanned < SCAN => after = next,
            Some(_) => {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "--since reaches back more than 1000 lanes; narrow it or name lanes with --id",
                ));
            }
            None => return Ok(ids),
        }
    }
}

pub(super) fn nothing_pending(ignored: usize) -> Fail {
    Fail::new(
        Status::Invalid,
        ErrorKind::Invalid,
        format!(
            "no named lane is pending: {ignored} settled before the wait started; \
             pass --since TIME to count earlier settlements or name the exact attempt ID"
        ),
    )
}

/// Reads the candidate lanes within the time left to the wait's deadline, so a
/// slow peer cannot hold the command past its budget.
pub(super) type Fetch<'a> = dyn FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail> + 'a;

pub(super) enum Tick {
    Lanes(Vec<run::AttemptRecord>),
    /// A transient read failure, to retry while the budget lasts.
    Retry(Fail),
}
/// One fetch, given what is left of the budget. Transient read failures are
/// retried within the budget.
// A reader deadline is a real transport boundary.
#[allow(clippy::disallowed_methods)]
pub(super) fn tick(fetch: &mut Fetch<'_>, deadline: Instant) -> Result<Tick, Fail> {
    match fetch(deadline.saturating_duration_since(Instant::now())) {
        Ok(lanes) => Ok(Tick::Lanes(lanes)),
        Err(fail) if fail.kind == ErrorKind::Contention => Ok(Tick::Retry(fail)),
        Err(fail) if fail.kind == ErrorKind::PeerUnavailable && Instant::now() < deadline => {
            Ok(Tick::Retry(fail))
        }
        Err(fail) => Err(fail),
    }
}
// Waits and watches have a real-time reader deadline independent of store progress.
#[allow(clippy::disallowed_methods)]
pub(super) fn pause(poll: Duration, deadline: Instant) {
    std::thread::park_timeout(poll.min(deadline.saturating_duration_since(Instant::now())));
}
/// Briefly recheck an unsafe store observation, retaining a persistent error.
#[allow(clippy::disallowed_methods)]
pub(super) fn retry_store_path<T>(
    deadline: Instant,
    mut operation: impl FnMut() -> Result<T, Fail>,
) -> Result<T, Fail> {
    let until = deadline.min(Instant::now() + Duration::from_secs(1));
    loop {
        match operation() {
            Err(mut fail) if fail.store_path_unsafe && Instant::now() < until => {
                pause(Duration::from_millis(50), until);
                if Instant::now() >= until {
                    fail.store_path_unsafe = false;
                    return Err(fail);
                }
            }
            result => return result,
        }
    }
}
/// Retry a store operation until it succeeds or contention outlasts the wait.
#[allow(clippy::disallowed_methods)]
pub(super) fn retry_contention(
    deadline: Instant,
    poll: Duration,
    mut operation: impl FnMut() -> Result<(), Fail>,
) -> Result<bool, Fail> {
    loop {
        match operation() {
            Ok(()) => return Ok(true),
            Err(fail) if fail.kind == ErrorKind::Contention => {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                pause(poll, deadline);
                if Instant::now() >= deadline {
                    return Ok(false);
                }
            }
            Err(fail) => return Err(fail),
        }
    }
}
/// Arm on the first successful fetch. No fetch starts after the deadline: the
/// last transient cause is reported instead.
// A reader deadline is a real transport boundary.
#[allow(clippy::disallowed_methods)]
fn arm(
    fetch: &mut Fetch<'_>,
    scope: Scope,
    deadline: Instant,
    poll: Duration,
) -> Result<(Armed, Vec<run::AttemptRecord>), Fail> {
    let first = loop {
        match tick(fetch, deadline)? {
            Tick::Lanes(first) => break first,
            Tick::Retry(fail) => {
                pause(poll, deadline);
                if Instant::now() >= deadline {
                    return Err(fail);
                }
            }
        }
    };
    let mut armed = Armed::arm(scope, &first);
    if armed.scope.explicit && !first.is_empty() && first.iter().all(|lane| settled(&lane.state).0)
    {
        armed.baseline.clear();
        // A wholly settled named selection is an explicit result lookup,
        // including when --since would otherwise exclude its terminal rows.
        armed.scope.since = None;
    }
    let counted = armed.counted(first);
    if armed.scope.explicit && counted.is_empty() {
        return Err(nothing_pending(armed.ignored()));
    }
    Ok((armed, counted))
}

/// Sleep out the poll, then, if the deadline has not passed, read the lanes
/// again. A transient read failure leaves `lanes` as they were and is kept in
/// `stale` until a later read succeeds, so a peer failure can be reported.
// A reader deadline is a real transport boundary.
#[allow(clippy::disallowed_methods)]
fn refresh(
    fetch: &mut Fetch<'_>,
    armed: &Armed,
    deadline: Instant,
    poll: Duration,
    lanes: &mut Vec<run::AttemptRecord>,
    stale: &mut Option<Fail>,
) -> Result<(), Fail> {
    pause(poll, deadline);
    if Instant::now() < deadline {
        match tick(fetch, deadline)? {
            Tick::Lanes(next) => {
                *lanes = armed.counted(next);
                *stale = None;
            }
            Tick::Retry(fail) => *stale = Some(fail),
        }
    }
    Ok(())
}

pub(super) struct Waited {
    pub(super) lanes: Vec<run::AttemptRecord>,
    pub(super) met: bool,
    pub(super) ignored: usize,
}
/// Poll until the condition holds over the counted lanes or the budget passes.
// A reader deadline is a real transport boundary.
#[allow(clippy::disallowed_methods)]
pub(super) fn wait(
    mut fetch: impl FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail>,
    scope: Scope,
    condition: Condition,
    budget: Duration,
    poll: Duration,
) -> Result<Waited, Fail> {
    let deadline = Instant::now() + budget;
    let (armed, mut lanes) = match arm(&mut fetch, scope, deadline, poll) {
        Err(fail) if fail.kind == ErrorKind::Contention => {
            return Ok(Waited {
                lanes: Vec::new(),
                met: false,
                ignored: 0,
            });
        }
        result => result?,
    };
    let mut stale = None;
    loop {
        let met = condition_met(&lanes, condition);
        if met || Instant::now() >= deadline {
            if let Some(fail) = stale.filter(|fail: &Fail| fail.kind != ErrorKind::Contention) {
                return Err(fail);
            }
            return Ok(Waited {
                lanes,
                met,
                ignored: armed.ignored(),
            });
        }
        refresh(&mut fetch, &armed, deadline, poll, &mut lanes, &mut stale)?;
    }
}

pub(super) struct Watched {
    pub(super) lanes: Vec<run::AttemptRecord>,
    pub(super) settled: bool,
}
/// Report each counted lane's state, then every change, until all have
/// settled or the budget passes. `emit` runs once per observed state.
// A reader deadline is a real transport boundary.
#[allow(clippy::disallowed_methods)]
pub(super) fn watch(
    mut fetch: impl FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail>,
    scope: Scope,
    budget: Duration,
    poll: Duration,
    emit: &mut dyn FnMut(&run::AttemptRecord) -> Result<(), Fail>,
) -> Result<Watched, Fail> {
    let deadline = Instant::now() + budget;
    let (armed, mut lanes) = match arm(&mut fetch, scope, deadline, poll) {
        Err(fail) if fail.kind == ErrorKind::Contention => {
            return Ok(Watched {
                lanes: Vec::new(),
                settled: false,
            });
        }
        result => result?,
    };
    let mut seen: BTreeMap<AttemptId, String> = BTreeMap::new();
    let mut stale = None;
    loop {
        for lane in &lanes {
            let state = state_name(&lane.state);
            if seen.get(&lane.attempt_id) != Some(&state) {
                emit(lane)?;
                seen.insert(lane.attempt_id, state);
            }
        }
        let all_settled = lanes.iter().all(|lane| settled(&lane.state).0);
        if all_settled || Instant::now() >= deadline {
            if let Some(fail) = stale.filter(|fail: &Fail| fail.kind != ErrorKind::Contention) {
                return Err(fail);
            }
            return Ok(Watched {
                lanes,
                settled: all_settled,
            });
        }
        refresh(&mut fetch, &armed, deadline, poll, &mut lanes, &mut stale)?;
    }
}

/// `STATE RUN/TASK/ATTEMPT`, plus ` host=ID` for a peer's lane. The line never
/// carries free text, so a peer cannot inject terminal controls into it.
pub(super) fn line(lane: &run::AttemptRecord, host: Option<HostId>) -> String {
    let mut line = format!(
        "{} {}/{}/{}",
        state_name(&lane.state),
        lane.run_id,
        lane.task_id,
        lane.attempt_id
    );
    if let Some(host) = host {
        line.push_str(&format!(" host={host}"));
    }
    line
}
/// The same event as one JSON object on one line.
pub(super) fn json_line(lane: &run::AttemptRecord, host: Option<HostId>) -> String {
    json!({
        "event": "lane",
        "state": state_name(&lane.state),
        "run_id": lane.run_id,
        "task_id": lane.task_id,
        "attempt_id": lane.attempt_id,
        "host": host,
    })
    .to_string()
}
/// The closing line of a watch.
pub(super) fn end_line(watched: &Watched) -> String {
    let pending = watched
        .lanes
        .iter()
        .filter(|lane| !settled(&lane.state).0)
        .count();
    if watched.settled {
        format!("end settled lanes={}\n", watched.lanes.len())
    } else {
        format!(
            "end timeout lanes={} pending={pending}\n",
            watched.lanes.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{Digest, ProjectId, RunId, TaskId};
    use provenant_records::CanonicalV1;

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn incomplete_store_is_rechecked_and_persistent_unsafe_store_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&checkout)?;
        let state = temp.path().join("state");
        let mut inputs = provenant_core::RootInputs::new(home, checkout);
        inputs.state_dir = Some(state.clone());
        let context = run::ExecutionContext {
            state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
            project_id: ProjectId::from_bytes([1; 16]),
            actor_id: provenant_core::ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        std::fs::create_dir(&state)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755))?;
        let clock = SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = run::Request {
            clock: &clock,
            budget: provenant_core::WaitBudget::nonblocking(provenant_core::Clock::mono_now(
                &clock,
            )),
            cancel: &cancel,
        };
        let mut reads = 0;
        retry_store_path(Instant::now() + Duration::from_secs(2), || {
            reads += 1;
            if reads == 2 {
                std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            run::RunStore::open_read_only(&context, &call)
                .map(|_| ())
                .map_err(Fail::from)
        })
        .unwrap();
        assert_eq!(reads, 2);
        let store = run::RunStore::open(&context, &call)?;
        drop(store);
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755))?;
        let failure = retry_store_path(Instant::now() + Duration::from_millis(120), || {
            run::RunStore::open_read_only(&context, &call)
                .map(|_| ())
                .map_err(Fail::from)
        })
        .unwrap_err();
        assert!(failure.message.contains("not private or contained"));
        Ok(())
    }

    fn lane(id: u8, state: serde_json::Value) -> run::AttemptRecord {
        serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([id; 16]),
            "run_id": RunId::from_bytes([9; 16]),
            "project_id": ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "watch-fixture",
            "attempt_number": u64::from(id),
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": Digest::from_bytes([5; 32]),
            "queued_at": WallTime::from_micros(1).unwrap(),
            "state": state,
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 0,
                "complete": true,
                "redacted": false
            }
        }))
        .unwrap()
    }
    fn running(id: u8) -> run::AttemptRecord {
        lane(id, json!({"state":"running"}))
    }
    fn done(id: u8, at: i64) -> run::AttemptRecord {
        lane(
            id,
            json!({"state":"terminal","outcome":"succeeded","exit_code":0,
                   "finished_at":WallTime::from_micros(at).unwrap(),"reason":null}),
        )
    }
    fn scope(exact: &[u8], since: Option<i64>, explicit: bool) -> Scope {
        Scope {
            exact: exact
                .iter()
                .map(|id| AttemptId::from_bytes([*id; 16]))
                .collect(),
            since: since.map(|at| WallTime::from_micros(at).unwrap()),
            entered: WallTime::from_micros(15).unwrap(),
            explicit,
            snapshot: true,
        }
    }
    const POLL: Duration = Duration::from_millis(1);

    #[test]
    fn a_settled_older_attempt_never_satisfies_a_wait_on_a_newer_one() {
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                Ok(if calls < 3 {
                    vec![done(1, 10), running(2)]
                } else {
                    vec![done(1, 10), done(2, 20)]
                })
            },
            scope(&[], None, true),
            Condition::Any,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(calls, 3);
        assert_eq!(waited.ignored, 1);
        assert_eq!(waited.lanes.len(), 1);
        assert_eq!(waited.lanes[0].attempt_id, AttemptId::from_bytes([2; 16]));
    }

    #[test]
    fn an_older_settlement_expires_the_wait_instead_of_satisfying_it() {
        let waited = wait(
            |_| Ok(vec![done(1, 10), running(2)]),
            scope(&[], None, true),
            Condition::Any,
            Duration::from_millis(20),
            POLL,
        )
        .unwrap();
        assert!(!waited.met);
        assert_eq!(waited.lanes.len(), 1);
    }

    #[test]
    fn a_named_attempt_counts_whenever_it_settled() {
        let waited = wait(
            |_| Ok(vec![done(1, 10), running(2)]),
            scope(&[1], None, true),
            Condition::Any,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(waited.ignored, 0);
    }

    #[test]
    fn since_counts_settlements_before_arming() {
        let waited = wait(
            |_| Ok(vec![done(1, 10), done(2, 30), running(3)]),
            scope(&[], Some(20), true),
            Condition::Any,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(waited.ignored, 1);
        let ids: Vec<_> = waited.lanes.iter().map(|lane| lane.attempt_id).collect();
        assert_eq!(
            ids,
            [
                AttemptId::from_bytes([2; 16]),
                AttemptId::from_bytes([3; 16])
            ]
        );
    }

    #[test]
    fn already_settled_named_lanes_return_their_terminal_rows() {
        for condition in [Condition::Any, Condition::All] {
            for since in [None, Some(20)] {
                let mut calls = 0;
                let waited = wait(
                    |_| {
                        calls += 1;
                        Ok(vec![done(1, 10), done(2, 12)])
                    },
                    scope(&[], since, true),
                    condition,
                    Duration::from_secs(5),
                    POLL,
                )
                .unwrap();
                assert!(waited.met);
                assert_eq!(waited.lanes.len(), 2);
                assert_eq!(waited.ignored, 0);
                assert_eq!(calls, 1);
            }
        }
        let open = wait(
            |_| Ok(vec![done(1, 10)]),
            scope(&[], None, false),
            Condition::All,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(open.met && open.lanes.is_empty());
    }

    #[test]
    fn a_lane_that_appears_after_arming_counts() {
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                Ok(if calls == 1 {
                    vec![running(1)]
                } else {
                    vec![done(1, 5), done(2, 6)]
                })
            },
            scope(&[], None, true),
            Condition::All,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(waited.lanes.len(), 2);
    }

    #[test]
    fn a_transient_peer_failure_is_retried_within_budget() {
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                if calls < 3 {
                    Err(Fail::new(
                        Status::Unavailable,
                        ErrorKind::PeerUnavailable,
                        "workshop: ssh timed out",
                    ))
                } else {
                    Ok(vec![done(1, 5)])
                }
            },
            scope(&[1], None, true),
            Condition::All,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(calls, 3);
    }

    #[test]
    fn store_contention_is_retried_before_and_during_a_wait() {
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                match calls {
                    1 | 3 => Err(run::RunError::Contention.into()),
                    2 => Ok(vec![running(1)]),
                    _ => Ok(vec![done(1, 20)]),
                }
            },
            scope(&[1], None, true),
            Condition::All,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(calls, 4);
    }

    #[test]
    fn persistent_store_contention_stops_at_the_deadline() {
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                Err(run::RunError::Contention.into())
            },
            scope(&[1], None, true),
            Condition::All,
            Duration::from_millis(20),
            Duration::from_millis(1),
        )
        .expect("contention expires as a wait timeout");
        assert!(!waited.met);
        assert!(calls > 1);
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn acknowledgement_retries_contention_and_honours_the_deadline() {
        let mut calls = 0;
        let done = retry_contention(Instant::now() + Duration::from_secs(5), POLL, || {
            calls += 1;
            if calls < 3 {
                Err(run::RunError::Contention.into())
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert!(done);
        assert_eq!(calls, 3);
        let done = retry_contention(Instant::now() + Duration::from_millis(10), POLL, || {
            Err(run::RunError::Contention.into())
        })
        .unwrap();
        assert!(!done);
    }

    #[test]
    fn persistent_store_contention_expires_a_watch_without_failure() {
        for armed in [false, true] {
            let mut calls = 0;
            let watched = watch(
                |_| {
                    calls += 1;
                    if armed && calls == 1 {
                        Ok(vec![running(1)])
                    } else {
                        Err(run::RunError::Contention.into())
                    }
                },
                scope(&[1], None, true),
                Duration::from_millis(20),
                POLL,
                &mut |_| Ok(()),
            )
            .expect("contention expires as a watch timeout");
            assert!(!watched.settled);
            assert_eq!(watched.lanes.len(), usize::from(armed));
        }
    }

    #[test]
    fn watch_emits_each_state_once_and_ends_when_all_settle() {
        let mut calls = 0;
        let mut lines = Vec::new();
        let watched = watch(
            |_| {
                calls += 1;
                Ok(match calls {
                    1 => vec![lane(1, json!({"state":"queued"})), running(2)],
                    2 => vec![running(1), running(2)],
                    3 => vec![running(1), done(2, 9)],
                    _ => vec![done(1, 10), done(2, 9)],
                })
            },
            scope(&[], None, true),
            Duration::from_secs(5),
            POLL,
            &mut |lane| {
                lines.push(line(lane, None));
                Ok(())
            },
        )
        .unwrap();
        assert!(watched.settled);
        let run = RunId::from_bytes([9; 16]);
        let task = TaskId::from_bytes([4; 16]);
        let one = AttemptId::from_bytes([1; 16]);
        let two = AttemptId::from_bytes([2; 16]);
        assert_eq!(
            lines,
            [
                format!("queued {run}/{task}/{one}"),
                format!("running {run}/{task}/{two}"),
                format!("running {run}/{task}/{one}"),
                format!("succeeded {run}/{task}/{two}"),
                format!("succeeded {run}/{task}/{one}"),
            ]
        );
        assert_eq!(end_line(&watched), "end settled lanes=2\n");
    }

    #[test]
    fn watch_times_out_with_pending_lanes_and_names_the_host() {
        let host = HostId::from_bytes([7; 16]);
        let mut lines = Vec::new();
        let watched = watch(
            |_| Ok(vec![running(1), done(2, 1)]),
            scope(&[2], None, true),
            Duration::from_millis(10),
            POLL,
            &mut |lane| {
                lines.push(line(lane, Some(host)));
                Ok(())
            },
        )
        .unwrap();
        assert!(!watched.settled);
        assert_eq!(lines.len(), 2);
        assert!(
            lines
                .iter()
                .all(|line| line.ends_with(&format!(" host={host}")))
        );
        assert_eq!(end_line(&watched), "end timeout lanes=2 pending=1\n");
        let json: serde_json::Value = serde_json::from_str(&json_line(&running(1), None)).unwrap();
        assert_eq!(json["event"], "lane");
        assert_eq!(json["state"], "running");
    }

    #[test]
    fn a_fetch_gets_the_time_left_and_none_starts_after_the_deadline() {
        let budget = Duration::from_millis(60);
        for watching in [false, true] {
            let mut given = Vec::new();
            let mut fetch = |left: Duration| {
                given.push(left);
                Ok(vec![running(1)])
            };
            // The poll outlasts the budget, so the sleep ends at the deadline.
            let long_poll = Duration::from_secs(30);
            if watching {
                let watched = watch(
                    &mut fetch,
                    scope(&[], None, true),
                    budget,
                    long_poll,
                    &mut |_| Ok(()),
                )
                .unwrap();
                assert!(!watched.settled);
            } else {
                let waited = wait(
                    &mut fetch,
                    scope(&[], None, true),
                    Condition::All,
                    budget,
                    long_poll,
                )
                .unwrap();
                assert!(!waited.met);
            }
            assert_eq!(given.len(), 1, "watching={watching}: {given:?}");
            assert!(given[0] <= budget, "{given:?}");
        }
    }

    #[test]
    fn since_selects_by_settlement_time_not_queue_order() {
        // Every fixture lane was queued at 1, before the window starting at 40.
        // Page one holds the newest queued lane, which settled before the
        // window; the lanes behind it settled inside it or are still running.
        let mut asked = Vec::new();
        let ids = settled_since(
            Vec::new(),
            |after| {
                asked.push(after);
                Ok(match after {
                    0 => (vec![done(1, 10)], Some(7)),
                    7 => (vec![done(2, 50), running(3)], Some(9)),
                    _ => (vec![done(4, 40), done(5, 39)], None),
                })
            },
            WallTime::from_micros(40).unwrap(),
        )
        .unwrap();
        let ids: Vec<_> = ids.iter().map(|id| id.to_string()).collect();
        let expected: Vec<_> = [2, 3, 4]
            .iter()
            .map(|n| AttemptId::from_bytes([*n; 16]).to_string())
            .collect();
        assert_eq!(ids, expected);
        assert_eq!(asked, [0, 7, 9]);
    }

    #[test]
    fn since_refuses_a_scan_past_its_cap_instead_of_missing_lanes() {
        let refused = settled_since(
            Vec::new(),
            |after| Ok((vec![done(1, 10); 100], Some(after + 1))),
            WallTime::from_micros(40).unwrap(),
        );
        let message = refused.err().map(|fail| fail.message).unwrap_or_default();
        assert!(message.contains("more than 1000 lanes"), "{message}");
    }

    #[test]
    fn a_lane_that_settles_during_initial_peer_failures_still_counts() {
        // The command entered at 15; the peer was unreachable until the lane
        // had settled at 20, so the first successful fetch already sees it.
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                if calls < 3 {
                    Err(Fail::new(
                        Status::Unavailable,
                        ErrorKind::PeerUnavailable,
                        "workshop: ssh timed out",
                    ))
                } else {
                    Ok(vec![done(1, 20), done(2, 10)])
                }
            },
            scope(&[], None, true),
            Condition::All,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(waited.ignored, 0, "all named lanes have settled");
        assert_eq!(waited.lanes.len(), 2);
        assert_eq!(waited.lanes[0].attempt_id, AttemptId::from_bytes([1; 16]));
    }

    #[test]
    fn a_running_lane_pushed_off_the_page_keeps_the_wait_open() {
        let mut tracked = Tracked::default();
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                // Later dispatches fill the page; lane 2 finishes, lane 1 runs on.
                let page = if calls == 1 {
                    vec![running(1)]
                } else {
                    vec![done(2, 20)]
                };
                tracked.complete(page, |missing| {
                    assert_eq!(missing, [AttemptId::from_bytes([1; 16])]);
                    Ok(vec![running(1)])
                })
            },
            scope(&[], None, false),
            Condition::All,
            Duration::from_millis(40),
            POLL,
        )
        .unwrap();
        assert!(!waited.met, "lane 1 is still running");
        let ids: Vec<_> = waited.lanes.iter().map(|lane| lane.attempt_id).collect();
        assert!(ids.contains(&AttemptId::from_bytes([1; 16])), "{ids:?}");
    }

    #[test]
    fn a_lane_that_settles_off_the_page_is_kept_in_the_result() {
        let mut tracked = Tracked::default();
        let mut calls = 0;
        let waited = wait(
            |_| {
                calls += 1;
                let page = match calls {
                    1 => vec![running(1), running(2)],
                    2 => vec![running(2)],
                    _ => vec![done(2, 31)],
                };
                tracked.complete(page, |missing| {
                    assert_eq!(missing, [AttemptId::from_bytes([1; 16])]);
                    Ok(vec![done(1, 30)])
                })
            },
            scope(&[], None, false),
            Condition::All,
            Duration::from_secs(5),
            POLL,
        )
        .unwrap();
        assert!(waited.met);
        assert_eq!(
            waited.lanes.len(),
            2,
            "the settled lane stays in the result"
        );
    }

    #[test]
    // The deadline under test is a real-time reader boundary.
    #[allow(clippy::disallowed_methods)]
    fn arming_stops_retrying_at_the_deadline_with_the_last_cause() {
        let mut starts = Vec::new();
        let begun = Instant::now();
        let refused = wait(
            |_| {
                starts.push(Instant::now());
                std::thread::sleep(Duration::from_millis(25));
                Err(Fail::new(
                    Status::Unavailable,
                    ErrorKind::PeerUnavailable,
                    format!("workshop: attempt {}", starts.len()),
                ))
            },
            scope(&[], None, true),
            Condition::All,
            Duration::from_millis(40),
            Duration::from_millis(100),
        );
        let message = refused.err().map(|fail| fail.message).unwrap_or_default();
        assert!(message.contains("workshop: attempt"), "{message}");
        assert!(
            starts
                .iter()
                .all(|start| *start < begun + Duration::from_millis(40)),
            "a fetch began after the deadline"
        );
    }
}
