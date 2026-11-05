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
/// Persist the definite outcome before performing any fallible follow-up.
pub fn settle(
    state: &std::path::Path,
    attempt: &mut PendingAttempt,
    record: LandingReceipt,
    refusal: Option<String>,
) -> Result<(), LandingError> {
    let landed_commit = record
        .pull_requests
        .last()
        .and_then(|pr| record.approved_to_landed.get(&pr.head))
        .cloned();
    let tree_pending =
        landed_commit.is_some() && record.pull_requests.len() == attempt.candidate.steps.len();
    let follow_pending = landed_commit.is_some();
    let mut refused = false;
    let outcomes = attempt
        .candidate
        .steps
        .iter()
        .map(|step| {
            let result = if let Some(commit) = record.approved_to_landed.get(&step.pr.head) {
                SettledOutcome::Landed {
                    commit: commit.clone(),
                }
            } else if refusal.is_some()
                && (!refused || attempt.publish.mode == PublishMode::FastForward)
            {
                refused = true;
                SettledOutcome::Refused
            } else {
                SettledOutcome::NotLanded
            };
            (step.pr.number, result)
        })
        .collect();
    attempt.settled = Some(SettledAttempt {
        record,
        outcomes,
        landed_commit: landed_commit.clone(),
        refusal,
        receipt_pending: true,
        tree_pending,
        follow_pending,
        local_follow_pending: landed_commit.is_some(),
        cleanup_pending: true,
    });
    receipt::record_pending(state, attempt)
}

/// Resume only follow-up work from a definite outcome. Checkpoint each completed step.
#[allow(
    clippy::too_many_arguments,
    reason = "follow-up uses the landing boundaries"
)]
pub fn finish_settled(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    state: &std::path::Path,
    attempt: &mut PendingAttempt,
) -> Result<LandingReceipt, LandingError> {
    if attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .receipt_pending
    {
        receipt::write_receipt(
            state,
            &attempt
                .settled
                .as_ref()
                .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
                .record,
        )?;
        attempt
            .settled
            .as_mut()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
            .receipt_pending = false;
        receipt::record_pending(state, attempt)?;
    }
    if attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .tree_pending
    {
        let settled = attempt
            .settled
            .as_mut()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?;
        let commit = settled
            .landed_commit
            .as_ref()
            .ok_or_else(|| LandingError::Failed("missing landed commit".into()))?;
        let tree = repo.tree_of(commit)?;
        if tree != attempt.candidate.tree {
            settled.record.alarms.push(Alarm {
                code: "LAND_TREE_MISMATCH".into(),
                message: format!(
                    "gated tree {} differs from landed commit {} tree {}",
                    attempt.candidate.tree, commit, tree
                ),
            });
        }
        settled.tree_pending = false;
        settled.receipt_pending = true;
        receipt::record_pending(state, attempt)?;
    }
    if attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .follow_pending
    {
        let settled = attempt
            .settled
            .as_mut()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?;
        for alarm in &settled.record.alarms {
            alarms.post(alarm)?;
        }
        let followed = postmerge::follow(forge, repo, runner, alarms, &mut settled.record);
        if followed.is_ok() {
            settled.follow_pending = false;
        }
        settled.receipt_pending = true;
        receipt::record_pending(state, attempt)?;
        followed?;
    }
    if attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .receipt_pending
    {
        receipt::write_receipt(
            state,
            &attempt
                .settled
                .as_ref()
                .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
                .record,
        )?;
        attempt
            .settled
            .as_mut()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
            .receipt_pending = false;
        receipt::record_pending(state, attempt)?;
    }
    let mut local_error = None;
    if attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .local_follow_pending
    {
        let mut complete = true;
        for pr in &attempt
            .settled
            .as_ref()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
            .record
            .pull_requests
        {
            let result = repo.follow_local(pr);
            super::ports::sync_project(repo, forge, runner, alarms);
            match result {
                Ok(done) => complete &= done,
                Err(error @ LandingError::LocalFollowUp(_)) => {
                    complete = false;
                    if local_error.is_none() {
                        local_error = Some(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        attempt
            .settled
            .as_mut()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
            .local_follow_pending = !complete;
        receipt::record_pending(state, attempt)?;
    }
    if attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .cleanup_pending
    {
        repo.delete_ref(&attempt.candidate.ref_name, &attempt.candidate.commit)?;
        attempt
            .settled
            .as_mut()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
            .cleanup_pending = false;
        receipt::record_pending(state, attempt)?;
    }
    let record = attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .record
        .clone();
    if !attempt
        .settled
        .as_ref()
        .is_some_and(|settled| settled.local_follow_pending)
    {
        receipt::clear_pending(state, &attempt.repository, &attempt.candidate)?;
    }
    if let Some(error) = local_error {
        return Err(error);
    }
    Ok(record)
}
#[allow(
    clippy::too_many_arguments,
    reason = "recovery reports durable progress on failure"
)]
fn finish_recovery(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    state: &std::path::Path,
    attempt: &mut PendingAttempt,
    recovered: &mut Vec<LandingReceipt>,
) -> Result<(), LandingError> {
    recovered.push(
        attempt
            .settled
            .as_ref()
            .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
            .record
            .clone(),
    );
    let result = finish_settled(forge, repo, runner, alarms, state, attempt);
    *recovered
        .last_mut()
        .ok_or_else(|| LandingError::Failed("missing recovered receipt".into()))? = attempt
        .settled
        .as_ref()
        .ok_or_else(|| LandingError::Failed("missing settled state".into()))?
        .record
        .clone();
    match result {
        Err(LandingError::LocalFollowUp(_)) => Ok(()),
        other => other.map(|_| ()),
    }
}
fn local_cleanup_only(settled: &SettledAttempt) -> bool {
    settled.local_follow_pending
        && !settled.receipt_pending
        && !settled.tree_pending
        && !settled.follow_pending
        && !settled.cleanup_pending
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
    #[allow(clippy::disallowed_methods, reason = "allow publication to settle")]
    recover_pending_with(
        forge,
        repo,
        runner,
        alarms,
        state,
        repository,
        target,
        shadow,
        recovered,
        &std::thread::sleep,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "recovery uses the landing boundaries and injectable wait"
)]
pub(crate) fn recover_pending_with(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    state: &std::path::Path,
    repository: &str,
    target: &str,
    shadow: bool,
    recovered: &mut Vec<LandingReceipt>,
    sleep: &dyn Fn(std::time::Duration),
) -> Result<(), LandingError> {
    for mut attempt in receipt::pending(state, repository, target)? {
        if shadow {
            if attempt.settled.as_ref().is_some_and(local_cleanup_only) {
                continue;
            }
            return Err(LandingError::RecoveryRequired {
                marker: receipt::pending_path(state, repository, &attempt.candidate)?,
            });
        }
        if attempt.settled.is_some() {
            // A previously reported partial refusal must not make a retained
            // local-cleanup marker refuse every later landing.
            let refusal = attempt.settled.as_ref().and_then(|s| {
                if local_cleanup_only(s) {
                    None
                } else {
                    s.refusal.clone()
                }
            });
            finish_recovery(forge, repo, runner, alarms, state, &mut attempt, recovered)?;
            if let Some(reason) = refusal {
                return Err(LandingError::Refused(reason));
            }
            continue;
        }
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
            sleep(std::time::Duration::from_secs(
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
                let mut record = LandingReceipt::new(
                    &attempt.candidate,
                    attempt.policy.clone(),
                    attempt.gate_definition.clone(),
                    attempt.gate_evidence.clone(),
                    attempt.publish.clone(),
                    attempt.cost.clone(),
                    false,
                );
                record.pull_requests.clear();
                record.candidate_steps.clear();
                for step in &attempt.candidate.steps {
                    record.outcomes.insert(step.pr.number, Outcome::Excluded);
                }
                settle(
                    state,
                    &mut attempt,
                    record,
                    Some("publication retry refused".into()),
                )?;
                finish_recovery(forge, repo, runner, alarms, state, &mut attempt, recovered)?;
                return Err(LandingError::Refused("publication retry refused".into()));
            }
            if retry.is_err() {
                return Err(unknown());
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
                        if pr.head == step.pr.head && pr.base_branch == step.pr.base_branch =>
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
                                sleep(std::time::Duration::from_secs(
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
                                    refused = true;
                                    None
                                }
                                // A lost retry response remains uncertain until recovery observes it.
                                Err(_) => {
                                    unproven = true;
                                    outcomes.insert(step.pr.number, Outcome::PendingUnknown);
                                    continue;
                                }
                            }
                        } else if settled {
                            None
                        } else {
                            #[allow(
                                clippy::disallowed_methods,
                                reason = "allow the original publication to settle"
                            )]
                            sleep(std::time::Duration::from_secs(
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
            attempt.policy.clone(),
            attempt.gate_definition.clone(),
            attempt.gate_evidence.clone(),
            attempt.publish.clone(),
            attempt.cost.clone(),
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
        if unproven {
            if !record.pull_requests.is_empty() {
                receipt::write_receipt(state, &record)?;
                recovered.push(record);
            }
            return Err(unknown());
        }
        settle(
            state,
            &mut attempt,
            record,
            refused.then(|| "publication retry refused".into()),
        )?;
        finish_recovery(forge, repo, runner, alarms, state, &mut attempt, recovered)?;
        if refused {
            return Err(LandingError::Refused("publication retry refused".into()));
        }
    }
    Ok(())
}
