use super::*;

fn full_wait_reason(reason: &str) -> String {
    reason.to_owned()
}

pub(super) fn lane_reason(lane: &run::AttemptRecord, waits: &[run::LaneWait]) -> Option<String> {
    state_and_reason(&lane.state).1.or_else(|| {
        waits
            .iter()
            .find(|wait| wait.attempt_id == lane.attempt_id)
            .map(|wait| full_wait_reason(&wait.waiting_for.description()))
    })
}

pub(super) fn structured_path_text(value: &Value) -> Option<String> {
    let path: provenant_records::StructuredPathWire = serde_json::from_value(value.clone()).ok()?;
    Some(path.to_path_buf().ok()?.to_string_lossy().into_owned())
}

pub(super) fn lane_query(all: bool, limit: u64, after: u64) -> run::LaneQuery {
    run::LaneQuery {
        targets: Vec::new(),
        all,
        limit,
        after,
        newest_first: false,
        owner: None,
        unseen_after: None,
    }
}

/// Artifact paths live under the session's execution root, which a nested
/// lane keeps apart from the global configuration root.
pub(super) fn artifact(root: &Path, record: &run::AttemptRecord, name: &str) -> PathBuf {
    root.join(format!("attempts/{}/{name}", record.attempt_id))
}

/// The projected state and recorded terminal reason.
pub fn state_and_reason(state: &run::AttemptState) -> (String, Option<String>) {
    provenant_records::wire::LaneRow::state_and_reason(state)
}

/// Read one retained attempt, by the id a contribution recorded, without
/// reaping or changing its history.
pub fn project_retained_attempt(
    id: AttemptId,
    edge: &Edge<'_>,
) -> Result<provenant_records::wire::LaneRow, Fail> {
    let census = provenant_supervisor::census::ProcessObserver::census(
        &provenant_supervisor::census::SystemProcessObserver,
        provenant_supervisor::census::MAX_CENSUS_ENTRIES,
    )
    .ok();
    project_retained_attempt_with_census(id, edge, census.as_ref().ok_or(()))
}

/// Project one retained attempt while sharing a command's bounded host census
/// across a batch of contributions.
pub fn project_retained_attempt_with_census(
    id: AttemptId,
    edge: &Edge<'_>,
    census: std::result::Result<&provenant_supervisor::census::Census, ()>,
) -> Result<provenant_records::wire::LaneRow, Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = read_session(edge, &call)?;
    let record = session.store.record(id, &call)?;
    Ok(project_lane_with_census(
        &record,
        &[],
        session.context.state_root.as_path(),
        clock.wall_now().ok(),
        record.superseded_by.is_none(),
        census,
    ))
}

/// Bounded artifact evidence for the shared status/wait projection. Reads the
/// sealed handoff rather than reparsing a provider transcript.
pub fn status_evidence(
    store: &run::RunStore,
    context: &run::ExecutionContext,
    record: &run::AttemptRecord,
    call: &run::Request<'_>,
) -> Value {
    let provider_error = matches!(
        record.state,
        run::AttemptState::Terminal {
            outcome: run::Outcome::Failed,
            ..
        }
    )
    .then(|| {
        output::provider_error_with(|part, range| {
            store
                .read_output(record.attempt_id, part, range, context, call)
                .ok()
        })
    })
    .flatten();
    let handoff = store.handoff(record).unwrap_or_else(|_| {
        Err(provenant_records::handoff::HandoffError::new(
            "$",
            "handoff snapshot unreadable",
        ))
    });
    json!({"provider_error": provider_error, "handoff": handoff})
}

pub fn project_lane(
    lane: &run::AttemptRecord,
    waits: &[run::LaneWait],
    root: &Path,
    now_wall: Option<WallTime>,
    observe_host: bool,
) -> provenant_records::wire::LaneRow {
    project_lane_inner(
        lane,
        waits,
        root,
        now_wall,
        (HostView::from_observation(observe_host), &[]),
        None,
        false,
    )
}

/// Collect live admission markers only from active lanes' scratch grants.
pub fn active_lane_admission(
    store: &run::RunStore,
    call: &run::Request<'_>,
) -> run::Result<(
    Vec<provenant_os::admission::HeldInfo>,
    Vec<provenant_os::admission::WaitInfo>,
)> {
    let mut query = lane_query(false, 100, 0);
    let mut holders = Vec::new();
    let mut waiters = Vec::new();
    loop {
        let page = store.lanes(&query, call)?;
        for lane in page.lanes {
            if !matches!(lane.state, run::AttemptState::Running { .. }) {
                continue;
            }
            for grant in &lane.write_grants {
                if grant.source == provenant_records::run::WriteSource::Scratch
                    && let Ok(path) = grant.path.to_path_buf()
                {
                    let attempt = lane.attempt_id.to_string();
                    holders.extend(
                        provenant_os::admission::lane_holders(&path)
                            .into_iter()
                            .map(|mut info| {
                                info.attempt_id = Some(attempt.clone());
                                info
                            }),
                    );
                    waiters.extend(
                        provenant_os::admission::lane_waiters(&path)
                            .into_iter()
                            .map(|mut info| {
                                info.attempt_id = Some(attempt.clone());
                                info
                            }),
                    );
                }
            }
        }
        match page.next {
            Some(next) => query.after = next,
            None => break,
        }
    }
    Ok((holders, waiters))
}

/// Ids of every live (non-terminal) lane, paged with the same bounded query as
/// [`active_lane_admission`]. A read-only caller uses this before a release
/// swap to name attempts that may still settle under the new format.
pub fn live_attempt_ids(
    store: &run::RunStore,
    call: &run::Request<'_>,
) -> run::Result<Vec<String>> {
    let mut query = lane_query(false, 100, 0);
    let mut ids = Vec::new();
    loop {
        let page = store.lanes(&query, call)?;
        ids.extend(
            page.lanes
                .into_iter()
                .map(|lane| lane.attempt_id.to_string()),
        );
        match page.next {
            Some(next) => query.after = next,
            None => break,
        }
    }
    Ok(ids)
}

/// Project a status row using one bounded process census shared by the caller.
/// `Err(())` records that census was unavailable, so each live attempt stays
/// explicitly unknown instead of triggering one fresh host scan per row,
/// while a sealed exit can still report settlement pending. A false
/// `observe_host` also stays unknown for running attempts because this host
/// cannot prove a remote writer dead or live.
pub fn project_lane_with_census(
    lane: &run::AttemptRecord,
    waits: &[run::LaneWait],
    root: &Path,
    now_wall: Option<WallTime>,
    observe_host: bool,
    census: std::result::Result<&provenant_supervisor::census::Census, ()>,
) -> provenant_records::wire::LaneRow {
    project_lane_inner(
        lane,
        waits,
        root,
        now_wall,
        (HostView::from_observation(observe_host), &[]),
        Some(census),
        true,
    )
}

pub fn project_lane_with_admission(
    lane: &run::AttemptRecord,
    waits: &[run::LaneWait],
    root: &Path,
    now_wall: Option<WallTime>,
    observe_host: bool,
    census: std::result::Result<&provenant_supervisor::census::Census, ()>,
    holders: &[provenant_os::admission::HeldInfo],
) -> provenant_records::wire::LaneRow {
    project_lane_inner(
        lane,
        waits,
        root,
        now_wall,
        (HostView::from_observation(observe_host), holders),
        Some(census),
        true,
    )
}

/// Project a row for a reader that may be on another host. Presence comes from
/// this host's sealed evidence without a census, so a finished attempt reads
/// as settling, but no host-local artefact path or host file fact is
/// projected: the row may cross a peer boundary.
pub fn project_lane_presence_only(
    lane: &run::AttemptRecord,
    root: &Path,
    now_wall: Option<WallTime>,
) -> provenant_records::wire::LaneRow {
    project_lane_inner(
        lane,
        &[],
        root,
        now_wall,
        (HostView::PresenceOnly, &[]),
        Some(Err(())),
        true,
    )
}

/// What a lane projection may read from, and publish about, this host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostView {
    /// Nothing: running attempts stay unknown and no host path is projected.
    Unobserved,
    /// Presence evidence only; host-local paths and file facts stay private.
    PresenceOnly,
    /// Presence plus host-local artefact paths for a caller on this host.
    Local,
}

impl HostView {
    fn from_observation(observe_host: bool) -> Self {
        if observe_host {
            Self::Local
        } else {
            Self::Unobserved
        }
    }

    fn observes_presence(self) -> bool {
        !matches!(self, Self::Unobserved)
    }

    fn projects_host_artifacts(self) -> bool {
        matches!(self, Self::Local)
    }
}

pub fn admission_holder_description(h: &provenant_os::admission::HeldInfo) -> String {
    let base = match &h.attempt_id {
        Some(attempt) => format!("slot {}: attempt {} (pid {})", h.slot, attempt, h.pid),
        None => format!("slot {}: pid {}", h.slot, h.pid),
    };
    match &h.command {
        // The holder's masked argv names the command actually blocking the
        // slot, so a lock wait is distinguishable from a hung writer.
        Some(command) => format!("{base} running {command}"),
        None => base,
    }
}

fn project_lane_inner(
    lane: &run::AttemptRecord,
    waits: &[run::LaneWait],
    root: &Path,
    now_wall: Option<WallTime>,
    host: (HostView, &[provenant_os::admission::HeldInfo]),
    census: Option<std::result::Result<&provenant_supervisor::census::Census, ()>>,
    unknown_when_host_unobserved: bool,
) -> provenant_records::wire::LaneRow {
    let (view, lane_holders) = host;
    let usage = lane.effective_usage();
    let host_artifacts = view.projects_host_artifacts();
    let presence = if !view.observes_presence() {
        (unknown_when_host_unobserved && matches!(lane.state, run::AttemptState::Running { .. }))
            .then_some(provenant_records::wire::LanePresence::Unknown)
    } else {
        let observed = match census {
            Some(Ok(census)) => run::attempt_presence_with_census(root, lane, census),
            Some(Err(())) => run::attempt_presence_with_unavailable_census(root, lane),
            None => run::attempt_presence(root, lane),
        };
        observed.map(|presence| match presence {
            run::AttemptPresence::Live => provenant_records::wire::LanePresence::Live,
            run::AttemptPresence::Unattended => provenant_records::wire::LanePresence::Unattended,
            run::AttemptPresence::Unknown => provenant_records::wire::LanePresence::Unknown,
            run::AttemptPresence::Settling => provenant_records::wire::LanePresence::Settling,
        })
    };
    let stale = presence.and_then(|presence| match presence {
        provenant_records::wire::LanePresence::Live => Some(false),
        provenant_records::wire::LanePresence::Unattended
        | provenant_records::wire::LanePresence::Settling => Some(true),
        provenant_records::wire::LanePresence::Unknown => None,
    });
    let wait = waits.iter().find(|wait| wait.attempt_id == lane.attempt_id);
    let admission_wait =
        if host_artifacts && matches!(lane.state, run::AttemptState::Running { .. }) {
            lane.write_grants.iter().find_map(|grant| {
                if grant.source == provenant_records::run::WriteSource::Scratch
                    && let Ok(path) = grant.path.to_path_buf()
                {
                    return provenant_os::admission::lane_waiters(&path)
                        .into_iter()
                        .next();
                }
                None
            })
        } else {
            None
        };
    // The shim records the masked argv before publishing a wait or hold. A
    // lane that is waiting names the command it waits to run; a lane that owns
    // the slot names the command it is running now.
    let waiting_command = admission_wait
        .as_ref()
        .and_then(|wait_info| wait_info.command.clone());
    let holding_command = || {
        let attempt = lane.attempt_id.to_string();
        lane_holders
            .iter()
            .find(|holder| holder.attempt_id.as_deref() == Some(attempt.as_str()))
            .and_then(|holder| holder.command.clone())
    };
    let child_command = host_artifacts
        .then(|| waiting_command.clone().or_else(holding_command))
        .flatten();
    let waiting_for = admission_wait
        .map(|wait_info| {
            let now_sec = now_wall.map_or_else(
                || {
                    use provenant_core::Clock;
                    provenant_os::clock::SystemClock::new()
                        .unix_seconds()
                        .max(0) as u64
                },
                |w| w.unix_seconds().max(0) as u64,
            );
            let elapsed = now_sec.saturating_sub(wait_info.start_unix_seconds);
            let mut holders = provenant_os::admission::host_holders(root, &wait_info.class);
            holders.extend(
                lane_holders
                    .iter()
                    .filter(|h| h.class == wait_info.class)
                    .cloned(),
            );
            if holders.is_empty() {
                format!("waiting for a host {} slot ({}s)", wait_info.class, elapsed)
            } else {
                let holders_desc = holders
                    .iter()
                    .map(admission_holder_description)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "waiting for a host {} slot ({}s; held by {})",
                    wait_info.class, elapsed, holders_desc
                )
            }
        })
        .or_else(|| wait.map(|wait| wait.waiting_for.description()))
        .or_else(|| {
            (host_artifacts && matches!(lane.state, run::AttemptState::Starting { .. }))
                .then(|| "launch preparation".to_owned())
        });
    let transcript = lane
        .applied_route
        .as_ref()
        .and_then(|route| route.provider.as_deref())
        .zip(lane.session.as_deref())
        .map(|(adapter, session)| {
            let attempt = artifact(root, lane, "");
            let path = host_artifacts
                .then(|| run::locate_transcript(&attempt, session, &lane.write_grants))
                .flatten()
                .map(|path| attempt.join(path));
            provenant_records::wire::LaneTranscript {
                adapter: adapter.to_owned(),
                session: session.to_owned(),
                path: path
                    .and_then(|path| provenant_records::StructuredPathWire::from_path(&path).ok()),
            }
        });
    let result_path = host_artifacts
        .then(|| {
            lane.result
                .as_ref()
                .map(|_| artifact(root, lane, "result.md"))
        })
        .flatten()
        .and_then(|path| provenant_records::StructuredPathWire::from_path(&path).ok());
    // The directory is a host-local artefact path: peer rows project no path.
    let attempt_directory = host_artifacts
        .then(|| root.join("attempts").join(lane.attempt_id.to_string()))
        .and_then(|path| provenant_records::StructuredPathWire::from_path(&path).ok());
    // There is no wall-clock column on a run event; the newest observed
    // activity is the last output, else the launch, else the queue time. Carry
    // which fallback was used so a quiet writer's old output is not read as a
    // fresh heartbeat.
    let (last_event_at, last_event_source) = lane
        .last_output_at
        .map(|at| (at, provenant_records::wire::LastEventSource::Output))
        .or_else(|| {
            lane.started_at
                .map(|at| (at, provenant_records::wire::LastEventSource::Started))
        })
        .unwrap_or((
            lane.queued_at,
            provenant_records::wire::LastEventSource::Queued,
        ));
    let retained = (host_artifacts
        && lane.settlement_receipts.as_ref().is_some_and(|receipts| {
            matches!(
                receipts.git_import,
                run::EffectDisposition::Failed {
                    retryable: false,
                    ..
                }
            )
        }))
    .then(|| run::retained_git(root, lane).ok())
    .flatten();
    let mut projected = provenant_records::wire::LaneRow::project(
        lane,
        provenant_records::wire::LaneProjectionFacts {
            superseded_by: lane.superseded_by,
            git_import_head: retained
                .as_ref()
                .and_then(|git| git.head)
                .or(lane.head)
                .map(|head| head.to_string()),
            retained_git_objects: retained
                .and_then(|git| git.objects)
                .and_then(|path| provenant_records::StructuredPathWire::from_path(&path).ok()),
            waiting_for,
            lane_class: wait.map(|wait| wait.lane_class.as_str().to_owned()),
            last_event_at: Some(last_event_at),
            last_event_source: Some(last_event_source),
            attempt_directory,
            child_command,
            overdue: now_wall.map(|now| run::overdue_at(lane, now)),
            stale,
            presence,
            result_path,
            transcript,
            payload_evicted: host_artifacts
                && artifact(
                    root,
                    lane,
                    provenant_records::AttemptFile::PayloadEvicted.name(),
                )
                .exists(),
            cache_read_share: provenant_routing::cache_read_share(
                usage.uncached_input,
                usage.cache_read,
                usage.cache_write,
            ),
            ..Default::default()
        },
    );
    if let run::AttemptState::Terminal {
        terminal_cause: Some(run::TerminalCause::HandoffRefused),
        reason: Some(reason),
        ..
    } = &lane.state
    {
        projected.reason = Some(format!(
            "{}; inspect the cause and retry the lane",
            provenant_redact::bounded_line(reason, 256)
        ));
    }
    if host_artifacts
        && let Some(releasing) =
            run::release_progress(root, lane, census.and_then(|census| census.ok()), now_wall)
    {
        let outcome = state_and_reason(&lane.state).0;
        projected.reason = Some(match projected.reason.take() {
            Some(reason) => format!("{outcome}; {releasing}; {reason}"),
            None => format!("{outcome}; {releasing}"),
        });
    }
    if host_artifacts && let Some(blocked) = run::settlement::diagnostic_for(root, lane, now_wall) {
        projected.reason = Some(match projected.reason.take() {
            Some(reason) => format!("{reason}; {blocked}"),
            None => blocked,
        });
    }
    if projected.reason == projected.waiting_for {
        projected.reason = projected.waiting_for.as_deref().map(full_wait_reason);
    }
    projected
}

/// A read-only page for the hook. Emit each record before reading the next so
/// a timed-out child still leaves complete, bounded records in its output.
/// Bounded session inventory moved from the retired `lanes` command to `status`.
pub fn status_session_stream(
    command: crate::Status,
    edge: &Edge<'_>,
    out: &mut dyn std::io::Write,
) -> Reply {
    let result = session_inventory_page(
        command.all,
        command.limit,
        command.cursor.unwrap_or(0),
        edge,
        &mut |line| {
            out.write_all(line.as_bytes())
                .and_then(|_| out.flush())
                .map_err(|_| {
                    Fail::new(
                        Status::Error,
                        ErrorKind::Io,
                        "cannot write session attempt stream",
                    )
                })
        },
    );
    let mut response = reply("status", result);
    response.read_only = true;
    response
}

fn session_inventory_page(
    all: bool,
    limit: u64,
    after: u64,
    edge: &Edge<'_>,
    out: &mut dyn FnMut(&str) -> Result<(), Fail>,
) -> Result<(Value, String), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let (context, store) = open_read_only(edge, &call)?;
    let Some(store) = store else {
        out("{\"total\":0,\"next\":null}\n")?;
        return Ok((json!({}), String::new()));
    };
    let session = Session {
        store,
        context,
        chair: None,
    };
    // One bounded host census serves the entire read-only page. If unavailable,
    // rows remain Unknown and no read queues recovery or guesses from a PID.
    let census = provenant_supervisor::census::ProcessObserver::census(
        &provenant_supervisor::census::SystemProcessObserver,
        provenant_supervisor::census::MAX_CENSUS_ENTRIES,
    )
    .ok();
    let mut query = lane_query(all, limit, after);
    query.newest_first = all;
    let mut failed = false;
    session.store.lanes_streaming(&query, &call, |total, entry, next| {
        if failed { return; }
        let event = match entry {
            None => json!({"total": total, "next": next}),
            Some((cursor, lane)) => {
                let waits = if all {
                    Vec::new()
                } else {
                    session.store.lane_waits(std::slice::from_ref(lane), &call)
                };
                let projected = project_lane_with_census(
                    lane,
                    &waits,
                    session.context.state_root.as_path(),
                    clock.wall_now().ok(),
                    lane.superseded_by.is_none(),
                    census.as_ref().ok_or(()),
                );
                let state = projected.state;
                let reason = projected.reason;
                let finished = match &lane.state {
                    run::AttemptState::Terminal { finished_at, .. } => Some(*finished_at),
                    _ => None,
                };
                json!({"cursor": cursor, "lane": {
                    "key": lane.key, "attempt_id": lane.attempt_id,
                    "state": state, "reason": reason, "finished_at": finished,
                    "requested_route": lane.requested_route, "applied_route": lane.applied_route,
                    "delivery": lane.delivery,
                }})
            }
        };
        failed = out(&format!("{event}\n")).is_err();
    })?;
    if failed {
        return Err(Fail::new(
            Status::Error,
            ErrorKind::Io,
            "cannot write session lane stream",
        ));
    }
    Ok((json!({}), String::new()))
}

/// What a pull request body needs from one local attempt's own record.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptFacts {
    pub attempt_id: AttemptId,
    pub state: String,
    pub exit_code: Option<i64>,
    /// `adapter/model@effort`, as the attempt ran.
    pub route: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    pub head: Option<provenant_core::GitOid>,
    /// The branch the attempt's host delivery pushed.
    pub branch: Option<String>,
    pub delivery_id: Option<DeliveryId>,
    /// The attempt's receipt directory, relative to the state root.
    pub receipt_path: String,
    /// The attempt's delivery receipt: risk tier and review attempts.
    pub risk: Option<String>,
    pub reviews: Vec<AttemptId>,
}

pub(super) fn route_label(record: &run::AttemptRecord) -> String {
    let ran = record
        .observed_route
        .as_ref()
        .or(record.applied_route.as_ref())
        .or(record.resolved_route.as_ref());
    if let run::RequestedRoute::Adapter { provider, name, .. } = &record.requested_route {
        return format!("{provider}:{name}");
    }
    let (requested_model, requested_effort) = match &record.requested_route {
        run::RequestedRoute::Adapter { .. } => (None, None),
        run::RequestedRoute::Model { model, effort, .. } => (Some(model.clone()), effort.clone()),
        run::RequestedRoute::Named { effort, .. } | run::RequestedRoute::Review { effort, .. } => {
            (None, effort.clone())
        }
    };
    let Some(model) = ran
        .and_then(|route| {
            let model = route.model.clone()?;
            Some(match &route.provider {
                Some(provider) if !model.starts_with(&format!("{provider}/")) => {
                    format!("{provider}/{model}")
                }
                _ => model,
            })
        })
        .or(requested_model)
    else {
        return "unresolved".into();
    };
    match ran
        .and_then(|route| route.effort.clone())
        .or(requested_effort)
    {
        Some(effort) => format!("{model}@{effort}"),
        None => model,
    }
}

/// Receipt facts shared by explicit PR opening and graph delivery. Workflow
/// review receipts, when present, are added by the caller that owns that store.
pub fn recorded_attempt_facts(record: &run::AttemptRecord) -> AttemptFacts {
    let lane = project_lane(record, &[], Path::new("/"), None, false);
    let state = lane.state;
    let exit_code = lane.exit_code;
    AttemptFacts {
        attempt_id: record.attempt_id,
        state,
        exit_code,
        route: route_label(record),
        role: record
            .context
            .as_ref()
            .and_then(|context| context.role.clone()),
        verdict: None,
        head: record
            .head
            .or_else(|| record.delivery.as_ref().and_then(|d| d.head)),
        branch: record.delivery.as_ref().map(|d| d.branch.clone()),
        delivery_id: record.delivery_id,
        receipt_path: format!("attempts/{}", record.attempt_id),
        risk: None,
        reviews: Vec::new(),
    }
}

/// The recorded facts of local attempts, for the pull request body. An attempt
/// that ran on a peer is `NOT_FOUND` here: its record lives on that host.
pub fn attempt_facts(edge: &Edge<'_>, targets: &[Target]) -> Result<Vec<AttemptFacts>, Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let resolved = targets
        .iter()
        .map(|target| key::resolve_target(edge, target, crate::Want::Attempt { settled: true }))
        .collect::<Result<Vec<_>, _>>()?;
    let session = read_session(edge, &call)?;
    resolved
        .iter()
        .map(|resolved| {
            let id = key::local_attempt(&session, resolved, &call)?;
            facts_of(&session, id, &call)
        })
        .collect()
}

/// The recorded facts of the review attempts a delivery receipt names.
pub fn review_attempt_facts(edge: &Edge<'_>, ids: &[AttemptId]) -> Result<Vec<AttemptFacts>, Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = read_session(edge, &call)?;
    ids.iter()
        .map(|id| facts_of(&session, *id, &call))
        .collect()
}

fn read_session(edge: &Edge<'_>, call: &run::Request<'_>) -> Result<Session, Fail> {
    let (context, store) = open_read_only(edge, call)?;
    let store = store.ok_or_else(|| Fail::from(run::RunError::NotFound))?;
    Ok(Session {
        store,
        context,
        chair: None,
    })
}

fn facts_of(
    session: &Session,
    id: AttemptId,
    call: &run::Request<'_>,
) -> Result<AttemptFacts, Fail> {
    let record = session.service().status(id, call)?;
    let (risk, reviews) = match record.delivery_id {
        Some(delivery) => {
            let receipt = provenant_workflow::store::ReceiptStore::new(
                session.context.state_root.clone(),
                session.context.project_id,
                delivery,
            )
            .read()
            .map_err(|error| match error {
                provenant_workflow::store::StoreError::Retired { .. } => {
                    Fail::new(Status::Refused, ErrorKind::Invalid, error.to_string())
                }
                error => Fail::new(Status::Error, ErrorKind::StateCorrupt, error.to_string()),
            })?;
            match receipt {
                Some(receipt) => (
                    serde_json::to_value(receipt.risk().tier())
                        .ok()
                        .and_then(|tier| tier.as_str().map(str::to_owned)),
                    receipt.review_attempts().to_vec(),
                ),
                None => (None, Vec::new()),
            }
        }
        None => (None, Vec::new()),
    };
    let mut facts = recorded_attempt_facts(&record);
    facts.risk = risk;
    facts.reviews = reviews;
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::wire::LastEventSource;

    const ID: &str = "11111111111111111111111111111111";

    /// A running attempt whose only write grant is a lane scratch directory.
    fn running_lane(scratch: &Path) -> run::AttemptRecord {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let mut record: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": ID,
            "project_id": ID,
            "key": {"project": ID, "graph": "graph", "node": "node", "attempt": 1},
            "attempt_number": 1,
            "revision": 1,
            "generation": 1,
            "requested_route": {"kind": "named", "name": "senior"},
            "authority_digest": digest,
            "queued_at": "2026-10-29T00:00:00.000000Z",
            "state": {"state": "running"},
            "usage": {},
            "read_only_writable": [],
            "protected_paths": [],
            "trace": {},
            "prompt": {
                "path": {"encoding": "utf8", "path": "prompt.md"},
                "digest": digest,
                "bytes": 0,
                "complete": true,
                "redacted": false
            }
        }))
        .expect("running attempt fixture");
        record.write_grants.push(run::WriteGrantRecord {
            extra: Default::default(),
            source: provenant_records::run::WriteSource::Scratch,
            path: provenant_records::AbsolutePath::new(scratch).expect("scratch path"),
            scope: provenant_records::run::PathScope::Subtree,
        });
        record
    }

    // Spec: docs/specs/cli.md#status-selection
    #[test]
    fn project_lane_inner_keeps_host_artifacts_private_from_peers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scratch = temp.path().join("scratch");
        std::fs::create_dir_all(scratch.join(provenant_os::admission::SHIM_DIRECTORY))
            .expect("shim directory");
        let _waiting = provenant_os::admission::mark_waiting_with_command(
            &scratch,
            "build",
            43,
            Some(ID),
            Some("cargo test --locked".into()),
        )
        .expect("waiting marker");
        let lane = running_lane(&scratch);

        let host = project_lane_with_census(&lane, &[], temp.path(), None, true, Err(()));
        assert!(host.attempt_directory.is_some(), "{host:?}");
        assert_eq!(host.child_command.as_deref(), Some("cargo test --locked"));

        let peer = project_lane_presence_only(&lane, temp.path(), None);
        assert!(peer.attempt_directory.is_none(), "{peer:?}");
        assert!(peer.child_command.is_none(), "{peer:?}");
    }

    // Spec: docs/specs/cli.md#status-selection
    #[test]
    fn admission_wait_and_hold_commands_resolve_for_waiting_and_holding_lanes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let waiting_scratch = temp.path().join("waiting");
        std::fs::create_dir_all(waiting_scratch.join(provenant_os::admission::SHIM_DIRECTORY))
            .expect("shim directory");
        let _waiting = provenant_os::admission::mark_waiting_with_command(
            &waiting_scratch,
            "build",
            43,
            Some(ID),
            Some("cargo test --locked".into()),
        )
        .expect("waiting marker");
        let waiter = running_lane(&waiting_scratch);
        let waiting = project_lane_with_census(&waiter, &[], temp.path(), None, true, Err(()));
        assert_eq!(
            waiting.child_command.as_deref(),
            Some("cargo test --locked"),
            "{waiting:?}"
        );
        assert!(
            waiting
                .waiting_for
                .as_deref()
                .is_some_and(|reason| reason.contains("host build slot")),
            "{waiting:?}"
        );

        let holding_scratch = temp.path().join("holding");
        std::fs::create_dir_all(holding_scratch.join(provenant_os::admission::SHIM_DIRECTORY))
            .expect("shim directory");
        let holder_record = running_lane(&holding_scratch);
        let holders = [provenant_os::admission::HeldInfo {
            class: "build".into(),
            slot: 2,
            pid: 42,
            start_unix_seconds: 0,
            attempt_id: Some(ID.into()),
            command: Some("cargo build --release".into()),
        }];
        let holding = project_lane_with_admission(
            &holder_record,
            &[],
            temp.path(),
            None,
            true,
            Err(()),
            &holders,
        );
        assert_eq!(
            holding.child_command.as_deref(),
            Some("cargo build --release"),
            "{holding:?}"
        );
    }

    // Spec: docs/specs/cli.md#status-selection
    #[test]
    fn last_event_source_names_which_fallback_supplied_the_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scratch = temp.path().join("scratch");
        let mut lane = running_lane(&scratch);
        lane.last_output_at = Some(WallTime::from_micros(2_000_000).expect("output time"));
        lane.started_at = Some(WallTime::from_micros(1_000_000).expect("start time"));
        let output = project_lane(&lane, &[], temp.path(), None, false);
        assert_eq!(output.last_event_source, Some(LastEventSource::Output));

        lane.last_output_at = None;
        let started = project_lane(&lane, &[], temp.path(), None, false);
        assert_eq!(started.last_event_source, Some(LastEventSource::Started));

        lane.started_at = None;
        let queued = project_lane(&lane, &[], temp.path(), None, false);
        assert_eq!(queued.last_event_source, Some(LastEventSource::Queued));
    }
}
