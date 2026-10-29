//! Pre-push refusal policy over pinned, bounded, read-only Git evidence.
use crate::{
    GitRead, UtilityBudget, UtilityRuntime,
    repo::{GitRefName, ReadOnlyRepo},
    utility::{advertised_remote_head, run_release_read_utility},
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
    #[error("deleting the default branch")]
    DefaultDeletion,
    #[error(
        "pushing the default branch from a linked worktree; push a topic branch and merge by pull request"
    )]
    LinkedWorktree,
    #[error("non-fast-forward update")]
    NonFastForward,
    #[error("conflict markers in pushed commits")]
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
        let head = advertised_remote_head(
            self.executor,
            self.repo,
            self.runtime,
            remote,
            self.budget,
            self.cancel,
        )
        .map_err(|_| PushRefusal::Evidence)?;
        if head.termination != Termination::Exit(0) {
            return Err(PushRefusal::Evidence);
        }
        let text = std::str::from_utf8(&head.stdout.bytes).map_err(|_| PushRefusal::Evidence)?;
        let mut targets = text.lines().filter_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        });
        let branch = targets.next().ok_or(PushRefusal::Evidence)?;
        if targets.next().is_some() {
            return Err(PushRefusal::Evidence);
        }
        let default = GitRefName::branch(branch).map_err(|_| PushRefusal::Evidence)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;
    use std::error::Error;

    #[test]
    fn advertised_default_overrides_stale_tracking_head() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let head = fixture.head()?;
        let root = fixture.root.to_str().ok_or("non-UTF8 root")?;
        fixture.run(&["remote", "add", "origin", root])?;
        let symbolic = fixture.run(&["symbolic-ref", "HEAD"])?;
        let default = std::str::from_utf8(&symbolic.stdout.bytes)?.trim();
        fixture.run(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/stale",
        ])?;
        let repo = fixture.runtime.open_read_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let cancel = CancelToken::new();
        let context = PushContext {
            repo: &repo,
            runtime: &fixture.runtime,
            executor: &fixture.executor,
            budget: fixture.budget()?,
            cancel: &cancel,
        };
        let update = PushUpdate {
            local: GitOid::parse(&"0".repeat(40))?,
            remote_ref: default.to_owned(),
            remote: head,
        };
        assert!(matches!(
            context.check("origin", &[update]),
            Err(PushRefusal::DefaultDeletion)
        ));
        Ok(())
    }
}
