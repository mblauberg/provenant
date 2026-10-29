//! Pre-push refusal policy over pinned, bounded, read-only Git evidence.
use crate::{
    GitRead, UtilityBudget, UtilityRuntime,
    repo::{GitRefName, ReadOnlyRepo},
    utility::run_release_read_utility,
};
use provenant_core::{CancelToken, GitOid};
use provenant_os::exec::{Executor, Termination};
use thiserror::Error;

pub struct PushUpdate {
    pub local: GitOid,
    pub remote_ref: String,
    pub remote: GitOid,
}
#[derive(Debug, Error)]
pub enum PushRefusal {
    #[error("Git evidence unavailable or incomplete")]
    Evidence,
    #[error(
        "deleting the default branch is refused; delete a topic branch instead, for example `git push origin --delete my-topic`"
    )]
    DefaultDeletion,
    #[error(
        "pushing the default branch from a linked worktree; push a topic branch and merge by pull request"
    )]
    LinkedWorktree,
    #[error(
        "non-fast-forward update is refused; integrate the remote first with `git fetch origin && git merge origin/my-topic`, then push again"
    )]
    NonFastForward,
    #[error(
        "conflict markers in pushed commits; remove the marker lines, then `git commit --amend --no-edit` and push again"
    )]
    ConflictMarkers,
}
pub fn is_zero(oid: GitOid) -> bool {
    match oid {
        GitOid::Sha1(b) => b.iter().all(|b| *b == 0),
        GitOid::Sha256(b) => b.iter().all(|b| *b == 0),
    }
}

pub struct PushContext<'a> {
    pub repo: &'a ReadOnlyRepo,
    pub runtime: &'a UtilityRuntime,
    pub executor: &'a dyn Executor,
    pub budget: UtilityBudget,
    pub cancel: &'a CancelToken,
}
impl PushContext<'_> {
    fn read(&self, request: GitRead) -> Result<provenant_os::exec::ExecOutcome, PushRefusal> {
        run_release_read_utility(
            self.executor,
            request,
            self.repo,
            self.runtime,
            self.budget,
            self.cancel,
        )
        .map_err(|_| PushRefusal::Evidence)
    }
    pub fn check(&self, remote: &str, updates: &[PushUpdate]) -> Result<(), PushRefusal> {
        let head = self.read(GitRead::RemoteHead {
            remote: remote.to_owned(),
        })?;
        let default = match head.termination {
            Termination::Exit(0) => {
                let prefix = format!("refs/remotes/{remote}/");
                let branch = std::str::from_utf8(&head.stdout.bytes)
                    .ok()
                    .and_then(|s| s.strip_suffix('\n'))
                    .and_then(|s| s.strip_prefix(&prefix))
                    .ok_or(PushRefusal::Evidence)?;
                GitRefName::branch(branch).map_err(|_| PushRefusal::Evidence)?
            }
            Termination::Exit(1) => {
                let mut found = None;
                for branch in ["main", "master"] {
                    let name = GitRefName::branch(branch).map_err(|_| PushRefusal::Evidence)?;
                    let tip = self.read(GitRead::ResolveRef { name: name.clone() })?;
                    if tip.termination == Termination::Exit(0) {
                        found = Some(name);
                        break;
                    }
                    if tip.termination != Termination::Exit(128)
                        && tip.termination != Termination::Exit(1)
                    {
                        return Err(PushRefusal::Evidence);
                    }
                }
                found.ok_or(PushRefusal::Evidence)?
            }
            _ => return Err(PushRefusal::Evidence),
        };
        for update in updates {
            if update.remote_ref == default.as_str() {
                if is_zero(update.local) {
                    return Err(PushRefusal::DefaultDeletion);
                }
                if self.repo.0.private_git_dir() != self.repo.0.common_git_dir() {
                    return Err(PushRefusal::LinkedWorktree);
                }
            }
            if is_zero(update.local) {
                continue;
            }
            if !is_zero(update.remote) {
                let ancestor = self.read(GitRead::MergeBaseIsAncestor {
                    ancestor: update.remote,
                    tip: update.local,
                })?;
                match ancestor.termination {
                    Termination::Exit(0) => {}
                    Termination::Exit(1) => return Err(PushRefusal::NonFastForward),
                    _ => return Err(PushRefusal::Evidence),
                }
            }
            let patch = self.read(GitRead::PushPatch {
                base: (!is_zero(update.remote)).then_some(update.remote),
                head: update.local,
            })?;
            if patch.termination != Termination::Exit(0) {
                return Err(PushRefusal::Evidence);
            }
            if patch.stdout.bytes.split(|b| *b == b'\n').any(|line| {
                [b"+<<<<<<<".as_slice(), b"+>>>>>>>".as_slice()]
                    .iter()
                    .any(|marker| {
                        line.strip_prefix(*marker).is_some_and(|tail| {
                            tail.is_empty() || tail.first().is_some_and(u8::is_ascii_whitespace)
                        })
                    })
            }) {
                return Err(PushRefusal::ConflictMarkers);
            }
        }
        Ok(())
    }
}
