//! What stands as a pull request's landing approval: an approving forge review
//! at the head, or a Provenant review record whose review covers the head.
use super::{
    LandingError,
    ports::{Approval, Forge, MergeTree, Repo, ReviewRecords, Reviews},
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
    fn reviews(&self, _number: u64) -> Result<Reviews, LandingError> {
        Ok(Reviews::default())
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
    /// A record that cannot be proved, or one of a retired shape, approves
    /// nothing and is named in [`Standing::unusable`]; it never fails the run.
    pub fn standing(
        &self,
        forge: &mut dyn Forge,
        repo: &mut dyn Repo,
        number: u64,
        head: &str,
        target: &str,
    ) -> Result<Standing, LandingError> {
        if self.source == ApprovalSource::Forge {
            return Ok(Standing {
                approvals: forge.approvals(number, head)?,
                unusable: Vec::new(),
            });
        }
        let Reviews {
            records,
            mut unusable,
        } = self.records.reviews(number)?;
        let mut approvals = Vec::new();
        for record in records {
            let reviewed = Reviewed {
                head: &record.head,
                base: Some(&record.base),
            };
            match review_cover::coverage(&mut RepoGit(repo), reviewed, head, target) {
                Ok(coverage) if coverage.covers() => approvals.push(Approval {
                    reference: record.reference,
                    reviewer: record.reviewer,
                }),
                Ok(_) => {}
                Err(error) => unusable.push(format!("unprovable: {}: {error}", record.reference)),
            }
        }
        Ok(Standing {
            approvals,
            unusable,
        })
    }

    /// Whether `approval` is independent of the pull request's `author`. A
    /// forge review names its reviewer's login. A receipt names no login: it
    /// was accepted only once its review coverage passed, which already
    /// refuses a reviewer who is an author.
    pub fn independent(&self, approval: &Approval, author: &str) -> bool {
        match self.source {
            ApprovalSource::Forge => {
                !approval.reviewer.is_empty() && !approval.reviewer.eq_ignore_ascii_case(author)
            }
            ApprovalSource::Receipt => true,
        }
    }
}

/// The approvals standing for a head, and why any other record could not.
#[derive(Debug, Default)]
pub struct Standing {
    pub approvals: Vec<Approval>,
    /// Records that might have approved the head but could not be used.
    pub unusable: Vec<String>,
}

impl Standing {
    pub fn includes(&self, reference: &str) -> bool {
        self.approvals
            .iter()
            .any(|approval| approval.reference == reference)
    }

    /// Why no approval stands: `missing`, or the records that could not be used.
    pub fn exclusion(&self, missing: &str) -> String {
        if self.unusable.is_empty() {
            missing.to_owned()
        } else {
            self.unusable.join("; ")
        }
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
    ) -> Result<Standing, LandingError> {
        self.approvals
            .standing(forge, &mut *self.repo, number, head, self.target)
    }

    pub fn independent(&self, approval: &Approval, author: &str) -> bool {
        self.approvals.independent(approval, author)
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

/// Prove a graph land head against a fresh fetch of the branch its pull
/// request merges into, as the forge reports it, without moving any reference.
pub(crate) fn graph_coverage(
    project: &std::path::Path,
    branch: &str,
    reviewed: Reviewed<'_>,
    head: &str,
) -> Result<review_cover::Coverage, LandingError> {
    let oid = head
        .parse::<provenant_core::GitOid>()
        .map_err(|error| LandingError::Failed(error.to_string()))?;
    let base_branch = crate::pr::pull_for_branch(project, branch, oid)
        .map_err(|error| LandingError::Failed(error.to_string()))?
        .base_branch;
    let mut repo = super::adapters::GitRepo::new(project)?;
    let target = repo.fetch_target_untracked(&base_branch)?;
    review_cover::coverage(&mut RepoGit(&mut repo), reviewed, head, &target)
}
