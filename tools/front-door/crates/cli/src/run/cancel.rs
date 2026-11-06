use super::*;

/// Cancel every attempt the target names. Queued work ends at once; started
/// work gets a stop request its guardian acts on. Requested is not exit.
pub fn cancel(command: Cancel, edge: &Edge<'_>) -> Reply {
    let id = command.operation_id.clone();
    let mut out = cancel_inner(command, edge);
    operation_reply(&mut out, &id);
    out
}

pub(super) fn cancel_inner(mut command: Cancel, edge: &Edge<'_>) -> Reply {
    let candidate = match targets::resolve(edge, &command.target, true) {
        Ok(candidate) => candidate,
        Err(fail) => return reply("stop", Err(fail)),
    };
    command.target = candidate.target.clone();
    match targets::placement(edge, &candidate.target, true) {
        Ok(Some(placement)) => {
            if crate::fabric::in_lane() {
                return reply(
                    "stop",
                    Err(Fail::new(
                        Status::Refused,
                        ErrorKind::LaneCoordinationRefused,
                        "coordination mutations are not allowed inside a lane",
                    )),
                );
            }
            return remote::cancel(&command, &placement, edge);
        }
        Ok(None) => {}
        Err(fail) => return reply("stop", Err(fail)),
    }
    let mut status = Status::Ok;
    let mut out = reply(
        "stop",
        (|| {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let (session, selected_target) = targets::open_resolved(edge, candidate, &call)?;
            let reason = command.reason.as_deref().unwrap_or("cancelled by request");
            let targets = [selected_target];
            let selected = select(&session, &targets, None, session.context.actor_id, &call)?;
            if selected.is_empty() {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::NotFound,
                    "no matching attempts",
                ));
            }
            let selected_ids = selected.clone();
            let target = &targets[0];
            let resolved = targets::local(&session, target, &call)?;
            let bound = match (resolved.attempt, resolved.task) {
                (Some(attempt), _) => attempt.to_string(),
                (None, Some(task)) => format!("{}/{task}", resolved.run),
                (None, None) => resolved.run.to_string(),
            };
            session
                .service()
                .bind_cancel(&command.operation_id, &bound, &call)?;
            let mut receipts = Vec::new();
            let mut human = String::new();
            let mut requested = false;
            let mut cloud_seen = false;
            let mut local_wait_targets = Vec::new();
            for id in selected {
                let record = session.store.record(id, &call)?;
                if cloud_owner::placement(&session.store, &record, &call)
                    .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?
                    .is_some()
                {
                    cloud_seen = true;
                    let cloud = cloud_owner::load(&session.store, id, &call)
                        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
                    let observed = match cloud {
                        Some(job) => cloud_owner::observe_installed(&session.store, id, &job, &call, edge.cwd, true),
                        None => Err("cloud submission identity is not recorded; cancellation is unconfirmed".into()),
                    };
                    match observed {
                        Ok(job) => {
                            let view = cloud_owner::view(&job);
                            human.push_str(&format!("{id} cloud {}\n", state_tag(&job.state)));
                            receipts.push(json!({"attempt_id": id, "cloud": view}));
                        }
                        Err(error) => {
                            status = Status::Failed;
                            human.push_str(&format!(
                                "{id} cloud cancellation unconfirmed: {}\n",
                                plain_text(&error, usize::MAX)
                            ));
                            receipts.push(json!({"attempt_id": id, "error": {"message": error}, "cancellation_unconfirmed": true}));
                        }
                    }
                    continue;
                }
                local_wait_targets.push(Target {
                    selection: None,
                    run: None,
                    task: None,
                    attempt: Some(id),
                });
                // One failure must not hide the receipts already earned.
                let receipt = request(&clock, &cancel)
                    .and_then(|call| Ok(session.service().cancel(id, reason, &call)?));
                let row = match receipt {
                    Ok(receipt) => {
                        if matches!(receipt, run::CancelReceipt::Unobservable { .. }) {
                            status = Status::Failed;
                        }
                        requested |= matches!(
                            receipt,
                            run::CancelReceipt::Requested | run::CancelReceipt::AlreadyRequested
                        );
                        human.push_str(&format!("{id} {}\n", state_tag(&receipt)));
                        json!({ "attempt_id": id, "receipt": receipt })
                    }
                    Err(fail) => {
                        status = Status::Failed;
                        human.push_str(&format!("{id} error: {}\n", fail.message));
                        json!({ "attempt_id": id, "error": command_error(&fail) })
                    }
                };
                receipts.push(row);
            }
            let mut data = json!({ "operation_id": command.operation_id, "receipts": receipts });
            if requested {
                // A request has no actor once the guardian is gone, so recover
                // the run now: ending proven orphans and settling is the same
                // path `reconcile` takes, and a live guardian is left alone.
                let scoped_cwd = targets::cwd(edge, session.context.project_id)?;
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
                let recovered = request(&clock, &cancel).and_then(|call| {
                    Ok(session
                        .service()
                        .reconcile(resolved.run, &registry, &call)?)
                });
                match recovered {
                    Ok(found) => {
                        let mut rows = Vec::new();
                        for (id, state) in &found {
                            if matches!(
                                state,
                                run::Reconciled::Uncertain { .. } | run::Reconciled::Escaped { .. }
                            ) {
                                status = Status::Failed;
                            }
                            human.push_str(&format!("{id} {}\n", reconcile_summary(state)));
                            let mut found = json!(state);
                            if let run::Reconciled::TemporaryRetained { path, .. } = state {
                                found["path"] = json!(
                                    provenant_records::StructuredPathWire::from_path(path).ok()
                                );
                            }
                            rows.push(json!({ "attempt_id": id, "found": found }));
                        }
                        data["recovered"] = Value::Array(rows);
                    }
                    Err(fail) => {
                        status = Status::Failed;
                        human.push_str(&format!("recovery error: {}\n", fail.message));
                    }
                }
            }
            if let Some(seconds) = command
                .wait
                .filter(|_| !cloud_seen || !local_wait_targets.is_empty())
            {
                let wait_targets: &[Target] = if cloud_seen {
                    &local_wait_targets
                } else {
                    &targets
                };
                let (lanes, rendered, timed_out) = wait_lanes(
                    &session,
                    wait_targets,
                    Condition::All,
                    Duration::from_secs(seconds),
                    edge,
                )?;
                apply_cancel_wait(&mut data, &lanes, timed_out, &mut status);
                human.push_str(&rendered);
            } else {
                let lanes = selected_ids
                    .into_iter()
                    .map(|id| session.service().status(id, &call))
                    .collect::<Result<Vec<_>, _>>()?;
                let waits = session.store.lane_waits(&lanes, &call);

                let (rendered, _) = render_lanes(
                    &lanes,
                    &waits,
                    None,
                    session.context.state_root.as_path(),
                    None,

                );
                data["lanes"] = rendered["lanes"].clone();
            }
            Ok((data, human))
        })(),
    );
    if out.result.status == Status::Ok {
        out.result.status = status;
    }
    out
}
