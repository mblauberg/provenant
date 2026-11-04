use super::{
    LandingError,
    model::*,
    ports::{AlarmSink, Forge, GateRunner, PullRequestState, Repo},
    postmerge, receipt,
};
use std::collections::BTreeMap;
/// Recover pending publications before selection, retaining unprovable outcomes.
/// In shadow mode, refuse when recovery would mutate a ref.
#[allow(
    clippy::too_many_arguments,
    reason = "recovery needs the publication boundaries and state scope"
)]
pub fn recover_pending(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    state: &std::path::Path,
    repository: &str,
    target: &str,
    shadow: bool,
    recovered: &mut Vec<LandingReceipt>,
) -> Result<(), LandingError> {
    for attempt in receipt::pending(state, repository, target)? {
        let marker = receipt::pending_path(state, repository, &attempt.candidate)?;
        let unknown = || LandingError::PublicationUnknownAt {
            marker: marker.clone(),
        };
        let mut tip = if shadow {
            repo.fetch_target_untracked(target)
        } else {
            repo.fetch_target(target)
        }
        .map_err(|_| unknown())?;
        if !shadow
            && attempt.publish.mode == PublishMode::FastForward
            && tip == attempt.candidate.base
        {
            // The identical lease is safe even if the original push is still running.
            let _ = repo.push_ref(
                target,
                &attempt.candidate.commit,
                Some(&attempt.candidate.base),
            );
            tip = repo.fetch_target(target).map_err(|_| unknown())?;
        }
        let exact = repo
            .is_ancestor(&attempt.candidate.commit, &tip)
            .map_err(|_| unknown())?;
        let fast_unpublished = attempt.publish.mode == PublishMode::FastForward
            && !exact
            && tip != attempt.candidate.base;
        let mut mapping = BTreeMap::new();
        let mut outcomes = BTreeMap::new();
        let mut landed_prs = Vec::new();
        let mut unproven = false;
        for (index, step) in attempt.candidate.steps.iter().enumerate() {
            let landed = if exact {
                Some(step.commit.clone())
            } else if attempt.publish.mode == PublishMode::FastForward {
                if fast_unpublished {
                    None
                } else {
                    unproven = true;
                    outcomes.insert(step.pr.number, Outcome::PendingUnknown);
                    continue;
                }
            } else {
                let pr = match forge.pull_request(step.pr.number) {
                    Ok(pr) => pr,
                    Err(_) => {
                        unproven = true;
                        outcomes.insert(step.pr.number, Outcome::PendingUnknown);
                        continue;
                    }
                };
                match (pr.state, pr.merged_commit) {
                    (PullRequestState::Merged, Some(commit))
                        if pr.head == step.pr.head
                            && repo.is_ancestor(&commit, &tip).unwrap_or(false) =>
                    {
                        Some(commit)
                    }
                    (PullRequestState::Closed, _) => None,
                    (PullRequestState::Open, _) if pr.head != step.pr.head => None,
                    (PullRequestState::Open, _) => {
                        // Retry the same pinned operation; a refusal or another lost response
                        // cannot prove the original request has stopped processing.
                        let expected = landed_prs
                            .last()
                            .and_then(|pr: &EligiblePr| mapping.get(&pr.head))
                            .unwrap_or(&attempt.candidate.base);
                        let approved = pr.base_branch == step.pr.base_branch
                            && forge.approvals(step.pr.number, &step.pr.head).is_ok_and(
                                |approvals| {
                                    approvals
                                        .iter()
                                        .any(|approval| approval.reference == step.pr.approval)
                                },
                            );
                        if !shadow
                            && !unproven
                            && approved
                            && landed_prs.len() == index
                            && &tip == expected
                        {
                            let method = attempt.policy.settings().map_err(|_| unknown())?.method;
                            let _ = forge.merge(step.pr.number, &step.pr.head, method);
                        }
                        unproven = true;
                        outcomes.insert(step.pr.number, Outcome::PendingUnknown);
                        continue;
                    }
                    _ => {
                        unproven = true;
                        outcomes.insert(step.pr.number, Outcome::PendingUnknown);
                        continue;
                    }
                }
            };
            if let Some(commit) = landed {
                mapping.insert(step.pr.head.clone(), commit.clone());
                mapping.insert(step.commit.clone(), commit);
                outcomes.insert(step.pr.number, Outcome::Landed);
                landed_prs.push(step.pr.clone());
            } else {
                outcomes.insert(step.pr.number, Outcome::Excluded);
            }
        }
        if shadow {
            return Err(LandingError::RecoveryRequired {
                marker: receipt::pending_path(state, repository, &attempt.candidate)?,
            });
        }
        let mut record = LandingReceipt::new(
            &attempt.candidate,
            attempt.policy,
            attempt.gate_definition,
            attempt.gate_evidence,
            attempt.publish,
            attempt.cost,
            false,
        );
        record
            .candidate_steps
            .retain(|step| outcomes.get(&step.pr.number) == Some(&Outcome::Landed));
        let is_prefix = landed_prs
            .iter()
            .zip(&attempt.candidate.steps)
            .all(|(pr, step)| pr.number == step.pr.number);
        if !is_prefix {
            // A candidate step includes all earlier heads, so a gap cannot be
            // used as evidence to blame any of the published pull requests.
            record.candidate_steps.clear();
            record.alarms.push(Alarm { code: "LAND_BISECT_UNAVAILABLE".into(), message: "recovered pull requests are not an ordered candidate prefix; bisection is unavailable".into() });
        }
        record.pull_requests = landed_prs;
        record.outcomes = outcomes;
        record.approved_to_landed = mapping;
        if !record.pull_requests.is_empty()
            && record.pull_requests.len() == attempt.candidate.steps.len()
        {
            let last = record
                .pull_requests
                .last()
                .and_then(|pr| record.approved_to_landed.get(&pr.head))
                .ok_or(LandingError::PublicationUnknown)?;
            let tree = repo.tree_of(last)?;
            if tree != attempt.candidate.tree {
                record.alarms.push(Alarm {
                    code: "LAND_TREE_MISMATCH".into(),
                    message: format!(
                        "gated tree {} differs from landed commit {} tree {}",
                        attempt.candidate.tree, last, tree
                    ),
                });
            }
        }
        if unproven && record.pull_requests.is_empty() {
            return Err(LandingError::PublicationUnknownAt {
                marker: receipt::pending_path(state, repository, &attempt.candidate)?,
            });
        }
        receipt::write_receipt(state, &record)?;
        recovered.push(record.clone());
        for alarm in &record.alarms {
            alarms.post(alarm)?;
        }
        if !record.pull_requests.is_empty() {
            let followed = postmerge::follow(forge, repo, runner, alarms, &mut record);
            receipt::write_receipt(state, &record)?;
            if let Some(saved) = recovered.last_mut() {
                *saved = record.clone();
            }
            followed?;
        }
        if unproven {
            return Err(LandingError::PublicationUnknownAt {
                marker: receipt::pending_path(state, repository, &attempt.candidate)?,
            });
        }
        repo.delete_ref(&attempt.candidate.ref_name, &attempt.candidate.commit)?;
        receipt::clear_pending(state, repository, &attempt.candidate)?;
    }
    Ok(())
}
