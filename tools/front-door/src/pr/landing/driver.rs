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
#[derive(Default)]
pub struct Report {
    pub receipts: Vec<LandingReceipt>,
    pub outcomes: BTreeMap<u64, Outcome>,
    pub warnings: Vec<String>,
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
    reconcile::recover_pending(forge, repo, request.state, request.target, request.shadow)?;
    let limit = usize::try_from(request.settings.batch_size)
        .map_err(|_| LandingError::Failed("batch size exceeds platform limit".into()))?;
    if limit == 0 {
        return Err(LandingError::Failed("batch size must be positive".into()));
    }
    let mut queue = VecDeque::new();
    let mut report = Report::default();
    for numbers in request.numbers.chunks(limit) {
        let selected = candidate::select_batch(
            forge,
            numbers,
            request.target,
            limit,
            request.settings.batch_wait_seconds,
        )?;
        for number in numbers {
            if !selected.iter().any(|pr| pr.number == *number) {
                report.outcomes.insert(*number, Outcome::Excluded);
            }
        }
        queue.push_back(selected);
    }
    while let Some(prs) = queue.pop_front() {
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
            let base = repo.fetch_target(request.target)?;
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
            let evidence = runner.gate(&candidate, request.gate, request.settings.poll_seconds)?;
            let result = gate::validate_evidence(&candidate, request.gate, &evidence)?;
            if result == GateResult::Failed {
                if prs.len() == 1 {
                    report.outcomes.insert(prs[0].number, Outcome::Red);
                } else {
                    for part in split::red_prefixes(&prs)?.into_iter().rev() {
                        queue.push_front(part);
                    }
                }
                break;
            }
            if result != GateResult::Passed {
                return Err(LandingError::Failed("required gate did not pass".into()));
            }
            let decision = publish::decide(
                forge,
                request.target,
                request.settings.publish,
                request.settings.method,
            )?;
            let durations = match (&evidence.pool, evidence.duration_seconds) {
                (Some(pool), Some(duration)) => vec![(pool.clone(), duration)],
                _ => Vec::new(),
            };
            let cost = if durations.is_empty() {
                CostEstimate::Unknown {
                    reason: "gate pool or duration unavailable".into(),
                }
            } else {
                budget::estimate(&request.settings.budget, &durations)?
            };
            let warning = budget::check(&request.settings.budget, &cost)?;
            if let Some(warning) = warning {
                report.warnings.push(warning);
            }
            let excluded = publish::revalidate(forge, &candidate)?;
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
                break;
            }
            if repo.fetch_target(request.target)? != base {
                if !split::rebuild_allowed(rebuilds, request.settings.max_rebuilds)? {
                    return Err(LandingError::Contention);
                }
                rebuilds += 1;
                continue;
            }
            if !request.shadow {
                let attempt = PendingAttempt {
                    target: request.target.into(),
                    candidate: candidate.clone(),
                    policy: request.policy.clone(),
                    gate_definition: request.gate.clone(),
                    gate_evidence: evidence.clone(),
                    publish: decision.clone(),
                    cost: cost.clone(),
                };
                receipt::record_pending(request.state, &attempt)?;
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
                    Err(_) => {
                        let _ = reconcile::reconcile(forge, repo, request.target, &candidate)?;
                        return Err(LandingError::PublicationUnknown);
                    }
                }
            };
            let mut mapping = BTreeMap::new();
            let mut outcomes = BTreeMap::new();
            if !request.shadow {
                if publication.landed.len() != prs.len() {
                    return Err(LandingError::PublicationUnknown);
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
                receipt::clear_pending(request.state, &candidate)?;
                repo.delete_ref(&candidate.ref_name, &candidate.commit)?;
            }
            report.receipts.push(record);
            break;
        }
    }
    Ok(report)
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
}
