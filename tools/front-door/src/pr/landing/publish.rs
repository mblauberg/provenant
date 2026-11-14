use super::{
    LandingError,
    approval::{Approvals, Approver, Decision},
    model::*,
    ports::{Forge, Repo},
};
use provenant_cli::domains::{MergeMethod, Publication};
/// Choose the configured publication mode, checking direct-push authority for auto.
pub fn decide(
    forge: &mut dyn Forge,
    target: &str,
    requested: Publication,
    method: MergeMethod,
) -> Result<PublishDecision, LandingError> {
    match requested {
        Publication::RestOrdered => Ok(PublishDecision {
            mode: PublishMode::RestOrdered,
            reason: "land.publish explicitly selects rest_ordered".into(),
        }),
        Publication::FastForward => {
            if method != MergeMethod::Merge {
                return Err(LandingError::Failed(
                    "fast_forward requires the merge method".into(),
                ));
            }
            if !forge.may_push_directly(target)? {
                return Err(LandingError::Failed(format!(
                    "land.publish=fast_forward requires direct push permission for {target}"
                )));
            }
            Ok(PublishDecision {
                mode: PublishMode::FastForward,
                reason: "land.publish explicitly selects fast_forward and direct push is permitted"
                    .into(),
            })
        }
        Publication::Auto => {
            if method != MergeMethod::Merge {
                return Ok(PublishDecision {
                    mode: PublishMode::RestOrdered,
                    reason: "the requested merge method requires forge publication".into(),
                });
            }
            if forge.may_push_directly(target)? {
                Ok(PublishDecision {
                    mode: PublishMode::FastForward,
                    reason: "direct push is permitted".into(),
                })
            } else {
                Ok(PublishDecision {
                    mode: PublishMode::RestOrdered,
                    reason: "direct push is not permitted".into(),
                })
            }
        }
    }
}

/// A stack must be published through the forge so it can retarget its upper
/// members. The decision is made before the pending marker is written.
pub fn decide_candidate(
    forge: &mut dyn Forge,
    target: &str,
    requested: Publication,
    method: MergeMethod,
    candidate: &Candidate,
) -> Result<PublishDecision, LandingError> {
    for step in &candidate.steps {
        if forge.pull_request(step.pr.number)?.stack.is_some() {
            return Ok(PublishDecision {
                mode: PublishMode::RestOrdered,
                reason: "stack members require asynchronous forge publication".into(),
            });
        }
    }
    decide(forge, target, requested, method)
}
/// Publication engine is selected once, from the shared strategy declaration.
pub fn decide_strategy(
    forge: &mut dyn Forge,
    target: &str,
    settings: &provenant_cli::domains::LandSettings,
    candidate: &Candidate,
) -> Result<PublishDecision, LandingError> {
    match settings.strategy.declaration().execution {
        provenant_records::landing::Execution::Command => {
            if !settings
                .command
                .as_ref()
                .is_some_and(|command| command.valid())
            {
                return Err(LandingError::Failed(
                    "command strategy requires a valid land.command declaration".into(),
                ));
            }
            Ok(PublishDecision {
                mode: PublishMode::Command,
                reason: "land.strategy selects the repository command".into(),
            })
        }
        provenant_records::landing::Execution::Builtin => {
            decide_candidate(forge, target, settings.publish, settings.method, candidate)
        }
        provenant_records::landing::Execution::Unavailable => Err(LandingError::Unimplemented(
            "strategy execution follows in 2.1",
        )),
    }
}

/// Return PRs whose pinned head or approval changed immediately before publication.
pub fn revalidate(
    forge: &mut dyn Forge,
    approver: &mut Approver<'_, '_>,
    candidate: &Candidate,
) -> Result<Vec<EligiblePr>, LandingError> {
    Ok(
        revalidate_with_reasons(forge, approver, &mut candidate.clone())?
            .into_iter()
            .map(|(pr, _)| pr)
            .collect(),
    )
}

pub fn revalidate_with_reasons(
    forge: &mut dyn Forge,
    approver: &mut Approver<'_, '_>,
    candidate: &mut Candidate,
) -> Result<Vec<(EligiblePr, String)>, LandingError> {
    let mut excluded = Vec::new();
    for step in &mut candidate.steps {
        let pr = &mut step.pr;
        let head = forge.head(pr.number)?;
        let info = forge.pull_request(pr.number)?;
        let reason = if info.base_branch != pr.base_branch {
            Some("base branch changed".to_owned())
        } else if head != pr.head {
            Some("head changed".to_owned())
        } else {
            match approver.decide(forge, pr.number, &pr.head, &info.author)? {
                Decision::Approved(approved) => {
                    pr.extend_approval_evidence(approved.evidence);
                    None
                }
                Decision::Refused(reason) => Some(reason),
            }
        };
        if let Some(reason) = reason {
            excluded.push((pr.clone(), reason));
        }
    }
    Ok(excluded)
}

/// Only request the lowest unmerged stack member: an async request for an
/// upper member would also merge its downstack members without their gates.
pub fn merge_member(
    forge: &mut dyn Forge,
    number: u64,
    head: &str,
    method: MergeMethod,
) -> Result<String, LandingError> {
    if let Some(stack) = forge.pull_request(number)?.stack {
        let members = forge.stack_members(stack.number)?;
        let position = members
            .iter()
            .position(|member| member.number == number)
            .ok_or(LandingError::PublicationUnknown)?;
        if let Some(lower) = members[..position]
            .iter()
            .find(|member| member.state != super::ports::PullRequestState::Merged)
        {
            return Err(LandingError::Refused(format!(
                "pull request #{number} has unmerged lower pull request #{}",
                lower.number
            )));
        }
        forge.merge_stack(number, head, method)
    } else {
        forge.merge(number, head, method)
    }
}
/// Advance the target from the candidate base with an expected-old-value push.
pub fn fast_forward(
    repo: &mut dyn Repo,
    target: &str,
    candidate: &Candidate,
) -> Result<PublishResult, LandingError> {
    if !repo
        .is_ancestor(&candidate.base, &candidate.commit)
        .map_err(|error| {
            LandingError::Refused(format!("could not verify candidate ancestry: {error}"))
        })?
    {
        return Err(LandingError::Refused(
            "candidate does not descend from its base".into(),
        ));
    }
    repo.push_ref(target, &candidate.commit, Some(&candidate.base))?;
    Ok(PublishResult {
        landed: candidate
            .steps
            .iter()
            .map(|step| step.commit.clone())
            .collect(),
        alarms: Vec::new(),
    })
}
/// Merge in order at pinned heads and record an alarm if the final tree differs from the gated tree.
#[allow(
    clippy::too_many_arguments,
    reason = "ordered publication needs its boundaries"
)]
pub fn rest_ordered(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    approvals: Approvals<'_>,
    target: &str,
    candidate: &Candidate,
    method: MergeMethod,
    landed: &mut Vec<String>,
    before_dispatch: &mut dyn FnMut(u64) -> Result<(), LandingError>,
) -> Result<PublishResult, LandingError> {
    let mut expected = candidate.base.clone();
    for step in &candidate.steps {
        // The forge API cannot atomically bind the target; movement between this read
        // and the merge remains possible and the final tree check detects it.
        if repo.fetch_target(target).map_err(|error| {
            LandingError::Refused(format!("could not read target before merge: {error}"))
        })? != expected
        {
            return Err(LandingError::TargetMoved);
        }
        if !revalidate(
            forge,
            &mut approvals.at(repo, &candidate.base),
            &Candidate {
                steps: vec![step.clone()],
                ..candidate.clone()
            },
        )
        .map_err(|error| {
            LandingError::Refused(format!("could not revalidate before merge: {error}"))
        })?
        .is_empty()
        {
            return Err(LandingError::PullRequestChanged(step.pr.number));
        }
        before_dispatch(step.pr.number)?;
        expected = merge_member(forge, step.pr.number, &step.pr.head, method)?;
        landed.push(expected.clone());
    }
    // The final landed commit is durable evidence for the later tree comparison.
    Ok(PublishResult {
        landed: landed.clone(),
        alarms: Vec::new(),
    })
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use crate::pr::landing::{
        fakes::{FakeForge, FakeRepo},
        ports::Approval,
    };

    fn candidate() -> Candidate {
        let step = |number, head: &str, commit: &str| CandidateStep {
            pr: EligiblePr {
                number,
                head: head.into(),
                approval: format!("approval-{number}"),
                approval_evidence: Vec::new(),
                accepted: None,
                base_branch: "main".into(),
            },
            commit: commit.into(),
            tree: format!("tree-{number}"),
        };
        Candidate {
            base: "base".into(),
            steps: vec![step(1, "head-1", "step-1"), step(2, "head-2", "candidate")],
            commit: "candidate".into(),
            tree: "tree-2".into(),
            ref_name: "land/test".into(),
        }
    }
    fn prepare(forge: &mut FakeForge) {
        use crate::pr::landing::ports::{PullRequestInfo, PullRequestState};
        for step in candidate().steps {
            let pr = step.pr;
            forge.heads.insert(pr.number, pr.head.clone());
            forge.pull_requests.insert(
                pr.number,
                PullRequestInfo {
                    number: pr.number,
                    head: pr.head.clone(),
                    base_branch: pr.base_branch,
                    author: "author".into(),
                    state: PullRequestState::Open,
                    merged_commit: None,
                    stack: None,
                },
            );
            forge.approvals_at_head.insert(
                (pr.number, pr.head),
                vec![Approval {
                    reference: pr.approval,
                    reviewer: "reviewer".into(),
                }],
            );
        }
    }
    #[test]
    fn stack_publication_uses_async_only_for_the_lowest_member() -> Result<(), LandingError> {
        use crate::pr::landing::ports::{PullRequestState, StackMember, StackMembership};
        let mut forge = FakeForge {
            direct_push: true,
            ..Default::default()
        };
        prepare(&mut forge);
        for (number, position) in [(1, 1), (2, 2)] {
            forge.pull_requests.get_mut(&number).unwrap().stack = Some(StackMembership {
                number: 42,
                size: 2,
                position,
                base_branch: "main".into(),
            });
        }
        forge.pull_requests.get_mut(&2).unwrap().base_branch = "lower".into();
        forge.stacks.insert(
            42,
            vec![
                StackMember {
                    number: 1,
                    state: PullRequestState::Open,
                },
                StackMember {
                    number: 2,
                    state: PullRequestState::Open,
                },
            ],
        );
        assert_eq!(
            decide_candidate(
                &mut forge,
                "main",
                Publication::Auto,
                MergeMethod::Merge,
                &candidate()
            )?
            .mode,
            PublishMode::RestOrdered
        );
        let error = merge_member(&mut forge, 2, "head-2", MergeMethod::Merge).unwrap_err();
        assert!(error.to_string().contains("#1"));
        assert!(forge.merge_attempts.is_empty());
        assert_eq!(
            merge_member(&mut forge, 1, "head-1", MergeMethod::Squash)?,
            "landed-1"
        );
        assert_eq!(forge.stack_merge_attempts, vec![1]);
        assert_eq!(forge.merges[0].2, MergeMethod::Squash);
        assert_eq!(forge.pull_requests[&2].base_branch, "main");
        forge.heads.insert(2, "moved".into());
        assert!(merge_member(&mut forge, 2, "head-2", MergeMethod::Merge).is_err());
        assert_eq!(forge.merges.len(), 1);
        Ok(())
    }
    #[test]
    fn publication_decisions_respect_permission_and_explicit_mode() -> Result<(), LandingError> {
        let mut forge = FakeForge::default();
        let decision = decide(&mut forge, "main", Publication::Auto, MergeMethod::Merge)?;
        assert_eq!(decision.mode, PublishMode::RestOrdered);
        assert!(decision.reason.contains("not permitted"));
        assert!(
            decide(
                &mut forge,
                "main",
                Publication::FastForward,
                MergeMethod::Merge
            )
            .is_err()
        );
        forge.direct_push = true;
        assert_eq!(
            decide(&mut forge, "main", Publication::Auto, MergeMethod::Merge)?.mode,
            PublishMode::FastForward
        );
        assert_eq!(
            decide(&mut forge, "main", Publication::Auto, MergeMethod::Squash)?.mode,
            PublishMode::RestOrdered
        );
        assert!(
            decide(
                &mut forge,
                "main",
                Publication::FastForward,
                MergeMethod::Rebase
            )
            .is_err()
        );
        assert_eq!(
            decide(
                &mut forge,
                "main",
                Publication::RestOrdered,
                MergeMethod::Merge
            )?
            .mode,
            PublishMode::RestOrdered
        );
        Ok(())
    }
    #[test]
    fn approval_evidence_unions_across_source_and_arrival_orders() -> Result<(), LandingError> {
        use crate::pr::landing::{fakes::FakeReviews, ports::ReviewRecord};
        use provenant_cli::domains::ApprovalSource;
        for sources in [
            vec![ApprovalSource::ProvenantReview, ApprovalSource::Forge],
            vec![ApprovalSource::Forge, ApprovalSource::ProvenantReview],
        ] {
            for review_first in [false, true] {
                let mut forge = FakeForge::default();
                prepare(&mut forge);
                let mut repo = FakeRepo::default();
                let mut reviews = FakeReviews::default();
                let mut c = candidate();
                c.steps.truncate(1);
                let pr = c.steps[0].pr.clone();
                let review = ReviewRecord {
                    reference: "provenant-review".into(),
                    reviewer: "independent".into(),
                    head: pr.head.clone(),
                    verdict: provenant_records::workflow::ReviewVerdict::Approve,
                    independent: true,
                };
                if review_first {
                    forge.approvals_at_head.clear();
                    reviews.records.insert(pr.number, vec![review.clone()]);
                }
                assert!(
                    revalidate_with_reasons(
                        &mut forge,
                        &mut Approvals::new(&sources, &reviews).at(&mut repo, ""),
                        &mut c
                    )?
                    .is_empty()
                );
                let initial = c.steps[0].pr.approval_evidence.clone();
                reviews.records.insert(pr.number, vec![review]);
                prepare(&mut forge);
                assert!(
                    revalidate_with_reasons(
                        &mut forge,
                        &mut Approvals::new(&sources, &reviews).at(&mut repo, ""),
                        &mut c
                    )?
                    .is_empty()
                );
                let evidence = &c.steps[0].pr.approval_evidence;
                assert!(sources.iter().all(|source| {
                    evidence
                        .iter()
                        .any(|proof| proof.source == *source && proof.head == pr.head)
                }));
                assert!(initial.iter().all(|proof| evidence.contains(proof)));
            }
        }
        Ok(())
    }

    #[test]
    fn revalidation_excludes_changed_head_and_lost_approval() -> Result<(), LandingError> {
        let mut forge = FakeForge::default();
        prepare(&mut forge);
        forge.approvals_at_head.clear();
        forge.heads.insert(1, "changed".into());
        forge.heads.insert(2, "head-2".into());
        let c = candidate();
        assert_eq!(
            revalidate(
                &mut forge,
                &mut Approvals::forge().at(&mut FakeRepo::default(), ""),
                &c
            )?,
            vec![c.steps[0].pr.clone(), c.steps[1].pr.clone()]
        );
        forge.heads.insert(1, "head-1".into());
        for step in &c.steps {
            forge.approvals_at_head.insert(
                (step.pr.number, step.pr.head.clone()),
                vec![Approval {
                    reference: step.pr.approval.clone(),
                    reviewer: "reviewer".into(),
                }],
            );
        }
        assert!(
            revalidate(
                &mut forge,
                &mut Approvals::forge().at(&mut FakeRepo::default(), ""),
                &c
            )?
            .is_empty()
        );
        forge
            .pull_requests
            .get_mut(&1)
            .expect("fixture")
            .base_branch = "other".into();
        assert_eq!(
            revalidate(
                &mut forge,
                &mut Approvals::forge().at(&mut FakeRepo::default(), ""),
                &c
            )?,
            vec![c.steps[0].pr.clone()]
        );
        Ok(())
    }
    /// A review on an older head withdraws its pull request from publication;
    /// the exact-head approval stays.
    #[test]
    fn revalidation_excludes_a_review_on_an_older_head() -> Result<(), LandingError> {
        use crate::pr::landing::{approval::Approvals, fakes::FakeReviews, ports::ReviewRecord};
        use provenant_cli::domains::ApprovalSource;
        let mut forge = FakeForge::default();
        prepare(&mut forge);
        forge.approvals_at_head.clear();
        let mut repo = FakeRepo::default();
        for (commit, parents) in [
            ("base", vec!["fork"]),
            ("head-1", vec!["fork"]),
            ("reviewed-2", vec!["fork"]),
            // PR 2 merged the target into its reviewed head; its tree is unreadable.
            ("head-2", vec!["reviewed-2", "base"]),
        ] {
            repo.parents
                .insert(commit.into(), parents.into_iter().map(Into::into).collect());
        }
        let record = |number: u64, head: &str| ReviewRecord {
            reference: format!("approval-{number}"),
            reviewer: "reviewer".into(),
            head: head.into(),
            verdict: provenant_records::workflow::ReviewVerdict::Approve,
            independent: true,
        };
        let reviews = FakeReviews::of(std::collections::BTreeMap::from([
            (1, vec![record(1, "head-1")]),
            (2, vec![record(2, "reviewed-2")]),
        ]));
        let c = candidate();
        let excluded = revalidate_with_reasons(
            &mut forge,
            &mut Approvals::new(&[ApprovalSource::ProvenantReview], &reviews).at(&mut repo, "base"),
            &mut c.clone(),
        )?;
        assert_eq!(excluded.len(), 1, "{excluded:?}");
        assert_eq!(excluded[0].0, c.steps[1].pr);
        Ok(())
    }
    #[test]
    fn fast_forward_requires_ancestry_and_expected_base() -> Result<(), LandingError> {
        let mut repo = FakeRepo {
            target: "base".into(),
            ..FakeRepo::default()
        };
        let c = candidate();
        repo.fetch_target("main")?;
        assert!(fast_forward(&mut repo, "main", &c).is_err());
        repo.parents.insert("candidate".into(), vec!["base".into()]);
        let result = fast_forward(&mut repo, "main", &c)?;
        assert_eq!(result.landed, vec!["step-1", "candidate"]);
        assert_eq!(
            repo.pushes[0],
            ("main".into(), "candidate".into(), Some("base".into()))
        );
        assert!(matches!(
            fast_forward(&mut repo, "main", &c),
            Err(LandingError::Refused(_))
        ));
        Ok(())
    }
    fn ordered_revalidation_rejects(change: &str) {
        let mut forge = FakeForge::default();
        prepare(&mut forge);
        match change {
            "head" => {
                forge.heads.insert(2, "new-head".into());
            }
            "approval" => {
                forge.approvals_at_head.remove(&(2, "head-2".into()));
            }
            "base" => {
                forge.pull_requests.get_mut(&2).unwrap().base_branch = "other".into();
            }
            _ => unreachable!(),
        }
        let mut repo = FakeRepo {
            target: "base".into(),
            fetches: ["base".into(), "landed-1".into()].into(),
            ..FakeRepo::default()
        };
        assert!(matches!(
            rest_ordered(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                "main",
                &candidate(),
                MergeMethod::Merge,
                &mut Vec::new(),
                &mut |_| Ok(())
            ),
            Err(LandingError::PullRequestChanged(2))
        ));
        assert_eq!(
            forge.merges.iter().map(|merge| merge.0).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn ordered_revalidates_head() {
        ordered_revalidation_rejects("head");
    }
    #[test]
    fn ordered_revalidates_approval() {
        ordered_revalidation_rejects("approval");
    }
    #[test]
    fn ordered_revalidates_base() {
        ordered_revalidation_rejects("base");
    }

    #[test]
    fn ordered_merge_stops_when_target_moves_between_merges() {
        let mut forge = FakeForge::default();
        prepare(&mut forge);
        let mut repo = FakeRepo {
            target: "base".into(),
            fetches: ["base".into(), "moved".into()].into(),
            ..FakeRepo::default()
        };
        assert!(matches!(
            rest_ordered(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                "main",
                &candidate(),
                MergeMethod::Merge,
                &mut Vec::new(),
                &mut |_| Ok(())
            ),
            Err(LandingError::TargetMoved)
        ));
        assert_eq!(forge.merges.len(), 1);
        assert_eq!(forge.merges[0].0, 1);
    }
    #[test]
    fn ordered_merge_pins_heads_and_records_landed_commits() -> Result<(), LandingError> {
        let mut forge = FakeForge::default();
        prepare(&mut forge);
        let mut repo = FakeRepo {
            target: "base".into(),
            fetches: ["base".into(), "landed-1".into(), "actual".into()].into(),
            ..FakeRepo::default()
        };
        repo.trees.insert("actual".into(), "different".into());
        let result = rest_ordered(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            "main",
            &candidate(),
            MergeMethod::Squash,
            &mut Vec::new(),
            &mut |_| Ok(()),
        )?;
        assert_eq!(
            forge.merges,
            vec![
                (1, "head-1".into(), MergeMethod::Squash),
                (2, "head-2".into(), MergeMethod::Squash)
            ]
        );
        assert_eq!(result.landed, vec!["landed-1", "landed-2"]);
        assert!(result.alarms.is_empty());
        Ok(())
    }
}
