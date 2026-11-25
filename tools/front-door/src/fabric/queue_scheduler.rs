//! Restart-safe advancement for persisted delivery queues.
use super::{
    Context, Factory,
    queue_registry::{QueueItem, QueueItemState, QueueRecord, QueueRegistry},
};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, WaitBudget, WallTime};
use provenant_fabric_runtime::graph::{CiState, CiTarget, FileJournal, GateCondition, GraphStatus};
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
    let registry = match QueueRegistry::open(&resolved.roots.state) {
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
                QueueItemState::Launching | QueueItemState::Active
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
            if execution.document.graph_id != item.graph_id
                || execution.document.queue.is_none()
                || execution.directory != Path::new(item.execution_directory.as_str())
            {
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
    let registry = match QueueRegistry::open(&resolved.roots.state) {
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
        for item in queue.items.iter().filter(|item| {
            matches!(
                item.state,
                QueueItemState::Launching | QueueItemState::Active
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
            match observe_item(&factory, &registry, queue, item, budget, &cancel) {
                Ok(()) => {}
                Err(error) => report.warnings.push(format!(
                    "queue {} item {} remains reserved after an inconclusive graph observation: {error}",
                    queue.queue_id, item.identity.item
                )),
            }
        }
        let schedule =
            match registry.claim_queue(&queue.queue_id, now, lock_budget, &cancel, &clock) {
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
        if !schedule.warnings.is_empty() {
            if let Err(error) =
                registry.acknowledge_wait_warning(&queue.queue_id, lock_budget, &cancel, &clock)
            {
                report.warnings.push(format!(
                    "queue {} cap warning acknowledgement failed; overflow remains gated: {error}",
                    queue.queue_id
                ));
                continue;
            }
        }
        report.warnings.extend(schedule.warnings);
        report.warnings.extend(schedule.waiting);
        for (queue_id, item) in schedule.launch {
            if item.state != QueueItemState::Launching {
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

fn observe_item(
    factory: &Factory,
    registry: &QueueRegistry,
    queue: &QueueRecord,
    item: &QueueItem,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), String> {
    let project = factory.context.identity.project;
    let snapshot =
        FileJournal::read_status_snapshot(&factory.context.roots.state, project, &item.graph_id)
            .map_err(|error| error.to_string())?;
    let Some((execution, _)) = snapshot else {
        if item.state == QueueItemState::Active {
            return Err(
                "active queue graph checkpoint is absent; ownership remains uncertain".into(),
            );
        }
        return Ok(());
    };
    if execution.document.graph_id != item.graph_id
        || execution.document.queue.is_none()
        || execution.directory != Path::new(item.execution_directory.as_str())
    {
        return Err("persisted graph does not match its reserved queue identity".into());
    }
    super::queue_controls::reconcile_pending(factory, &item.graph_id, budget, cancel)
        .map_err(|error| error.to_string())?;
    let snapshot =
        FileJournal::read_status_snapshot(&factory.context.roots.state, project, &item.graph_id)
            .map_err(|error| error.to_string())?;
    let Some((execution, checkpoint)) = snapshot else {
        return Err("queue graph checkpoint disappeared during reconciliation".into());
    };
    match super::graph_status::graph_snapshot(checkpoint).status {
        GraphStatus::Cancelled => registry.mark_terminal(
            &queue.queue_id,
            &item.identity,
            &item.graph_id,
            "cancelled",
            budget,
            cancel,
            &factory.clock,
        ),
        GraphStatus::Complete => registry.mark_terminal(
            &queue.queue_id,
            &item.identity,
            &item.graph_id,
            "complete",
            budget,
            cancel,
            &factory.clock,
        ),
        GraphStatus::Paused | GraphStatus::Pending => {
            if item.state == QueueItemState::Launching {
                registry.mark_active(
                    &queue.queue_id,
                    &item.identity,
                    &item.graph_id,
                    budget,
                    cancel,
                    &factory.clock,
                )?;
            }
            let _ = execution;
            Ok(())
        }
    }
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
    if let Some((execution, checkpoint)) =
        FileJournal::read_status_snapshot(&factory.context.roots.state, project, &item.graph_id)
            .map_err(|error| error.to_string())?
    {
        if execution.document.graph_id != item.graph_id
            || execution.document.queue.is_none()
            || execution.directory != Path::new(item.execution_directory.as_str())
        {
            return Err("persisted graph does not match its reserved queue identity".into());
        }
        let outcome = match super::graph_status::graph_snapshot(checkpoint).status {
            GraphStatus::Cancelled => Some("cancelled"),
            GraphStatus::Complete => Some("complete"),
            GraphStatus::Paused | GraphStatus::Pending => None,
        };
        if let Some(outcome) = outcome {
            registry.mark_terminal(
                queue_id,
                &item.identity,
                &item.graph_id,
                outcome,
                budget,
                cancel,
                &factory.clock,
            )?;
        } else {
            registry.mark_active(
                queue_id,
                &item.identity,
                &item.graph_id,
                budget,
                cancel,
                &factory.clock,
            )?;
        }
        return Ok(QueueLaunchDisposition::Observed);
    }
    let marker = super::queue_registry::QueueItemMarker {
        queue_id: queue_id.to_owned(),
        identity: item.identity.clone(),
        graph_id: item.graph_id.clone(),
        launch_digest: item.launch_digest.clone(),
    };
    let mut request = item.launch_request.clone();
    request["queue_item"] = serde_json::to_value(marker).map_err(|error| error.to_string())?;
    let receipt = match factory.graph_document_announcing(request, budget, cancel, None) {
        Ok(receipt) => receipt,
        Err(error) if super::queue_dispatch::is_binder_unavailable(&error) => {
            match FileJournal::read_status_snapshot(
                &factory.context.roots.state,
                project,
                &item.graph_id,
            ) {
                Ok(None) => {
                    let reason = super::queue_dispatch::TYPED_BINDER_UNAVAILABLE_REASON.to_owned();
                    registry.mark_launch_unavailable(
                        queue_id,
                        &item.identity,
                        &item.graph_id,
                        &reason,
                        budget,
                        cancel,
                        &factory.clock,
                    )?;
                    return Ok(QueueLaunchDisposition::Unavailable(reason));
                }
                _ => {
                    return Err(format!(
                        "{}; checkpoint absence could not be proved",
                        super::queue_dispatch::TYPED_BINDER_UNAVAILABLE_REASON
                    ));
                }
            }
        }
        Err(error) => return Err(error.to_string()),
    };
    if receipt["named"]["graph"].as_str() != Some(item.graph_id.as_str()) {
        return Err("launch response did not confirm the reserved graph ID".into());
    }
    registry.mark_active(
        queue_id,
        &item.identity,
        &item.graph_id,
        budget,
        cancel,
        &factory.clock,
    )?;
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
    use super::{BTreeSet, lease_owner, mark_superseded_lease};

    #[test]
    fn exact_supersession_overrides_active_lease_classification() {
        let owner = lease_owner("project", "graph", "publish", "0123456789abcdef");
        let mut active = BTreeSet::new();
        active.insert(owner.clone());
        let mut conclusive = BTreeSet::new();

        mark_superseded_lease(&owner, &mut active, &mut conclusive);

        assert!(!active.contains(&owner));
        assert!(conclusive.contains(&owner));
    }
}
