use super::*;

/// Active attempts plus the twenty most recent settled lanes.
pub fn status(command: crate::Status, _json_output: bool, edge: &Edge<'_>) -> Reply {
    let mut summary = None;
    let mut out = reply(
        "status",
        (|| {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            if let Some(graph_id) = command.id.as_deref().filter(|id| graph_identifier(id))
                && let Some((data, human)) = paused_graph_status(edge, &call, graph_id)?
            {
                return Ok((data, human));
            }
            let id_target = command
                .id
                .as_deref()
                .map(|id| {
                    id.parse::<Target>()
                        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error))
                })
                .transpose()?;
            let resolved = id_target
                .as_ref()
                .map(|target| targets::resolve(edge, target, true))
                .transpose()?;
            let targets: Vec<Target> = resolved
                .as_ref()
                .map(|candidate| vec![candidate.target.clone()])
                .unwrap_or_default();
            let (context, store) = if let Some(candidate) = resolved {
                let (session, _) = targets::read_resolved(edge, candidate, &call)?;
                (session.context, Some(session.store))
            } else {
                open_read_only(edge, &call)?
            };
            let (exempt, exempt_line) = review_exempt_view(&context);
            let native = if command.id.is_none() {
                Some(provenant_telemetry::native_attempts::read(edge.roots))
            } else {
                None
            };
            let Some(store) = store else {
                let mut data = json!({"lanes": [], "next_cursor": null, "review_exempt": exempt});
                let mut human = format!("no lanes\n{exempt_line}");
                append_native_status(native, &mut data, &mut human);
                return Ok((data, human));
            };
            let session = Session { store, context };
            let _ = session.store.reap(&call);
            let full = command.all || command.id.is_some();
            let mut query = lane_query(
                &session,
                &targets,
                full,
                if full { command.limit } else { 100 },
                command.cursor.unwrap_or(0),
                &call,
            )?;
            query.newest_first = true;
            let mut rows = Vec::new();
            let mut next = None;
            if full {
                let page = session.service().lanes(&query, &call)?;
                rows = page.lanes;
                next = page.next;
            } else {
                // Active lanes remain visible even when old; settled history
                // needs only enough pages to find the twenty newest results.
                loop {
                    let page = session.service().lanes(&query, &call)?;
                    rows.extend(page.lanes);
                    match page.next {
                        Some(after) => query.after = after,
                        None => break,
                    }
                }
                query.all = true;
                query.after = 0;
                let mut recent = 0;
                loop {
                    let page = session.service().lanes(&query, &call)?;
                    for lane in page.lanes {
                        if settled(&lane.state).0 {
                            rows.push(lane);
                            recent += 1;
                            if recent == 20 {
                                break;
                            }
                        }
                    }
                    if recent == 20 {
                        break;
                    }
                    match page.next {
                        Some(after) => query.after = after,
                        None => break,
                    }
                }
                rows.sort_by_key(|lane| std::cmp::Reverse(lane.queued_at));
            }
            cloud_owner::hydrate(&session.store, &mut rows, &call)
                .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
            let waits = session.store.lane_waits(&rows, &call);
            let now = now(&clock)?;
            let human = if command.all {
                let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
                render_lanes(
                    &rows,
                    &waits,
                    next,
                    session.context.state_root.as_path(),
                    lane_cooldown_view(&cooldowns),
                )
                .1
            } else {
                compact_status(&rows, &waits, now)
            };
            let continuation = next.map(|cursor| {
                let target = targets
                    .first()
                    .map(|target| {
                        [
                            target.run.map(|id| id.to_string()),
                            target.task.map(|id| id.to_string()),
                            target.attempt.map(|id| id.to_string()),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("/")
                    })
                    .unwrap_or_default();
                format!("provenant status {target} --all --cursor {cursor}")
            });
            let lines: Vec<LaneLine> = rows
                .iter()
                .map(|lane| {
                    let (_, reason) = state_and_reason(&lane.state);
                    let state = lane_display_state(lane);
                    let reason = reason.or_else(|| {
                        waits
                            .iter()
                            .find(|wait| wait.attempt_id == lane.attempt_id)
                            .map(|wait| wait.waiting_for.clone())
                    });
                    LaneLine {
                        id: lane.attempt_id.to_string(),
                        state,
                        model: lane
                            .applied_route
                            .as_ref()
                            .and_then(|route| route.model.clone())
                            .unwrap_or_else(|| "unresolved".into()),
                        reason: reason.unwrap_or_default(),
                        provider_error: None,
                        result_path: None,
                    }
                })
                .collect();
            let mut compact = lane_summary(&lines, continuation);
            // The compact view keeps the paths a writer may change unreviewed.
            compact.lines.push(exempt_line.trim_end().to_owned());
            let mut human = format!("{human}{exempt_line}");
            let successors = policy_successors(&session, &rows, &call)?;
            let (mut data, _) = render_lanes_with_lineage(
                &rows,
                &waits,
                next,
                session.context.state_root.as_path(),
                None,
                &successors,
            );
            if let Some(object) = data.as_object_mut() {
                object.remove("capacity_cooldowns");
            }
            attach_capability_sources(&session, &rows, &mut data, &call);
            let capabilities = capability_sources_text(&data);
            compact
                .lines
                .extend(capabilities.lines().take(8).map(str::to_owned));
            summary = Some(compact);
            human.push_str(&capabilities);
            data["review_exempt"] = exempt;
            attach_cloud_jobs(&session.store, &rows, &mut data, &mut human, &call)?;
            if append_native_status(native, &mut data, &mut human) {
                summary = None;
            }
            if data.get("cloud_jobs").is_some() {
                summary = None;
            }
            Ok((data, human))
        })(),
    );
    out.summary = summary;
    out.read_only = true;
    out
}

fn append_native_status(
    attempts: Option<
        Result<
            Vec<provenant_telemetry::native_attempts::Attempt>,
            provenant_telemetry::native_attempts::AttemptError,
        >,
    >,
    data: &mut Value,
    human: &mut String,
) -> bool {
    match attempts {
        Some(Ok(attempts)) if !attempts.is_empty() => {
            human.push_str("native attempts (host):\n");
            for attempt in &attempts {
                let state = if attempt.ended_at.is_some() {
                    "settled"
                } else {
                    "running"
                };
                human.push_str(&format!(
                    "  {} {} {} {}\n",
                    attempt.id, attempt.alias, attempt.model, state
                ));
            }
            data["native_attempts"] = json!(attempts);
            true
        }
        Some(Err(error)) => {
            human.push_str(&format!("native attempts unavailable: {error}\n"));
            data["native_attempts_error"] = json!(error.to_string());
            true
        }
        _ => false,
    }
}

pub(super) fn attach_capability_sources(
    session: &Session,
    rows: &[run::AttemptRecord],
    data: &mut Value,
    call: &run::Request<'_>,
) {
    let Some(output) = data["lanes"].as_array_mut() else {
        return;
    };
    for (lane, row) in rows.iter().zip(output) {
        if let Ok(request) = session.store.request(lane.run_id, call)
            && let Some(task) = request
                .tasks
                .iter()
                .find(|task| task.task_id == lane.task_id)
            && let Some(sources) = task.access.extra.get("capability_sources")
        {
            row["capabilities"] = sources.clone();
        }
    }
}

fn capability_sources_text(data: &Value) -> String {
    let mut text = String::new();
    for lane in data["lanes"].as_array().into_iter().flatten() {
        for capability in lane["capabilities"].as_array().into_iter().flatten() {
            text.push_str(&format!(
                "{}: capability {} ({})\n",
                plain_text(lane["attempt_id"].as_str().unwrap_or("-"), 128),
                plain_text(capability["name"].as_str().unwrap_or("-"), 128),
                plain_text(capability["source"].as_str().unwrap_or("-"), 128)
            ));
        }
    }
    text
}

fn paused_graph_status(
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    graph_id: &str,
) -> Result<Option<(serde_json::Value, String)>, Fail> {
    use provenant_fabric_runtime::graph::FileJournal;
    let (context, _) = open_read_only(edge, call)?;
    let checkpoint =
        match FileJournal::read_checkpoint(&context.state_root, context.project_id, graph_id) {
            Ok(Some(checkpoint)) => checkpoint,
            Ok(None) => return Ok(None),
            Err(error) => {
                return Err(Fail::new(Status::Error, ErrorKind::Io, error.to_string()));
            }
        };
    Ok(paused_graph_reply(graph_id, checkpoint))
}

/// Any valid graph id is probed first; a graph checkpoint wins over a lane
/// target with the same text, and a missing checkpoint falls back to lanes.
fn graph_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn paused_graph_reply(
    graph_id: &str,
    checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Option<(serde_json::Value, String)> {
    use provenant_fabric_runtime::graph::NodeState;
    let unfinished =
        checkpoint.nodes.iter().any(|node| {
            node.expansion_pause.is_some()
                || matches!(node.state, NodeState::Pending | NodeState::Running)
        }) || (checkpoint.delivery_required && !checkpoint.delivered && checkpoint.successful());
    let status = if checkpoint.cancelled {
        "cancelled"
    } else if !unfinished {
        "complete"
    } else if checkpoint.pause_reason.is_some() {
        "paused"
    } else {
        "pending"
    };
    let label = match status {
        "cancelled" => "Cancelled",
        "complete" => "Complete",
        "paused" => "Paused",
        _ => "Pending",
    };
    let mut human = format!("Graph {graph_id}: {label}\n");
    let reason = (status == "paused")
        .then_some(checkpoint.pause_reason.as_deref())
        .flatten()
        .map(|reason| provenant_fabric_runtime::hosts::plain_text(reason, 1024));
    if let Some(reason) = &reason {
        human.push_str(&format!("Pause reason: {reason}\n"));
    }
    let cancel_reason = (status == "cancelled")
        .then_some(checkpoint.cancel_reason.as_deref())
        .flatten()
        .map(|reason| provenant_fabric_runtime::hosts::plain_text(reason, 1024));
    if let Some(reason) = &cancel_reason {
        human.push_str(&format!("Cancellation reason: {reason}\n"));
    }
    Some((
        json!({"graph_id": graph_id, "status": status, "pause_reason": reason, "cancel_reason": cancel_reason, "checkpoint": checkpoint}),
        human,
    ))
}

#[cfg(test)]
mod graph_status_tests {
    #[test]
    fn graph_target_status_shows_cancellation_reason() {
        let checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(serde_json::json!({
                "schema_version": 1, "graph_id": "graph-123", "plan_digest": "digest",
                "nodes": [], "cancelled": true, "cancel_reason": "requested by chair"
            }))
            .unwrap();
        let (data, human) = super::paused_graph_reply("graph-123", checkpoint).unwrap();
        assert_eq!(data["cancel_reason"], "requested by chair");
        assert!(human.contains("Cancellation reason: requested by chair"));
    }

    #[test]
    fn graph_target_status_shows_a_bounded_sanitised_pause_reason() {
        let reason = format!("invalid planner fragment \u{1b}[31m{}", "x".repeat(2048));
        let checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "graph_id": "graph-123",
                "plan_digest": "digest",
                "delivery_required": true,
                "nodes": [],
                "pause_reason": reason,
            }))
            .unwrap();
        let (data, human) = super::paused_graph_reply("graph-123", checkpoint).unwrap();
        assert!(
            human.starts_with(
                "Graph graph-123: Paused\nPause reason: invalid planner fragment [31m"
            )
        );
        assert!(!human.contains('\u{1b}'));
        assert!(human.lines().nth(1).unwrap().chars().count() <= 1024 + "Pause reason: ".len());
        assert_eq!(data["status"], "paused");
        assert_eq!(data["pause_reason"].as_str().unwrap().len(), 1024);
    }
}

/// One lane as the compact views show it.
pub(super) struct LaneLine {
    id: String,
    state: String,
    model: String,
    reason: String,
    provider_error: Option<String>,
    result_path: Option<String>,
}

/// Needs action (0), still moving (1), or finished well (2).
pub(super) fn lane_rank(state: &str) -> u8 {
    match state {
        "queued" | "starting" | "running" | "settling" => 1,
        "succeeded" | "cancelled" => 2,
        _ => 0,
    }
}

/// Counts by state, then the lanes that need action, then those still moving,
/// then the one typed next command. Finished lanes are counted, not listed.
pub(super) fn lane_summary(lanes: &[LaneLine], continuation: Option<String>) -> crate::Summary {
    if lanes.is_empty() {
        return crate::Summary {
            lines: vec!["no lanes".into()],
            next: None,
            omitted: 0,
        };
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    for lane in lanes {
        match counts.iter_mut().find(|(state, _)| *state == lane.state) {
            Some((_, count)) => *count += 1,
            None => counts.push((lane.state.clone(), 1)),
        }
    }
    counts.sort_by_key(|(state, count)| (lane_rank(state), std::cmp::Reverse(*count)));
    let tally = crate::Summary::tally(
        &counts
            .iter()
            .map(|(state, count)| (state.as_str(), *count))
            .collect::<Vec<_>>(),
    );
    let headline = format!(
        "{} lane{}: {tally}",
        lanes.len(),
        if lanes.len() == 1 { "" } else { "s" }
    );
    let mut listed: Vec<&LaneLine> = lanes
        .iter()
        .filter(|lane| lane_rank(&lane.state) < 2)
        .collect();
    listed.sort_by_key(|lane| lane_rank(&lane.state));
    let rows = listed
        .iter()
        .map(|lane| {
            format!(
                "{} {} {} {}",
                &lane.id[..lane.id.len().min(8)],
                lane.state,
                plain_text(&lane.model, usize::MAX),
                plain_text(&lane.reason, usize::MAX)
            )
            .trim_end()
            .to_owned()
        })
        .collect();
    let next = continuation.or_else(|| {
        listed.first().map(|lane| {
            if lane_rank(&lane.state) == 0 {
                format!("provenant output {}", lane.id)
            } else {
                format!("provenant lanes --id {} --wait 4m", lane.id)
            }
        })
    });
    let mut summary = crate::Summary::new(headline, rows, next);
    for lane in listed.iter().take(crate::limits::SUMMARY_ROWS) {
        if lane.state == "failed" {
            if let Some(error) = &lane.provider_error {
                summary.lines.push(format!(
                    "  provider error {}: {}",
                    &lane.id[..lane.id.len().min(8)],
                    plain_text(error, usize::MAX)
                ));
            }
            if let Some(path) = &lane.result_path {
                summary.lines.push(format!(
                    "  result: {} ({})",
                    plain_text(path, usize::MAX),
                    &lane.id[..lane.id.len().min(8)]
                ));
            }
        }
    }
    summary
}

pub(super) fn structured_path_text(value: &Value) -> Option<String> {
    let path: provenant_records::StructuredPathWire = serde_json::from_value(value.clone()).ok()?;
    Some(path.to_path_buf().ok()?.to_string_lossy().into_owned())
}

/// The lane rows of a `lanes` or `run` reply, in the shape `lane_summary` reads.
pub(super) fn lane_lines(data: &Value) -> Vec<LaneLine> {
    data["lanes"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(LaneLine {
                        id: row["attempt_id"].as_str()?.to_owned(),
                        state: row["state"].as_str()?.to_owned(),
                        model: row["applied_route"]["model"]
                            .as_str()
                            .unwrap_or("unresolved")
                            .to_owned(),
                        reason: row["reason"]
                            .as_str()
                            .or_else(|| row["waiting_for"].as_str())
                            .unwrap_or_default()
                            .to_owned(),
                        provider_error: row["provider_error"].as_str().map(str::to_owned),
                        result_path: structured_path_text(&row["result_path"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The paths a writer may change without review in this project, as JSON and
/// as the line `status` ends with, so the chair sees what will not stop a lane.
pub(super) fn review_exempt_view(context: &run::ExecutionContext) -> (serde_json::Value, String) {
    use provenant_confinement::run_config::RunConfig;
    match RunConfig::read(context.state_root.as_path()) {
        Ok(config) => {
            let paths = config.review_exempt_for(&context.project_id.to_string());
            let line = if paths.is_empty() {
                "review-exempt paths: none\n".to_owned()
            } else {
                format!("review-exempt paths: {}\n", paths.join(", "))
            };
            (json!(paths), line)
        }
        Err(_) => (
            json!([]),
            "review-exempt paths: none (config unreadable; every protected change needs review)\n"
                .to_owned(),
        ),
    }
}

pub(super) fn compact_status(
    rows: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    now: WallTime,
) -> String {
    if rows.is_empty() {
        return "no lanes\n".into();
    }
    rows.iter()
        .map(|lane| {
            let id = lane.attempt_id.to_string();
            let (_, reason) = state_and_reason(&lane.state);
            let state = lane_display_state(lane);
            let model = lane
                .applied_route
                .as_ref()
                .and_then(|route| route.model.as_deref())
                .unwrap_or("unresolved");
            let age = now
                .as_micros()
                .saturating_sub(lane.queued_at.as_micros())
                .max(0)
                / 1_000_000;
            let age = if age < 60 {
                format!("{age}s")
            } else if age < 3600 {
                format!("{}m", age / 60)
            } else {
                format!("{}h", age / 3600)
            };
            let reason = reason
                .or_else(|| {
                    waits
                        .iter()
                        .find(|wait| wait.attempt_id == lane.attempt_id)
                        .map(|wait| wait.waiting_for.clone())
                })
                .unwrap_or_default();
            format!(
                "{} {state} {} {age} {}\n",
                &id[..8],
                plain_text(model, usize::MAX),
                plain_text(&reason, usize::MAX)
            )
        })
        .collect()
}

/// The lanes of a just-accepted run, optionally after waiting for all of them.
pub(super) fn report(
    session: &Session,
    record: &run::RunRecord,
    started: &[run::Started],
    wait: Option<Duration>,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
) -> Result<(Value, String, bool), Fail> {
    let target = Target {
        selection: None,
        run: Some(record.run_id),
        task: None,
        attempt: None,
    };
    let (mut data, mut human, timed_out) = match wait {
        Some(duration) => wait_lanes(session, &[target], Condition::All, duration, edge)?,
        None => {
            let page = session
                .service()
                .lanes(&lane_query(session, &[target], true, 100, 0, call)?, call)?;
            let waits = session.store.lane_waits(&page.lanes, call);
            let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
            let (data, human) = render_lanes(
                &page.lanes,
                &waits,
                None,
                session.context.state_root.as_path(),
                lane_cooldown_view(&cooldowns),
            );
            (data, human, false)
        }
    };
    let canonical = session.store.request(record.run_id, call)?;
    if let Some(rows) = data["lanes"].as_array_mut() {
        for row in rows {
            if let Some(id) = row["attempt_id"].as_str().and_then(|id| id.parse().ok()) {
                let tail = session.store.policy_tail(id, call)?;
                if tail.attempt_id != id {
                    row["superseded_by"] = json!(tail.attempt_id);
                }
            }
            if let Some(task) = canonical
                .tasks
                .iter()
                .find(|task| row["task_id"] == task.task_id.to_string())
            {
                row["network"] = json!(task.access.network(task.writer.is_some()));
                if let Some(sources) = task.access.extra.get("capability_sources") {
                    row["capabilities"] = sources.clone();
                }
            }
        }
    }
    data["run_id"] = json!(record.run_id);
    data["operation_id"] = json!(record.operation_id);
    let unknown = launch_unknown(started);
    if !unknown.is_empty() {
        data["launch_unknown"] = json!(unknown);
    }
    let blocked: Vec<_> = started
        .iter()
        .filter_map(|item| match item {
            run::Started::Blocked { attempt, error } => Some(format!("{attempt}: {error}")),
            _ => None,
        })
        .collect();
    if !blocked.is_empty() {
        for reason in &blocked {
            human.push_str(&format!("\nlaunch blocked: {reason}"));
        }
        data["launch_blocked"] = json!(blocked);
    }
    let capability_text = capability_sources_text(&data);
    Ok((
        data,
        format!("run {}\n{human}{capability_text}", record.run_id),
        timed_out,
    ))
}

pub(super) fn lane_query(
    session: &Session,
    targets: &[Target],
    all: bool,
    limit: u64,
    after: u64,
    call: &run::Request<'_>,
) -> Result<run::LaneQuery, Fail> {
    Ok(run::LaneQuery {
        targets: targets
            .iter()
            .map(|target| targets::local(session, target, call))
            .collect::<Result<_, _>>()?,
        all,
        limit,
        after,
        newest_first: false,
        owner: None,
        unseen_after: None,
    })
}

/// Artifact paths live under the session's execution root, which a nested
/// lane keeps apart from the global configuration root.
pub(super) fn artifact(root: &Path, record: &run::AttemptRecord, name: &str) -> PathBuf {
    root.join(format!(
        "runs/{}/attempts/{}/{name}",
        record.run_id, record.attempt_id
    ))
}

pub(super) fn lane_display_state(lane: &run::AttemptRecord) -> String {
    lane.cloud_observation.0.map_or_else(
        || state_and_reason(&lane.state).0,
        |state| state.state().to_owned(),
    )
}

/// The projected state and recorded terminal reason.
pub fn state_and_reason(state: &run::AttemptState) -> (String, Option<String>) {
    provenant_records::wire::LaneRow::state_and_reason(state)
}

pub(super) fn delivery_state_text(state: &provenant_records::run::WriterDeliveryState) -> String {
    use provenant_records::run::WriterDeliveryState;
    match state {
        WriterDeliveryState::Delivered { pull_request, .. } => format!(
            "Delivered {{ pull_request: {:?} }}",
            pull_request
                .as_deref()
                .map(|value| plain_text(value, usize::MAX))
        ),
        WriterDeliveryState::NoNewCommits { .. } => "NoNewCommits".into(),
        WriterDeliveryState::NotAttempted { .. } => "NotAttempted".into(),
        WriterDeliveryState::Failed { stage, .. } => format!("Failed {{ stage: {stage:?} }}"),
    }
}

pub(super) fn render_lanes(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    next: Option<u64>,
    root: &Path,
    cooldowns: Option<(&[provenant_quota::AccountRecord], i64)>,
) -> (Value, String) {
    render_lanes_with_lineage(lanes, waits, next, root, cooldowns, &Default::default())
}

/// Read one retained attempt without reaping or changing its history.
pub fn project_retained_attempt(
    target: &Target,
    edge: &Edge<'_>,
) -> Result<provenant_records::wire::LaneRow, Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let candidate = targets::resolve(edge, target, true)?;
    let (session, target) = targets::read_resolved(edge, candidate, &call)?;
    let id = target
        .attempt
        .ok_or_else(|| Fail::new(Status::Invalid, ErrorKind::Invalid, "attempt required"))?;
    let mut record = session.store.record(id, &call)?;
    cloud_owner::hydrate(&session.store, std::slice::from_mut(&mut record), &call)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
    if target.run.is_some_and(|run| run != record.run_id)
        || target.task.is_some_and(|task| task != record.task_id)
    {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::NotFound,
            "attempt is not in that run/task",
        ));
    }
    let successors = policy_successors(&session, std::slice::from_ref(&record), &call)?;
    Ok(project_lane(
        &record,
        &[],
        session.context.state_root.as_path(),
        &successors,
        clock.wall_now().ok(),
        false,
    ))
}

pub fn project_lane(
    lane: &run::AttemptRecord,
    waits: &[run::LaneWait],
    root: &Path,
    successors: &std::collections::BTreeMap<AttemptId, AttemptId>,
    now_wall: Option<WallTime>,
    observe_host: bool,
) -> provenant_records::wire::LaneRow {
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
    provenant_records::wire::LaneRow::project(
        lane,
        provenant_records::wire::LaneProjectionFacts {
            superseded_by: successors.get(&lane.attempt_id).copied(),
            waiting_for,
            lane_class: wait.map(|wait| wait.lane_class.as_str().to_owned()),
            overdue: now_wall.map(|now| run::overdue_at(lane, now)),
            stale: observe_host.then(|| run::holder_absent(root, lane)),
            result_path,
            transcript,
            cache_read_share: provenant_routing::cache_read_share(
                lane.usage.uncached_input,
                lane.usage.cache_read,
                lane.usage.cache_write,
            ),
            ..Default::default()
        },
    )
}

pub(super) fn render_lanes_with_lineage(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    next: Option<u64>,
    root: &Path,
    cooldowns: Option<(&[provenant_quota::AccountRecord], i64)>,
    successors: &std::collections::BTreeMap<AttemptId, AttemptId>,
) -> (Value, String) {
    render_lanes_with_view(lanes, waits, next, root, cooldowns, successors, true)
}

pub(super) fn render_lanes_with_view(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    next: Option<u64>,
    root: &Path,
    cooldowns: Option<(&[provenant_quota::AccountRecord], i64)>,
    successors: &std::collections::BTreeMap<AttemptId, AttemptId>,
    observe_host: bool,
) -> (Value, String) {
    let mut human = String::new();
    let now_wall = SystemClock::new().wall_now().ok();
    let rows: Vec<Value> = lanes
        .iter()
        .map(|lane| {
            let projected = project_lane(lane, waits, root, successors, now_wall, observe_host);
            let state = projected.state.clone();
            let route = lane
                .applied_route
                .as_ref()
                .and_then(|r| r.model.clone())
                .unwrap_or_else(|| "unresolved".into());
            let result = lane
                .result
                .as_ref()
                .map(|_| artifact(root, lane, "result.md"));
            let resolved_account = lane
                .applied_route
                .as_ref()
                .and_then(|r| r.account_label.as_deref());
            let account_field = resolved_account
                .map(|a| format!(" account={}", plain_text(a, usize::MAX)))
                .unwrap_or_default();
            let route_text = plain_text(&route, usize::MAX);
            let reason = projected.reason.clone();
            let reason_text = reason
                .as_deref()
                .map(|r| format!(" ({})", plain_text(r, usize::MAX)))
                .unwrap_or_default();
            let title_text = lane
                .title
                .as_deref()
                .map(|title| format!(" {}", plain_text(title, usize::MAX)))
                .unwrap_or_default();
            // A partial finish keeps its outcome; the header says so beside it.
            let state_text = if lane.partial_signals.is_empty() {
                state.clone()
            } else {
                format!("{state} (partial)")
            };
            let state_text = match successors.get(&lane.attempt_id) {
                Some(id) => format!("{state_text} → retrying ({id})"),
                None => state_text,
            };
            let state_text = lane.retry_of.map_or(state_text.clone(), |id| {
                format!("{state_text} (retry of {id})")
            });
            // Peer records remain intact in JSON; terminal text excludes controls.
            human.push_str(&format!(
                "{}/{}/{}{title_text} {state_text} {route_text}{account_field}{reason_text}\n",
                lane.run_id, lane.task_id, lane.attempt_id,
            ));
            if !lane.partial_signals.is_empty() {
                // The outcome is untouched; the chair decides whether to resume.
                human.push_str(&format!(
                    "  partial finish: {}; the chair decides whether to resume\n",
                    lane.partial_signals
                        .iter()
                        .map(|signal| signal.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                if lane
                    .session
                    .as_deref()
                    .is_some_and(|session| !session.is_empty())
                {
                    human.push_str(&format!(
                        "  continue: provenant resume {} --prompt-file PROMPT\n",
                        lane.attempt_id
                    ));
                }
            }
            if matches!(lane.state, run::AttemptState::Running { .. })
                && let Some(deadline) = lane.runtime_deadline
            {
                human.push_str(&format!("  runtime deadline {deadline}\n"));
            }
            if matches!(
                lane.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::TimedOut,
                    ..
                }
            ) && lane
                .session
                .as_deref()
                .is_some_and(|session| !session.is_empty())
            {
                human.push_str(&format!(
                    "  continue: provenant resume {} --prompt-file PROMPT\n",
                    lane.attempt_id
                ));
            }
            if matches!(
                lane.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::ModelUnavailable,
                    ..
                }
            ) {
                // Preserve the existing capacity presentation decision while
                // the displayed reason comes from the safe typed projection.
                let capacity = match &lane.state {
                    run::AttemptState::Terminal { reason, .. } => {
                        reason.as_deref().is_some_and(capacity_reason)
                    }
                    _ => false,
                };
                if capacity
                    && lane
                        .session
                        .as_deref()
                        .is_some_and(|session| !session.is_empty())
                {
                    human.push_str(&format!(
                        "  continue: provenant resume {} --prompt-file PROMPT\n",
                        lane.attempt_id
                    ));
                } else if capacity {
                    human.push_str("  redispatch with another model\n");
                } else {
                    human.push_str("  redispatch with a corrected model\n");
                }
            }
            for skipped in &lane.skipped_routes {
                let diagnostic = skipped.diagnostic();
                human.push_str(&format!(
                    "  skipped {} {:?}: {}; {}\n",
                    plain_text(&skipped.route, usize::MAX),
                    skipped.kind,
                    diagnostic.detail.as_deref().unwrap_or(&diagnostic.code),
                    diagnostic
                        .next
                        .as_deref()
                        .unwrap_or("inspect the current state")
                ));
            }
            let review_required = matches!(
                lane.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::ReviewRequired,
                    ..
                }
            );
            if review_required || !lane.protected_paths.is_empty() {
                human.push_str(&format!(
                    "  review head={} paths={}\n",
                    lane.head
                        .map(|head| head.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    lane.protected_paths
                        .iter()
                        .map(|path| {
                            plain_text(path.as_path_text().unwrap_or("[native path]"), usize::MAX)
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if review_required {
                human.push_str(&format!(
                    "  next: provenant lane deliver --accept-review {}\n",
                    lane.attempt_id
                ));
            }
            if matches!(
                lane.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::Failed,
                    ..
                }
            ) && let Some(path) = &result
            {
                human.push_str(&format!(
                    "  result: {}\n",
                    plain_text(&path.display().to_string(), usize::MAX)
                ));
            }
            if let Some(delivery) = &lane.delivery {
                human.push_str(&format!(
                    "  delivery {}:{} {} head={}\n",
                    plain_text(&delivery.remote, usize::MAX),
                    plain_text(&delivery.branch, usize::MAX),
                    delivery_state_text(&delivery.state),
                    delivery
                        .head
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "unknown".into())
                ));
            }
            json!(projected)
        })
        .collect();
    if rows.is_empty() {
        human.push_str("no matching lanes\n");
    }
    let cooldowns = cooldowns.map_or_else(Vec::new, |(records, now)| {
        render_cooldowns(records, now, &mut human)
    });
    (
        json!({ "lanes": rows, "next_cursor": next.map(|n| n.to_string()), "capacity_cooldowns": cooldowns }),
        human,
    )
}

/// Read optional host cooldowns before composing a lane reply.
pub(super) fn read_lane_cooldowns(
    root: &Path,
) -> Option<(Vec<provenant_quota::AccountRecord>, i64)> {
    crate::quota::read_cache_at(root).ok().zip(
        SystemClock::new()
            .wall_now()
            .ok()
            .map(|now| now.as_timestamp().as_second()),
    )
}

pub(super) fn lane_cooldown_view(
    cooldowns: &Option<(Vec<provenant_quota::AccountRecord>, i64)>,
) -> Option<(&[provenant_quota::AccountRecord], i64)> {
    cooldowns
        .as_ref()
        .map(|(records, now)| (records.as_slice(), *now))
}

pub(super) fn render_cooldowns(
    records: &[provenant_quota::AccountRecord],
    now: i64,
    human: &mut String,
) -> Vec<Value> {
    let mut shown = Vec::new();
    for record in records {
        for cooldown in &record.capacity_cooldowns {
            if cooldown.observed_unix_seconds <= now && cooldown.expires_unix_seconds > now {
                let remaining = cooldown.expires_unix_seconds.saturating_sub(now);
                human.push_str(&format!(
                    "capacity cooldown {}/{} [{}]: {remaining}s remaining\n",
                    plain_text(&record.account.adapter, usize::MAX),
                    plain_text(&cooldown.model, usize::MAX),
                    plain_text(&record.account.label, usize::MAX)
                ));
                shown.push(json!({"account": record.account, "model": cooldown.model, "observed_unix_seconds": cooldown.observed_unix_seconds, "expires_unix_seconds": cooldown.expires_unix_seconds, "remaining_seconds": remaining}));
            }
        }
    }
    shown
}

/// `lanes`, buffering any watch lines into the reply for callers that cannot
/// stream them.
pub fn lanes(command: Lanes, edge: &Edge<'_>) -> Reply {
    let mut lines = String::new();
    let mut reply = lanes_streaming(command, edge, false, &mut |line| {
        lines.push_str(line);
        Ok(())
    });
    reply.human.insert_str(0, &lines);
    reply
}

/// `lanes` with a watch writing each line to `out` and flushing it as it
/// happens. With `json` each line is a JSON object and the reply is the last.
pub fn lanes_streaming(
    command: Lanes,
    edge: &Edge<'_>,
    json: bool,
    out: &mut dyn FnMut(&str) -> Result<(), Fail>,
) -> Reply {
    if command.session_stream {
        let mut result = reply("lanes", session_lane_page(&command, edge, out));
        result.read_only = true;
        return result;
    }
    let result = if let Some(seconds) = command.wait {
        lanes_wait_entry(&command, edge, json, out, seconds)
    } else {
        lanes_inner(&command, edge, json, out, None)
    };
    let mut out = reply_waited("lanes", result);
    // A watch already printed its events; its final line is its own summary.
    if out.result.error.is_none()
        && out.result.data["lanes"].is_array()
        && !(command.watch.is_some() && !json)
    {
        out.summary = Some(lane_summary(&lane_lines(&out.result.data), None));
    }
    out.read_only = true;
    out
}

/// A read-only page for the hook. Emit each record before reading the next so
/// a timed-out child still leaves complete, bounded records in its output.
fn session_lane_page(
    command: &Lanes,
    edge: &Edge<'_>,
    out: &mut dyn FnMut(&str) -> Result<(), Fail>,
) -> Result<(Value, String), Fail> {
    if !command.id.is_empty()
        || command.host.is_some()
        || command.session.is_some()
        || command.cursor.is_some()
        || command.wait.is_some()
        || command.watch.is_some()
        || !command.states.is_empty()
        || command.since.is_some()
    {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "invalid session lane stream",
        ));
    }
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
    let _ = session.store.reap(&call);
    let mut query = lane_query(&session, &[], command.all, command.limit, 0, &call)?;
    query.newest_first = command.all;
    let mut failed = false;
    session.store.lanes_streaming(&query, &call, |total, entry, next| {
        if failed { return; }
        let event = match entry {
            None => json!({"total": total, "next": next}),
            Some((cursor, lane)) => {
                let (state, reason) = state_and_reason(&lane.state);
                let reason = reason.or_else(|| {
                    (!command.all)
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

#[allow(clippy::disallowed_methods)]
pub(super) fn lanes_wait_entry(
    command: &Lanes,
    edge: &Edge<'_>,
    json: bool,
    emit: Emit<'_>,
    seconds: u64,
) -> Result<(Value, String, bool), Fail> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let entered = watch::entered()?;
    loop {
        match lanes_inner(command, edge, json, &mut *emit, Some((deadline, entered))) {
            Err(fail) if fail.kind == ErrorKind::Contention => {
                if Instant::now() >= deadline {
                    let cooldowns = read_lane_cooldowns(edge.roots.state.as_path());
                    let (mut data, human) = render_lanes(
                        &[],
                        &[],
                        None,
                        edge.roots.state.as_path(),
                        lane_cooldown_view(&cooldowns),
                    );
                    data["condition_met"] = json!(false);
                    return Ok((data, human, true));
                }
                watch::pause(LOCAL_POLL, deadline);
                if Instant::now() >= deadline {
                    let cooldowns = read_lane_cooldowns(edge.roots.state.as_path());
                    let (mut data, human) = render_lanes(
                        &[],
                        &[],
                        None,
                        edge.roots.state.as_path(),
                        lane_cooldown_view(&cooldowns),
                    );
                    data["condition_met"] = json!(false);
                    return Ok((data, human, true));
                }
            }
            result => return result,
        }
    }
}

pub(super) fn lanes_inner(
    command: &Lanes,
    edge: &Edge<'_>,
    json: bool,
    emit: Emit<'_>,
    wait_entry: Option<(Instant, WallTime)>,
) -> Result<(Value, String, bool), Fail> {
    let mut normalised = command.clone();
    let mut projects = std::collections::BTreeSet::new();
    let mut first = None;
    for target in &mut normalised.id {
        let candidate = targets::resolve(edge, target, true)?;
        if let Some(project) = candidate.project {
            projects.insert(project);
        }
        *target = candidate.target.clone();
        if first.is_none() {
            first = Some(candidate);
        }
    }
    if projects.len() > 1 {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "targets belong to different projects; run one `provenant lanes --id ID --wait 4m` command per project",
        ));
    }
    let command = &normalised;
    if let Some(host) = command.host.as_deref()
        && !remote::names_local_host(edge, host)?
    {
        return remote::host_lanes(command, host, edge, json, emit);
    }
    if let Some(placed) = remote::lanes(command, edge, json, emit)? {
        return Ok(placed);
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = if wait_entry.is_some() {
        run::Request {
            clock: &clock,
            cancel: &cancel,
        }
    } else {
        request(&clock, &cancel)?
    };
    let (context, store) = if let Some(candidate) = first {
        let (session, _) = targets::read_resolved(edge, candidate, &call)?;
        (session.context, Some(session.store))
    } else {
        open_read_only(edge, &call)?
    };
    let Some(store) = store else {
        let root = execution_state(edge.roots, edge.home, edge.assets)?;
        let cooldowns = read_lane_cooldowns(root.as_path());
        let (mut data, human) = render_lanes(
            &[],
            &[],
            None,
            root.as_path(),
            lane_cooldown_view(&cooldowns),
        );
        if command.wait.is_some() || command.watch.is_some() {
            // No store means no lanes, so there is nothing left to wait for.
            data["condition_met"] = json!(true);
        }
        let human = if command.watch.is_some() && !json {
            "end settled lanes=0\n".into()
        } else {
            human
        };
        return Ok((data, human, false));
    };
    let session = Session { store, context };
    let owner = match command.session.as_deref() {
        Some(label) => session_actor(edge, label)?,
        None => session.context.actor_id,
    };
    let since = command
        .since
        .map(|since| WallTime::from_micros(since.as_microsecond()))
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "invalid --since value"))?;
    let picks = Picks {
        targets: &command.id,
        owner,
        label: command.session.as_deref(),
        since,
    };
    if let Some(seconds) = command.watch {
        return watch_command(&session, &picks, seconds, json, emit);
    }
    if let Some((deadline, entered)) = wait_entry {
        return wait_command(&session, &picks, command.condition, deadline, entered, edge);
    }
    let after = match &command.cursor {
        Some(cursor) => cursor
            .parse()
            .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "invalid cursor"))?,
        None => 0,
    };
    // A lane whose guardian died is settled by the next read, not left running.
    let _ = session.store.reap(&call);
    let mut query = lane_query(
        &session,
        &command.id,
        command.all,
        command.limit,
        after,
        &call,
    )?;
    if command.session.is_some() && command.id.is_empty() {
        query.owner = Some(owner);
    }
    let by_session =
        |lane: &run::AttemptRecord| command.session.is_none() || lane.verified_owner == Some(owner);
    let filtered = !command.states.is_empty() || command.since.is_some();
    query.newest_first = filtered || command.all;
    // A filtered view hydrates row by row across pages, and then again as a whole.
    let budget_end = cloud_owner::view_observation_budget();
    let (mut lanes, next) = if filtered {
        query.limit = 100;
        let mut matches = Vec::new();
        let mut next = None;
        loop {
            let page = session.service().lanes(&query, &call)?;
            for mut lane in page.lanes {
                cloud_owner::hydrate_within(
                    &session.store,
                    std::slice::from_mut(&mut lane),
                    &call,
                    budget_end,
                )
                .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
                let state_matches = command.states.is_empty()
                    || command
                        .states
                        .iter()
                        .any(|state| state.as_str() == lane_display_state(&lane));
                let since_matches = since.is_none_or(|since| lane.queued_at >= since);
                if state_matches && since_matches && by_session(&lane) {
                    matches.push(lane);
                }
                if matches.len() > command.limit as usize {
                    let last = matches.get(command.limit as usize - 1).ok_or_else(|| {
                        Fail::new(Status::Error, ErrorKind::Internal, "lane page invariant")
                    })?;
                    next = Some(session.store.attempt_sequence(last.attempt_id, &call)?);
                    matches.truncate(command.limit as usize);
                    break;
                }
            }
            if next.is_some() {
                break;
            }
            match page.next {
                Some(cursor) => query.after = cursor,
                None => break,
            }
        }
        (matches, next)
    } else {
        let page = session.service().lanes(&query, &call)?;
        let lanes: Vec<_> = page.lanes.into_iter().filter(by_session).collect();
        (lanes, page.next)
    };
    cloud_owner::hydrate_within(&session.store, &mut lanes, &call, budget_end)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
    let waits = session.store.lane_waits(&lanes, &call);
    let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
    let successors = policy_successors(&session, &lanes, &call)?;
    let (mut data, mut human) = render_lanes_with_lineage(
        &lanes,
        &waits,
        next,
        session.context.state_root.as_path(),
        lane_cooldown_view(&cooldowns),
        &successors,
    );
    append_provider_errors(&session, &lanes, &mut data, &mut human);
    attach_handoffs(&session, &lanes, &mut data);
    attach_cloud_jobs(&session.store, &lanes, &mut data, &mut human, &call)?;
    Ok((data, human, false))
}

pub(super) fn attach_cloud_jobs(
    store: &run::RunStore,
    lanes: &[run::AttemptRecord],
    data: &mut Value,
    human: &mut String,
    call: &run::Request<'_>,
) -> Result<(), Fail> {
    let mut jobs = Vec::new();
    for lane in lanes {
        let Some(placement) = cloud_owner::placement(store, lane, call)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?
        else {
            continue;
        };
        let mut value = match lane.cloud_view.0.clone() {
            Some(view) => view,
            None => match cloud_owner::load(store, lane.attempt_id, call)
                .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?
            {
                Some(job) => cloud_owner::view(&job),
                None => json!({"target": placement.target, "state": {"state": "submitting"}}),
            },
        };
        value["attempt_id"] = json!(lane.attempt_id);
        let state = value["state"]["state"].as_str().unwrap_or("unknown");
        human.push_str(&format!(
            "{} cloud {}: {}",
            lane.attempt_id,
            plain_text(&placement.target, usize::MAX),
            state
        ));
        if value["stale"] == true {
            human.push_str(" (saved state may be stale)");
        }
        if let Some(passed) = value["passed"].as_bool() {
            human.push_str(if passed {
                " (advisory pass)"
            } else {
                " (advisory failure)"
            });
        }
        if let Some(model) = value["observed_model_status"].as_str() {
            human.push_str(&format!(", observed model {model}"));
        }
        if let Some(detail) = value["state"]["detail"].as_str() {
            human.push_str(&format!(": {}", plain_text(detail, usize::MAX)));
        }
        if let Some(remedy) = value["remedy"].as_str() {
            human.push_str(&format!("; {}", plain_text(remedy, usize::MAX)));
        }
        human.push('\n');
        jobs.push(value);
    }
    if !jobs.is_empty() {
        data["cloud_jobs"] = Value::Array(jobs);
    }
    Ok(())
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
    let lane = project_lane(
        record,
        &[],
        Path::new("/"),
        &Default::default(),
        None,
        false,
    );
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
        receipt_path: format!("runs/{}/attempts/{}", record.run_id, record.attempt_id),
        risk: None,
        reviews: Vec::new(),
    }
}

/// The recorded facts of local attempts, for the pull request body. An attempt
/// that ran on a peer is refused: its record lives on that host.
pub fn attempt_facts(
    edge: &Edge<'_>,
    attempts: &[crate::Attempt],
) -> Result<Vec<AttemptFacts>, Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    attempts
        .iter()
        .map(|attempt| {
            let candidate = targets::resolve(edge, &attempt.0, false)?;
            if targets::placement(edge, &candidate.target, false)?.is_some() {
                return Err(Fail::new(
                    Status::Refused,
                    ErrorKind::Unsupported,
                    "that attempt ran on a peer; its record is read on that host",
                ));
            }
            let (session, selected) = targets::read_resolved(edge, candidate, &call)?;
            let id = targets::local(&session, &selected, &call)?
                .attempt
                .ok_or(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "attempt required",
                ))?;
            let record = session.service().status(id, &call)?;
            let (risk, reviews) = match record.delivery_id {
                Some(delivery) => {
                    let receipt = provenant_workflow::store::ReceiptStore::new(
                        session.context.state_root.clone(),
                        session.context.project_id,
                        delivery,
                    )
                    .read()
                    .map_err(|error| {
                        Fail::new(Status::Error, ErrorKind::StateCorrupt, error.to_string())
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
        })
        .collect()
}
