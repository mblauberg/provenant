//! What stands as a pull request's landing approval, decided once for single
//! and batch landing: configured independent review sources on the exact head.
use super::{
    LandingError,
    model::{Accepted, ApprovalEvidence},
    ports::{Forge, MergeTree, Repo, ReviewRecords},
};
use provenant_cli::domains::{ApprovalSource, ApprovalTier};
use provenant_confinement::review_exempt::ReviewExempt;
use provenant_core::review_cover::{self, ReviewGit, Reviewed};
use std::collections::BTreeMap;

/// The approval source and tier of one landing run.
#[derive(Clone, Copy)]
pub struct Approvals<'a> {
    sources: &'a [ApprovalSource],
    records: &'a dyn ReviewRecords,
    tier: ApprovalTier,
    exempt: Option<&'a ReviewExempt>,
    /// The reason the chair accepts a delta no review covers.
    accept_delta: Option<&'a str>,
    /// Explicit per-member owner acceptances carried by a landing request.
    acceptances: Option<&'a BTreeMap<u64, String>>,
}

/// What lets a pull request land at its head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Approved(Approved),
    Refused(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approved {
    pub reference: String,
    pub evidence: Vec<ApprovalEvidence>,
    pub accepted: Option<Accepted>,
}

impl Approved {
    fn by(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            evidence: Vec::new(),
            accepted: None,
        }
    }
}

#[cfg(test)]
struct NoRecords;
#[cfg(test)]
impl ReviewRecords for NoRecords {
    fn reviews(&self, _number: u64, _head: &str) -> Result<super::ports::Reviews, LandingError> {
        Ok(super::ports::Reviews::default())
    }
}

impl<'a> Approvals<'a> {
    pub fn new(sources: &'a [ApprovalSource], records: &'a dyn ReviewRecords) -> Self {
        Self {
            sources,
            records,
            tier: ApprovalTier::Auto,
            exempt: None,
            accept_delta: None,
            acceptances: None,
        }
    }

    /// Carry each member's explicit owner acceptance.
    pub fn with_acceptances<'b>(self, acceptances: &'b BTreeMap<u64, String>) -> Approvals<'b>
    where
        'a: 'b,
    {
        Approvals {
            sources: self.sources,
            records: self.records,
            tier: self.tier,
            exempt: self.exempt,
            accept_delta: self.accept_delta,
            acceptances: Some(acceptances),
        }
    }

    /// Apply the landing tier, the review-exempt paths and the chair's override.
    pub fn governed(
        self,
        tier: ApprovalTier,
        exempt: &'a ReviewExempt,
        accept_delta: Option<&'a str>,
    ) -> Self {
        Self {
            tier,
            exempt: Some(exempt),
            accept_delta,
            ..self
        }
    }

    /// Approving forge reviews only.
    #[cfg(test)]
    pub fn forge() -> Approvals<'static> {
        Approvals {
            sources: &[ApprovalSource::Forge],
            records: &NoRecords,
            tier: ApprovalTier::Carry,
            exempt: None,
            accept_delta: None,
            acceptances: None,
        }
    }

    /// Whether the run must read the target to decide an approval.
    pub fn needs_target(&self) -> bool {
        self.tier == ApprovalTier::Auto
    }

    fn production(&self, path: &str) -> bool {
        !(self
            .exempt
            .is_some_and(|exempt| exempt.covers(path.as_bytes()))
            || test_or_doc(path))
    }

    /// Whether `paths` is a change to no production code.
    fn non_production(&self, paths: &[String]) -> bool {
        !paths.is_empty() && paths.iter().all(|path| !self.production(path))
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
    /// Decide whether `head` of pull request `number`, by `author`, may land.
    pub fn decide(
        &mut self,
        forge: &mut dyn Forge,
        number: u64,
        head: &str,
        author: &str,
    ) -> Result<Decision, LandingError> {
        let approvals = self.approvals;
        let tier = match approvals.tier {
            ApprovalTier::Auto => {
                let paths = match self.repo.merge_base(head, self.target)? {
                    Some(base) => self.repo.changed_paths(&base, head)?,
                    None => Vec::new(),
                };
                if approvals.non_production(&paths) {
                    ApprovalTier::None
                } else {
                    ApprovalTier::Carry
                }
            }
            tier => tier,
        };
        if tier == ApprovalTier::None {
            return Ok(Decision::Approved(Approved::by("ci-only")));
        }
        let mut unusable = Vec::new();
        let mut failures = Vec::new();
        let mut evidence_set = Vec::new();
        for source in approvals.sources {
            let candidates = match source {
                ApprovalSource::ProvenantReview => match approvals.records.reviews(number, head) {
                    Ok(reviews) => {
                        unusable.extend(reviews.unusable);
                        reviews
                            .records
                            .into_iter()
                            .filter(|record| {
                                record.head == head
                                    && record.independent
                                    && matches!(record.verdict,
                                    provenant_records::workflow::ReviewVerdict::Approve
                                    | provenant_records::workflow::ReviewVerdict::ApproveWithNits)
                            })
                            .map(|record| ApprovalEvidence {
                                source: *source,
                                reference: record.reference,
                                reviewer: record.reviewer,
                                head: record.head,
                            })
                            .collect::<Vec<_>>()
                    }
                    Err(error) => {
                        failures.push(error.to_string());
                        Vec::new()
                    }
                },
                ApprovalSource::Forge => match forge.approvals(number, head) {
                    Ok(records) => records
                        .into_iter()
                        .filter(|record| {
                            !record.reviewer.is_empty()
                                && !record.reviewer.eq_ignore_ascii_case(author)
                        })
                        .map(|record| ApprovalEvidence {
                            source: *source,
                            reference: record.reference,
                            reviewer: record.reviewer,
                            head: head.to_owned(),
                        })
                        .collect(),
                    Err(error) => {
                        failures.push(error.to_string());
                        Vec::new()
                    }
                },
            };
            for evidence in candidates
                .into_iter()
                .filter(|record| !record.reference.is_empty() && !record.reviewer.is_empty())
            {
                if !evidence_set.contains(&evidence) {
                    evidence_set.push(evidence);
                }
            }
        }
        if let Some(first) = evidence_set.first() {
            return Ok(Decision::Approved(Approved {
                reference: first.reference.clone(),
                evidence: evidence_set,
                accepted: None,
            }));
        }
        let accepted = approvals
            .acceptances
            .and_then(|accepted| accepted.get(&number))
            .map(String::as_str)
            .or(approvals.accept_delta);
        if let Some(reason) = accepted {
            return Ok(Decision::Approved(Approved {
                accepted: Some(Accepted {
                    reason: reason.to_owned(),
                    actor: forge.actor()?,
                }),
                ..Approved::by("accepted-delta")
            }));
        }
        if !failures.is_empty() {
            return Err(LandingError::Failed(failures.join("; ")));
        }
        Ok(Decision::Refused(if unusable.is_empty() {
            "no independent approval for the current head".to_owned()
        } else {
            unusable.join("; ")
        }))
    }
}

/// A path that is a test or a doc. Hidden paths, such as CI and agent
/// configuration, are never either.
fn test_or_doc(path: &str) -> bool {
    let Some((name, directories)) = path
        .split('/')
        .collect::<Vec<_>>()
        .split_last()
        .map(|(n, d)| (*n, d.to_vec()))
    else {
        return false;
    };
    if path
        .split('/')
        .any(|part| part.starts_with('.') || part == "..")
    {
        return false;
    }
    directories
        .iter()
        .any(|part| matches!(*part, "tests" | "test" | "docs" | "__tests__"))
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.rs")
        || name == "tests.rs"
        || name.ends_with(".md")
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
    host_state: &std::path::Path,
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
    let mut repo = super::adapters::GitRepo::new(project, host_state)?;
    let target = repo.fetch_target_untracked(&base_branch)?;
    review_cover::coverage(&mut RepoGit(&mut repo), reviewed, head, &target)
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use crate::pr::landing::fakes::{FakeForge, FakeRepo, FakeReviews};

    #[test]
    fn configured_source_order_selects_the_approving_evidence() -> Result<(), LandingError> {
        use crate::pr::landing::ports::{Approval, ReviewRecord};
        use provenant_records::workflow::ReviewVerdict;
        let mut forge = FakeForge::default();
        forge.approvals_at_head.insert(
            (1, "head".into()),
            vec![Approval {
                reference: "forge-review".into(),
                reviewer: "reviewer".into(),
            }],
        );
        let reviews = FakeReviews::of(BTreeMap::from([(
            1,
            vec![ReviewRecord {
                reference: "review-attempt".into(),
                reviewer: "review-execution".into(),
                head: "head".into(),
                verdict: ReviewVerdict::Approve,
                independent: true,
            }],
        )]));
        let mut repo = FakeRepo::default();
        for sources in [
            vec![ApprovalSource::ProvenantReview, ApprovalSource::Forge],
            vec![ApprovalSource::Forge, ApprovalSource::ProvenantReview],
            vec![ApprovalSource::Forge],
        ] {
            let Decision::Approved(approved) = Approvals::new(&sources, &reviews)
                .at(&mut repo, "base")
                .decide(&mut forge, 1, "head", "writer")?
            else {
                return Err(LandingError::Failed(
                    "configured source did not approve".into(),
                ));
            };
            assert!(
                sources
                    .iter()
                    .all(|source| approved.evidence.iter().any(|e| e.source == *source))
            );
        }
        let disabled = Approvals::new(&[], &reviews)
            .at(&mut repo, "base")
            .decide(&mut forge, 1, "head", "writer")?;
        assert!(matches!(disabled, Decision::Refused(_)));
        Ok(())
    }

    #[test]
    fn owner_override_is_explicit_and_records_actor_and_reason() -> Result<(), LandingError> {
        let mut forge = FakeForge::default();
        let mut repo = FakeRepo::default();
        let reviews = FakeReviews::default();
        let exempt = ReviewExempt::none();
        let sources = [ApprovalSource::ProvenantReview, ApprovalSource::Forge];
        let approvals = Approvals::new(&sources, &reviews);
        assert!(matches!(
            approvals
                .at(&mut repo, "base")
                .decide(&mut forge, 1, "head", "writer")?,
            Decision::Refused(_)
        ));
        let decision = approvals
            .governed(ApprovalTier::Carry, &exempt, Some("owner accepts delta"))
            .at(&mut repo, "base")
            .decide(&mut forge, 1, "head", "writer")?;
        let Decision::Approved(approved) = decision else {
            return Err(LandingError::Failed(
                "explicit override did not approve".into(),
            ));
        };
        let accepted = approved
            .accepted
            .ok_or_else(|| LandingError::Failed("missing owner evidence".into()))?;
        assert_eq!(accepted.actor, "chair");
        assert_eq!(accepted.reason, "owner accepts delta");
        assert!(approved.evidence.is_empty());
        Ok(())
    }
}
