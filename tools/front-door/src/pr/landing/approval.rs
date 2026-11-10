//! What stands as a pull request's landing approval: an approving forge review
//! at the head, or a Provenant review record whose review covers the head.
use super::{
    LandingError,
    ports::{Approval, Forge, MergeTree, Repo, ReviewRecords},
};
use provenant_cli::domains::ApprovalSource;
use provenant_core::review_cover::{self, ReviewGit, Reviewed};

/// The approval source of one landing run.
#[derive(Clone, Copy)]
pub struct Approvals<'a> {
    source: ApprovalSource,
    records: &'a dyn ReviewRecords,
}

#[cfg(test)]
struct NoRecords;
#[cfg(test)]
impl ReviewRecords for NoRecords {
    fn reviews(&self, _number: u64) -> Result<Vec<super::ports::ReviewRecord>, LandingError> {
        Ok(Vec::new())
    }
}

impl<'a> Approvals<'a> {
    pub fn new(source: ApprovalSource, records: &'a dyn ReviewRecords) -> Self {
        Self { source, records }
    }

    /// Approving forge reviews only.
    #[cfg(test)]
    pub fn forge() -> Approvals<'static> {
        Approvals {
            source: ApprovalSource::Forge,
            records: &NoRecords,
        }
    }

    pub fn source(&self) -> ApprovalSource {
        self.source
    }

    /// Approvals that stand for `head` of pull request `number`. A review
    /// record is proved against `target`, the fetched target commit, so a
    /// merge-in from it or a rebase with an unchanged delta keeps the review.
    pub fn standing(
        &self,
        forge: &mut dyn Forge,
        repo: &mut dyn Repo,
        number: u64,
        head: &str,
        target: &str,
    ) -> Result<Vec<Approval>, LandingError> {
        if self.source == ApprovalSource::Forge {
            return forge.approvals(number, head);
        }
        let mut approvals = Vec::new();
        for record in self.records.reviews(number)? {
            let reviewed = Reviewed {
                head: &record.head,
                base: Some(&record.base),
            };
            if review_cover::coverage(&mut RepoGit(repo), reviewed, head, target)?.covers() {
                approvals.push(Approval {
                    reference: record.reference,
                    reviewer: record.reviewer,
                });
            }
        }
        Ok(approvals)
    }
}

impl<'a> Approvals<'a> {
    /// Bind to the repository and fetched target the proof reads.
    pub fn at<'r>(self, repo: &'r mut dyn Repo, target: &'r str) -> Approver<'a, 'r> {
        Approver {
            approvals: self,
            repo,
            target,
        }
    }
}

/// [`Approvals`] bound to a repository and the fetched target commit.
pub struct Approver<'a, 'r> {
    approvals: Approvals<'a>,
    repo: &'r mut dyn Repo,
    target: &'r str,
}

impl Approver<'_, '_> {
    pub fn approvals(
        &mut self,
        forge: &mut dyn Forge,
        number: u64,
        head: &str,
    ) -> Result<Vec<Approval>, LandingError> {
        self.approvals
            .standing(forge, &mut *self.repo, number, head, self.target)
    }
}

struct RepoGit<'a>(&'a mut dyn Repo);

impl ReviewGit for RepoGit<'_> {
    type Error = LandingError;
    fn merge_base(&mut self, left: &str, right: &str) -> Result<Option<String>, LandingError> {
        self.0.merge_base(left, right)
    }
    fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool, LandingError> {
        self.0.is_ancestor(ancestor, descendant)
    }
    fn tree_of(&mut self, commit: &str) -> Result<String, LandingError> {
        self.0.tree_of(commit)
    }
    fn merge_tree(&mut self, left: &str, right: &str) -> Result<Option<String>, LandingError> {
        Ok(match self.0.merge_tree(left, right)? {
            MergeTree::Tree(tree) => Some(tree),
            MergeTree::Conflict => None,
        })
    }
}

/// Prove a graph land head against the project's freshly fetched target,
/// without moving any reference.
pub(crate) fn graph_coverage(
    project: &std::path::Path,
    reviewed: Reviewed<'_>,
    head: &str,
) -> Result<review_cover::Coverage, LandingError> {
    let branch = super::adapters::GhForge::new(project).target_branch()?;
    let mut repo = super::adapters::GitRepo::new(project)?;
    let target = repo.fetch_target_untracked(&branch)?;
    review_cover::coverage(&mut RepoGit(&mut repo), reviewed, head, &target)
}
