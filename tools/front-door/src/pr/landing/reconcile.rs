use super::{
    LandingError,
    model::*,
    ports::{AlarmSink, Forge, GateRunner, PullRequestState, Repo},
    postmerge, publish, receipt,
};
use std::collections::BTreeMap;
// A closed PR must never be published by a retry. A merged REST prefix may
// remain part of the attempt, but a fast-forward retry requires all PRs open.
fn valid_for_retry(forge: &mut dyn Forge, attempt: &PendingAttempt) -> bool {
    publish::revalidate(forge, &attempt.candidate).is_ok_and(|excluded| excluded.is_empty())
        && attempt.candidate.steps.iter().all(|step| {
            forge
                .pull_request(step.pr.number)
                .is_ok_and(|pr| match pr.state {
                    PullRequestState::Open => true,
                    PullRequestState::Merged => attempt.publish.mode == PublishMode::RestOrdered,
                    PullRequestState::Closed => false,
                })
        })
}
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
        let settings = attempt.policy.settings().map_err(|_| unknown())?;
        let mut settled = false;
        let can_retry = !shadow && valid_for_retry(forge, &attempt);
        if !shadow && !can_retry {
            #[allow(
                clippy::disallowed_methods,
                reason = "allow the original publication to settle"
            )]
            std::thread::sleep(std::time::Duration::from_secs(
                settings.publish_settle_seconds,
            ));
            settled = true;
            tip = repo.fetch_target(target).map_err(|_| unknown())?;
        }
        if can_retry
            && attempt.publish.mode == PublishMode::FastForward
            && tip == attempt.candidate.base
        {
            let retry = repo.push_ref(
                target,
                &attempt.candidate.commit,
                Some(&attempt.candidate.base),
            );
            if matches!(
                retry,
                Err(LandingError::Refused(_)
                    | LandingError::TargetMoved
                    | LandingError::PullRequestChanged(_))
            ) {
                // The retry is definitely refused; later observation or cleanup must
                // not leave an intent that authorizes another push.
                receipt::clear_pending(state, repository, &attempt.candidate)?;
            }
            if retry.is_err() {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "allow the original publication to settle"
                )]
                std::thread::sleep(std::time::Duration::from_secs(
                    settings.publish_settle_seconds,
                ));
                settled = true;
            }
            tip = repo.fetch_target(target).map_err(|_| unknown())?;
        }
        let exact = repo
            .is_ancestor(&attempt.candidate.commit, &tip)
            .map_err(|_| unknown())?;
        let fast_unpublished = attempt.publish.mode == PublishMode::FastForward
            && !exact
            && (tip != attempt.candidate.base || settled);
        let mut mapping = BTreeMap::new();
        let mut outcomes = BTreeMap::new();
        let mut landed_prs = Vec::new();
        let mut unproven = false;
        let mut refused = false;
        for (index, step) in attempt.candidate.steps.iter().enumerate() {
            let landed = if refused {
                None
            } else if exact {
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
                            && pr.base_branch == step.pr.base_branch
                            && (settled || repo.is_ancestor(&commit, &tip).unwrap_or(false)) =>
                    {
                        Some(commit)
                    }
                    (PullRequestState::Closed, _) => None,
                    (_, _) if pr.head != step.pr.head => None,
                    (PullRequestState::Open, _) => {
                        let expected = landed_prs
                            .last()
                            .and_then(|pr: &EligiblePr| mapping.get(&pr.head))
                            .unwrap_or(&attempt.candidate.base);
                        if can_retry && !unproven && landed_prs.len() == index && &tip == expected {
                            // A previous recovered merge may have advanced the target; read it
                            // again before each ordered retry to detect intervening movement.
                            tip = repo.fetch_target(target).map_err(|_| unknown())?;
                            if &tip != expected {
                                #[allow(
                                    clippy::disallowed_methods,
                                    reason = "allow the original publication to settle"
                                )]
                                std::thread::sleep(std::time::Duration::from_secs(
                                    settings.publish_settle_seconds,
                                ));
                                match forge.pull_request(step.pr.number) {
                                    Ok(pr)
                                        if pr.state == PullRequestState::Open
                                            || pr.state == PullRequestState::Closed =>
                                    {
                                        outcomes.insert(step.pr.number, Outcome::Excluded);
                                    }
                                    _ => {
                                        unproven = true;
                                        outcomes.insert(step.pr.number, Outcome::PendingUnknown);
                                    }
                                }
                                continue;
                            }
                            let retry = if valid_for_retry(forge, &attempt) {
                                forge.merge(step.pr.number, &step.pr.head, settings.method)
                            } else {
                                Err(LandingError::PublicationUnknown)
                            };
                            match retry {
                                Ok(commit) => {
                                    tip = commit.clone();
                                    Some(commit)
                                }
                                Err(
                                    LandingError::Refused(_)
                                    | LandingError::TargetMoved
                                    | LandingError::PullRequestChanged(_),
                                ) => {
                                    // No earlier step is unknown when a retry is attempted. All
                                    // remaining operations were never attempted by this retry.
                                    receipt::clear_pending(state, repository, &attempt.candidate)?;
                                    refused = true;
                                    None
                                }
                                Err(_) => {
                                    #[allow(
                                        clippy::disallowed_methods,
                                        reason = "allow the original publication to settle"
                                    )]
                                    std::thread::sleep(std::time::Duration::from_secs(
                                        settings.publish_settle_seconds,
                                    ));
                                    tip = repo.fetch_target(target).map_err(|_| unknown())?;
                                    match forge.pull_request(step.pr.number) {
                                        Ok(pr)
                                            if pr.state == PullRequestState::Merged
                                                && pr.head == step.pr.head
                                                && pr.base_branch == step.pr.base_branch =>
                                        {
                                            match pr.merged_commit {
                                                Some(commit) => Some(commit),
                                                _ => {
                                                    unproven = true;
                                                    outcomes.insert(
                                                        step.pr.number,
                                                        Outcome::PendingUnknown,
                                                    );
                                                    continue;
                                                }
                                            }
                                        }
                                        Ok(pr)
                                            if pr.state == PullRequestState::Closed
                                                || pr.state == PullRequestState::Open
                                                || pr.head != step.pr.head =>
                                        {
                                            None
                                        }
                                        _ => {
                                            unproven = true;
                                            outcomes
                                                .insert(step.pr.number, Outcome::PendingUnknown);
                                            continue;
                                        }
                                    }
                                }
                            }
                        } else if settled {
                            None
                        } else {
                            #[allow(
                                clippy::disallowed_methods,
                                reason = "allow the original publication to settle"
                            )]
                            std::thread::sleep(std::time::Duration::from_secs(
                                settings.publish_settle_seconds,
                            ));
                            match forge.pull_request(step.pr.number) {
                                Ok(pr)
                                    if pr.state == PullRequestState::Open
                                        || pr.state == PullRequestState::Closed =>
                                {
                                    None
                                }
                                _ => {
                                    unproven = true;
                                    outcomes.insert(step.pr.number, Outcome::PendingUnknown);
                                    continue;
                                }
                            }
                        }
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
            let verified = if record.publish.mode == PublishMode::RestOrdered {
                repo.fetch_target(target).map_err(|_| unknown())?
            } else {
                last.clone()
            };
            let tree = repo.tree_of(&verified).map_err(|_| unknown())?;
            if tree != attempt.candidate.tree {
                record.alarms.push(Alarm {
                    code: "LAND_TREE_MISMATCH".into(),
                    message: format!(
                        "gated tree {} differs from landed commit {} tree {}",
                        attempt.candidate.tree, verified, tree
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
