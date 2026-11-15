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
            repair: Some("Inspect the reported custody or settlement reason, then rerun `provenant reconcile NAME`.".into()),
        });
    }
    out
}

pub(super) const SCAN: u64 = 320;

fn reservation_root(context: &run::ExecutionContext) -> PathBuf {
    context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
}

pub(super) fn reconcile_inner(
    command: Reconcile,
    edge: &Edge<'_>,
    fenced: &mut bool,
) -> Result<(Value, String), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    if let Some(target) = command.abandon {
        let call = request(&clock, &cancel)?;
        let (session, id) = key::open_attempt(
            edge,
            &target,
            crate::Want::Attempt { settled: false },
            &call,
        )?;
        let reason = command
            .reason
            .ok_or(run::RunError::Validation("abandonment requires a reason"))?;
        let state =
            session
                .service()
                .abandon_settlement(id, &reason, &request(&clock, &cancel)?)?;
        return Ok((
            json!({"target": target.to_string(), "attempt_id": id, "found": state}),
            format!("{target} {}\n", reconcile_summary(&state)),
        ));
    }
    let session = open(edge, &request(&clock, &cancel)?)?;
    let (attempts, truncated) = if command.targets.is_empty() {
        let call = request(&clock, &cancel)?;
        let mut attempts = session.store.open_attempts_readonly(SCAN + 1, &call)?;
        let truncated = attempts.len() as u64 > SCAN;
        attempts.truncate(SCAN as usize);
        (attempts, truncated)
    } else {
        let mut attempts = Vec::new();
        for target in &command.targets {
            let resolved = key::resolve_target(edge, target, crate::Want::Work)?;
            attempts.extend(key::local_attempts(
                &session,
                &resolved,
                &request(&clock, &cancel)?,
            )?);
        }
        attempts.sort();
        attempts.dedup();
        (attempts, false)
    };
    let mut rows = Vec::new();
    let mut human = String::new();
    for run in attempts {
        let registry = Registry {
            roots: Some(edge.roots),
            home: Some(edge.home),
            assets: Some(edge.assets),
            cwd: Some(edge.cwd),
            state_root: Some(session.context.state_root.as_path()),
            project_id: Some(session.context.project_id),
            host: None,
            cancel: Some(&cancel),
            repo_facts: session.store.repo_facts().clone(),
        };
        // Each step gets a fresh budget; recovery waits on custody locks.
        let found = match request(&clock, &cancel)
            .and_then(|call| Ok(session.service().reconcile(run, &registry, &call)?))
        {
            Ok(found) => found,
            Err(fail) => {
                *fenced = true;
                human.push_str(&format!("{run} error: {}\n", fail.message));
                rows.push(json!({ "attempt_id": run, "error": { "code": fail.kind.code(), "message": fail.message } }));
                continue;
            }
        };
        let started = if found.iter().any(|(_, r)| *r == run::Reconciled::Queued) {
            request(&clock, &cancel).and_then(|call| launch(&session, run, edge, &call))
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
                check_launch_policy(started)
                    .err()
                    .map(|failure| failure.message),
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
                human.push_str(&format!("{id} {}\n", reconcile_summary(state)));
                json!({ "attempt_id": id, "found": state })
            })
            .collect();
        *fenced |= !unknown.is_empty() || launch_error.is_some();
        rows.push(json!({
            "attempt_id": run,
            "attempts": attempts,
            "launched": launched,
            "launch_unknown": unknown,
            "launch_error": launch_error,
            "started": started.as_ref().ok(),
        }));
    }
    if rows.is_empty() {
        human.push_str("no open attempts\n");
    }
    if truncated {
        human.push_str(&format!(
            "more than {SCAN} open attempts; name the rest as targets\n"
        ));
    }
    let session = open(edge, &request(&clock, &cancel)?)?;
    let listing = session.store.writer_fence_listing();
    let mut reservations = match listing {
        Ok(listing) => listing,
        Err(error) => {
            let mut listing = provenant_supervisor::custody::ReservationListing::default();
            listing
                .issues
                .push(provenant_supervisor::custody::ReservationIssue {
                    reservation: reservation_root(&session.context),
                    reason: error.to_string(),
                });
            listing
        }
    };
    reservations
        .reservations
        .sort_by(|left, right| left.reservation.cmp(&right.reservation));
    reservations
        .reservations
        .dedup_by(|left, right| left.scope == right.scope && left.reservation == right.reservation);
    reservations.issues.sort_by(|left, right| {
        (&left.reservation, &left.reason).cmp(&(&right.reservation, &right.reason))
    });
    reservations.issues.dedup_by(|left, right| {
        left.reservation == right.reservation && left.reason == right.reason
    });
    *fenced |= !reservations.issues.is_empty();
    for issue in &reservations.issues {
        human.push_str(&format!(
            "writer reservation requires recovery: {}: {}\n",
            issue.reservation.display(),
            issue.reason
        ));
    }
    Ok((
        json!({ "runs": rows, "truncated": truncated, "writer_reservations": reservations.reservations, "writer_reservation_errors": reservations.issues }),
        human,
    ))
}
