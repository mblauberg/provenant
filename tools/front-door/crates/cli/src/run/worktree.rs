use super::*;

/// Worktree access is explicit. Submission resolves and verifies the path;
/// a worktree with read-only access is refused, never run read-only.
pub fn worktree_access(writer: bool, worktree: Option<&Path>) -> Result<(), Fail> {
    match worktree {
        None if !writer => Ok(()),
        None => Err(Fail::new(
            Status::Invalid,
            ErrorKind::WorktreeRequired,
            "worktree-write access needs --worktree, or --branch and --base with a peer --host",
        )),
        Some(_) if !writer => Err(Fail::new(
            Status::Invalid,
            ErrorKind::WorktreeNotApplicable,
            "--worktree requires --mode worktree-write",
        )),
        Some(_) => Ok(()),
    }
}

/// A `--worktree-branch` dispatch's owned tree, as preflight settled it.
pub(super) struct OwnedWorktree {
    pub(super) branch: String,
    pub(super) host: provenant_core::HostId,
    pub(super) created_at: WallTime,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum OwnWorktreeError {
    #[error(
        "--worktree-branch cannot reuse an adopted or foreign owner; its ownership, label and base are unchanged"
    )]
    ForeignOwner,
    #[error("{error}")]
    Dirty {
        error: run::RunError,
        path: PathBuf,
        base: provenant_core::GitOid,
        identifier: std::ffi::OsString,
    },
    #[error(transparent)]
    Other(run::RunError),
}

impl From<OwnWorktreeError> for Fail {
    fn from(error: OwnWorktreeError) -> Self {
        match error {
            OwnWorktreeError::ForeignOwner => {
                Fail::new(Status::Refused, ErrorKind::Refused, error.to_string())
            }
            OwnWorktreeError::Dirty { error, .. } => error.into(),
            OwnWorktreeError::Other(error) => error.into(),
        }
    }
}

/// Create or claim the owned tree and publish its lane ownership. A new
/// tree's owner records the commit creation pinned, so nothing is resolved
/// again after the side effect and a replay finds the same base.
#[allow(clippy::too_many_arguments)]
pub(super) fn own_worktree(
    owned: &OwnedWorktree,
    requested_base: Option<&str>,
    operation_id: &OperationId,
    edge: &Edge<'_>,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
    terminal_attempt: impl Fn(provenant_core::AttemptId) -> bool,
    created: &mut Option<(PathBuf, String)>,
    clock: &SystemClock,
    cancel: &CancelToken,
    lock_budget: Option<provenant_core::WaitBudget>,
) -> Result<
    (
        provenant_confinement::worktrees::WorktreeInfo,
        provenant_core::GitOid,
    ),
    OwnWorktreeError,
> {
    let OwnedWorktree {
        branch,
        host,
        created_at,
    } = owned;
    let mut owner_refused = false;
    let mut dirty_tree = None;
    provenant_confinement::worktrees::with_context(
        edge.roots,
        edge.home,
        edge.cwd,
        clock,
        cancel,
        |context| {
            use provenant_confinement::worktrees::{
                self, WorktreeCreate, WorktreeOrigin, WorktreeOwner, WriterState,
            };
            let context = &worktrees::WorktreeContext {
                workers,
                lock_budget: lock_budget.unwrap_or(context.lock_budget),
                ..*context
            };
            let requested_base_oid = if let Some(base) = requested_base {
                let git = provenant_os::git::git_executable()
                    .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                let resolved = run::resolve_git_base(&git, context.repo.root_path(), base)
                    .map_err(|_| worktrees::WorktreeError::BaseNotFound {
                        base: base.to_owned(),
                    })?;
                Some(
                    provenant_core::GitOid::parse(&resolved)
                        .map_err(|_| worktrees::WorktreeError::InvalidOutput)?,
                )
            } else {
                None
            };
            let existing = worktrees::list_with_observation(context, cancel)?
                .into_iter()
                .find(|observation| {
                    observation.info.branch.as_deref()
                        == Some(format!("refs/heads/{branch}").as_str())
                });
            let (info, pinned) = match existing {
                Some(observation) => {
                    let info = observation.info;
                    if let Some(base) = requested_base_oid
                        && !worktrees::is_ancestor(context, base, info.head, cancel)?
                    {
                        return Err(worktrees::WorktreeError::BranchExistsWithBase {
                            branch: branch.clone(),
                            head: info.head,
                            base,
                        });
                    }
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    let owner = worktrees::load_owner(&tree)?;
                    if owner
                        .as_ref()
                        .is_some_and(|owner| owner.origin != WorktreeOrigin::Lane)
                    {
                        owner_refused = true;
                        return Err(worktrees::WorktreeError::UnknownWriter);
                    }
                    if observation.observed == worktrees::WorkerState::Unknown
                        && !observation.registration_unknown
                        && owner.is_none()
                        && worktrees::cache_writer_probe(context.project_state, &info.path).is_ok()
                    {
                        let evidence = context
                            .workers
                            .observe_evidence(std::slice::from_ref(&info.path), false);
                        return Err(worktrees::WorktreeError::CensusUnknown {
                            path: info.path.clone(),
                            detail: evidence
                                .details
                                .get(&info.path)
                                .cloned()
                                .unwrap_or_else(|| context.workers.incomplete_census_detail()),
                        });
                    }
                    match info.writer {
                        WriterState::Free => {}
                        WriterState::Busy => {
                            return Err(worktrees::WorktreeError::WriterBusy {
                                holder: owner.and_then(|owner| owner.attempt_id),
                            });
                        }
                        WriterState::Live => {
                            return Err(worktrees::live_refusal(context.workers, &info.path));
                        }
                        WriterState::Unknown => {
                            let settled = owner
                                .as_ref()
                                .and_then(|owner| owner.attempt_id)
                                .is_some_and(&terminal_attempt);
                            if !settled
                                || worktrees::cache_writer_probe(context.project_state, &info.path)
                                    .is_err()
                            {
                                return Err(worktrees::WorktreeError::UnknownWriterAt {
                                    path: info.path.clone(),
                                });
                            }
                        }
                    }
                    (info, None)
                }
                None => {
                    // The creation receipt: the tree's head is the commit
                    // creation pinned the base to.
                    let info = worktrees::create(
                        context,
                        WorktreeCreate {
                            branch: branch.clone(),
                            base: requested_base.map(str::to_owned),
                        },
                        cancel,
                    )?;
                    *created = Some((info.path.clone(), branch.clone()));
                    let pinned = info.head.to_string();
                    (info, Some(pinned))
                }
            };
            let verified = provenant_confinement::verify_worktree(context.repo, &info.path)?;
            let prior = worktrees::load_owner(&verified)?;
            if prior
                .as_ref()
                .is_some_and(|owner| owner.origin != WorktreeOrigin::Lane)
            {
                owner_refused = true;
                return Err(worktrees::WorktreeError::UnknownWriter);
            }
            let terminal_prior = prior
                .as_ref()
                .and_then(|prior| prior.attempt_id)
                .is_some_and(&terminal_attempt);
            let expected_prior = prior.clone();
            let owner = match prior {
                Some(owner) if owner.operation_id.as_ref() == Some(operation_id) => owner,
                prior => WorktreeOwner {
                    attempt_id: None,
                    operation_id: Some(operation_id.clone()),
                    branch: branch.clone(),
                    base: match (pinned, requested_base_oid, prior) {
                        (Some(pinned), _, _) => pinned,
                        (_, Some(base), _) => base.to_string(),
                        (None, None, Some(prior)) => prior.base,
                        // An unowned existing tree: nothing was created, so
                        // a base that does not resolve leaves no side effect.
                        (None, None, None) => {
                            let git = provenant_os::git::git_executable()
                                .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                            run::resolve_git_base(
                                &git,
                                context.repo.root_path(),
                                requested_base.unwrap_or("HEAD"),
                            )
                            .map_err(|_| worktrees::WorktreeError::InvalidRequest)?
                        }
                    },
                    host: *host,
                    created_at: *created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            };
            if created.as_ref().is_some_and(|(path, _)| path == &info.path) {
                worktrees::save_owner(&verified, &owner)?;
            } else {
                if let Err(error) = worktrees::save_lane_owner_when_free(
                    context,
                    &verified,
                    &owner,
                    info.head,
                    expected_prior.as_ref(),
                    terminal_prior,
                    cancel,
                ) {
                    if matches!(error, worktrees::WorktreeError::Dirty(_))
                        && let Ok(base) = provenant_core::GitOid::parse(&owner.base)
                    {
                        let identifier = verified
                            .repo()
                            .private_git_dir()
                            .file_name()
                            .ok_or(worktrees::WorktreeError::InvalidOutput)?
                            .to_owned();
                        dirty_tree = Some((info.path.clone(), base, identifier));
                    }
                    return Err(error);
                }
            }
            let base = provenant_core::GitOid::parse(&owner.base)
                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
            Ok((info, base))
        },
    )
    .map_err(|error| {
        if owner_refused {
            OwnWorktreeError::ForeignOwner
        } else if let Some((path, base, identifier)) = dirty_tree {
            OwnWorktreeError::Dirty {
                error: run::RunError::from(error),
                path,
                base,
                identifier,
            }
        } else {
            OwnWorktreeError::Other(run::RunError::from(error))
        }
    })
}
