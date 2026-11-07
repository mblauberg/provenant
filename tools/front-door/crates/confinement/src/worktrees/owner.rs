//! Ownership is host state, never part of the provider's checkout.
//! GC consumers must still prove clean/merged state and acquire custody.
use super::*;
use provenant_core::{AttemptId, HostId, OperationId, WallTime};

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
/// The repository lock serialises this check with lane-owner claims, and the
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
    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let tree = verify_worktree(context.repo, worktree.path())?;
    if !tree.identity().same_identity(&worktree.identity()) {
        return Err(WorktreeError::IdentityChanged);
    }
    let current = load_owner(&tree)?;
    if current.as_ref() == Some(updated) {
        return Ok(());
    }
    if current.as_ref() != Some(expected) {
        return Err(WorktreeError::IdentityChanged);
    }
    let _writer =
        cache_writer_guard(context.project_state, tree.path()).map_err(|state| match state {
            Occupancy::Held => WorktreeError::WriterBusy {
                holder: current.as_ref().and_then(|owner| owner.attempt_id),
            },
            other => custody_refusal(other),
        })?;
    save_owner(&tree, updated)
}

fn validate(owner: &WorktreeOwner) -> Result<(), WorktreeError> {
    GitRefName::branch(&owner.branch).map_err(|_| WorktreeError::InvalidRequest)?;
    GitOid::parse(&owner.base).map_err(|_| WorktreeError::InvalidRequest)?;
    if owner.label.as_ref().is_some_and(|label| {
        label.is_empty() || label.len() > 128 || label.chars().any(char::is_control)
    }) || owner.origin == WorktreeOrigin::Lane && owner.operation_id.is_none()
    {
        return Err(WorktreeError::InvalidRequest);
    }
    Ok(())
}

/// Publish lane ownership only while both repository and writer custody are
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
    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let tree = verify_worktree(context.repo, worktree.path())?;
    if !tree.identity().same_identity(&worktree.identity()) {
        return Err(WorktreeError::IdentityChanged);
    }
    let branch = format!("refs/heads/{}", owner.branch);
    if !context
        .facts
        .read_inventory(context, cancel)?
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
    let _writer =
        cache_writer_guard(context.project_state, tree.path()).map_err(custody_refusal)?;
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

/// Opt-in lane cleanup. The delivered/base OID is pinned by the host. The
/// normal merged-clean GC policy for other worktrees is unchanged. Only
/// declared regenerable output is removed, through GC's existing walker.
pub fn prune_owned(
    context: &WorktreeContext<'_>,
    path: &Path,
    attempt: AttemptId,
    head: GitOid,
    upstream: GitOid,
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
) -> Result<RemoveOutcome, WorktreeError> {
    let verified = verify_worktree(context.repo, path)?;
    let owner = load_owner(&verified)?.ok_or(WorktreeError::UnknownWriter)?;
    if owner.origin != WorktreeOrigin::Lane
        || owner.attempt_id != Some(attempt)
        || !path.starts_with(context.repo.root_path().join(WORKTREE_ROOT))
    {
        return Err(WorktreeError::UnknownWriter);
    }
    if !is_ancestor(context, head, upstream, cancel)? {
        return Err(WorktreeError::InvalidRequest);
    }
    let lanes = list_for_gc_matching(context, cancel, |candidate, _| candidate == path)?
        .into_iter()
        .filter(|lane| lane.path == path && lane.head == head)
        .collect::<Vec<_>>();
    if lanes.len() != 1 {
        return Err(WorktreeError::IdentityChanged);
    }
    // This pass only cleans declared output; final removal is separately
    // fenced below, after rechecking ownership, ancestry and exact HEAD.
    let output_policy = WorktreeGcPolicy {
        upstream: None,
        ..*policy
    };
    collect_lazy(context, &lanes, &output_policy, cancel)?;
    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let entries = context.facts.read_inventory(context, cancel)?;
    let entry = entries
        .iter()
        .find(|entry| canonicalize(&entry.path).ok().as_deref() == Some(path))
        .ok_or(WorktreeError::NotFound)?;
    let branch = entry.branch.as_ref().ok_or(WorktreeError::InvalidRequest)?;
    if branch.as_str() != format!("refs/heads/{}", owner.branch)
        || entry.head != head
        || !is_ancestor(context, head, upstream, cancel)?
    {
        return Err(WorktreeError::IdentityChanged);
    }
    // A pushed lane ref or an open PR is not an integration target. Its
    // worktree can be pruned while the local ref remains for landing and GC.
    let disposition = match policy.upstream {
        Some(integration) if is_ancestor(context, head, integration, cancel)? => {
            Disposition::Merged
        }
        _ => Disposition::Keep(RetainReason::NotMerged),
    };
    let removed = remove_tree_when(
        context,
        entry,
        head,
        cancel,
        &|| {
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
        Some(&provenant_config::ProjectGcSettings {
            regenerable: policy.regenerable.to_vec(),
            keep: policy.keep.to_vec(),
            ..Default::default()
        }),
        Repository::Held,
    )?;
    let mut result = RemoveOutcome::default();
    result.removed.push(removed.info.clone());
    if let Err(error) = settle_branch(context, branch, head, disposition, cancel, &mut result) {
        retain(&mut result, branch, retain_reason(&error));
    }
    if let Some(cache) = removed.lane_cache.as_deref()
        && let Err(error) = crate::cache::prune_worktree_cargo_cache(context.home, cache)
    {
        warn_cache_removal(&error);
    }
    Ok(result)
}
