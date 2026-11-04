//! Landing orchestration. Lane-owned decisions and I/O remain behind their ports.
use super::{
    LandingError, budget, candidate, gate,
    model::*,
    policy::LandPolicy,
    ports::{AlarmSink, Forge, GateRunner, Repo},
    postmerge, publish, receipt, reconcile, split,
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
}
#[derive(Debug, Default)]
pub struct Report {
    pub receipts: Vec<LandingReceipt>,
    pub outcomes: BTreeMap<u64, Outcome>,
    pub warnings: Vec<String>,
}

fn fetch_target(repo: &mut dyn Repo, target: &str, shadow: bool) -> Result<String, LandingError> {
    if shadow {
        repo.fetch_target_untracked(target)
    } else {
        repo.fetch_target(target)
    }
}

fn cleanup_unpublished<T>(
    result: Result<T, LandingError>,
    repo: &mut dyn Repo,
    candidate: &Candidate,
    shadow: bool,
) -> Result<T, LandingError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if !shadow {
                repo.delete_ref(&candidate.ref_name, &candidate.commit)
                    .map_err(|cleanup| {
                        LandingError::Failed(format!(
                            "{error}; candidate cleanup failed: {cleanup}"
                        ))
                    })?;
            }
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

fn unmet_checks(
    candidate: &Candidate,
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> String {
    definition
        .checks
        .iter()
        .filter_map(|name| {
            if evidence.commit != candidate.commit || evidence.tree != candidate.tree {
                return Some(format!("{name}: evidence is for another commit or tree"));
            }
            let named: Vec<_> = evidence
                .checks
                .iter()
                .filter(|check| check.name == *name)
                .collect();
            if named.is_empty() {
                return Some(format!("{name}: missing"));
            }
            let matching: Vec<_> = named
                .into_iter()
                .filter(|check| {
                    definition
                        .issuer
                        .as_ref()
                        .is_none_or(|issuer| check.issuer.as_ref() == Some(issuer))
                })
                .collect();
            if matching.is_empty() {
                return Some(format!("{name}: wrong issuer"));
            }
            let states: Vec<_> = matching
                .into_iter()
                .filter(|check| check.result != GateResult::Passed)
                .map(|check| format!("{:?}", check.result).to_lowercase())
                .collect();
            if states.is_empty() {
                None
            } else {
                Some(format!("{name}: {}", states.join(", ")))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
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
    if request.settings.post_merge.revert_forward && !forge.supports_revert_forward() {
        report.warnings.push(
            "revert-forward is configured but this forge adapter cannot open a revert pull request"
                .into(),
        );
    }
    let repository = repo.repository_identity(request.target)?;
    let recovered = reconcile::recover_pending(
        forge,
        repo,
        runner,
        alarms,
        request.state,
        &repository,
        request.target,
        request.shadow,
    )?;
    add_recovered(report, recovered);
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
        .filter(|number| seen.insert(*number))
        .collect();
    for numbers in unique.chunks(limit) {
        let selected = candidate::select_batch(
            forge,
            numbers,
            request.target,
            limit,
            request.settings.batch_wait_seconds,
        )?;
        for number in numbers {
            if !selected.iter().any(|pr| pr.number == *number) {
                report.outcomes.entry(*number).or_insert(Outcome::Excluded);
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
            let evidence =
                match runner.gate(&candidate, request.gate, request.settings.poll_seconds) {
                    Ok(evidence) => evidence,
                    Err(error) => {
                        if !request.shadow {
                            repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                        }
                        return Err(error);
                    }
                };
            let result = match gate::validate_evidence(&candidate, request.gate, &evidence) {
                Ok(result) => result,
                Err(error) => {
                    if !request.shadow {
                        repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                    }
                    return Err(LandingError::Failed(format!(
                        "{error}; {}",
                        unmet_checks(&candidate, request.gate, &evidence)
                    )));
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
                    request.shadow,
                )?
            };
            let warning = cleanup_unpublished(
                budget::check(&request.settings.budget, &cost),
                repo,
                &candidate,
                request.shadow,
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
                    let names = unmet_checks(&candidate, request.gate, &evidence);
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
                let record = LandingReceipt {
                    policy: request.policy.clone(),
                    base: candidate.base.clone(),
                    candidate: candidate.commit.clone(),
                    tree: candidate.tree.clone(),
                    candidate_ref: candidate.ref_name.clone(),
                    candidate_steps: candidate.steps.clone(),
                    pull_requests: prs.clone(),
                    gate_definition: request.gate.clone(),
                    gate_evidence: evidence,
                    publish: cleanup_unpublished(
                        publish::decide(
                            forge,
                            request.target,
                            request.settings.publish,
                            request.settings.method,
                        ),
                        repo,
                        &candidate,
                        request.shadow,
                    )?,
                    outcomes,
                    split: parts
                        .iter()
                        .map(|part| part.iter().map(|pr| pr.number).collect())
                        .collect(),
                    approved_to_landed: BTreeMap::new(),
                    cost: cost.clone(),
                    alarms: Vec::new(),
                    shadow: request.shadow,
                };
                cleanup_unpublished(
                    receipt::write_receipt(request.state, &record),
                    repo,
                    &candidate,
                    request.shadow,
                )?;
                report.receipts.push(record);
                for part in parts.into_iter().rev() {
                    queue.push_front(part);
                }
                if !request.shadow {
                    repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                }
                break;
            }
            if result != GateResult::Passed {
                let checks = evidence
                    .checks
                    .iter()
                    .filter(|check| check.result != GateResult::Passed)
                    .map(|check| check.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if !request.shadow {
                    repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                }
                return Err(LandingError::Failed(format!(
                    "required gate did not pass: {checks}"
                )));
            }
            let decision = cleanup_unpublished(
                publish::decide(
                    forge,
                    request.target,
                    request.settings.publish,
                    request.settings.method,
                ),
                repo,
                &candidate,
                request.shadow,
            )?;
            let excluded = cleanup_unpublished(
                publish::revalidate(forge, &candidate),
                repo,
                &candidate,
                request.shadow,
            )?;
            if !excluded.is_empty() {
                for pr in &excluded {
                    report.outcomes.insert(pr.number, Outcome::Excluded);
                }
                let retained: Vec<_> = prs
                    .iter()
                    .filter(|pr| !excluded.iter().any(|excluded| excluded.number == pr.number))
                    .cloned()
                    .collect();
                if !retained.is_empty() {
                    queue.push_front(retained);
                }
                if !request.shadow {
                    repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                }
                break;
            }
            let observed_target = fetch_target(repo, request.target, request.shadow);
            if cleanup_unpublished(observed_target, repo, &candidate, request.shadow)? != base {
                if !request.shadow {
                    repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                }
                if !split::rebuild_allowed(rebuilds, request.settings.max_rebuilds)? {
                    return Err(LandingError::Contention);
                }
                rebuilds += 1;
                continue;
            }
            if !request.shadow {
                let attempt = PendingAttempt {
                    repository: repository.clone(),
                    target: request.target.into(),
                    candidate: candidate.clone(),
                    policy: request.policy.clone(),
                    gate_definition: request.gate.clone(),
                    gate_evidence: evidence.clone(),
                    publish: decision.clone(),
                    cost: cost.clone(),
                };
                cleanup_unpublished(
                    receipt::record_pending(request.state, &attempt),
                    repo,
                    &candidate,
                    request.shadow,
                )?;
            }
            let publication = if request.shadow {
                PublishResult::default()
            } else {
                let published = match decision.mode {
                    PublishMode::FastForward => {
                        publish::fast_forward(repo, request.target, &candidate)
                    }
                    PublishMode::RestOrdered => publish::rest_ordered(
                        forge,
                        repo,
                        request.target,
                        &candidate,
                        request.settings.method,
                    ),
                };
                match published {
                    Ok(landed) => landed,
                    Err(LandingError::TargetMoved) => {
                        let recovered = reconcile::recover_pending(
                            forge,
                            repo,
                            runner,
                            alarms,
                            request.state,
                            &repository,
                            request.target,
                            false,
                        )?;
                        add_recovered(report, recovered);
                        let mut remaining = Vec::new();
                        for pr in prs {
                            if forge.pull_request(pr.number)?.state
                                == super::ports::PullRequestState::Merged
                            {
                                report.outcomes.insert(pr.number, Outcome::Landed);
                            } else {
                                remaining.push(pr);
                            }
                        }
                        prs = remaining;
                        if prs.is_empty() {
                            break;
                        }
                        if !split::rebuild_allowed(rebuilds, request.settings.max_rebuilds)? {
                            return Err(LandingError::Contention);
                        }
                        rebuilds += 1;
                        continue;
                    }
                    Err(LandingError::PullRequestChanged(number)) => {
                        let recovered = reconcile::recover_pending(
                            forge,
                            repo,
                            runner,
                            alarms,
                            request.state,
                            &repository,
                            request.target,
                            false,
                        )?;
                        add_recovered(report, recovered);
                        let moved = repo.fetch_target(request.target)? != base;
                        let mut remaining = Vec::new();
                        for pr in prs {
                            if pr.number == number {
                                report.outcomes.insert(pr.number, Outcome::Excluded);
                            } else if forge.pull_request(pr.number)?.state
                                == super::ports::PullRequestState::Merged
                            {
                                report.outcomes.insert(pr.number, Outcome::Landed);
                            } else {
                                remaining.push(pr);
                            }
                        }
                        prs = remaining;
                        if prs.is_empty() {
                            break;
                        }
                        if moved {
                            if !split::rebuild_allowed(rebuilds, request.settings.max_rebuilds)? {
                                return Err(LandingError::Contention);
                            }
                            rebuilds += 1;
                        }
                        continue;
                    }
                    Err(LandingError::Refused(reason)) => {
                        let recovered = reconcile::recover_pending(
                            forge,
                            repo,
                            runner,
                            alarms,
                            request.state,
                            &repository,
                            request.target,
                            false,
                        )?;
                        add_recovered(report, recovered);
                        if repo.fetch_target(request.target)? != base {
                            let mut remaining = Vec::new();
                            for pr in prs {
                                if forge.pull_request(pr.number)?.state
                                    == super::ports::PullRequestState::Merged
                                {
                                    report.outcomes.insert(pr.number, Outcome::Landed);
                                } else {
                                    remaining.push(pr);
                                }
                            }
                            prs = remaining;
                            if prs.is_empty() {
                                break;
                            }
                            if !split::rebuild_allowed(rebuilds, request.settings.max_rebuilds)? {
                                return Err(LandingError::Contention);
                            }
                            rebuilds += 1;
                            continue;
                        }
                        return Err(LandingError::Refused(reason));
                    }
                    Err(_) => {
                        return Err(LandingError::PublicationUnknownAt {
                            marker: receipt::pending_path(request.state, &repository, &candidate)?,
                        });
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
            let mut record = LandingReceipt {
                policy: request.policy.clone(),
                base: candidate.base.clone(),
                candidate: candidate.commit.clone(),
                tree: candidate.tree.clone(),
                candidate_ref: candidate.ref_name.clone(),
                candidate_steps: candidate.steps.clone(),
                pull_requests: prs.clone(),
                gate_definition: request.gate.clone(),
                gate_evidence: evidence,
                publish: decision,
                outcomes,
                split: Vec::new(),
                approved_to_landed: mapping,
                cost,
                alarms: publication.alarms,
                shadow: request.shadow,
            };
            receipt::write_receipt(request.state, &record)?;
            if !request.shadow {
                for alarm in &record.alarms {
                    alarms.post(alarm)?;
                }
                let followed = postmerge::follow(forge, repo, runner, alarms, &mut record);
                receipt::write_receipt(request.state, &record)?;
                followed?;
                repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
                receipt::clear_pending(request.state, &repository, &candidate)?;
            }
            report.receipts.push(record);
            break;
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
    use provenant_cli::domains::LandOverride;
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
            pr_ci: provenant_cli::domains::PrCiSettings {
                mode: provenant_cli::domains::PrCiMode::None,
                ..Default::default()
            },
            ..LandSettings::default()
        };
        let definition = super::super::receipt::tests::definition();
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
            },
        )
        .map_err(|error| match error {
            LandingError::RunFailed { cause, .. } => *cause,
            error => error,
        })
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
        assert_eq!(repo.refs.len(), 1);
        assert_eq!(repo.refs.get(&repo.pushes[0].0), Some(&repo.pushes[0].1));
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
        assert!(report.warnings.iter().any(|warning| warning.contains("ci")));
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
                assert!(
                    result?
                        .warnings
                        .iter()
                        .any(|message| message.contains("ci"))
                );
            } else {
                let message = match result {
                    Err(error) => error.to_string(),
                    Ok(_) => panic!("{kind} passed"),
                };
                assert!(message.contains("ci"), "{message}");
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
            panic!("expected unknown outcome with report")
        };
        assert!(matches!(*cause, LandingError::PublicationUnknownAt { .. }));
        let response = super::super::batch_response(*report, Some(*cause));
        assert_eq!(response.status, provenant_records::Outcome::Pending);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("LAND_PUBLICATION_UNKNOWN")
        );
        assert_eq!(response.data["outcomes"]["1"], "pending_unknown");
        assert!(response.error.unwrap().message.contains("pending marker"));
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        forge.approvals_at_head.clear();
        repo.lose_push_response = false;
        let mut alarms = FakeAlarms::default();
        let report = run(
            &mut forge,
            &mut repo,
            &mut gate,
            &mut alarms,
            Request {
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
        assert!(alarms.0.iter().any(|alarm| alarm.code == "approval_lost"));
        assert_eq!(report.outcomes[&1], Outcome::Landed);
        assert!(repo.refs.keys().all(|name| !name.starts_with("land/")));
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
        assert!(forge.check_runs.is_empty());
        assert_eq!(gate.observed.len(), 1);
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
    fn definite_refusal_clears_marker_and_allows_next_run() -> Result<(), Box<dyn std::error::Error>>
    {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        repo.refuse_target_push = true;
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
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        repo.refuse_target_push = false;
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
        Ok(())
    }
    #[test]
    fn ordered_merge_excludes_head_changed_after_initial_revalidation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge
            .head_reads
            .insert(1, [format!("{:040x}", 1), "changed".into()].into());
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
    fn recovery_retains_marker_until_forge_merge_is_in_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        forge.direct_push = false;
        forge.lose_merge_response = true;
        let state = tempfile::tempdir()?;
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
        let attempts = receipt::pending(state.path(), "fake-repository", "main")?;
        assert_eq!(attempts.len(), 1);
        let error = match exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        ) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("unproven merge recovered"),
        };
        assert!(error.contains("pending marker"));
        assert_eq!(
            receipt::pending(state.path(), "fake-repository", "main")?.len(),
            1
        );
        repo.parents.insert("a".repeat(40), vec!["landed-1".into()]);
        repo.trees
            .insert("landed-1".into(), attempts[0].candidate.tree.clone());
        exercise(
            &mut forge,
            &mut repo,
            &mut gate,
            &settings,
            &definition,
            &[1],
            false,
            state.path(),
        )?;
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
        Ok(())
    }
    #[test]
    fn ordered_refusal_preserves_landed_prefix_and_clears_pending()
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
        assert!(receipt::pending(state.path(), "fake-repository", "main")?.is_empty());
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
    fn recovery_retries_candidate_cleanup_before_clearing_pending()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut forge, mut repo, mut gate, settings, definition) = fixture(1);
        repo.lose_push_response = true;
        let state = tempfile::tempdir()?;
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
        repo.lose_push_response = false;
        repo.refuse_delete = true;
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
        let state = tempfile::tempdir()?;
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
        let published = repo.target.clone();
        let refs = repo.refs.clone();
        let advanced = "b".repeat(40);
        repo.parents.insert(advanced.clone(), vec![published]);
        repo.target = advanced;
        repo.lose_push_response = false;
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
