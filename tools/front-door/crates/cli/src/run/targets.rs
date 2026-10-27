//! Resolve CLI selections at the shared run owner boundary.
use super::{Edge, Fail, Session, Target, remote, run};

/// Peer placements and local records must agree that a bare ID is unique.
pub(super) fn placement(
    edge: &Edge<'_>,
    target: &Target,
    allow_run: bool,
) -> Result<Option<remote::Placement>, Fail> {
    let placement = remote::find_target(edge.roots, target, allow_run)?;
    if placement.is_none() || target.run.is_some() {
        return Ok(placement);
    }
    let clock = super::SystemClock::new();
    let cancel = super::CancelToken::new();
    let call = super::request(&clock, &cancel)?;
    let (_, store) = super::open_read_only(edge, &call)?;
    if let Some(store) = store {
        match run::Service::new(&store).resolve_target(
            target.attempt.ok_or(run::RunError::NotFound)?,
            allow_run,
            &call,
        ) {
            Ok(_) | Err(run::RunError::Conflict) => return Err(run::RunError::Conflict.into()),
            Err(run::RunError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(placement)
}

pub(super) fn local(
    session: &Session,
    target: &Target,
    call: &run::Request<'_>,
) -> Result<run::LaneTarget, Fail> {
    match target.run {
        Some(run) => {
            // `RUN/ATTEMPT`: the second part is an attempt of this run, not a task.
            if let (Some(task), None) = (target.task, target.attempt) {
                let candidate = provenant_core::AttemptId::from_bytes(*task.as_bytes());
                if session
                    .service()
                    .status(candidate, call)
                    .is_ok_and(|record| record.run_id == run)
                {
                    return Ok(run::LaneTarget {
                        run,
                        task: None,
                        attempt: Some(candidate),
                    });
                }
            }
            Ok(run::LaneTarget {
                run,
                task: target.task,
                attempt: target.attempt,
            })
        }
        None => Ok(session.service().resolve_target(
            target.attempt.ok_or(run::RunError::NotFound)?,
            true,
            call,
        )?),
    }
}
