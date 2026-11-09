use super::*;

pub(super) fn settled(state: &run::AttemptState) -> (bool, bool) {
    match state {
        run::AttemptState::Terminal { outcome, .. } => {
            (true, *outcome == run::Outcome::InputRequired)
        }
        _ => (false, false),
    }
}

pub(super) fn cloud_observed_at(lane: &run::AttemptRecord) -> Option<WallTime> {
    lane.cloud_observation
        .0
        .filter(|state| state.is_terminal())?;
    lane.extra
        .get("cloud_observed_at")?
        .as_str()
        .and_then(|time| WallTime::parse_rfc3339(time).ok())
}

pub(super) fn lane_settled(lane: &run::AttemptRecord) -> (bool, bool) {
    lane.cloud_observation.0.map_or_else(
        || settled(&lane.state),
        |state| (state.is_terminal(), false),
    )
}

/// Whether a lane selection satisfies a wait condition. Input required ends
/// an all-lanes wait because it cannot progress unattended.
pub(super) fn condition_met(lanes: &[run::AttemptRecord], condition: Condition) -> bool {
    let states: Vec<(bool, bool)> = lanes
        .iter()
        .filter(|l| l.superseded_by.is_none())
        .map(lane_settled)
        .collect();
    lanes.is_empty()
        || (!states.is_empty()
            && match condition {
                Condition::Any => states.iter().any(|(done, _)| *done),
                Condition::All => {
                    states.iter().all(|(done, _)| *done) || states.iter().any(|(_, input)| *input)
                }
            })
}

pub(crate) fn silent_lane(lanes: &[run::AttemptRecord]) -> Option<AttemptId> {
    lanes
        .iter()
        .find(|lane| lane.provider_silent_for_ms.0.is_some() && !lane.provider_silence_suppressed.0)
        .map(|lane| lane.attempt_id)
}

pub(crate) fn apply_lane_wait_suppression(
    lanes: &mut [run::AttemptRecord],
    waits: &[run::LaneWait],
) {
    for lane in lanes {
        lane.provider_silence_suppressed.0 |= waits
            .iter()
            .find(|wait| wait.attempt_id == lane.attempt_id)
            .is_some_and(|wait| {
                wait.waiting_for.starts_with("disk pressure paused")
                    || wait.waiting_for.starts_with("build slot:")
            });
    }
}

/// Snapshot every attempt the targets name. Without targets, the selection is
/// `owner`'s nonterminal work; host caps bound it. With `seen`, terminal
/// lanes at or before that event cursor are already acknowledged and dropped.
pub(super) fn select(
    session: &Session,
    targets: &[Target],
    seen: Option<u64>,
    owner: ActorId,
    call: &run::Request<'_>,
) -> Result<Vec<AttemptId>, Fail> {
    let mut selected = Vec::new();
    let mut after = 0;
    loop {
        let mut query = lane_query(
            session,
            targets,
            !targets.is_empty() || seen.is_some(),
            100,
            after,
            call,
        )?;
        query.owner = targets.is_empty().then_some(owner);
        query.unseen_after = seen;
        let page = session.service().lanes(&query, call)?;
        selected.extend(page.lanes.iter().map(|lane| lane.attempt_id));
        match page.next {
            Some(next) if selected.len() < 1000 => after = next,
            Some(_) => {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "selection exceeds 1000 lanes; narrow it with --id",
                ));
            }
            None => return Ok(selected),
        }
    }
}

const BACKLOG_SAMPLE: usize = 100;

#[derive(Default)]
struct Backlog {
    count: usize,
    latest_event: u64,
    newest: Vec<run::AttemptRecord>,
}

impl Backlog {
    fn add(
        &mut self,
        session: &Session,
        lane: run::AttemptRecord,
        call: &run::Request<'_>,
    ) -> Result<(), Fail> {
        self.count += 1;
        self.latest_event = self
            .latest_event
            .max(session.store.last_event_sequence(lane.attempt_id, call)?);
        self.newest.push(lane);
        self.newest.sort_by_key(|lane| match lane.state {
            run::AttemptState::Terminal { finished_at, .. } => std::cmp::Reverse(finished_at),
            _ => std::cmp::Reverse(cloud_observed_at(lane).unwrap_or(lane.queued_at)),
        });
        self.newest.truncate(BACKLOG_SAMPLE);
        Ok(())
    }
}

/// Preserve the ordinary selection until it exceeds the cap. For a stale
/// default cursor, keep every unsettled lane and a bounded sample of the
/// settled backlog, while retaining the event cursor needed to acknowledge it.
fn select_unseen(
    session: &Session,
    owner: ActorId,
    seen: u64,
    tracked: &mut std::collections::BTreeSet<AttemptId>,
    call: &run::Request<'_>,
) -> Result<(Vec<AttemptId>, Option<Backlog>), Fail> {
    let mut after = 0;
    let mut pending = Vec::new();
    let mut backlog = None;
    loop {
        let mut query = lane_query(session, &[], true, 100, after, call)?;
        query.owner = Some(owner);
        query.unseen_after = Some(seen);
        let page = session.service().lanes(&query, call)?;
        for mut lane in page.lanes {
            cloud_owner::hydrate(
                &Roots {
                    state: session.context.state_root.clone(),
                },
                &session.store,
                std::slice::from_mut(&mut lane),
                call,
            )
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
            if lane
                .cloud_observation
                .0
                .is_some_and(|state| state.is_terminal())
                && !tracked.contains(&lane.attempt_id)
                && session.store.last_event_sequence(lane.attempt_id, call)? <= seen
            {
                continue;
            }
            if !lane_settled(&lane).0 {
                tracked.insert(lane.attempt_id);
            }
            if let Some(summary) = backlog.as_mut() {
                if lane_settled(&lane).0 && !tracked.contains(&lane.attempt_id) {
                    Backlog::add(summary, session, lane, call)?;
                } else {
                    pending.push(lane);
                }
            } else {
                pending.push(lane);
                if pending.len() > 1000 {
                    let mut summary = Backlog::default();
                    let mut open = Vec::new();
                    for lane in pending.drain(..) {
                        if lane_settled(&lane).0 && !tracked.contains(&lane.attempt_id) {
                            summary.add(session, lane, call)?;
                        } else {
                            open.push(lane);
                        }
                    }
                    pending = open;
                    backlog = Some(summary);
                }
            }
            if backlog.is_some() && pending.len() > 1000 {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "selection exceeds 1000 unsettled lanes; narrow it with --id",
                ));
            }
        }
        match page.next {
            Some(next) => after = next,
            None => break,
        }
    }
    let ids = pending.into_iter().map(|lane| lane.attempt_id).collect();
    Ok((ids, backlog))
}

/// The seen-cursor key for a default wait selection.
pub(super) fn selection_key(label: Option<&str>) -> String {
    label.map_or_else(|| "default".into(), |label| format!("session:{label}"))
}

/// What a wait or watch selects: the lanes of named targets, or those of one
/// session when none are named.
pub(super) struct Picks<'a> {
    pub(super) targets: &'a [Target],
    /// The session whose lanes count; this session unless `--session` names one.
    pub(super) owner: ActorId,
    /// Set when `--session` names the session, which then also narrows targets.
    pub(super) label: Option<&'a str>,
    pub(super) since: Option<WallTime>,
}

impl Picks<'_> {
    pub(super) fn scope(&self) -> Result<watch::Scope, Fail> {
        Ok(watch::Scope {
            exact: self.targets.iter().filter_map(|t| t.attempt).collect(),
            since: self.since,
            entered: watch::entered()?,
            explicit: !self.targets.is_empty(),
            snapshot: !self.targets.is_empty(),
        })
    }
}

/// The agent label's actor on this host: the identity that submitted its lanes.
pub(super) fn session_actor(edge: &Edge<'_>, label: &str) -> Result<ActorId, Fail> {
    let identity = |error: String| Fail::new(Status::Error, ErrorKind::Identity, error);
    let host = provenant_config::bind_host(edge.roots).map_err(|e| identity(e.to_string()))?;
    provenant_fabric_runtime::identity::Address::new(host, label)
        .map(|address| address.actor)
        .map_err(|e| identity(e.to_string()))
}

/// Lanes still unsettled, or settled at or after `--since`, found by scanning
/// the session's lanes newest-queued first. A lane queued before the window can
/// still settle inside it, so the scan reads every retained lane up to its cap
/// rather than stopping at the first older queue time.
pub(super) fn select_since(
    session: &Session,
    owner: ActorId,
    since: WallTime,
    call: &run::Request<'_>,
) -> Result<Vec<AttemptId>, Fail> {
    let ids = select(session, &[], None, owner, call)?;
    watch::settled_since(
        ids,
        |after| {
            let mut query = lane_query(session, &[], true, 100, after, call)?;
            query.newest_first = true;
            query.owner = Some(owner);
            let page = session.service().lanes(&query, call)?;
            Ok((page.lanes, page.next))
        },
        since,
    )
    .map_err(Fail::from)
}

/// The candidate lanes for a wait or watch, read afresh on each call. With
/// `seen`, an unnamed selection holds the unsettled lanes and those settled
/// after that event cursor.
pub(super) fn candidates<'a>(
    session: &'a Session,
    picks: &'a Picks<'a>,
    seen: Option<u64>,
) -> impl FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail> + 'a {
    candidates_with_backlog(session, picks, seen, None, None)
}

fn candidates_with_backlog<'a>(
    session: &'a Session,
    picks: &'a Picks<'a>,
    seen: Option<u64>,
    backlog: Option<&'a std::cell::RefCell<Option<Backlog>>>,
    tracked: Option<&'a std::cell::RefCell<std::collections::BTreeSet<AttemptId>>>,
) -> impl FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail> + 'a {
    move |_| {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        // The outer wait owns the deadline and retries contention between polls.
        // Under WAL a read rarely waits; when it does, the busy timeout bounds it.
        let call = run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let _ = session.store.reap(&call);
        let ids = match picks.since {
            Some(since) if picks.targets.is_empty() => {
                select_since(session, picks.owner, since, &call)?
            }
            _ => match (picks.targets.is_empty(), seen, backlog, tracked) {
                (true, Some(cursor), Some(backlog), Some(tracked)) => {
                    let (ids, summary) = select_unseen(
                        session,
                        picks.owner,
                        cursor,
                        &mut tracked.borrow_mut(),
                        &call,
                    )?;
                    *backlog.borrow_mut() = summary;
                    ids
                }
                _ => select(session, picks.targets, seen, picks.owner, &call)?,
            },
        };
        let mut lanes = Vec::with_capacity(ids.len());
        let mut included = std::collections::BTreeSet::new();
        for id in ids {
            let lane = session.service().status_for_wait(id, &call)?;
            if included.insert(id)
                && (picks.label.is_none() || lane.verified_owner == Some(picks.owner))
            {
                lanes.push(lane);
            }
        }
        cloud_owner::hydrate(
            &Roots {
                state: session.context.state_root.clone(),
            },
            &session.store,
            &mut lanes,
            &call,
        )
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
        let waits = session.store.lane_waits(&lanes, &call);
        apply_lane_wait_suppression(&mut lanes, &waits);
        Ok(lanes)
    }
}

pub(super) const LOCAL_POLL: Duration = Duration::from_millis(500);

/// The lanes a wait or watch ended on, each unstarted one with what it waits for.
pub(super) fn render_settled(
    session: &Session,
    lanes: &[run::AttemptRecord],
    cooldowns: Option<(&[provenant_quota::AccountRecord], i64)>,
) -> Result<(Value, String), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let waits = session.store.lane_waits(
        lanes,
        &run::Request {
            clock: &clock,
            cancel: &cancel,
        },
    );
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };

    let (mut data, mut human) = render_lanes(
        lanes,
        &waits,
        None,
        session.context.state_root.as_path(),
        cooldowns,
    );
    append_provider_errors(session, lanes, &mut data, &mut human);
    attach_handoffs(session, lanes, &mut data);
    status::attach_cloud_jobs(&session.store, lanes, &mut data, &mut human, &call)?;
    Ok((data, human))
}

pub(super) fn append_provider_errors(
    session: &Session,
    lanes: &[run::AttemptRecord],
    data: &mut Value,
    human: &mut String,
) {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let Some(rows) = data["lanes"].as_array_mut() else {
        return;
    };
    for (lane, row) in lanes.iter().zip(rows) {
        if matches!(
            lane.state,
            run::AttemptState::Terminal {
                outcome: run::Outcome::Failed,
                ..
            }
        ) && let Some(error) = provider_error_line(session, lane.attempt_id, &call)
        {
            row["provider_error"] = json!(error);
            human.push_str(&format!(
                "  provider error {}: {}\n",
                lane.attempt_id,
                plain_text(&error, usize::MAX)
            ));
        }
    }
}

/// Each row's `handoff`: the settlement snapshot's parsed object, or null when
/// the attempt recorded none. Rows follow `lanes`, as `render_lanes` makes them.
pub(super) fn attach_handoffs(session: &Session, lanes: &[run::AttemptRecord], data: &mut Value) {
    let Some(rows) = data["lanes"].as_array_mut() else {
        return;
    };
    for (row, lane) in rows.iter_mut().zip(lanes) {
        row["handoff"] = session
            .store
            .handoff(lane)
            .ok()
            .flatten()
            .map_or(Value::Null, |handoff| json!(handoff));
        row["spec_concern_count"] = json!(
            row["handoff"]["spec_concerns"]
                .as_array()
                .map_or(0, Vec::len)
        );
    }
}

pub(super) fn waited_reply(
    session: &Session,
    waited: watch::Waited,
) -> Result<(Value, String, bool), Fail> {
    let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
    let (mut data, human) = render_settled(session, &waited.lanes, lane_cooldown_view(&cooldowns))?;
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

pub(super) fn apply_cancel_wait(
    data: &mut Value,
    lanes: &Value,
    timed_out: bool,
    status: &mut Status,
) {
    if lanes["reason"].is_string() {
        if *status == Status::Ok {
            *status = Status::Failed;
        }
    } else if timed_out && *status == Status::Ok {
        *status = Status::Timeout;
    }
    data["lanes"] = lanes["lanes"].clone();
    data["condition_met"] = lanes["condition_met"].clone();
    for field in ["timed_out", "reason"] {
        if let Some(value) = lanes.get(field) {
            data[field] = value.clone();
        }
    }
}

/// Wait for every lane a run, task or attempt names, settled or not: the
/// whole named set, for commands that act on a lane they just created.
pub(super) fn wait_lanes(
    session: &Session,
    targets: &[Target],
    condition: Condition,
    duration: Duration,
    _edge: &Edge<'_>,
) -> Result<(Value, String, bool), Fail> {
    let picks = Picks {
        targets,
        owner: session.context.actor_id,
        label: None,
        since: None,
    };
    let scope = watch::Scope {
        explicit: false,
        snapshot: false,
        ..picks.scope()?
    };
    let waited = watch::wait(
        candidates(session, &picks, None),
        scope,
        condition,
        duration,
        LOCAL_POLL,
    )?;
    waited_reply(session, waited)
}

#[cfg(test)]
thread_local! {
    pub(super) static AFTER_FIRST_FETCH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// `lanes --wait`: settlements after the wait starts, or after `--since`. Exact
/// attempt IDs count whenever they settled. Without named lanes, a session's
/// wait resumes after the settlements it last acknowledged.
#[allow(clippy::disallowed_methods)]
pub(super) fn wait_command(
    session: &Session,
    picks: &Picks<'_>,
    condition: Condition,
    deadline: Instant,
    entered: WallTime,
    edge: &Edge<'_>,
) -> Result<(Value, String, bool), Fail> {
    let key = selection_key(picks.label);
    let cursor = picks.targets.is_empty() && picks.since.is_none();
    let mut seen = None;
    let backlog = std::cell::RefCell::new(None);
    let tracked = std::cell::RefCell::new(std::collections::BTreeSet::new());
    let waited = watch::wait(
        |remaining| {
            let cursor_value = if cursor {
                if seen.is_none() {
                    let clock = SystemClock::new();
                    let cancel = CancelToken::new();
                    let call = run::Request {
                        clock: &clock,
                        cancel: &cancel,
                    };
                    seen = Some(session.store.seen_cursor(&key, true, &call)?);
                }
                seen
            } else {
                None
            };
            #[cfg(not(test))]
            {
                candidates_with_backlog(
                    session,
                    picks,
                    cursor_value,
                    Some(&backlog),
                    Some(&tracked),
                )(remaining)
            }
            #[cfg(test)]
            {
                let result = candidates_with_backlog(
                    session,
                    picks,
                    cursor_value,
                    Some(&backlog),
                    Some(&tracked),
                )(remaining);
                if result.is_ok() {
                    AFTER_FIRST_FETCH.with(|hook| {
                        if let Some(hook) = hook.borrow_mut().take() {
                            hook();
                        }
                    });
                }
                result
            }
        },
        watch::Scope {
            entered,
            ..picks.scope()?
        },
        condition,
        deadline.saturating_duration_since(Instant::now()),
        LOCAL_POLL,
    )?;
    let mut waited = waited;
    if cursor && waited.met {
        let acknowledged = watch::retry_contention(deadline, LOCAL_POLL, || {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let entry = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let mut latest = backlog
                .borrow()
                .as_ref()
                .map_or(0, |summary| summary.latest_event);
            for lane in waited.lanes.iter().filter(|lane| lane_settled(lane).0) {
                latest = latest.max(session.store.last_event_sequence(lane.attempt_id, &entry)?);
            }
            if latest > 0 {
                open(edge, &entry)?.store.ack_seen(&key, latest, &entry)?;
            }
            Ok(())
        })?;
        if !acknowledged {
            waited.met = false;
        }
    }
    let (mut data, mut human, timed_out) = waited_reply(session, waited)?;
    if let Some(summary) = backlog.into_inner() {
        let (sample, sample_human) = render_settled(session, &summary.newest, None)?;
        data["settled_backlog"] = json!({
            "count": summary.count,
            "newest": sample["lanes"],
        });
        human.push_str(&format!(
            "settled backlog: {} lanes; showing newest {}\n",
            summary.count,
            summary.newest.len()
        ));
        human.push_str(&sample_human);
    }
    Ok((data, human, timed_out))
}

/// One line per call, already newline-terminated, for a watch to stream.
pub(super) type Emit<'a> = &'a mut dyn FnMut(&str) -> Result<(), Fail>;

pub(super) fn watched_reply(
    session: &Session,
    watched: &watch::Watched,
    json: bool,
) -> Result<(Value, String, bool), Fail> {
    let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
    let (mut data, _) = render_settled(session, &watched.lanes, lane_cooldown_view(&cooldowns))?;
    data["condition_met"] = json!(watched.settled);
    if let Some(id) = watched.silent_lane {
        data["timed_out"] = json!(false);
        data["reason"] = json!(format!("provider silent; lane {id} is still running"));
    }
    let human = if json {
        String::new()
    } else {
        watch::end_line(watched)
    };
    Ok((
        data,
        human,
        !watched.settled && watched.silent_lane.is_none(),
    ))
}

/// `lanes --watch`: a line for each state a counted lane enters, until all
/// have settled or the budget passes.
pub(super) fn watch_command(
    session: &Session,
    picks: &Picks<'_>,
    seconds: u64,
    json: bool,
    emit: Emit<'_>,
) -> Result<(Value, String, bool), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let entry = request(&clock, &cancel)?;
    // A watch reports from its start, so it never reads the acknowledged cursor.
    let base = (picks.targets.is_empty() && picks.since.is_none())
        .then(|| {
            let key = format!("watch:{}", selection_key(picks.label));
            session.store.seen_cursor(&key, true, &entry)
        })
        .transpose()?;
    let watched = watch::watch(
        candidates(session, picks, base),
        picks.scope()?,
        Duration::from_secs(seconds),
        LOCAL_POLL,
        &mut |lane| {
            let text = if json {
                watch::json_line_with_successor(lane, None, None)
            } else {
                watch::line_with_successor(lane, None, None)
            };
            emit(&format!("{text}\n"))
        },
    )?;
    watched_reply(session, &watched, json)
}

/// Write one watch line to standard output and flush it, so a pipe or monitor
/// sees each event as it happens.
pub fn write_line(line: &str) -> Result<(), Fail> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|_| Fail::new(Status::Error, ErrorKind::Io, "cannot write watch output"))
}

/// Mixed status waits share lane selection and settlement semantics.
pub fn status_wait_lanes(
    target: &Target,
    edge: &Edge<'_>,
) -> Result<Vec<run::AttemptRecord>, Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let resolved = targets::resolve(edge, target, true)?;
    let (session, target) = targets::read_resolved(edge, resolved, &call)?;
    let targets = [target];
    let picks = Picks {
        targets: &targets,
        owner: session.context.actor_id,
        label: None,
        since: None,
    };
    candidates(&session, &picks, None)(Duration::ZERO)
}
