//! Restart-safe advancement for persisted delivery queues.
use super::{
    Context, Factory,
    queue_registry::{QueueItem, QueueItemState, QueueRecord, QueueRegistry},
};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, WaitBudget, WallTime};
use provenant_fabric_runtime::graph::{
    CiState, CiTarget, FileJournal, GateCondition, GraphJournal, GraphStatus,
};
use provenant_os::{clock::SystemClock, exec::SystemExecutor};
use std::{collections::BTreeSet, path::Path};

pub(super) fn reconcile_controls_before_tick(
    resolved: &crate::roots::Resolved,
    workers: &'static dyn WorkerObserver,
) -> crate::tick::Graphs {
    let mut report = crate::tick::Graphs::default();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let lock_budget = WaitBudget::nonblocking(clock.mono_now());
    let registry = match QueueRegistry::open(resolved.roots.state.as_path()) {
        Ok(registry) => registry,
        Err(_) => return report,
    };
    let queues = match registry.queues(lock_budget, &cancel, &clock) {
        Ok(queues) => queues,
        Err(error) => {
            report.warnings.push(format!(
                "delivery queue controls were not reconciled: {error}"
            ));
            return report;
        }
    };
    for queue in queues {
        for item in queue.items.iter().filter(|item| {
            matches!(
                item.state,
                QueueItemState::Launching
                    | QueueItemState::Active
                    | QueueItemState::Parked
                    | QueueItemState::Resuming
            )
        }) {
            let project = match registered_project(resolved, &queue.project) {
                Some(project) => project,
                None => continue,
            };
            let (factory, budget) =
                match factory_for(resolved, workers, &project, &item.execution_directory) {
                    Ok(result) => result,
                    Err(_) => continue,
                };
            let snapshot = FileJournal::read_status_snapshot(
                &factory.context.roots.state,
                factory.context.identity.project,
                &item.graph_id,
            );
            let Ok(Some((execution, _checkpoint))) = snapshot else {
                continue;
            };
            if !matches_item_execution(&queue.queue_id, item, &execution).unwrap_or(false) {
                continue;
            }
            if let Err(error) =
                super::queue_controls::reconcile_pending(&factory, &item.graph_id, budget, &cancel)
            {
                report.warnings.push(format!(
                    "queue {} item {} control reconciliation was inconclusive: {error}",
                    queue.queue_id, item.identity.item
                ));
            }
        }
    }
    report
}

pub(super) fn tick_queues(
    resolved: &crate::roots::Resolved,
    now: WallTime,
    workers: &'static dyn WorkerObserver,
) -> crate::tick::Graphs {
    let mut report = crate::tick::Graphs::default();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let lock_budget = WaitBudget::nonblocking(clock.mono_now());
    let registry = match QueueRegistry::open(resolved.roots.state.as_path()) {
        Ok(registry) => registry,
        Err(error) => {
            report
                .errors
                .push(format!("delivery queue registry: {error}"));
            return report;
        }
    };
    let projects = match provenant_config::registered_projects(&resolved.roots) {
        Ok(projects) => projects,
        Err(error) => {
            report
                .errors
                .push(format!("delivery queue project registry: {error}"));
            return report;
        }
    };
    let queues = match registry.queues(lock_budget, &cancel, &clock) {
        Ok(queues) => queues,
        Err(error) => {
            report
                .errors
                .push(format!("delivery queue registry: {error}"));
            return report;
        }
    };
    for queue in &queues {
        let Some(project) = projects
            .iter()
            .find(|binding| binding.project_id().to_string() == queue.project)
            .map(|binding| binding.project_id())
        else {
            report.warnings.push(format!(
                "queue {} remains durable because project {} is no longer registered",
                queue.queue_id, queue.project
            ));
            continue;
        };
        let mut runnable_parked = BTreeSet::new();
        for item in queue.items.iter().filter(|item| {
            matches!(
                item.state,
                QueueItemState::Launching
                    | QueueItemState::Active
                    | QueueItemState::Parked
                    | QueueItemState::Resuming
            )
        }) {
            let (factory, budget) =
                match factory_for(resolved, workers, &project, &item.execution_directory) {
                    Ok(result) => result,
                    Err(error) => {
                        report.warnings.push(format!(
                            "queue {} item {} remains reserved: {error}",
                            queue.queue_id, item.identity.item
                        ));
                        continue;
                    }
                };
            match observe_item(&factory, &registry, queue, item, now, budget, &cancel) {
                Ok(true) if item.state == QueueItemState::Parked => {
                    runnable_parked.insert(item.identity.clone());
                }
                Ok(_) => {}
                Err(error) => report.warnings.push(format!(
                    "queue {} item {} remains reserved after an inconclusive graph observation: {error}",
                    queue.queue_id, item.identity.item
                )),
            }
        }
        let schedule = match registry.claim_queue_with_parked(
            &queue.queue_id,
            &runnable_parked,
            now,
            lock_budget,
            &cancel,
            &clock,
        ) {
            Ok(schedule) => schedule,
            Err(error) => {
                report.warnings.push(format!(
                    "queue {} could not claim pending items: {error}",
                    queue.queue_id
                ));
                continue;
            }
        };
        for warning in &schedule.warnings {
            super::interface::warn_operator(warning);
        }
        if !schedule.warnings.is_empty()
            && let Err(error) =
                registry.acknowledge_wait_warning(&queue.queue_id, lock_budget, &cancel, &clock)
        {
            report.warnings.push(format!(
                "queue {} cap warning acknowledgement failed; overflow remains gated: {error}",
                queue.queue_id
            ));
            continue;
        }
        report.warnings.extend(schedule.warnings);
        report.warnings.extend(schedule.waiting);
        for (queue_id, item) in schedule.launch {
            if !matches!(
                item.state,
                QueueItemState::Launching | QueueItemState::Resuming
            ) {
                continue;
            }
            let (item_factory, item_budget) =
                match factory_for(resolved, workers, &project, &item.execution_directory) {
                    Ok(result) => result,
                    Err(error) => {
                        report.warnings.push(format!(
                            "queue {} item {} remains reserved: {error}",
                            queue_id, item.identity.item
                        ));
                        continue;
                    }
                };
            match launch_item(
                &item_factory,
                &registry,
                &queue_id,
                &item,
                item_budget,
                &cancel,
            ) {
                Ok(QueueLaunchDisposition::Launched) => report.advanced += 1,
                Ok(QueueLaunchDisposition::Observed) => {}
                Ok(QueueLaunchDisposition::Unavailable(reason)) => report.warnings.push(format!(
                    "queue {} item {} is unavailable: {reason}",
                    queue_id, item.identity.item
                )),
                Err(error) => report.warnings.push(format!(
                    "queue {} item {} remains reserved for graph {}: {error}",
                    queue_id, item.identity.item, item.graph_id
                )),
            }
        }
    }
    reconcile_ci_class(resolved, workers, &projects, &mut report);
    report
}

fn registered_project(
    resolved: &crate::roots::Resolved,
    project: &str,
) -> Option<provenant_core::ProjectId> {
    provenant_config::registered_projects(&resolved.roots)
        .ok()?
        .into_iter()
        .find(|binding| binding.project_id().to_string() == project)
        .map(|binding| binding.project_id())
}

fn factory_for(
    resolved: &crate::roots::Resolved,
    workers: &'static dyn WorkerObserver,
    project: &provenant_core::ProjectId,
    directory: &str,
) -> Result<(Factory, WaitBudget), String> {
    let cwd = Path::new(directory);
    if !cwd.is_absolute() {
        return Err("saved execution directory is not absolute".into());
    }
    let context = Context::open(resolved.roots.clone(), &resolved.home, cwd)
        .map_err(|error| format!("cannot reopen registered queue project: {error}"))?;
    if context.identity.project != *project || context.in_lane || context.inspection_only {
        return Err(
            "saved execution directory no longer belongs to its registered host project".into(),
        );
    }
    let clock = SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), context.config.graph.launch_wait_seconds)
        .map_err(|error| format!("invalid configured graph launch wait: {error}"))?;
    let executor = SystemExecutor::new(4)
        .map_err(|error| format!("cannot create queue execution helper: {error}"))?;
    let factory = Factory {
        project_attribution: crate::usage::attribution_enabled(&context.roots),
        context,
        clock,
        backoff: provenant_core::ThreadPause,
        executor,
        home: resolved.home.clone(),
        cwd: cwd.to_path_buf(),
        assets: resolved.assets.clone(),
        host_launch: None,
        workers,
    };
    Ok((factory, budget))
}

fn item_request(queue_id: &str, item: &QueueItem) -> Result<serde_json::Value, String> {
    let marker = super::queue_registry::QueueItemMarker {
        queue_id: queue_id.to_owned(),
        identity: item.identity.clone(),
        graph_id: item.graph_id.clone(),
        launch_digest: item.launch_digest.clone(),
    };
    let mut request = item.launch_request.clone();
    request["queue_item"] = serde_json::to_value(marker).map_err(|error| error.to_string())?;
    Ok(request)
}

pub(super) fn matches_item_execution(
    queue_id: &str,
    item: &QueueItem,
    execution: &provenant_fabric_runtime::graph::GraphExecution,
) -> Result<bool, String> {
    let request = item_request(queue_id, item)?;
    let prepared = super::interface::launch_input(&request).map_err(|error| error.to_string())?;
    let digest = super::interface::submission_digest(&prepared, None, None)
        .map_err(|error| error.to_string())?;
    Ok(execution.document.graph_id == item.graph_id
        && execution.document.queue.is_some()
        && execution.document.replay_digest() == Some(digest.as_str())
        && execution
            .document
            .context
            .as_ref()
            .and_then(|context| context.work_item.as_deref())
            == Some(item.identity.item.as_str())
        && execution.directory == Path::new(item.execution_directory.as_str()))
}

/// Gate the generic graph tick before it dispatches pending steers or advances
/// a resumed queue graph. Returns the durable queue wait details for the tick
/// receipt; non-queue graphs pass through unchanged.
pub(super) fn admit_graph_tick(
    factory: &Factory,
    graph_id: &str,
    execution: &provenant_fabric_runtime::graph::GraphExecution,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Result<(bool, Vec<String>, Vec<String>), String> {
    let clock = &factory.clock;
    let cancel = CancelToken::new();
    let budget = WaitBudget::nonblocking(clock.mono_now());
    let registry = QueueRegistry::open(factory.context.roots.state.as_path())?;
    let queues = registry.queues(budget, &cancel, clock)?;
    let project = factory.context.identity.project.to_string();
    let owners: Vec<_> = queues
        .into_iter()
        .filter(|queue| queue.project == project)
        .flat_map(|queue| {
            let queue_id = queue.queue_id;
            queue
                .items
                .into_iter()
                .filter(|item| item.graph_id == graph_id)
                .map(move |item| (queue_id.clone(), item))
        })
        .collect();
    if owners.is_empty() {
        if execution.document.queue.is_some() {
            return Err("queue graph has no durable registry owner".into());
        }
        return Ok((true, Vec::new(), Vec::new()));
    }
    let [(queue_id, item)] = owners.as_slice() else {
        return Err("graph has multiple durable queue owners".into());
    };
    if !matches_item_execution(queue_id, item, execution)? {
        return Err("persisted graph does not match its durable queue identity".into());
    }
    if super::queue_controls::checkpoint_has_pending_control(checkpoint)
        .map_err(|error| error.to_string())?
    {
        return Ok((false, Vec::new(), Vec::new()));
    }
    match item.state {
        QueueItemState::Launching | QueueItemState::Active | QueueItemState::Resuming => {
            Ok((true, Vec::new(), Vec::new()))
        }
        // Parked items are claimed only by tick_queues after it has observed
        // every runnable row and can preserve queue order under the cap.
        QueueItemState::Parked => Ok((false, Vec::new(), Vec::new())),
        QueueItemState::Pending | QueueItemState::Terminal => Ok((false, Vec::new(), Vec::new())),
    }
}

pub(super) fn observe_item(
    factory: &Factory,
    registry: &QueueRegistry,
    queue: &QueueRecord,
    item: &QueueItem,
    now: WallTime,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<bool, String> {
    let project = factory.context.identity.project;
    {
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            project,
            &item.graph_id,
            budget,
            cancel,
            &factory.clock,
        )
        .map_err(|error| error.to_string())?;
        let checkpoint = journal
            .load(&item.graph_id)
            .map_err(|error| error.to_string())?;
        let Some(checkpoint) = checkpoint else {
            if matches!(
                item.state,
                QueueItemState::Active | QueueItemState::Parked | QueueItemState::Resuming
            ) {
                return Err(
                    "owned queue graph checkpoint is absent; ownership remains uncertain".into(),
                );
            }
            return Ok(false);
        };
        let execution =
            FileJournal::execution(&factory.context.roots.state, project, &item.graph_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "owned queue graph execution is absent".to_owned())?;
        if !matches_item_execution(&queue.queue_id, item, &execution)? {
            return Err("persisted graph does not match its reserved queue identity".into());
        }
        if item.state == QueueItemState::Parked {
            let status = super::graph_status::graph_snapshot(checkpoint.clone()).status;
            let control_needs_slot =
                super::queue_controls::checkpoint_has_pending_parallel_control(&checkpoint)
                    .map_err(|error| error.to_string())?;
            if control_needs_slot {
                return Ok(true);
            }
            match status {
                GraphStatus::Cancelled | GraphStatus::Complete => {
                    mark_observed_terminal(
                        registry,
                        &queue.queue_id,
                        item,
                        status,
                        budget,
                        cancel,
                        &factory.clock,
                    )?;
                    return Ok(false);
                }
                GraphStatus::Pending => {
                    return Ok(super::graph_execution::graph_wake_due(&checkpoint, now));
                }
                GraphStatus::Paused => {
                    return queue_paused_wake_due(factory, &checkpoint, now, cancel);
                }
            }
        }
    }
    super::queue_controls::reconcile_pending(factory, &item.graph_id, budget, cancel)
        .map_err(|error| error.to_string())?;
    // Re-read and hold graph custody through the queue state transition. A
    // resumed graph cannot be overwritten by a stale Paused observation.
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        &item.graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(|error| error.to_string())?;
    let checkpoint = journal
        .load(&item.graph_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "queue graph checkpoint disappeared during reconciliation".to_owned())?;
    let execution = FileJournal::execution(&factory.context.roots.state, project, &item.graph_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "queue graph execution disappeared during reconciliation".to_owned())?;
    if !matches_item_execution(&queue.queue_id, item, &execution)? {
        return Err("persisted graph changed its reserved queue identity".into());
    }
    let status = super::graph_status::graph_snapshot(checkpoint.clone()).status;
    let control_needs_slot =
        super::queue_controls::checkpoint_has_pending_parallel_control(&checkpoint)
            .map_err(|error| error.to_string())?;
    if control_needs_slot {
        return Ok(item.state == QueueItemState::Parked);
    }
    match status {
        GraphStatus::Cancelled | GraphStatus::Complete => {
            mark_observed_terminal(
                registry,
                &queue.queue_id,
                item,
                status,
                budget,
                cancel,
                &factory.clock,
            )?;
            Ok(false)
        }
        GraphStatus::Paused => {
            if item.state == QueueItemState::Resuming {
                if queue_paused_wake_due(factory, &checkpoint, now, cancel)? {
                    // Keep the claim only while another graph advance can act.
                    return Ok(true);
                }
                registry.mark_parked(
                    &queue.queue_id,
                    &item.identity,
                    &item.graph_id,
                    budget,
                    cancel,
                    &factory.clock,
                )?;
                return Ok(false);
            }
            registry.mark_parked(
                &queue.queue_id,
                &item.identity,
                &item.graph_id,
                budget,
                cancel,
                &factory.clock,
            )?;
            if item.state == QueueItemState::Parked {
                queue_paused_wake_due(factory, &checkpoint, now, cancel)
            } else {
                Ok(false)
            }
        }
        GraphStatus::Pending => {
            if matches!(
                item.state,
                QueueItemState::Launching | QueueItemState::Resuming
            ) {
                registry.mark_active(
                    &queue.queue_id,
                    &item.identity,
                    &item.graph_id,
                    budget,
                    cancel,
                    &factory.clock,
                )?;
            }
            Ok(item.state == QueueItemState::Parked
                && super::graph_execution::graph_wake_due(&checkpoint, now))
        }
    }
}

fn queue_paused_wake_due(
    factory: &Factory,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    now: WallTime,
    cancel: &CancelToken,
) -> Result<bool, String> {
    if queue_paused_scheduled_wake_due(checkpoint, now) {
        return Ok(true);
    }
    if checkpoint.asks.is_empty() {
        return Ok(false);
    }
    let actor = checkpoint
        .ask_actor
        .as_deref()
        .ok_or_else(|| "graph has asks but no saved ask identity".to_owned())?;
    super::owner::graph_asks_have_answer(factory, actor, &checkpoint.asks, cancel)
        .map_err(|error| error.to_string())
}

fn queue_paused_scheduled_wake_due(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    now: WallTime,
) -> bool {
    checkpoint.pending_steers.has_pending()
        || (checkpoint.cancelled
            && checkpoint.nodes.iter().any(|node| {
                matches!(
                    node.state,
                    provenant_fabric_runtime::graph::NodeState::Pending
                        | provenant_fabric_runtime::graph::NodeState::Running
                )
            }))
        || checkpoint.wake_at.is_some_and(|deadline| now >= deadline)
        || checkpoint
            .asks
            .iter()
            .any(|ask| ask.wake_at.is_some_and(|deadline| now >= deadline))
        || checkpoint.nodes.iter().any(|node| {
            node.state == provenant_fabric_runtime::graph::NodeState::Running
                && !node.attempts.is_empty()
                && node
                    .launch_retry
                    .as_ref()
                    .is_some_and(|retry| retry.wake_at.is_none())
        })
}

pub(super) fn mark_launch_unavailable_if_absent(
    factory: &Factory,
    registry: &QueueRegistry,
    queue_id: &str,
    item: &QueueItem,
    reason: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<bool, String> {
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        &item.graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(|error| error.to_string())?;
    if journal
        .load(&item.graph_id)
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Ok(false);
    }
    registry.mark_launch_unavailable(
        (queue_id, &item.identity),
        &item.graph_id,
        reason,
        budget,
        cancel,
        &factory.clock,
    )?;
    Ok(true)
}

fn mark_observed_terminal(
    registry: &QueueRegistry,
    queue_id: &str,
    item: &QueueItem,
    status: GraphStatus,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<(), String> {
    let outcome = match status {
        GraphStatus::Cancelled => "cancelled",
        GraphStatus::Complete => "complete",
        GraphStatus::Paused | GraphStatus::Pending => {
            return Err("non-terminal graph status cannot settle a queue item".into());
        }
    };
    registry.mark_terminal(
        (queue_id, &item.identity),
        &item.graph_id,
        outcome,
        budget,
        cancel,
        clock,
    )
}

enum QueueLaunchDisposition {
    Observed,
    Launched,
    Unavailable(String),
}

fn launch_item(
    factory: &Factory,
    registry: &QueueRegistry,
    queue_id: &str,
    item: &QueueItem,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<QueueLaunchDisposition, String> {
    let project = factory.context.identity.project;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        &item.graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(|error| error.to_string())?;
    if journal
        .load(&item.graph_id)
        .map_err(|error| error.to_string())?
        .is_some()
    {
        let execution =
            FileJournal::execution(&factory.context.roots.state, project, &item.graph_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "persisted queue checkpoint has no execution".to_owned())?;
        if !matches_item_execution(queue_id, item, &execution)? {
            return Err("persisted graph does not match its reserved queue identity".into());
        }
        drop(journal);
        super::queue_controls::reconcile_pending(factory, &item.graph_id, budget, cancel)
            .map_err(|error| error.to_string())?;
        journal = FileJournal::open(
            &factory.context.roots.state,
            project,
            &item.graph_id,
            budget,
            cancel,
            &factory.clock,
        )
        .map_err(|error| error.to_string())?;
        let checkpoint = journal
            .load(&item.graph_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "resuming queue graph checkpoint disappeared".to_owned())?;
        let execution =
            FileJournal::execution(&factory.context.roots.state, project, &item.graph_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "resuming queue graph execution disappeared".to_owned())?;
        if !matches_item_execution(queue_id, item, &execution)? {
            return Err("resumed graph changed its reserved queue identity".into());
        }
        let control_needs_slot =
            super::queue_controls::checkpoint_has_pending_parallel_control(&checkpoint)
                .map_err(|error| error.to_string())?;
        let status = super::graph_status::graph_snapshot(checkpoint.clone()).status;
        if control_needs_slot {
            return Ok(QueueLaunchDisposition::Observed);
        }
        match status {
            GraphStatus::Cancelled | GraphStatus::Complete => mark_observed_terminal(
                registry,
                queue_id,
                item,
                status,
                budget,
                cancel,
                &factory.clock,
            )?,
            GraphStatus::Paused if item.state == QueueItemState::Resuming => {
                let now = factory
                    .clock
                    .wall_now()
                    .map_err(|error| error.to_string())?;
                if !queue_paused_wake_due(factory, &checkpoint, now, cancel)? {
                    registry.mark_parked(
                        queue_id,
                        &item.identity,
                        &item.graph_id,
                        budget,
                        cancel,
                        &factory.clock,
                    )?;
                }
            }
            GraphStatus::Paused => registry.mark_parked(
                queue_id,
                &item.identity,
                &item.graph_id,
                budget,
                cancel,
                &factory.clock,
            )?,
            GraphStatus::Pending => registry.mark_active(
                queue_id,
                &item.identity,
                &item.graph_id,
                budget,
                cancel,
                &factory.clock,
            )?,
        }
        return Ok(QueueLaunchDisposition::Observed);
    }
    drop(journal);
    if item.state == QueueItemState::Resuming {
        return Err(
            "parked queue graph lost its checkpoint after slot reacquisition; refusing fresh launch"
                .into(),
        );
    }
    let request = item_request(queue_id, item)?;
    let receipt = match factory.graph_document_announcing(request, budget, cancel, None) {
        Ok(receipt) => receipt,
        Err(error) if super::queue_dispatch::deterministic_launch_refusal(&error).is_some() => {
            let reason = super::queue_dispatch::deterministic_launch_refusal(&error)
                .ok_or_else(|| "static refusal could not be classified".to_owned())?;
            match mark_launch_unavailable_if_absent(
                factory, registry, queue_id, item, reason, budget, cancel,
            ) {
                Ok(true) => {
                    return Ok(QueueLaunchDisposition::Unavailable(reason.to_owned()));
                }
                Ok(false) => {
                    return Err(format!("{reason}; checkpoint absence could not be proved"));
                }
                Err(error) => {
                    return Err(format!(
                        "{reason}; checkpoint absence could not be proved: {error}"
                    ));
                }
            }
        }
        Err(error) => return Err(error.to_string()),
    };
    if receipt["named"]["graph"].as_str() != Some(item.graph_id.as_str()) {
        return Err("launch response did not confirm the reserved graph ID".into());
    }
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        &item.graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(|error| error.to_string())?;
    let checkpoint = journal
        .load(&item.graph_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "launched queue graph checkpoint is absent".to_owned())?;
    let execution = FileJournal::execution(&factory.context.roots.state, project, &item.graph_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "launched queue graph execution is absent".to_owned())?;
    if !matches_item_execution(queue_id, item, &execution)? {
        return Err("launched graph does not match its reserved queue identity".into());
    }
    let control_needs_slot =
        super::queue_controls::checkpoint_has_pending_parallel_control(&checkpoint)
            .map_err(|error| error.to_string())?;
    if control_needs_slot {
        return Ok(QueueLaunchDisposition::Observed);
    }
    let status = super::graph_status::graph_snapshot(checkpoint.clone()).status;
    match status {
        GraphStatus::Cancelled | GraphStatus::Complete => mark_observed_terminal(
            registry,
            queue_id,
            item,
            status,
            budget,
            cancel,
            &factory.clock,
        )?,
        GraphStatus::Paused if item.state == QueueItemState::Resuming => {
            let now = factory
                .clock
                .wall_now()
                .map_err(|error| error.to_string())?;
            if !queue_paused_wake_due(factory, &checkpoint, now, cancel)? {
                registry.mark_parked(
                    queue_id,
                    &item.identity,
                    &item.graph_id,
                    budget,
                    cancel,
                    &factory.clock,
                )?;
            }
        }
        GraphStatus::Paused => registry.mark_parked(
            queue_id,
            &item.identity,
            &item.graph_id,
            budget,
            cancel,
            &factory.clock,
        )?,
        GraphStatus::Pending => registry.mark_active(
            queue_id,
            &item.identity,
            &item.graph_id,
            budget,
            cancel,
            &factory.clock,
        )?,
    }
    Ok(QueueLaunchDisposition::Launched)
}

fn reconcile_ci_class(
    resolved: &crate::roots::Resolved,
    workers: &'static dyn WorkerObserver,
    projects: &[provenant_config::ProjectBinding],
    report: &mut crate::tick::Graphs,
) {
    let cancel = CancelToken::new();
    let mut active = BTreeSet::new();
    let mut conclusive = BTreeSet::new();
    let mut wait_seconds = 0;
    for binding in projects {
        let project = binding.project_id();
        let Ok(ids) = FileJournal::graph_ids(&resolved.roots.state, project) else {
            continue;
        };
        for graph_id in ids {
            let Ok(Some(execution)) =
                FileJournal::execution(&resolved.roots.state, project, &graph_id)
            else {
                continue;
            };
            let Ok(Some(checkpoint)) =
                FileJournal::read_checkpoint(&resolved.roots.state, project, &graph_id)
            else {
                continue;
            };
            let directory = execution.directory.to_string_lossy().into_owned();
            let expanded = match factory_for(resolved, workers, &project, &directory) {
                Ok((factory, _)) => {
                    wait_seconds =
                        wait_seconds.max(factory.context.config.graph.launch_wait_seconds);
                    execution
                        .document
                        .expanded(&factory.context.config.graph.limits)
                        .ok()
                }
                Err(_) => None,
            };
            let graph_terminal = checkpoint.cancelled
                || super::graph_status::graph_snapshot(checkpoint.clone()).status
                    == GraphStatus::Complete;
            let mut superseded_owners = BTreeSet::new();
            for superseded in &checkpoint.delivery_ci_superseded {
                let owner = lease_owner(
                    &project.to_string(),
                    &graph_id,
                    &superseded.node_id,
                    &superseded.head_sha,
                );
                conclusive.insert(owner.clone());
                superseded_owners.insert(owner);
            }
            for progress in &checkpoint.nodes {
                if let Some(publication) = &progress.delivery_publication {
                    let owner = lease_owner(
                        &project.to_string(),
                        &graph_id,
                        &progress.id,
                        &publication.input_head,
                    );
                    let done = graph_terminal
                        || expanded.as_ref().is_some_and(|document| {
                            exact_ci_result(
                                document,
                                &checkpoint,
                                &progress.id,
                                &publication.input_head,
                            )
                        });
                    if done {
                        conclusive.insert(owner);
                    } else {
                        active.insert(owner);
                    }
                }
                if let Some(node) = expanded.as_ref().and_then(|document| {
                    document
                        .nodes
                        .iter()
                        .find(|node| node.id == progress.id && node.delivery_adoption.is_some())
                }) && let Some(intent) = &node.delivery_adoption
                    && let Some(adoption) = progress.delivery_adoption()
                    && adoption.receipt.is_some()
                {
                    let owner =
                        lease_owner(&project.to_string(), &graph_id, &progress.id, &intent.head);
                    let done = graph_terminal
                        || expanded.as_ref().is_some_and(|document| {
                            exact_ci_result(document, &checkpoint, &progress.id, &intent.head)
                        });
                    if done {
                        conclusive.insert(owner);
                    } else {
                        active.insert(owner);
                    }
                }
            }
            for owner in superseded_owners {
                mark_superseded_lease(&owner, &mut active, &mut conclusive);
            }
        }
    }
    if let Err(error) = provenant_run::build::queue::reconcile_ci_leases(
        resolved.roots.state.as_path(),
        &active,
        &conclusive,
        wait_seconds,
        &cancel,
    ) {
        report.warnings.push(format!(
            "delivery CI lease reconciliation was inconclusive after configured launch wait {wait_seconds} seconds: {error}"
        ));
    }
}

fn lease_owner(
    project: &str,
    graph_id: &str,
    node_id: &str,
    head_sha: &str,
) -> provenant_run::build::queue::CiLeaseOwner {
    provenant_run::build::queue::CiLeaseOwner {
        project: project.to_owned(),
        graph_id: graph_id.to_owned(),
        node_id: node_id.to_owned(),
        head_sha: head_sha.to_owned(),
    }
}

fn mark_superseded_lease(
    owner: &provenant_run::build::queue::CiLeaseOwner,
    active: &mut BTreeSet<provenant_run::build::queue::CiLeaseOwner>,
    conclusive: &mut BTreeSet<provenant_run::build::queue::CiLeaseOwner>,
) {
    active.remove(owner);
    conclusive.insert(owner.clone());
}

fn exact_ci_result(
    document: &provenant_fabric_runtime::graph::GraphDocument,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    publication_node: &str,
    head_sha: &str,
) -> bool {
    document.nodes.iter().any(|node| {
        let Some(GateCondition::Ci {
            target: CiTarget::Node { node: target, .. },
            ..
        }) = &node.condition
        else {
            return false;
        };
        if target != publication_node {
            return false;
        }
        checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == node.id)
            .is_some_and(|progress| {
                progress.ci.as_ref().is_some_and(|ci| {
                    progress.state.settled()
                        && progress
                            .gate
                            .as_ref()
                            .is_some_and(|gate| gate.passed == (ci.state == CiState::Passed))
                        && ci.head_sha.as_deref() == Some(head_sha)
                        && matches!(ci.state, CiState::Passed | CiState::Failed)
                })
            })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn unanswered_paused_queue_item_without_a_timer_is_not_wakeable() {
        use super::*;
        use serde_json::json;

        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": []
            }))
            .unwrap();
        let now = WallTime::from_micros(10).unwrap();

        assert!(!queue_paused_scheduled_wake_due(&checkpoint, now));
        checkpoint.wake_at = Some(WallTime::from_micros(9).unwrap());
        assert!(queue_paused_scheduled_wake_due(&checkpoint, now));
    }

    #[test]
    fn scheduler_requires_the_reserved_item_replay_identity() {
        use super::*;
        use serde_json::json;

        let mut item = QueueItem {
            identity: super::super::queue_registry::QueueIdentity::new(
                "project", "github", "issue-1",
            )
            .unwrap(),
            graph_id: "item-1".into(),
            snapshot_index: 0,
            launch_request: json!({"op":"run", "template":"deliver", "name":"item-1", "brief":"fix"}),
            execution_directory: "/repo".into(),
            adoption: None,
            launch_digest: format!("sha256:{}", "a".repeat(64)),
            state: QueueItemState::Launching,
            terminal_outcome: None,
            unavailable_reason: None,
            overflow_reason: None,
        };
        let request = item_request("queue-1", &item).unwrap();
        let prepared = super::super::interface::launch_input(&request).unwrap();
        let digest = super::super::interface::submission_digest(&prepared, None, None).unwrap();
        let mut document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(json!({
                "schema_version":2, "graph_id":"item-1", "nodes":[],
                "context":{"work_item":"issue-1"}
            }))
            .unwrap();
        document.queue = Some(provenant_fabric_runtime::graph::QueueKnobs::default());
        document.set_replay_digest(&digest);
        let mut execution = provenant_fabric_runtime::graph::GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: "/repo".into(),
            document,
        };
        assert!(matches_item_execution("queue-1", &item, &execution).unwrap());
        item.launch_digest = format!("sha256:{}", "b".repeat(64));
        assert!(!matches_item_execution("queue-1", &item, &execution).unwrap());
        item.launch_digest = format!("sha256:{}", "a".repeat(64));
        execution.document.context.as_mut().unwrap().work_item = Some("other".into());
        assert!(!matches_item_execution("queue-1", &item, &execution).unwrap());
    }

    #[test]
    fn exact_supersession_overrides_active_lease_classification() {
        use super::{BTreeSet, lease_owner, mark_superseded_lease};
        let owner = lease_owner("project", "graph", "publish", "0123456789abcdef");
        let mut active = BTreeSet::new();
        active.insert(owner.clone());
        let mut conclusive = BTreeSet::new();

        mark_superseded_lease(&owner, &mut active, &mut conclusive);

        assert!(!active.contains(&owner));
        assert!(conclusive.contains(&owner));
    }
}
