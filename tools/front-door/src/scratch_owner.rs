//! Finished-owner proofs for attempt scratch retention.
use crate::retention_owner;
use provenant_confinement::{
    repo::GitRefName,
    worktrees::{ForgeObservation, RetirementFeeds},
};
use provenant_core::{AttemptId, CancelToken, GitOid, Roots};
use provenant_os::{
    clock::SystemClock,
    exec::{SystemExecutor, Termination},
};
use provenant_run::{AttemptRecord, CanonicalRequest, Contributions, Request, ScratchOwner};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

/// Application evidence that an attempt's temporary files no longer have a
/// live owner. Unavailable or ambiguous evidence always retains the scratch.
pub(crate) struct Owner<'a> {
    roots: &'a Roots,
    home: &'a Path,
    _assets: &'a Path,
    contributions: RefCell<Option<Result<Option<Contributions>, String>>>,
    branches: RefCell<BTreeMap<AttemptId, Option<String>>>,
    forge_owners: RefCell<BTreeMap<PathBuf, retention_owner::Owner<'a>>>,
}

impl<'a> Owner<'a> {
    pub(crate) fn new(roots: &'a Roots, home: &'a Path, assets: &'a Path) -> Self {
        Self {
            roots,
            home,
            _assets: assets,
            contributions: RefCell::new(None),
            branches: RefCell::new(BTreeMap::new()),
            forge_owners: RefCell::new(BTreeMap::new()),
        }
    }

    pub(crate) fn disposition(
        &self,
        record: &AttemptRecord,
        request: &CanonicalRequest,
    ) -> ScratchOwner {
        if record.superseded_by.is_some() {
            return ScratchOwner::Finished;
        }

        let writer = request.task.writer.as_ref();
        let owner_path = writer.map(|writer| writer.worktree.as_path()).or(request
            .task
            .access
            .source_root
            .as_deref());
        if let Some(path) = owner_path {
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return ScratchOwner::Finished;
                }
                Err(_) | Ok(_) => {}
            }
        }

        let graph_id = record
            .context
            .as_ref()
            .and_then(|context| context.graph_id.as_deref())
            .or_else(|| {
                request
                    .task
                    .context
                    .as_ref()
                    .and_then(|context| context.graph_id.as_deref())
            });
        if let Some(graph_id) = graph_id
            && provenant_fabric_runtime::graph::FileJournal::read_status_checkpoint(
                &self.roots.state,
                record.project_id,
                graph_id,
            )
            .is_ok_and(|checkpoint| checkpoint.is_some_and(|checkpoint| checkpoint.cancelled))
        {
            return ScratchOwner::Finished;
        }

        let Some(project) = writer.map(|writer| writer.project.as_path()).or(request
            .task
            .access
            .source_root
            .as_deref())
        else {
            return if graph_id.is_some() {
                ScratchOwner::Owned
            } else {
                ScratchOwner::Unowned
            };
        };
        let Some(branch) = self.branch(record.attempt_id, record.project_id) else {
            return ScratchOwner::Owned;
        };
        if writer.is_some()
            && record
                .delivery
                .as_ref()
                .is_some_and(|delivery| delivery.branch != branch)
        {
            return ScratchOwner::Owned;
        }
        let attempt_head = record.head.or_else(|| {
            if writer.is_none() {
                request
                    .task
                    .head
                    .as_deref()
                    .and_then(|head| GitOid::parse(head).ok())
            } else {
                None
            }
        });
        let Some(head) = attempt_head else {
            return ScratchOwner::Owned;
        };
        if writer.is_some()
            && record
                .delivery
                .as_ref()
                .and_then(|delivery| delivery.head)
                .is_some_and(|delivery_head| delivery_head != head)
        {
            return ScratchOwner::Owned;
        }

        let Ok(branch_ref) = GitRefName::branch(&branch) else {
            return ScratchOwner::Owned;
        };
        match self.forge(project, &branch_ref) {
            Some(observation)
                if Self::forge_proves_finished(&observation, head, |ancestor, descendant| {
                    self.is_ancestor(project, ancestor, &descendant.to_string())
                }) =>
            {
                ScratchOwner::Finished
            }
            Some(ForgeObservation::Absent) => {
                if self.local_default_contains(project, &branch, head) {
                    ScratchOwner::Finished
                } else {
                    ScratchOwner::Owned
                }
            }
            Some(_) | None => ScratchOwner::Owned,
        }
    }

    fn forge_proves_finished(
        observation: &ForgeObservation,
        expected: GitOid,
        mut is_ancestor: impl FnMut(GitOid, GitOid) -> bool,
    ) -> bool {
        let closed_head = match observation {
            ForgeObservation::Merged { head, .. } | ForgeObservation::Closed { head } => *head,
            ForgeObservation::Absent
            | ForgeObservation::Open { .. }
            | ForgeObservation::Unknown(_) => return false,
        };
        expected == closed_head || is_ancestor(expected, closed_head)
    }

    fn branch(&self, attempt: AttemptId, project: provenant_core::ProjectId) -> Option<String> {
        if let Some(branch) = self.branches.borrow().get(&attempt) {
            return branch.clone();
        }
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let mut contributions = self.contributions.borrow_mut();
        if contributions.is_none() {
            *contributions = Some(
                Contributions::open_read_only(self.roots.state.as_path(), &call)
                    .map_err(|error| error.to_string()),
            );
        }
        let branch = contributions
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(Option::as_ref)
            .and_then(|index| {
                index
                    .branch_for_attempt(project, attempt, &call)
                    .ok()
                    .flatten()
            });
        self.branches.borrow_mut().insert(attempt, branch.clone());
        branch
    }

    fn forge(&self, project: &Path, branch_ref: &GitRefName) -> Option<ForgeObservation> {
        let mut owners = self.forge_owners.borrow_mut();
        let owner = owners
            .entry(project.to_path_buf())
            .or_insert_with(|| retention_owner::Owner::new(self.roots, self.home, project));
        Some(owner.forge(branch_ref))
    }

    /// Prove a no-PR branch's exact recorded tip is in the locally observed
    /// origin default history. No ref is fetched or rewritten here.
    fn local_default_contains(&self, project: &Path, branch: &str, head: GitOid) -> bool {
        let Ok(branch) = GitRefName::branch(branch) else {
            return false;
        };
        let Ok(executor) = SystemExecutor::new(1) else {
            return false;
        };
        let local_ref = branch.as_str();
        let local_commit_ref = format!("{local_ref}^{{commit}}");
        let local = match crate::gc_support::host_git_with_stderr(
            project,
            &executor,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &local_commit_ref,
            ],
            Duration::from_secs(5),
            false,
        ) {
            Ok(output) if output.termination == Termination::Exit(0) => output,
            _ => return false,
        };
        let Ok(local_tip) = std::str::from_utf8(&local.stdout.bytes) else {
            return false;
        };
        let Ok(local_tip) = GitOid::parse(local_tip.trim()) else {
            return false;
        };
        if local_tip != head {
            return false;
        }

        let default = match crate::gc_support::host_git_with_stderr(
            project,
            &executor,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            Duration::from_secs(5),
            false,
        ) {
            Ok(output) if output.termination == Termination::Exit(0) => output,
            _ => return false,
        };
        let Ok(default) = std::str::from_utf8(&default.stdout.bytes) else {
            return false;
        };
        let default = default.trim();
        let Some(remote_name) = default.strip_prefix("refs/remotes/") else {
            return false;
        };
        let Ok(default_ref) = GitRefName::remote_tracking(remote_name) else {
            return false;
        };
        self.is_ancestor(project, head, default_ref.as_str())
    }

    fn is_ancestor(&self, project: &Path, ancestor: GitOid, descendant: &str) -> bool {
        let Ok(executor) = SystemExecutor::new(1) else {
            return false;
        };
        matches!(
            crate::gc_support::host_git_with_stderr(
                project,
                &executor,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &ancestor.to_string(),
                    descendant,
                ],
                Duration::from_secs(5),
                false,
            ),
            Ok(output) if output.termination == Termination::Exit(0)
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn closed_or_merged_branch_finishes_only_covered_attempt_heads() {
        use super::*;

        // Spec: docs/specs/disk.md#classes-grant-no-authority
        let earlier = GitOid::parse(&"4a".repeat(20)).unwrap();
        let closed_head = GitOid::parse(&"5b".repeat(20)).unwrap();
        let later = GitOid::parse(&"6c".repeat(20)).unwrap();
        let unrelated = GitOid::parse(&"7d".repeat(20)).unwrap();
        let ancestry = [(earlier, closed_head), (closed_head, later)];
        let cases = [
            (
                ForgeObservation::Merged {
                    pr: "42".into(),
                    head: closed_head,
                    merge_commit: None,
                },
                closed_head,
                true,
            ),
            (
                ForgeObservation::Closed { head: closed_head },
                earlier,
                true,
            ),
            (
                ForgeObservation::Merged {
                    pr: "42".into(),
                    head: closed_head,
                    merge_commit: None,
                },
                earlier,
                true,
            ),
            (ForgeObservation::Closed { head: closed_head }, later, false),
            (
                ForgeObservation::Merged {
                    pr: "42".into(),
                    head: closed_head,
                    merge_commit: None,
                },
                unrelated,
                false,
            ),
            (
                ForgeObservation::Merged {
                    pr: "42".into(),
                    head: unrelated,
                    merge_commit: None,
                },
                closed_head,
                false,
            ),
            (
                ForgeObservation::Open {
                    base: "refs/heads/main".into(),
                },
                earlier,
                false,
            ),
            (ForgeObservation::Unknown("offline".into()), earlier, false),
            (ForgeObservation::Absent, earlier, false),
        ];
        for (observation, attempt_head, finished) in cases {
            assert_eq!(
                Owner::forge_proves_finished(&observation, attempt_head, |ancestor, descendant| {
                    ancestry.contains(&(ancestor, descendant))
                }),
                finished
            );
        }
    }
}
