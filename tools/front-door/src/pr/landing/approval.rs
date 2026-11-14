//! What stands as a pull request's landing approval, decided once for single
//! and batch landing: by tier, an approving forge review at the head or a
//! Provenant review record whose review covers the head, a change after it
//! that touches no production code, or a review of the delta since.
use super::{
    LandingError,
    model::Accepted,
    ports::{Approval, Forge, MergeTree, Repo, ReviewRecords, Reviews},
};
use provenant_cli::domains::{ApprovalSource, ApprovalTier};
use provenant_confinement::review_exempt::ReviewExempt;
use provenant_core::review_cover::{self, Coverage, ReviewGit, Reviewed};
use std::collections::BTreeMap;

/// The approval source and tier of one landing run.
#[derive(Clone, Copy)]
pub struct Approvals<'a> {
    source: ApprovalSource,
    records: &'a dyn ReviewRecords,
    tier: ApprovalTier,
    exempt: Option<&'a ReviewExempt>,
    /// The reason the chair accepts a delta no review covers.
    accept_delta: Option<&'a str>,
    /// Per-member review proofs a landing request carried.
    proofs: Option<&'a BTreeMap<u64, Proof>>,
}

/// The review proof one landing member carries beyond the landing policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proof {
    /// The caller proved review coverage of the pinned head, as a graph does.
    Proved(String),
    /// The chair accepts a delta no review covers, for this reason.
    Accepted(String),
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
    /// Paths changed after the review, none of them production code.
    pub carried: Vec<String>,
    pub accepted: Option<Accepted>,
}

impl Approved {
    fn by(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            carried: Vec::new(),
            accepted: None,
        }
    }
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
        Self {
            source,
            records,
            tier: ApprovalTier::Auto,
            exempt: None,
            accept_delta: None,
            proofs: None,
        }
    }

    /// Carry each member's own review proof.
    pub fn with_proofs<'b>(self, proofs: &'b BTreeMap<u64, Proof>) -> Approvals<'b>
    where
        'a: 'b,
    {
        Approvals {
            source: self.source,
            records: self.records,
            tier: self.tier,
            exempt: self.exempt,
            accept_delta: self.accept_delta,
            proofs: Some(proofs),
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
            source: ApprovalSource::Forge,
            records: &NoRecords,
            tier: ApprovalTier::Carry,
            exempt: None,
            accept_delta: None,
            proofs: None,
        }
    }

    /// Whether the run must read the target to decide an approval.
    pub fn needs_target(&self) -> bool {
        self.source == ApprovalSource::Receipt || self.tier == ApprovalTier::Auto
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

    /// Approvals that stand for `head` of pull request `number`. A review
    /// record is proved against `target`, the fetched target commit, so a
    /// merge-in from it or a rebase with an unchanged delta keeps the review.
    /// Under `carry` a change after the review that touches no production code
    /// keeps it too, and under `carry` and `delta` a review of the delta since
    /// stands in for a review of the whole. A record that cannot be proved, or
    /// one of a retired shape, approves nothing and is named in
    /// [`Standing::unusable`]; it never fails the run.
    pub fn standing(
        &self,
        forge: &mut dyn Forge,
        repo: &mut dyn Repo,
        number: u64,
        head: &str,
        target: &str,
        tier: ApprovalTier,
    ) -> Result<Standing, LandingError> {
        if self.source == ApprovalSource::Forge {
            return Ok(Standing {
                approvals: forge.approvals(number, head)?,
                ..Standing::default()
            });
        }
        let Reviews {
            records,
            mut unusable,
        } = self.records.reviews(number)?;
        let mut standing = Standing::default();
        let cover = |repo: &mut dyn Repo, record: &super::ports::ReviewRecord, at: &str| {
            let reviewed = Reviewed {
                head: &record.head,
                base: Some(&record.base),
            };
            review_cover::coverage(&mut RepoGit(repo), reviewed, at, target)
        };
        for record in &records {
            let approval = Approval {
                reference: record.reference.clone(),
                reviewer: record.reviewer.clone(),
            };
            match cover(repo, record, head) {
                Ok(Coverage::DeltaChanged) if tier == ApprovalTier::Carry => {
                    match self.changed_after(repo, record, head, target) {
                        Ok(paths) if self.non_production(&paths) => {
                            standing.carried.insert(record.reference.clone(), paths);
                            standing.approvals.push(approval);
                        }
                        Ok(_) => {}
                        Err(error) => {
                            unusable.push(format!("unprovable: {}: {error}", record.reference));
                        }
                    }
                }
                Ok(coverage) if coverage.covers() => standing.approvals.push(approval),
                Ok(_) => {}
                Err(error) => unusable.push(format!("unprovable: {}: {error}", record.reference)),
            }
        }
        // A review of the head against the head a covering review read.
        if tier != ApprovalTier::None {
            for record in records.iter().filter(|record| record.head == head) {
                for prior in records.iter().filter(|prior| prior.head == record.base) {
                    match cover(repo, prior, &prior.head) {
                        Ok(coverage)
                            if coverage.covers() && !standing.includes(&record.reference) =>
                        {
                            standing.approvals.push(Approval {
                                reference: record.reference.clone(),
                                reviewer: record.reviewer.clone(),
                            });
                        }
                        Ok(_) => {}
                        Err(error) => {
                            unusable.push(format!("unprovable: {}: {error}", prior.reference));
                        }
                    }
                }
            }
        }
        standing.unusable = unusable;
        Ok(standing)
    }

    /// The paths `head` changes on top of the reviewed delta replayed on its base.
    fn changed_after(
        &self,
        repo: &mut dyn Repo,
        record: &super::ports::ReviewRecord,
        head: &str,
        target: &str,
    ) -> Result<Vec<String>, LandingError> {
        let Some(base) = repo.merge_base(head, target)? else {
            return Ok(Vec::new());
        };
        let Some(replayed) = RepoGit(repo).merge_tree(&record.head, &base)? else {
            return Ok(Vec::new());
        };
        let tree = repo.tree_of(head)?;
        repo.changed_paths(&replayed, &tree)
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
    /// For an approval carried over a change after the review, the changed paths.
    pub carried: BTreeMap<String, Vec<String>>,
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
    /// Decide whether `head` of pull request `number`, by `author`, may land.
    pub fn decide(
        &mut self,
        forge: &mut dyn Forge,
        number: u64,
        head: &str,
        author: &str,
    ) -> Result<Decision, LandingError> {
        let approvals = self.approvals;
        let proof = approvals.proofs.and_then(|proofs| proofs.get(&number));
        if let Some(Proof::Proved(reference)) = proof {
            return Ok(Decision::Approved(Approved::by(reference.clone())));
        }
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
        let standing =
            approvals.standing(forge, &mut *self.repo, number, head, self.target, tier)?;
        if let Some(approval) = standing.approvals.iter().find(|approval| {
            !approval.reference.is_empty() && approvals.independent(approval, author)
        }) {
            return Ok(Decision::Approved(Approved {
                reference: approval.reference.clone(),
                carried: standing
                    .carried
                    .get(&approval.reference)
                    .cloned()
                    .unwrap_or_default(),
                accepted: None,
            }));
        }
        let accepted = match proof {
            Some(Proof::Accepted(reason)) => Some(reason.as_str()),
            _ => approvals.accept_delta,
        };
        if let Some(reason) = accepted {
            return Ok(Decision::Approved(Approved {
                accepted: Some(Accepted {
                    reason: reason.to_owned(),
                    actor: forge.actor()?,
                }),
                ..Approved::by("accepted-delta")
            }));
        }
        Ok(Decision::Refused(
            standing.exclusion("no independent approval for the current head"),
        ))
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

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use crate::pr::landing::{
        candidate::select_batch_with_reasons,
        fakes::{FakeForge, FakeRepo, FakeReviews},
        ports::{PullRequestInfo, PullRequestState, ReviewRecord},
    };

    const H: &str = "1111111111111111111111111111111111111111";
    const R: &str = "2222222222222222222222222222222222222222";
    const B: &str = "3333333333333333333333333333333333333333";
    const T: &str = "4444444444444444444444444444444444444444";
    const OTHER: &str = "5555555555555555555555555555555555555555";
    const X: &str = "6666666666666666666666666666666666666666";

    fn record(reference: &str, head: &str, base: &str) -> ReviewRecord {
        ReviewRecord {
            reference: reference.into(),
            reviewer: "reviewer".into(),
            head: head.into(),
            base: base.into(),
        }
    }

    /// Main is `t` on `b`. The pull request was reviewed at `r` against `b`
    /// and is now at `h`, one commit later, which changes `changed`.
    fn repo(changed: &[&str]) -> FakeRepo {
        let mut repo = FakeRepo::default();
        for (commit, parent) in [(T, B), (R, B), (H, R)] {
            repo.parents.insert(commit.into(), vec![parent.into()]);
        }
        repo.trees.insert(H.into(), format!("tree-{H}"));
        repo.paths.insert(
            (format!("tree-{R}-{B}"), format!("tree-{H}")),
            changed.iter().map(|path| (*path).to_owned()).collect(),
        );
        repo
    }

    fn decide(
        repo: &mut FakeRepo,
        records: &[ReviewRecord],
        tier: ApprovalTier,
        accept: Option<&str>,
    ) -> Decision {
        let reviews = FakeReviews::of(BTreeMap::from([(7, records.to_vec())]));
        let exempt = ReviewExempt::none();
        Approvals::new(ApprovalSource::Receipt, &reviews)
            .governed(tier, &exempt, accept)
            .at(repo, T)
            .decide(&mut FakeForge::default(), 7, H, "author")
            .unwrap()
    }

    fn refused(decision: &Decision) -> bool {
        matches!(decision, Decision::Refused(_))
    }

    #[test]
    fn a_change_to_tests_or_docs_after_review_keeps_the_approval_and_lists_its_paths() {
        let reviewed = [record("d1", R, B)];
        let paths = ["docs/x.md", "src/tests/a.rs"];
        let Decision::Approved(approved) =
            decide(&mut repo(&paths), &reviewed, ApprovalTier::Carry, None)
        else {
            panic!("a test and doc change should carry the review");
        };
        assert_eq!(approved.reference, "d1");
        assert_eq!(approved.carried, paths);
        // Production code, and a tier that reviews every change, do not carry.
        let production = ["src/a.rs", "docs/x.md"];
        assert!(refused(&decide(
            &mut repo(&production),
            &reviewed,
            ApprovalTier::Carry,
            None
        )));
        assert!(refused(&decide(
            &mut repo(&paths),
            &reviewed,
            ApprovalTier::Delta,
            None
        )));
    }

    #[test]
    fn a_production_change_after_review_needs_a_review_of_the_delta() {
        let (full, delta) = (record("d1", R, B), record("d2", H, R));
        let mut repo = repo(&["src/a.rs"]);
        assert!(refused(&decide(
            &mut repo,
            std::slice::from_ref(&full),
            ApprovalTier::Carry,
            None
        )));
        // A review of the new head against a head no review covers is not one.
        let stray = record("d3", H, X);
        assert!(refused(&decide(
            &mut repo,
            &[full.clone(), stray],
            ApprovalTier::Carry,
            None
        )));
        for tier in [ApprovalTier::Carry, ApprovalTier::Delta] {
            let decision = decide(&mut repo, &[full.clone(), delta.clone()], tier, None);
            assert!(
                matches!(&decision, Decision::Approved(a) if a.reference == "d2"),
                "{decision:?}"
            );
        }
    }

    #[test]
    fn accept_delta_lands_the_change_and_records_the_reason_and_actor() {
        let reviewed = [record("d1", R, B)];
        let mut repo = repo(&["src/a.rs"]);
        let Decision::Approved(approved) =
            decide(&mut repo, &reviewed, ApprovalTier::Carry, Some("typo"))
        else {
            panic!("the override should land the change");
        };
        assert_eq!(
            approved.accepted,
            Some(Accepted {
                reason: "typo".into(),
                actor: "chair".into()
            })
        );
    }

    #[test]
    fn single_and_batch_landing_decide_alike() {
        let info = |number: u64, head: &str| PullRequestInfo {
            number,
            head: head.into(),
            base_branch: "main".into(),
            author: "author".into(),
            state: PullRequestState::Open,
            merged_commit: None,
            stack: None,
        };
        let full = record("d1", R, B);
        for records in [vec![full.clone()], vec![full, record("d2", H, R)]] {
            let mut forge = FakeForge::default();
            forge.pull_requests.insert(7, info(7, H));
            forge.pull_requests.insert(8, info(8, OTHER));
            let mut repo = repo(&["src/a.rs"]);
            let alone = decide(&mut repo, &records, ApprovalTier::Carry, None);
            let reviews = FakeReviews::of(BTreeMap::from([(7, records)]));
            let exempt = ReviewExempt::none();
            let mut approver = Approvals::new(ApprovalSource::Receipt, &reviews)
                .governed(ApprovalTier::Carry, &exempt, None)
                .at(&mut repo, T);
            let (selected, reasons) = select_batch_with_reasons(
                &mut forge,
                &mut approver,
                &[8, 7],
                &[8, 7],
                "main",
                2,
                0,
            )
            .unwrap();
            match alone {
                Decision::Approved(approved) => {
                    assert_eq!(selected.len(), 1);
                    assert_eq!(selected[0].approval, approved.reference);
                }
                Decision::Refused(reason) => assert_eq!(reasons[&7], reason),
            }
        }
    }
}
