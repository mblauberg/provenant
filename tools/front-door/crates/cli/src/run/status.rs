use super::*;

fn full_wait_reason(reason: &str) -> String {
    if reason.starts_with("host capacity: disk space:") {
        format!("{reason}; free disk space or run `provenant gc --apply`")
    } else {
        reason.to_owned()
    }
}

pub(super) fn lane_reason(lane: &run::AttemptRecord, waits: &[run::LaneWait]) -> Option<String> {
    state_and_reason(&lane.state).1.or_else(|| {
        waits
            .iter()
            .find(|wait| wait.attempt_id == lane.attempt_id)
            .map(|wait| full_wait_reason(&wait.waiting_for))
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
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = read_session(edge, &call)?;
    let record = session.store.record(id, &call)?;
    Ok(project_lane(
        &record,
        &[],
        session.context.state_root.as_path(),
        clock.wall_now().ok(),
        false,
    ))
}

pub fn project_lane(
    lane: &run::AttemptRecord,
    waits: &[run::LaneWait],
    root: &Path,
    now_wall: Option<WallTime>,
    observe_host: bool,
) -> provenant_records::wire::LaneRow {
    let usage = lane.effective_usage();
    let wait = waits.iter().find(|wait| wait.attempt_id == lane.attempt_id);
    let waiting_for = wait.map(|wait| wait.waiting_for.clone()).or_else(|| {
        lane.provider_silent_for_ms
            .0
            .map(run::provider_silence_reason)
            .or_else(|| {
                (observe_host && matches!(lane.state, run::AttemptState::Starting { .. }))
                    .then(|| "launch preparation".to_owned())
            })
    });
    let transcript = lane
        .applied_route
        .as_ref()
        .and_then(|route| route.provider.as_deref())
        .zip(lane.session.as_deref())
        .map(|(adapter, session)| {
            let attempt = artifact(root, lane, "");
            let path = observe_host
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
    let result_path = observe_host
        .then(|| {
            lane.result
                .as_ref()
                .map(|_| artifact(root, lane, "result.md"))
        })
        .flatten()
        .and_then(|path| provenant_records::StructuredPathWire::from_path(&path).ok());
    let retained = (observe_host
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
            overdue: now_wall.map(|now| run::overdue_at(lane, now)),
            stale: observe_host.then(|| run::holder_absent(root, lane)),
            result_path,
            transcript,
            payload_evicted: observe_host
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
    let session = Session { store, context };
    // Match the normal listing's recovery of a dead guardian before observing
    // the page; the hook reports only records this read actually completed.
    let mut query = lane_query(all, limit, after);
    query.newest_first = all;
    let mut failed = false;
    session.store.lanes_streaming(&query, &call, |total, entry, next| {
        if failed { return; }
        let event = match entry {
            None => json!({"total": total, "next": next}),
            Some((cursor, lane)) => {
                let (state, reason) = state_and_reason(&lane.state);
                let reason = reason.or_else(|| {
                    (!all)
                        .then(|| session.store.lane_waits(std::slice::from_ref(lane), &call))
                        .and_then(|waits| waits.first().map(|wait| wait.waiting_for.clone()))
                });
                let finished = match &lane.state {
                    run::AttemptState::Terminal { finished_at, .. } => Some(*finished_at),
                    _ => None,
                };
                json!({"cursor": cursor, "lane": {
                    "run_id": lane.run_id, "task_id": lane.task_id, "attempt_id": lane.attempt_id,
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
    pub run_id: RunId,
    pub task_id: TaskId,
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
        run_id: record.run_id,
        task_id: record.task_id,
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
    Ok(Session { store, context })
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
