use super::*;

pub(in crate::fabric) fn advance_after_remote_terminal(
    run: &provenant_fabric_runtime::graph::GraphRun,
    remote_terminal: bool,
    advance: impl FnOnce() -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
    if remote_terminal
        && run
            .checkpoint
            .nodes
            .iter()
            .any(|node| !node.state.settled())
    {
        advance()
    } else {
        Ok(run.clone())
    }
}

fn graph_run_has_remote_pending(
    run: &provenant_fabric_runtime::graph::GraphRun,
    local_host: Option<provenant_core::HostId>,
    target: Option<&super::super::unified::GraphTarget>,
) -> bool {
    use provenant_fabric_runtime::graph::AttemptState;
    run.checkpoint
        .nodes
        .iter()
        .filter(|node| {
            target.is_none_or(|target| {
                target
                    .node
                    .as_ref()
                    .is_none_or(|id| &node.id == id || node.subtree_cancel.as_deref() == Some(id))
            })
        })
        .flat_map(|node| node.recorded_attempts())
        .filter(|attempt| {
            attempt.state != AttemptState::Terminal
                && target
                    .and_then(|target| target.attempt.as_deref())
                    .is_none_or(|reference| attempt.attempt_ref.as_deref() == Some(reference))
        })
        .any(|attempt| {
            let Some(reference) = attempt.attempt_ref.as_deref() else {
                return false;
            };
            let Some(local_host) = local_host.as_ref() else {
                return true;
            };
            !reference
                .split_once('/')
                .and_then(|(owner, _)| owner.parse::<provenant_core::HostId>().ok())
                .is_some_and(|owner| &owner == local_host)
        })
}

pub(super) fn observation_clock_error(error: provenant_core::TimeError) -> FacadeError {
    FacadeError::StateCorruptDetail {
        detail: format!("graph observation clock: {error}"),
    }
}

fn next_launch_retry_delay(
    run: &provenant_fabric_runtime::graph::GraphRun,
    clock: &(impl provenant_core::Clock + ?Sized),
) -> Result<Option<std::time::Duration>, provenant_core::TimeError> {
    let wake_at = run
        .checkpoint
        .nodes
        .iter()
        .filter_map(|node| node.launch_retry.as_ref()?.wake_at)
        .min();
    let Some(wake_at) = wake_at else {
        return Ok(None);
    };
    let now = clock.wall_now()?;
    Ok(Some(std::time::Duration::from_micros(
        wake_at.as_micros().saturating_sub(now.as_micros()).max(0) as u64,
    )))
}

pub(super) fn launch_retry_due(
    run: &provenant_fabric_runtime::graph::GraphRun,
    clock: &(impl provenant_core::Clock + ?Sized),
) -> Result<bool, provenant_core::TimeError> {
    let wake_at = run
        .checkpoint
        .nodes
        .iter()
        .filter_map(|node| node.launch_retry.as_ref()?.wake_at)
        .min();
    let Some(wake_at) = wake_at else {
        return Ok(false);
    };
    Ok(clock.wall_now()? >= wake_at)
}

pub(in crate::fabric) fn graph_wait_needs_refresh(
    wake: crate::status_wait::ObservationWake,
    launch_retry_due: bool,
) -> bool {
    use crate::status_wait::ObservationWake;
    match wake {
        ObservationWake::Changed | ObservationWake::PacedFallback => true,
        ObservationWake::NativeTimeout => launch_retry_due,
        ObservationWake::Woken | ObservationWake::Cancelled => false,
    }
}

#[derive(Default)]
pub(super) struct GraphObservationHistory {
    remote_source_seen: bool,
    pub(super) reader_woken: bool,
    /// The most recent failed remote reading. A later success clears it, so a
    /// wait that ends on its budget names the last failure rather than the
    /// first one it saw.
    last_remote_error: Option<FacadeError>,
}
impl GraphObservationHistory {
    fn record(
        &mut self,
        run: &provenant_fabric_runtime::graph::GraphRun,
        local_host: Option<provenant_core::HostId>,
        target: Option<&super::super::unified::GraphTarget>,
    ) -> bool {
        let remote_pending = graph_run_has_remote_pending(run, local_host, target);
        self.remote_source_seen |= remote_pending;
        remote_pending
    }

    /// An unreachable home endpoint is a pending observation, not a failed
    /// wait: keep reading and keep only the newest error.
    fn record_remote(&mut self, result: Result<(), FacadeError>) {
        self.last_remote_error = result.err();
    }

    pub(super) fn report(&self, observation: &crate::status_wait::LocalObservation) -> String {
        observation.report(self.remote_source_seen)
    }
}

fn interval_micros(interval: std::time::Duration) -> u64 {
    u64::try_from(interval.as_micros()).unwrap_or(u64::MAX)
}

pub(in crate::fabric) fn next_remote_deadline(
    previous: Option<u64>,
    remote_pending: bool,
    remote_due: bool,
    now: u64,
    interval: u64,
) -> Option<u64> {
    if !remote_pending {
        None
    } else if previous.is_none() || remote_due {
        Some(now.saturating_add(interval))
    } else {
        previous
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn wait_graph_updates<C: provenant_core::Clock + ?Sized>(
    mut run: provenant_fabric_runtime::graph::GraphRun,
    wait: WaitBudget,
    clock: &C,
    cancel: &CancelToken,
    observation: &mut crate::status_wait::LocalObservation,
    local_host: Option<provenant_core::HostId>,
    remote_target: Option<&super::super::unified::GraphTarget>,
    watch_launch_retries: bool,
    mut active: impl FnMut(&provenant_fabric_runtime::graph::GraphRun) -> bool,
    mut observe: impl FnMut(
        &provenant_fabric_runtime::graph::GraphRun,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
    mut observe_remote: impl FnMut(
        &provenant_fabric_runtime::graph::GraphRun,
    )
        -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
) -> Result<
    (
        provenant_fabric_runtime::graph::GraphRun,
        GraphObservationHistory,
    ),
    FacadeError,
> {
    use crate::status_wait::ObservationWake;
    let interval = observation.interval();
    let mut history = GraphObservationHistory::default();
    let mut remote_pending = history.record(&run, local_host, remote_target);
    let mut remote_deadline = next_remote_deadline(
        None,
        remote_pending,
        false,
        clock.mono_now().as_micros(),
        interval_micros(interval),
    );

    // The observer is registered before this refresh, closing the race between
    // submission/stop and the first status read.
    if active(&run) && !cancel.is_cancelled() && wait.remaining(clock.mono_now()).as_micros() > 0 {
        run = observe(&run)?;
        remote_pending = history.record(&run, local_host, remote_target);
        if remote_pending {
            let observed = observe_remote(&run);
            if let Ok(observed) = &observed {
                run = observed.clone();
                remote_pending = history.record(&run, local_host, remote_target);
            }
            history.record_remote(observed.map(|_| ()));
        }
        remote_deadline = next_remote_deadline(
            remote_deadline,
            remote_pending,
            remote_pending,
            clock.mono_now().as_micros(),
            interval_micros(interval),
        );
    }

    while active(&run) && !cancel.is_cancelled() {
        let now = clock.mono_now();
        let remaining = wait.remaining(now).as_micros();
        if remaining == 0 {
            break;
        }
        let retry_due = watch_launch_retries
            && launch_retry_due(&run, clock).map_err(observation_clock_error)?;
        let remote_due = remote_deadline.is_some_and(|deadline| now.as_micros() >= deadline);
        if retry_due || remote_due {
            if retry_due {
                run = observe(&run)?;
                remote_pending = history.record(&run, local_host, remote_target);
            }
            if remote_due && remote_pending {
                let observed = observe_remote(&run);
                if let Ok(observed) = &observed {
                    run = observed.clone();
                    remote_pending = history.record(&run, local_host, remote_target);
                }
                history.record_remote(observed.map(|_| ()));
            }
            remote_deadline = next_remote_deadline(
                remote_deadline,
                remote_pending,
                remote_due,
                clock.mono_now().as_micros(),
                interval_micros(interval),
            );
            continue;
        }

        let mut maximum_micros = remaining;
        if watch_launch_retries
            && let Some(delay) =
                next_launch_retry_delay(&run, clock).map_err(observation_clock_error)?
        {
            maximum_micros = maximum_micros.min(interval_micros(delay));
        }
        if let Some(deadline) = remote_deadline {
            maximum_micros = maximum_micros.min(deadline.saturating_sub(now.as_micros()));
        }
        let wake = observation
            .wait(
                std::time::Duration::from_micros(maximum_micros),
                cancel,
                clock,
            )
            .map_err(|error| FacadeError::StateCorruptDetail {
                detail: format!("graph observation clock: {error}"),
            })?;
        if matches!(wake, ObservationWake::Woken | ObservationWake::Cancelled) {
            history.reader_woken = true;
            break;
        }
        if wait.remaining(clock.mono_now()).as_micros() == 0 {
            break;
        }
        let now = clock.mono_now();
        let retry_due = watch_launch_retries
            && launch_retry_due(&run, clock).map_err(observation_clock_error)?;
        let remote_due = remote_deadline.is_some_and(|deadline| now.as_micros() >= deadline);
        if graph_wait_needs_refresh(wake, retry_due) {
            run = observe(&run)?;
            remote_pending = history.record(&run, local_host, remote_target);
        }
        if remote_due && remote_pending {
            let observed = observe_remote(&run);
            if let Ok(observed) = &observed {
                run = observed.clone();
                remote_pending = history.record(&run, local_host, remote_target);
            }
            history.record_remote(observed.map(|_| ()));
        }
        remote_deadline = next_remote_deadline(
            remote_deadline,
            remote_pending,
            remote_due,
            clock.mono_now().as_micros(),
            interval_micros(interval),
        );
    }
    // The wait kept reading through remote failures. If it ended still
    // pending, name the last failed reading on the returned run so the final
    // status line reports that failure instead of the first one held at the
    // start. A settled or cancelled wait has no pending failure to name.
    if active(&run) {
        if let Some(error) = history.last_remote_error.take() {
            run.advance_error = Some(error.to_string());
        }
    } else {
        history.last_remote_error = None;
    }
    Ok((run, history))
}

/// Observe a submitted graph within the caller's original wait budget.
#[allow(clippy::too_many_arguments)]
pub(in crate::fabric) fn wait_submission<C: provenant_core::Clock + ?Sized>(
    run: provenant_fabric_runtime::graph::GraphRun,
    wait: WaitBudget,
    clock: &C,
    cancel: &CancelToken,
    observation: &mut crate::status_wait::LocalObservation,
    local_host: Option<provenant_core::HostId>,
    mut observe: impl FnMut(&str) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
    observe_remote: impl FnMut(
        &provenant_fabric_runtime::graph::GraphRun,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
) -> Result<(provenant_fabric_runtime::graph::GraphRun, String, bool), FacadeError> {
    let (run, history) = wait_graph_updates(
        run,
        wait,
        clock,
        cancel,
        observation,
        local_host,
        None,
        true,
        |run| run.status == GraphStatus::Pending,
        |run| observe(&run.checkpoint.graph_id),
        observe_remote,
    )?;
    Ok((run, history.report(observation), !history.reader_woken))
}
