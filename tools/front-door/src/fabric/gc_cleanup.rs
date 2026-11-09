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

// The identity payload and retry budget travel together, including on exhaustion.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Retry {
    attempts: u32,
    next_at: u64,
    #[serde(default)]
    last_cause: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Completion {
    request: String,
    #[serde(default)]
    retry: Retry,
}

impl Completion {
    fn decode(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        // Requests persisted before the single-record queue were identity tuples.
        if let Ok(record) = serde_json::from_slice::<Self>(bytes) {
            return Ok(record);
        }
        let _: (
            provenant_run::HolderContext,
            Option<provenant_run::WriterDeliveryReceipt>,
        ) = serde_json::from_slice(bytes)?;
        Ok(Self {
            request: std::str::from_utf8(bytes)?.to_owned(),
            retry: Retry::default(),
        })
    }

    fn save(
        &self,
        directory: &provenant_os::fs::Directory,
        name: &std::ffi::OsStr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        directory
            .atomic_write(
                name,
                "completion",
                &serde_json::to_vec(self)?,
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()?;
        Ok(())
    }
}

fn now_seconds() -> Result<u64, Box<dyn std::error::Error>> {
    use provenant_core::Clock;
    Ok(u64::try_from(SystemClock.wall_now()?.as_micros() / 1_000_000).unwrap_or(0))
}

fn final_receipt(receipt: &serde_json::Value) -> bool {
    if receipt["cleanup_deferred"] == true
        || receipt["retryable"] == true
        || receipt["reason"] == crate::worktrees::COLLECTION_BUSY
    {
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
    publish(&directory, &request)
}

fn publish(
    directory: &provenant_os::fs::Directory,
    request: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let record = Completion {
        request: request.to_owned(),
        retry: Retry::default(),
    };
    // No lock, spawn or network operation belongs on the settlement path.
    // Workers only mutate a claimed record. Requeue can therefore publish fresh
    // intent even while an earlier generation is finishing or exhausting.
    match directory.atomic_write(
        request_name(request).as_ref(),
        "completion",
        &serde_json::to_vec(&record)?,
        provenant_os::fs::PublishMode::NoReplace,
    ) {
        Ok(publication) => {
            publication.require_durable()?;
            Ok(())
        }
        Err(provenant_os::fs::FsError::Rename { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            directory.sync()?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn claimed_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    format!("{}.claim", name.to_string_lossy()).into()
}

// Called under the stable request lock. A dead worker leaves the whole record
// here; the host tick can resume it as soon as its OS lock is released.
fn load_claimed(
    directory: &provenant_os::fs::Directory,
    name: &std::ffi::OsStr,
) -> Result<Option<Completion>, Box<dyn std::error::Error>> {
    let claimed = claimed_name(name);
    match directory.read_bounded(&claimed, 128 * 1024) {
        Ok(bytes) => return Ok(Some(Completion::decode(&bytes)?)),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    match directory.rename_noreplace(name, directory, &claimed) {
        Ok(()) => directory.sync()?,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Ok(Some(Completion::decode(
        &directory.read_bounded(&claimed, 128 * 1024)?,
    )?))
}

fn claim(
    state: &Path,
    name: &std::ffi::OsStr,
) -> Result<Option<provenant_os::fs::LockGuard>, Box<dyn std::error::Error>> {
    let locks = provenant_config::private_directory(&state.join("gc/completion-locks"), true)?;
    match locks.try_lock(
        format!("{}.lock", name.to_string_lossy()).as_ref(),
        provenant_os::fs::LockMode::Exclusive,
    ) {
        Ok(lock) => Ok(Some(lock)),
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn exhaust(
    directory: &provenant_os::fs::Directory,
    state: &Path,
    name: &std::ffi::OsStr,
) -> Result<(), Box<dyn std::error::Error>> {
    let exhausted =
        provenant_config::private_directory(&state.join("gc/completion-exhausted"), true)?;
    // One rename moves both intent and budget. There is no half-deleted state.
    #[cfg(test)]
    exhaustion_checkpoint(ExhaustionPhase::BeforeRename)?;
    directory.replace(&claimed_name(name), &exhausted, name)?;
    #[cfg(test)]
    exhaustion_checkpoint(ExhaustionPhase::AfterRename)?;
    exhausted.sync()?;
    directory.sync()?;
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExhaustionPhase {
    BeforeRename,
    AfterRename,
}

#[cfg(test)]
thread_local! {
    static EXHAUSTION_CRASH: std::cell::Cell<Option<ExhaustionPhase>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn exhaustion_checkpoint(phase: ExhaustionPhase) -> Result<(), Box<dyn std::error::Error>> {
    if EXHAUSTION_CRASH.get() == Some(phase) {
        EXHAUSTION_CRASH.set(None);
        return Err("injected exhaustion crash".into());
    }
    Ok(())
}

/// Retry only due transient work, with a bounded batch and durable backoff.
pub(crate) fn retry_pending(state: &Path, home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    retry_pending_with(state, |request| spawn(request, home))
}

fn retry_pending_with(
    state: &Path,
    mut dispatch: impl FnMut(&str) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = provenant_config::private_directory(&state.join("gc/completions"), true)?;
    let now = now_seconds()?;
    let mut dispatched = 0;
    let mut names = std::collections::BTreeSet::new();
    for entry in directory.entries()? {
        let entry = entry?;
        let text = entry.to_string_lossy();
        let name = text.strip_suffix(".claim").unwrap_or(&text);
        if Path::new(name).extension().is_some_and(|ext| ext == "json") {
            names.insert(std::ffi::OsString::from(name));
        }
    }
    for name in names {
        let Some(worker) = claim(state, &name)? else {
            continue;
        };
        let Some(mut record) = load_claimed(&directory, &name)? else {
            continue;
        };
        if now < record.retry.next_at {
            continue;
        }
        if record.retry.attempts >= 5 {
            exhaust(&directory, state, &name)?;
            continue;
        }
        // Reserve dispatch durably before spawning. A dead child is retried
        // after this bounded delay; OS locks disappear when a worker dies.
        record.retry.attempts += 1;
        record.retry.next_at = now.saturating_add(600 * (1_u64 << (record.retry.attempts - 1)));
        record.retry.last_cause = Some("worker dispatched; completion not yet confirmed".into());
        record.save(&directory, &claimed_name(&name))?;
        let reservation = serde_json::to_vec(&record)?;
        drop(worker);
        if let Err(error) = dispatch(&serde_json::to_string(&record)?) {
            // Preserve the actual failure if this reservation still owns the
            // claimed record. A worker or another tick may already have moved it.
            if let Some(_claim) = claim(state, &name)? {
                match directory.read_bounded(&claimed_name(&name), 128 * 1024) {
                    Ok(bytes) if bytes == reservation => {
                        record.retry.last_cause = Some(error.to_string());
                        record.save(&directory, &claimed_name(&name))?;
                    }
                    Ok(_) => {}
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
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
    let dispatched = Completion::decode(request.as_bytes())?;
    let (settled, delivery): (
        provenant_run::HolderContext,
        Option<provenant_run::WriterDeliveryReceipt>,
    ) = serde_json::from_str(&dispatched.request)?;
    let mut inputs =
        provenant_core::RootInputs::new(settled.roots.home.clone(), settled.roots.checkout.clone());
    inputs.state_dir = Some(settled.roots.state_root.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let directory =
        provenant_config::private_directory(&roots.state.as_path().join("gc/completions"), true)?;
    let name = request_name(&dispatched.request);
    let Some(_worker) = claim(roots.state.as_path(), name.as_ref())? else {
        return Ok(());
    };
    // Reload after claiming: a delayed child must not resurrect drained work.
    let Some(mut record) = load_claimed(&directory, name.as_ref())? else {
        return Ok(());
    };
    let claimed = claimed_name(name.as_ref());
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
    let outcome = collect_owned_worktree(&settled, &roots, workers, delivery.as_ref());
    let receipt = store.worktree_settlement(settled.attempt_id, &call)?;
    match outcome {
        Ok(CollectionOutcome::Finished) if receipt.as_ref().is_some_and(final_receipt) => {
            directory.unlink(&claimed)?;
            directory.sync()?;
        }
        result => {
            record.retry.last_cause = Some(match &result {
                Err(error) => error.to_string(),
                Ok(CollectionOutcome::Busy) => crate::worktrees::COLLECTION_BUSY.to_owned(),
                Ok(CollectionOutcome::Finished) => receipt
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "cleanup has no final receipt".into()),
            });
            // Direct invocations also get durable backoff, before leaving the claim.
            if record.retry.attempts == 0 {
                record.retry.attempts = 1;
                record.retry.next_at = now_seconds()?.saturating_add(600);
            }
            record.save(&directory, &claimed)?;
            result?;
        }
    }
    Ok(())
}

// Keep lock-contention regression tests deterministic without a two-minute sleep.
#[cfg(test)]
thread_local! {
    pub(super) static COLLECTION_WAIT_SECONDS: std::cell::Cell<u64> = const { std::cell::Cell::new(120) };
}

#[derive(PartialEq, Eq)]
pub(crate) enum CollectionOutcome {
    Finished,
    Busy,
}

pub(crate) fn collect_owned_worktree(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    workers: &dyn WorkerObserver,
    delivery_override: Option<&provenant_run::WriterDeliveryReceipt>,
) -> Result<CollectionOutcome, Box<dyn std::error::Error>> {
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
        return Ok(CollectionOutcome::Finished);
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
        return Ok(CollectionOutcome::Finished);
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
                return Ok(CollectionOutcome::Finished);
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
            return Ok(CollectionOutcome::Finished);
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
    let _collection = match crate::worktrees::collection_lock_for(roots, Some(collection_wait)) {
        Ok(lock) => lock,
        // A long-running collector still owns the pass. Leave a visible
        // receipt rather than skipping silently; replay ignores retained ones.
        Err(crate::worktrees::Error::Fs(provenant_os::fs::FsError::LockTimeout { .. })) => {
            let receipt = serde_json::json!({"schema_version":1, "state":"retained", "worktree":writer.worktree, "reason":crate::worktrees::COLLECTION_BUSY, "retryable":true});
            // Never replace a receipt that already settled or began pruning.
            if store
                .worktree_settlement(record.attempt_id, &call)?
                .is_none_or(|receipt| receipt["cleanup_deferred"] == true)
            {
                record_worktree_settlement(&store, record.attempt_id, &receipt)?;
            }
            return Ok(CollectionOutcome::Busy);
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
                        return Ok(CollectionOutcome::Finished);
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
    Ok(CollectionOutcome::Finished)
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
            json!({"state": "retained", "reason": "worktree cleanup queued for completion GC", "cleanup_deferred": true}),
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
    fn exhaustion_moves_one_record_and_requeue_gets_fresh_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let state = temporary.path();
        let requests = provenant_config::private_directory(&state.join("gc/completions"), true)?;
        let request = "exhaustion fixture";
        let name = request_name(request);
        let record = Completion {
            request: request.into(),
            retry: Retry {
                attempts: 5,
                next_at: 0,
                last_cause: Some("worker died".into()),
            },
        };
        record.save(&requests, name.as_ref())?;
        let bytes = requests.read_bounded(name.as_ref(), 4096)?;
        // Crash immediately before the rename: the entire exhausted record is
        // still pending. Repeated host ticks complete this transition once.
        for _ in 0..2 {
            retry_pending_with(state, |_| panic!("exhausted work must not dispatch"))?;
            assert!(!state.join("gc/completions").join(&name).exists());
            assert_eq!(
                std::fs::read(state.join("gc/completion-exhausted").join(&name))?,
                bytes
            );
        }
        // Crash immediately after rename (before any later operation): there
        // is no retry sidecar to reconcile and identical intent gets a new budget.
        publish(&requests, request)?;
        let fresh = Completion::decode(&requests.read_bounded(name.as_ref(), 4096)?)?;
        assert_eq!(fresh.request, request);
        assert_eq!(fresh.retry.attempts, 0);
        assert_eq!(fresh.retry.next_at, 0);
        assert_eq!(fresh.retry.last_cause, None);
        assert_eq!(
            std::fs::read(state.join("gc/completion-exhausted").join(&name))?,
            bytes
        );
        Ok(())
    }

    #[test]
    fn exhaustion_crash_points_preserve_one_complete_record()
    -> Result<(), Box<dyn std::error::Error>> {
        for phase in [ExhaustionPhase::BeforeRename, ExhaustionPhase::AfterRename] {
            let temporary = tempfile::tempdir()?;
            let state = temporary.path();
            let requests =
                provenant_config::private_directory(&state.join("gc/completions"), true)?;
            let name = request_name("crash fixture");
            let record = Completion {
                request: "crash fixture".into(),
                retry: Retry {
                    attempts: 5,
                    next_at: 0,
                    last_cause: Some("collection busy".into()),
                },
            };
            record.save(&requests, &claimed_name(name.as_ref()))?;
            let bytes = requests.read_bounded(&claimed_name(name.as_ref()), 4096)?;
            EXHAUSTION_CRASH.set(Some(phase));
            let result = retry_pending_with(state, |_| panic!("exhausted work must not dispatch"));
            EXHAUSTION_CRASH.set(None);
            assert_eq!(result.unwrap_err().to_string(), "injected exhaustion crash");
            let active = state
                .join("gc/completions")
                .join(claimed_name(name.as_ref()));
            let diagnostic = state.join("gc/completion-exhausted").join(&name);
            assert_ne!(active.exists(), diagnostic.exists());
            assert_eq!(
                std::fs::read(if phase == ExhaustionPhase::BeforeRename {
                    &active
                } else {
                    &diagnostic
                })?,
                bytes
            );
            retry_pending_with(state, |_| panic!("exhausted work must not dispatch"))?;
            assert!(!active.exists());
            assert_eq!(std::fs::read(&diagnostic)?, bytes);
            publish(&requests, "crash fixture")?;
            assert_eq!(
                Completion::decode(&requests.read_bounded(name.as_ref(), 4096)?)?
                    .retry
                    .attempts,
                0
            );
        }
        Ok(())
    }

    #[test]
    fn requeue_during_exhausted_claim_keeps_fresh_intent_pending()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let state = temporary.path();
        let requests = provenant_config::private_directory(&state.join("gc/completions"), true)?;
        let name = request_name("requeue fixture");
        let exhausted = Completion {
            request: "requeue fixture".into(),
            retry: Retry {
                attempts: 5,
                next_at: 0,
                last_cause: Some("busy".into()),
            },
        };
        exhausted.save(&requests, &claimed_name(name.as_ref()))?;
        let lock = claim(state, name.as_ref())?.ok_or("claim")?;
        publish(&requests, "requeue fixture")?;
        let bytes = requests.read_bounded(name.as_ref(), 4096)?;
        assert_eq!(Completion::decode(&bytes)?.retry.attempts, 0);
        drop(lock);
        retry_pending_with(state, |_| panic!("exhausted claim must be archived first"))?;
        assert_eq!(requests.read_bounded(name.as_ref(), 4096)?, bytes);
        let mut calls = 0;
        retry_pending_with(state, |_| {
            calls += 1;
            Ok(())
        })?;
        assert_eq!(calls, 1);
        assert_eq!(
            Completion::decode(&requests.read_bounded(&claimed_name(name.as_ref()), 4096)?)?
                .retry
                .attempts,
            1
        );
        Ok(())
    }

    #[test]
    fn failed_exhaustion_rename_leaves_complete_request_pending()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let state = temporary.path();
        let requests = provenant_config::private_directory(&state.join("gc/completions"), true)?;
        let name = request_name("failed rename");
        let record = Completion {
            request: "failed rename".into(),
            retry: Retry {
                attempts: 5,
                next_at: 0,
                last_cause: Some("busy".into()),
            },
        };
        record.save(&requests, name.as_ref())?;
        let bytes = requests.read_bounded(name.as_ref(), 4096)?;
        // A directory at the diagnostic destination forces the rename to fail.
        std::fs::create_dir_all(state.join("gc/completion-exhausted").join(&name))?;
        assert!(retry_pending_with(state, |_| panic!("must not dispatch")).is_err());
        assert_eq!(
            requests.read_bounded(&claimed_name(name.as_ref()), 4096)?,
            bytes
        );
        std::fs::remove_dir(state.join("gc/completion-exhausted").join(&name))?;
        retry_pending_with(state, |_| panic!("must not dispatch"))?;
        assert_eq!(
            std::fs::read(state.join("gc/completion-exhausted").join(&name))?,
            bytes
        );
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Exercise OS lock release after a real child process dies.
    fn dead_claim_is_retried_with_durable_backoff() -> Result<(), Box<dyn std::error::Error>> {
        use std::io::BufRead;
        let temporary = tempfile::tempdir()?;
        let state = temporary.path();
        let requests = provenant_config::private_directory(&state.join("gc/completions"), true)?;
        publish(&requests, "dead claim")?;
        let name = request_name("dead claim");
        // Create the stable lock through the production primitive, then let
        // a separate process hold it and die without releasing it explicitly.
        let initial_claim = claim(state, name.as_ref())?.ok_or("initial claim")?;
        assert!(load_claimed(&requests, name.as_ref())?.is_some());
        drop(initial_claim);
        let path = state
            .join("gc/completion-locks")
            .join(format!("{name}.lock"));
        let mut child = std::process::Command::new("python3")
            .args(["-c", "import fcntl,sys,time; f=open(sys.argv[1], 'r+'); fcntl.flock(f, fcntl.LOCK_EX); print('claimed', flush=True); time.sleep(60)"])
            .arg(path).stdout(std::process::Stdio::piped()).spawn()?;
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().ok_or("child stdout")?)
            .read_line(&mut ready)?;
        assert_eq!(ready.trim(), "claimed");
        let mut calls = 0;
        retry_pending_with(state, |_| {
            calls += 1;
            Ok(())
        })?;
        assert_eq!(calls, 0, "a live claim excludes dispatch");
        child.kill()?;
        child.wait()?;
        retry_pending_with(state, |_| {
            calls += 1;
            Ok(())
        })?;
        assert_eq!(calls, 1, "a dead claim must not strand work");
        let bytes = requests.read_bounded(&claimed_name(name.as_ref()), 4096)?;
        let reserved = Completion::decode(&bytes)?;
        assert_eq!(reserved.retry.attempts, 1);
        assert!(reserved.retry.next_at > now_seconds()?);
        assert!(reserved.retry.last_cause.is_some());
        retry_pending_with(state, |_| panic!("backoff must prevent redispatch"))?;
        assert_eq!(
            requests.read_bounded(&claimed_name(name.as_ref()), 4096)?,
            bytes
        );
        // A child that dies after reservation is retried when the backoff expires.
        let mut due = reserved;
        due.retry.next_at = 0;
        due.save(&requests, &claimed_name(name.as_ref()))?;
        retry_pending_with(state, |_| {
            calls += 1;
            Ok(())
        })?;
        assert_eq!(calls, 2);
        assert_eq!(
            Completion::decode(&requests.read_bounded(&claimed_name(name.as_ref()), 4096)?)?
                .retry
                .attempts,
            2
        );
        Ok(())
    }
}
