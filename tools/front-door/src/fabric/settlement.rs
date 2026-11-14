use super::*;

/// Advance graphs and deliver the reactions owed by this settlement.
pub(crate) fn advance_settled_graphs(
    holder: &provenant_cli::run::HostHolder<'_>,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(settled) = holder.settled()? else {
        return Ok(());
    };
    settlement_reactions(
        &crate::settlement::Subject {
            home: settled.roots.home.clone(),
            checkout: settled.roots.checkout.clone(),
            state_root: settled.roots.state_root.clone(),
            project: settled.project_id,
            actor: settled.actor_id,
            attempt: settled.attempt_id,
        },
        Some(settled),
        workers,
    )
}

/// Launch inputs for a graph advance when no live guardian supplies them,
/// derived the way a fresh host launch derives them: from this trusted process
/// and the recovery edge, never from the run store, which a lane can rewrite.
/// The graph's own directory comes from the host-owned graph journal.
pub(crate) fn recovery_launch(
    state_root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<provenant_run::HostLaunchContext, Box<dyn std::error::Error>> {
    Ok(provenant_run::HostLaunchContext::from_environment(
        state_root,
        cwd,
        provenant_os::git::git_executable().ok_or("Git unavailable")?,
    ))
}

/// Drain the reactions a settlement owes: the graph advance, then the notice
/// and the chair log. `live` is the launch context of the guardian that
/// settled, which only the host holds; recovery has none and derives its own.
/// If those cannot be derived the graph reaction stays pending.
pub(crate) fn settlement_reactions(
    subject: &crate::settlement::Subject,
    live: Option<provenant_run::HolderContext>,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    crate::settlement::drain_run(subject, &mut |reaction| {
        let launch = match live.as_ref().and_then(|live| live.launch.as_ref()) {
            Some(launch) => launch.clone(),
            None => recovery_launch(&subject.state_root, &subject.checkout)?,
        };
        let settled = provenant_run::HolderContext {
            schema_version: 1,
            roots: provenant_run::RootSeed {
                home: subject.home.clone(),
                checkout: subject.checkout.clone(),
                state_root: subject.state_root.clone(),
            },
            project_id: reaction.project,
            actor_id: subject.actor,
            attempt_id: reaction.attempt,
            host_delivery: None,
            launch: Some(launch),
            sealed: None,
        };
        advance_graphs_after_settlement(&settled, workers)
    })
}

pub(crate) fn advance_graphs_after_settlement(
    settled: &provenant_run::HolderContext,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::FileJournal;

    let Some(host) = &settled.launch else {
        return Ok(());
    };
    let roots = crate::roots::child(
        &settled.roots.home,
        &settled.roots.checkout,
        host.configuration_root.clone(),
    )?;
    // A lane's settlement must not stop unrelated graphs from advancing.
    // Settlement records its own retention reason on the attempt.
    let settlement_error = settle_owned_worktree(settled, &roots, workers, None).err();
    let settlement_reason = settlement_error.as_ref().map(|error| error.to_string());
    let mut first_error = settlement_error;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: settled.project_id,
        actor_id: settled.actor_id,
        read_prompts: true,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    let session = provenant_cli::run::Session { store, context };
    let store = &session.store;
    // The holder admitted this successor before releasing predecessor custody.
    // Launch only now, through the ordinary host resume path. Replaying this
    // durable settlement reaction reuses its operation and queued run.
    if let Some(run) = store.prepare_budget_wrap_up(settled.attempt_id, &call)? {
        let edge = provenant_cli::run::Edge {
            roots: &roots,
            home: &settled.roots.home,
            assets: &settled.roots.checkout,
            cwd: &host.cwd,
        };
        provenant_cli::run::launch_budget_wrap_up(&session, run, &edge, &call, host)
            .map_err(|failure| std::io::Error::other(failure.message))?;
    }
    let record = store.record(settled.attempt_id, &call)?;
    let operation = store
        .attempt_operation(settled.attempt_id, &call)?
        .operation_id;
    if let Some(id) = matching_graph_hint(&roots.state, settled.project_id, &record, &operation) {
        let result = advance_attempt_graph(settled, &roots, &record, &operation, &id, workers);
        // If the attempt store itself refused the retention receipt, retain
        // the error on the graph containing this lane, without affecting
        // the status or advancement of any other graph.
        let settlement_reason = settlement_reason.as_deref();
        // The guardian's output is discarded, so `graph status` shows a failure.
        let advance_reason = result.as_ref().err().map(|error| error.to_string());
        let recorded = FileJournal::set_advance_error(
            &roots.state,
            settled.project_id,
            &id,
            advance_reason.as_deref().or(settlement_reason),
        );
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
        if let Err(error) = recorded {
            first_error.get_or_insert(error.into());
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Wake only the graph owned by this host-custodied attempt, after admission.
/// The relay has already committed and released the journal lock.
pub(crate) fn advance_live_graph(
    holder: &provenant_run::HolderContext,
    id: &str,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    let host = holder
        .launch
        .as_ref()
        .ok_or("host launch context missing")?;
    let roots = crate::roots::child(
        &holder.roots.home,
        &holder.roots.checkout,
        host.configuration_root.clone(),
    )?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let execution = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: holder.project_id,
        actor_id: holder.actor_id,
        read_prompts: false,
    };
    let store = provenant_run::RunStore::open(&execution, &call)?;
    let record = store.record(holder.attempt_id, &call)?;
    let operation = store
        .attempt_operation(holder.attempt_id, &call)?
        .operation_id;
    advance_attempt_graph(holder, &roots, &record, &operation, id, workers)
}

fn advance_attempt_graph(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    record: &provenant_records::run::AttemptRecord,
    operation: &provenant_core::OperationId,
    id: &str,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, advance_with_budget};
    let host = settled
        .launch
        .as_ref()
        .ok_or("host launch context missing")?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 55)?;
    let mut journal = FileJournal::open(
        &roots.state,
        settled.project_id,
        id,
        budget,
        &cancel,
        &clock,
    )?;
    let Some(checkpoint) = journal.load(id)? else {
        return Ok(());
    };
    if !checkpoint_matches_attempt(&checkpoint, record, operation) {
        return Ok(());
    }
    let Some(execution) = FileJournal::pending_execution(&roots.state, settled.project_id, id)?
    else {
        return Ok(());
    };
    let context = Context::open(roots.clone(), &settled.roots.home, &execution.directory)?;
    if context.identity.project != settled.project_id {
        return Err(EdgeError::Invalid.into());
    }
    // The host-custodied attempt supplies launch inputs for the entire graph.
    let mut host = host.clone();
    host.cwd = execution.directory.clone();
    let factory = Factory {
        project_attribution: crate::usage::attribution_enabled(&context.roots),
        context,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: SystemExecutor::new(4)?,
        home: settled.roots.home.clone(),
        cwd: execution.directory,
        assets: settled.roots.checkout.clone(),
        host_launch: Some(host),
        workers,
    };
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        launch_warnings: Vec::new(),
        repo_facts: Default::default(),
        factory: &factory,
        integration_base: Some(BoundGraphBases {
            integration: execution.integration_base,
            review: execution.review_base,
        }),
        integration_branch: execution.document.integration_branch.clone(),
        network: graph_network(&execution.document)?,
        capability_overrides: graph_capability_overrides(&execution.document)?,
        exclude: graph_exclude(&execution.document)?,
        budget,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let graph_config = &factory.context.config.graph;
    let advanced = advance_with_budget(
        &execution.document,
        &mut owner,
        &mut journal,
        &graph_config.limits,
        &graph_config.budget,
    )?;
    if matches!(
        advanced.status,
        provenant_fabric_runtime::graph::GraphStatus::Complete
            | provenant_fabric_runtime::graph::GraphStatus::Cancelled
    ) {
        crate::custody::settle_graph(roots, settled.project_id, id, settled.actor_id)?;
    }
    Ok(())
}

/// `operation` is the record's submission, which matches an attempt whose
/// reference was never written back.
pub(crate) fn checkpoint_matches_attempt(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    record: &provenant_records::run::AttemptRecord,
    operation: &provenant_core::OperationId,
) -> bool {
    checkpoint
        .nodes
        .iter()
        .flat_map(|node| &node.attempts)
        .any(|attempt| {
            attempt
                .attempt_ref
                .as_deref()
                .and_then(|reference| reference.rsplit('/').next())
                == Some(record.attempt_id.to_string().as_str())
                || record.retry_of.is_some_and(|prior| {
                    attempt
                        .attempt_ref
                        .as_deref()
                        .and_then(|reference| reference.rsplit('/').next())
                        == Some(prior.to_string().as_str())
                })
                || graph_operation("graph-attempt-operation", &attempt.key)
                    .is_ok_and(|derived| &derived == operation)
        })
}

pub(crate) fn matching_graph_hint(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    record: &provenant_records::run::AttemptRecord,
    operation: &provenant_core::OperationId,
) -> Option<String> {
    let id = record.context.as_ref()?.graph_id.as_deref()?;
    provenant_fabric_runtime::graph::FileJournal::read_checkpoint(root, project, id)
        .ok()
        .flatten()
        .filter(|checkpoint| checkpoint_matches_attempt(checkpoint, record, operation))?;
    Some(id.to_owned())
}

pub(crate) fn settle_owned_worktree(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    _workers: &dyn WorkerObserver,
    delivery_override: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<(), Box<dyn std::error::Error>> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: settled.project_id,
        actor_id: settled.actor_id,
        read_prompts: true,
    };
    let store =
        provenant_run::RunStore::open(&context, &call)?.with_writer_registry(roots.state.as_path());
    let record = store.record(settled.attempt_id, &call)?;
    if record.superseded_by.is_some()
        || !matches!(record.state, provenant_run::AttemptState::Terminal { .. })
    {
        return Ok(());
    }
    let task = store.request(record.attempt_id, &call)?.task;
    let Some(writer) = task.writer else {
        return Ok(());
    };
    if task.deliver.is_none()
        && delivery_override.is_none()
        && record.delivery.is_none()
        && store.delivered(record.attempt_id, &call)?.is_none()
    {
        let operation = store
            .attempt_operation(record.attempt_id, &call)?
            .operation_id;
        let graph_member =
            matching_graph_hint(&roots.state, settled.project_id, &record, &operation).is_some();
        // An unmatched graph operation retains custody without scanning other graphs.
        if graph_member || operation.as_str().starts_with("graph-") {
            let reason = if graph_member {
                "graph writer worktree retained for continuation, later rounds and explicit delivery"
            } else {
                "graph custody could not be read; worktree retained for explicit delivery"
            };
            record_worktree_settlement(
                &store,
                record.attempt_id,
                &serde_json::json!({"schema_version":1,"state":"retained","worktree":writer.worktree,"reason":reason}),
            )?;
            return Ok(());
        }
    }
    // Settlement only starts collection. Fetching refs, inspecting output trees
    // and removing worktrees/caches belong to the detached completion worker.
    // Replay completed/pruning receipts without walking a worktree.
    let prior = store.worktree_settlement(record.attempt_id, &call)?;
    if let Some(receipt) = replay_worktree_settlement(prior.as_ref(), &writer.worktree)? {
        record_worktree_settlement(&store, record.attempt_id, &receipt)?;
        return Ok(());
    }
    let pickup = store.delivered(record.attempt_id, &call)?;
    let delivery = delivery_override
        .or(pickup.as_ref().map(|(_, receipt)| receipt))
        .or(record.delivery.as_ref());
    let reason = settlement_retention_reason(&record.state, task.deliver.is_some(), delivery)
        .unwrap_or("worktree cleanup deferred to completion GC");
    // Record before spawning so the worker's receipt always comes later. A
    // worker that began pruning owns the receipt and its replay, so the check
    // runs under the receipt lock rather than against `prior`.
    store.record_worktree_settlement_if(
        record.attempt_id,
        &serde_json::json!({
            "schema_version":1,
            "state":"retained",
            "worktree":writer.worktree,
            "reason":reason,
            "cleanup_deferred":true,
        }),
        |current| current.is_none_or(|receipt| receipt["state"] != "pruning"),
        &call,
    )?;
    // One attempt. A miss records nothing; the gc sweep walks every lane.
    if let Err(error) = super::gc_cleanup::spawn(settled, delivery_override) {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: completion GC did not start: {error}; the next gc sweep collects the worktree"
        );
    }
    Ok(())
}

pub(crate) fn record_worktree_settlement(
    store: &provenant_run::RunStore,
    id: provenant_core::AttemptId,
    receipt: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    store.record_worktree_settlement(id, receipt, &call)?;
    Ok(())
}

pub(crate) fn settlement_retention_reason(
    state: &provenant_run::AttemptState,
    delivery_requested: bool,
    delivery: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Option<&'static str> {
    if !matches!(
        state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Succeeded,
            ..
        }
    ) {
        return Some("lane did not succeed; worktree retained for resume");
    }
    // The tree is kept on purpose without a Delivered receipt (e.g. NoNewCommits); reclaimed via gc or disk reclaim.
    if (delivery_requested || delivery.is_some())
        && !delivery.is_some_and(|receipt| {
            matches!(
                receipt.state,
                provenant_run::WriterDeliveryState::Delivered { .. }
            )
        })
    {
        return Some("delivery has not succeeded; worktree retained for pickup");
    }
    None
}

pub(crate) fn replay_worktree_settlement(
    receipt: Option<&serde_json::Value>,
    worktree: &Path,
) -> Result<Option<serde_json::Value>, std::io::Error> {
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    if receipt["state"] == "pruned" {
        return Ok(Some(receipt.clone()));
    }
    if receipt["state"] != "pruning" || receipt["worktree"] != serde_json::json!(worktree) {
        return Ok(None);
    }
    match std::fs::symlink_metadata(worktree) {
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut receipt = receipt.clone();
            receipt["state"] = serde_json::json!("pruned");
            receipt["recovered_from_intent"] = serde_json::json!(true);
            Ok(Some(receipt))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn settle_delivered_attempt(
    edge: &provenant_cli::run::Edge<'_>,
    reply: &mut provenant_cli::run::Reply,
    workers: &dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    if reply.result.status != provenant_records::Outcome::Ok {
        return Ok(());
    }
    let Some(id) = reply.result.data["attempt_id"].as_str() else {
        return Ok(());
    };
    // `lane deliver --deliver none` has no delivery receipt to settle.
    if reply.result.data["delivery"].is_null() {
        return Ok(());
    }
    let id = id.parse()?;
    let delivery = serde_json::from_value::<provenant_run::WriterDeliveryReceipt>(
        reply.result.data["delivery"].clone(),
    )?;
    if matches!(
        delivery.state,
        provenant_run::WriterDeliveryState::Delivered { .. }
    ) {
        let context = Context::open(edge.roots.clone(), edge.home, edge.cwd)?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        provenant_fabric_runtime::graph::FileJournal::retry_delivered_attempt(
            &edge.roots.state,
            context.identity.project,
            id,
            budget,
            &cancel,
            &clock,
        )?;
    }
    if let Some(receipt) = settle_attempt(edge, id, Some(&delivery), workers)? {
        reply.human.push_str(&format!(
            "worktree {}{}\n",
            receipt["state"].as_str().unwrap_or("unknown"),
            receipt["reason"]
                .as_str()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ));
        reply.result.data["worktree_settlement"] = receipt;
    }
    Ok(())
}

pub(crate) fn settle_attempt(
    edge: &provenant_cli::run::Edge<'_>,
    id: provenant_core::AttemptId,
    delivery: Option<&provenant_run::WriterDeliveryReceipt>,
    workers: &dyn WorkerObserver,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error>> {
    let context = Context::open(edge.roots.clone(), edge.home, edge.cwd)?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let execution = provenant_run::ExecutionContext {
        state_root: edge.roots.state.clone(),
        project_id: context.identity.project,
        actor_id: context.identity.inbox.actor,
        read_prompts: true,
    };
    let store = provenant_run::RunStore::open(&execution, &call)?;
    let record = store.record(id, &call)?;
    if let Some(receipt) = store.worktree_settlement(id, &call)?
        && receipt["state"] == "pruned"
    {
        return Ok(Some(receipt));
    }
    let settled = provenant_run::HolderContext {
        schema_version: 1,
        roots: provenant_run::RootSeed {
            home: edge.home.into(),
            checkout: edge.assets.into(),
            state_root: edge.roots.state.as_path().into(),
        },
        project_id: record.project_id,
        actor_id: execution.actor_id,
        attempt_id: id,
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    settle_owned_worktree(&settled, edge.roots, workers, delivery)?;
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    Ok(store.worktree_settlement(id, &call)?)
}
