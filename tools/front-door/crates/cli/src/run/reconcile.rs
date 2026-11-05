use super::*;

pub(super) fn state_tag(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.get("state").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
}

pub(super) fn reconcile_summary(state: &run::Reconciled) -> String {
    match state {
        run::Reconciled::TemporaryRetained { path, reason } => format!(
            "temporary_retained {}: {}",
            plain_text(&path.to_string_lossy(), usize::MAX),
            plain_text(reason, usize::MAX)
        ),
        run::Reconciled::Settling { .. } | run::Reconciled::Abandoned { .. } => {
            plain_text(&state.to_string(), usize::MAX)
        }
        run::Reconciled::Uncertain { reason } => {
            format!("uncertain: {}", plain_text(&reason.to_string(), usize::MAX))
        }
        _ => state_tag(state),
    }
}

/// Recover runs whose guardian or submitter died: commit durable exits,
/// withdraw launches that never reached a guardian, and start queued work.
/// Uncertain custody stays fenced, is reported, and fails the command.
pub fn reconcile(command: Reconcile, edge: &Edge<'_>) -> Reply {
    let mut fenced = false;
    let outcome = reconcile_inner(command, edge, &mut fenced);
    reconcile_reply(outcome, fenced)
}

pub(super) fn reconcile_reply(outcome: Result<(Value, String), Fail>, fenced: bool) -> Reply {
    let mut out = reply("reconcile", outcome);
    if fenced && out.result.status == Status::Ok {
        out.result.status = Status::Failed;
        out.result.error = Some(CommandError {
            diagnostic: None,
            code: "RECONCILE_INCOMPLETE".into(),
            message: out.human.trim().to_owned(),
            cause: None,
            repair: Some("Inspect the reported custody or settlement reason, then retry `provenant reconcile --id <run>`.".into()),
        });
    }
    out
}

pub(super) const SCAN: u64 = 320;

pub(super) fn reconcile_inner(
    command: Reconcile,
    edge: &Edge<'_>,
    fenced: &mut bool,
) -> Result<(Value, String), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    if let Some(target) = command.abandon {
        let (session, selected) =
            targets::open(edge, &target.0, false, &request(&clock, &cancel)?)?;
        let id = selected.attempt.ok_or(run::RunError::NotFound)?;
        let reason = command
            .reason
            .ok_or(run::RunError::Validation("abandonment requires a reason"))?;
        let state =
            session
                .service()
                .abandon_settlement(id, &reason, &request(&clock, &cancel)?)?;
        return Ok((
            json!({"run_id": selected.run, "attempt_id": id, "found": state}),
            format!("{id} {}\n", reconcile_summary(&state)),
        ));
    }
    let (runs, truncated) = if command.id.is_empty() {
        let session = open(edge, &request(&clock, &cancel)?)?;
        let call = request(&clock, &cancel)?;
        let initial = session.store.open_runs(SCAN + 1, &call)?;
        let mut finished = Vec::new();
        for run in &initial {
            if cloud_owner::finished_run(&session.store, *run, &call).unwrap_or(false) {
                finished.push(*run);
            }
        }
        // Finished cloud jobs retain their advisory record. Scan past them so
        // they cannot crowd live jobs out of the bounded recovery page.
        let scan_cap = SCAN * 32;
        let mut runs = if finished.is_empty() {
            initial
        } else {
            session.store.open_runs(scan_cap + 1, &call)?
        };
        let exhausted = runs.len() as u64 > scan_cap;
        if !finished.is_empty() {
            let mut active = Vec::new();
            for run in runs {
                if !cloud_owner::finished_run(&session.store, run, &call).unwrap_or(false) {
                    active.push(run);
                }
            }
            runs = active;
        }
        let truncated = exhausted || runs.len() as u64 > SCAN;
        runs.truncate(SCAN as usize);
        (
            runs.into_iter()
                .map(|run| Target {
                    selection: None,
                    run: Some(run),
                    task: None,
                    attempt: None,
                })
                .collect(),
            truncated,
        )
    } else {
        (command.id, false)
    };
    let mut rows = Vec::new();
    let mut human = String::new();
    for target in runs {
        let (session, selected) = targets::open(edge, &target, true, &request(&clock, &cancel)?)?;
        let run = selected.run.ok_or(run::RunError::NotFound)?;
        let scoped_cwd = targets::cwd(edge, session.context.project_id)?;
        let scoped_edge = Edge {
            cwd: &scoped_cwd,
            ..*edge
        };
        let mut cloud_jobs = Vec::new();
        let mut cloud_ids = std::collections::BTreeSet::new();
        for record in session
            .store
            .pending_attempts(run, &request(&clock, &cancel)?)?
        {
            let call = request(&clock, &cancel)?;
            let Some(placement) = cloud_owner::placement(&session.store, &record, &call)
                .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?
            else {
                continue;
            };
            cloud_ids.insert(record.attempt_id);
            let job = cloud_owner::load(&session.store, record.attempt_id, &call)
                .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
            match job {
                Some(job) => match cloud_owner::observe_installed(
                    &session.store,
                    record.attempt_id,
                    &job,
                    &call,
                    &scoped_cwd,
                    false,
                ) {
                    Ok(job) => {
                        let mut view = cloud_owner::view(&job);
                        view["attempt_id"] = json!(record.attempt_id);
                        human.push_str(&format!(
                            "{run}/{} cloud {}\n",
                            record.attempt_id,
                            state_tag(&job.state)
                        ));
                        cloud_jobs.push(view);
                    }
                    Err(error) => {
                        *fenced = true;
                        human.push_str(&format!(
                            "{run}/{} cloud observation failed: {}\n",
                            record.attempt_id,
                            plain_text(&error, usize::MAX)
                        ));
                        cloud_jobs.push(json!({"attempt_id": record.attempt_id, "error": error}));
                    }
                },
                None => {
                    *fenced = true;
                    human.push_str(&format!("{run}/{} cloud target {} has no recorded submission; retry the original run\n", record.attempt_id, plain_text(&placement.target, usize::MAX)));
                    cloud_jobs.push(json!({"attempt_id": record.attempt_id, "state": "submission_not_recorded"}));
                }
            }
        }
        let registry = Registry {
            roots: Some(edge.roots),
            home: Some(edge.home),
            assets: Some(edge.assets),
            cwd: Some(&scoped_cwd),
            state_root: Some(session.context.state_root.as_path()),
            project_id: Some(session.context.project_id),
            host: None,
            cancel: Some(&cancel),
        };
        // Each step gets a fresh budget; recovery waits on custody locks.
        let mut found = match request(&clock, &cancel)
            .and_then(|call| Ok(session.service().reconcile(run, &registry, &call)?))
        {
            Ok(found) => found,
            Err(fail) => {
                *fenced = true;
                human.push_str(&format!("{run} error: {}\n", fail.message));
                rows.push(json!({ "run_id": run, "error": { "code": fail.kind.code(), "message": fail.message } }));
                continue;
            }
        };
        found.retain(|(id, _)| !cloud_ids.contains(id));
        let started = if found.iter().any(|(_, r)| *r == run::Reconciled::Queued) {
            request(&clock, &cancel).and_then(|call| launch(&session, run, &scoped_edge, &call))
        } else {
            Ok(Vec::new())
        };
        let (launched, unknown, launch_error) = match &started {
            Ok(started) => (
                started
                    .iter()
                    .filter(|s| matches!(s, run::Started::Launched(_)))
                    .count(),
                launch_unknown(started),
                None,
            ),
            Err(fail) => (0, Vec::new(), Some(fail.message.clone())),
        };
        let attempts: Vec<Value> = found
            .iter()
            .map(|(id, state)| {
                if matches!(
                    state,
                    run::Reconciled::Uncertain { .. }
                        | run::Reconciled::Escaped { .. }
                        | run::Reconciled::Settling {
                            blocking_reason: Some(_),
                            ..
                        }
                ) {
                    *fenced = true;
                }
                human.push_str(&format!("{run}/{id} {}\n", reconcile_summary(state)));
                json!({ "attempt_id": id, "found": state })
            })
            .collect();
        *fenced |= !unknown.is_empty() || launch_error.is_some();
        rows.push(json!({
            "run_id": run,
            "attempts": attempts,
            "launched": launched,
            "launch_unknown": unknown,
            "launch_error": launch_error,
            "cloud_jobs": cloud_jobs,
        }));
    }
    if rows.is_empty() {
        human.push_str("no open runs\n");
    }
    if truncated {
        human.push_str(&format!(
            "more than {SCAN} open runs; name the rest with --id\n"
        ));
    }
    let reservations = open(edge, &request(&clock, &cancel)?)?
        .store
        .writer_fence_listing()?;
    *fenced |= !reservations.issues.is_empty();
    for issue in &reservations.issues {
        human.push_str(&format!(
            "writer reservation unreadable: {}: {}\n",
            issue.reservation.display(),
            issue.reason
        ));
    }
    Ok((
        json!({ "runs": rows, "truncated": truncated, "writer_reservations": reservations.reservations, "writer_reservation_errors": reservations.issues }),
        human,
    ))
}
