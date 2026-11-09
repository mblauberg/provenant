//! Completion-triggered GC. Only this detached worker walks and mutates lane output.
use super::*;
use sha2::{Digest, Sha256};

fn request_name(request: &str) -> String {
    let digest = Sha256::digest(request.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{hex}.json")
}

fn spawn(request: &str, home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let argv = [
        std::env::current_exe()?.into_os_string(),
        "internal".into(),
        "lane-cleanup".into(),
        request.into(),
    ];
    provenant_os::exec::spawn_detached(&argv, home)?;
    Ok(())
}

// Dispatch records are written before spawning, so a slow or crashed worker
// cannot be respawned on every host tick. Five attempts, ten minutes apart
// initially, bound both process churn and the lifetime of transient failures.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Retry {
    attempts: u32,
    next_at: u64,
}

fn now_seconds() -> Result<u64, Box<dyn std::error::Error>> {
    use provenant_core::Clock;
    Ok(u64::try_from(SystemClock.wall_now()?.as_micros() / 1_000_000).unwrap_or(0))
}

fn final_receipt(receipt: &serde_json::Value) -> bool {
    if receipt["retryable"] == true || receipt["reason"] == crate::worktrees::COLLECTION_BUSY {
        return false;
    }
    if receipt["state"] == "retained" {
        return receipt["reason"].as_str().is_some_and(|reason| {
            !reason.is_empty() && reason != crate::worktrees::COLLECTION_BUSY
        });
    }
    (receipt["state"] == "pruned" || receipt["cleanup_complete"] == true)
        && receipt["cleanup_warning"].is_null()
}

pub(crate) fn queue(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    delivery: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut request = settled.clone();
    request.launch = None;
    request.sealed = None;
    request.host_delivery = None;
    let request = serde_json::to_string(&(request, delivery))?;
    let directory =
        provenant_config::private_directory(&roots.state.as_path().join("gc/completions"), true)?;
    directory.atomic_write(
        request_name(&request).as_ref(),
        "completion",
        request.as_bytes(),
        provenant_os::fs::PublishMode::Replace,
    )?;
    // Settlement touches only this request. Queue enumeration belongs to the
    // host tick, never to the synchronous settlement path.
    let retries = provenant_config::private_directory(
        &roots.state.as_path().join("gc/completion-retries"),
        true,
    )?;
    let _dispatch = match retries.try_lock(
        "dispatch.lock".as_ref(),
        provenant_os::fs::LockMode::Exclusive,
    ) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let name = request_name(&request);
    let mut retry = match retries.read_bounded(name.as_ref(), 1024) {
        Ok(bytes) => serde_json::from_slice::<Retry>(&bytes)?,
        Err(error) if error.is_not_found() => Retry::default(),
        Err(error) => return Err(error.into()),
    };
    let now = now_seconds()?;
    if now < retry.next_at || retry.attempts >= 5 {
        return Ok(());
    }
    dispatch(
        &retries,
        name.as_ref(),
        &mut retry,
        &request,
        &settled.roots.home,
        now,
    )
}

fn dispatch(
    retries: &provenant_os::fs::Directory,
    name: &std::ffi::OsStr,
    retry: &mut Retry,
    request: &str,
    home: &Path,
    now: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    retry.attempts += 1;
    retry.next_at = now.saturating_add(600 * (1_u64 << (retry.attempts - 1)));
    retries.atomic_write(
        name,
        "retry",
        &serde_json::to_vec(retry)?,
        provenant_os::fs::PublishMode::Replace,
    )?;
    spawn(request, home)
}

/// Retry only due transient work, with a bounded batch and durable backoff.
pub(crate) fn retry_pending(state: &Path, home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let directory = match provenant_os::fs::Directory::open_anchor(&state.join("gc/completions")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let metadata = provenant_config::private_directory(&state.join("gc/completion-retries"), true)?;
    let _dispatch = match metadata.try_lock(
        "dispatch.lock".as_ref(),
        provenant_os::fs::LockMode::Exclusive,
    ) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let now = now_seconds()?;
    let mut dispatched = 0;
    for entry in directory.entries()? {
        let name = entry?;
        let bytes = match directory.read_bounded(&name, 64 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(error.into()),
        };
        let mut retry = match metadata.read_bounded(&name, 1024) {
            Ok(bytes) => serde_json::from_slice::<Retry>(&bytes)?,
            Err(error) if error.is_not_found() => Retry::default(),
            Err(error) => return Err(error.into()),
        };
        if now < retry.next_at {
            continue;
        }
        if retry.attempts >= 5 {
            // Preserve the exhausted request as diagnostic evidence outside
            // the active queue; it is never automatically spawned again.
            let exhausted =
                provenant_config::private_directory(&state.join("gc/completion-exhausted"), true)?;
            exhausted.atomic_write(
                &name,
                "exhausted",
                &bytes,
                provenant_os::fs::PublishMode::Replace,
            )?;
            directory.unlink(&name)?;
            metadata.unlink(&name)?;
            continue;
        }
        dispatch(
            &metadata,
            &name,
            &mut retry,
            std::str::from_utf8(&bytes)?,
            home,
            now,
        )?;
        dispatched += 1;
        if dispatched == 4 {
            break;
        }
    }
    Ok(())
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
    let directory =
        provenant_config::private_directory(&roots.state.as_path().join("gc/completions"), true)?;
    let locks = provenant_config::private_directory(
        &roots.state.as_path().join("gc/completion-locks"),
        true,
    )?;
    let _worker = match locks.try_lock(
        format!("{}.lock", settled.attempt_id).as_ref(),
        provenant_os::fs::LockMode::Exclusive,
    ) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
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
    let store = provenant_run::RunStore::open(&context, &call)?;
    // A later explicit delivery may supersede an earlier final retention.
    // Finality belongs to this request, not every request for the attempt.
    collect_owned_worktree(&settled, &roots, workers, delivery.as_ref())?;
    if store
        .worktree_settlement(settled.attempt_id, &call)?
        .is_some_and(|receipt| final_receipt(&receipt))
    {
        match directory.unlink(request_name(request).as_ref()) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        let retries = provenant_config::private_directory(
            &roots.state.as_path().join("gc/completion-retries"),
            true,
        )?;
        match retries.unlink(request_name(request).as_ref()) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn collect_owned_worktree(
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
        .request(record.run_id, &call)?
        .tasks
        .into_iter()
        .find(|task| task.task_id == record.task_id)
        .ok_or("settled task missing")?;
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
    let _collection = match crate::worktrees::collection_lock_for(roots, Some(120)) {
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
                        return Err("completion output cleanup failed; retry queued".into());
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
                let upstream = upstream.as_deref().unwrap_or(&owner.base);
                let pinned = worktrees::resolve_base(context, upstream, cancel)?;
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
                        // Keep the durable request for the next host tick.
                        receipt["cleanup_warning"] =
                            serde_json::json!("remote cleanup failed; retry queued");
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

#[cfg(test)]
// Custom harness=false drivers include this module without running its unit tests.
#[allow(dead_code, unused_imports)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn final_retention_reason_wins_over_cleanup_warning() {
        for reason in [
            "not landed on base and no delivery requested",
            "dirty worktree retained",
            "worktree already removed",
            "task has no writer",
            "graph writer worktree retained for continuation, later rounds and explicit delivery",
        ] {
            assert!(
                final_receipt(&json!({
                    "state": "retained",
                    "reason": reason,
                    "cleanup_complete": false,
                    "cleanup_warning": "output cleanup could not finish"
                })),
                "final retention must drain its request: {reason}"
            );
        }
    }

    #[test]
    fn transient_receipts_remain_pending() {
        for receipt in [
            json!({"state": "retained", "reason": crate::worktrees::COLLECTION_BUSY}),
            json!({"state": "retained", "reason": "attempt is not terminal", "retryable": true}),
            json!({"state": "retained", "reason": "", "cleanup_warning": "unknown failure"}),
            json!({"state": "pruned", "cleanup_warning": "remote cleanup failed; retry queued"}),
            json!({"state": "retained", "reason": "fetch failed", "retryable": true, "cleanup_complete": true}),
            json!({"state": "retained", "reason": crate::worktrees::COLLECTION_BUSY, "cleanup_complete": true}),
        ] {
            assert!(
                !final_receipt(&receipt),
                "transient receipt must stay queued: {receipt}"
            );
        }
        assert!(final_receipt(&json!({"state": "pruned"})));
        assert!(final_receipt(&json!({"cleanup_complete": true})));
    }

    #[test]
    fn retry_tick_honours_backoff_and_drains_exhausted_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let state = temporary.path();
        let requests = provenant_config::private_directory(&state.join("gc/completions"), true)?;
        let retries =
            provenant_config::private_directory(&state.join("gc/completion-retries"), true)?;
        // A missing child working directory makes accidental dispatch fail;
        // these requests must only be deferred or moved into diagnostics.
        let missing_home = state.join("must-not-spawn");
        let waiting_name = request_name("waiting fixture");
        let exhausted_name = request_name("exhausted fixture");
        for name in [&waiting_name, &exhausted_name] {
            requests.atomic_write(
                name.as_ref(),
                "fixture",
                b"not a cleanup request",
                provenant_os::fs::PublishMode::Replace,
            )?;
        }
        let deferred = serde_json::to_vec(&Retry {
            attempts: 2,
            next_at: u64::MAX,
        })?;
        retries.atomic_write(
            waiting_name.as_ref(),
            "fixture",
            &deferred,
            provenant_os::fs::PublishMode::Replace,
        )?;
        retries.atomic_write(
            exhausted_name.as_ref(),
            "fixture",
            &serde_json::to_vec(&Retry {
                attempts: 5,
                next_at: 0,
            })?,
            provenant_os::fs::PublishMode::Replace,
        )?;
        for _ in 0..2 {
            retry_pending(state, &missing_home)?;
            assert_eq!(
                requests.read_bounded(waiting_name.as_ref(), 1024)?,
                b"not a cleanup request"
            );
            assert_eq!(retries.read_bounded(waiting_name.as_ref(), 1024)?, deferred);
            assert!(!state.join("gc/completions").join(&exhausted_name).exists());
            assert!(
                !state
                    .join("gc/completion-retries")
                    .join(&exhausted_name)
                    .exists()
            );
            assert_eq!(
                std::fs::read(state.join("gc/completion-exhausted").join(&exhausted_name))?,
                b"not a cleanup request"
            );
        }
        requests.unlink(waiting_name.as_ref())?;
        retries.unlink(waiting_name.as_ref())?;
        retry_pending(state, &missing_home)?;
        assert_eq!(requests.entries()?.count(), 0);
        Ok(())
    }
}
