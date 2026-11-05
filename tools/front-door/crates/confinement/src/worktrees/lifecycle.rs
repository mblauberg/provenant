//! Worktree creation, listing, removal and ref pruning.
use super::*;

/// Advance a free, registered linked worktree to a pinned descendant commit.
/// Writer exclusion covers verification and mutation. Local edits, divergence
/// and ignored files that would be overwritten always refuse the handoff. A
/// checkout already ahead of the pin is left as it is.
pub fn fast_forward(
    context: &WorktreeContext<'_>,
    path: &Path,
    head: GitOid,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    validate_binding(context)?;
    let _registry = repository_lock(context, LockMode::Exclusive, cancel)?;
    context.repo.revalidate()?;
    let path = canonicalize(path)?;
    if path == canonicalize(context.repo.root_path())? {
        return Err(WorktreeError::Primary);
    }
    let entries = context.facts.read_inventory(context, cancel)?;
    let entry = entries
        .iter()
        .find(|entry| canonicalize(&entry.path).ok().as_ref() == Some(&path))
        .ok_or(WorktreeError::NotFound)?;
    let branch = entry.branch.as_ref().ok_or(WorktreeError::InvalidRequest)?;
    let locked = lock_worktree_for_mutation(context, entry, cancel)?;
    let fresh = context.facts.read_inventory(context, cancel)?;
    let current = matching_worktree(&fresh, branch, context.repo.root_path())?
        .ok_or(WorktreeError::IdentityChanged)?;
    if canonicalize(&current.path)? != path
        || current.head != entry.head
        || current.locked
        || current.prunable
    {
        return Err(WorktreeError::IdentityChanged);
    }
    let repo = locked.verified.repo();
    match worktree_cleanliness_at(context, repo, cancel)? {
        WorktreeCleanliness::Clean | WorktreeCleanliness::Ignored => {}
        WorktreeCleanliness::Dirty(paths) => return Err(WorktreeError::Dirty(paths)),
        WorktreeCleanliness::GitOperationInProgress => {
            return Err(WorktreeError::GitOperationInProgress);
        }
        WorktreeCleanliness::Unknown => return Err(WorktreeError::UnknownWriter),
    }
    // A retry or fallback writer resumes in a checkout that already holds work
    // committed past the pin; that descendant of the pin is not a divergence.
    if current.head != head && is_ancestor(context, head, current.head, cancel)? {
        return Ok(());
    }
    if !is_ancestor(context, current.head, head, cancel)? {
        return Err(WorktreeError::SourceDiverged {
            current: current.head,
            target: head,
        });
    }
    repo.revalidate()?;
    let actual = context.facts.checkout_head(context, repo, cancel);
    match actual {
        Ok(actual) if actual == current.head => {}
        Ok(_) | Err(WorktreeError::NotFound) => return Err(WorktreeError::IdentityChanged),
        Err(error) => return Err(error),
    }
    if current.head != head {
        context.workers.invalidate();
        let outcome = execute_git_mutation(
            context.executor,
            repo,
            context.utility,
            GitMutation::FastForward { head },
            context.mutation_budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::SourceAdvanceFailed { target: head });
        }
    }
    repo.revalidate()?;
    check_writer_terminal(repo)?;
    let actual = context.facts.checkout_head(context, repo, cancel);
    match actual {
        Ok(actual) if actual == head => {}
        Ok(_) | Err(WorktreeError::NotFound) => return Err(WorktreeError::IdentityChanged),
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Bounded, unpaged repository-scoped worktree inventory.
pub fn list(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    Ok(list_with_observation(context, cancel)?
        .into_iter()
        .map(|observation| observation.info)
        .collect())
}

/// The native census and registration/custody provenance for one inventory row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeObservation {
    pub info: WorktreeInfo,
    pub occupancy: Occupancy,
}

/// Like [`list`], preserving the native observation and whether registration
/// or on-disk custody was itself unresolved. Admission callers use these
/// typed facts to distinguish incomplete census from unresolved custody.
pub fn list_with_observation(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Vec<WorktreeObservation>, WorktreeError> {
    list_streaming(context, cancel, |_, _| {})
}

/// Report the inventory size before the census and each completed row after
/// its normal checks. A caller can retain these advisory rows if its deadline
/// expires; the complete inventory still follows the ordinary safety path.
pub fn list_streaming(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
    report: impl FnMut(usize, Option<&WorktreeInfo>),
) -> Result<Vec<WorktreeObservation>, WorktreeError> {
    list_impl(context, cancel, report, true, context.facts)
}

/// Inventory for GC. Cleanliness is checked only for candidates after the
/// policy and custody checks, rather than running Git in every checkout.
pub fn list_for_gc(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    Ok(list_impl(context, cancel, |_, _| {}, false, context.facts)?
        .into_iter()
        .map(|observation| observation.info)
        .collect())
}

fn list_impl(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
    mut report: impl FnMut(usize, Option<&WorktreeInfo>),
    eager_git: bool,
    facts: &RepoFacts,
) -> Result<Vec<WorktreeObservation>, WorktreeError> {
    validate_binding(context)?;
    let entries = {
        let _lock = repository_lock(context, LockMode::Exclusive, cancel)?;
        context.repo.revalidate()?;
        facts.capture_inventory(context, cancel)?
    };
    // A stale Git registration must not hide the remaining worktrees from GC.
    let candidates: Vec<_> = entries
        .iter()
        .filter(|entry| !entry.bare)
        .filter_map(|entry| canonicalize(&entry.path).ok().map(|path| (path, entry)))
        .collect();
    let total = candidates.len();
    report(total, None);
    let roots: Vec<_> = candidates.iter().map(|(path, _)| path.clone()).collect();
    let worker_evidence = context.workers.observe_evidence(&roots, false);
    let primary = canonicalize(context.repo.root_path())?;
    let mut result = Vec::with_capacity(candidates.len());
    for (path, entry) in candidates {
        let branch_text = entry
            .branch
            .as_ref()
            .map(|branch| branch.as_str().to_owned());
        let observed = worker_evidence
            .states
            .get(&path)
            .copied()
            .unwrap_or(WorkerState::Unknown);
        let mut owner = OwnerStatus::Unknown;
        let mut owner_evidence = None;
        let mut registration_evidence = None;
        let (dirty, writer, disk, registration_unknown) = if path == primary {
            owner = OwnerStatus::Primary;
            (
                if eager_git {
                    worktree_dirty_at(context, context.repo, cancel)
                        .unwrap_or(WorktreeCondition::Unknown)
                } else {
                    WorktreeCondition::Unknown
                },
                combine_worker_state(WorkerState::Unknown, observed),
                WorkerState::Unknown,
                true,
            )
        } else {
            // GC only reads registration and custody here. Its candidate paths
            // probe their own Git configuration before running Git or
            // mutating anything; probing every unrelated tree is costly.
            let verified = if eager_git {
                verify_probed_worktree(context, &path, cancel)
            } else {
                verify_worktree(context.repo, &path).map_err(WorktreeError::from)
            };
            match verified {
                Ok(worktree) => {
                    owner = match load_owner(&worktree) {
                        Ok(Some(_)) => OwnerStatus::Registered,
                        Ok(None) => OwnerStatus::Unowned,
                        Err(error) => {
                            owner_evidence =
                                Some(format!("owner record could not be read: {error}"));
                            OwnerStatus::Unknown
                        }
                    };
                    let disk = if entry.locked || entry.prunable {
                        WorkerState::Unknown
                    } else {
                        disk_writer_state(context.project_state, &path)
                    };
                    (
                        if eager_git {
                            worktree_dirty_at(context, worktree.repo(), cancel)
                                .unwrap_or(WorktreeCondition::Unknown)
                        } else {
                            WorktreeCondition::Unknown
                        },
                        if entry.locked || entry.prunable {
                            WriterState::Unknown
                        } else {
                            combine_worker_state(disk, observed)
                        },
                        disk,
                        entry.locked || entry.prunable || disk == WorkerState::Unknown,
                    )
                }
                Err(error) => {
                    registration_evidence = Some(format!("worktree verification failed: {error}"));
                    (
                        WorktreeCondition::Unknown,
                        WriterState::Unknown,
                        WorkerState::Unknown,
                        true,
                    )
                }
            }
        };
        // Until all consumers use Occupancy, legacy inventory consumers must
        // retain an unreadable owner rather than treating the tree as free.
        let writer = if owner_evidence.is_some() && disk != WorkerState::Busy {
            WriterState::Unknown
        } else {
            writer
        };
        let occupancy = if disk == WorkerState::Busy {
            Occupancy::Held
        } else if registration_unknown || owner_evidence.is_some() {
            let evidence = if entry.locked {
                "Git marks the worktree locked".to_owned()
            } else if entry.prunable {
                "Git marks the worktree prunable".to_owned()
            } else {
                owner_evidence.or(registration_evidence).unwrap_or_else(|| {
                    "worktree registration or writer custody could not be verified".to_owned()
                })
            };
            Occupancy::Unknown {
                scope: path.clone(),
                evidence,
                source: OccupancyUnknownSource::Custody,
            }
        } else {
            match observed {
                WorkerState::Free => Occupancy::Available,
                WorkerState::Busy => Occupancy::Unknown {
                    scope: path.clone(),
                    evidence: "census reported contention without a held writer lease".into(),
                    source: OccupancyUnknownSource::Census,
                },
                WorkerState::Live => Occupancy::Observed {
                    process: worker_evidence
                        .holders
                        .get(&path)
                        .cloned()
                        .or_else(|| context.workers.live_process(&path)),
                },
                WorkerState::Unknown => Occupancy::Unknown {
                    scope: path.clone(),
                    evidence: worker_evidence
                        .details
                        .get(&path)
                        .cloned()
                        .unwrap_or_else(|| context.workers.incomplete_census_detail()),
                    source: OccupancyUnknownSource::Census,
                },
            }
        };
        let observation = WorktreeObservation {
            info: WorktreeInfo {
                path,
                branch: branch_text,
                head: entry.head,
                dirty,
                writer,
                owner,
            },
            occupancy,
        };
        report(total, Some(&observation.info));
        result.push(observation);
    }
    Ok(result)
}

/// Create a clean linked checkout at a pinned commit. A failed or killed
/// `worktree add` is rolled back (see `roll_back_failed_add`). Failed
/// verification after Git succeeds leaves a visible tree for inspection.
pub fn create(
    context: &WorktreeContext<'_>,
    request: WorktreeCreate,
    cancel: &CancelToken,
) -> Result<WorktreeInfo, WorktreeError> {
    validate_binding(context)?;
    let branch = GitRefName::branch(&request.branch).map_err(|_| WorktreeError::InvalidRequest)?;
    let facts = context.facts;
    let _lock = repository_lock(context, LockMode::Exclusive, cancel)?;
    context.repo.revalidate()?;
    let entries = context.facts.read_inventory(context, cancel)?;
    let checked_out = entries
        .iter()
        .filter(|entry| !entry.prunable)
        .find(|entry| entry.branch.as_ref() == Some(&branch));
    if let Some(entry) = checked_out {
        return Err(if let Some(base) = request.base.as_deref() {
            let base_oid = pin_revision(context, facts, base, cancel)?;
            if !is_ancestor(context, base_oid, entry.head, cancel)? {
                WorktreeError::BranchExistsWithBase {
                    branch: request.branch.clone(),
                    head: entry.head,
                    base: base_oid,
                }
            } else {
                WorktreeError::CheckedOut
            }
        } else {
            WorktreeError::CheckedOut
        });
    }
    if entries
        .iter()
        .any(|entry| entry.prunable && entry.branch.as_ref() == Some(&branch))
    {
        return Err(WorktreeError::StaleRegistration);
    }
    let branch_tips = read_branch_tips(context, cancel)?;
    let existing = branch_tips.get(&branch).copied();
    if let Some(existing_head) = existing
        && let Some(base) = request.base.as_deref()
    {
        let base_oid = pin_revision(context, facts, base, cancel)?;
        if !is_ancestor(context, base_oid, existing_head, cancel)? {
            return Err(WorktreeError::BranchExistsWithBase {
                branch: request.branch.clone(),
                head: existing_head,
                base: base_oid,
            });
        }
    }
    let create_branch = existing.is_none();
    if create_branch {
        let remote = run_git(context, context.repo, GitRead::RemoteBranchTips, cancel)?;
        if exit_code(&remote)? != 0 {
            return Err(WorktreeError::InvalidOutput);
        }
        let remote_tips = parse_branch_tips(&remote.stdout.bytes)?;
        for name in branch_tips.keys().chain(remote_tips.keys()) {
            let text = name.as_str();
            let local_collision = text
                .strip_prefix("refs/heads/")
                .is_some_and(|candidate| provenant_core::refs_collide(&request.branch, candidate));
            // Remote names can themselves contain slashes. Consider every
            // possible remote/branch boundary to avoid missing an alias.
            let remote_collision = text.strip_prefix("refs/remotes/").is_some_and(|rest| {
                rest.match_indices('/').any(|(index, _)| {
                    provenant_core::refs_collide(&request.branch, &rest[index + 1..])
                })
            });
            if local_collision || remote_collision {
                return Err(WorktreeError::BranchCollision {
                    existing: text.to_owned(),
                });
            }
        }
    }
    let base = if let Some(existing) = existing {
        existing
    } else if let Some(base) = request.base.as_deref() {
        pin_revision(context, facts, base, cancel)?
    } else {
        let head = GitRevision::new("HEAD").map_err(|_| WorktreeError::InvalidRequest)?;
        facts
            .resolve_revision(context, head, cancel)
            .map_err(|error| {
                if matches!(error, WorktreeError::NotFound) {
                    WorktreeError::InvalidOutput
                } else {
                    error
                }
            })?
    };
    let worktree_root = match context
        .repo
        .root_dir()
        .open_verified_directory(OsStr::new(WORKTREE_ROOT))
    {
        Ok(root) => root,
        Err(error) if error.is_not_found() => {
            context
                .repo
                .root_dir()
                .mkdir(OsStr::new(WORKTREE_ROOT), 0o700)?;
            context.repo.root_dir().sync()?;
            context
                .repo
                .root_dir()
                .open_verified_directory(OsStr::new(WORKTREE_ROOT))?
        }
        Err(error) => return Err(error.into()),
    };
    let root_identity = worktree_root.metadata()?;
    if root_identity.uid != provenant_os::fs::effective_uid()
        || root_identity.mode & 0o022 != 0
        || root_identity.device != context.repo.root_identity().device
    {
        return Err(WorktreeError::UnknownWriter);
    }
    let path_name = request.branch.replace('/', "-");
    if path_name.is_empty() || path_name.len() > 240 || path_name.as_bytes().contains(&0) {
        return Err(WorktreeError::InvalidRequest);
    }
    let target = canonicalize(context.repo.root_path())?
        .join(WORKTREE_ROOT)
        .join(&path_name);
    match worktree_root.metadata_at(OsStr::new(&path_name), false) {
        Ok(_) => return Err(WorktreeError::InvalidRequest),
        Err(error) if error.is_not_found() => {}
        Err(_) => return Err(WorktreeError::UnknownWriter),
    }
    for (path, _) in canonical_worktree_paths(&entries)? {
        if path == target {
            return Err(WorktreeError::UnknownWriter);
        }
    }
    let _writer =
        cache_writer_guard(context.project_state, &target).map_err(|state| match state {
            WorkerState::Busy => WorktreeError::LockBusy,
            _ => WorktreeError::UnknownWriter,
        })?;
    let named_root = context
        .repo
        .root_dir()
        .metadata_at(OsStr::new(WORKTREE_ROOT), false)?;
    if !root_identity.same_identity(&named_root) {
        return Err(WorktreeError::UnknownWriter);
    }
    let primary_head = primary_head(context, facts, cancel)?;
    if let Err(cause) = execute_mutation(
        context,
        GitMutation::WorktreeAdd {
            branch: branch.clone(),
            base: create_branch.then_some(base),
            path: target.clone(),
            create_branch,
        },
        cancel,
    ) {
        let rolled_back = roll_back_failed_add(
            context,
            &target,
            create_branch.then_some((&branch, base)),
            cancel,
        );
        return Err(WorktreeError::CreateFailed {
            cause: Box::new(cause),
            rolled_back,
        });
    }
    // Creating a linked worktree must leave the primary's HEAD, branch or
    // detached, exactly as it was (#1331). A hook or race that moved it is
    // reported rather than hidden; the worktree stays for the operator.
    if primary_head != self::primary_head(context, facts, cancel)? {
        return Err(WorktreeError::PrimaryMoved);
    }
    context.repo.revalidate()?;
    let fresh = context.facts.read_inventory(context, cancel)?;
    let entry = fresh
        .iter()
        .find(|entry| canonicalize(&entry.path).ok().as_deref() == Some(target.as_path()))
        .ok_or(WorktreeError::RegistrationUnconfirmed)?;
    if entry.branch.as_ref() != Some(&branch)
        || entry.head != base
        || entry.locked
        || entry.prunable
    {
        return Err(WorktreeError::RegistrationUnconfirmed);
    }
    let verified = verify_probed_worktree(context, &entry.path, cancel)?;
    if worktree_dirty_at(context, verified.repo(), cancel)? != WorktreeCondition::Clean {
        return Err(WorktreeError::RegistrationUnconfirmed);
    }
    let worker = context.workers.observe_roots(std::slice::from_ref(&target));
    let writer = worker.get(&target).copied().unwrap_or(WorkerState::Unknown);
    Ok(WorktreeInfo {
        path: target,
        branch: Some(branch.as_str().to_owned()),
        head: entry.head,
        dirty: WorktreeCondition::Clean,
        owner: OwnerStatus::Unowned,
        writer: match writer {
            WorkerState::Free => WriterState::Free,
            WorkerState::Live => WriterState::Live,
            WorkerState::Busy => WriterState::Busy,
            WorkerState::Unknown => WriterState::Unknown,
        },
    })
}

/// Undo a `worktree add` that failed or was killed part-way. Only what this
/// call made is touched: `target` was verified absent before the add, its
/// admin entry is found by the `gitdir` file that points at it, and the branch
/// goes only when this call created it, it still points at `base` and no
/// worktree has it checked out. Anything that cannot be confirmed is left and
/// reported as not rolled back. Returns whether nothing of ours remains.
pub(super) fn roll_back_failed_add(
    context: &WorktreeContext<'_>,
    target: &Path,
    created_branch: Option<(&GitRefName, GitOid)>,
    cancel: &CancelToken,
) -> bool {
    let mut clean = remove_admin_entry(context.repo.common_git_dir(), target);
    clean &= match std::fs::remove_dir_all(target) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    let Some((branch, base)) = created_branch else {
        return clean;
    };
    let Ok(entries) = context.facts.read_inventory(context, cancel) else {
        return false;
    };
    if entries
        .iter()
        .any(|entry| entry.branch.as_ref() == Some(branch))
    {
        return false;
    }
    match read_branch_tips(context, cancel) {
        Ok(tips) => match tips.get(branch) {
            None => clean,
            Some(tip) if *tip == base => {
                clean
                    && execute_mutation(
                        context,
                        GitMutation::DeleteRef {
                            name: branch.clone(),
                            expected_old_oid: base,
                        },
                        cancel,
                    )
                    .is_ok()
            }
            Some(_) => false,
        },
        Err(_) => false,
    }
}

/// Remove the `<common git dir>/worktrees/<name>` entry whose `gitdir` names
/// `target`. Returns false when a matching entry could not be removed.
pub(super) fn remove_admin_entry(common_git_dir: &Path, target: &Path) -> bool {
    let wanted = target.join(".git");
    let Ok(entries) = std::fs::read_dir(common_git_dir.join("worktrees")) else {
        return true;
    };
    let Ok(entries) = entries
        .take(MAX_WORKTREES + 1)
        .collect::<std::io::Result<Vec<_>>>()
    else {
        return false;
    };
    if entries.len() > MAX_WORKTREES {
        return false;
    }
    let mut removed = true;
    for entry in entries {
        let Ok(text) = std::fs::read(entry.path().join("gitdir")) else {
            continue;
        };
        if text.trim_ascii_end() == wanted.as_os_str().as_encoded_bytes() {
            removed &= std::fs::remove_dir_all(entry.path()).is_ok();
        }
    }
    removed
}

/// The primary checkout's `HEAD` file (it lives in the common git dir, so a
/// linked worktree finds it too) and, when symbolic, the commit it resolves to,
/// so a reset or commit on the same branch also counts as a move. Read
/// failures propagate: an unreadable snapshot must not disable the check.
pub(super) fn primary_head(
    context: &WorktreeContext<'_>,
    facts: &RepoFacts,
    cancel: &CancelToken,
) -> Result<(Vec<u8>, Option<GitOid>), WorktreeError> {
    let bytes = std::fs::read(context.repo.common_git_dir().join("HEAD"))
        .map_err(|_| WorktreeError::InvalidOutput)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let Some(name) = text.trim_end().strip_prefix("ref: ") else {
        return Ok((bytes, None));
    };
    let name = GitRefName::new(name).map_err(|_| WorktreeError::InvalidOutput)?;
    let commit = match facts.resolve_commit(context, name, cancel) {
        Ok(commit) => Some(commit),
        Err(WorktreeError::NotFound) => None,
        Err(error) => return Err(error),
    };
    Ok((bytes, commit))
}

pub(super) fn pin_revision(
    context: &WorktreeContext<'_>,
    facts: &RepoFacts,
    revision: &str,
    cancel: &CancelToken,
) -> Result<GitOid, WorktreeError> {
    GitRevision::new(revision).map_err(|_| WorktreeError::InvalidRequest)?;
    // A tracking ref can alias another ref on the host filesystem. Verify the
    // root against the remote's advertisement and apply any operators to that.
    let (root, suffix) = provenant_core::split_revision_root(revision);
    let parsed = GitRevision::new(root).map_err(|_| WorktreeError::InvalidRequest)?;
    let symbolic = facts.symbolic_revision(context, parsed, cancel)?;
    let symbolic_name = symbolic.as_deref().unwrap_or("");
    let tracking = symbolic_name.starts_with("refs/remotes/");
    let remote_head = (tracking && !root.contains('/') && GitRefName::branch(root).is_ok())
        .then(|| format!("{root}/HEAD"));
    // `remotes/origin/HEAD` and `refs/remotes/origin/HEAD` name the same
    // advertised default as `origin/HEAD`; symbolic resolution has already
    // followed them to a branch, so recover the remote from the spelling.
    let explicit_head = root
        .strip_prefix("refs/remotes/")
        .or_else(|| root.strip_prefix("remotes/"))
        .filter(|rest| rest.ends_with("/HEAD"));
    let candidate = if let Some(head) = &remote_head {
        head.as_str()
    } else if let Some(head) = explicit_head {
        head
    } else if root.ends_with("/HEAD") {
        root
    } else if tracking {
        symbolic_name
    } else {
        root
    };
    let remote = crate::utility::fetch_missing_remote_branch(
        context.executor,
        context.repo,
        context.utility,
        context.home,
        candidate,
        context.mutation_budget,
        cancel,
    )?;
    let verified_revision = match remote {
        crate::utility::RemoteBase::Found(oid) => format!("{oid}{suffix}"),
        crate::utility::RemoteBase::Missing => {
            return Err(WorktreeError::BaseNotFound {
                base: revision.to_owned(),
            });
        }
        // Git resolved the root to a remote-tracking ref, but the remote could
        // not vouch for it (for example `Origin/main` on a case-insensitive
        // filesystem, where the lookup finds `origin/main`). Falling back to the
        // local tracking OID would trust a ref the remote never advertised.
        crate::utility::RemoteBase::NotRemote if tracking => {
            return Err(WorktreeError::BaseUnverified {
                base: revision.to_owned(),
            });
        }
        crate::utility::RemoteBase::NotRemote => revision.to_owned(),
    };
    let parsed = GitRevision::new(&verified_revision).map_err(|_| WorktreeError::InvalidRequest)?;
    facts
        .resolve_revision(context, parsed, cancel)
        .map_err(|error| {
            if matches!(error, WorktreeError::NotFound) {
                WorktreeError::BaseNotFound {
                    base: revision.to_owned(),
                }
            } else {
                error
            }
        })
}

pub(super) fn execute_mutation(
    context: &WorktreeContext<'_>,
    mutation: GitMutation,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    // A failed or timed-out mutation may still have changed repository state.
    let _ = context.facts.take_inventory(context);
    context.workers.invalidate();
    let outcome = execute_git_mutation(
        context.executor,
        context.repo,
        context.utility,
        mutation,
        context.mutation_budget,
        cancel,
    )?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(())
}

pub(super) const MAX_PRUNE_BRANCHES: usize = 100;

pub(super) struct RemovedTree {
    pub(super) info: WorktreeInfo,
    pub(super) lane_cache: Option<PathBuf>,
    pub(super) _writer: LockGuard,
    /// Held when removal took the repository lock itself, so the caller can
    /// settle the branch under it.
    pub(super) repository: Option<LockGuard>,
}

/// How `remove_tree_when` meets the repository lock.
#[derive(Clone, Copy)]
pub(super) enum Repository<'a> {
    /// The caller holds it throughout, as ordinary removal does.
    Held,
    /// Output is deleted under writer custody alone. The lock is taken for the
    /// final registration recheck and the Git mutation, and returned with the
    /// tree. `lanes` is the cheap fence rechecked under it.
    Acquire { lanes: &'a dyn Fn() -> bool },
}

/// The registration still names this tree with its branch and head, unheld.
pub(super) fn tree_unchanged(
    fresh: &[GitWorktree],
    entry: &GitWorktree,
    head: GitOid,
) -> Result<(), WorktreeError> {
    let canonical = canonicalize(&entry.path)?;
    let current = fresh
        .iter()
        .find(|tree| canonicalize(&tree.path).ok().as_deref() == Some(&canonical))
        .ok_or(WorktreeError::IdentityChanged)?;
    if current.branch != entry.branch || current.head != head || current.locked || current.prunable
    {
        return Err(WorktreeError::IdentityChanged);
    }
    Ok(())
}

/// The branch is checked out only at `path` and still points at `head`.
pub(super) fn branch_unchanged(
    context: &WorktreeContext<'_>,
    fresh: &[GitWorktree],
    branch: &GitRefName,
    path: &Path,
    head: GitOid,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    if checked_out_elsewhere(fresh, branch, Some(path))?
        || context
            .facts
            .resolve_commit(context, branch.clone(), cancel)?
            != head
    {
        return Err(WorktreeError::IdentityChanged);
    }
    Ok(())
}

pub(super) fn matching_worktree<'a>(
    entries: &'a [GitWorktree],
    branch: &GitRefName,
    repo_root: &Path,
) -> Result<Option<&'a GitWorktree>, WorktreeError> {
    let repo_root = canonicalize(repo_root)?;
    let mut found = None;
    for entry in entries
        .iter()
        .filter(|entry| !entry.prunable && entry.branch.as_ref() == Some(branch))
    {
        let path = canonicalize(&entry.path)?;
        if path == repo_root {
            return Err(WorktreeError::Primary);
        }
        if found.is_some() {
            return Err(WorktreeError::InvalidOutput);
        }
        found = Some(entry);
    }
    Ok(found)
}

pub(super) fn checked_out_elsewhere(
    entries: &[GitWorktree],
    branch: &GitRefName,
    allowed_path: Option<&Path>,
) -> Result<bool, WorktreeError> {
    let allowed = allowed_path.map(canonicalize).transpose()?;
    for entry in entries
        .iter()
        .filter(|entry| !entry.prunable && entry.branch.as_ref() == Some(branch))
    {
        if allowed.is_none() {
            return Ok(true);
        }
        let path = canonicalize(&entry.path)?;
        if allowed.as_ref() != Some(&path) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn remove_tree(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    expected_head: GitOid,
    cancel: &CancelToken,
    output: &provenant_config::ProjectGcSettings,
) -> Result<RemovedTree, WorktreeError> {
    remove_tree_when(
        context,
        entry,
        expected_head,
        cancel,
        &|| Ok(()),
        Some(output),
        Repository::Held,
    )
}

pub(super) fn remove_tree_when(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    expected_head: GitOid,
    cancel: &CancelToken,
    eligible: &dyn Fn() -> Result<(), WorktreeError>,
    output: Option<&provenant_config::ProjectGcSettings>,
    repository: Repository<'_>,
) -> Result<RemovedTree, WorktreeError> {
    if entry.locked || entry.prunable {
        return Err(WorktreeError::GitEntryHeld {
            locked: entry.locked,
        });
    }
    let canonical = canonicalize(&entry.path)?;
    if canonical == canonicalize(context.repo.root_path())? {
        return Err(WorktreeError::Primary);
    }
    let mut locked = lock_worktree_for_mutation(context, entry, cancel)?;
    context.repo.revalidate()?;
    tree_unchanged(
        &context.facts.read_inventory(context, cancel)?,
        entry,
        expected_head,
    )?;
    eligible()?;
    let prepared =
        worktree_dirty_at(context, locked.verified.repo(), cancel)? == WorktreeCondition::Ignored;
    if prepared {
        let settings = output.cloned().unwrap_or_default();
        gc::prepare_removal(
            context,
            locked.verified.repo(),
            &settings.regenerable,
            &settings.keep,
            cancel,
        )?;
        if matches!(repository, Repository::Held) {
            refresh_census_under_custody(context, &canonical)?;
        }
    }
    // Output preparation can take time. Recheck registration, head and lane
    // eligibility before removing the tree while writer exclusion is held.
    let repository_guard = match repository {
        Repository::Held => None,
        Repository::Acquire { .. } => {
            // Any slow census happens before the repository lock is taken.
            if prepared {
                eligible()?;
            }
            Some(repository_lock(context, LockMode::Exclusive, cancel)?)
        }
    };
    tree_unchanged(
        &context.facts.read_inventory(context, cancel)?,
        entry,
        expected_head,
    )?;
    let fenced = match repository {
        Repository::Held => {
            eligible()?;
            true
        }
        Repository::Acquire { lanes } => lanes(),
    };
    if !fenced {
        return Err(WorktreeError::UnknownWriter);
    }
    match worktree_cleanliness_at(context, locked.verified.repo(), cancel)? {
        WorktreeCleanliness::Clean => {}
        WorktreeCleanliness::Dirty(paths) => return Err(WorktreeError::Dirty(paths)),
        WorktreeCleanliness::Ignored => return Err(WorktreeError::Dirty(String::new())),
        WorktreeCleanliness::GitOperationInProgress => {
            return Err(WorktreeError::GitOperationInProgress);
        }
        WorktreeCleanliness::Unknown => return Err(WorktreeError::UnknownWriter),
    }
    // A cache that cannot be derived must not block removal; warn and skip it.
    let cargo_cache = crate::permissions::worktree_cargo_cache(context.home, &locked.verified)
        .map_err(|error| warn_cache_removal(&error))
        .ok();
    let verified_path = locked.verified.path().to_path_buf();
    execute_mutation(
        context,
        GitMutation::WorktreeRemove {
            path: verified_path,
        },
        cancel,
    )?;
    context.repo.revalidate()?;
    // Keep writer exclusion through cache detachment and verified deletion.
    // Cleanup remains best-effort after Git has removed the worktree.
    let info = WorktreeInfo {
        path: canonical,
        branch: entry
            .branch
            .as_ref()
            .map(|branch| branch.as_str().to_owned()),
        head: expected_head,
        dirty: WorktreeCondition::Clean,
        owner: OwnerStatus::Unknown,
        writer: WriterState::Free,
    };
    let writer = locked.writer.take().ok_or(WorktreeError::UnknownWriter)?;
    Ok(RemovedTree {
        info,
        lane_cache: cargo_cache.map(|cache| cache.path),
        _writer: writer,
        repository: repository_guard,
    })
}

/// Output preparation can take long enough for a native process to enter the
/// tree. Refresh under the retained writer lease before a held-repository
/// removal; land requires complete Free, ordinary custody rejects only a
/// positive occupant as before.
pub(super) fn refresh_census_under_custody(
    context: &WorktreeContext<'_>,
    root: &Path,
) -> Result<(), WorktreeError> {
    context.workers.invalidate();
    let (states, details) = context
        .workers
        .observe_roots_with_details(std::slice::from_ref(&root.to_path_buf()));
    match states.get(root).copied().unwrap_or(WorkerState::Unknown) {
        WorkerState::Free => Ok(()),
        WorkerState::Unknown if !context.workers.requires_complete_census() => Ok(()),
        WorkerState::Unknown => Err(WorktreeError::IncompleteCensus {
            detail: details
                .get(root)
                .cloned()
                .unwrap_or_else(|| context.workers.incomplete_census_detail()),
        }),
        WorkerState::Live => Err(live_refusal(context.workers, root)),
        WorkerState::Busy => Err(WorktreeError::LockBusy),
    }
}

#[allow(clippy::print_stderr)]
pub(super) fn warn_cache_removal(error: &crate::permissions::PermissionError) {
    eprintln!("warning: could not remove worktree lane cache: {error}");
}

/// Pins an integration ref given by name. A local branch wins, then a
/// remote-tracking ref such as `origin/main`, so a stale local branch does not
/// hide a merge that landed on the remote.
pub(super) fn pin_integration(
    context: &WorktreeContext<'_>,
    text: &str,
    cancel: &CancelToken,
) -> Result<(GitRefName, GitOid), WorktreeError> {
    let facts = context.facts;
    let candidates = [
        GitRefName::new(text),
        GitRefName::branch(text),
        GitRefName::remote_tracking(text),
    ];
    let mut invalid = true;
    for candidate in candidates.into_iter().flatten() {
        invalid = false;
        match facts.resolve_commit(context, candidate.clone(), cancel) {
            Ok(oid) => {
                let oid = if candidate.as_str().starts_with("refs/remotes/") {
                    pin_revision(context, facts, candidate.as_str(), cancel)?
                } else {
                    oid
                };
                return Ok((candidate, oid));
            }
            Err(WorktreeError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Err(if invalid {
        WorktreeError::InvalidRequest
    } else {
        WorktreeError::NotFound
    })
}

/// The worktree a name refers to: its branch, or its directory name under
/// `.worktrees/`. Two different worktrees matching one name are ambiguous.
pub(super) fn named_worktree<'a>(
    entries: &'a [GitWorktree],
    name: &str,
    root: &Path,
) -> Result<&'a GitWorktree, WorktreeError> {
    let branch = GitRefName::branch(name).ok();
    let repo_root = canonicalize(root)?;
    let mut found: Option<&GitWorktree> = None;
    for (path, entry) in canonical_worktree_paths(entries)? {
        let by_branch = branch
            .as_ref()
            .is_some_and(|branch| entry.branch.as_ref() == Some(branch));
        let by_directory = path.file_name() == Some(OsStr::new(name));
        if !by_branch && !by_directory {
            continue;
        }
        if path == repo_root {
            return Err(WorktreeError::Primary);
        }
        match found {
            Some(first) if !std::ptr::eq(first, entry) => {
                return Err(WorktreeError::AmbiguousName { name: name.into() });
            }
            _ => found = Some(entry),
        }
    }
    if let Some(entry) = found {
        return Ok(entry);
    }
    if entries.iter().any(|entry| {
        entry.prunable
            && (branch
                .as_ref()
                .is_some_and(|branch| entry.branch.as_ref() == Some(branch))
                || entry.path.file_name() == Some(OsStr::new(name)))
    }) {
        return Err(WorktreeError::StaleRegistration);
    }
    Err(WorktreeError::NotFound)
}

pub(super) fn pin_branch_ref(
    context: &WorktreeContext<'_>,
    reference: &str,
    cancel: &CancelToken,
) -> Result<(GitRefName, GitOid), WorktreeError> {
    let reference = GitRefName::new(reference)
        .or_else(|_| GitRefName::branch(reference))
        .map_err(|_| WorktreeError::InvalidRequest)?;
    let oid = context
        .facts
        .resolve_commit(context, reference.clone(), cancel)?;
    Ok((reference, oid))
}

pub(super) fn retain_reason(error: &WorktreeError) -> RetainReason {
    if error.admission_cause() == Some(AdmissionCause::RegistryLockTimeout) {
        return RetainReason::LockBusy;
    }
    match error {
        WorktreeError::Dirty(_) | WorktreeError::IgnoredPaths(_) => RetainReason::Dirty,
        WorktreeError::GitOperationInProgress => RetainReason::GitOperationInProgress,
        WorktreeError::LiveWriter | WorktreeError::LiveProcess(_) => RetainReason::LiveWriter,
        WorktreeError::LockBusy => RetainReason::LockBusy,
        WorktreeError::Cancelled | WorktreeError::IncompleteCensus { .. } => {
            RetainReason::UnknownWriter
        }
        error if error.is_custody_unknown() => RetainReason::UnknownWriter,
        WorktreeError::CheckedOut => RetainReason::CheckedOut,
        WorktreeError::IdentityChanged => RetainReason::Changed,
        WorktreeError::Primary => RetainReason::Primary,
        WorktreeError::NotFound => RetainReason::Unregistered,
        _ => RetainReason::GitFailure,
    }
}

pub(super) fn retain(outcome: &mut RemoveOutcome, branch: &GitRefName, reason: RetainReason) {
    outcome.retained.push(RetainedBranch {
        branch: branch.short_name().to_owned(),
        reason,
        detail: None,
    });
}

pub(super) struct PruneRange<'a> {
    pub(super) integration: &'a GitRefName,
    pub(super) tip: GitOid,
}

/// Which removal asked for the branch, which decides how contained branches
/// are treated.
#[derive(Clone, Copy, Debug)]
pub(super) enum Policy {
    Sweep,
    Named { discard: bool },
}

/// What becomes of a candidate branch. Decided before any worktree is touched,
/// so an unmerged worktree is never removed by a merged sweep.
#[derive(Clone, Copy, Debug)]
pub(super) enum Disposition {
    Keep(RetainReason),
    Merged,
    Discard,
}

pub(super) fn keep_disposition(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    tip: GitOid,
    range: PruneRange<'_>,
    policy: Policy,
    cancel: &CancelToken,
) -> Disposition {
    let PruneRange {
        integration,
        tip: integration_tip,
    } = range;
    if branch == integration {
        return Disposition::Keep(RetainReason::IntegrationRef);
    }
    // A named removal deletes a branch whose commits are all contained in the
    // integration branch. A sweep keeps those, since a fresh lane branch at
    // the tip may be about to get work.
    if tip == integration_tip {
        return match policy {
            Policy::Sweep => Disposition::Keep(RetainReason::AtIntegrationTip),
            Policy::Named { .. } => Disposition::Merged,
        };
    }
    match is_ancestor(context, tip, integration_tip, cancel) {
        Ok(true) => {}
        Ok(false) => {
            return match policy {
                Policy::Named { discard: true } => Disposition::Discard,
                _ => Disposition::Keep(RetainReason::NotMerged),
            };
        }
        Err(error) => return Disposition::Keep(retain_reason(&error)),
    }
    if let Policy::Named { .. } = policy {
        return Disposition::Merged;
    }
    match is_first_parent_commit(context, integration_tip, tip, cancel) {
        Ok(false) => Disposition::Merged,
        Ok(true) => Disposition::Keep(RetainReason::NoUniqueCommit),
        Err(error) => Disposition::Keep(retain_reason(&error)),
    }
}

/// Delete the local ref once its worktree is gone, under the exact expected
/// tip, after rechecking that no other worktree has it checked out.
pub(super) fn settle_branch(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    tip: GitOid,
    disposition: Disposition,
    cancel: &CancelToken,
    outcome: &mut RemoveOutcome,
) -> Result<(), WorktreeError> {
    let discard = match disposition {
        Disposition::Keep(reason) => {
            retain(outcome, branch, reason);
            return Ok(());
        }
        Disposition::Merged => false,
        Disposition::Discard => true,
    };
    context.repo.revalidate()?;
    let fresh = context.facts.read_inventory(context, cancel)?;
    if checked_out_elsewhere(&fresh, branch, None)? {
        retain(outcome, branch, RetainReason::CheckedOut);
        return Ok(());
    }
    match context
        .facts
        .resolve_commit(context, branch.clone(), cancel)
    {
        Ok(current) if current == tip => {}
        Ok(_) | Err(WorktreeError::NotFound) => {
            retain(outcome, branch, RetainReason::Changed);
            return Ok(());
        }
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
    }
    context.repo.revalidate()?;
    let deleted = execute_git_mutation(
        context.executor,
        context.repo,
        context.utility,
        GitMutation::DeleteRef {
            name: branch.clone(),
            expected_old_oid: tip,
        },
        context.mutation_budget,
        cancel,
    );
    if deleted.is_ok_and(|result| exit_code(&result).ok() == Some(0)) {
        let short = branch.short_name().to_owned();
        if discard {
            outcome.discarded.push(DiscardedRef {
                branch: short,
                head: tip,
            });
        } else {
            outcome.pruned_refs.push(short);
        }
    } else {
        retain(outcome, branch, RetainReason::Changed);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn remove_merged_candidate(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    range: PruneRange<'_>,
    entries: &[GitWorktree],
    branch_tips: &BTreeMap<GitRefName, GitOid>,
    cancel: &CancelToken,
    outcome: &mut RemoveOutcome,
    detached: &mut Vec<RemovedTree>,
    output: &provenant_config::ProjectGcSettings,
) -> Result<(), WorktreeError> {
    let Some(tip) = branch_tips.get(branch).copied() else {
        retain(outcome, branch, RetainReason::Changed);
        return Ok(());
    };
    let disposition = keep_disposition(context, branch, tip, range, Policy::Sweep, cancel);
    if let Disposition::Keep(reason) = disposition {
        retain(outcome, branch, reason);
        return Ok(());
    }
    let entry = match matching_worktree(entries, branch, context.repo.root_path()) {
        Ok(entry) => entry,
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
    };
    // Held until the ref is settled, so the writer lock covers the deletion.
    let _held = match entry {
        None => None,
        Some(entry) => {
            if entry.head != tip {
                retain(outcome, branch, RetainReason::Changed);
                return Ok(());
            }
            if entry.locked || entry.prunable {
                retain(outcome, branch, RetainReason::LockedOrPrunable);
                return Ok(());
            }
            match remove_tree(context, entry, tip, cancel, output) {
                Ok(removed) => {
                    outcome.removed.push(removed.info.clone());
                    detached.push(removed);
                    Some(())
                }
                Err(error) => {
                    retain(outcome, branch, retain_reason(&error));
                    return Ok(());
                }
            }
        }
    };
    settle_branch(context, branch, tip, disposition, cancel, outcome)
}

/// The integration branch the primary checkout is on, pinned to its tip. A
/// detached primary names no integration branch, so removal by name refuses.
pub(super) fn primary_integration(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<(GitRefName, GitOid), WorktreeError> {
    let bytes = std::fs::read(context.repo.common_git_dir().join("HEAD"))
        .map_err(|_| WorktreeError::InvalidOutput)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let name = text
        .trim_end()
        .strip_prefix("ref: ")
        .ok_or(WorktreeError::DetachedPrimary)?;
    pin_branch_ref(context, name, cancel)
}

/// Read-only dirty/output prechecks before a caller releases recovered writer
/// custody. The caller holds the writer lock; removal rechecks after recovery.
pub fn check_removal_preconditions(
    context: &WorktreeContext<'_>,
    worktree: &VerifiedWorktree,
    output: &provenant_config::ProjectGcSettings,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    worktree.repo().revalidate()?;
    match worktree_cleanliness_at(context, worktree.repo(), cancel)? {
        WorktreeCleanliness::Clean => Ok(()),
        WorktreeCleanliness::Ignored => gc::precheck_removal(
            context,
            worktree.repo(),
            &output.regenerable,
            &output.keep,
            cancel,
        ),
        WorktreeCleanliness::Dirty(paths) => Err(WorktreeError::Dirty(paths)),
        WorktreeCleanliness::GitOperationInProgress => Err(WorktreeError::GitOperationInProgress),
        WorktreeCleanliness::Unknown => Err(WorktreeError::UnknownWriter),
    }
}

/// Remove one clean managed linked worktree, or prune a caller-scoped set of
/// merged managed worktrees and their exact expected-old local refs.
pub fn remove(
    context: &WorktreeContext<'_>,
    request: WorktreeRemove,
    cancel: &CancelToken,
) -> Result<RemoveOutcome, WorktreeError> {
    remove_with_output(context, request, cancel, &Default::default())
}

pub fn remove_with_output(
    context: &WorktreeContext<'_>,
    request: WorktreeRemove,
    cancel: &CancelToken,
    output: &provenant_config::ProjectGcSettings,
) -> Result<RemoveOutcome, WorktreeError> {
    remove_with_owner(context, request, cancel, output, None)
}

/// Remove a freshly created lane only while it is still owned by this operation.
/// The owner is checked again under repository and writer custody.
pub fn remove_created_by_operation(
    context: &WorktreeContext<'_>,
    name: String,
    expected_head: GitOid,
    operation: &provenant_core::OperationId,
    cancel: &CancelToken,
) -> Result<RemoveOutcome, WorktreeError> {
    remove_with_owner(
        context,
        WorktreeRemove::Name {
            name,
            discard: false,
            integration: None,
            expected_head: Some(expected_head),
        },
        cancel,
        &Default::default(),
        Some(operation),
    )
}

fn remove_with_owner(
    context: &WorktreeContext<'_>,
    request: WorktreeRemove,
    cancel: &CancelToken,
    output: &provenant_config::ProjectGcSettings,
    operation: Option<&provenant_core::OperationId>,
) -> Result<RemoveOutcome, WorktreeError> {
    validate_binding(context)?;
    let repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let mut detached = Vec::new();
    let result = (|| {
        context.repo.revalidate()?;
        let entries = context.facts.read_inventory(context, cancel)?;
        let mut outcome = RemoveOutcome::default();
        match request {
            WorktreeRemove::Name {
                name,
                discard,
                integration,
                expected_head,
            } => {
                let (integration, integration_tip) = match integration {
                    Some(text) => pin_integration(context, &text, cancel)?,
                    None => primary_integration(context, cancel)?,
                };
                let branch_tips = read_branch_tips(context, cancel)?;
                let entry = named_worktree(&entries, &name, context.repo.root_path())?;
                let tip = entry.head;
                let remove_named = || {
                    let eligible = || {
                        if let Some(operation) = operation {
                            let tree = verify_worktree(context.repo, &entry.path)?;
                            let owner = load_owner(&tree)?.ok_or(WorktreeError::UnknownWriter)?;
                            if owner.origin != WorktreeOrigin::Lane
                                || owner.operation_id.as_ref() != Some(operation)
                            {
                                return Err(WorktreeError::IdentityChanged);
                            }
                        }
                        Ok(())
                    };
                    remove_tree_when(
                        context,
                        entry,
                        tip,
                        cancel,
                        &eligible,
                        Some(output),
                        Repository::Held,
                    )
                };
                // `remove_tree` rechecks this tip under the worktree lock, so
                // a commit made after the caller's check cannot be removed.
                if expected_head.is_some_and(|expected| expected != tip) {
                    return Err(WorktreeError::IdentityChanged);
                }
                let Some(branch) = &entry.branch else {
                    if !discard && !is_ancestor(context, tip, integration_tip, cancel)? {
                        return Err(WorktreeError::DetachedUnmerged { head: tip });
                    }
                    let removed = remove_named()?;
                    outcome.removed.push(removed.info.clone());
                    detached.push(removed);
                    return Ok(outcome);
                };
                if branch_tips.get(branch) != Some(&tip) {
                    return Err(WorktreeError::IdentityChanged);
                }
                let disposition = keep_disposition(
                    context,
                    branch,
                    tip,
                    PruneRange {
                        integration: &integration,
                        tip: integration_tip,
                    },
                    Policy::Named { discard },
                    cancel,
                );
                // A dirty or live worktree fails here, before any branch changes.
                let removed = remove_named()?;
                outcome.removed.push(removed.info.clone());
                detached.push(removed);
                settle_branch(context, branch, tip, disposition, cancel, &mut outcome)?;
            }
            WorktreeRemove::Merged { integration, scope } => {
                let (integration, integration_tip) =
                    pin_integration(context, &integration, cancel)?;
                let branch_tips = read_branch_tips(context, cancel)?;
                let mut branches = BTreeMap::new();
                match scope {
                    PruneScope::Branches(names) => {
                        if names.is_empty() || names.len() > MAX_PRUNE_BRANCHES {
                            return Err(WorktreeError::InvalidRequest);
                        }
                        for name in names {
                            let branch = GitRefName::branch(name)
                                .map_err(|_| WorktreeError::InvalidRequest)?;
                            if branches.insert(branch, ()).is_some() {
                                return Err(WorktreeError::InvalidRequest);
                            }
                        }
                    }
                    PruneScope::Since(bound) => {
                        if !is_ancestor(context, bound, integration_tip, cancel)? {
                            return Err(WorktreeError::InvalidRequest);
                        }
                        // Scope ancestry-proven tips to the pinned graph delta.
                        // Squash merges lack exact-head proof here and stay put.
                        for (branch, tip) in &branch_tips {
                            if is_ancestor(context, *tip, integration_tip, cancel)?
                                && !is_ancestor(context, *tip, bound, cancel)?
                            {
                                branches.insert(branch.clone(), ());
                            }
                        }
                        if branches.len() > MAX_PRUNE_BRANCHES {
                            return Err(WorktreeError::InvalidRequest);
                        }
                    }
                }
                for branch in branches.keys() {
                    remove_merged_candidate(
                        context,
                        branch,
                        PruneRange {
                            integration: &integration,
                            tip: integration_tip,
                        },
                        &entries,
                        &branch_tips,
                        cancel,
                        &mut outcome,
                        &mut detached,
                        output,
                    )?;
                }
            }
        }
        Ok(outcome)
    })();
    drop(repository);
    for removed in detached {
        if let Some(cache) = removed.lane_cache.as_deref()
            && let Err(error) = crate::cache::prune_worktree_cargo_cache(context.home, cache)
        {
            warn_cache_removal(&error);
        }
    }
    result
}
