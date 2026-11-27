//! Queue-scoped selection for a reader wake, using the scheduler's claims.
use super::{
    Context, Factory,
    queue_registry::{QueueItemState, QueueRegistry},
    queue_scheduler,
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::{clock::SystemClock, exec::SystemExecutor};
use std::{collections::BTreeSet, path::Path};

fn item_factory(factory: &Factory, directory: &str) -> Result<Factory, String> {
    let path = Path::new(directory);
    let context = Context::open(factory.context.roots.clone(), &factory.home, path)
        .map_err(|error| error.to_string())?;
    if context.identity.project != factory.context.identity.project
        || context.in_lane
        || context.inspection_only
    {
        return Err("queue execution project is unavailable".into());
    }
    Ok(Factory {
        project_attribution: factory.project_attribution,
        context,
        clock: SystemClock::new(),
        backoff: provenant_core::ThreadPause,
        executor: SystemExecutor::new(1).map_err(|error| error.to_string())?,
        home: factory.home.clone(),
        cwd: path.to_path_buf(),
        assets: factory.assets.clone(),
        host_launch: factory.host_launch.clone(),
        workers: factory.workers,
    })
}

/// Reconcile the selected queue, run its normal capacity selection and launch
/// the selected item without claiming unrelated work.
pub(super) fn select(
    factory: &Factory,
    queue_id: &str,
    item_id: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Option<String>, String> {
    let registry = QueueRegistry::open(factory.context.roots.state.as_path())?;
    let clock = &factory.clock;
    let queues = registry.queues(budget, cancel, clock)?;
    let Some(queue) = queues.into_iter().find(|queue| {
        queue.queue_id == queue_id && queue.project == factory.context.identity.project.to_string()
    }) else {
        return Ok(None);
    };
    let Some(graph_id) = queue
        .items
        .iter()
        .find(|item| item.identity.item == item_id)
        .map(|item| item.graph_id.clone())
    else {
        return Ok(None);
    };
    let now = clock.wall_now().map_err(|error| error.to_string())?;
    let mut runnable = BTreeSet::new();
    for item in queue.items.iter().filter(|item| {
        matches!(
            item.state,
            QueueItemState::Launching
                | QueueItemState::Active
                | QueueItemState::Parked
                | QueueItemState::Resuming
        )
    }) {
        let owner = match item_factory(factory, &item.execution_directory) {
            Ok(owner) => owner,
            Err(error) => {
                super::interface::warn_operator(format!(
                    "queue {queue_id} item {} remains reserved: {error}",
                    item.identity.item
                ));
                continue;
            }
        };
        if super::queue_controls::reconcile_pending(&owner, &item.graph_id, budget, cancel).is_err()
        {
            super::interface::warn_operator(format!(
                "queue {} item {} control reconciliation was inconclusive",
                queue_id, item.identity.item
            ));
        }
        match queue_scheduler::observe_item(&owner, &registry, &queue, item, now, budget, cancel) {
            Ok(true) if item.state == QueueItemState::Parked => {
                runnable.insert(item.identity.clone());
            }
            Ok(_) => {}
            Err(error) => super::interface::warn_operator(format!(
                "queue {queue_id} item {} observation was inconclusive: {error}",
                item.identity.item
            )),
        }
    }
    let schedule = registry.claim_item_with_parked(
        (queue_id, item_id),
        &runnable,
        now,
        budget,
        cancel,
        clock,
    )?;
    for warning in &schedule.warnings {
        super::interface::warn_operator(warning);
    }
    if !schedule.warnings.is_empty() {
        registry.acknowledge_wait_warning(queue_id, budget, cancel, clock)?;
    }
    for waiting in &schedule.waiting {
        super::interface::warn_operator(waiting);
    }
    for (queue_id, item) in schedule.launch {
        let owner = match item_factory(factory, &item.execution_directory) {
            Ok(owner) => owner,
            Err(error) => {
                super::interface::warn_operator(format!(
                    "queue {queue_id} item {} remains reserved: {error}",
                    item.identity.item
                ));
                continue;
            }
        };
        match queue_scheduler::launch_item(&owner, &registry, &queue_id, &item, budget, cancel) {
            Ok(queue_scheduler::QueueLaunchDisposition::Unavailable(reason)) | Err(reason) => {
                super::interface::warn_operator(reason)
            }
            Ok(
                queue_scheduler::QueueLaunchDisposition::Launched
                | queue_scheduler::QueueLaunchDisposition::Observed,
            ) => {}
        }
    }
    Ok(Some(graph_id))
}
