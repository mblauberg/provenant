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
            run: settled.run_id,
        },
        settled.launch.clone(),
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
    live: Option<provenant_run::HostLaunchContext>,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    crate::settlement::drain_run(subject, &mut |reaction| {
        let launch = match &live {
            Some(live) => live.clone(),
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
            run_id: reaction.run,
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
    let pressure_error = crate::disk_guard::settlement_gc(settled).err();
    let settlement_error = settle_owned_worktree(settled, &roots, workers, None).err();
    let settlement_reason = settlement_error.as_ref().map(|error| error.to_string());
    let mut first_error = settlement_error.or_else(|| pressure_error.map(Into::into));
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
    let record = store.record(settled.attempt_id, &call)?;
    if let Some(id) = matching_graph_hint(&roots.state, settled.project_id, &record) {
        let result = advance_attempt_graph(settled, &roots, &record, &id, workers);
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
    advance_attempt_graph(holder, &roots, &record, id, workers)
}

fn advance_attempt_graph(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    record: &provenant_records::run::AttemptRecord,
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
    if !checkpoint_matches_attempt(&checkpoint, record) {
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
        mcp_project: None,
        workers,
    };
    let mut owner = BoundGraphOwner {
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
    advance_with_budget(
        &execution.document,
        &mut owner,
        &mut journal,
        &graph_config.limits,
        &graph_config.budget,
    )?;
    Ok(())
}

pub(crate) fn checkpoint_matches_attempt(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    record: &provenant_records::run::AttemptRecord,
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
                || graph_operation("graph-attempt-operation", &attempt.key)
                    .is_ok_and(|operation| operation == record.operation_id)
        })
}

pub(crate) fn matching_graph_hint(
    root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    record: &provenant_records::run::AttemptRecord,
) -> Option<String> {
    let id = record.context.as_ref()?.graph_id.as_deref()?;
    provenant_fabric_runtime::graph::FileJournal::read_checkpoint(root, project, id)
        .ok()
        .flatten()
        .filter(|checkpoint| checkpoint_matches_attempt(checkpoint, record))?;
    Some(id.to_owned())
}

pub(crate) fn settle_owned_worktree(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    workers: &dyn WorkerObserver,
    delivery_override: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{self, WorktreeGcPolicy, WorktreeOrigin};
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
    if !matches!(record.state, provenant_run::AttemptState::Terminal { .. }) {
        return Ok(());
    }
    let task = store
        .request(record.run_id, &call)?
        .tasks
        .into_iter()
        .find(|task| task.task_id == record.task_id)
        .ok_or("settled task missing")?;
    let Some(writer) = task.writer else {
        return Ok(());
    };
    if task.deliver.is_none()
        && delivery_override.is_none()
        && record.delivery.is_none()
        && store.delivered(record.attempt_id, &call)?.is_none()
    {
        let graph_member = matching_graph_hint(&roots.state, settled.project_id, &record).is_some();
        // An unmatched graph operation retains custody without scanning other graphs.
        if graph_member || record.operation_id.as_str().starts_with("graph-") {
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
    // Refresh before taking the collection lock and before the prune budget starts.
    // Explicit worktrees have no owner record, so use their verified HEAD.
    let mut tracking_ref_warning = None;
    let mut lane_owned = false;
    let _ = crate::worktrees::with_context(
        roots,
        &settled.roots.home,
        &writer.project,
        std::time::Duration::from_secs(15),
        workers,
        |context, _| {
            use std::io::Read;
            let tree = provenant_confinement::verify_worktree(context.repo, &writer.worktree)
                .map_err(worktrees::WorktreeError::from)?;
            let owner = worktrees::load_owner(&tree)?;
            lane_owned = owner
                .as_ref()
                .is_some_and(|owner| owner.origin == WorktreeOrigin::Lane);
            let branch = if let Some(owner) = owner {
                Some(owner.branch)
            } else {
                let mut head = Vec::new();
                tree.repo()
                    .private_dir()
                    .open_read(std::ffi::OsStr::new("HEAD"))?
                    .take(4097)
                    .read_to_end(&mut head)
                    .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
                if head.len() > 4096 {
                    None
                } else {
                    std::str::from_utf8(&head)
                        .ok()
                        .and_then(|head| head.trim().strip_prefix("ref: refs/heads/"))
                        .map(str::to_owned)
                }
            };
            if let Some(branch) = branch
                && let Err(error) = crate::gc_support::refresh_writer_tracking_ref(context, &branch)
            {
                tracking_ref_warning = Some(error.to_string());
            }
            Ok(())
        },
    );
    if !lane_owned {
        tracking_ref_warning = None;
    }
    let _collection = match crate::worktrees::event_collection_lock(roots) {
        Ok(lock) => lock,
        // A long-running collector still owns the pass. Leave a visible
        // receipt rather than skipping silently; replay ignores retained ones.
        Err(crate::worktrees::Error::Fs(provenant_os::fs::FsError::LockTimeout { .. })) => {
            let receipt = serde_json::json!({"schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":crate::worktrees::COLLECTION_BUSY});
            // Never replace a receipt that already settled or began pruning.
            if store
                .worktree_settlement(record.attempt_id, &call)?
                .is_none()
            {
                record_worktree_settlement(&store, record.attempt_id, &receipt)?;
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(receipt) = replay_worktree_settlement(
        store
            .worktree_settlement(record.attempt_id, &call)?
            .as_ref(),
        &writer.worktree,
    )? {
        record_worktree_settlement(&store, record.attempt_id, &receipt)?;
        return Ok(());
    }
    let pickup = store.delivered(record.attempt_id, &call)?;
    let delivery_request = pickup
        .as_ref()
        .map(|(request, _)| request)
        .or(task.deliver.as_ref());
    let delivery = delivery_override
        .or(pickup.as_ref().map(|(_, receipt)| receipt))
        .or(record.delivery.as_ref());
    let retention_reason =
        settlement_retention_reason(&record.state, task.deliver.is_some(), delivery);
    let mut pruning_intent = None;
    let collection_error = std::cell::RefCell::new(None::<String>);
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let config = provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)?;
        let settings = config
            .gc
            .projects
            .get(&settled.project_id.to_string())
            .cloned()
            .unwrap_or_default();
        Ok(crate::worktrees::with_context(
            roots,
            &settled.roots.home,
            &writer.project,
            std::time::Duration::from_secs(120),
            workers,
            |context, cancel| {
                let tree = provenant_confinement::verify_worktree(context.repo, &writer.worktree)
                    .map_err(worktrees::WorktreeError::from)?;
                let Some(owner) = worktrees::load_owner(&tree)? else {
                    return Ok(None);
                };
                // Resumed attempts share the original owner's lane. Apply the
                // continuation proof before pruning against that owner's ID.
                if owner.origin == WorktreeOrigin::Lane
                    && store
                        .check_continuation_writer(record.attempt_id, &call)
                        .map_err(|_| worktrees::WorktreeError::UnknownWriter)?
                        != Some(owner.clone())
                {
                    return Err(worktrees::WorktreeError::UnknownWriter.into());
                }
                if owner.origin != WorktreeOrigin::Lane {
                    return Ok(None);
                }
                let owner_attempt = owner
                    .attempt_id
                    .ok_or(worktrees::WorktreeError::UnknownWriter)?;
                let states = || {
                    crate::gc_support::lane_states(
                        roots,
                        &settled.roots.home,
                        &settled.roots.checkout,
                        settled.project_id,
                    )
                    .ok()
                };
                let terminal = |path: &Path| {
                    states().as_ref().and_then(|snapshot| snapshot.get(path)) == Some(&true)
                };
                let no_live =
                    |path: &Path| states().is_some_and(|snapshot| snapshot.no_live_lanes(path));
                let output_policy = WorktreeGcPolicy {
                    upstream: None,
                    terminal: &terminal,
                    no_live_lanes: &no_live,
                    regenerable: &settings.regenerable,
                    keep: &settings.keep,
                    written_before_ns: i128::MAX,
                    foreign_merged_clean: false,
                    dry_run: false,
                };
                // A failed pre-prune step must not replace a retained lane's
                // real reason, so record it and carry on.
                let pre_prune = || -> Result<(), Box<dyn std::error::Error>> {
                    let lane = worktrees::list(context, cancel)?
                        .into_iter()
                        .find(|lane| lane.path == writer.worktree)
                        .ok_or(worktrees::WorktreeError::NotFound)?;
                    worktrees::collect(
                        context,
                        std::slice::from_ref(&lane),
                        &output_policy,
                        cancel,
                    )?;
                    // Retained lanes can resume with fresh caches. Restrict this
                    // pass to their owner, including detached remnants, and prove
                    // terminal state again while acquiring writer exclusion.
                    if lane.occupancy.is_available() {
                        provenant_confinement::cache::gc_worktree_cargo_caches_with_writer_guard(
                            &settled.roots.home,
                            &provenant_confinement::cache::CacheGcPolicy {
                                idle_before_ns: Some(i128::MAX),
                                project_owners: [writer.worktree.clone()].into(),
                                selected_only: true,
                                ..Default::default()
                            },
                            |path| {
                                let guard =
                                    worktrees::cache_writer_guard(context.project_state, path)?;
                                if !terminal(path) || !no_live(path) {
                                    return Err(worktrees::Occupancy::custody_unknown(
                                        path.to_path_buf(),
                                        "lane settlement could not be verified",
                                    ));
                                }
                                Ok(Some(guard))
                            },
                        )
                        .map_err(|error| crate::worktrees::Error::Permissions(*error.source))?;
                    }
                    Ok(())
                };
                if let Err(error) = pre_prune() {
                    *collection_error.borrow_mut() = Some(error.to_string());
                }
                if retention_reason.is_some() {
                    return Ok(None);
                }
                let upstream = delivery
                    .filter(|receipt| {
                        matches!(
                            receipt.state,
                            provenant_run::WriterDeliveryState::Delivered { .. }
                        )
                    })
                    .map(|receipt| {
                        if receipt.remote.is_empty() {
                            format!("refs/heads/{}", receipt.branch)
                        } else {
                            format!("refs/remotes/{}/{}", receipt.remote, receipt.branch)
                        }
                    });
                let upstream = upstream.as_deref().unwrap_or(&owner.base);
                let git = provenant_os::git::git_executable()
                    .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                let pinned = provenant_run::resolve_git_base(&git, &writer.project, upstream)
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                let Some(head) = record.head.or_else(|| {
                    (record.provider_launched == Some(false))
                        .then(|| owner.base.parse().ok())
                        .flatten()
                }) else {
                    return Err(worktrees::WorktreeError::InvalidOutput.into());
                };
                let policy = WorktreeGcPolicy {
                    // A push or open PR has not landed the branch. Only an
                    // integration receipt grants local branch cleanup.
                    upstream: delivery_request
                        .filter(|request| {
                            matches!(
                                request.mode,
                                provenant_records::run::DeliveryMode::Integrate(_)
                            )
                        })
                        .map(|_| pinned),
                    ..output_policy
                };
                let mut intent = serde_json::json!({
                    "schema_version":1, "state":"pruning", "worktree":writer.worktree,
                    "branch":format!("refs/heads/{}", owner.branch)
                });
                if let Some(warning) = tracking_ref_warning.as_ref() {
                    intent["tracking_ref_warning"] = serde_json::json!(warning);
                }
                record_worktree_settlement(&store, record.attempt_id, &intent)
                    .map_err(|_| worktrees::WorktreeError::RegistrationUnconfirmed)?;
                pruning_intent = Some(intent);
                Ok(Some(worktrees::prune_owned(
                    context,
                    &writer.worktree,
                    owner_attempt,
                    head,
                    pinned,
                    &policy,
                    cancel,
                )?))
            },
        )?)
    })();
    let mut receipt = match result {
        Ok(None) => {
            let reason = match retention_reason {
                Some(reason) => reason,
                None => return Ok(()),
            };
            let reason = match collection_error.take() {
                Some(error) => format!("{reason}; collection failed: {error}"),
                None => reason.to_owned(),
            };
            serde_json::json!({"schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":reason})
        }
        Ok(Some(result)) => {
            serde_json::json!({"schema_version":1, "state":"pruned", "worktree": writer.worktree, "result":result})
        }
        Err(error) => {
            if let Some(mut pruned) =
                replay_worktree_settlement(pruning_intent.as_ref(), &writer.worktree)?
            {
                pruned["cleanup_warning"] = serde_json::json!(error.to_string());
                pruned
            } else {
                serde_json::json!({"schema_version":1, "state":"retained", "worktree": writer.worktree, "reason":error.to_string()})
            }
        }
    };
    if let Some(warning) = tracking_ref_warning {
        receipt["tracking_ref_warning"] = serde_json::json!(warning);
    }
    // The Git mutation has its own 120-second bound. Start a fresh storage
    // budget after it so a completed prune can always publish its receipt.
    record_worktree_settlement(&store, record.attempt_id, &receipt)?;
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
        run_id: record.run_id,
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
