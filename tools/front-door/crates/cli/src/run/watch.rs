//! Armed lane waits and the streaming `lanes --watch`.
//!
//! A wait counts a lane only when it was unsettled as the wait started or has
//! settled since then (or since `--since`). A lane named by its exact attempt
//! ID always counts, whenever it settled. Both modes poll a `fetch` of the
//! candidate lanes, so the local store and peers share one rule.
use super::{Condition, ErrorKind, Fail, Status, condition_met, settled, state_name};
use provenant_core::{AttemptId, HostId, WallTime};
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
                    && finished(lane).is_some_and(|at| scope.since.is_none_or(|since| at < since))
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

/// One fetch. A transient peer failure is retried within the budget, and the
/// last cause is reported only if the budget runs out on it.
// A reader deadline is a real transport boundary.
#[allow(clippy::disallowed_methods)]
fn tick(
    fetch: &mut dyn FnMut() -> Result<Vec<run::AttemptRecord>, Fail>,
    deadline: Instant,
) -> Result<Option<Vec<run::AttemptRecord>>, Fail> {
    match fetch() {
        Ok(lanes) => Ok(Some(lanes)),
        Err(fail) if fail.kind == ErrorKind::PeerUnavailable && Instant::now() < deadline => {
            Ok(None)
        }
        Err(fail) => Err(fail),
    }
}
// Waits and watches have a real-time reader deadline independent of store progress.
#[allow(clippy::disallowed_methods)]
fn pause(poll: Duration, deadline: Instant) {
    std::thread::park_timeout(poll.min(deadline.saturating_duration_since(Instant::now())));
}
/// Arm on the first successful fetch.
fn arm(
    fetch: &mut dyn FnMut() -> Result<Vec<run::AttemptRecord>, Fail>,
    scope: Scope,
    deadline: Instant,
    poll: Duration,
) -> Result<(Armed, Vec<run::AttemptRecord>), Fail> {
    let first = loop {
        if let Some(first) = tick(fetch, deadline)? {
            break first;
        }
        pause(poll, deadline);
    };
    let armed = Armed::arm(scope, &first);
    let counted = armed.counted(first);
    if armed.scope.explicit && counted.is_empty() {
        return Err(nothing_pending(armed.ignored()));
    }
    Ok((armed, counted))
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
    mut fetch: impl FnMut() -> Result<Vec<run::AttemptRecord>, Fail>,
    scope: Scope,
    condition: Condition,
    budget: Duration,
    poll: Duration,
) -> Result<Waited, Fail> {
    let deadline = Instant::now() + budget;
    let (armed, mut lanes) = arm(&mut fetch, scope, deadline, poll)?;
    loop {
        let met = condition_met(&lanes, condition);
        if met || Instant::now() >= deadline {
            return Ok(Waited {
                lanes,
                met,
                ignored: armed.ignored(),
            });
        }
        pause(poll, deadline);
        if let Some(next) = tick(&mut fetch, deadline)? {
            lanes = armed.counted(next);
        }
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
    mut fetch: impl FnMut() -> Result<Vec<run::AttemptRecord>, Fail>,
    scope: Scope,
    budget: Duration,
    poll: Duration,
    emit: &mut dyn FnMut(&run::AttemptRecord) -> Result<(), Fail>,
) -> Result<Watched, Fail> {
    let deadline = Instant::now() + budget;
    let (armed, mut lanes) = arm(&mut fetch, scope, deadline, poll)?;
    let mut seen: BTreeMap<AttemptId, String> = BTreeMap::new();
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
            return Ok(Watched {
                lanes,
                settled: all_settled,
            });
        }
        pause(poll, deadline);
        if let Some(next) = tick(&mut fetch, deadline)? {
            lanes = armed.counted(next);
        }
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
            explicit,
            snapshot: true,
        }
    }
    const POLL: Duration = Duration::from_millis(1);

    #[test]
    fn a_settled_older_attempt_never_satisfies_a_wait_on_a_newer_one() {
        let mut calls = 0;
        let waited = wait(
            || {
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
            || Ok(vec![done(1, 10), running(2)]),
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
            || Ok(vec![done(1, 10), running(2)]),
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
            || Ok(vec![done(1, 10), done(2, 30), running(3)]),
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
    fn nothing_pending_is_refused_for_named_lanes_only() {
        let settled_only = || Ok(vec![done(1, 10)]);
        let refused = wait(
            settled_only,
            scope(&[], None, true),
            Condition::All,
            Duration::from_secs(5),
            POLL,
        );
        let message = refused.err().map(|fail| fail.message).unwrap_or_default();
        assert!(
            message.contains("1 settled before the wait started"),
            "{message}"
        );
        let open = wait(
            settled_only,
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
            || {
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
            || {
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
    fn watch_emits_each_state_once_and_ends_when_all_settle() {
        let mut calls = 0;
        let mut lines = Vec::new();
        let watched = watch(
            || {
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
            || Ok(vec![running(1), done(2, 1)]),
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
}
