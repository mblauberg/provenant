//! Landing orchestration. Lane-owned decisions and I/O remain behind their ports.
use super::{
    LandingError,
    approval::Approvals,
    budget, candidate, gate,
    model::*,
    policy::LandPolicy,
    ports::{AlarmSink, Forge, GateRunner, Repo},
    publish, receipt, reconcile, split,
};
use provenant_cli::domains::LandSettings;
use std::collections::{BTreeMap, VecDeque};

pub struct Request<'a> {
    pub numbers: &'a [u64],
    pub target: &'a str,
    pub settings: &'a LandSettings,
    pub policy: &'a LandPolicy,
    pub gate: &'a GateDefinition,
    pub shadow: bool,
    pub state: &'a std::path::Path,
    pub approvals: Approvals<'a>,
}
#[derive(Debug, Default)]
pub struct Report {
    pub receipts: Vec<LandingReceipt>,
    pub outcomes: BTreeMap<u64, Outcome>,
    pub warnings: Vec<String>,
    pub exclusion_reasons: BTreeMap<u64, String>,
    pub local_steps: BTreeMap<u64, serde_json::Value>,
}

fn fetch_target(repo: &mut dyn Repo, target: &str, shadow: bool) -> Result<String, LandingError> {
    if shadow {
        repo.fetch_target_untracked(target)
    } else {
        repo.fetch_target(target)
    }
}

/// The fetched target a review record or a pull request's paths are read
/// against. A forge approval of a set tier is bound to the head alone and
/// needs none.
fn approval_target(repo: &mut dyn Repo, request: &Request<'_>) -> Result<String, LandingError> {
    if request.approvals.needs_target() {
        fetch_target(repo, request.target, request.shadow)
    } else {
        Ok(String::new())
    }
}

/// The warning for a pull request landed on the chair's word.
pub(super) fn accepted_warning(pr: &EligiblePr) -> Option<String> {
    pr.accepted.as_ref().map(|accepted| {
        format!(
            "warning: pull request #{} lands without a review covering its delta; accepted by {}: {}",
            pr.number, accepted.actor, accepted.reason
        )
    })
}

fn cleanup_unpublished<T>(
    result: Result<T, LandingError>,
    repo: &mut dyn Repo,
    candidate: &Candidate,
) -> Result<T, LandingError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            repo.delete_ref(&candidate.ref_name, &candidate.commit)
                .map_err(|cleanup| {
                    LandingError::Failed(format!("{error}; candidate cleanup failed: {cleanup}"))
                })?;
            Err(error)
        }
    }
}

fn add_recovered(report: &mut Report, recovered: Vec<LandingReceipt>) {
    for record in recovered {
        for (number, outcome) in &record.outcomes {
            if *outcome == Outcome::Landed {
                report.outcomes.insert(*number, Outcome::Landed);
            }
        }
        report.receipts.push(record);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "recovery needs the active boundaries"
)]
fn recover(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    request: &Request<'_>,
    repository: &str,
    report: &mut Report,
) -> Result<(), LandingError> {
    let mut recovered = Vec::new();
    let result = reconcile::recover_pending(
        forge,
        repo,
        request.approvals,
        runner,
        alarms,
        request.state,
        repository,
        request.target,
        request.shadow,
        &mut recovered,
    );
    add_recovered(report, recovered);
    result
}

fn consume_rebuild(rebuilds: &mut u64, max: u64) -> Result<(), LandingError> {
    if !split::rebuild_allowed(*rebuilds, max)? {
        return Err(LandingError::Contention);
    }
    *rebuilds += 1;
    Ok(())
}

/// Select, build, gate, revalidate and publish batches in order. Unknown
/// publication responses are reconciled before this function returns.
pub fn run(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    request: Request<'_>,
) -> Result<Report, LandingError> {
    if request.numbers.is_empty() {
        return Err(LandingError::Failed("no pull requests selected".into()));
    }
    let numbers = request.numbers;
    let state = request.state;
    let shadow = request.shadow;
    let mut report = Report::default();
    let result = run_inner(forge, repo, runner, alarms, request, &mut report);
    if let Err(error) = &result {
        let unknown = matches!(
            error,
            LandingError::PublicationUnknown
                | LandingError::PublicationUnknownAt { .. }
                | LandingError::PublicationStale { .. }
                | LandingError::RecoveryRequired { .. }
        );
        for number in numbers {
            report.outcomes.entry(*number).or_insert(if unknown {
                Outcome::PendingUnknown
            } else {
                Outcome::Excluded
            });
        }
    }
    report.local_steps = repo.take_local_reports();
    let summary = receipt::write_run_summary(state, &report, shadow);
    match (result, summary) {
        (Ok(()), Ok(())) => Ok(report),
        (Err(cause), summary) => {
            if let Err(error) = summary {
                report
                    .warnings
                    .push(format!("could not write run summary: {error}"));
            }
            Err(LandingError::RunFailed {
                cause: Box::new(cause),
                report: Box::new(report),
            })
        }
        (Ok(()), Err(cause)) => Err(LandingError::RunFailed {
            cause: Box::new(cause),
            report: Box::new(report),
        }),
    }
}

fn run_inner(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    request: Request<'_>,
    report: &mut Report,
) -> Result<(), LandingError> {
    let _lock = receipt::lock(request.state)?;
    if request.settings.post_merge.revert_forward && !request.settings.post_merge.bisect {
        report.warnings.push(
            "post_merge.revert_forward requires post_merge.bisect to identify a culprit".into(),
        );
    }
    if request.settings.post_merge.revert_forward && !forge.supports_revert_forward() {
        report.warnings.push(
            "revert-forward is configured but this forge adapter cannot open a revert pull request"
                .into(),
        );
    }
    if request.settings.publish_settle_seconds == 0 {
        report.warnings.push(
            "publish_settle_seconds = 0 disables the in-flight publication settle window".into(),
        );
    }
    let repository = forge.repository_identity()?;
    recover(forge, repo, runner, alarms, &request, &repository, report)?;
    let limit = usize::try_from(request.settings.batch_size)
        .map_err(|_| LandingError::Failed("batch size exceeds platform limit".into()))?;
    if limit == 0 {
        return Err(LandingError::Failed("batch size must be positive".into()));
    }
    let mut queue = VecDeque::new();
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<_> = request
        .numbers
        .iter()
        .copied()
        .filter(|number| {
            seen.insert(*number) && report.outcomes.get(number) != Some(&Outcome::Landed)
        })
        .collect();
    let mut open = Vec::new();
    for number in unique {
        let info = forge.pull_request(number)?;
        if info.state == super::ports::PullRequestState::Merged && !request.shadow {
            let pr = EligiblePr {
                number,
                head: info.head,
                approval: String::new(),
                carried_paths: Vec::new(),
                accepted: None,
                base_branch: info.base_branch,
            };
            report.outcomes.insert(number, Outcome::Landed);
            let result = repo.follow_local(&pr);
            super::ports::sync_project(repo, forge, runner, alarms);
            result?;
        } else {
            open.push(number);
        }
    }
    let mut deferred = Vec::new();
    for numbers in open.chunks(limit) {
        let approval_target = approval_target(repo, &request)?;
        let (selected, reasons) = candidate::select_batch_with_reasons(
            forge,
            &mut request.approvals.at(repo, &approval_target),
            numbers,
            &open,
            request.target,
            limit,
            request.settings.batch_wait_seconds,
        )?;
        report.exclusion_reasons.extend(reasons);
        report
            .warnings
            .extend(selected.iter().filter_map(accepted_warning));
        for number in numbers {
            if !selected.iter().any(|pr| pr.number == *number) {
                report.outcomes.entry(*number).or_insert(Outcome::Excluded);
                if let Some(stack) = forge.pull_request(*number)?.stack {
                    let members = forge.stack_members(stack.number)?;
                    if let Some(position) =
                        members.iter().position(|member| member.number == *number)
                        && members[..position].iter().any(|member| {
                            member.state != super::ports::PullRequestState::Merged
                                && open.contains(&member.number)
                        })
                    {
                        deferred.push(*number);
                    }
                }
            }
        }
        queue.push_back(selected);
    }
    while let Some(mut prs) = queue.pop_front() {
        if prs.is_empty() {
            continue;
        }
        gate::await_pr_ci(
            forge,
            &prs,
            &request.settings.pr_ci,
            request.settings.poll_seconds,
        )?;
        let mut rebuilds = 0;
        loop {
            let base = fetch_target(repo, request.target, request.shadow)?;
            let candidate = match candidate::build_candidate(
                repo,
                &base,
                &prs,
                &request.settings.candidate_ref_prefix,
            ) {
                Ok(candidate) => candidate,
                Err(LandingError::Conflict(number)) => {
                    let (before, after) = split::conflict_prefix(&prs, number)?;
                    report.outcomes.insert(number, Outcome::Conflicted);
                    if !after.is_empty() {
                        queue.push_front(after);
                    }
                    if !before.is_empty() {
                        queue.push_front(before);
                    }
                    break;
                }
                Err(error) => return Err(error),
            };
            let evidence = cleanup_unpublished(
                runner.gate(&candidate, request.gate, request.settings.poll_seconds),
                repo,
                &candidate,
            )?;
            let (result, unmet) = gate::assess_evidence(&candidate, request.gate, &evidence);
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let message = error.to_string();
                    return cleanup_unpublished(
                        Err(LandingError::UnmetGate(
                            if unmet.is_empty() || message.contains(&unmet) {
                                message
                            } else {
                                format!("{message}; {unmet}")
                            },
                        )),
                        repo,
                        &candidate,
                    );
                }
            };
            let durations = match (&evidence.pool, evidence.duration_seconds) {
                (Some(pool), Some(duration)) => vec![(pool.clone(), duration)],
                _ => Vec::new(),
            };
            let cost = if durations.is_empty() {
                CostEstimate::Unknown {
                    reason: "gate pool or duration unavailable".into(),
                }
            } else {
                cleanup_unpublished(
                    budget::estimate(&request.settings.budget, &durations),
                    repo,
                    &candidate,
                )?
            };
            let warning = cleanup_unpublished(
                budget::check(&request.settings.budget, &cost),
                repo,
                &candidate,
            )?;
            if let Some(warning) = warning {
                report.warnings.push(warning);
            }
            if result == GateResult::Failed {
                let parts = if prs.len() == 1 {
                    Vec::new()
                } else {
                    split::red_prefixes(&prs)?
                };
                let mut outcomes = BTreeMap::new();
                if prs.len() == 1 {
                    let names = unmet.clone();
                    report.warnings.push(format!(
                        "pull request #{}: failed gate checks: {}",
                        prs[0].number, names
                    ));
                    report.outcomes.insert(prs[0].number, Outcome::Red);
                    outcomes.insert(prs[0].number, Outcome::Red);
                } else {
                    for pr in &prs {
                        outcomes.insert(pr.number, Outcome::PendingUnknown);
                    }
                }
                let decision = cleanup_unpublished(
                    publish::decide_candidate(
                        forge,
                        request.target,
                        request.settings.publish,
                        request.settings.method,
                        &candidate,
                    ),
                    repo,
                    &candidate,
                )?;
                let mut record = LandingReceipt::new(
                    &candidate,
                    request.policy.clone(),
                    request.gate.clone(),
                    evidence,
                    decision,
                    cost.clone(),
                    request.shadow,
                );
                record.outcomes = outcomes;
                record.split = parts
                    .iter()
                    .map(|part| part.iter().map(|pr| pr.number).collect())
                    .collect();
                cleanup_unpublished(
                    receipt::write_receipt(request.state, &record),
                    repo,
                    &candidate,
                )?;
                report.receipts.push(record);
                for part in parts.into_iter().rev() {
                    queue.push_front(part);
                }
                repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                break;
            }
            if result != GateResult::Passed {
                let checks = unmet.clone();
                return cleanup_unpublished(Err(LandingError::UnmetGate(checks)), repo, &candidate);
            }
            let decision = cleanup_unpublished(
                publish::decide_candidate(
                    forge,
                    request.target,
                    request.settings.publish,
                    request.settings.method,
                    &candidate,
                ),
                repo,
                &candidate,
            )?;
            let revalidated = publish::revalidate_with_reasons(
                forge,
                &mut request.approvals.at(repo, &candidate.base),
                &candidate,
            );
            let excluded = cleanup_unpublished(revalidated, repo, &candidate)?;
            if !excluded.is_empty() {
                for (pr, reason) in &excluded {
                    report.outcomes.insert(pr.number, Outcome::Excluded);
                    report.exclusion_reasons.insert(pr.number, reason.clone());
                }
                let retained: Vec<_> = prs
                    .iter()
                    .filter(|pr| {
                        !excluded
                            .iter()
                            .any(|(excluded, _)| excluded.number == pr.number)
                    })
                    .cloned()
                    .collect();
                if !retained.is_empty() {
                    queue.push_front(retained);
                }
                repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                break;
            }
            let observed_target = fetch_target(repo, request.target, request.shadow);
            if cleanup_unpublished(observed_target, repo, &candidate)? != base {
                repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                consume_rebuild(&mut rebuilds, request.settings.max_rebuilds)?;
                continue;
            }
            let publication = if request.shadow {
                PublishResult::default()
            } else {
                // Write the intent before publishing, so a crash or lost response mid-publication
                // leaves a marker for the next run to reconcile. Definite outcomes clear it below.
                let mut attempt = PendingAttempt {
                    repository: repository.clone(),
                    target: request.target.into(),
                    candidate: candidate.clone(),
                    policy: request.policy.clone(),
                    gate_definition: request.gate.clone(),
                    gate_evidence: evidence.clone(),
                    publish: decision.clone(),
                    cost: cost.clone(),
                    settled: None,
                    dispatched: Some(Vec::new()),
                    started_at: super::reconcile::now_seconds(),
                };
                receipt::record_pending(request.state, &attempt)?;
                let mut progress = Vec::new();
                let published = match decision.mode {
                    PublishMode::FastForward => {
                        publish::fast_forward(repo, request.target, &candidate)
                    }
                    PublishMode::RestOrdered => publish::rest_ordered(
                        forge,
                        repo,
                        request.approvals,
                        request.target,
                        &candidate,
                        request.settings.method,
                        &mut progress,
                        &mut |number| {
                            attempt.dispatched.get_or_insert_default().push(number);
                            receipt::record_pending(request.state, &attempt)
                        },
                    ),
                };
                match published {
                    Ok(landed) => landed,
                    Err(error) => {
                        if !matches!(
                            error,
                            LandingError::Refused(_)
                                | LandingError::TargetMoved
                                | LandingError::PullRequestChanged(_)
                        ) {
                            recover(forge, repo, runner, alarms, &request, &repository, report)?;
                            prs.retain(|pr| {
                                report.outcomes.get(&pr.number) != Some(&Outcome::Landed)
                            });
                            if prs.is_empty() {
                                break;
                            }
                            return Err(error);
                        }
                        // Record the definite outcome before any fallible follow-up.
                        let mut record = LandingReceipt::new(
                            &candidate,
                            request.policy.clone(),
                            request.gate.clone(),
                            evidence.clone(),
                            decision.clone(),
                            cost.clone(),
                            false,
                        );
                        record.candidate_steps.truncate(progress.len());
                        record.pull_requests.truncate(progress.len());
                        for (index, step) in candidate.steps.iter().enumerate() {
                            if let Some(sha) = progress.get(index) {
                                record.outcomes.insert(step.pr.number, Outcome::Landed);
                                report.outcomes.insert(step.pr.number, Outcome::Landed);
                                record
                                    .approved_to_landed
                                    .insert(step.pr.head.clone(), sha.clone());
                                record
                                    .approved_to_landed
                                    .insert(step.commit.clone(), sha.clone());
                            } else {
                                record.outcomes.insert(step.pr.number, Outcome::Excluded);
                            }
                        }
                        let refusal =
                            matches!(error, LandingError::Refused(_)).then(|| error.to_string());
                        reconcile::settle(request.state, &mut attempt, record, refusal)?;
                        let record = reconcile::finish_settled(
                            forge,
                            repo,
                            request.approvals,
                            runner,
                            alarms,
                            request.state,
                            &mut attempt,
                        )?;
                        report.receipts.push(record);
                        prs.drain(..progress.len());
                        if let LandingError::PullRequestChanged(number) = error {
                            prs.retain(|pr| {
                                if pr.number == number {
                                    report.outcomes.insert(number, Outcome::Excluded);
                                    report.exclusion_reasons.insert(
                                        number,
                                        "head, base, or approval changed during publication".into(),
                                    );
                                    false
                                } else {
                                    true
                                }
                            });
                            if !prs.is_empty() {
                                consume_rebuild(&mut rebuilds, request.settings.max_rebuilds)?;
                                continue;
                            }
                            break;
                        }
                        match error {
                            LandingError::TargetMoved if !prs.is_empty() => {
                                consume_rebuild(&mut rebuilds, request.settings.max_rebuilds)?;
                                continue;
                            }
                            _ => return Err(error),
                        }
                    }
                }
            };
            let mut mapping = BTreeMap::new();
            let mut outcomes = BTreeMap::new();
            if request.shadow {
                for pr in &prs {
                    outcomes.insert(pr.number, Outcome::Landed);
                    report.outcomes.insert(pr.number, Outcome::Landed);
                }
            } else {
                if publication.landed.len() != prs.len() {
                    return Err(LandingError::PublicationUnknownAt {
                        marker: receipt::pending_path(request.state, &repository, &candidate)?,
                    });
                }
                for (pr, sha) in prs.iter().zip(publication.landed) {
                    mapping.insert(pr.head.clone(), sha.clone());
                    if let Some(step) = candidate
                        .steps
                        .iter()
                        .find(|step| step.pr.number == pr.number)
                    {
                        mapping.insert(step.commit.clone(), sha);
                    }
                    outcomes.insert(pr.number, Outcome::Landed);
                    report.outcomes.insert(pr.number, Outcome::Landed);
                }
            }
            let mut record = LandingReceipt::new(
                &candidate,
                request.policy.clone(),
                request.gate.clone(),
                evidence,
                decision,
                cost,
                request.shadow,
            );
            record.outcomes = outcomes;
            record.approved_to_landed = mapping;
            record.alarms = publication.alarms;
            if request.shadow {
                receipt::write_receipt(request.state, &record)?;
                repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                report.receipts.push(record);
            } else {
                let mut attempt = PendingAttempt {
                    repository: repository.clone(),
                    target: request.target.into(),
                    candidate: candidate.clone(),
                    policy: request.policy.clone(),
                    gate_definition: record.gate_definition.clone(),
                    gate_evidence: record.gate_evidence.clone(),
                    publish: record.publish.clone(),
                    cost: record.cost.clone(),
                    settled: None,
                    dispatched: None,
                    started_at: None,
                };
                reconcile::settle(request.state, &mut attempt, record, None)?;
                let record = reconcile::finish_settled(
                    forge,
                    repo,
                    request.approvals,
                    runner,
                    alarms,
                    request.state,
                    &mut attempt,
                )?;
                report.receipts.push(record);
            }
            break;
        }
        if !request.shadow && !deferred.is_empty() {
            let mut still_deferred = Vec::new();
            for number in deferred.drain(..) {
                let info = forge.pull_request(number)?;
                let ready = if let Some(stack) = info.stack {
                    let members = forge.stack_members(stack.number)?;
                    members
                        .iter()
                        .position(|member| member.number == number)
                        .is_some_and(|position| {
                            members[..position].iter().all(|member| {
                                member.state == super::ports::PullRequestState::Merged
                            })
                        })
                } else {
                    true
                };
                if ready {
                    let approval_target = approval_target(repo, &request)?;
                    let selected = candidate::select_batch(
                        forge,
                        &mut request.approvals.at(repo, &approval_target),
                        &[number],
                        request.target,
                        1,
                        0,
                    )?;
                    if !selected.is_empty() {
                        report.outcomes.remove(&number);
                        queue.push_back(selected);
                    }
                } else {
                    still_deferred.push(number);
                }
            }
            deferred = still_deferred;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use crate::pr::landing::fakes::{FakeAlarms, FakeForge, FakeGate, FakeRepo};
    use provenant_cli::domains::{ApprovalSource, LandOverride};
    use std::collections::BTreeMap;

    #[test]
    fn empty_selection_refuses_before_side_effects() -> Result<(), Box<dyn std::error::Error>> {
        let settings = LandSettings::default();
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let definition = GateDefinition {
            kind: provenant_cli::domains::GateKind::Checks,
            digest: "test".into(),
            checks: vec!["ci-status".into()],
            commands: vec![],
            issuer: None,
            executor: None,
            coverage: vec![],
            timeout_seconds: 3600,
            pool: None,
            env_passthrough: Vec::new(),
        };
        let mut forge = FakeForge::default();
        let mut repo = FakeRepo::default();
        let mut runner = FakeGate::default();
        let mut alarms = FakeAlarms::default();
        let result = run(
            &mut forge,
            &mut repo,
            &mut runner,
            &mut alarms,
            Request {
                approvals: Approvals::forge(),
                numbers: &[],
                target: "main",
                settings: &settings,
                policy: &policy,
                gate: &definition,
                shadow: false,
                state: std::path::Path::new("."),
            },
        );
        assert!(result.is_err());
        assert!(repo.pushes.is_empty());
        assert!(forge.merges.is_empty());
        assert!(runner.observed.is_empty());
        Ok(())
    }
    fn fixture(count: u64) -> (FakeForge, FakeRepo, FakeGate, LandSettings, GateDefinition) {
        use super::super::ports::{Approval, PullRequestInfo, PullRequestState};
        let mut forge = FakeForge {
            direct_push: true,
            ..FakeForge::default()
        };
        for n in 1..=count {
            let head = format!("{n:040x}");
            forge.pull_requests.insert(
                n,
                PullRequestInfo {
                    number: n,
                    head: head.clone(),
                    base_branch: "main".into(),
                    author: "author".into(),
                    state: PullRequestState::Open,
                    merged_commit: None,
                    stack: None,
                },
            );
            forge.heads.insert(n, head.clone());
            forge.approvals_at_head.insert(
                (n, head),
                vec![Approval {
                    reference: format!("approval-{n}"),
                    reviewer: "reviewer".into(),
                }],
            );
        }
        let repo = FakeRepo {
            target: "a".repeat(40),
            valid_commit_ids: true,
            ..FakeRepo::default()
        };
        let gate = FakeGate {
            auto: true,
            ..FakeGate::default()
        };
        let settings = LandSettings {
            batch_wait_seconds: 0,
            publish_settle_seconds: 0,
            pr_ci: provenant_cli::domains::PrCiSettings {
                mode: provenant_cli::domains::PrCiMode::None,
                ..Default::default()
            },
            ..LandSettings::default()
        };
        let mut definition = super::super::receipt::tests::definition();
        definition.checks = vec!["ci-status".into()];
        (forge, repo, gate, settings, definition)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test rig passes independent boundaries"
    )]
    fn exercise(
        forge: &mut FakeForge,
        repo: &mut FakeRepo,
        runner: &mut FakeGate,
        settings: &LandSettings,
        definition: &GateDefinition,
        numbers: &[u64],
        shadow: bool,
        state: &std::path::Path,
    ) -> Result<Report, LandingError> {
        exercise_with(
            forge,
            repo,
            runner,
            settings,
            definition,
            numbers,
            shadow,
            state,
            Approvals::forge(),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test rig passes independent boundaries"
    )]
    fn exercise_with(
        forge: &mut FakeForge,
        repo: &mut FakeRepo,
        runner: &mut FakeGate,
        settings: &LandSettings,
        definition: &GateDefinition,
        numbers: &[u64],
        shadow: bool,
        state: &std::path::Path,
        approvals: Approvals<'_>,
    ) -> Result<Report, LandingError> {
        let policy = LandPolicy::resolve(
            settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .map_err(|e| LandingError::Failed(e.to_string()))?;
        run(
            forge,
            repo,
            runner,
            &mut FakeAlarms::default(),
            Request {
                numbers,
                target: "main",
                settings,
                policy: &policy,
                gate: definition,
                shadow,
                state,
                approvals,
            },
        )
        .map_err(|error| match error {
            LandingError::RunFailed { cause, .. } => *cause,
            error => error,
        })
    }

    #[test]
    fn stack_lost_response_recovers_without_remerging() -> Result<(), Box<dyn std::error::Error>> {
        use super::super::ports::{PullRequestState, StackMember, StackMembership};
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        let target = std::rc::Rc::new(std::cell::RefCell::new(repo.target.clone()));
        forge.shared_target = Some(target.clone());
        repo.shared_target = Some(target);
        forge.valid_merge_ids = true;
        forge.lose_merge_response_at = Some(1);
        repo.trees.insert(format!("{:040x}", 2001), "tree-1".into());
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
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(forge.stack_merge_attempts, vec![1]);
        assert_eq!(forge.pull_requests[&2].base_branch, "main");
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn stack_requested_out_of_order_gates_each_retargeted_member()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::super::ports::{PullRequestState, StackMember, StackMembership};
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(2);
        settings.batch_size = 1;
        let target = std::rc::Rc::new(std::cell::RefCell::new(repo.target.clone()));
        forge.shared_target = Some(target.clone());
        repo.shared_target = Some(target);
        forge.valid_merge_ids = true;
        for number in 1..=2 {
            repo.trees
                .insert(format!("{:040x}", 2000 + number), format!("tree-{number}"));
        }
        forge.lose_merge_response_at = Some(1);
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
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[2, 1],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::Landed);
        assert_eq!(forge.stack_merge_attempts, vec![1, 2]);
        assert_eq!(report.receipts.len(), 2);
        Ok(())
    }

    /// PR 1 was reviewed at its head, PR 2 at an older head it has since
    /// merged the target into, and PR 3 at an older head before one more change.
    #[test]
    fn receipt_approval_lands_reviewed_pull_requests_without_forge_approval()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::super::{fakes::FakeReviews, ports::ReviewRecord};
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(3);
        forge.approvals_at_head.clear();
        let target = repo.target.clone();
        let reviewed_base = "b".repeat(40);
        repo.parents
            .insert(target.clone(), vec![reviewed_base.clone()]);
        let reviewed = |n: u64| format!("{:040x}", 0xa0 + n);
        // PR 1's head is the reviewed head, on the reviewed base.
        repo.parents
            .insert(format!("{:040x}", 1), vec![reviewed_base.clone()]);
        for n in [2, 3] {
            let head = format!("{n:040x}");
            repo.parents
                .insert(head.clone(), vec![reviewed(n), target.clone()]);
            let tree = format!("tree-{}-{target}", reviewed(n));
            let tree = if n == 3 { format!("{tree}+line") } else { tree };
            repo.trees.insert(head, tree);
        }
        let record = |n: u64, head: String| ReviewRecord {
            reference: format!("delivery-{n}"),
            reviewer: "reviewer".into(),
            head,
            base: reviewed_base.clone(),
        };
        let reviews = FakeReviews::of(BTreeMap::from([
            (1, vec![record(1, format!("{:040x}", 1))]),
            (2, vec![record(2, reviewed(2))]),
            (3, vec![record(3, reviewed(3))]),
        ]));
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2, 3],
            false,
            state.path(),
        )?;
        assert!(
            report
                .outcomes
                .values()
                .all(|outcome| *outcome == Outcome::Excluded)
        );
        assert!(forge.merges.is_empty() && repo.pushes.is_empty());

        settings.approval = ApprovalSource::Receipt;
        let report = exercise_with(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2, 3],
            false,
            state.path(),
            Approvals::new(ApprovalSource::Receipt, &reviews),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::Landed);
        assert_eq!(report.outcomes[&3], Outcome::Excluded);
        assert_eq!(
            report.receipts[0]
                .pull_requests
                .iter()
                .map(|pr| pr.approval.as_str())
                .collect::<Vec<_>>(),
            ["delivery-1", "delivery-2"]
        );
        Ok(())
    }

    /// PR 2's record names a head whose history this repository lacks, so
    /// its proof cannot be read. That excludes PR 2 alone, with the reason.
    #[test]
    fn an_unprovable_review_record_excludes_only_its_pull_request()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::super::{fakes::FakeReviews, ports::ReviewRecord};
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(3);
        forge.approvals_at_head.clear();
        let target = repo.target.clone();
        let reviewed_base = "b".repeat(40);
        repo.parents
            .insert(target.clone(), vec![reviewed_base.clone()]);
        let reviewed = format!("{:040x}", 0xa2);
        // PR 2 merged the target into its reviewed head; its tree is unreadable.
        repo.parents
            .insert(format!("{:040x}", 2), vec![reviewed.clone(), target]);
        for n in [1, 3] {
            repo.parents
                .insert(format!("{n:040x}"), vec![reviewed_base.clone()]);
        }
        let record = |n: u64, head: String| ReviewRecord {
            reference: format!("delivery-{n}"),
            reviewer: "reviewer".into(),
            head,
            base: reviewed_base.clone(),
        };
        let reviews = FakeReviews::of(BTreeMap::from([
            (1, vec![record(1, format!("{:040x}", 1))]),
            (2, vec![record(2, reviewed)]),
            (3, vec![record(3, format!("{:040x}", 3))]),
        ]));
        settings.approval = ApprovalSource::Receipt;
        let state = tempfile::tempdir()?;
        let report = exercise_with(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2, 3],
            false,
            state.path(),
            Approvals::new(ApprovalSource::Receipt, &reviews),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::Excluded);
        assert_eq!(report.outcomes[&3], Outcome::Landed);
        assert!(
            report.exclusion_reasons[&2].starts_with("unprovable: delivery-2"),
            "{}",
            report.exclusion_reasons[&2]
        );
        Ok(())
    }

    /// PR 1 was reviewed at its current head against the head of the branch
    /// below it, then retargeted past that branch. The head is unchanged but
    /// the lower branch's changes were never reviewed, so the record does not
    /// approve it.
    #[test]
    fn a_receipt_does_not_approve_its_head_once_the_base_leaves_the_reviewed_base()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::super::{fakes::FakeReviews, ports::ReviewRecord};
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(1);
        forge.approvals_at_head.clear();
        let fork = "b".repeat(40);
        let lower = "c".repeat(40);
        let head = forge.pull_requests[&1].head.clone();
        repo.parents.insert(repo.target.clone(), vec![fork.clone()]);
        repo.parents.insert(lower.clone(), vec![fork]);
        repo.parents.insert(head.clone(), vec![lower.clone()]);
        let reviews = FakeReviews::of(BTreeMap::from([(
            1,
            vec![ReviewRecord {
                reference: "delivery-1".into(),
                reviewer: "reviewer".into(),
                head,
                base: lower,
            }],
        )]));
        settings.approval = ApprovalSource::Receipt;
        let state = tempfile::tempdir()?;
        let report = exercise_with(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
            Approvals::new(ApprovalSource::Receipt, &reviews),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Excluded);
        assert!(forge.merges.is_empty() && repo.pushes.is_empty());
        Ok(())
    }

    /// An unreadable review store is not a missing approval: the run fails
    /// with the reason and merges nothing.
    #[test]
    fn a_review_store_read_failure_fails_the_run_and_merges_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::super::fakes::FakeReviews;
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(2);
        forge.approvals_at_head.clear();
        let reviews = FakeReviews {
            failure: Some("read review records: store unreadable".into()),
            ..FakeReviews::default()
        };
        settings.approval = ApprovalSource::Receipt;
        let state = tempfile::tempdir()?;
        let result = exercise_with(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2],
            false,
            state.path(),
            Approvals::new(ApprovalSource::Receipt, &reviews),
        );
        let error = result.err().ok_or("the run should fail")?;
        assert!(error.to_string().contains("store unreadable"), "{error}");
        assert!(forge.merges.is_empty() && repo.pushes.is_empty());
        Ok(())
    }

    /// A retired receipt beside an accepted record that covers the head does
    /// not block it; a pull request with only a retired receipt is excluded
    /// with the remedy.
    #[test]
    fn an_accepted_record_lands_despite_a_retired_receipt() -> Result<(), Box<dyn std::error::Error>>
    {
        use super::super::{fakes::FakeReviews, ports::ReviewRecord};
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(2);
        forge.approvals_at_head.clear();
        let reviewed_base = "b".repeat(40);
        repo.parents
            .insert(repo.target.clone(), vec![reviewed_base.clone()]);
        for n in [1u64, 2] {
            repo.parents
                .insert(format!("{n:040x}"), vec![reviewed_base.clone()]);
        }
        let retired = "receipt shape retired: save it again".to_owned();
        let reviews = FakeReviews {
            records: BTreeMap::from([(
                1,
                vec![ReviewRecord {
                    reference: "delivery-1".into(),
                    reviewer: "reviewer".into(),
                    head: format!("{:040x}", 1),
                    base: reviewed_base,
                }],
            )]),
            unusable: BTreeMap::from([(1, vec![retired.clone()]), (2, vec![retired.clone()])]),
            failure: None,
        };
        settings.approval = ApprovalSource::Receipt;
        let state = tempfile::tempdir()?;
        let report = exercise_with(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2],
            false,
            state.path(),
            Approvals::new(ApprovalSource::Receipt, &reviews),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::Excluded);
        assert_eq!(report.exclusion_reasons[&2], retired);
        Ok(())
    }

    #[test]
    fn driver_three_prs_gate_once_and_publish_ordered_parents()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(3);
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2, 3],
            false,
            state.path(),
        )?;
        assert_eq!(gate.observed.len(), 1);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("publish_settle_seconds = 0"))
        );
        let record = &report.receipts[0];
        assert!(record.candidate_ref.starts_with("land/1-"));
        for step in &record.candidate_steps {
            assert_eq!(repo.parents[&step.commit][1], step.pr.head);
        }
        assert_eq!(report.outcomes.len(), 3);
        assert!(
            report
                .outcomes
                .values()
                .all(|outcome| *outcome == Outcome::Landed)
        );
        assert_eq!(
            repo.pushes
                .iter()
                .filter(|(name, _, _)| name == "main")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn driver_rebuilds_moved_target_then_reports_contention()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(1);
        settings.max_rebuilds = 1;
        repo.fetches = [
            "a".repeat(40),
            "b".repeat(40),
            "b".repeat(40),
            "c".repeat(40),
        ]
        .into();
        let state = tempfile::tempdir()?;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ),
            Err(LandingError::Contention)
        ));
        assert_eq!(gate.observed.len(), 2);
        assert!(
            repo.pushes
                .iter()
                .all(|(name, _, _)| name.starts_with("land/"))
        );
        Ok(())
    }

    #[test]
    fn ordered_target_move_rebuilds_open_same_head_remainder()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(2);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        settings.max_rebuilds = 1;
        let base = "a".repeat(40);
        let moved = "b".repeat(40);
        let first = format!("{:040x}", 2001);
        let second = format!("{:040x}", 2002);
        repo.parents.insert(moved.clone(), vec![first.clone()]);
        repo.trees.insert(second.clone(), "final".into());
        let target = std::rc::Rc::new(std::cell::RefCell::new(base));
        forge.shared_target = Some(target.clone());
        forge.move_after_merge = Some((1, moved));
        repo.shared_target = Some(target);
        let state = tempfile::tempdir()?;
        let result = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2],
            false,
            state.path(),
        );
        let report = result?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::Landed);
        assert!(
            report
                .receipts
                .iter()
                .any(|r| r.outcomes.get(&1) == Some(&Outcome::Landed))
        );
        assert_eq!(
            forge.merges.iter().map(|merge| merge.0).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            0
        );
        Ok(())
    }

    #[test]
    fn driver_shadow_changes_only_candidate_ref_and_records_decision()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            true,
            state.path(),
        )?;
        assert_eq!(repo.target, "a".repeat(40));
        assert_eq!(repo.pushes.len(), 1);
        assert!(repo.pushes[0].0.starts_with("land/"));
        assert!(repo.refs.is_empty());
        assert!(forge.merges.is_empty());
        let record = &report.receipts[0];
        assert!(record.shadow);
        assert_eq!(record.publish.mode, PublishMode::FastForward);
        assert!(!record.publish.reason.is_empty());
        assert_eq!(record.outcomes[&1], Outcome::Landed);
        Ok(())
    }

    #[test]
    fn driver_isolates_one_red_in_eight_within_seven_gates()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(8);
        gate.pool = Some("test-pool".into());
        gate.duration_seconds = Some(1);
        settings
            .budget
            .rate_micros_per_minute
            .insert("test-pool".into(), 60);
        gate.red_head = Some(format!("{:040x}", 4));
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &(1..=8).collect::<Vec<_>>(),
            false,
            state.path(),
        )?;
        assert!(gate.observed.len() <= 7);
        assert_eq!(report.outcomes[&4], Outcome::Red);
        assert!(
            report
                .receipts
                .iter()
                .any(|record| record.outcomes.get(&4) == Some(&Outcome::Red)
                    && record.cost == CostEstimate::Known { micros: 1 })
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("ci-status: failed"))
        );
        assert_eq!(
            report
                .outcomes
                .values()
                .filter(|o| **o == Outcome::Landed)
                .count(),
            7
        );
        Ok(())
    }
    #[test]
    fn driver_rejects_unmet_gate_evidence_without_publishing()
    -> Result<(), Box<dyn std::error::Error>> {
        for kind in ["missing", "skipped", "neutral", "issuer", "tree", "failed"] {
            let (mut forge, mut repo, mut gate, settings, mut definition) = fixture(1);
            definition.issuer = Some("trusted".into());
            definition.checks.push("lint".into());
            match kind {
                "missing" => gate.omit_check = true,
                "skipped" => gate.check_result = Some(GateResult::Skipped),
                "neutral" => gate.check_result = Some(GateResult::Neutral),
                "issuer" => gate.wrong_issuer = true,
                "tree" => gate.wrong_tree = true,
                "failed" => gate.check_result = Some(GateResult::Failed),
                _ => unreachable!(),
            }
            let state = tempfile::tempdir()?;
            let result = exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path(),
            );
            if kind == "failed" {
                let report = result?;
                assert_eq!(report.outcomes[&1], Outcome::Red);
                assert!(
                    report
                        .warnings
                        .iter()
                        .any(|message| message.contains("ci-status: failed")
                            && message.contains("lint: failed"))
                );
            } else {
                let message = match result {
                    Err(error) => error.to_string(),
                    Ok(_) => panic!("{kind} passed"),
                };
                assert!(message.contains("ci-status:"), "{message}");
                assert!(message.contains("lint"), "{message}");
                if kind == "tree" {
                    assert!(message.contains("tree"));
                }
            }
            assert_eq!(repo.target, "a".repeat(40));
            assert!(repo.refs.keys().all(|name| !name.starts_with("land/")));
            assert!(
                repo.pushes
                    .iter()
                    .all(|(name, _, _)| name.starts_with("land/"))
            );
        }
        Ok(())
    }

    #[test]
    fn driver_conflict_isolates_pr_and_publishes_clean_prefix()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        repo.conflicts
            .insert((format!("{:040x}", 1000), format!("{:040x}", 2)));
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::Conflicted);
        assert_eq!(gate.observed.len(), 1);
        let summary = std::fs::read_dir(state.path().join("landing/receipts"))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("run-"))
            })
            .ok_or("missing run summary")?;
        let summary: serde_json::Value = serde_json::from_slice(&std::fs::read(summary)?)?;
        assert_eq!(summary["outcomes"]["2"], "conflicted");
        Ok(())
    }

    #[test]
    fn driver_auto_rest_alarms_on_tree_mismatch() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        repo.fetches = [
            "a".repeat(40),
            "a".repeat(40),
            "a".repeat(40),
            "landed-1".into(),
        ]
        .into();
        repo.trees.insert("landed-1".into(), "different".into());
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert_eq!(report.receipts[0].publish.mode, PublishMode::RestOrdered);
        assert_eq!(forge.merges.len(), 1);
        assert_eq!(report.receipts[0].alarms[0].code, "LAND_TREE_MISMATCH");
        Ok(())
    }

    #[test]
    fn driver_recovers_lost_push_and_runs_approval_alarm() -> Result<(), Box<dyn std::error::Error>>
    {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        repo.lose_push_response = true;
        repo.refuse_delete = true;
        let state = tempfile::tempdir()?;
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let result = run(
            &mut forge,
            &mut repo,
            &mut gate,
            &mut FakeAlarms::default(),
            Request {
                approvals: Approvals::forge(),
                numbers: &[1],
                target: "main",
                settings: &settings,
                policy: &policy,
                gate: &definition,
                shadow: false,
                state: state.path(),
            },
        );
        let Err(LandingError::RunFailed { cause, report }) = result else {
            panic!("expected cleanup failure with report")
        };
        assert!(matches!(*cause, LandingError::Failed(ref message) if message == "delete refused"));
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.receipts.len(), 1);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        forge.approvals_at_head.clear();
        repo.lose_push_response = false;
        repo.refuse_delete = false;
        let mut alarms = FakeAlarms::default();
        let report = run(
            &mut forge,
            &mut repo,
            &mut gate,
            &mut alarms,
            Request {
                approvals: Approvals::forge(),
                numbers: &[1],
                target: "main",
                settings: &settings,
                policy: &policy,
                gate: &definition,
                shadow: false,
                state: state.path(),
            },
        )?;
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        assert!(alarms.0.is_empty()); // Follow completed before cleanup failed.
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        Ok(())
    }
    #[test]
    fn driver_excludes_changed_head_and_rebuilds_remaining_prs()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        forge.heads.insert(1, "changed".into());
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Excluded);
        assert_eq!(report.outcomes[&2], Outcome::Landed);
        assert_eq!(gate.observed.len(), 2);
        assert_eq!(report.receipts[0].pull_requests.len(), 1);
        Ok(())
    }

    #[test]
    fn driver_opt_in_without_label_does_not_wait_for_pr_ci()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(1);
        settings.pr_ci.mode = provenant_cli::domains::PrCiMode::OptIn;
        settings.pr_ci.label = Some("full-ci".into());
        forge.direct_push = false;
        forge.refuse_merge = Some(1); // Forge-required checks remain authoritative.
        let head = format!("{:040x}", 1);
        let mut pending = super::super::receipt::tests::evidence(&head);
        pending.result = GateResult::Pending;
        forge.check_runs.insert(head, vec![pending]);
        let state = tempfile::tempdir()?;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ),
            Err(LandingError::Refused(_))
        ));
        assert_eq!(gate.observed.len(), 1);
        assert!(forge.merges.is_empty());
        Ok(())
    }

    #[test]
    fn bundled_defaults_select_batch_and_ci_status_for_protected_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let defaults = LandSettings::default();
        assert_eq!(
            defaults.integration,
            provenant_cli::domains::Integration::Batch
        );
        assert_eq!(defaults.gate.checks, vec!["ci-status"]);
        let mut forge = FakeForge::default();
        let decision = publish::decide(&mut forge, "main", defaults.publish, defaults.method)?;
        assert_eq!(decision.mode, PublishMode::RestOrdered);
        Ok(())
    }
    #[test]
    fn shadow_red_receipts_record_split_and_outcome() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        gate.red_head = Some(format!("{:040x}", 2));
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2],
            true,
            state.path(),
        )?;
        assert_eq!(report.receipts[0].split, vec![vec![1], vec![2]]);
        assert!(
            report
                .receipts
                .iter()
                .any(|record| record.outcomes.get(&2) == Some(&Outcome::Red))
        );
        assert_eq!(repo.target, "a".repeat(40));
        assert!(
            repo.pushes
                .iter()
                .all(|(name, _, _)| name.starts_with("land/"))
        );
        Ok(())
    }
    #[test]
    fn shadow_decisions_match_real_run_on_same_fixture() -> Result<(), Box<dyn std::error::Error>> {
        let (mut sf, mut sr, mut sg, settings, definition) = fixture(2);
        let (mut rf, mut rr, mut rg, _, _) = fixture(2);
        sg.red_head = Some(format!("{:040x}", 2));
        rg.red_head = sg.red_head.clone();
        let shadow_state = tempfile::tempdir()?;
        let real_state = tempfile::tempdir()?;
        let shadow = exercise(
            &mut sf,
            &mut sr,
            &mut sg,
            &settings,
            &definition,
            &[1, 2],
            true,
            shadow_state.path(),
        )?;
        let real = exercise(
            &mut rf,
            &mut rr,
            &mut rg,
            &settings,
            &definition,
            &[1, 2],
            false,
            real_state.path(),
        )?;
        assert_eq!(shadow.outcomes, real.outcomes);
        assert_eq!(
            shadow
                .receipts
                .iter()
                .map(|r| (&r.publish, &r.split, &r.outcomes))
                .collect::<Vec<_>>(),
            real.receipts
                .iter()
                .map(|r| (&r.publish, &r.split, &r.outcomes))
                .collect::<Vec<_>>()
        );
        assert!(sr.refs.is_empty());
        Ok(())
    }

    #[test]
    fn refusal_clears_marker_without_retry() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        repo.refuse_target_push = true;
        let state = tempfile::tempdir()?;
        repo.pending_dir_to_observe = Some(state.path().join("landing/pending"));
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ),
            Err(LandingError::Refused(_))
        ));
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            0
        );
        // The marker is written ahead of publication and cleared once the refusal is definite.
        assert!(repo.saw_pending_on_target_push);
        assert_eq!(
            repo.push_attempts
                .iter()
                .filter(|name| name.as_str() == "main")
                .count(),
            1
        );
        Ok(())
    }
    #[test]
    fn refused_merge_and_failed_cleanup_cannot_be_retried() -> Result<(), Box<dyn std::error::Error>>
    {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge.refuse_merge = Some(1);
        repo.refuse_delete = true;
        let state = tempfile::tempdir()?;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ),
            Err(LandingError::Failed(_))
        ));
        assert!(
            receipt::pending(state.path(), "fake-repository", "main")?[0]
                .settled
                .is_some()
        );
        repo.refuse_delete = false;
        let pushes = repo.push_attempts.len();
        assert!(matches!(
            reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut Vec::new(),
            ),
            Err(LandingError::Refused(_))
        ));
        assert_eq!(repo.push_attempts.len(), pushes);
        assert_eq!(forge.merge_attempts, vec![1]);
        Ok(())
    }

    #[test]
    fn published_tree_read_failure_remains_pending_until_mismatch_checked()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        let target = std::rc::Rc::new(std::cell::RefCell::new(repo.target.clone()));
        forge.shared_target = Some(target.clone());
        repo.shared_target = Some(target);
        let state = tempfile::tempdir()?;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ),
            Err(LandingError::Failed(_))
        ));
        assert!(
            receipt::pending(state.path(), "fake-repository", "main")?[0]
                .settled
                .is_some()
        );
        let landed = format!("{:040x}", 2001);
        repo.trees.insert(landed, "unexpected-tree".into());
        let mut alarms = FakeAlarms::default();
        let mut recovered = Vec::new();
        reconcile::recover_pending(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut alarms,
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
        )?;
        assert!(
            alarms
                .0
                .iter()
                .any(|alarm| alarm.code == "LAND_TREE_MISMATCH"
                    && alarm.message.contains(&recovered[0].tree)
                    && alarm.message.contains("unexpected-tree"))
        );
        assert_eq!(recovered[0].outcomes[&1], Outcome::Landed);
        assert_eq!(forge.merges.len(), 1);
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn refused_recovery_push_and_merge_settle() -> Result<(), Box<dyn std::error::Error>> {
        for mode in [PublishMode::FastForward, PublishMode::RestOrdered] {
            let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
            let policy = LandPolicy::resolve(
                &settings,
                &BTreeMap::new(),
                None,
                None,
                &LandOverride::default(),
            )?;
            let state = tempfile::tempdir()?;
            let base = repo.target.clone();
            let pr = &forge.pull_requests[&1];
            let candidate = Candidate {
                base: base.clone(),
                commit: "candidate".into(),
                tree: "tree".into(),
                ref_name: "land/test".into(),
                steps: vec![CandidateStep {
                    pr: EligiblePr {
                        number: 1,
                        head: pr.head.clone(),
                        approval: "approval-1".into(),
                        carried_paths: Vec::new(),
                        accepted: None,
                        base_branch: "main".into(),
                    },
                    commit: "candidate".into(),
                    tree: "tree".into(),
                }],
            };
            repo.refs
                .insert(candidate.ref_name.clone(), candidate.commit.clone());
            repo.refuse_target_push = true;
            repo.target_ref = "main".into();
            forge.refuse_merge = Some(1);
            let fail = std::rc::Rc::new(std::cell::Cell::new(false));
            forge.fail_fetch_on_refusal = Some(fail.clone());
            repo.fail_fetch_on_push_refusal = Some(fail.clone());
            repo.fail_fetch = Some(fail);
            receipt::record_pending(
                state.path(),
                &PendingAttempt {
                    repository: "fake-repository".into(),
                    target: "main".into(),
                    candidate: candidate.clone(),
                    policy,
                    gate_definition: definition.clone(),
                    gate_evidence: super::super::receipt::tests::evidence("candidate"),
                    publish: PublishDecision {
                        mode,
                        reason: "test".into(),
                    },
                    cost: CostEstimate::Unknown {
                        reason: "test".into(),
                    },
                    settled: None,
                    dispatched: None,
                    started_at: None,
                },
            )?;
            let result = reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut Vec::new(),
            );
            assert!(matches!(result, Err(LandingError::Refused(_))));
            assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
            assert!(forge.merges.is_empty());
        }
        Ok(())
    }
    #[test]
    fn transport_retry_error_keeps_publishing_marker() -> Result<(), Box<dyn std::error::Error>> {
        for mode in [PublishMode::FastForward, PublishMode::RestOrdered] {
            let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
            let policy = LandPolicy::resolve(
                &settings,
                &BTreeMap::new(),
                None,
                None,
                &LandOverride::default(),
            )?;
            let state = tempfile::tempdir()?;
            let base = repo.target.clone();
            let pr = &forge.pull_requests[&1];
            let candidate = Candidate {
                base: base.clone(),
                commit: "candidate".into(),
                tree: "tree".into(),
                ref_name: "land/test".into(),
                steps: vec![CandidateStep {
                    pr: EligiblePr {
                        number: 1,
                        head: pr.head.clone(),
                        approval: "approval-1".into(),
                        carried_paths: Vec::new(),
                        accepted: None,
                        base_branch: "main".into(),
                    },
                    commit: "candidate".into(),
                    tree: "tree".into(),
                }],
            };
            repo.refs
                .insert(candidate.ref_name.clone(), candidate.commit.clone());
            repo.target_ref = "main".into();
            repo.lose_push_response = true;
            forge.fail_merge = Some(1);
            receipt::record_pending(
                state.path(),
                &PendingAttempt {
                    repository: "fake-repository".into(),
                    target: "main".into(),
                    candidate: candidate.clone(),
                    policy,
                    gate_definition: definition.clone(),
                    gate_evidence: super::super::receipt::tests::evidence("candidate"),
                    publish: PublishDecision {
                        mode,
                        reason: "test".into(),
                    },
                    cost: CostEstimate::Unknown {
                        reason: "test".into(),
                    },
                    settled: None,
                    dispatched: None,
                    started_at: None,
                },
            )?;
            let result = reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut Vec::new(),
            );
            assert!(matches!(
                result,
                Err(LandingError::PublicationUnknownAt { .. })
            ));
            let attempts = receipt::pending(state.path(), "fake-repository", "main")?;
            assert_eq!(attempts.len(), 1);
            assert!(attempts[0].settled.is_none());
            assert!(repo.local_followed.is_empty());
            assert!(forge.merges.is_empty());
        }
        Ok(())
    }
    fn recovery_does_not_retry_changed_pr(
        mode: PublishMode,
        change: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let state = tempfile::tempdir()?;
        let head = forge.pull_requests[&1].head.clone();
        let candidate = Candidate {
            base: repo.target.clone(),
            commit: "candidate".into(),
            tree: "tree".into(),
            ref_name: "land/test".into(),
            steps: vec![CandidateStep {
                pr: EligiblePr {
                    number: 1,
                    head: head.clone(),
                    approval: "approval-1".into(),
                    carried_paths: Vec::new(),
                    accepted: None,
                    base_branch: "main".into(),
                },
                commit: "candidate".into(),
                tree: "tree".into(),
            }],
        };
        repo.refs
            .insert(candidate.ref_name.clone(), candidate.commit.clone());
        match change {
            "head" => {
                forge.heads.insert(1, "new-head".into());
                forge.pull_requests.get_mut(&1).unwrap().head = "new-head".into();
            }
            "approval" => {
                forge.approvals_at_head.clear();
            }
            "base" => {
                forge.pull_requests.get_mut(&1).unwrap().base_branch = "other".into();
            }
            "closed" => {
                forge.pull_requests.get_mut(&1).unwrap().state =
                    super::super::ports::PullRequestState::Closed;
            }
            _ => unreachable!(),
        }
        receipt::record_pending(
            state.path(),
            &PendingAttempt {
                repository: "fake-repository".into(),
                target: "main".into(),
                candidate,
                policy,
                gate_definition: definition,
                gate_evidence: super::super::receipt::tests::evidence("candidate"),
                publish: PublishDecision {
                    mode,
                    reason: "fixture".into(),
                },
                cost: CostEstimate::Unknown {
                    reason: "fixture".into(),
                },
                settled: None,
                dispatched: None,
                started_at: None,
            },
        )?;
        let result = reconcile::recover_pending(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut Vec::new(),
        );
        assert!(forge.merges.is_empty());
        assert!(repo.pushes.is_empty());
        if mode == PublishMode::RestOrdered && change != "closed" {
            assert!(matches!(
                result,
                Err(LandingError::PublicationUnknownAt { .. })
            ));
            assert_eq!(
                receipt::pending(state.path(), "fake-repository", "main")?.len(),
                1
            );
        } else {
            result?;
            assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        }
        Ok(())
    }
    #[test]
    fn recovery_revalidates_heads() -> Result<(), Box<dyn std::error::Error>> {
        for mode in [PublishMode::FastForward, PublishMode::RestOrdered] {
            recovery_does_not_retry_changed_pr(mode, "head")?;
        }
        Ok(())
    }
    #[test]
    fn recovery_revalidates_approvals() -> Result<(), Box<dyn std::error::Error>> {
        for mode in [PublishMode::FastForward, PublishMode::RestOrdered] {
            recovery_does_not_retry_changed_pr(mode, "approval")?;
        }
        Ok(())
    }
    #[test]
    fn recovery_revalidates_bases() -> Result<(), Box<dyn std::error::Error>> {
        for mode in [PublishMode::FastForward, PublishMode::RestOrdered] {
            recovery_does_not_retry_changed_pr(mode, "base")?;
        }
        Ok(())
    }

    #[test]
    fn recovery_does_not_retry_closed_pr() -> Result<(), Box<dyn std::error::Error>> {
        for mode in [PublishMode::FastForward, PublishMode::RestOrdered] {
            recovery_does_not_retry_changed_pr(mode, "closed")?;
        }
        Ok(())
    }

    #[test]
    fn recovery_retains_open_prs_after_target_or_sibling_moves()
    -> Result<(), Box<dyn std::error::Error>> {
        for changed_sibling in [false, true] {
            let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
            let policy = LandPolicy::resolve(
                &settings,
                &BTreeMap::new(),
                None,
                None,
                &LandOverride::default(),
            )?;
            let state = tempfile::tempdir()?;
            let base = repo.target.clone();
            let steps = (1..=2)
                .map(|number| CandidateStep {
                    pr: EligiblePr {
                        number,
                        head: forge.pull_requests[&number].head.clone(),
                        approval: format!("approval-{number}"),
                        carried_paths: Vec::new(),
                        accepted: None,
                        base_branch: "main".into(),
                    },
                    commit: format!("step-{number}"),
                    tree: format!("tree-{number}"),
                })
                .collect();
            let candidate = Candidate {
                base,
                commit: "step-2".into(),
                tree: "tree-2".into(),
                ref_name: "land/test".into(),
                steps,
            };
            repo.refs
                .insert(candidate.ref_name.clone(), candidate.commit.clone());
            if changed_sibling {
                forge.heads.insert(2, "changed".into());
                forge.pull_requests.get_mut(&2).unwrap().head = "changed".into();
            } else {
                repo.target = "moved".into();
            }
            receipt::record_pending(
                state.path(),
                &PendingAttempt {
                    repository: "fake-repository".into(),
                    target: "main".into(),
                    candidate,
                    policy,
                    gate_definition: definition,
                    gate_evidence: super::super::receipt::tests::evidence("step-2"),
                    publish: PublishDecision {
                        mode: PublishMode::RestOrdered,
                        reason: "fixture".into(),
                    },
                    cost: CostEstimate::Unknown {
                        reason: "fixture".into(),
                    },
                    settled: None,
                    dispatched: None,
                    started_at: None,
                },
            )?;
            let mut recovered = Vec::new();
            let result = reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut recovered,
            );
            assert!(matches!(
                result,
                Err(LandingError::PublicationUnknownAt { .. })
            ));
            assert!(forge.merge_attempts.is_empty());
            assert_eq!(
                receipt::pending(state.path(), "fake-repository", "main")?.len(),
                1
            );
        }
        Ok(())
    }

    #[test]
    fn successful_recovery_merge_records_returned_commit() -> Result<(), Box<dyn std::error::Error>>
    {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let state = tempfile::tempdir()?;
        let head = forge.pull_requests[&1].head.clone();
        let candidate = Candidate {
            base: repo.target.clone(),
            commit: "candidate".into(),
            tree: "tree".into(),
            ref_name: "land/test".into(),
            steps: vec![CandidateStep {
                pr: EligiblePr {
                    number: 1,
                    head: head.clone(),
                    approval: "approval-1".into(),
                    carried_paths: Vec::new(),
                    accepted: None,
                    base_branch: "main".into(),
                },
                commit: "candidate".into(),
                tree: "tree".into(),
            }],
        };
        repo.refs
            .insert(candidate.ref_name.clone(), candidate.commit.clone());
        let target = std::rc::Rc::new(std::cell::RefCell::new(repo.target.clone()));
        forge.shared_target = Some(target.clone());
        repo.shared_target = Some(target);
        repo.trees.insert("landed-1".into(), "tree".into());
        receipt::record_pending(
            state.path(),
            &PendingAttempt {
                repository: "fake-repository".into(),
                target: "main".into(),
                candidate,
                policy,
                gate_definition: definition,
                gate_evidence: super::super::receipt::tests::evidence("candidate"),
                publish: PublishDecision {
                    mode: PublishMode::RestOrdered,
                    reason: "fixture".into(),
                },
                cost: CostEstimate::Unknown {
                    reason: "fixture".into(),
                },
                settled: None,
                dispatched: None,
                started_at: None,
            },
        )?;
        let mut recovered = Vec::new();
        reconcile::recover_pending(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
        )?;
        assert_eq!(forge.merges.len(), 1);
        assert_eq!(recovered[0].approved_to_landed[&head], "landed-1");
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn recovery_retains_ambiguous_stack_merge_until_it_is_observed()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(1);
        settings.publish_settle_seconds = 7;
        let mut attempt = super::super::receipt::tests::attempt("main", "candidate");
        attempt.policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        attempt.gate_definition = definition;
        attempt.candidate.base = repo.target.clone();
        attempt.candidate.steps = vec![CandidateStep {
            pr: EligiblePr {
                number: 1,
                head: forge.pull_requests[&1].head.clone(),
                approval: "revoked".into(),
                carried_paths: Vec::new(),
                accepted: None,
                base_branch: "main".into(),
            },
            commit: "candidate".into(),
            tree: "tree".into(),
        }];
        use super::super::ports::{Forge, PullRequestState, StackMembership};
        forge.pull_requests.get_mut(&1).unwrap().stack = Some(StackMembership {
            number: 42,
            size: 1,
            position: 1,
            base_branch: "main".into(),
        });
        let head = forge.pull_requests[&1].head.clone();
        attempt.candidate.steps[0].pr.approval = "approval-1".into();
        let state = tempfile::tempdir()?;
        receipt::record_pending(state.path(), &attempt)?;
        forge.fail_merge = Some(1);
        assert!(forge.merge_stack(1, &head, settings.method).is_err());
        forge.approvals_at_head.clear();
        let waited = std::rc::Rc::new(std::cell::Cell::new(false));
        let reads = std::rc::Rc::new(std::cell::Cell::new(0));
        forge.after_wait_reads = Some((waited.clone(), reads.clone()));
        let mut recovered = Vec::new();
        let result = reconcile::recover_pending_with(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
            &|duration| {
                assert_eq!(duration, std::time::Duration::from_secs(7));
                waited.set(true);
            },
        );
        assert!(matches!(
            result,
            Err(LandingError::PublicationUnknownAt { .. })
        ));
        assert!(waited.get());
        assert!(reads.get() > 0);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        assert_eq!(forge.merge_attempts, vec![1]);
        let pr = forge.pull_requests.get_mut(&1).unwrap();
        pr.state = PullRequestState::Merged;
        pr.merged_commit = Some("landed-1".into());
        repo.trees.insert("landed-1".into(), "tree".into());
        reconcile::recover_pending_with(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
            &|_| {},
        )?;
        assert_eq!(recovered[0].outcomes[&1], Outcome::Landed);
        assert_eq!(recovered[0].approved_to_landed[&head], "landed-1");
        assert!(repo.local_followed.contains(&1));
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn shadow_recovery_does_not_wait_or_mutate() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, _, _) = fixture(2);
        let state = tempfile::tempdir()?;
        let mut attempt = super::super::receipt::tests::attempt("main", "candidate");
        attempt.candidate.steps = (1..=2)
            .map(|n| CandidateStep {
                pr: EligiblePr {
                    number: n,
                    head: forge.pull_requests[&n].head.clone(),
                    approval: format!("approval-{n}"),
                    carried_paths: Vec::new(),
                    accepted: None,
                    base_branch: "main".into(),
                },
                commit: format!("step-{n}"),
                tree: format!("tree-{n}"),
            })
            .collect();
        receipt::record_pending(state.path(), &attempt)?;
        let result = reconcile::recover_pending_with(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            true,
            &mut Vec::new(),
            &|_| panic!("shadow recovery waited"),
        );
        assert!(matches!(result, Err(LandingError::RecoveryRequired { .. })));
        assert!(forge.merge_attempts.is_empty());
        assert!(repo.push_attempts.is_empty());
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?,
            vec![attempt]
        );
        Ok(())
    }

    #[test]
    fn recovery_excludes_members_never_dispatched() -> Result<(), Box<dyn std::error::Error>> {
        use super::super::ports::{PullRequestState, StackMembership};
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(2);
        settings.publish_unknown_warn_seconds = 0;
        forge.pull_requests.get_mut(&1).unwrap().stack = Some(StackMembership {
            number: 42,
            size: 2,
            position: 1,
            base_branch: "main".into(),
        });
        let state = tempfile::tempdir()?;
        let mut attempt = super::super::receipt::tests::attempt("main", "step-2");
        attempt.policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        attempt.gate_definition = definition;
        attempt.candidate.base = repo.target.clone();
        attempt.candidate.steps = (1..=2)
            .map(|n| CandidateStep {
                pr: EligiblePr {
                    number: n,
                    head: forge.pull_requests[&n].head.clone(),
                    approval: format!("approval-{n}"),
                    carried_paths: Vec::new(),
                    accepted: None,
                    base_branch: "main".into(),
                },
                commit: format!("step-{n}"),
                tree: format!("tree-{n}"),
            })
            .collect();
        attempt.dispatched = Some(vec![1]);
        repo.refs.insert(
            attempt.candidate.ref_name.clone(),
            attempt.candidate.commit.clone(),
        );
        receipt::record_pending(state.path(), &attempt)?;
        let mut recovered = Vec::new();
        let unknown = reconcile::recover_pending_with(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
            &|_| {},
        );
        let error = unknown.unwrap_err();
        assert!(matches!(error, LandingError::PublicationStale { .. }));
        assert!(error.to_string().contains("provenant land 1"));
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        forge.pull_requests.get_mut(&1).unwrap().state = PullRequestState::Merged;
        forge.pull_requests.get_mut(&1).unwrap().merged_commit = Some("landed-1".into());
        reconcile::recover_pending_with(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
            &|_| {},
        )?;
        assert_eq!(recovered[0].outcomes[&1], Outcome::Landed);
        assert_eq!(recovered[0].outcomes[&2], Outcome::Excluded);
        assert!(forge.merge_attempts.is_empty());
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn recovery_checks_target_before_each_ordered_retry() -> Result<(), Box<dyn std::error::Error>>
    {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let state = tempfile::tempdir()?;
        let base = repo.target.clone();
        let steps: Vec<_> = (1..=2)
            .map(|n| CandidateStep {
                pr: EligiblePr {
                    number: n,
                    head: forge.pull_requests[&n].head.clone(),
                    approval: format!("approval-{n}"),
                    carried_paths: Vec::new(),
                    accepted: None,
                    base_branch: "main".into(),
                },
                commit: format!("step-{n}"),
                tree: format!("tree-{n}"),
            })
            .collect();
        let candidate = Candidate {
            base: base.clone(),
            commit: "step-2".into(),
            tree: "tree-2".into(),
            ref_name: "land/test".into(),
            steps,
        };
        repo.refs
            .insert(candidate.ref_name.clone(), candidate.commit.clone());
        let target = std::rc::Rc::new(std::cell::RefCell::new(base));
        forge.shared_target = Some(target.clone());
        forge.move_after_merge = Some((1, "moved".into()));
        repo.shared_target = Some(target);
        repo.trees.insert("landed-2".into(), "tree-2".into());
        receipt::record_pending(
            state.path(),
            &PendingAttempt {
                repository: "fake-repository".into(),
                target: "main".into(),
                candidate,
                policy,
                gate_definition: definition,
                gate_evidence: super::super::receipt::tests::evidence("step-2"),
                publish: PublishDecision {
                    mode: PublishMode::RestOrdered,
                    reason: "fixture".into(),
                },
                cost: CostEstimate::Unknown {
                    reason: "fixture".into(),
                },
                settled: None,
                dispatched: None,
                started_at: None,
            },
        )?;
        let mut recovered = Vec::new();
        let result = reconcile::recover_pending(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut FakeAlarms::default(),
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
        );
        assert!(matches!(
            result,
            Err(LandingError::PublicationUnknownAt { .. })
        ));
        assert_eq!(
            forge.merges.iter().map(|merge| merge.0).collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(recovered[0].pull_requests.len(), 1);
        assert_eq!(recovered[0].outcomes[&2], Outcome::PendingUnknown);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        Ok(())
    }

    #[test]
    fn transport_retry_preserves_proven_landed_prefix() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let state = tempfile::tempdir()?;
        let base = repo.target.clone();
        let steps: Vec<_> = (1..=2)
            .map(|n| CandidateStep {
                pr: EligiblePr {
                    number: n,
                    head: forge.pull_requests[&n].head.clone(),
                    approval: format!("approval-{n}"),
                    carried_paths: Vec::new(),
                    accepted: None,
                    base_branch: "main".into(),
                },
                commit: format!("step-{n}"),
                tree: format!("tree-{n}"),
            })
            .collect();
        let candidate = Candidate {
            base: base.clone(),
            commit: "step-2".into(),
            tree: "tree-2".into(),
            ref_name: "land/test".into(),
            steps,
        };
        repo.refs
            .insert(candidate.ref_name.clone(), candidate.commit.clone());
        let target = std::rc::Rc::new(std::cell::RefCell::new(base));
        forge.shared_target = Some(target.clone());
        forge.fail_merge = Some(2);
        repo.shared_target = Some(target);
        repo.trees.insert("landed-2".into(), "tree-2".into());
        receipt::record_pending(
            state.path(),
            &PendingAttempt {
                repository: "fake-repository".into(),
                target: "main".into(),
                candidate,
                policy,
                gate_definition: definition,
                gate_evidence: super::super::receipt::tests::evidence("step-2"),
                publish: PublishDecision {
                    mode: PublishMode::RestOrdered,
                    reason: "fixture".into(),
                },
                cost: CostEstimate::Unknown {
                    reason: "fixture".into(),
                },
                settled: None,
                dispatched: None,
                started_at: None,
            },
        )?;
        let mut recovered = Vec::new();
        assert!(matches!(
            reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut recovered,
            ),
            Err(LandingError::PublicationUnknownAt { .. })
        ));
        assert_eq!(
            forge.merges.iter().map(|merge| merge.0).collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(recovered[0].pull_requests.len(), 1);
        assert!(repo.local_followed.is_empty());
        assert_eq!(recovered[0].outcomes[&2], Outcome::PendingUnknown);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        assert!(
            std::fs::read_dir(state.path().join("landing/receipts"))?
                .next()
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn earlier_recovery_receipt_survives_later_unknown_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::super::ports::PullRequestState;
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let state = tempfile::tempdir()?;
        repo.target = "tip".into();
        repo.parents.insert("tip".into(), vec!["a".into()]);
        repo.trees.insert("a".into(), "tree".into());
        forge.pull_requests.get_mut(&2).unwrap().state = PullRequestState::Merged;
        forge.pull_requests.get_mut(&2).unwrap().merged_commit = None;
        for (number, commit) in [(1, "a"), (2, "b")] {
            let pr = &forge.pull_requests[&number];
            let candidate = Candidate {
                base: "base".into(),
                commit: commit.into(),
                tree: "tree".into(),
                ref_name: format!("land/{commit}"),
                steps: vec![CandidateStep {
                    pr: EligiblePr {
                        number,
                        head: pr.head.clone(),
                        approval: format!("approval-{number}"),
                        carried_paths: Vec::new(),
                        accepted: None,
                        base_branch: "main".into(),
                    },
                    commit: commit.into(),
                    tree: "tree".into(),
                }],
            };
            repo.refs.insert(candidate.ref_name.clone(), commit.into());
            receipt::record_pending(
                state.path(),
                &PendingAttempt {
                    repository: "fake-repository".into(),
                    target: "main".into(),
                    candidate,
                    policy: policy.clone(),
                    gate_definition: definition.clone(),
                    gate_evidence: super::super::receipt::tests::evidence(commit),
                    publish: PublishDecision {
                        mode: PublishMode::RestOrdered,
                        reason: "fixture".into(),
                    },
                    cost: CostEstimate::Unknown {
                        reason: "fixture".into(),
                    },
                    settled: None,
                    dispatched: None,
                    started_at: None,
                },
            )?;
        }
        let result = run(
            &mut forge,
            &mut repo,
            &mut gate,
            &mut FakeAlarms::default(),
            Request {
                approvals: Approvals::forge(),
                numbers: &[1, 2],
                target: "main",
                settings: &settings,
                policy: &policy,
                gate: &definition,
                shadow: false,
                state: state.path(),
            },
        );
        let Err(LandingError::RunFailed { cause, report }) = result else {
            panic!("expected pending second attempt")
        };
        assert!(matches!(*cause, LandingError::PublicationUnknownAt { .. }));
        assert!(!repo.local_followed.contains(&2));
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::PendingUnknown);
        assert_eq!(report.receipts.len(), 1);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        Ok(())
    }

    #[test]
    fn ordered_merge_excludes_pr_changed_before_merge() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge
            .head_reads
            .insert(1, [format!("{:040x}", 1), "changed".into()].into());
        let state = tempfile::tempdir()?;
        let result = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        );
        let report = result?;
        assert_eq!(report.outcomes[&1], Outcome::Excluded);
        assert!(forge.merges.is_empty());
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }
    #[test]
    fn driver_bundled_defaults_gate_and_use_rest_on_protected_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, _, mut definition) = fixture(1);
        let settings = LandSettings::default();
        forge.direct_push = false;
        let head = format!("{:040x}", 1);
        let mut head_evidence = super::super::receipt::tests::evidence(&head);
        head_evidence.checks.push(CheckEvidence {
            name: "ci-status".into(),
            issuer: None,
            result: GateResult::Passed,
        });
        forge.check_runs.insert(head, vec![head_evidence]);
        definition.checks = settings.gate.checks.clone();
        repo.fetches = [
            "a".repeat(40),
            "a".repeat(40),
            "a".repeat(40),
            "landed-1".into(),
        ]
        .into();
        repo.trees.insert("landed-1".into(), "different".into());
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert_eq!(
            settings.integration,
            provenant_cli::domains::Integration::Batch
        );
        assert_eq!(report.receipts[0].gate_definition.checks, vec!["ci-status"]);
        assert_eq!(report.receipts[0].publish.mode, PublishMode::RestOrdered);
        assert_eq!(gate.observed.len(), 1);
        Ok(())
    }

    #[test]
    fn recovery_accepts_forge_merge_absent_from_moved_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge.lose_merge_response = true;
        repo.trees.insert("landed-1".into(), "different".into());
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        assert_eq!(forge.merge_attempts, vec![1]);
        Ok(())
    }
    #[test]
    fn ambiguous_ordered_merge_records_proven_prefix_before_returning_pending()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        forge.lose_merge_response_at = Some(2);
        forge.hide_merge_commit_at = Some(2);
        let base = "a".repeat(40);
        let first = format!("{:040x}", 2001);
        repo.parents.insert(first.clone(), vec![base.clone()]);
        repo.fetches = [base.clone(), base.clone(), base, first.clone(), first].into();
        let state = tempfile::tempdir()?;
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let result = run(
            &mut forge,
            &mut repo,
            &mut gate,
            &mut FakeAlarms::default(),
            Request {
                approvals: Approvals::forge(),
                numbers: &[1, 2],
                target: "main",
                settings: &settings,
                policy: &policy,
                gate: &definition,
                shadow: false,
                state: state.path(),
            },
        );
        let Err(LandingError::RunFailed { cause, report }) = result else {
            panic!("expected unresolved second merge")
        };
        assert!(matches!(*cause, LandingError::PublicationUnknownAt { .. }));
        assert!(!repo.local_followed.contains(&2));
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::PendingUnknown);
        assert_eq!(
            report.receipts[0]
                .candidate_steps
                .iter()
                .map(|step| step.pr.number)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        Ok(())
    }

    #[test]
    fn ordered_refusal_preserves_landed_prefix_without_pending()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        forge.refuse_merge = Some(2);
        let landed = format!("{:040x}", 2001);
        repo.parents.insert(landed.clone(), vec!["a".repeat(40)]);
        repo.fetches = ["a".repeat(40), "a".repeat(40), "a".repeat(40), landed].into();
        let state = tempfile::tempdir()?;
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let result = run(
            &mut forge,
            &mut repo,
            &mut gate,
            &mut FakeAlarms::default(),
            Request {
                approvals: Approvals::forge(),
                numbers: &[1, 2],
                target: "main",
                settings: &settings,
                policy: &policy,
                gate: &definition,
                shadow: false,
                state: state.path(),
            },
        );
        let Err(LandingError::RunFailed { cause, report }) = result else {
            panic!("expected refusal with partial report")
        };
        assert!(matches!(*cause, LandingError::Refused(_)));
        let response = super::super::batch_response(*report, Some(*cause));
        assert_eq!(response.status, provenant_records::Outcome::Refused);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("LAND_REFUSED")
        );
        assert_eq!(response.data["outcomes"]["1"], "landed");
        assert_eq!(response.data["outcomes"]["2"], "excluded");
        assert!(
            response.data["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["outcomes"]["1"] == "landed")
        );
        assert_eq!(forge.merges.len(), 1);
        let recovered = response.data["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["outcomes"]["1"] == "landed")
            .unwrap();
        assert_eq!(recovered["candidate_steps"].as_array().unwrap().len(), 1);
        assert_eq!(recovered["candidate_steps"][0]["pr"]["number"], 1);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            0
        );
        let summary = std::fs::read_dir(state.path().join("landing/receipts"))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("run-"))
            })
            .ok_or("missing run summary")?;
        let summary: serde_json::Value = serde_json::from_slice(&std::fs::read(summary)?)?;
        assert_eq!(summary["outcomes"]["1"], "landed");
        assert_eq!(summary["outcomes"]["2"], "excluded");
        assert!(
            summary["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["outcomes"]["1"] == "landed")
        );
        Ok(())
    }
    #[test]
    fn alarm_failure_keeps_settled_marker_for_recovery() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        repo.trees
            .insert(format!("{:040x}", 2001), "different".into());
        let state = tempfile::tempdir()?;
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        let mut alarms = FakeAlarms(Vec::new(), true);
        assert!(
            run(
                &mut forge,
                &mut repo,
                &mut gate,
                &mut alarms,
                Request {
                    approvals: Approvals::forge(),
                    numbers: &[1],
                    target: "main",
                    settings: &settings,
                    policy: &policy,
                    gate: &definition,
                    shadow: false,
                    state: state.path(),
                }
            )
            .is_err()
        );
        assert!(
            receipt::pending(state.path(), "fake-repository", "main")?[0]
                .settled
                .is_some()
        );
        let merges = forge.merge_attempts.clone();
        let pushes = repo.push_attempts.clone();
        alarms.1 = false;
        let mut recovered = Vec::new();
        reconcile::recover_pending(
            &mut forge,
            &mut repo,
            Approvals::forge(),
            &mut gate,
            &mut alarms,
            state.path(),
            "fake-repository",
            "main",
            false,
            &mut recovered,
        )?;
        assert_eq!(forge.merge_attempts, merges);
        assert_eq!(repo.push_attempts, pushes);
        assert_eq!(
            alarms
                .0
                .iter()
                .filter(|a| a.code == "LAND_TREE_MISMATCH")
                .count(),
            1
        );
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn target_read_failure_after_merge_does_not_block_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        let fail = std::rc::Rc::new(std::cell::Cell::new(false));
        forge.fail_fetch_after_merge = Some(fail.clone());
        repo.fail_fetch = Some(fail);
        repo.trees
            .insert(format!("{:040x}", 2001), "different".into());
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert!(
            report.receipts[0]
                .alarms
                .iter()
                .any(|a| a.code == "LAND_TREE_MISMATCH")
        );
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn failed_settlement_rewrite_keeps_publishing_marker() -> Result<(), Box<dyn std::error::Error>>
    {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge.refuse_merge = Some(1);
        forge.fail_settle_write_on_refusal = true;
        let state = tempfile::tempdir()?;
        assert!(
            matches!(exercise(&mut forge, &mut repo, &mut gate, &settings, &definition,
            &[1], false, state.path()), Err(LandingError::Failed(ref m)) if m == "injected marker rewrite failure")
        );
        let attempts = receipt::pending(state.path(), "fake-repository", "main")?;
        assert_eq!(attempts.len(), 1);
        assert!(attempts[0].settled.is_none());
        assert!(matches!(
            reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut Vec::new()
            ),
            Err(LandingError::Refused(_))
        ));
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        assert_eq!(forge.merge_attempts, vec![1, 1]);
        Ok(())
    }

    #[test]
    fn refused_prefix_receipt_failure_recovers_without_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        forge.refuse_merge = Some(2);
        let landed = format!("{:040x}", 2001);
        repo.fetches = ["a".repeat(40), "a".repeat(40), "a".repeat(40), landed].into();
        let state = tempfile::tempdir()?;
        std::fs::create_dir_all(state.path().join("landing"))?;
        std::fs::write(state.path().join("landing/receipts"), b"blocked")?;
        assert!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1, 2],
                false,
                state.path()
            )
            .is_err()
        );
        let attempts = receipt::pending(state.path(), "fake-repository", "main")?;
        assert_eq!(attempts.len(), 1);
        assert!(attempts[0].settled.is_some());
        let merges = forge.merge_attempts.clone();
        let pushes = repo.push_attempts.clone();
        std::fs::remove_file(state.path().join("landing/receipts"))?;
        let mut recovered = Vec::new();
        assert!(matches!(
            reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut recovered,
            ),
            Err(LandingError::Refused(_))
        ));
        assert_eq!(recovered[0].outcomes[&1], Outcome::Landed);
        assert_eq!(forge.merge_attempts, merges);
        assert_eq!(repo.push_attempts, pushes);
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn refused_prefix_follow_failure_recovers_without_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        forge.refuse_merge = Some(2);
        let landed = format!("{:040x}", 2001);
        repo.fetches = ["a".repeat(40), "a".repeat(40), "a".repeat(40), landed].into();
        let state = tempfile::tempdir()?;
        let fail = std::rc::Rc::new(std::cell::Cell::new(false));
        forge.fail_fetch_on_refusal = Some(fail.clone());
        forge.fail_approvals = Some(fail.clone());
        assert!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1, 2],
                false,
                state.path()
            )
            .is_err()
        );
        let attempts = receipt::pending(state.path(), "fake-repository", "main")?;
        assert_eq!(attempts.len(), 1);
        assert!(attempts[0].settled.is_some());
        let merges = forge.merge_attempts.clone();
        let pushes = repo.push_attempts.clone();
        fail.set(false);
        let mut recovered = Vec::new();
        assert!(matches!(
            reconcile::recover_pending(
                &mut forge,
                &mut repo,
                Approvals::forge(),
                &mut gate,
                &mut FakeAlarms::default(),
                state.path(),
                "fake-repository",
                "main",
                false,
                &mut recovered,
            ),
            Err(LandingError::Refused(_))
        ));
        assert_eq!(recovered[0].outcomes[&1], Outcome::Landed);
        assert_eq!(forge.merge_attempts, merges);
        assert_eq!(repo.push_attempts, pushes);
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn failed_gate_never_runs_local_follow_up() -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        gate.check_result = Some(GateResult::Failed);
        let state = tempfile::tempdir()?;
        let _ = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        );
        assert!(repo.local_followed.is_empty());
        assert!(repo.push_attempts.iter().all(|name| name != "main"));
        Ok(())
    }

    #[test]
    fn confirmed_landing_retries_local_follow_up_before_clearing_marker()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        let state = tempfile::tempdir()?;
        repo.fail_local = true;
        assert!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            )
            .is_err()
        );
        assert_eq!(repo.local_followed, vec![1]);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        let pushes = repo.push_attempts.clone();
        repo.fail_local = false;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(repo.local_followed, vec![1, 1]);
        assert_eq!(repo.push_attempts, pushes);
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn refused_local_cleanup_does_not_stop_later_batches_or_recovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, mut settings, definition) = fixture(3);
        settings.batch_size = 1;
        repo.refuse_local = true;
        let state = tempfile::tempdir()?;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1, 2],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert_eq!(report.outcomes[&2], Outcome::Landed);
        assert_eq!(report.receipts.len(), 2);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            2
        );
        let later = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[3],
            false,
            state.path(),
        )?;
        assert_eq!(later.outcomes[&3], Outcome::Landed);
        assert_eq!(later.receipts.len(), 3);
        repo.refuse_local = false;
        exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[3],
            false,
            state.path(),
        )?;
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }

    #[test]
    fn retained_partial_refusal_does_not_block_later_landings()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(3);
        forge.direct_push = false;
        forge.valid_merge_ids = true;
        forge.refuse_merge = Some(2);
        repo.refuse_local = true;
        let landed = format!("{:040x}", 2001);
        repo.parents.insert(landed.clone(), vec!["a".repeat(40)]);
        repo.fetches = ["a".repeat(40), "a".repeat(40), "a".repeat(40), landed].into();
        let state = tempfile::tempdir()?;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1, 2],
                false,
                state.path()
            ),
            Err(LandingError::Refused(_))
        ));
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        forge.direct_push = true;
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[3],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&3], Outcome::Landed);
        Ok(())
    }

    #[test]
    fn local_io_failure_does_not_skip_other_prs_in_a_settled_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(3);
        let state = tempfile::tempdir()?;
        repo.fail_local_at = Some(1);
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1, 2],
                false,
                state.path()
            ),
            Err(LandingError::LocalFollowUp(_))
        ));
        assert_eq!(repo.local_followed, vec![1, 2]);
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[3],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&3], Outcome::Landed);
        assert_eq!(repo.local_followed, vec![1, 2, 1, 2, 3]);
        Ok(())
    }

    #[test]
    fn recovery_local_io_failure_does_not_block_a_later_landing()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(2);
        let state = tempfile::tempdir()?;
        repo.fail_local = true;
        assert!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            )
            .is_err()
        );
        let result = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[2],
            false,
            state.path(),
        );
        assert!(matches!(result, Err(LandingError::LocalFollowUp(_))));
        assert!(repo.local_followed.contains(&2));
        assert_eq!(
            repo.push_attempts
                .iter()
                .filter(|name| *name == "main")
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn recovery_retries_candidate_cleanup_before_clearing_pending()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        repo.lose_push_response = true;
        repo.refuse_delete = true;
        let state = tempfile::tempdir()?;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ), Err(LandingError::Failed(ref message)) if message == "delete refused"
        ));
        repo.lose_push_response = false;
        repo.refuse_delete = true;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ), Err(LandingError::Failed(ref message)) if message == "delete refused"
        ));
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        repo.refuse_delete = false;
        forge.approvals_at_head.clear();
        let report = exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        assert!(repo.refs.keys().all(|name| !name.starts_with("land/")));
        Ok(())
    }
    #[test]
    fn shadow_recovery_does_not_update_refs_for_an_advanced_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        repo.lose_push_response = true;
        repo.refuse_delete = true;
        let state = tempfile::tempdir()?;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                false,
                state.path()
            ), Err(LandingError::Failed(ref message)) if message == "delete refused"
        ));
        let published = repo.target.clone();
        let refs = repo.refs.clone();
        let advanced = "b".repeat(40);
        repo.parents.insert(advanced.clone(), vec![published]);
        repo.target = advanced;
        repo.lose_push_response = false;
        repo.refuse_delete = false;
        assert!(matches!(
            exercise(
                &mut forge,
                &mut repo,
                &mut gate,
                &settings,
                &definition,
                &[1],
                true,
                state.path()
            ),
            Err(LandingError::RecoveryRequired { .. })
        ));
        assert_eq!(repo.refs, refs);
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        Ok(())
    }
}
