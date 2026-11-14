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

/// Exact integration ref and pinned tip supplied by an applied GC sweep.
pub(crate) enum Collection {
    Settlement,
    Sweep {
        upstream: Option<(String, provenant_core::GitOid)>,
    },
}

#[derive(Default)]
pub(crate) struct UnsettledCollection {
    pub(crate) removed: Vec<PathBuf>,
    pub(crate) removals: Vec<provenant_records::custody::RemovalRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RemovalIntent {
    project: provenant_core::ProjectId,
    subject: provenant_records::custody::Subject,
    tip: provenant_core::GitOid,
    evidence: provenant_records::custody::LandedEvidence,
}

impl RemovalIntent {
    fn record(self) -> provenant_records::custody::RemovalRecord {
        provenant_records::custody::RemovalRecord {
            project: self.project,
            subject: self.subject,
            tip: self.tip,
            outcome: provenant_records::custody::RemovalOutcome::Removed {
                evidence: self.evidence,
            },
        }
    }
}

fn landed_evidence(
    reference: String,
    commit: provenant_core::GitOid,
) -> provenant_records::custody::LandedEvidence {
    provenant_records::custody::LandedEvidence {
        predicate: provenant_records::custody::LandingPredicate::Landed,
        source: provenant_records::custody::LandingSource::IntegrationTarget { reference, commit },
    }
}

fn retained_ref_evidence(
    reference: String,
    commit: provenant_core::GitOid,
) -> provenant_records::custody::LandedEvidence {
    provenant_records::custody::LandedEvidence {
        predicate: provenant_records::custody::LandingPredicate::Subsumed,
        source: provenant_records::custody::LandingSource::RetainedRef { reference, commit },
    }
}

fn valid_removal_evidence(evidence: &provenant_records::custody::LandedEvidence) -> bool {
    use provenant_records::custody::{LandingPredicate, LandingSource};
    matches!(
        (&evidence.predicate, &evidence.source),
        (
            LandingPredicate::Landed,
            LandingSource::IntegrationTarget { .. }
        ) | (
            LandingPredicate::Subsumed,
            LandingSource::RetainedRef { .. }
        )
    )
}

fn is_integration_evidence(evidence: &provenant_records::custody::LandedEvidence) -> bool {
    matches!(
        (&evidence.predicate, &evidence.source),
        (
            provenant_records::custody::LandingPredicate::Landed,
            provenant_records::custody::LandingSource::IntegrationTarget { .. }
        )
    )
}

fn delivered_removal_evidence(
    request: Option<&provenant_records::run::WriterDeliveryRequest>,
    receipt: Option<&provenant_run::WriterDeliveryReceipt>,
    reference: &str,
    head: provenant_core::GitOid,
    pinned: provenant_core::GitOid,
) -> provenant_records::custody::LandedEvidence {
    let integration_matches = request.zip(receipt).is_some_and(|(request, receipt)| {
        use provenant_records::run::{DeliveryMode, WriterDeliveryState};
        if !matches!(request.mode, DeliveryMode::Integrate(_)) {
            return false;
        }
        if !matches!(receipt.state, WriterDeliveryState::Delivered { .. })
            || receipt.head != Some(head)
            || receipt.remote != request.remote
            || receipt.branch != request.branch
        {
            return false;
        }
        let Ok(Some(destination)) =
            provenant_run::HostDeliveryContext::destination(request.mode.clone(), &request.branch)
        else {
            return false;
        };
        let remote = destination.remote;
        let branch = destination.branch;
        let target_ref = format!("refs/heads/{branch}");
        let expected_reference = if remote.is_empty() {
            target_ref.clone()
        } else {
            format!("refs/remotes/{remote}/{branch}")
        };
        request.remote == remote
            && request.branch == branch
            && reference == expected_reference
            && receipt.integration.as_ref().is_some_and(|integration| {
                integration.remote == remote && integration.target == target_ref
            })
    });
    if integration_matches {
        landed_evidence(reference.to_owned(), pinned)
    } else {
        retained_ref_evidence(reference.to_owned(), pinned)
    }
}

fn removal_intent(
    project: provenant_core::ProjectId,
    kind: provenant_records::custody::SubjectKind,
    subject: impl Into<String>,
    tip: provenant_core::GitOid,
    evidence: &provenant_records::custody::LandedEvidence,
) -> RemovalIntent {
    RemovalIntent {
        project,
        subject: provenant_records::custody::Subject::new(kind, subject),
        tip,
        evidence: evidence.clone(),
    }
}

fn removal_record(
    project: provenant_core::ProjectId,
    kind: provenant_records::custody::SubjectKind,
    subject: impl Into<String>,
    tip: provenant_core::GitOid,
    evidence: &provenant_records::custody::LandedEvidence,
) -> provenant_records::custody::RemovalRecord {
    removal_intent(project, kind, subject, tip, evidence).record()
}

fn kept_unproven_worktree(
    project: provenant_core::ProjectId,
    path: &Path,
    tip: provenant_core::GitOid,
) -> provenant_records::custody::RemovalRecord {
    provenant_records::custody::RemovalRecord {
        project,
        subject: provenant_records::custody::Subject::new(
            provenant_records::custody::SubjectKind::Worktree,
            path.to_string_lossy(),
        ),
        tip,
        outcome: provenant_records::custody::RemovalOutcome::Kept {
            cause: provenant_records::custody::RemovalUncertainty::UnprovenLanding,
        },
    }
}

fn removal_records_for_result(
    project: provenant_core::ProjectId,
    result: &provenant_confinement::worktrees::RemoveOutcome,
    evidence: &provenant_records::custody::LandedEvidence,
) -> Vec<provenant_records::custody::RemovalRecord> {
    use provenant_records::custody::{RemovalOutcome, Subject, SubjectKind};
    let mut records: Vec<_> = result
        .removed
        .iter()
        .map(|tree| {
            removal_record(
                project,
                SubjectKind::Worktree,
                tree.path.to_string_lossy(),
                tree.head,
                evidence,
            )
        })
        .collect();
    if let Some(tree) = result.removed.first() {
        for branch in &result.pruned_refs {
            records.push(removal_record(
                project,
                SubjectKind::Branch,
                format!("refs/heads/{branch}"),
                tree.head,
                evidence,
            ));
        }
    }
    for discarded in &result.discarded {
        records.push(removal_record(
            project,
            SubjectKind::Branch,
            format!("refs/heads/{}", discarded.branch),
            discarded.head,
            evidence,
        ));
    }
    records
}

fn recovered_removal_records(
    context: &provenant_confinement::worktrees::WorktreeContext<'_>,
    project: provenant_core::ProjectId,
    path: &Path,
    branch: Option<&str>,
    candidates: &[RemovalIntent],
    cancel: &CancelToken,
) -> Result<Vec<provenant_records::custody::RemovalRecord>, Box<dyn std::error::Error>> {
    use provenant_records::custody::SubjectKind;
    let path = path.to_string_lossy();
    let worktree_intent = candidates.iter().find(|candidate| {
        candidate.project == project
            && candidate.subject.kind == SubjectKind::Worktree
            && candidate.subject.id.as_str() == path.as_ref()
            && valid_removal_evidence(&candidate.evidence)
    });
    let worktree_tip = worktree_intent.map(|candidate| candidate.tip);
    let mut confirmed = Vec::new();
    for candidate in candidates {
        if candidate.project != project || !valid_removal_evidence(&candidate.evidence) {
            continue;
        }
        match candidate.subject.kind {
            SubjectKind::Worktree if candidate.subject.id.as_str() == path.as_ref() => {
                confirmed.push(candidate.clone().record());
            }
            SubjectKind::Branch
                if branch == Some(candidate.subject.id.as_str())
                    && worktree_tip == Some(candidate.tip)
                    && worktree_intent
                        .is_some_and(|intent| intent.evidence == candidate.evidence) =>
            {
                let reference = provenant_confinement::repo::GitRefName::new(&candidate.subject.id)
                    .map_err(|_| "pruning intent contains an invalid branch subject")?;
                if worktrees::ref_tip(context, &reference, cancel)?.is_none() {
                    confirmed.push(candidate.clone().record());
                }
            }
            _ => {}
        }
    }
    Ok(confirmed)
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
        Collection::Settlement,
        None,
    )
}

/// The sweep's half of completion cleanup. Settlement makes one detached
/// attempt and leaves a receipt that is not final until it succeeds. The
/// applied sweep records exact removal evidence in the same receipt before
/// mutation, then lifts confirmed removals into its report.
pub(crate) fn collect_unsettled(
    roots: &provenant_core::Roots,
    home: &Path,
    checkout: &Path,
    project_id: provenant_core::ProjectId,
    upstream: Option<(String, provenant_core::GitOid)>,
    workers: &dyn WorkerObserver,
) -> Result<UnsettledCollection, Box<dyn std::error::Error>> {
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
        return Ok(UnsettledCollection::default());
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
            let Some(writer) = store.request(attempt.attempt_id, &call)?.task.writer else {
                continue;
            };
            let receipt = store.worktree_settlement(attempt.attempt_id, &call)?;
            let exists = std::fs::symlink_metadata(&writer.worktree).is_ok();
            let recovering_prune = receipt
                .as_ref()
                .is_some_and(|receipt| receipt["state"] == "pruning");
            if !exists && !recovering_prune {
                continue;
            }
            let unsettled = receipt.is_some_and(|receipt| {
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
    let mut collection = UnsettledCollection::default();
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
            Collection::Sweep {
                upstream: upstream.clone(),
            },
            Some(&mut collection.removals),
        );
        if let Err(error) = collected {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: settled lane collection failed for {}: {error}",
                worktree.display()
            );
        }
        if std::fs::symlink_metadata(&worktree).is_err() {
            collection.removed.push(worktree);
        }
    }
    Ok(collection)
}

fn collect_owned_worktree_with(
    settled: &provenant_run::HolderContext,
    roots: &provenant_core::Roots,
    workers: &dyn WorkerObserver,
    delivery_override: Option<&provenant_run::WriterDeliveryReceipt>,
    collection: Collection,
    mut removal_records: Option<&mut Vec<provenant_records::custody::RemovalRecord>>,
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
    let task = store.request(record.attempt_id, &call)?.task;
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
    // Private output belongs to a settled lane, not to the lifetime of its
    // worktree. A continuation can recreate it; shared download links are
    // unlinked without traversing their targets. Busy/unknown is retried by GC.
    if let Err(error) = crate::worktrees::with_context_using_facts(
        roots,
        &settled.roots.home,
        &writer.project,
        std::time::Duration::ZERO,
        workers,
        store.repo_facts(),
        |context, _| {
            provenant_confinement::cache::gc_worktree_cargo_caches_with_writer_guard(
                &settled.roots.home,
                &provenant_confinement::cache::CacheGcPolicy {
                    idle_before_ns: Some(i128::MAX),
                    project_owners: [writer.worktree.clone()].into(),
                    selected_only: true,
                    ..Default::default()
                },
                |path| {
                    let guard = worktrees::cache_writer_guard(context.project_state, path)?;
                    let state = crate::gc_support::lane_states(
                        roots,
                        &settled.roots.home,
                        &settled.roots.checkout,
                        settled.project_id,
                    )
                    .map_err(|error| {
                        worktrees::Occupancy::custody_unknown(path.to_owned(), error.to_string())
                    })?;
                    if state.get(path) != Some(&true) || !state.no_live_lanes(path) {
                        return Err(worktrees::Occupancy::custody_unknown(
                            path.to_owned(),
                            "settlement custody is not free",
                        ));
                    }
                    Ok(Some(guard))
                },
            )
            .map_err(|error| crate::worktrees::Error::Permissions(*error.source))
        },
    ) {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: private cache release for {}: {error}",
            writer.worktree.display()
        );
    }
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
    let facts = store.repo_facts();
    // Refresh before item custody and before the prune budget starts.
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
    let sweep_upstream = match &collection {
        Collection::Sweep { upstream } => upstream.as_ref(),
        Collection::Settlement => None,
    };
    let mut replayed = replay_worktree_settlement(
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
    let removal_evidence =
        std::cell::RefCell::new(None::<provenant_records::custody::LandedEvidence>);
    let collection_error = std::cell::RefCell::new(None::<String>);
    let final_reason = std::cell::Cell::new(retention_reason);
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let config = provenant_cli::config_reads::read_for(roots, "gc")?;
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
                if let Some(replayed_receipt) = replayed.as_mut() {
                    if replayed_receipt["recovered_from_intent"] == true {
                        let evidence = replayed_receipt.get("removal_intents").cloned().ok_or(
                            "pruning receipt is missing required removal intent; recovery cannot report the removal",
                        )?;
                        let candidates: Vec<RemovalIntent> = serde_json::from_value(evidence)?;
                        if replayed_receipt["schema_version"] != 2 {
                            return Err(
                                "pruning receipt has an unsupported removal-evidence schema".into(),
                            );
                        }
                        let confirmed = recovered_removal_records(
                            context,
                            settled.project_id,
                            &writer.worktree,
                            replayed_receipt["branch"].as_str(),
                            &candidates,
                            cancel,
                        )?;
                        if !confirmed.iter().any(|record| {
                            record.subject.kind == provenant_records::custody::SubjectKind::Worktree
                        }) {
                            return Err(
                                "pruned worktree has no matching recorded removal evidence".into(),
                            );
                        }
                        if let Some(records) = removal_records.as_deref_mut() {
                            records.extend(confirmed.iter().cloned());
                        }
                        replayed_receipt["removal_records"] = serde_json::to_value(&confirmed)?;
                        if let Some(object) = replayed_receipt.as_object_mut() {
                            object.remove("removal_intents");
                        }
                    }
                    return Ok(None);
                }
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
                    Ok(())
                };
                if let Err(error) = pre_prune() {
                    *collection_error.borrow_mut() = Some(error.to_string());
                }
                if retention_reason.is_some() {
                    return Ok(None);
                }
                let delivered_ref = delivery
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
                let Some(head) = record.head.or_else(|| {
                    (record.provider_launched == Some(false))
                        .then(|| owner.base.parse().ok())
                        .flatten()
                }) else {
                    return Err(worktrees::WorktreeError::InvalidOutput.into());
                };
                let has_delivered_ref = delivered_ref.is_some();
                let (proof_reference, pinned) = match (delivered_ref, sweep_upstream) {
                    (Some(reference), _) => {
                        let pinned = worktrees::resolve_base(context, &reference, cancel)?;
                        (reference, pinned)
                    }
                    (None, Some((reference, fetched))) => (reference.clone(), *fetched),
                    (None, None) => {
                        if let Some(records) = removal_records.as_deref_mut() {
                            records.push(kept_unproven_worktree(
                                settled.project_id,
                                &writer.worktree,
                                head,
                            ));
                        }
                        return Err("no exact ref is available for removal evidence".into());
                    }
                };
                let evidence = if has_delivered_ref {
                    delivered_removal_evidence(
                        delivery_request,
                        delivery,
                        &proof_reference,
                        head,
                        pinned,
                    )
                } else {
                    landed_evidence(proof_reference.clone(), pinned)
                };
                *removal_evidence.borrow_mut() = Some(evidence.clone());
                let target = worktrees::LandingTarget {
                    reference: Some(
                        provenant_confinement::repo::GitRefName::new(&proof_reference)
                            .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                    ),
                    tip: pinned,
                };
                if delivery.is_none()
                    && delivery_request.is_none()
                    && !worktrees::landing_in_repo(context, head, &target, &[], cancel)?.is_landed()
                {
                    final_reason.set(Some("not landed on base and no delivery requested"));
                    return Ok(None);
                }
                let policy = WorktreeGcPolicy {
                    // Retained lane refs prove subsumption, but only an exact
                    // integration target permits deleting the local branch.
                    upstream: delivery_request
                        .filter(|request| {
                            matches!(
                                request.mode,
                                provenant_records::run::DeliveryMode::Integrate(_)
                            )
                        })
                        .filter(|_| is_integration_evidence(&evidence))
                        .map(|_| pinned),
                    ..output_policy
                };
                let mut intent = serde_json::json!({
                    "schema_version":2, "state":"pruning", "worktree":writer.worktree,
                    "branch":format!("refs/heads/{}", owner.branch)
                });
                let mut candidates = vec![removal_intent(
                    settled.project_id,
                    provenant_records::custody::SubjectKind::Worktree,
                    writer.worktree.to_string_lossy(),
                    head,
                    &evidence,
                )];
                let branch = provenant_confinement::repo::GitRefName::branch(&owner.branch)
                    .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
                if policy.upstream.is_some()
                    && worktrees::created_branch(context, &branch, owner.reference.as_ref())?
                        .is_some()
                    && worktrees::ref_tip(context, &branch, cancel)? == Some(head)
                {
                    candidates.push(removal_intent(
                        settled.project_id,
                        provenant_records::custody::SubjectKind::Branch,
                        branch.as_str(),
                        head,
                        &evidence,
                    ));
                }
                intent["removal_intents"] = serde_json::to_value(&candidates)?;
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
                let records = removal_evidence
                    .borrow()
                    .as_ref()
                    .map(|evidence| {
                        removal_records_for_result(settled.project_id, &result, evidence)
                    })
                    .ok_or("pruned worktree is missing removal evidence")?;
                if let Some(sink) = removal_records.as_deref_mut() {
                    sink.extend(records.iter().cloned());
                }
                let receipt = serde_json::json!({"schema_version":2, "state":"pruned", "worktree": writer.worktree, "result":result, "artefacts":artefacts, "removal_records":records});
                receipt
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
    if receipt["state"] == "pruned" && !receipt["removal_records"].is_array() {
        return Err("pruned GC receipt is missing required removal evidence".into());
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
                    let removals: Vec<_> = outcomes
                        .iter()
                        .filter_map(|outcome| outcome.removal.clone())
                        .collect();
                    if let Some(sink) = removal_records.as_deref_mut() {
                        sink.extend(removals.iter().cloned());
                    }
                    receipt["removal_records"]
                        .as_array_mut()
                        .ok_or("missing removal_records")?
                        .extend(serde_json::to_value(removals)?);
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
mod removal_tests {
    use super::*;

    #[test]
    fn applied_lane_prune_records_worktree_and_branch_with_exact_target_evidence() {
        let project = provenant_core::ProjectId::from_bytes([9; 16]);
        let tip = provenant_core::GitOid::parse(&"a1".repeat(20)).unwrap();
        let target = provenant_core::GitOid::parse(&"b2".repeat(20)).unwrap();
        let path = Path::new("/repo/.worktrees/topic");
        let evidence = landed_evidence("refs/remotes/origin/main".into(), target);
        let mut result = provenant_confinement::worktrees::RemoveOutcome::default();
        result
            .removed
            .push(provenant_confinement::worktrees::WorktreeInfo {
                warnings: Vec::new(),
                path: path.to_path_buf(),
                branch: Some("refs/heads/topic".into()),
                head: tip,
                dirty: provenant_confinement::worktrees::WorktreeCondition::Clean,
                occupancy: provenant_confinement::worktrees::Occupancy::Available,
                owner: Ok(None),
                primary: false,
            });
        result.pruned_refs.push("topic".into());

        let records = removal_records_for_result(project, &result, &evidence);
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| {
            record.project == project
                && record.tip == tip
                && matches!(
                    &record.outcome,
                    provenant_records::custody::RemovalOutcome::Removed { evidence: actual }
                        if actual == &evidence
                )
        }));
        assert!(records.iter().any(|record| {
            record.subject.kind == provenant_records::custody::SubjectKind::Worktree
                && record.subject.id.as_str() == path.to_string_lossy().as_ref()
        }));
        assert!(records.iter().any(|record| {
            record.subject.kind == provenant_records::custody::SubjectKind::Branch
                && record.subject.id == "refs/heads/topic"
        }));
        let encoded = serde_json::to_value(&records).unwrap();
        let decoded: Vec<provenant_records::custody::RemovalRecord> =
            serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, records);

        let intents = vec![
            removal_intent(
                project,
                provenant_records::custody::SubjectKind::Worktree,
                path.to_string_lossy(),
                tip,
                &evidence,
            ),
            removal_intent(
                project,
                provenant_records::custody::SubjectKind::Branch,
                "refs/heads/topic",
                tip,
                &evidence,
            ),
        ];
        let encoded = serde_json::to_value(&intents).unwrap();
        let decoded: Vec<RemovalIntent> = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, intents);
    }

    #[test]
    fn delivery_removal_evidence_distinguishes_retained_refs_from_integration_targets() {
        use provenant_records::run::{
            DeliveryMode, WriterDeliveryRequest, WriterDeliveryState, WriterIntegration,
        };
        let head = provenant_core::GitOid::parse(&"a1".repeat(20)).unwrap();
        let target = provenant_core::GitOid::parse(&"b2".repeat(20)).unwrap();
        let make_request = |mode, remote: &str, branch: &str, open_pr| WriterDeliveryRequest {
            extra: Default::default(),
            mode,
            remote: remote.into(),
            branch: branch.into(),
            open_pr,
            fast_forward_only: false,
        };
        let make_receipt =
            |remote: &str, branch: &str, integration| provenant_run::WriterDeliveryReceipt {
                extra: Default::default(),
                schema_version: 1,
                remote: remote.into(),
                branch: branch.into(),
                head: Some(head),
                review_acceptance: None,
                state: WriterDeliveryState::Delivered {
                    extra: Default::default(),
                    pull_request: None,
                },
                integration,
            };
        let lane_integration = Some(WriterIntegration {
            extra: Default::default(),
            remote: "origin".into(),
            push_url: Some("https://example.invalid/repo".into()),
            target: "refs/heads/topic".into(),
            previous: None,
            integrated: head,
        });
        for (mode, open_pr) in [(DeliveryMode::Push, false), (DeliveryMode::Pr, true)] {
            let request = make_request(mode, "origin", "topic", open_pr);
            let receipt = make_receipt("origin", "topic", lane_integration.clone());
            let evidence = delivered_removal_evidence(
                Some(&request),
                Some(&receipt),
                "refs/remotes/origin/topic",
                head,
                head,
            );
            assert_eq!(
                evidence,
                retained_ref_evidence("refs/remotes/origin/topic".into(), head)
            );
            assert!(valid_removal_evidence(&evidence));
            assert!(!is_integration_evidence(&evidence));
        }

        let request = make_request(
            DeliveryMode::Integrate("origin/main".into()),
            "origin",
            "main",
            false,
        );
        let receipt = make_receipt(
            "origin",
            "main",
            Some(WriterIntegration {
                extra: Default::default(),
                remote: "origin".into(),
                push_url: Some("https://example.invalid/repo".into()),
                target: "refs/heads/main".into(),
                previous: Some(target),
                integrated: target,
            }),
        );
        let evidence = delivered_removal_evidence(
            Some(&request),
            Some(&receipt),
            "refs/remotes/origin/main",
            head,
            target,
        );
        assert_eq!(
            evidence,
            landed_evidence("refs/remotes/origin/main".into(), target)
        );
        assert!(valid_removal_evidence(&evidence));
        assert!(is_integration_evidence(&evidence));

        let mismatched = make_request(
            DeliveryMode::Integrate("origin/other".into()),
            "origin",
            "main",
            false,
        );
        let evidence = delivered_removal_evidence(
            Some(&mismatched),
            Some(&receipt),
            "refs/remotes/origin/main",
            head,
            target,
        );
        assert_eq!(
            evidence,
            retained_ref_evidence("refs/remotes/origin/main".into(), target)
        );
        assert!(valid_removal_evidence(&evidence));
        assert!(!is_integration_evidence(&evidence));

        let invalid_pair = provenant_records::custody::LandedEvidence {
            predicate: provenant_records::custody::LandingPredicate::Landed,
            source: provenant_records::custody::LandingSource::RetainedRef {
                reference: "refs/remotes/origin/topic".into(),
                commit: head,
            },
        };
        assert!(!valid_removal_evidence(&invalid_pair));
    }
}
