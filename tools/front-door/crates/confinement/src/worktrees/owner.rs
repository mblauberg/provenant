//! Ownership is host state, never part of the provider's checkout.
//! GC consumers must still prove clean/merged state and acquire custody.
use super::*;
use provenant_core::{AttemptId, HostId, OperationId, WallTime};
use provenant_records::custody::{RetirementStep, RetirementStepState};

const OWNER: &str = "provenant-owner.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeOrigin {
    Lane,
    Adopted,
}

/// Stored in the linked worktree's private Git admin directory. Optional IDs
/// distinguish an adopted human checkout from a lane. Ownership alone never
/// grants deletion authority. `base` is pinned to a commit when created.
/// `reference` links the branch to its generation in the host ref ledger
/// (see `RefLedger`); an owner without it has an unobserved branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeOwner {
    pub attempt_id: Option<AttemptId>,
    pub operation_id: Option<OperationId>,
    pub branch: String,
    pub base: String,
    pub host: HostId,
    pub created_at: WallTime,
    pub origin: WorktreeOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<RefOwnership>,
}

pub fn load_owner(worktree: &VerifiedWorktree) -> Result<Option<WorktreeOwner>, WorktreeError> {
    worktree.repo().revalidate()?;
    let bytes = match read_bounded(
        worktree.repo().private_dir(),
        OsStr::new(OWNER),
        MAX_STATE_RECORD,
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let owner: WorktreeOwner =
        serde_json::from_slice(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    validate(&owner)?;
    Ok(Some(owner))
}

/// Denial inventory from a host-registered Git common directory only. Read
/// bounded ownership records and Git backlinks, without opening checkouts or
/// probing their processes. `custody_slots` comes from the supervisor's validated
/// reservation listing. Missing owned checkouts remain protected by name.
pub fn owned_worktree_paths(
    common: &Path,
    custody_slots: &[PathBuf],
) -> Result<Vec<PathBuf>, WorktreeError> {
    let directory = match Directory::open_anchor(common) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let registrations = match directory.open_verified_directory(OsStr::new("worktrees")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for (count, name) in registrations.entries()?.enumerate() {
        if count >= MAX_WORKTREES {
            return Err(WorktreeError::InvalidOutput);
        }
        let name = name?;
        if registrations.metadata_at(&name, false)?.kind == provenant_os::fs::FileKind::Regular {
            continue;
        }
        let registration = registrations.open_verified_directory(&name)?;
        let owner = match read_bounded(&registration, OsStr::new(OWNER), MAX_STATE_RECORD) {
            Ok(bytes) => Some(
                serde_json::from_slice::<WorktreeOwner>(&bytes)
                    .map_err(|_| WorktreeError::InvalidOutput)?,
            ),
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(owner) = &owner {
            validate(owner)?;
        }
        if owner.is_none() && custody_slots.is_empty() {
            continue;
        }
        let bytes = read_bounded(&registration, OsStr::new("gitdir"), 4096)?;
        let backlink =
            crate::repo::parse_git_path(&bytes, b"").ok_or(WorktreeError::InvalidOutput)?;
        let marker =
            crate::repo::resolve_git_path(&common.join("worktrees").join(name), &backlink)?;
        if marker.file_name() != Some(OsStr::new(".git")) {
            return Err(WorktreeError::InvalidOutput);
        }
        let path = marker.parent().ok_or(WorktreeError::InvalidOutput)?;
        let canonical = crate::permissions::resolve_boundary_path(path)
            .map_err(|_| WorktreeError::InvalidOutput)?;
        if owner.is_some() || custody_slots.contains(&PathBuf::from(state_dir_name(&canonical))) {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}

pub fn save_owner(worktree: &VerifiedWorktree, owner: &WorktreeOwner) -> Result<(), WorktreeError> {
    validate(owner)?;
    worktree.repo().revalidate()?;
    worktree
        .repo()
        .private_dir()
        .atomic_write(
            OsStr::new(OWNER),
            ".owner-",
            &serde_json::to_vec(owner).map_err(|_| WorktreeError::InvalidOutput)?,
            PublishMode::Replace,
        )?
        .require_durable()
        .map_err(|_| WorktreeError::RegistrationUnconfirmed)
}

/// Update ownership metadata only if the observed record is still current.
/// The per-worktree owner lock serialises this check with lane-owner claims; the
/// writer guard excludes updates while custody is held. Call before acquiring
/// writer custody; the guard does not bind this reservation to a later lease.
pub fn update_owner_when_unchanged(
    context: &WorktreeContext<'_>,
    worktree: &VerifiedWorktree,
    expected: &WorktreeOwner,
    updated: &WorktreeOwner,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    validate(updated)?;
    validate_binding(context)?;
    // Ownership is a per-worktree record. Serialise its CAS without holding
    // the repository mutation lock through inventory and cleanliness probes.
    let slot = writer_directory(context.project_state, worktree.path())?;
    let _owner = slot
        .lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            context.lock_budget,
            cancel,
            context.clock,
        )
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => WorktreeError::RegistryLockTimeout,
            other => other.into(),
        })?;
    let tree = verify_worktree(context.repo, worktree.path())?;
    if !tree.identity().same_identity(&worktree.identity()) {
        return Err(WorktreeError::IdentityChanged);
    }
    let current = load_owner(&tree)?;
    // An exact replay performs no mutation and remains valid under live custody.
    if current.as_ref() == Some(updated) {
        return Ok(());
    }
    if current.as_ref() != Some(expected) {
        return Err(WorktreeError::IdentityChanged);
    }
    let _writer =
        cache_writer_guard(context.project_state, tree.path()).map_err(|state| match state {
            Occupancy::Held => WorktreeError::WriterBusy {
                holder: expected.attempt_id,
            },
            other => custody_refusal(other),
        })?;
    // Custody also excludes direct owner publications outside owner.lock.
    if load_owner(&tree)?.as_ref() != Some(expected) {
        return Err(WorktreeError::IdentityChanged);
    }
    save_owner(&tree, updated)
}

/// Refresh a lane's generation-qualified ref link while the caller already
/// holds repository and writer custody. The caller checks pending retirement
/// records before advancing the RefLedger; this function only CAS-publishes the
/// verified owner snapshot and does not reacquire locks.
pub fn refresh_owner_reference_while_held(
    context: &WorktreeContext<'_>,
    worktree: &VerifiedWorktree,
    expected: &WorktreeOwner,
    reference: RefOwnership,
) -> Result<WorktreeOwner, WorktreeError> {
    validate_binding(context)?;
    if reference.repository != context.binding.project_id()
        || reference.endpoint != RefEndpoint::Local
        || reference.reference != format!("refs/heads/{}", expected.branch)
        || reference.generation == 0
    {
        return Err(WorktreeError::InvalidRequest);
    }
    let path = canonicalize(worktree.path())?;
    let tree = verify_worktree(context.repo, &path)?;
    if canonicalize(tree.path())? != path
        || !tree.identity().same_identity(&worktree.identity())
        || load_owner(&tree)?.as_ref() != Some(expected)
    {
        return Err(WorktreeError::IdentityChanged);
    }
    let updated = WorktreeOwner {
        reference: Some(reference),
        ..expected.clone()
    };
    validate(&updated)?;
    if load_owner(&tree)?.as_ref() != Some(expected) {
        return Err(WorktreeError::IdentityChanged);
    }
    save_owner(&tree, &updated)?;
    Ok(updated)
}

fn validate(owner: &WorktreeOwner) -> Result<(), WorktreeError> {
    GitRefName::branch(&owner.branch).map_err(|_| WorktreeError::InvalidRequest)?;
    GitOid::parse(&owner.base).map_err(|_| WorktreeError::InvalidRequest)?;
    if owner.label.as_ref().is_some_and(|label| {
        label.is_empty() || label.len() > 128 || label.chars().any(char::is_control)
    }) || owner.origin == WorktreeOrigin::Lane && owner.operation_id.is_none()
        || owner.reference.as_ref().is_some_and(|reference| {
            reference.endpoint != RefEndpoint::Local
                || reference.generation == 0
                || reference.reference != format!("refs/heads/{}", owner.branch)
        })
    {
        return Err(WorktreeError::InvalidRequest);
    }
    Ok(())
}

/// Publish lane ownership only while both owner updates and writer custody are
/// excluded. Reassignment requires the caller's owner snapshot to remain
/// current and the worktree HEAD must still be the one the caller observed
/// (and validated, for example for base ancestry). An unowned tree or a
/// verified terminal prior or a replay of the same operation may permit a live
/// or unknown census observation.
#[allow(clippy::too_many_arguments)]
pub fn save_lane_owner_when_free(
    context: &WorktreeContext<'_>,
    worktree: &VerifiedWorktree,
    owner: &WorktreeOwner,
    expected_head: GitOid,
    expected_prior: Option<&WorktreeOwner>,
    terminal_prior: bool,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    if owner.origin != WorktreeOrigin::Lane {
        return Err(WorktreeError::InvalidRequest);
    }
    validate(owner)?;
    validate_binding(context)?;
    // Ownership is a per-worktree record. Serialise its CAS without holding
    // the repository mutation lock through inventory and cleanliness probes.
    let slot = writer_directory(context.project_state, worktree.path())?;
    let _owner = slot
        .lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            context.lock_budget,
            cancel,
            context.clock,
        )
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => WorktreeError::RegistryLockTimeout,
            other => other.into(),
        })?;
    let tree = verify_worktree(context.repo, worktree.path())?;
    if !tree.identity().same_identity(&worktree.identity()) {
        return Err(WorktreeError::IdentityChanged);
    }
    let _writer =
        cache_writer_guard(context.project_state, tree.path()).map_err(custody_refusal)?;
    let branch = format!("refs/heads/{}", owner.branch);
    if !context
        .facts
        .fresh_inventory(context, cancel)?
        .iter()
        .any(|entry| {
            canonicalize(&entry.path).ok().as_deref() == Some(tree.path())
                && entry.branch.as_ref().map(GitRefName::as_str) == Some(branch.as_str())
                && entry.head == expected_head
                && !entry.locked
                && !entry.prunable
        })
    {
        return Err(WorktreeError::IdentityChanged);
    }
    let current_prior = load_owner(&tree)?;
    if current_prior.as_ref() != expected_prior {
        return Err(WorktreeError::IdentityChanged);
    }
    if let Some(prior) = current_prior.as_ref()
        && prior.origin == WorktreeOrigin::Lane
        && prior.operation_id != owner.operation_id
        && !terminal_prior
        && prior.attempt_id.is_some()
    {
        return Err(WorktreeError::WriterBusy {
            holder: prior.attempt_id,
        });
    }
    if current_prior
        .as_ref()
        .is_some_and(|prior| prior.origin != WorktreeOrigin::Lane)
        || terminal_prior
            && !current_prior
                .as_ref()
                .is_some_and(|prior| prior.attempt_id.is_some())
    {
        return Err(WorktreeError::UnknownWriter);
    }
    match worktree_cleanliness_at(context, tree.repo(), cancel)? {
        WorktreeCleanliness::Clean | WorktreeCleanliness::Ignored => {}
        WorktreeCleanliness::Dirty(paths) => return Err(WorktreeError::Dirty(paths)),
        WorktreeCleanliness::GitOperationInProgress => {
            return Err(WorktreeError::GitOperationInProgress);
        }
        WorktreeCleanliness::Unknown => return Err(WorktreeError::UnknownWriter),
    }
    check_writer_terminal(tree.repo())?;
    save_owner(&tree, owner)
}

/// The ledger generation to link from a new owner of `branch`, when the
/// ledger observed it and the branch still sits on the commit recorded there.
/// Any other state, including an unreadable ledger, leaves the branch
/// unobserved, which retains it.
pub fn branch_ownership(
    context: &WorktreeContext<'_>,
    branch: &str,
    head: GitOid,
) -> Result<Option<RefOwnership>, WorktreeError> {
    let branch = GitRefName::branch(branch).map_err(|_| WorktreeError::InvalidRequest)?;
    let Ok(ledger) = RefLedger::for_context(context) else {
        return Ok(None);
    };
    let Ok(Some(record)) = ledger.load(&RefEndpoint::Local, &branch) else {
        return Ok(None);
    };
    Ok((record.tip == head).then(|| record.ownership()))
}

/// Opt-in lane cleanup. The delivered/base OID is pinned by the host. The
/// normal merged-clean GC policy for other worktrees is unchanged. Only
/// declared regenerable output is removed, through GC's existing walker. The
/// callback receives the durable S2 intent after the verified owner/ref refresh
/// and before Git mutation so the caller can stage its settlement receipt.
pub fn prune_owned(
    context: &WorktreeContext<'_>,
    path: &Path,
    attempt: AttemptId,
    head: GitOid,
    upstream: GitOid,
    policy: &WorktreeGcPolicy<'_>,
    retirement: &dyn RetirementPort,
    decision: provenant_records::custody::RetirementDecision,
    before_remove: impl FnOnce(
        &provenant_records::custody::RetirementRecord,
    ) -> Result<(), WorktreeError>,
    cancel: &CancelToken,
) -> Result<(RemoveOutcome, provenant_records::custody::RetirementRecord), WorktreeError> {
    let verified = verify_worktree(context.repo, path)?;
    let owner = load_owner(&verified)?.ok_or(WorktreeError::UnknownWriter)?;
    let relative = path
        .strip_prefix(context.repo.root_path())
        .map_err(|_| WorktreeError::UnknownWriter)?;
    let managed = managed_writer_branch(context.repo.root_path(), relative)?.is_some();
    if owner.origin != WorktreeOrigin::Lane || owner.attempt_id != Some(attempt) || !managed {
        return Err(WorktreeError::UnknownWriter);
    }
    let lanes = list_for_gc_matching(context, cancel, |candidate, _| candidate == path)?
        .into_iter()
        .filter(|lane| lane.path == path && lane.head == head)
        .collect::<Vec<_>>();
    if lanes.len() != 1 {
        return Err(WorktreeError::IdentityChanged);
    }
    let canonical = canonicalize(path)?;
    let Some(worktree_path) = canonical.to_str() else {
        return Err(WorktreeError::InvalidRequest);
    };
    let worktree_subject =
        provenant_records::custody::Subject::new(SubjectKind::Worktree, worktree_path);
    let branch = GitRefName::branch(&owner.branch).map_err(|_| WorktreeError::InvalidRequest)?;
    let evidence = match decision.evidence() {
        provenant_records::custody::RemovalEvidence::Landing { evidence } => Some(evidence),
        provenant_records::custody::RemovalEvidence::IdleRetirement { tip } if *tip == head => None,
        _ => return Err(WorktreeError::InvalidRequest),
    };
    if !decision.valid_for(std::slice::from_ref(&worktree_subject), head)
        || owner.reference.as_ref().is_some_and(|reference| {
            reference.repository != context.binding.project_id()
                || reference.endpoint != RefEndpoint::Local
                || reference.reference != branch.as_str()
                || reference.origin != RefOrigin::Created
        })
    {
        return Err(WorktreeError::IdentityChanged);
    }
    let mut completion = decision.evidence().clone();
    let worktree_step = RetirementStep::Worktree {
        subject: worktree_subject.clone(),
    };
    if let Some(evidence) = evidence
        && !super::gc::verify_retirement_landing_evidence(
            context, head, upstream, evidence, cancel,
        )?
    {
        return Err(WorktreeError::InvalidRequest);
    }
    // This pass only cleans declared output; final removal is separately
    // fenced below, after rechecking ownership, ancestry and exact HEAD.
    let output_policy = WorktreeGcPolicy {
        upstream: None,
        mode: CollectionMode::Outputs,
        retirement_port: None,
        retirement_feeds: None,
        ..*policy
    };

    collect(context, &lanes, &output_policy, cancel)?;

    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let entries = context.facts.read_inventory(context, cancel)?;
    let entry = entries
        .iter()
        .find(|entry| canonicalize(&entry.path).ok().as_deref() == Some(path))
        .ok_or(WorktreeError::NotFound)?;
    let entry_branch = entry.branch.as_ref().ok_or(WorktreeError::InvalidRequest)?;
    if entry_branch != &branch || entry.head != head {
        return Err(WorktreeError::IdentityChanged);
    }

    // Landing-based cleanup needs a retained ref or open PR. An idle decision
    // keeps the local branch and requires only the clean writer fences.
    let references = if evidence.is_some() {
        let references = retained_refs(
            context,
            Some(branch),
            None,
            policy.integration_patterns,
            cancel,
        )?;
        if !matches!(
            removal_evidence(context, head, &references, cancel)?,
            LandingAssessment::Proven(_)
        ) {
            return Err(WorktreeError::IdentityChanged);
        }
        references
    } else {
        Vec::new()
    };
    let disposition = if decision.removes_reference() {
        Disposition::Merged
    } else {
        Disposition::Keep(RetainReason::NotMerged)
    };
    let settings = provenant_config::ProjectGcSettings {
        regenerable: policy.regenerable.to_vec(),
        keep: policy.keep.to_vec(),
        ..Default::default()
    };

    let mut predelete_ready = false;
    let mut retirement_record = None;
    let removed_result = remove_tree_when_before_delete(
        context,
        entry,
        head,
        cancel,
        &|| {
            if let Some(evidence) = evidence {
                validate_removal_evidence(context, head, evidence, &references, cancel)?;
            }
            ((policy.terminal)(path)
                && (policy.no_live_lanes)(path)
                && verify_worktree(context.repo, path)
                    .ok()
                    .and_then(|tree| load_owner(&tree).ok().flatten())
                    .as_ref()
                    == Some(&owner))
            .then_some(())
            .ok_or(WorktreeError::UnknownWriter)
        },
        Some(&settings),
        Repository::Held,
        |verified, canonical, _archive| {
            if canonical != path || load_owner(verified)?.as_ref() != Some(&owner) {
                return Err(WorktreeError::IdentityChanged);
            }
            if let Some(evidence) = evidence {
                validate_removal_evidence(context, head, evidence, &references, cancel)?;
            }
            if read_branch_tips(context, cancel)?.get(&branch) != Some(&head) {
                return Err(WorktreeError::IdentityChanged);
            }
            if let Some(evidence) = evidence
                && !super::gc::verify_retirement_landing_evidence(
                    context, head, upstream, evidence, cancel,
                )?
            {
                return Err(WorktreeError::IdentityChanged);
            }
            let lane = WorktreeInfo {
                warnings: Vec::new(),
                path: canonical.to_path_buf(),
                branch: Some(branch.as_str().to_owned()),
                head,
                dirty: WorktreeCondition::Clean,
                owner: Ok(Some(owner.clone())),
                primary: false,
                occupancy: Occupancy::Available,
            };
            let record = super::gc::begin_worktree_retirement(
                context,
                retirement,
                &lane,
                verified,
                decision.clone(),
                cancel,
            )?;
            before_remove(&record)?;
            retirement_record = Some(record);
            predelete_ready = true;
            Ok(())
        },
    );
    let removed = match removed_result {
        Ok(removed) => removed,
        Err(error) => {
            if let Some(record) = retirement_record.as_ref() {
                super::gc::update_retirement_step(
                    retirement,
                    record,
                    worktree_step.clone(),
                    if predelete_ready {
                        RetirementStepState::Unknown {
                            reason: error.to_string(),
                        }
                    } else {
                        RetirementStepState::Refused {
                            reason: error.to_string(),
                        }
                    },
                )?;
            }
            return Err(error);
        }
    };
    let mut record = retirement_record.ok_or(WorktreeError::RegistrationUnconfirmed)?;
    let settlement = (|| {
        let mut result = RemoveOutcome::default();
        result.removed.push(removed.info.clone());
        record = super::gc::finish_worktree_retirement(
            retirement,
            &record,
            &removed.info.path,
            completion.clone(),
        )?;
        if matches!(&disposition, Disposition::Merged) {
            record =
                super::gc::prepare_retirement_step(retirement, &record, RetirementStep::Reference)?;
        }
        let forge_reader = || {
            policy.retirement_feeds.map_or_else(
                || ForgeObservation::Unknown("forge feed unavailable".into()),
                |feeds| feeds.forge(&branch),
            )
        };
        let forge = evidence
            .filter(|evidence| {
                matches!(
                    &evidence.source,
                    provenant_records::custody::LandingSource::ForgeMerged {
                        merge_commit: None,
                        ..
                    }
                )
            })
            .map(|_| &forge_reader as &dyn Fn() -> ForgeObservation);
        let landing = if matches!(&disposition, Disposition::Merged) {
            evidence
        } else {
            None
        };
        let settled = settle_branch(
            context,
            &branch,
            head,
            disposition,
            landing,
            forge,
            cancel,
            &mut result,
        );
        if let Ok(Some(evidence)) = &settled {
            completion = provenant_records::custody::RemovalEvidence::Landing {
                evidence: evidence.clone(),
            };
        }
        if let Err(error) = settled {
            retain(&mut result, &branch, retain_reason(&error));
        }
        let reference_removed = result
            .pruned_refs
            .iter()
            .any(|name| name == branch.short_name());
        record = super::gc::finish_reference_retirement(
            retirement,
            &record,
            reference_removed,
            completion,
        )?;
        Ok((result, record))
    })();
    if let Some(cache) = removed.lane_cache.as_deref()
        && let Err(error) = crate::cache::prune_worktree_cargo_cache(context.home, cache)
    {
        warn_cache_removal(&error);
    }
    settlement
}
