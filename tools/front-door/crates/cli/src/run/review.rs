use super::*;

/// Admit reviewers through the ordinary run scheduler and collate their results.
pub fn review(command: crate::Review, edge: &Edge<'_>) -> Reply {
    let mut data = json!({"reviewers": []});
    let outcome = review_inner(command, edge, &mut data);
    let collected = review_collect(edge, &mut data);
    let verdicts = aggregate_verdicts(&data);
    data["verdicts"] = verdicts;
    let human = review_text(&data);
    let (status, error) = match (outcome, collected) {
        (Err(error), _) | (Ok(_), Err(error)) => (error.status, Some(error)),
        (Ok(true), Ok(())) => (Status::Timeout, None),
        (Ok(false), Ok(()))
            if data["reviewers"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|r| r["state"].as_str() != Some("succeeded") || r["verdict"] != "approve")
            }) =>
        {
            (Status::Failed, None)
        }
        (Ok(false), Ok(())) => (Status::Ok, None),
    };
    let mut result = CommandResult::new("review", status, data);
    if let Some(error) = error {
        result.error = Some(CommandError {
            code: error.kind.code().into(),
            message: format!("{}\n{}", error.message, human),
            cause: None,
            repair: None,
        });
    }
    Reply {
        result,
        human,
        summary: None,
        read_only: false,
    }
}

#[allow(clippy::disallowed_methods)]
fn review_inner(command: crate::Review, edge: &Edge<'_>, data: &mut Value) -> Result<bool, Fail> {
    let invalid = |message: String| Fail::new(Status::Invalid, ErrorKind::Invalid, message);
    let git = |args: &[&str]| -> Result<String, Fail> {
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        let executable =
            provenant_os::git::git_executable().ok_or_else(|| invalid("Git unavailable".into()))?;
        let mut argv = vec![executable.into_os_string()];
        argv.extend(args.iter().map(std::ffi::OsString::from));
        let executor = SystemExecutor::new(1).map_err(|e| invalid(e.to_string()))?;
        let output = executor
            .execute(ExecOptions::new(&argv, edge.cwd, Duration::from_secs(30)))
            .map_err(|e| invalid(e.to_string()))?;
        if output.termination != Termination::Exit(0) || output.timed_out || output.stdout.truncated
        {
            return Err(invalid(format!(
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes).trim()
            )));
        }
        String::from_utf8(output.stdout.bytes).map_err(|_| {
            invalid("Git output is not UTF-8; use --shape to bypass changed-path selection".into())
        })
    };
    let head = git(&[
        "rev-parse",
        "--verify",
        "--end-of-options",
        &format!("{}^{{commit}}", command.target),
    ])?
    .trim()
    .to_owned();
    let paths: Vec<String> =
        if let Some(range) = selection_range(command.shape.as_deref(), command.range.as_deref())? {
            git(&["diff", "--no-renames", "--name-only", "-z", range, "--"])?
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        } else {
            vec![]
        };
    let brief = read_bounded(&command.brief_file)?;
    let domains = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|e| invalid(e.to_string()))?
        .domains;
    let selected = domains
        .routing
        .review
        .select_shape(
            command.shape.as_deref(),
            command.risk.as_deref(),
            command.kind.as_deref(),
            &paths,
        )
        .map_err(invalid)?;
    if selected
        .shape
        .reviewers
        .iter()
        .map(|group| group.count)
        .sum::<usize>()
        > 64
    {
        return Err(invalid("review shape exceeds 64 reviewers".into()));
    }
    data["shape"] = json!(selected.name);
    data["selection_source"] = json!(selected.source);
    data["target"] = json!(head);

    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| invalid(e.to_string()))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = open(edge, &call)?;
    let chair = if crate::fabric::in_lane() {
        None
    } else {
        let context = crate::fabric::Context::open(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|e| Fail::new(Status::Error, ErrorKind::Identity, e.to_string()))?;
        Some(run::ChairBinding {
            extra: Default::default(),
            host: context.identity.inbox.host,
            actor: crate::workflow::session_actor(&context, None)
                .map_err(|e| Fail::new(Status::Error, ErrorKind::Identity, e))?,
        })
    };
    let mut slots = Vec::new();
    for (group_index, group) in selected.shape.reviewers.iter().enumerate() {
        for index in 0..group.count {
            let lens = group.lenses.get(index).cloned();
            let (operation_id, task_id) = review_identity(
                &head,
                selected.name,
                selected.shape,
                &brief,
                group_index,
                index,
                0,
            )?;
            data["reviewers"].as_array_mut().ok_or_else(|| invalid("invalid review result".into()))?.push(json!({
                "operation_id": operation_id, "run_id": null, "attempt_id": null, "pool": group.pool,
                "lens": lens, "group": group_index, "distinct_families": group.distinct_families,
                "state": "not_submitted", "ordinal": 0, "applied_route": null, "family": null,
                "substitution": false, "verdict": "unknown", "result_path": null
            }));
            slots.push(ReviewSlot {
                group: group_index,
                pool: group.pool.clone(),
                lens,
                distinct: group.distinct_families,
                index,
                ordinal: 0,
                operation_id,
                task_id,
                run: None,
                ready: false,
                family: None,
            });
        }
    }
    let deadline = Instant::now() + Duration::from_secs(command.wait);
    // Recover every already-published slot before considering dependencies, so
    // a timed-out retry still reports lanes created by an earlier invocation.
    for (i, slot) in slots.iter_mut().enumerate() {
        let call = request(&clock, &cancel)?;
        let choice = recover_slot(
            &head,
            selected.name,
            selected.shape,
            &brief,
            slot.group,
            slot.index,
            |id| inspect_slot(&session, id, &call),
        )?;
        slot.ordinal = choice.ordinal;
        slot.operation_id = choice.operation_id;
        slot.task_id = choice.task_id;
        data["reviewers"][i]["operation_id"] = json!(slot.operation_id);
        data["reviewers"][i]["ordinal"] = json!(slot.ordinal);
        if let Some(id) = choice.run {
            slot.run = Some(id);
            data["reviewers"][i]["run_id"] = json!(id);
            data["reviewers"][i]["state"] = json!("submitted");
            launch(&session, id, edge, &call)?;
        }
    }
    // Each pass admits all independent slots before waiting for a distinct
    // group's preceding route. Other groups continue even when one is paced.
    loop {
        let mut progressed = false;
        // Observe recovered siblings before replacing a failed earlier slot,
        // so the replacement avoids families already used later in the group.
        for slot in &mut slots {
            if !slot.distinct || slot.ready {
                continue;
            }
            let Some(id) = slot.run else { continue };
            let target = Target {
                selection: None,
                run: Some(id),
                task: None,
                attempt: None,
            };
            let call = request(&clock, &cancel)?;
            let page = session.service().lanes(
                &lane_query(&session, &[target], true, 100, 0, &call)?,
                &call,
            )?;
            if let Some(lane) = page.lanes.first() {
                if let Some(route) = &lane.applied_route {
                    slot.family = route
                        .model
                        .as_deref()
                        .and_then(|m| ModelId::parse(m).ok())
                        .and_then(|m| catalogue.model(&m).and_then(|m| m.family.clone()));
                    slot.ready = true;
                    progressed = true;
                } else if matches!(lane.state, run::AttemptState::Terminal { .. }) {
                    slot.ready = true;
                    progressed = true;
                }
            }
        }
        for i in 0..slots.len() {
            if !can_submit(&slots, i) {
                continue;
            }
            let families: Vec<String> = slots
                .iter()
                .enumerate()
                .filter(|(j, s)| *j != i && s.group == slots[i].group)
                .filter_map(|(_, s)| s.family.clone())
                .collect();
            let slot = &slots[i];
            let call = request(&clock, &cancel)?;
            // A published operation owns its immutable request. Launch any
            // still-queued work without reconstructing changing exclusions.
            let published = published_operation(&session, &slot.operation_id, &call)?;
            let run_id = if let Some(id) = published {
                data["reviewers"][i]["run_id"] = json!(id);
                data["reviewers"][i]["state"] = json!("submitted");
                launch(&session, id, edge, &call)?;
                id
            } else {
                let submitted = submit(
                    &session,
                    edge,
                    Submission {
                        chair: chair.clone(),
                        operation_id: slot.operation_id.clone(),
                        concurrency: Some(1),
                        knobs: vec![],
                        tasks: vec![review_task(slot, &head, &brief, families)],
                    },
                    &call,
                );
                match submitted {
                    Ok((record, _)) => record.run_id,
                    Err(error) => {
                        if let Ok(Some(id)) =
                            published_operation(&session, &slot.operation_id, &call)
                        {
                            data["reviewers"][i]["run_id"] = json!(id);
                            data["reviewers"][i]["state"] = json!("submitted");
                            // A concurrent retry may have sealed this same slot
                            // with different route exclusions. Attach to it.
                            if error.kind == ErrorKind::OperationConflict {
                                launch(&session, id, edge, &call)?;
                                slots[i].run = Some(id);
                                progressed = true;
                                continue;
                            }
                        }
                        return Err(error);
                    }
                }
            };
            slots[i].run = Some(run_id);
            data["reviewers"][i]["run_id"] = json!(run_id);
            data["reviewers"][i]["state"] = json!("submitted");
            progressed = true;
        }
        if slots.iter().all(|s| s.run.is_some()) {
            break;
        }
        if Instant::now() >= deadline {
            return Ok(true);
        }
        if !progressed {
            watch::pause(Duration::from_millis(500), deadline);
        }
    }
    let targets: Vec<Target> = slots
        .iter()
        .filter_map(|slot| slot.run)
        .map(|id| Target {
            selection: None,
            run: Some(id),
            task: None,
            attempt: None,
        })
        .collect();
    let (_, _, timed_out) = wait_lanes(
        &session,
        &targets,
        Condition::All,
        deadline.saturating_duration_since(Instant::now()),
        edge,
    )?;
    Ok(timed_out)
}

struct ReviewSlot {
    group: usize,
    index: usize,
    ordinal: u64,
    pool: String,
    lens: Option<String>,
    distinct: bool,
    operation_id: OperationId,
    task_id: TaskId,
    run: Option<RunId>,
    ready: bool,
    family: Option<String>,
}

fn can_submit(slots: &[ReviewSlot], index: usize) -> bool {
    let slot = &slots[index];
    slot.run.is_none()
        && (!slot.distinct
            || !slots[..index]
                .iter()
                .any(|s| s.group == slot.group && !s.ready)
                && !slots[index + 1..]
                    .iter()
                    .any(|s| s.group == slot.group && s.run.is_some() && !s.ready))
}

/// Return only sealed operations. A prepared reservation without its request
/// is not a submitted run and must be repaired through the normal create path.
fn sealed_operation(
    operation: Option<run::OperationRecord>,
    request: impl FnOnce(RunId) -> Result<(), run::RunError>,
) -> Result<Option<RunId>, run::RunError> {
    let Some(operation) = operation else {
        return Ok(None);
    };
    match request(operation.run_id) {
        Ok(()) => Ok(Some(operation.run_id)),
        Err(run::RunError::NotFound) if operation.state == run::OperationState::Prepared => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn published_operation(
    session: &Session,
    id: &OperationId,
    call: &run::Request<'_>,
) -> Result<Option<RunId>, Fail> {
    Ok(sealed_operation(
        session.store.operation(id, call)?,
        |run| session.store.request(run, call).map(|_| ()),
    )?)
}

#[derive(Debug, PartialEq, Eq)]
enum SlotProbe {
    Vacant,
    Reattach(RunId),
    Failed,
}

fn inspect_slot(
    session: &Session,
    id: &OperationId,
    call: &run::Request<'_>,
) -> Result<SlotProbe, Fail> {
    let Some(id) = published_operation(session, id, call)? else {
        return Ok(SlotProbe::Vacant);
    };
    let target = Target {
        selection: None,
        run: Some(id),
        task: None,
        attempt: None,
    };
    let page = session
        .service()
        .lanes(&lane_query(session, &[target], true, 100, 0, call)?, call)?;
    let lane = page
        .lanes
        .first()
        .ok_or_else(|| Fail::new(Status::Error, ErrorKind::NotFound, "review run has no lane"))?;
    Ok(probe_state(id, &lane.state))
}

fn probe_state(id: RunId, state: &run::AttemptState) -> SlotProbe {
    match state {
        run::AttemptState::Terminal { outcome, .. }
            if !matches!(
                outcome,
                run::Outcome::Succeeded
                    | run::Outcome::ReviewRequired
                    | run::Outcome::InputRequired
            ) =>
        {
            SlotProbe::Failed
        }
        _ => SlotProbe::Reattach(id),
    }
}

struct SlotRecovery {
    ordinal: u64,
    operation_id: OperationId,
    task_id: TaskId,
    run: Option<RunId>,
}

#[allow(clippy::too_many_arguments)]
fn recover_slot(
    head: &str,
    name: &str,
    shape: &provenant_routing::ReviewShape,
    brief: &str,
    group: usize,
    index: usize,
    mut probe: impl FnMut(&OperationId) -> Result<SlotProbe, Fail>,
) -> Result<SlotRecovery, Fail> {
    for ordinal in 0..=1024 {
        let (operation_id, task_id) =
            review_identity(head, name, shape, brief, group, index, ordinal)?;
        match probe(&operation_id)? {
            SlotProbe::Failed => continue,
            SlotProbe::Vacant => {
                return Ok(SlotRecovery {
                    ordinal,
                    operation_id,
                    task_id,
                    run: None,
                });
            }
            SlotProbe::Reattach(run) => {
                return Ok(SlotRecovery {
                    ordinal,
                    operation_id,
                    task_id,
                    run: Some(run),
                });
            }
        }
    }
    Err(Fail::new(
        Status::Error,
        ErrorKind::Conflict,
        "review retry limit exceeded",
    ))
}

fn review_identity(
    head: &str,
    name: &str,
    shape: &provenant_routing::ReviewShape,
    brief: &str,
    group: usize,
    index: usize,
    ordinal: u64,
) -> Result<(OperationId, TaskId), Fail> {
    let mut identity = json!({
        "head": head, "name": name, "shape": shape, "brief": brief, "group": group, "index": index,
    });
    // Keep the original digest for ordinal zero so previously submitted slots
    // remain recoverable after upgrading.
    if ordinal > 0 {
        identity["ordinal"] = json!(ordinal);
    }
    let digest = CanonicalV1::record_digest("review-slot", 1, &identity)
        .map_err(|e| Fail::new(Status::Invalid, ErrorKind::Invalid, e.to_string()))?;
    let operation = OperationId::parse(&format!(
        "review-{}",
        provenant_core::hex(digest.as_bytes())
    ))
    .map_err(|e| Fail::new(Status::Invalid, ErrorKind::Invalid, e.to_string()))?;
    let mut task = [0; 16];
    task.copy_from_slice(&digest.as_bytes()[..16]);
    Ok((operation, TaskId::from_bytes(task)))
}

fn review_task(slot: &ReviewSlot, head: &str, brief: &str, families: Vec<String>) -> Submitted {
    let prompt = format!(
        "Review commit {head}. Work read-only.\nLens: {}\nStart with exactly one line: Verdict: approve, Verdict: request_changes, or Verdict: inconclusive. Then give findings.\n\n{brief}",
        slot.lens.as_deref().unwrap_or("general")
    );
    Submitted {
        context: None,
        build_heavy: false,
        title: Some(format!(
            "Review {}",
            slot.lens.as_deref().unwrap_or("general")
        )),
        task_id: slot.task_id,
        prompt,
        base: None,
        head: Some(head.into()),
        route: run::RequestedRoute::Review {
            extra: Default::default(),
            name: slot.pool.clone(),
            effort: None,
            distinct_families: slot.distinct,
            excluded_families: if slot.distinct { families } else { vec![] },
            require_distinct: false,
        },
        account: None,
        fallback: vec![],
        provider_timeout: 3600,
        delivery_id: None,
        deliver: None,
        worktree: None,
        access: run::LaneAccess::default(),
    }
}

fn selection_range<'a>(
    shape: Option<&str>,
    range: Option<&'a str>,
) -> Result<Option<&'a str>, Fail> {
    if shape.is_some() {
        return Ok(None);
    }
    if range.is_some_and(|range| range.starts_with('-')) {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "git range must not begin with -",
        ));
    }
    Ok(range)
}

fn review_collect(edge: &Edge<'_>, data: &mut Value) -> Result<(), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = open(edge, &call)?;
    let catalogue = provenant_routing::Catalogue::bundled()
        .map_err(|e| Fail::new(Status::Error, ErrorKind::Config, e.to_string()))?;
    let mut seen: std::collections::BTreeMap<u64, Vec<String>> = std::collections::BTreeMap::new();
    let Some(rows) = data["reviewers"].as_array_mut() else {
        return Ok(());
    };
    for reviewer in rows {
        let Some(id) = reviewer["run_id"]
            .as_str()
            .and_then(|id| id.parse::<RunId>().ok())
        else {
            continue;
        };
        let target = Target {
            selection: None,
            run: Some(id),
            task: None,
            attempt: None,
        };
        let call = request(&clock, &cancel)?;
        let page = session.service().lanes(
            &lane_query(&session, &[target], true, 100, 0, &call)?,
            &call,
        )?;
        let (rendered, _) = render_lanes(
            &page.lanes,
            &[],
            None,
            session.context.state_root.as_path(),
            None,
        );
        let Some(row) = rendered["lanes"].as_array().and_then(|r| r.first()) else {
            continue;
        };
        let family = row["applied_route"]["model"]
            .as_str()
            .and_then(|m| ModelId::parse(m).ok())
            .and_then(|m| catalogue.model(&m).and_then(|m| m.family.clone()));
        let used = seen
            .entry(reviewer["group"].as_u64().unwrap_or_default())
            .or_default();
        let substitution = reviewer["distinct_families"].as_bool() == Some(true)
            && row["applied_route"]["model"].is_string()
            && family.as_ref().is_none_or(|f| used.contains(f));
        if let Some(f) = &family {
            used.push(f.clone());
        }
        let verdict = row["result_path"]
            .as_str()
            .and_then(|p| review_verdict_file(Path::new(p)))
            .unwrap_or("unknown");
        if let (Some(target), Some(source)) = (reviewer.as_object_mut(), row.as_object()) {
            target.extend(source.clone());
        }
        reviewer["family"] = json!(family);
        reviewer["substitution"] = json!(substitution);
        reviewer["verdict"] = json!(verdict);
    }
    Ok(())
}
// Verdicts are at the beginning; the rest of a large result need not be read.
fn review_verdict_file(path: &Path) -> Option<&'static str> {
    const LIMIT: u64 = 8192;
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() as u64 > LIMIT;
    bytes.truncate(LIMIT as usize);
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if truncated && !line.ends_with(b"\n") {
            return None;
        }
        let line = std::str::from_utf8(line).ok()?;
        if !line.trim().is_empty() {
            return Some(review_verdict(line));
        }
    }
    None
}
fn review_verdict(output: &str) -> &'static str {
    let Some(line) = output.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return "unknown";
    };
    let stripped = line.replace('*', "");
    let line = stripped.trim_matches('_').trim();
    let Some((label, verdict)) = line.split_once(':') else {
        return "unknown";
    };
    if !label
        .trim_matches(|c| c == '*' || c == '_')
        .trim()
        .eq_ignore_ascii_case("verdict")
    {
        return "unknown";
    }
    match verdict
        .trim()
        .trim_matches(|c| c == '*' || c == '_')
        .to_ascii_lowercase()
        .as_str()
    {
        "approve" => "approve",
        "request_changes" => "request_changes",
        "inconclusive" => "inconclusive",
        _ => "unknown",
    }
}
fn aggregate_verdicts(data: &Value) -> Value {
    let mut counts = json!({"approve": 0, "request_changes": 0, "inconclusive": 0, "unknown": 0, "not_submitted": 0});
    if let Some(rows) = data["reviewers"].as_array() {
        for row in rows {
            let verdict = row["verdict"].as_str().unwrap_or("unknown");
            let key = if row["state"] == "not_submitted" {
                "not_submitted"
            } else {
                match verdict {
                    "approve" | "request_changes" | "inconclusive" => verdict,
                    _ => "unknown",
                }
            };
            counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
        }
    }
    counts
}
fn review_text(data: &Value) -> String {
    let mut text = format!(
        "review {} ({})\n",
        data["shape"].as_str().unwrap_or("unknown"),
        data["selection_source"].as_str().unwrap_or("unknown")
    );
    if let Some(rows) = data["reviewers"].as_array() {
        for row in rows {
            let model = row["applied_route"]["model"]
                .as_str()
                .unwrap_or("unresolved");
            let effort = row["applied_route"]["effort"]
                .as_str()
                .map(|e| format!("@{e}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  run={} operation={} {}{} state={} verdict={} output={}{}\n",
                row["run_id"].as_str().unwrap_or("unknown"),
                row["operation_id"].as_str().unwrap_or("unknown"),
                model,
                effort,
                row["state"].as_str().unwrap_or("pending"),
                row["verdict"].as_str().unwrap_or("unknown"),
                row["result_path"].as_str().unwrap_or("none"),
                if row["substitution"].as_bool() == Some(true) {
                    " (family substitution)"
                } else {
                    ""
                }
            ));
            if let Some(id) = row["run_id"].as_str()
                && matches!(
                    row["state"].as_str(),
                    Some("queued" | "starting" | "running" | "settling" | "submitted")
                )
            {
                text.push_str(&format!("    collect: provenant lanes --id {id} --wait 4m; cancel: provenant cancel {id}\n"));
            }
        }
    }
    if data["reviewers"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["state"] == "not_submitted"))
    {
        text.push_str("  Retry the same review command to submit remaining reviewers and reattach to existing runs.\n");
    }
    if data["reviewers"].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            matches!(
                row["state"].as_str(),
                Some(
                    "failed"
                        | "rejected"
                        | "cancelled"
                        | "timed_out"
                        | "rate_limited"
                        | "quota_limited"
                        | "auth_required"
                        | "model_unavailable"
                        | "permission_denied"
                        | "quarantined"
                        | "lost"
                        | "uncommitted"
                )
            )
        })
    }) {
        text.push_str("  Retry the same review command to resubmit failed or cancelled reviewers and reattach to the others.\n");
    }
    text
}

#[cfg(test)]
mod review_shape_tests {
    use super::*;
    #[test]
    fn verdict_requires_an_exact_first_line() {
        assert_eq!(review_verdict("Verdict: approve\nNothing found"), "approve");
        assert_eq!(
            review_verdict("Verdict: request_changes\nFinding"),
            "request_changes"
        );
        assert_eq!(review_verdict("I approve\nVerdict: approve"), "unknown");
        assert_eq!(review_verdict("Verdict: approved"), "unknown");
        assert_eq!(review_verdict("\n **Verdict: APPROVE**\n"), "approve");
        assert_eq!(
            review_verdict("**Verdict:** *Request_Changes*"),
            "request_changes"
        );
    }
    #[test]
    fn identity_is_stable_and_changes_with_review_inputs() -> Result<(), Fail> {
        let shape = &provenant_routing::ReviewConfig::default().shapes["quick"];
        let first = review_identity("head", "quick", shape, "brief", 0, 0, 0)?;
        assert_eq!(
            first,
            review_identity("head", "quick", shape, "brief", 0, 0, 0)?
        );
        assert_ne!(
            first,
            review_identity("head", "quick", shape, "changed", 0, 0, 0)?
        );
        assert_ne!(
            first,
            review_identity("head", "quick", shape, "brief", 0, 1, 0)?
        );
        assert_ne!(
            first,
            review_identity("head", "quick", shape, "brief", 0, 0, 1)?
        );
        Ok(())
    }
    #[test]
    fn prepared_partial_retry_reuses_ordinal_and_sealed_bypasses_resubmission()
    -> Result<(), Box<dyn std::error::Error>> {
        let shape = &provenant_routing::ReviewConfig::default().shapes["standard"];
        let (id, _) =
            review_identity("head", "standard", shape, "brief", 0, 0, 0).map_err(|e| e.message)?;
        let run_id = RunId::from_bytes([9; 16]);
        let mut prepared = run::OperationRecord {
            extra: Default::default(),
            schema_version: 1,
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            operation_id: id.clone(),
            kind: run::OperationKind::Dispatch,
            request_digest: provenant_core::Digest::from_bytes([3; 32]),
            run_id,
            state: run::OperationState::Prepared,
        };
        let missing = sealed_operation(Some(prepared.clone()), |_| Err(run::RunError::NotFound))?;
        assert_eq!(missing, None);
        let unsealed = recover_slot("head", "standard", shape, "brief", 0, 0, |candidate| {
            assert_eq!(*candidate, id);
            Ok(if missing.is_some() {
                SlotProbe::Reattach(run_id)
            } else {
                SlotProbe::Vacant
            })
        })
        .map_err(|e| e.message)?;
        assert_eq!(unsealed.ordinal, 0);
        assert_eq!(unsealed.operation_id, id);
        assert_eq!(unsealed.run, None); // submit repairs the original prepared operation
        // Once sealed, even a differently constructed request must bypass submit.
        let sealed = sealed_operation(Some(prepared.clone()), |_| Ok(()))?;
        let replayed = recover_slot("head", "standard", shape, "brief", 0, 0, |_| {
            Ok(SlotProbe::Reattach(sealed.ok_or_else(|| {
                Fail::new(Status::Error, ErrorKind::NotFound, "missing")
            })?))
        })
        .map_err(|e| e.message)?;
        assert_eq!(replayed.run, Some(run_id));
        assert_eq!(replayed.ordinal, 0);
        prepared.state = run::OperationState::Accepted;
        assert!(matches!(
            sealed_operation(Some(prepared), |_| Err(run::RunError::NotFound)),
            Err(run::RunError::NotFound)
        ));
        let next = recover_slot("head", "standard", shape, "brief", 0, 0, |candidate| {
            Ok(if *candidate == id {
                SlotProbe::Failed
            } else {
                SlotProbe::Vacant
            })
        })
        .map_err(|e| e.message)?;
        assert_eq!(next.ordinal, 1);
        assert_ne!(next.operation_id, id);
        let retry = recover_slot("head", "standard", shape, "brief", 0, 0, |candidate| {
            Ok(if *candidate == id {
                SlotProbe::Failed
            } else {
                SlotProbe::Reattach(run_id)
            })
        })
        .map_err(|e| e.message)?;
        assert_eq!(retry.ordinal, 1);
        assert_eq!(retry.run, Some(run_id));
        Ok(())
    }
    #[test]
    fn retries_failed_and_cancelled_but_reattaches_succeeded_and_running()
    -> Result<(), Box<dyn std::error::Error>> {
        let id = RunId::from_bytes([4; 16]);
        let terminal = |outcome| run::AttemptState::Terminal {
            extra: Default::default(),
            outcome,
            exit_code: None,
            finished_at: WallTime::from_micros(1).unwrap(),
            reason: None,
        };
        assert_eq!(
            probe_state(id, &terminal(run::Outcome::Failed)),
            SlotProbe::Failed
        );
        assert_eq!(
            probe_state(id, &terminal(run::Outcome::Cancelled)),
            SlotProbe::Failed
        );
        assert_eq!(
            probe_state(id, &terminal(run::Outcome::QuotaLimited)),
            SlotProbe::Failed
        );
        assert_eq!(
            probe_state(id, &terminal(run::Outcome::Succeeded)),
            SlotProbe::Reattach(id)
        );
        assert_eq!(
            probe_state(
                id,
                &run::AttemptState::Running {
                    extra: Default::default()
                }
            ),
            SlotProbe::Reattach(id)
        );
        assert_eq!(
            probe_state(id, &terminal(run::Outcome::InputRequired)),
            SlotProbe::Reattach(id)
        );
        Ok(())
    }
    #[test]
    fn review_has_no_extra_write_grants() {
        let slot = ReviewSlot {
            group: 0,
            index: 0,
            ordinal: 0,
            pool: "junior".into(),
            lens: None,
            distinct: false,
            operation_id: OperationId::parse("test").unwrap(),
            task_id: TaskId::from_bytes([1; 16]),
            run: None,
            ready: false,
            family: None,
        };
        let task = review_task(&slot, "head", "brief", vec![]);
        assert!(task.access.write_roots.is_empty());
        assert!(task.access.is_default());
        assert!(task.worktree.is_none());
    }
    #[test]
    fn blocked_family_group_does_not_block_other_reviewers() {
        let make = |group, distinct, run| ReviewSlot {
            group,
            index: 0,
            ordinal: 0,
            pool: "junior".into(),
            lens: None,
            distinct,
            operation_id: OperationId::parse("test").unwrap(),
            task_id: TaskId::from_bytes([1; 16]),
            run,
            ready: false,
            family: None,
        };
        let slots = vec![
            make(0, true, Some(RunId::from_bytes([1; 16]))),
            make(0, true, None),
            make(1, false, None),
            make(1, false, None),
            make(2, true, None),
        ];
        assert!(!can_submit(&slots, 1));
        assert!(can_submit(&slots, 2));
        assert!(can_submit(&slots, 3));
        assert!(can_submit(&slots, 4));
    }
    #[test]
    fn verdict_summary_counts_unparsed_and_not_submitted() {
        let data = json!({"reviewers":[{"verdict":"approve"},{"verdict":"request_changes"},{"verdict":"unknown"},{"state":"not_submitted"}]});
        assert_eq!(
            aggregate_verdicts(&data),
            json!({"approve":1,"request_changes":1,"inconclusive":0,"unknown":1,"not_submitted":1})
        );
    }
    #[test]
    fn verdict_reads_only_a_complete_bounded_prefix() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("result");
        std::fs::write(
            &path,
            format!("\nVerdict: approve\n{}", "x".repeat(1_100_000)),
        )?;
        assert_eq!(review_verdict_file(&path), Some("approve"));
        std::fs::write(&path, format!("Verdict: approve{}", " ".repeat(9000)))?;
        assert_eq!(review_verdict_file(&path), None);
        std::fs::write(&path, b"Verdict: approve")?;
        assert_eq!(review_verdict_file(&path), Some("approve"));
        Ok(())
    }
    #[test]
    fn explicit_shape_skips_range_validation() {
        assert_eq!(
            selection_range(Some("quick"), Some("-invalid")).unwrap(),
            None
        );
        assert!(selection_range(None, Some("-invalid")).is_err());
        assert_eq!(
            selection_range(None, Some("main..HEAD")).unwrap(),
            Some("main..HEAD")
        );
    }
    #[test]
    fn text_reports_partial_run_and_actual_receipt() {
        let data = json!({"shape":"deep","selection_source":"by_risk","reviewers":[{"run_id":"abc","operation_id":"op","applied_route":{"model":"codex/test","effort":"high"},"state":"failed","verdict":"unknown","result_path":"/output","substitution":true}]});
        let text = review_text(&data);
        for expected in [
            "deep (by_risk)",
            "run=abc",
            "codex/test@high",
            "state=failed",
            "verdict=unknown",
            "/output",
            "family substitution",
            "resubmit failed or cancelled",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }
}
