//! Completion-triggered GC. Settlement makes one detached attempt to collect the
//! settled lane's worktree. Nothing beyond its settlement receipt is recorded
//! when that attempt misses: the `provenant gc` sweep runs the same owner-bound
//! collection for every settled lane whose receipt is not final.
use super::*;

/// The payload `provenant internal lane-cleanup` accepts.
fn request(
    settled: &provenant_run::HolderContext,
    delivery: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut request = settled.clone();
    request.launch = None;
    request.sealed = None;
    request.host_delivery = None;
    Ok(serde_json::to_string(&(request, delivery))?)
}

// Tests take the request instead of starting a detached process.
#[cfg(test)]
thread_local! {
    pub(crate) static SPAWNED: std::cell::RefCell<Result<Vec<String>, String>> =
        const { std::cell::RefCell::new(Ok(Vec::new())) };
}

/// Start one detached collection of the settled lane's worktree.
pub(crate) fn spawn(
    settled: &provenant_run::HolderContext,
    delivery: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = request(settled, delivery)?;
    #[cfg(test)]
    return SPAWNED.with_borrow_mut(|spawned| match spawned {
        Ok(spawned) => {
            spawned.push(request);
            Ok(())
        }
        Err(error) => Err(error.clone().into()),
    });
    #[cfg(not(test))]
    {
        let argv = [
            std::env::current_exe()?.into_os_string(),
            "internal".into(),
            "lane-cleanup".into(),
            request.into(),
        ];
        provenant_os::exec::spawn_detached(&argv, &settled.roots.home)?;
        Ok(())
    }
}

pub(crate) fn run(
    request: &str,
    workers: &dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    if provenant_cli::fabric::in_lane() {
        return Err("lane cleanup requires host custody".into());
    }
    let (settled, delivery): (
        provenant_run::HolderContext,
        Option<provenant_run::WriterDeliveryReceipt>,
    ) = serde_json::from_str(request)?;
    let mut inputs =
        provenant_core::RootInputs::new(settled.roots.home.clone(), settled.roots.checkout.clone());
    inputs.state_dir = Some(settled.roots.state_root.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    collect_owned_worktree(&settled, &roots, workers, delivery.as_ref())
}

// Keep lock-contention regression tests deterministic without a two-minute sleep.
#[cfg(test)]
thread_local! {
    pub(crate) static COLLECTION_WAIT_SECONDS: std::cell::Cell<u64> = const { std::cell::Cell::new(120) };
}

/// How an owner-bound collection gets the GC collection lock.
pub(crate) enum Collection<'a> {
    /// Settlement's detached worker takes the lock, waiting a bounded time.
    Acquire,
    /// The `provenant gc` sweep already holds the lock. `upstream` is the
    /// default-branch tip that sweep fetched; with no delivery receipt, the
    /// lane lands against it or falls back to the owner's base when upstream is None.
    Held {
        _lock: &'a provenant_os::fs::LockGuard,
        upstream: Option<provenant_core::GitOid>,
    },
}

pub(crate) fn collect_owned_worktree(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    workers: &dyn WorkerObserver,
    delivery_override: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<(), Box<dyn std::error::Error>> {
    collect_owned_worktree_with(
        settled,
        roots,
        workers,
        delivery_override,
        Collection::Acquire,
    )
}

/// The sweep's half of completion cleanup. Settlement makes one detached
/// attempt and leaves a receipt that is not final until it succeeds. For each
/// settled lane whose receipt is still deferred, retryable or mid-prune and
/// whose worktree still exists, run the same owner-bound collection under the
/// lock the sweep holds, landing against the fetched default branch or falling
/// back to the owner's base when upstream is None. That receipt is the only
/// marker: nothing else is recorded. Returns the worktrees this removed.
pub(crate) fn collect_unsettled(
    roots: &provenant_core::Roots,
    home: &Path,
    checkout: &Path,
    project_id: provenant_core::ProjectId,
    lock: &provenant_os::fs::LockGuard,
    upstream: Option<provenant_core::GitOid>,
    workers: &dyn WorkerObserver,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id,
        actor_id: provenant_core::ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let Some(store) = provenant_run::RunStore::open_disk_read_only(&context, &call)? else {
        return Ok(Vec::new());
    };
    let mut pending = Vec::new();
    let mut query = provenant_run::LaneQuery {
        targets: Vec::new(),
        all: true,
        limit: 100,
        after: 0,
        newest_first: false,
        owner: None,
        unseen_after: None,
    };
    loop {
        let page = store.lanes(&query, &call)?;
        for attempt in page.lanes {
            if attempt.superseded_by.is_some()
                || !matches!(attempt.state, provenant_run::AttemptState::Terminal { .. })
            {
                continue;
            }
            let Some(writer) = store
                .request(attempt.attempt_id, &call)?
                .task.writer
            else {
                continue;
            };
            if std::fs::symlink_metadata(&writer.worktree).is_err() {
                continue;
            }
            let unsettled = store
                .worktree_settlement(attempt.attempt_id, &call)?
                .is_some_and(|receipt| {
                    receipt["cleanup_deferred"] == true
                        || receipt["retryable"] == true
                        || receipt["state"] == "pruning"
                });
            if unsettled {
                let actor_id = store.attempt_operation(attempt.attempt_id, &call)?.actor_id;
                pending.push((attempt.attempt_id, actor_id, writer.worktree));
            }
        }
        match page.next {
            Some(next) => query.after = next,
            None => break,
        }
    }
    drop(store);
    let mut removed = Vec::new();
    for (attempt_id, actor_id, worktree) in pending {
        let settled = provenant_run::HolderContext {
            schema_version: 1,
            roots: provenant_run::RootSeed {
                home: home.to_path_buf(),
                checkout: checkout.to_path_buf(),
                state_root: roots.state.as_path().to_path_buf(),
            },
            project_id,
            actor_id,
            attempt_id,
            host_delivery: None,
            launch: None,
            sealed: None,
        };
        // A miss keeps its receipt for the next sweep; carry on with the rest.
        let collected = collect_owned_worktree_with(
            &settled,
            roots,
            workers,
            None,
            Collection::Held {
                _lock: lock,
                upstream,
            },
        );
        if let Err(error) = collected {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: settled lane collection failed for {}: {error}",
                worktree.display()
            );
        } else if std::fs::symlink_metadata(&worktree).is_err() {
            removed.push(worktree);
        }
    }
    Ok(removed)
}

fn collect_owned_worktree_with(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    workers: &dyn WorkerObserver,
    delivery_override: Option<&provenant_run::WriterDeliveryReceipt>,
    collection: Collection<'_>,
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
        record_worktree_settlement(
            &store,
            record.attempt_id,
            &serde_json::json!({
                "schema_version":1, "state":"retained", "reason":"attempt is not terminal", "retryable":true
            }),
        )?;
        return Ok(());
    }
    let task = store
        .request(record.attempt_id, &call)?
        .task;
    let Some(writer) = task.writer else {
        record_worktree_settlement(
            &store,
            record.attempt_id,
            &serde_json::json!({
                "schema_version":1, "state":"retained", "reason":"task has no writer"
            }),
        )?;
        return Ok(());
    };
    // Recover an interrupted prune before classifying an absent worktree.
    let prior = store.worktree_settlement(record.attempt_id, &call)?;
    if replay_worktree_settlement(prior.as_ref(), &writer.worktree)?.is_none() {
        match std::fs::symlink_metadata(&writer.worktree) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                record_worktree_settlement(
                    &store,
                    record.attempt_id,
                    &serde_json::json!({
                        "schema_version":1, "state":"retained", "worktree":writer.worktree,
                        "reason":"worktree already removed"
                    }),
                )?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
    }
    if task.deliver.is_none()
        && delivery_override.is_none()
        && record.delivery.is_none()
        && store.delivered(record.attempt_id, &call)?.is_none()
    {
        let operation = store.attempt_operation(record.attempt_id, &call)?.operation_id;
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
    let facts = store.repo_facts();
    // Refresh before taking the collection lock and before the prune budget starts.
    // Explicit worktrees have no owner record, so use their verified HEAD.
    let mut tracking_ref_warning = None;
    let mut lane_owned = false;
    let _ = crate::worktrees::with_context_using_facts(
        roots,
        &settled.roots.home,
        &writer.project,
        std::time::Duration::from_secs(15),
        workers,
        facts,
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
    #[cfg(test)]
    let collection_wait = COLLECTION_WAIT_SECONDS.get();
    #[cfg(not(test))]
    let collection_wait = 120;
    let sweep_upstream = match collection {
        Collection::Held { upstream, .. } => upstream,
        Collection::Acquire => None,
    };
    let _collection = match matches!(collection, Collection::Acquire)
        .then(|| crate::worktrees::collection_lock_for(roots, Some(collection_wait)))
        .transpose()
    {
        Ok(lock) => lock,
        // A long-running collector still owns the pass. Leave a visible
        // receipt rather than skipping silently; replay ignores retained ones.
        Err(crate::worktrees::Error::Fs(provenant_os::fs::FsError::LockTimeout { .. })) => {
            let receipt = serde_json::json!({"schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":crate::worktrees::COLLECTION_BUSY, "retryable":true});
            // Never replace a receipt that already settled or began pruning.
            store.record_worktree_settlement_if(
                record.attempt_id,
                &receipt,
                |current| current.is_none_or(|receipt| receipt["cleanup_deferred"] == true),
                &call,
            )?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let replayed = replay_worktree_settlement(
        store
            .worktree_settlement(record.attempt_id, &call)?
            .as_ref(),
        &writer.worktree,
    )?;
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
    let mut pruning_intent = replayed.clone();
    let collection_error = std::cell::RefCell::new(None::<String>);
    let final_reason = std::cell::Cell::new(retention_reason);
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        if replayed.is_some() {
            return Ok(None);
        }
        let config = provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)?;
        let settings = config
            .gc
            .projects
            .get(&settled.project_id.to_string())
            .cloned()
            .unwrap_or_default();
        Ok(crate::worktrees::with_context_using_facts(
            roots,
            &settled.roots.home,
            &writer.project,
            std::time::Duration::from_secs(120),
            workers,
            facts,
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
                    let lane = worktrees::list_for_gc_matching(context, cancel, |path, _| {
                        path == writer.worktree
                    })?
                    .into_iter()
                    .find(|lane| lane.path == writer.worktree)
                    .ok_or(worktrees::WorktreeError::NotFound)?;
                    if !lane.occupancy.is_available()
                        || !terminal(&writer.worktree)
                        || !no_live(&writer.worktree)
                    {
                        return Err("completion cleanup awaits free terminal writer custody".into());
                    }
                    let output = worktrees::collect_lazy(
                        context,
                        std::slice::from_ref(&lane),
                        &output_policy,
                        cancel,
                    )?;
                    if output
                        .output
                        .iter()
                        .chain(&output.idle_output)
                        .any(|entry| !entry.removed)
                    {
                        return Err(
                            "completion output cleanup failed; the next gc sweep retries it".into(),
                        );
                    }
                    // Retained lanes can resume with fresh caches. Restrict this
                    // pass to their owner, including detached remnants, and prove
                    // terminal state again while acquiring writer exclusion.
                    if lane.occupancy.is_available() {
                        let report = provenant_confinement::cache::gc_worktree_cargo_caches_with_writer_guard(
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
                        if !report.kept.is_empty() {
                            return Err(
                                "completion cache cleanup retained busy or unobserved output"
                                    .into(),
                            );
                        }
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
                let pinned = match (upstream, sweep_upstream) {
                    (Some(upstream), _) => worktrees::resolve_base(context, &upstream, cancel)?,
                    (None, Some(fetched)) => fetched,
                    (None, None) => worktrees::resolve_base(context, &owner.base, cancel)?,
                };
                let Some(head) = record.head.or_else(|| {
                    (record.provider_launched == Some(false))
                        .then(|| owner.base.parse().ok())
                        .flatten()
                }) else {
                    return Err(worktrees::WorktreeError::InvalidOutput.into());
                };
                if delivery.is_none()
                    && delivery_request.is_none()
                    && !worktrees::landing_in_repo(
                        context,
                        head,
                        &worktrees::LandingTarget {
                            reference: None,
                            tip: pinned,
                        },
                        &[],
                        cancel,
                    )?
                    .is_landed()
                {
                    final_reason.set(Some("not landed on base and no delivery requested"));
                    return Ok(None);
                }
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
    let mut receipt = if let Some(receipt) = replayed {
        receipt
    } else {
        match result {
            Ok(None) => {
                let reason = match final_reason.get() {
                    Some(reason) => reason,
                    None => {
                        record_worktree_settlement(
                            &store,
                            record.attempt_id,
                            &serde_json::json!({
                                "schema_version":1, "state":"retained", "worktree":writer.worktree,
                                "reason":"worktree is not a Provenant-created lane", "cleanup_complete":true
                            }),
                        )?;
                        return Ok(());
                    }
                };
                let warning = collection_error.take();
                serde_json::json!({"schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":reason, "cleanup_complete":true, "cleanup_warning":warning})
            }
            Ok(Some(result)) => {
                let artefacts = worktrees::ArtefactOutcome::from_remove(&result);
                serde_json::json!({"schema_version":1, "state":"pruned", "worktree": writer.worktree, "result":result, "artefacts":artefacts})
            }
            Err(error) => {
                if let Some(mut pruned) =
                    replay_worktree_settlement(pruning_intent.as_ref(), &writer.worktree)?
                {
                    pruned["cleanup_warning"] = serde_json::json!(error.to_string());
                    pruned
                } else {
                    let final_error = error.downcast_ref::<crate::worktrees::Error>().and_then(
                        |error| match error {
                            crate::worktrees::Error::Worktree(
                                worktrees::WorktreeError::Dirty(_)
                                | worktrees::WorktreeError::IgnoredPaths(_),
                            ) => Some("dirty worktree retained"),
                            crate::worktrees::Error::Worktree(
                                worktrees::WorktreeError::NotFound,
                            ) => Some("worktree already removed"),
                            _ => None,
                        },
                    );
                    let reason = final_reason.get().or(final_error);
                    serde_json::json!({"schema_version":1, "state":"retained", "worktree": writer.worktree, "reason":reason.map(str::to_owned).unwrap_or_else(|| error.to_string()), "retryable":reason.is_none(), "cleanup_warning":error.to_string()})
                }
            }
        }
    };
    if let Some(branch) = pruning_intent
        .as_ref()
        .and_then(|intent| intent.get("branch"))
    {
        receipt["branch"] = branch.clone();
    }
    // Retry remote removal even when an earlier worker already pruned the tree.
    if receipt["state"] == "pruned" {
        if let Some(object) = receipt.as_object_mut() {
            let _ = object.remove("cleanup_warning");
        }
        if !receipt["artefacts"].is_array() {
            receipt["artefacts"] = serde_json::json!([]);
        }
    }
    if let Some(warning) = tracking_ref_warning {
        receipt["tracking_ref_warning"] = serde_json::json!(warning);
    }
    // An integrated lane's own remote branch, when a delivery created one,
    // goes once the integration receipt proves it landed. Failure is a
    // warning in the receipt, never a settlement failure.
    if receipt["state"] == "pruned"
        && delivery_request.is_some_and(|request| {
            matches!(
                request.mode,
                provenant_records::run::DeliveryMode::Integrate(_)
            )
        })
        && let Some(delivered) = delivery.filter(|receipt| {
            matches!(
                receipt.state,
                provenant_run::WriterDeliveryState::Delivered { .. }
            )
        })
        && let Some(branch) = pruning_intent
            .as_ref()
            .and_then(|intent| intent["branch"].as_str())
            .and_then(|branch| provenant_confinement::repo::GitRefName::new(branch).ok())
    {
        {
            let sweep = crate::lane_cleanup::Sweep::new(false);
            let outcomes = crate::worktrees::with_context_using_facts(
                roots,
                &settled.roots.home,
                &writer.project,
                std::time::Duration::from_secs(60),
                workers,
                facts,
                |context, cancel| {
                    Ok(crate::lane_cleanup::settle_integrated(
                        context, &branch, delivered, &sweep, cancel,
                    ))
                },
            );
            match (outcomes, receipt["artefacts"].is_array()) {
                (Ok(outcomes), true) => {
                    if outcomes.iter().any(|outcome| {
                        serde_json::to_value(outcome)
                            .ok()
                            .is_some_and(|value| value["outcome"] == "failed")
                    }) {
                        // The next full gc sweep retries landed created refs.
                        receipt["cleanup_warning"] = serde_json::json!(
                            "remote cleanup failed; the next gc sweep retries it"
                        );
                    }
                    receipt["artefacts"]
                        .as_array_mut()
                        .ok_or("missing artefacts")?
                        .extend(
                            outcomes
                                .iter()
                                .filter_map(|outcome| serde_json::to_value(outcome).ok()),
                        );
                }
                (Ok(_), false) => {}
                (Err(error), _) => {
                    receipt["cleanup_warning"] =
                        serde_json::json!(format!("remote branch cleanup did not run: {error}"));
                }
            }
        }
    }
    // The Git mutation has its own 120-second bound. Start a fresh storage
    // budget after it so a completed prune can always publish its receipt.
    record_worktree_settlement(&store, record.attempt_id, &receipt)?;
    Ok(())
}
