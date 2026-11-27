use super::*;

/// Resolve every target before a wait starts. Inside a lane the host performs
/// one status read for validation and the wait's initial snapshot.
pub(crate) fn check_targets(
    targets: &[provenant_cli::Target],
    edge: &provenant_cli::run::Edge<'_>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
    mut observed: impl FnMut(&provenant_cli::Target, Vec<Value>),
) -> Result<(), Box<dyn std::error::Error>> {
    if targets.is_empty() {
        return Ok(());
    }
    wait_remaining(deadline, cancel)?;
    if provenant_cli::fabric::in_lane() {
        for target in targets {
            let left = wait_remaining(deadline, cancel)?;
            let operation = provenant_fabric_runtime::graph::relay::RelayOp::Read {
                input: json!({"op":"status","id":target.to_string()}),
            };
            let value =
                match provenant_cli::fabric::graph_relay::from_lane(&operation, cancel, left) {
                    Ok(value) => value,
                    Err(error) => {
                        let error = super::super::interface::relay_error(error);
                        if is_target_not_found(&error) {
                            return Err(Box::new(target_fault(ResolveError::NotFound {
                                target: target.to_string(),
                                candidates: Vec::new(),
                            })));
                        }
                        return Err(Box::new(error));
                    }
                };
            wait_remaining(deadline, cancel)?;
            observed(
                target,
                value["rows"]
                    .as_array()
                    .ok_or(FacadeError::StateCorrupt)?
                    .clone(),
            );
        }
        return Ok(());
    }
    let context = read_context(edge)?;
    for target in targets {
        wait_remaining(deadline, cancel)?;
        let Some(context) = &context else {
            return Err(Box::new(target_fault(ResolveError::NotFound {
                target: target.to_string(),
                candidates: Vec::new(),
            })));
        };
        resolve(
            &context.roots.state,
            context.identity.project,
            target,
            Want::Work,
        )
        .map_err(target_fault)?;
        wait_remaining(deadline, cancel)?;
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct WaitGraph {
    pub target: provenant_cli::Target,
    pub error: Option<String>,
    pub rows: Vec<Value>,
}

/// The project's running graphs: what a wait with no target waits for. A
/// paused graph waits on its owner, so it is left out.
pub(crate) fn unsettled_graphs(
    edge: &provenant_cli::run::Edge<'_>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
) -> Result<Vec<WaitGraph>, Box<dyn std::error::Error>> {
    wait_remaining(deadline, cancel)?;
    let context = read_context(edge)?;
    wait_remaining(deadline, cancel)?;
    let Some(context) = context else {
        return Ok(Vec::new());
    };
    let root = &context.roots.state;
    let project = context.identity.project;
    let mut graphs = Vec::new();
    for name in FileJournal::graph_ids(root, project)? {
        wait_remaining(deadline, cancel)?;
        let (error, rows) = match FileJournal::read_status_checkpoint(root, project, &name) {
            Ok(Some(checkpoint)) => {
                let run = graph_snapshot(checkpoint);
                if run.status != provenant_fabric_runtime::graph::GraphStatus::Pending {
                    continue;
                }
                let target = GraphTarget {
                    graph: name.clone(),
                    node: None,
                    attempt: None,
                };
                let mut rows = status_rows(&run, &target);
                status_keys(&mut rows, project)?;
                (None, rows)
            }
            Ok(_) => continue,
            Err(error) => (Some(error.to_string()), Vec::new()),
        };
        graphs.push(WaitGraph {
            target: provenant_cli::Target {
                name,
                node: None,
                attempt: None,
            },
            error,
            rows,
        });
    }
    wait_remaining(deadline, cancel)?;
    Ok(graphs)
}

/// Every wait phase spends the same wall-clock deadline and cancellation token.
#[allow(
    clippy::disallowed_methods,
    reason = "wait transport owns its real-time deadline"
)]
fn wait_remaining(
    deadline: std::time::Instant,
    cancel: &CancelToken,
) -> Result<std::time::Duration, FacadeError> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if cancel.is_cancelled() {
        Err(FacadeError::Cancelled)
    } else if left.is_zero() {
        Err(FacadeError::Timeout)
    } else {
        Ok(left)
    }
}

/// The status rows one target names, read afresh: the wait verb's reader.
/// `session` is opened on first use; a caller that polls keeps it, because
/// opening resolves the repository and runs git.
pub(crate) fn read_status_rows(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let left = wait_remaining(deadline, cancel)?;
    if provenant_cli::fabric::in_lane() {
        let value = provenant_cli::fabric::graph_relay::from_lane(
            &provenant_fabric_runtime::graph::relay::RelayOp::Read {
                input: json!({"op":"status","id":target.to_string()}),
            },
            cancel,
            left,
        )
        .map_err(super::super::interface::relay_error)?;
        let left = wait_remaining(deadline, cancel)?;
        let outcome = crate::fabric::progress::wake_lane_reading(
            &value,
            crate::fabric::progress::WakeReason::Wait,
            left,
        );
        let value = if matches!(
            outcome,
            crate::fabric::progress::WakeOutcome::Advanced { .. }
        ) {
            provenant_cli::fabric::graph_relay::from_lane(
                &provenant_fabric_runtime::graph::relay::RelayOp::Read {
                    input: json!({"op":"status","id":target.to_string()}),
                },
                cancel,
                wait_remaining(deadline, cancel)?,
            )
            .map_err(super::super::interface::relay_error)?
        } else {
            value
        };
        wait_remaining(deadline, cancel)?;
        return Ok(value["rows"]
            .as_array()
            .ok_or(FacadeError::StateCorrupt)?
            .clone());
    }
    let rows = local_status_rows(target, edge, session, cancel, |clock| {
        let left = wait_remaining(deadline, cancel)?;
        Ok(WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(
                u64::try_from(left.as_micros()).unwrap_or(u64::MAX),
            ),
        ))
    })?;
    wait_remaining(deadline, cancel)?;
    Ok(rows)
}

/// The one final snapshot a wait takes after its deadline: the target's rows
/// from the local store, read without blocking. Remote sources get nothing
/// past the original budget, so a lane's relay read is refused and peer
/// enrichment sees an expired budget.
pub(crate) fn final_status_rows(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    if provenant_cli::fabric::in_lane() {
        return Err(FacadeError::Timeout.into());
    }
    local_status_rows(target, edge, session, &CancelToken::new(), |clock| {
        Ok(WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(0),
        ))
    })
}

/// The checkpoint, resolved target, context and peer budget behind one local
/// status read. The context stays in the caller's session so a polling waiter
/// resolves the repository only once.
fn local_status_parts<'a>(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &'a mut Option<Context>,
    peer_budget: impl FnOnce(&SystemClock) -> Result<WaitBudget, FacadeError>,
) -> Result<
    (
        provenant_fabric_runtime::graph::GraphRun,
        GraphTarget,
        &'a Context,
        WaitBudget,
    ),
    Box<dyn std::error::Error>,
> {
    let context = match session {
        Some(context) => context,
        None => session.insert(read_context(edge)?.ok_or_else(|| {
            target_fault(ResolveError::NotFound {
                target: target.to_string(),
                candidates: Vec::new(),
            })
        })?),
    };
    let (resolved, checkpoint) = resolve_checkpoint(
        &context.roots.state,
        context.identity.project,
        target,
        Want::Work,
    )?;
    let target = GraphTarget {
        graph: resolved.graph,
        node: resolved.node,
        attempt: resolved.attempt_ref,
    };
    let clock = SystemClock::new();
    let budget = peer_budget(&clock)?;
    Ok((graph_snapshot(checkpoint), target, context, budget))
}

/// A target's rows from this host's graph checkpoint, enriched within the
/// peer budget `peer_budget` chooses once the local read is done.
fn local_status_rows(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
    cancel: &CancelToken,
    peer_budget: impl FnOnce(&SystemClock) -> Result<WaitBudget, FacadeError>,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let (run, target, context, budget) = local_status_parts(target, edge, session, peer_budget)?;
    let mut rows = status_rows(&run, &target);
    let clock = SystemClock::new();
    enrich_status_rows_within(&mut rows, context, &clock, cancel, budget)?;
    Ok(rows)
}

/// Re-drive the journal-locked advance when a terminal remote attempt says the
/// peer's work is done but this host's checkpoint is still unsettled. The read
/// keeps the peer error as a pending observation; only a successful terminal
/// reading authorises an advance. `advance` is injected so the decision is
/// deterministic and the call can be pinned without a tick or a peer process.
#[cfg(test)]
pub(crate) fn recover_after_remote_terminal(
    run: &provenant_fabric_runtime::graph::GraphRun,
    target: &GraphTarget,
    rows: &mut Vec<Value>,
    local_host: provenant_core::HostId,
    peer_error: Option<FacadeError>,
    advance: impl FnOnce() -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
) -> Result<bool, FacadeError> {
    let remote_rows: Vec<Value> = rows
        .iter()
        .filter(|row| {
            row["kind"] == "attempt"
                && row["attempt_id"].as_str().is_some_and(|reference| {
                    reference.split_once('/').is_some_and(|(owner, _)| {
                        owner.parse::<provenant_core::HostId>() != Ok(local_host)
                    })
                })
        })
        .cloned()
        .collect();
    if remote_rows.is_empty() {
        return Ok(false);
    }
    let terminal =
        super::super::interface::observation::remote_status_reading(&remote_rows, peer_error)?;
    let advanced = if terminal
        && run
            .checkpoint
            .nodes
            .iter()
            .any(|node| !node.state.settled())
    {
        advance()?
    } else {
        run.clone()
    };
    // Only a settled checkpoint replaces the peer-enriched reading: a partial
    // advance that merely relaunched work is reread on the waiter's next poll,
    // keeping this reading's digests and peer facts intact.
    let settled = &advanced != run
        && advanced
            .checkpoint
            .nodes
            .iter()
            .all(|node| node.state.settled());
    if settled {
        *rows = status_rows(&advanced, target);
    }
    Ok(settled)
}

/// The wait verb's graph read: same rows as `read_status_rows`, plus the
/// recovery a terminal remote reading justifies. A wait is still a reader of
/// work; the advance only retries a settlement the peer has already reported,
/// under the graph journal's own lock. Inside a lane the relay owns the read
/// and the peer owns advancement, so this path is host-only.
pub(crate) fn read_status_rows_with_recovery(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<Context>,
    deadline: std::time::Instant,
    cancel: &CancelToken,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
    reason: crate::fabric::progress::WakeReason,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    wait_remaining(deadline, cancel)?;
    if provenant_cli::fabric::in_lane() {
        return read_status_rows(target, edge, session, deadline, cancel);
    }
    let (run, resolved, context, budget) = local_status_parts(target, edge, session, |clock| {
        let left = wait_remaining(deadline, cancel)?;
        Ok(WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(
                u64::try_from(left.as_micros()).unwrap_or(u64::MAX),
            ),
        ))
    })?;
    if reader_wake_budget(&context.roots, &SystemClock::new(), budget).is_none() {
        let mut rows = status_rows(&run, &resolved);
        enrich_status_rows_within(&mut rows, context, &SystemClock::new(), cancel, budget)?;
        return Ok(rows);
    }
    let factory = Factory {
        project_attribution: crate::usage::attribution_enabled(&context.roots),
        context: Context::open(edge.roots.clone(), edge.home, edge.cwd)?,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: edge.home.to_path_buf(),
        cwd: edge.cwd.to_path_buf(),
        assets: edge.assets.to_path_buf(),
        host_launch: None,
        workers,
    };
    let outcome = wake_graph_for_reader(&factory, &resolved.graph, reason, budget, cancel);
    let current = if matches!(
        outcome,
        crate::fabric::progress::WakeOutcome::Advanced { .. }
    ) {
        FileJournal::read_checkpoint(
            &context.roots.state,
            context.identity.project,
            &resolved.graph,
        )?
        .map(graph_snapshot)
        .unwrap_or(run)
    } else {
        run
    };
    let clock = SystemClock::new();
    let mut rows = status_rows(&current, &resolved);
    enrich_status_rows_within(&mut rows, context, &clock, cancel, budget)?;
    wait_remaining(deadline, cancel)?;
    Ok(rows)
}

pub(super) fn missing_graph_context(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<provenant_cli::fabric::EdgeError>(),
        Some(provenant_cli::fabric::EdgeError::Invalid)
    )
}

/// A `wait --pr N` reader also wakes the queue item bound to that PR, so an
/// adopted item advances even before its graph exists. Best effort: a reader
/// without a bound item stays a pure snapshot.
pub(crate) fn wake_pr_item(
    edge: &provenant_cli::run::Edge<'_>,
    project: provenant_core::ProjectId,
    pr: u64,
    deadline: provenant_core::Deadline,
    cancel: &CancelToken,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> crate::fabric::progress::WakeOutcome {
    use super::super::queue_registry::QueueRegistry;
    let clock = SystemClock::new();
    let now = clock.mono_now();
    let budget = WaitBudget::up_to(now, deadline.remaining(now));
    let Some(budget) = reader_wake_budget(edge.roots, &clock, budget) else {
        return crate::fabric::progress::WakeOutcome::Idle;
    };
    let registry = match QueueRegistry::open(edge.roots.state.as_path()) {
        Ok(registry) => registry,
        Err(error) => {
            super::super::interface::warn_operator(format!(
                "WAKE_PR_LOOKUP_FAILED: queue registry: {error}"
            ));
            return crate::fabric::progress::WakeOutcome::Skipped {
                code: "WAKE_PR_LOOKUP_FAILED".into(),
            };
        }
    };
    let queues = match registry.queues(WaitBudget::nonblocking(clock.mono_now()), cancel, &clock) {
        Ok(queues) => queues,
        Err(error) => {
            super::super::interface::warn_operator(format!(
                "WAKE_PR_LOOKUP_FAILED: queue lookup: {error}"
            ));
            return crate::fabric::progress::WakeOutcome::Skipped {
                code: "WAKE_PR_LOOKUP_FAILED".into(),
            };
        }
    };
    let selected = queues
        .into_iter()
        .filter(|record| record.project == project.to_string())
        .find_map(|record| {
            let item = record.items.into_iter().find(|item| {
                item.adoption
                    .as_ref()
                    .is_some_and(|adopted| adopted["pull_request"].as_u64() == Some(pr))
            })?;
            Some((record.queue_id, item.identity.item))
        });
    let Some((queue, item)) = selected else {
        return crate::fabric::progress::WakeOutcome::Idle;
    };
    let mut factory = Factory {
        project_attribution: crate::usage::attribution_enabled(edge.roots),
        context: match Context::open(edge.roots.clone(), edge.home, edge.cwd) {
            Ok(context) => context,
            Err(_) => {
                return crate::fabric::progress::WakeOutcome::Skipped {
                    code: "WAKE_PR_CONTEXT_UNAVAILABLE".into(),
                };
            }
        },
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: match SystemExecutor::new(1) {
            Ok(executor) => executor,
            Err(_) => {
                return crate::fabric::progress::WakeOutcome::Skipped {
                    code: "WAKE_PR_EXECUTOR_UNAVAILABLE".into(),
                };
            }
        },
        home: edge.home.to_path_buf(),
        cwd: edge.cwd.to_path_buf(),
        assets: edge.assets.to_path_buf(),
        host_launch: None,
        workers,
    };
    wake_in_host(
        &mut factory,
        budget,
        cancel,
        crate::fabric::progress::WakeSubjectRef::QueueItem {
            project,
            queue,
            item,
        },
        crate::fabric::progress::WakeReason::PullRequest,
    )
}

/// Resolve the reader's progress policy and bound a wake by its configured cap.
fn reader_wake_budget(
    roots: &provenant_core::Roots,
    clock: &SystemClock,
    budget: WaitBudget,
) -> Option<WaitBudget> {
    let config = match provenant_config::read_knobs(roots) {
        Ok(config) => config,
        Err(error) => {
            super::super::interface::warn_operator(format!(
                "wake settings unavailable: {error}; using defaults"
            ));
            let now = clock.mono_now();
            return Some(WaitBudget::up_to(
                now,
                budget
                    .remaining(now)
                    .min(provenant_core::DurationMicros::from_micros(2_000_000)),
            ));
        }
    };
    if config
        .knobs
        .get("domains.wait.advance")
        .and_then(|knob| knob.value.as_str())
        == Some("off")
    {
        return None;
    }
    let millis = config
        .knobs
        .get("domains.wait.advance_budget_ms")
        .and_then(|knob| knob.value.as_u64())
        .unwrap_or(2_000);
    let now = clock.mono_now();
    Some(WaitBudget::up_to(
        now,
        budget
            .remaining(now)
            .min(provenant_core::DurationMicros::from_micros(
                millis.saturating_mul(1_000),
            )),
    ))
}

/// Wake using the checkpoint's sealed execution directory and the reader's cap.
pub(in crate::fabric) fn wake_graph_for_reader(
    factory: &Factory,
    graph_id: &str,
    reason: crate::fabric::progress::WakeReason,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> crate::fabric::progress::WakeOutcome {
    use crate::fabric::progress::{WakeOutcome, WakeSubjectRef};
    let Some(budget) = reader_wake_budget(&factory.context.roots, &factory.clock, budget) else {
        return WakeOutcome::Idle;
    };
    let project = factory.context.identity.project;
    let execution =
        match FileJournal::pending_execution(&factory.context.roots.state, project, graph_id) {
            Ok(Some(execution)) => execution,
            Ok(None) => return WakeOutcome::Idle,
            Err(_) => {
                return WakeOutcome::Skipped {
                    code: "WAKE_EXECUTION_UNAVAILABLE".into(),
                };
            }
        };
    let context = match Context::open(
        factory.context.roots.clone(),
        &factory.home,
        &execution.directory,
    ) {
        Ok(context) if context.identity.project == project && !context.in_lane => context,
        _ => {
            return WakeOutcome::Skipped {
                code: "WAKE_CONTEXT_UNAVAILABLE".into(),
            };
        }
    };
    let executor = match SystemExecutor::new(1) {
        Ok(executor) => executor,
        Err(_) => {
            return WakeOutcome::Skipped {
                code: "WAKE_EXECUTOR_UNAVAILABLE".into(),
            };
        }
    };
    let mut owner = Factory {
        project_attribution: factory.project_attribution,
        context,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor,
        home: factory.home.clone(),
        cwd: execution.directory,
        assets: factory.assets.clone(),
        host_launch: factory.host_launch.clone(),
        workers: factory.workers,
    };
    wake_in_host(
        &mut owner,
        budget,
        cancel,
        WakeSubjectRef::Graph {
            project,
            id: graph_id.to_owned(),
        },
        reason,
    )
}

/// Host-side wake. The reader owns the subject's host context, so it drives
/// the shared coordinator with a subject-scoped advance that reuses the
/// graph's existing recovery path. The frame grants no mutation authority.
pub(in crate::fabric) fn wake_in_host(
    factory: &mut Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
    subject: crate::fabric::progress::WakeSubjectRef,
    reason: crate::fabric::progress::WakeReason,
) -> crate::fabric::progress::WakeOutcome {
    use crate::fabric::progress::{WakeClaim, WakeOutcome, WakeReason, WakeSource, wake_subject};
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, advance_with_budget, patch};

    let project = factory.context.identity.project;
    if budget.remaining(factory.clock.mono_now()).as_micros() == 0 {
        let cursor = match &subject {
            crate::fabric::progress::WakeSubjectRef::Graph { id, .. } => id.clone(),
            crate::fabric::progress::WakeSubjectRef::QueueItem { queue, item, .. } => {
                format!("{queue}/{item}")
            }
        };
        return WakeOutcome::OverBudget { cursor };
    }
    let subject_project = match &subject {
        crate::fabric::progress::WakeSubjectRef::Graph { project, .. }
        | crate::fabric::progress::WakeSubjectRef::QueueItem { project, .. } => *project,
    };
    if subject_project != project {
        return WakeOutcome::Skipped {
            code: "WAKE_PROJECT_MISMATCH".into(),
        };
    }
    let graph_id = match &subject {
        crate::fabric::progress::WakeSubjectRef::Graph { id, .. } => id.clone(),
        crate::fabric::progress::WakeSubjectRef::QueueItem { .. } => String::new(),
    };

    struct HostWake<'a> {
        factory: &'a mut Factory,
        budget: WaitBudget,
        cancel: &'a CancelToken,
        graph_id: String,
        before: usize,
        journal: Option<FileJournal>,
    }
    impl WakeSource for HostWake<'_> {
        fn reconcile(
            &mut self,
            subject: &crate::fabric::progress::WakeSubjectRef,
            _reason: WakeReason,
        ) -> Result<(), WakeOutcome> {
            if let crate::fabric::progress::WakeSubjectRef::QueueItem { queue, item, .. } = subject
            {
                self.graph_id = match super::super::wake_queue::select(
                    self.factory,
                    queue,
                    item,
                    self.budget,
                    self.cancel,
                ) {
                    Ok(Some(graph)) => graph,
                    Ok(None) => return Err(WakeOutcome::Idle),
                    Err(error) => {
                        super::super::interface::warn_operator(format!(
                            "queue wake selection failed: {error}"
                        ));
                        return Err(WakeOutcome::Skipped {
                            code: "WAKE_QUEUE_SELECTION_FAILED".into(),
                        });
                    }
                };
            }
            match FileJournal::read_checkpoint(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &self.graph_id,
            ) {
                Ok(Some(_)) => {}
                Ok(None) => return Err(WakeOutcome::Idle),
                Err(_) => {
                    return Err(WakeOutcome::Skipped {
                        code: "WAKE_CHECKPOINT_UNREADABLE".into(),
                    });
                }
            }
            let execution = match FileJournal::pending_execution(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &self.graph_id,
            ) {
                Ok(Some(execution)) => execution,
                _ => {
                    return Err(WakeOutcome::Skipped {
                        code: "WAKE_EXECUTION_UNAVAILABLE".into(),
                    });
                }
            };
            let context = match Context::open(
                self.factory.context.roots.clone(),
                &self.factory.home,
                &execution.directory,
            ) {
                Ok(context)
                    if context.identity.project == self.factory.context.identity.project
                        && !context.in_lane =>
                {
                    context
                }
                _ => {
                    return Err(WakeOutcome::Skipped {
                        code: "WAKE_CONTEXT_UNAVAILABLE".into(),
                    });
                }
            };
            self.factory.context = context;
            self.factory.cwd = execution.directory;
            match FileJournal::try_open(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &self.graph_id,
            ) {
                Ok(guard) => drop(guard),
                Err(error) if error.is_lock_timeout() => {
                    return Err(WakeOutcome::Busy {
                        holder: self.graph_id.clone(),
                    });
                }
                Err(_) => {
                    return Err(WakeOutcome::Skipped {
                        code: "WAKE_JOURNAL_UNAVAILABLE".into(),
                    });
                }
            }
            super::super::queue_controls::reconcile_pending(
                self.factory,
                &self.graph_id,
                self.budget,
                self.cancel,
            )
            .map_err(|error| WakeOutcome::Skipped {
                code: error.code().to_owned(),
            })
        }
        fn claim(
            &mut self,
            _subject: &crate::fabric::progress::WakeSubjectRef,
            deadline: provenant_core::Deadline,
        ) -> WakeClaim {
            if deadline
                .remaining(self.factory.clock.mono_now())
                .as_micros()
                == 0
            {
                return WakeClaim::Skipped {
                    code: "WAKE_OVER_BUDGET".into(),
                };
            }
            let root = &self.factory.context.roots.state;
            let project = self.factory.context.identity.project;
            let checkpoint = match FileJournal::read_checkpoint(root, project, &self.graph_id) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => return WakeClaim::Idle,
                Err(_) => {
                    return WakeClaim::Skipped {
                        code: "WAKE_CHECKPOINT_UNREADABLE".into(),
                    };
                }
            };
            if checkpoint.nodes.iter().all(|node| node.state.settled())
                || (checkpoint.pause_cause().is_some() && !checkpoint.cancelled)
            {
                return WakeClaim::Idle;
            }
            let execution = match FileJournal::pending_execution(root, project, &self.graph_id) {
                Ok(Some(execution)) => execution,
                _ => {
                    return WakeClaim::Skipped {
                        code: "WAKE_EXECUTION_UNAVAILABLE".into(),
                    };
                }
            };
            match super::super::queue_scheduler::admit_graph_tick(
                self.factory,
                &self.graph_id,
                &execution,
                &checkpoint,
            ) {
                Ok((true, warnings, waiting)) => {
                    for warning in warnings.into_iter().chain(waiting) {
                        super::super::interface::warn_operator(warning);
                    }
                }
                Ok((false, _, _)) => return WakeClaim::Idle,
                Err(_) => {
                    return WakeClaim::Skipped {
                        code: "WAKE_ADMISSION_UNAVAILABLE".into(),
                    };
                }
            }
            let journal = match FileJournal::try_open(root, project, &self.graph_id) {
                Ok(journal) => journal,
                Err(error) if error.is_lock_timeout() => {
                    return WakeClaim::Busy {
                        holder: self.graph_id.clone(),
                    };
                }
                Err(_) => {
                    return WakeClaim::Skipped {
                        code: "WAKE_JOURNAL_UNAVAILABLE".into(),
                    };
                }
            };
            self.before = checkpoint
                .nodes
                .iter()
                .filter(|node| node.state.settled())
                .count();
            self.journal = Some(journal);
            WakeClaim::Ready {
                revision: patch::revision(&checkpoint),
            }
        }
        fn live_revision(&self, _subject: &crate::fabric::progress::WakeSubjectRef) -> u64 {
            match FileJournal::read_checkpoint(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &self.graph_id,
            ) {
                Ok(Some(checkpoint)) => patch::revision(&checkpoint),
                _ => u64::MAX,
            }
        }
        fn advance(
            &mut self,
            _subject: &crate::fabric::progress::WakeSubjectRef,
            _reason: WakeReason,
            deadline: provenant_core::Deadline,
        ) -> WakeOutcome {
            if deadline
                .remaining(self.factory.clock.mono_now())
                .as_micros()
                == 0
            {
                return WakeOutcome::OverBudget {
                    cursor: self.graph_id.clone(),
                };
            }
            let root = &self.factory.context.roots.state;
            let project = self.factory.context.identity.project;
            let Some(journal) = self.journal.as_mut() else {
                return WakeOutcome::Skipped {
                    code: "WAKE_JOURNAL_UNAVAILABLE".into(),
                };
            };
            let execution = match FileJournal::pending_execution(root, project, &self.graph_id) {
                Ok(Some(execution)) => execution,
                _ => {
                    return WakeOutcome::Skipped {
                        code: "WAKE_EXECUTION_UNAVAILABLE".into(),
                    };
                }
            };
            let mut checkpoint = match journal.load(&self.graph_id) {
                Ok(Some(checkpoint)) => checkpoint,
                _ => {
                    return WakeOutcome::Skipped {
                        code: "WAKE_CHECKPOINT_UNREADABLE".into(),
                    };
                }
            };
            match super::super::steer::dispatch_pending_steers(
                self.factory,
                journal,
                &mut checkpoint,
                self.budget,
                self.cancel,
            ) {
                Ok(warnings) => {
                    for warning in warnings {
                        super::super::interface::warn_operator(warning);
                    }
                }
                Err(_) => {
                    return WakeOutcome::Skipped {
                        code: "WAKE_STEER_FAILED".into(),
                    };
                }
            }
            let mut owner = super::super::BoundGraphOwner {
                writer_launch_budget: None,
                admission_timeout: None,
                launch_warnings: Vec::new(),
                repo_facts: Default::default(),
                factory: self.factory,
                integration_base: checkpoint.bases,
                integration_branch: execution.document.integration_branch.clone(),
                network: super::super::graph_network(&execution.document),
                capability_overrides: super::super::graph_capability_overrides(&execution.document),
                exclude: super::super::graph_exclude(&execution.document),
                budget: self.budget,
                cancel: self.cancel,
                prices: None,
                tick: true,
            };
            let config = &self.factory.context.config.graph;
            match advance_with_budget(
                &execution.document,
                &mut owner,
                journal,
                &config.limits,
                &config.budget,
            ) {
                Ok(next) => {
                    let after = next
                        .checkpoint
                        .nodes
                        .iter()
                        .filter(|node| node.state.settled())
                        .count();
                    WakeOutcome::Advanced {
                        transitions: after.saturating_sub(self.before),
                    }
                }
                Err(_) => WakeOutcome::Skipped {
                    code: "WAKE_ADVANCE_FAILED".into(),
                },
            }
        }
    }

    let outcome = wake_subject(
        subject,
        reason,
        budget.deadline(),
        &mut HostWake {
            factory,
            budget,
            cancel,
            graph_id,
            before: 0,
            journal: None,
        },
    );
    if let WakeOutcome::Skipped { code } = &outcome {
        super::super::interface::warn_operator(format!(
            "wake skipped: {code} (source: graph owner)"
        ));
    }
    outcome
}

// Stdin's bounded I/O API takes a real-time deadline at this transport edge.
#[allow(
    clippy::disallowed_methods,
    reason = "sample the real-time stdin transport deadline"
)]
/// The relay has already bound this input to the calling attempt's graph.
pub(crate) fn read_lane_graph(
    holder: &provenant_run::HolderContext,
    input: Value,
    cancel: &CancelToken,
    within: std::time::Duration,
    workers: &'static dyn WorkerObserver,
) -> Result<Value, ServiceError> {
    let read = || -> Result<Value, Box<dyn std::error::Error>> {
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled.into());
        }
        if within <= std::time::Duration::from_millis(100) {
            return Err(FacadeError::Timeout.into());
        }

        let clock = SystemClock::new();
        let budget = provenant_fabric_runtime::relay::host_budget(&clock, within)?;
        let launch = holder
            .launch
            .as_ref()
            .ok_or("host launch context missing")?;
        let roots = crate::roots::child(
            &holder.roots.home,
            &holder.roots.checkout,
            launch.configuration_root.clone(),
        )?;
        let graph_directory = if input["op"] == "prepare_child" {
            // The admitted parent launch owns this path. A lane cannot choose
            // another repository or worktree for host-side object preparation.
            launch.cwd.as_path()
        } else {
            launch.graph_directory.as_deref().unwrap_or(&launch.cwd)
        };
        let context = Context::open_inspection(roots, &holder.roots.home, graph_directory)?;
        if context.identity.project != holder.project_id {
            return Err("host graph project mismatch".into());
        }
        let mut factory = Factory {
            project_attribution: false,
            context,
            clock: SystemClock::new(),
            backoff: ThreadPause,
            executor: SystemExecutor::new(4)?,
            home: holder.roots.home.clone(),
            cwd: graph_directory.to_path_buf(),
            assets: holder.roots.checkout.clone(),
            host_launch: Some(launch.clone()),
            workers,
        };
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled.into());
        }
        if budget.remaining(factory.clock.mono_now()).as_micros() == 0 {
            return Err(FacadeError::Timeout.into());
        }
        // Wake is a read-class frame. The relay has already applied the
        // sibling-session authorisation; the host runs one custody-checked
        // advance and grants no resume, retry or landing authority.
        if input["op"] == "wake" {
            let subject: provenant_records::progress::WakeSubjectRef =
                serde_json::from_value(input["subject"].clone())?;
            let Some(budget) = reader_wake_budget(&factory.context.roots, &factory.clock, budget)
            else {
                return Ok(crate::fabric::progress::WakeOutcome::Idle.to_value());
            };
            return Ok(wake_in_host(
                &mut factory,
                budget,
                cancel,
                subject,
                crate::fabric::progress::WakeReason::Lane,
            )
            .to_value());
        }
        if input["op"] == "prepare_child" {
            return Ok(serde_json::to_value(
                factory.child_request(&input["input"])?,
            )?);
        }
        if input["op"] == "output"
            && let Some(selector) = input["id"].as_str()
            && !selector.is_empty()
            && let Ok(Some(target)) = factory.graph_target(selector)
        {
            let _ = wake_graph_for_reader(
                &factory,
                &target.graph,
                crate::fabric::progress::WakeReason::Output,
                budget,
                cancel,
            );
        }
        if input["op"] != "output" || input["trace"] == true {
            let mut result = factory.graph_document(input.clone(), budget, cancel)?;
            if input["op"] == "status" {
                result["wake_project"] = json!(factory.context.identity.project.to_string());
            }
            return Ok(result);
        }
        let part = match input["part"].as_str().unwrap_or("result") {
            "result" => provenant_cli::Part::Result,
            "prompt" => provenant_cli::Part::Prompt,
            "stderr" => provenant_cli::Part::Stderr,
            "events" => provenant_cli::Part::Events,
            "receipt" => provenant_cli::Part::Receipt,
            "transcript" => provenant_cli::Part::Transcript,
            _ => return Err(FacadeError::invalid(InvalidInputDetail::Field("part".into())).into()),
        };
        // A malformed target is the caller's input, not a host I/O failure.
        let target = input["id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field("id".into())))?;
        let command = provenant_cli::Output {
            common: Default::default(),
            target,
            part,
            to: None,
            final_result: false,
            path: false,
            offset: input["offset"].as_u64().unwrap_or(0),
            tail: input["tail"].as_u64(),
            tail_lines: input["tail_lines"].as_u64(),
            max_bytes: input["max_bytes"].as_u64().unwrap_or(65536).clamp(1, 65536),
        };
        let edge = provenant_cli::run::Edge {
            roots: &factory.context.roots,
            home: &factory.home,
            assets: &factory.assets,
            cwd: &factory.cwd,
        };
        let reply = host_graph_output(
            &command,
            &edge,
            &factory,
            budget,
            cancel,
            input["full"].as_bool() == Some(true),
        )?;
        if input["render"] == true {
            Ok(json!({"result":reply.result,"human":reply.human}))
        } else {
            Ok(reply.result.data)
        }
    };
    read().map_err(|error| match error.downcast::<FacadeError>() {
        Ok(error) => ServiceError::Facade(*error),
        Err(error) => match error.downcast::<TargetFault>() {
            Ok(fault) if fault.code == "NOT_FOUND" => ServiceError::Facade(FacadeError::NotFound),
            Ok(_) | Err(_) => ServiceError::Facade(FacadeError::io("read calling lane graph")),
        },
    })
}
