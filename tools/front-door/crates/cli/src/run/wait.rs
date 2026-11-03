use super::*;

pub(super) fn settled(state: &run::AttemptState) -> (bool, bool) {
    match state {
        run::AttemptState::Terminal { outcome, .. } => {
            (true, *outcome == run::Outcome::InputRequired)
        }
        _ => (false, false),
    }
}

/// Whether a lane selection satisfies a wait condition. Input required ends
/// an all-lanes wait because it cannot progress unattended.
pub(super) fn condition_met(lanes: &[run::AttemptRecord], condition: Condition) -> bool {
    let states: Vec<(bool, bool)> = lanes.iter().map(|l| settled(&l.state)).collect();
    states.is_empty()
        || match condition {
            Condition::Any => states.iter().any(|(done, _)| *done),
            Condition::All => {
                states.iter().all(|(done, _)| *done) || states.iter().any(|(_, input)| *input)
            }
        }
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
                    "selection exceeds 1000 lanes",
                ));
            }
            None => return Ok(selected),
        }
    }
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
            _ => select(session, picks.targets, seen, picks.owner, &call)?,
        };
        let mut lanes = Vec::with_capacity(ids.len());
        let mut included = std::collections::BTreeSet::new();
        for id in ids {
            // Settlement may have queued a successor after the selection read.
            // Keep the original receipt and wait for its current policy tail.
            follow_policy_tail::<_, Fail>(
                id,
                |id| {
                    let lane = session.service().status_for_wait(id, &call)?;
                    if included.insert(id)
                        && (picks.label.is_none() || lane.verified_owner == Some(picks.owner))
                    {
                        lanes.push(lane);
                    }
                    Ok(())
                },
                |id| Ok(session.store.policy_tail(id, &call)?.attempt_id),
            )?;
        }
        let waits = session.store.lane_waits(&lanes, &call);
        apply_lane_wait_suppression(&mut lanes, &waits);
        Ok(lanes)
    }
}

/// Visit `id`, then each policy successor after it. Each hop reads the state
/// before the tail: a terminal state commits its successor in the same
/// transaction, so the tail read afterwards cannot miss it. A policy chain holds
/// at most `POLICY_ATTEMPT_CEILING` attempts.
pub(super) fn follow_policy_tail<K: Copy + PartialEq, E>(
    mut id: K,
    mut visit: impl FnMut(K) -> Result<(), E>,
    mut tail: impl FnMut(K) -> Result<K, E>,
) -> Result<(), E> {
    for _ in 0..=provenant_run::POLICY_ATTEMPT_CEILING {
        visit(id)?;
        let next = tail(id)?;
        if next == id {
            break;
        }
        id = next;
    }
    Ok(())
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
    let (mut data, mut human) = render_lanes(
        lanes,
        &waits,
        None,
        session.context.state_root.as_path(),
        cooldowns,
    );
    append_provider_errors(session, lanes, &mut data, &mut human);
    attach_handoffs(session, lanes, &mut data);
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
            candidates(session, picks, cursor_value)(remaining)
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
            let mut latest = 0;
            for lane in waited.lanes.iter().filter(|l| settled(&l.state).0) {
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
    waited_reply(session, waited)
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
                watch::json_line(lane, None)
            } else {
                watch::line(lane, None)
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

/// Mixed status waits share lane selection, settlement and policy-tail semantics.
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
