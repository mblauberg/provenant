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
    #[error(transparent)]
    Other(run::RunError),
}

impl From<OwnWorktreeError> for Fail {
    fn from(error: OwnWorktreeError) -> Self {
        match error {
            OwnWorktreeError::ForeignOwner => {
                Fail::new(Status::Refused, ErrorKind::Refused, error.to_string())
            }
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
    created: &mut Option<(PathBuf, String)>,
    clock: &SystemClock,
    cancel: &CancelToken,
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
    provenant_confinement::worktrees::with_context(
        edge.roots,
        edge.home,
        edge.cwd,
        clock,
        cancel,
        |context| {
            use provenant_confinement::worktrees::{
                self, WorktreeCondition, WorktreeCreate, WorktreeOrigin, WorktreeOwner, WriterState,
            };
            let context = &worktrees::WorktreeContext {
                workers,
                ..*context
            };
            let existing = worktrees::list(context, cancel)?.into_iter().find(|tree| {
                tree.branch.as_deref() == Some(format!("refs/heads/{branch}").as_str())
            });
            let (info, pinned) = match existing {
                Some(info) => {
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    let owner = worktrees::load_owner(&tree)?;
                    if owner
                        .as_ref()
                        .is_some_and(|owner| owner.origin != WorktreeOrigin::Lane)
                    {
                        owner_refused = true;
                        return Err(worktrees::WorktreeError::UnknownWriter);
                    }
                    if info.dirty != WorktreeCondition::Clean {
                        return Err(worktrees::WorktreeError::Dirty);
                    }
                    match info.writer {
                        WriterState::Free => {}
                        WriterState::Busy | WriterState::Live => {
                            return Err(worktrees::WorktreeError::WriterBusy {
                                holder: owner.and_then(|owner| owner.attempt_id),
                            });
                        }
                        WriterState::Unknown => {
                            return Err(worktrees::WorktreeError::UnknownWriter);
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
            let owner = match prior {
                Some(owner) if owner.operation_id.as_ref() == Some(operation_id) => owner,
                prior => WorktreeOwner {
                    attempt_id: None,
                    operation_id: Some(operation_id.clone()),
                    branch: branch.clone(),
                    base: match (pinned, prior) {
                        (Some(pinned), _) => pinned,
                        (None, Some(prior)) => prior.base,
                        // An unowned existing tree: nothing was created, so
                        // a base that does not resolve leaves no side effect.
                        (None, None) => {
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
                worktrees::save_lane_owner_when_free(context, &verified, &owner, cancel)?;
            }
            let base = provenant_core::GitOid::parse(&owner.base)
                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
            Ok((info, base))
        },
    )
    .map_err(|error| {
        if owner_refused {
            OwnWorktreeError::ForeignOwner
        } else {
            OwnWorktreeError::Other(run::RunError::from(error))
        }
    })
}
