use super::{
    LandingError,
    approval::{Approvals, Approver, Decision},
    model::*,
    ports::{Forge, Mergeability, PullRequestState, Repo},
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
            extra: Default::default(),
        }),
        Publication::FastForward => {
            if method != MergeMethod::Merge {
                return Err(LandingError::Failed(
                    "land.publish=fast_forward requires the merge method; set land.method=merge or land.publish=auto".into(),
                ));
            }
            if !forge.may_push_directly(target)? {
                return Err(LandingError::Failed(format!(
                    "land.publish=fast_forward requires direct push permission for {target}; set land.publish=auto or rest_ordered to publish through a pull request"
                )));
            }
            Ok(PublishDecision {
                mode: PublishMode::FastForward,
                reason: "land.publish explicitly selects fast_forward and direct push is permitted"
                    .into(),
                extra: Default::default(),
            })
        }
        Publication::Auto => {
            if method != MergeMethod::Merge {
                return Ok(PublishDecision {
                    mode: PublishMode::RestOrdered,
                    reason: "the requested merge method requires forge publication".into(),
                    extra: Default::default(),
                });
            }
            if forge.may_push_directly(target)? {
                Ok(PublishDecision {
                    mode: PublishMode::FastForward,
                    reason: "direct push is permitted".into(),
                    extra: Default::default(),
                })
            } else {
                Ok(PublishDecision {
                    mode: PublishMode::RestOrdered,
                    reason: "direct push is not permitted".into(),
                    extra: Default::default(),
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
                extra: Default::default(),
            });
        }
    }
    decide(forge, target, requested, method)
}
/// Backend authority comes only from the effective landing port. Strategy remains
/// the candidate/ordering plan and cannot enable a disabled publication backend.
pub fn decide_strategy(
    forge: &mut dyn Forge,
    target: &str,
    settings: &provenant_cli::domains::LandSettings,
    candidate: &Candidate,
    port: &provenant_config::LandPort,
) -> Result<PublishDecision, LandingError> {
    let (backend, _) = provenant_config::land_backend(port);
    provenant_config::dispatch_port_backend(
        &backend.backend,
        || match settings.strategy.declaration().execution {
            provenant_records::landing::Execution::Builtin => {
                decide_candidate(forge, target, settings.publish, settings.method, candidate)
            }
            provenant_records::landing::Execution::Unavailable => Err(LandingError::Unimplemented(
                "strategy execution follows in 2.1",
            )),
        },
        |_| {
            Ok(PublishDecision {
                mode: PublishMode::Command,
                reason: format!("land command port (source: {:?})", port.source),
                extra: Default::default(),
            })
        },
        || {
            Err(LandingError::PortUnavailable {
                port_source: port.source,
            })
        },
    )
}

/// Return PRs no longer ready or whose pinned head or approval changed before publication.
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
        let info = match super::ready_pull_request(forge, pr.number) {
            Ok(info) => info,
            Err(LandingError::RefusedWith {
                cause: "PR_DRAFT",
                message,
                ..
            }) => {
                excluded.push((pr.clone(), message));
                continue;
            }
            Err(error) => return Err(error),
        };
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
/// `grant` and `reviewed` come from the candidate that already passed land's
/// own gate: an admin retry is never a way around a red gate or a missing
/// independent review.
pub fn merge_member(
    forge: &mut dyn Forge,
    number: u64,
    head: &str,
    method: MergeMethod,
    grant: &PublicationGrant,
    reviewed: bool,
) -> Result<String, LandingError> {
    merge_member_with_queue_policy(forge, number, head, method, grant, reviewed, None)
}

/// Repeat the queue's exact-head checks when recovering an uncertain dispatch.
pub(super) fn merge_delivery_member(
    forge: &mut dyn Forge,
    pr: &EligiblePr,
    method: MergeMethod,
    grant: &PublicationGrant,
    reviewed: bool,
) -> Result<String, LandingError> {
    super::gate::require_delivery_queue_ci(forge, pr)?;
    merge_member_with_queue_policy(
        forge,
        pr.number,
        &pr.head,
        method,
        grant,
        reviewed,
        Some(pr),
    )
}

fn merge_member_with_queue_policy(
    forge: &mut dyn Forge,
    number: u64,
    head: &str,
    method: MergeMethod,
    grant: &PublicationGrant,
    reviewed: bool,
    queue_pr: Option<&EligiblePr>,
) -> Result<String, LandingError> {
    let stack = forge.pull_request(number)?.stack;
    if let Some(stack) = &stack {
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
    }
    if let Some(pr) = queue_pr {
        require_queue_mergeability(forge, pr)?;
    }
    let result = if stack.is_some() {
        forge.merge_stack(number, head, method)
    } else {
        forge.merge(number, head, method)
    };
    match result {
        Err(LandingError::RefusedWith {
            cause: "REVIEW_REQUIRED",
            ..
        }) if reviewed && grant.allows_review_unavailable() => {
            // The only 405 this grant may answer. The gate has passed, the
            // member carries independent review evidence, and the owner gave
            // host-layer permission; the admin call still uses their authority.
            if let Some(pr) = queue_pr {
                super::gate::require_delivery_queue_ci(forge, pr)?;
                require_queue_mergeability(forge, pr)?;
            }
            forge.merge_admin(number, head, method)
        }
        other => other,
    }
}
fn require_queue_mergeability(forge: &mut dyn Forge, pr: &EligiblePr) -> Result<(), LandingError> {
    let current = forge.pull_request(pr.number)?;
    if current.state != PullRequestState::Open
        || current.head != pr.head
        || current.base_branch != pr.base_branch
    {
        return Err(LandingError::PullRequestChanged(pr.number));
    }
    if current.mergeability != Mergeability::Clean {
        return Err(LandingError::Pending(format!(
            "pull request #{} is not known clean immediately before merge ({:?})",
            pr.number, current.mergeability
        )));
    }
    Ok(())
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
    grant: &PublicationGrant,
    delivery_queue: bool,
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
        if delivery_queue {
            super::gate::require_delivery_queue_ci(forge, &step.pr)?;
        }
        before_dispatch(step.pr.number)?;
        let reviewed = !step.pr.approval_evidence.is_empty();
        expected = merge_member_with_queue_policy(
            forge,
            step.pr.number,
            &step.pr.head,
            method,
            grant,
            reviewed,
            delivery_queue.then_some(&step.pr),
        )?;
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
        ports::{Approval, RequiredCiCheck},
        test_support::LandingFixture,
    };

    fn candidate() -> Candidate {
        LandingFixture::ordered_candidate()
    }
    fn host_grant(mode: provenant_cli::domains::AdminPublication) -> PublicationGrant {
        PublicationGrant {
            mode,
            source: provenant_records::provenance::Source {
                layer: provenant_records::Layer::Host,
                origin: Some("host.toml#domains.pr.land.admin_publication".into()),
            },
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
                    mergeability: crate::pr::landing::ports::Mergeability::Unknown,
                    merged_commit: None,
                    stack: None,
                    draft: false,
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
    fn queue_fixture() -> (FakeForge, FakeRepo, Candidate) {
        let mut forge = FakeForge::default();
        prepare(&mut forge);
        let candidate = Candidate {
            steps: vec![candidate().steps[0].clone()],
            ..candidate()
        };
        let step = &candidate.steps[0];
        forge
            .pull_requests
            .get_mut(&step.pr.number)
            .expect("prepared pull request")
            .mergeability = Mergeability::Clean;
        forge.required_ci_policies.insert(
            "main".into(),
            Some(vec![RequiredCiCheck {
                name: "build".into(),
                app_id: Some(42),
            }]),
        );
        forge.check_runs.insert(
            step.pr.head.clone(),
            vec![GateEvidence {
                commit: step.pr.head.clone(),
                tree: "head-tree".into(),
                definition_digest: String::new(),
                checks: vec![CheckEvidence {
                    name: "build".into(),
                    issuer: Some("required-app".into()),
                    app_id: Some(42),
                    result: GateResult::Passed,
                }],
                issuer: None,
                coverage: Vec::new(),
                result: GateResult::Passed,
                pool: None,
                duration_seconds: None,
                reused_from_commit: None,
                extra: Default::default(),
            }],
        );
        let repo = FakeRepo {
            target: candidate.base.clone(),
            fetches: [candidate.base.clone()].into(),
            ..FakeRepo::default()
        };
        (forge, repo, candidate)
    }

    fn queue_publish(
        forge: &mut FakeForge,
        repo: &mut FakeRepo,
        candidate: &Candidate,
    ) -> Result<super::super::model::PublishResult, LandingError> {
        rest_ordered(
            forge,
            repo,
            Approvals::forge(),
            "main",
            candidate,
            MergeMethod::Merge,
            &PublicationGrant::default(),
            true,
            &mut Vec::new(),
            &mut |_| Ok(()),
        )
    }
    #[test]
    fn delivery_recovery_rechecks_current_ci_before_retrying_merge() {
        let (mut forge, _, candidate) = queue_fixture();
        let pr = &candidate.steps[0].pr;
        forge.check_runs.get_mut(&pr.head).unwrap()[0].checks[0].result = GateResult::Failed;
        assert!(matches!(
            merge_delivery_member(
                &mut forge,
                pr,
                MergeMethod::Merge,
                &PublicationGrant::default(),
                true
            ),
            Err(LandingError::UnmetGate(_))
        ));
        assert!(forge.merges.is_empty());
    }

    #[test]
    fn delivery_admin_retry_rechecks_ci_after_review_refusal() {
        use provenant_cli::domains::AdminPublication;

        let (mut forge, _, candidate) = queue_fixture();
        let pr = &candidate.steps[0].pr;
        let green = forge.check_runs[&pr.head].clone();
        let mut red = green.clone();
        red[0].checks[0].result = GateResult::Failed;
        forge
            .check_sequences
            .insert(pr.head.clone(), [green, red].into());
        forge.review_required_merge = Some(pr.number);
        let grant = host_grant(AdminPublication::WhenReviewUnavailable);
        assert!(matches!(
            merge_delivery_member(&mut forge, pr, MergeMethod::Merge, &grant, true),
            Err(LandingError::UnmetGate(_))
        ));
        assert!(forge.admin_merges.is_empty());
    }

    #[test]
    fn delivery_queue_merges_only_after_exact_head_ci_and_fresh_clean_mergeability()
    -> Result<(), LandingError> {
        let (mut forge, mut repo, candidate) = queue_fixture();
        let result = queue_publish(&mut forge, &mut repo, &candidate)?;
        assert_eq!(result.landed, vec!["landed-1".to_owned()]);
        let tail = &forge.events[forge.events.len() - 2..];
        assert_eq!(tail[0].as_str(), "pull_request:1:Clean:head-1");
        assert_eq!(tail[1].as_str(), "merge:1:head-1");
        assert!(forge.events.contains(&"checks:head-1".into()));
        Ok(())
    }

    #[test]
    fn delivery_queue_does_not_accept_red_absent_or_wrong_app_ci() {
        let (mut red, mut red_repo, red_candidate) = queue_fixture();
        red.check_runs.get_mut("head-1").unwrap()[0].checks[0].result = GateResult::Failed;
        assert!(matches!(
            queue_publish(&mut red, &mut red_repo, &red_candidate),
            Err(LandingError::UnmetGate(_))
        ));
        assert!(red.merges.is_empty());

        let (mut absent, mut absent_repo, absent_candidate) = queue_fixture();
        absent.check_runs.remove("head-1");
        assert!(matches!(
            queue_publish(&mut absent, &mut absent_repo, &absent_candidate),
            Err(LandingError::Pending(_))
        ));
        assert!(absent.merges.is_empty());

        let (mut wrong_app, mut wrong_app_repo, wrong_app_candidate) = queue_fixture();
        wrong_app.check_runs.get_mut("head-1").unwrap()[0].checks[0].app_id = Some(17);
        assert!(matches!(
            queue_publish(&mut wrong_app, &mut wrong_app_repo, &wrong_app_candidate),
            Err(LandingError::Pending(_))
        ));
        assert!(wrong_app.merges.is_empty());
    }

    #[test]
    fn delivery_queue_rechecks_moved_head_and_mergeability_after_ci() {
        let (mut moved, mut moved_repo, moved_candidate) = queue_fixture();
        moved
            .move_head_after_checks
            .insert("head-1".into(), "new-head".into());
        assert!(matches!(
            queue_publish(&mut moved, &mut moved_repo, &moved_candidate),
            Err(LandingError::PullRequestChanged(1))
        ));
        assert!(moved.merges.is_empty());

        let (mut retargeted, mut retargeted_repo, retargeted_candidate) = queue_fixture();
        retargeted
            .move_base_after_checks
            .insert("head-1".into(), "other".into());
        assert!(matches!(
            queue_publish(&mut retargeted, &mut retargeted_repo, &retargeted_candidate),
            Err(LandingError::PullRequestChanged(1))
        ));
        assert!(retargeted.merges.is_empty());

        let (mut conflicting, mut conflicting_repo, conflicting_candidate) = queue_fixture();
        conflicting
            .mergeability_after_checks
            .insert("head-1".into(), Mergeability::Conflicting);
        assert!(matches!(
            queue_publish(
                &mut conflicting,
                &mut conflicting_repo,
                &conflicting_candidate
            ),
            Err(LandingError::Pending(_))
        ));
        assert!(conflicting.merges.is_empty());

        let (mut unknown, mut unknown_repo, unknown_candidate) = queue_fixture();
        unknown
            .mergeability_after_checks
            .insert("head-1".into(), Mergeability::Unknown);
        assert!(matches!(
            queue_publish(&mut unknown, &mut unknown_repo, &unknown_candidate),
            Err(LandingError::Pending(_))
        ));
        assert!(unknown.merges.is_empty());
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
        let error = merge_member(
            &mut forge,
            2,
            "head-2",
            MergeMethod::Merge,
            &PublicationGrant::default(),
            true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("#1"));
        assert!(forge.merge_attempts.is_empty());
        assert_eq!(
            merge_member(
                &mut forge,
                1,
                "head-1",
                MergeMethod::Squash,
                &PublicationGrant::default(),
                true
            )?,
            "landed-1"
        );
        assert_eq!(forge.stack_merge_attempts, vec![1]);
        assert_eq!(forge.merges[0].2, MergeMethod::Squash);
        assert_eq!(forge.pull_requests[&2].base_branch, "main");
        forge.heads.insert(2, "moved".into());
        assert!(
            merge_member(
                &mut forge,
                2,
                "head-2",
                MergeMethod::Merge,
                &PublicationGrant::default(),
                true
            )
            .is_err()
        );
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
    #[derive(Clone, Copy)]
    enum RevalidationChange {
        Head,
        Approval,
        Base,
    }
    fn ordered_revalidation_rejects(change: RevalidationChange) {
        let mut forge = FakeForge::default();
        prepare(&mut forge);
        match change {
            RevalidationChange::Head => {
                forge.heads.insert(2, "new-head".into());
            }
            RevalidationChange::Approval => {
                forge.approvals_at_head.remove(&(2, "head-2".into()));
            }
            RevalidationChange::Base => {
                forge.pull_requests.get_mut(&2).unwrap().base_branch = "other".into();
            }
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
                &PublicationGrant::default(),
                false,
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
    fn ordered_revalidation_rejects_every_changed_input_before_the_next_merge() {
        for change in [
            RevalidationChange::Head,
            RevalidationChange::Approval,
            RevalidationChange::Base,
        ] {
            ordered_revalidation_rejects(change);
        }
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
                &PublicationGrant::default(),
                false,
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
            &PublicationGrant::default(),
            false,
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

    // Spec: docs/specs/cli.md#landing-adds-no-merge-authority
    #[test]
    fn review_requirement_405_retries_with_admin_after_the_gate_with_a_host_grant()
    -> Result<(), LandingError> {
        use provenant_cli::domains::AdminPublication;
        let mut forge = FakeForge {
            review_required_merge: Some(1),
            ..Default::default()
        };
        prepare(&mut forge);
        let grant = host_grant(AdminPublication::WhenReviewUnavailable);
        assert_eq!(
            merge_member(&mut forge, 1, "head-1", MergeMethod::Merge, &grant, true)?,
            "landed-1"
        );
        assert_eq!(forge.admin_merges, vec![1]);
        Ok(())
    }

    // Spec: docs/specs/cli.md#landing-adds-no-merge-authority
    #[test]
    fn admin_retry_never_bypasses_missing_grant_other_layers_or_missing_review() {
        use provenant_cli::domains::AdminPublication;
        let grant = host_grant(AdminPublication::WhenReviewUnavailable);
        let non_host = PublicationGrant {
            mode: AdminPublication::WhenReviewUnavailable,
            source: provenant_records::provenance::Source {
                layer: provenant_records::Layer::Project,
                origin: Some(".provenant/config.toml".into()),
            },
        };
        for (name, grant, reviewed, refuse) in [
            ("no grant", PublicationGrant::default(), true, true),
            ("never", host_grant(AdminPublication::Never), true, true),
            ("project layer", non_host, true, true),
            ("no review evidence", grant.clone(), false, true),
            ("other 405 cause", grant.clone(), true, false),
        ] {
            let mut forge = FakeForge {
                review_required_merge: refuse.then_some(1),
                refuse_merge: (!refuse).then_some(1),
                ..Default::default()
            };
            prepare(&mut forge);
            let error = merge_member(
                &mut forge,
                1,
                "head-1",
                MergeMethod::Merge,
                &grant,
                reviewed,
            )
            .unwrap_err();
            assert!(error.is_refusal(), "{name}: {error}");
            assert!(forge.admin_merges.is_empty(), "{name}");
        }
    }
}
