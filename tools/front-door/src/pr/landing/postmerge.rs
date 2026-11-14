use super::{
    LandingError,
    approval::{Approvals, Decision},
    model::*,
    ports::{AlarmSink, Forge, GateRunner, Repo},
};

fn alarm(code: &str, message: String) -> Alarm {
    Alarm {
        code: code.into(),
        message,
    }
}

fn raise(
    alarms: &mut dyn AlarmSink,
    receipt: &mut LandingReceipt,
    raised: Alarm,
) -> Result<(), LandingError> {
    // Record first: the receipt must show the alarm even when delivery fails.
    receipt.alarms.push(raised.clone());
    alarms.post(&raised)
}

/// The commit on the target that the batch produced: the last landed pull request, or the
/// candidate itself when the publication fast-forwarded.
fn tip(receipt: &LandingReceipt) -> String {
    receipt
        .pull_requests
        .last()
        .and_then(|pr| receipt.approved_to_landed.get(&pr.head))
        .cloned()
        .unwrap_or_else(|| receipt.candidate.clone())
}

#[derive(Debug, PartialEq, Eq)]
enum Observed {
    Passed,
    Failed(Vec<String>),
    Unsettled {
        failed: Vec<String>,
        pending: Vec<String>,
    },
}

fn observe(
    forge: &mut dyn Forge,
    commit: &str,
    names: &[String],
    timeout: std::time::Duration,
) -> Result<Observed, LandingError> {
    let runs = forge.checks_with_timeout(commit, timeout)?;
    let mut failed = Vec::new();
    let mut pending = Vec::new();
    for name in names {
        let results: Vec<&GateResult> = runs
            .iter()
            .flat_map(|run| run.checks.iter())
            .filter(|check| &check.name == name)
            .map(|check| &check.result)
            .collect();
        if results.iter().any(|r| {
            matches!(
                r,
                GateResult::Failed | GateResult::Skipped | GateResult::Neutral
            )
        }) {
            failed.push(name.clone());
        }
        if results.is_empty()
            || results
                .iter()
                .any(|r| matches!(r, GateResult::Pending | GateResult::Missing))
        {
            pending.push(name.clone());
        }
    }
    Ok(if !pending.is_empty() {
        Observed::Unsettled { failed, pending }
    } else if !failed.is_empty() {
        Observed::Failed(failed)
    } else {
        Observed::Passed
    })
}

/// Gate ordered prefixes of the candidate to find the first one that fails, assuming a pull
/// request that breaks a prefix breaks every longer one. `None` means the search was
/// inconclusive and the caller must not blame a pull request.
fn bisect(
    repo: &mut dyn Repo,
    gate: &mut dyn GateRunner,
    receipt: &LandingReceipt,
    definition: &GateDefinition,
    poll_seconds: u64,
    costs: &mut Vec<Option<(String, u64)>>,
) -> Option<u64> {
    let steps = &receipt.candidate_steps;
    let last = steps.last()?;
    let full = Candidate {
        base: receipt.base.clone(),
        steps: steps.clone(),
        commit: last.commit.clone(),
        tree: last.tree.clone(),
        ref_name: format!("{}-prefix-full", receipt.candidate_ref),
    };
    repo.push_ref(&full.ref_name, &full.commit, None).ok()?;
    let full_verdict = gate.gate(&full, definition, poll_seconds);
    let _ = repo.delete_ref(&full.ref_name, &full.commit);
    let full_verdict = full_verdict.ok();
    costs.push(full_verdict.as_ref().and_then(evidence_duration));
    if full_verdict?.result != GateResult::Failed {
        return None;
    }
    let (mut low, mut high) = (1, steps.len());
    while low < high {
        let mid = (low + high) / 2;
        let step = &steps[mid - 1];
        let prefix = Candidate {
            base: receipt.base.clone(),
            steps: steps[..mid].to_vec(),
            commit: step.commit.clone(),
            tree: step.tree.clone(),
            ref_name: format!("{}-prefix-{mid}", receipt.candidate_ref),
        };
        repo.push_ref(&prefix.ref_name, &prefix.commit, None).ok()?;
        let verdict = gate.gate(&prefix, definition, poll_seconds);
        // The prefix ref is scratch; a failed delete leaves a stray ref but no wrong verdict.
        let _ = repo.delete_ref(&prefix.ref_name, &prefix.commit);
        let verdict = verdict.ok();
        costs.push(verdict.as_ref().and_then(evidence_duration));
        match verdict?.result {
            GateResult::Passed => low = mid + 1,
            GateResult::Failed => high = mid,
            _ => return None,
        }
    }
    steps.get(low.checked_sub(1)?).map(|step| step.pr.number)
}

fn evidence_duration(evidence: &GateEvidence) -> Option<(String, u64)> {
    Some((evidence.pool.clone()?, evidence.duration_seconds?))
}

fn add_cost(receipt: &mut LandingReceipt, cost: CostEstimate) {
    receipt.cost = match (&receipt.cost, cost) {
        (CostEstimate::Known { micros: prior }, CostEstimate::Known { micros }) => {
            CostEstimate::Known {
                micros: prior.saturating_add(micros),
            }
        }
        (CostEstimate::Unknown { reason }, _) => CostEstimate::Unknown {
            reason: reason.clone(),
        },
        (_, CostEstimate::Unknown { reason }) => CostEstimate::Unknown { reason },
    };
}

/// Recheck approvals after publication; follow configured checks, bisect failures,
/// send alarms and respect revert-forward policy.
#[allow(clippy::disallowed_methods, reason = "post-merge polling boundary")]
pub fn follow(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    approvals: Approvals<'_>,
    gate: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    receipt: &mut LandingReceipt,
) -> Result<(), LandingError> {
    let started = std::time::Instant::now();
    follow_with(
        forge,
        repo,
        approvals,
        gate,
        alarms,
        receipt,
        &std::thread::sleep,
        &|| started.elapsed(),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "follow-up uses the landing boundaries"
)]
fn follow_with(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    approvals: Approvals<'_>,
    gate: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    receipt: &mut LandingReceipt,
    sleep: &dyn Fn(std::time::Duration),
    clock: &dyn Fn() -> std::time::Duration,
) -> Result<(), LandingError> {
    if receipt.shadow {
        return Ok(());
    }
    let settings = receipt
        .policy
        .settings()
        .map_err(|e| LandingError::Failed(format!("policy settings: {e}")))?;
    let post = settings.post_merge;

    // Approvals must still stand at the approved head after publication,
    // proved against the target the candidate was built on.
    for (index, pr) in receipt.pull_requests.clone().into_iter().enumerate() {
        let standing = match approvals
            .at(repo, &receipt.base)
            .decide(forge, pr.number, &pr.head, "")?
        {
            Decision::Approved(approved) => {
                receipt.pull_requests[index].extend_approval_evidence(approved.evidence.clone());
                if let Some(step) = receipt
                    .candidate_steps
                    .iter_mut()
                    .find(|step| step.pr.number == pr.number)
                {
                    step.pr.extend_approval_evidence(approved.evidence);
                }
                true
            }
            Decision::Refused(_) => false,
        };
        if !standing {
            let message = format!(
                "approval {} for pull request {} is no longer present at {}",
                pr.approval, pr.number, pr.head
            );
            raise(alarms, receipt, alarm("approval_lost", message))?;
        }
    }

    if post.checks.is_empty() {
        return Ok(());
    }
    let landed = tip(receipt);
    let mut elapsed = 0;
    let mut observed_failures = Vec::new();
    let mut last_pending = post.checks.clone();
    let failed = loop {
        let remaining = std::time::Duration::from_secs(post.timeout_seconds)
            .saturating_sub(clock().max(std::time::Duration::from_secs(elapsed)));
        let observation = if remaining.is_zero() {
            Observed::Unsettled {
                failed: Vec::new(),
                pending: last_pending.clone(),
            }
        } else {
            let result = observe(forge, &landed, &post.checks, remaining);
            if clock() >= std::time::Duration::from_secs(post.timeout_seconds) {
                match result? {
                    Observed::Failed(failed) => Observed::Failed(failed),
                    Observed::Unsettled { failed, pending } => {
                        Observed::Unsettled { failed, pending }
                    }
                    Observed::Passed => Observed::Passed,
                }
            } else {
                result?
            }
        };
        elapsed = elapsed.max(clock().as_secs());
        match observation {
            Observed::Passed => break None,
            Observed::Failed(names) => break Some(names),
            Observed::Unsettled { failed, pending } if elapsed >= post.timeout_seconds => {
                for name in failed {
                    if !observed_failures.contains(&name) {
                        observed_failures.push(name);
                    }
                }
                raise(
                    alarms,
                    receipt,
                    alarm(
                        "unsettled",
                        format!(
                            "post-merge checks {} did not settle on {landed}",
                            pending.join(", ")
                        ),
                    ),
                )?;
                break (!observed_failures.is_empty()).then_some(observed_failures);
            }
            Observed::Unsettled { failed, pending } => {
                last_pending = pending;
                for name in failed {
                    if !observed_failures.contains(&name) {
                        observed_failures.push(name);
                    }
                }
                let pause = settings
                    .poll_seconds
                    .max(1)
                    .min(post.timeout_seconds - elapsed);
                sleep(std::time::Duration::from_secs(pause));
                elapsed += pause;
            }
        }
    };
    let seconds = clock().as_secs().max(elapsed);
    let post_cost = match &post.pool {
        Some(pool) => super::budget::estimate(&settings.budget, &[(pool.clone(), seconds)])?,
        None => CostEstimate::Unknown {
            reason: "post-merge pool unavailable".into(),
        },
    };
    add_cost(receipt, post_cost);
    let Some(failed) = failed else {
        return Ok(());
    };
    let mut message = format!(
        "post-merge check {} failed on {} at {landed}",
        failed.join(", "),
        receipt.publish_target()
    );
    let mut culprit = None;
    let mut bisect_costs = Vec::new();
    if post.bisect {
        culprit = match receipt.candidate_steps.len() {
            0 => None,
            1 => Some(receipt.candidate_steps[0].pr.number),
            _ => {
                let mut definition = receipt.gate_definition.clone();
                definition.kind = provenant_cli::domains::GateKind::Checks;
                definition.checks = post.checks.clone();
                definition.commands.clear();
                definition.issuer = None;
                definition.pool = post.pool.clone();
                definition.timeout_seconds = post.timeout_seconds;
                bisect(
                    repo,
                    gate,
                    receipt,
                    &definition,
                    settings.poll_seconds,
                    &mut bisect_costs,
                )
            }
        };
        match culprit {
            Some(number) => message.push_str(&format!("; first failing pull request is #{number}")),
            None => message.push_str("; bisection could not isolate a pull request"),
        }
    }
    if !bisect_costs.is_empty() {
        let cost = if bisect_costs.iter().any(Option::is_none) {
            CostEstimate::Unknown {
                reason: "post-merge bisection pool or duration unavailable".into(),
            }
        } else {
            super::budget::estimate(
                &settings.budget,
                &bisect_costs.into_iter().flatten().collect::<Vec<_>>(),
            )?
        };
        add_cost(receipt, cost);
    }
    raise(alarms, receipt, alarm("post_merge_failed", message))?;

    if let (true, Some(number)) = (post.revert_forward, culprit) {
        let landed_commit = receipt
            .pull_requests
            .iter()
            .find(|pr| pr.number == number)
            .and_then(|pr| receipt.approved_to_landed.get(&pr.head))
            .cloned();
        let target = receipt.publish_target().to_owned();
        let branch = format!("{}revert/{number}", settings.candidate_ref_prefix);
        let result = (|| {
            let landed_commit = landed_commit.ok_or_else(|| {
                LandingError::Failed(format!("no landed commit recorded for #{number}"))
            })?;
            if let Some(existing) = forge.find_revert(number, &branch, &target)? {
                return Ok(existing);
            }
            let base = repo.fetch_target(&target)?;
            let commit = repo.revert_commit(&base, &landed_commit)?;
            if let Err(error) = repo.push_ref(&branch, &commit, None) {
                if let Some(existing) = forge.find_revert(number, &branch, &target)? {
                    return Ok(existing);
                }
                // A previous run may have created the branch before its POST.
                // Adopt it only when it has the same base and revert tree.
                let existing = repo.fetch_target(&branch)?;
                if !repo.is_ancestor(&base, &existing)?
                    || repo.tree_of(&existing)? != repo.tree_of(&commit)?
                {
                    return Err(error);
                }
            }
            forge.open_revert(number, &branch, &target, &failed.join(", "))
        })();
        let outcome = match result {
            Ok(revert) => alarm(
                "revert_forward_opened",
                format!("opened pull request #{revert} reverting #{number}"),
            ),
            Err(error) => alarm(
                "revert_forward_failed",
                format!(
                    "could not open a revert for #{number}: {error}; inspect branch {branch} and retry creating the revert pull request manually"
                ),
            ),
        };
        raise(alarms, receipt, outcome)?;
    }
    Ok(())
}

impl LandingReceipt {
    fn publish_target(&self) -> &str {
        self.pull_requests
            .first()
            .map_or("the target", |pr| pr.base_branch.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::landing::{
        fakes::{FakeAlarms, FakeForge, FakeGate, FakeRepo},
        ports::Approval,
    };
    use provenant_cli::domains::{LandOverride, LandSettings, PostMergeOverride};
    use std::collections::BTreeMap;

    struct Rig {
        forge: FakeForge,
        repo: FakeRepo,
        gate: FakeGate,
        alarms: FakeAlarms,
        receipt: LandingReceipt,
    }

    fn rig(bisect: bool, revert: bool, prs: u64) -> Rig {
        let invocation = LandOverride {
            post_merge: Some(PostMergeOverride {
                checks: Some(vec!["post".into()]),
                bisect: Some(bisect),
                revert_forward: Some(revert),
                ..PostMergeOverride::default()
            }),
            ..LandOverride::default()
        };
        let policy = crate::pr::landing::policy::LandPolicy::resolve(
            &LandSettings::default(),
            &BTreeMap::new(),
            None,
            None,
            &invocation,
        )
        .expect("policy");
        let mut forge = FakeForge::default();
        let mut steps = Vec::new();
        let mut map = BTreeMap::new();
        let mut pull_requests = Vec::new();
        for n in 1..=prs {
            let pr = EligiblePr {
                number: n,
                head: format!("h{n}"),
                approval: format!("a{n}"),
                approval_evidence: Vec::new(),
                accepted: None,
                base_branch: "main".into(),
            };
            forge.approvals_at_head.insert(
                (n, pr.head.clone()),
                vec![Approval {
                    reference: pr.approval.clone(),
                    reviewer: "r".into(),
                }],
            );
            map.insert(pr.head.clone(), format!("l{n}"));
            steps.push(CandidateStep {
                pr: pr.clone(),
                commit: format!("s{n}"),
                tree: format!("t{n}"),
            });
            pull_requests.push(pr);
        }
        let mut receipt = super::super::receipt::tests::receipt(&format!("s{prs}"), false);
        receipt.policy = policy;
        receipt.candidate_steps = steps;
        receipt.pull_requests = pull_requests;
        receipt.approved_to_landed = map;
        Rig {
            forge,
            repo: FakeRepo::default(),
            gate: FakeGate::default(),
            alarms: FakeAlarms::default(),
            receipt,
        }
    }

    fn post_result(rig: &mut Rig, commit: &str, result: GateResult) {
        rig.forge.check_runs.insert(
            commit.into(),
            vec![GateEvidence {
                checks: vec![CheckEvidence {
                    name: "post".into(),
                    issuer: None,
                    result,
                }],
                ..super::super::receipt::tests::evidence(commit)
            }],
        );
    }

    fn verdict(commit: &str, result: GateResult) -> GateEvidence {
        GateEvidence {
            result,
            ..super::super::receipt::tests::evidence(commit)
        }
    }

    fn run(rig: &mut Rig) -> Result<(), LandingError> {
        follow(
            &mut rig.forge,
            &mut rig.repo,
            Approvals::forge(),
            &mut rig.gate,
            &mut rig.alarms,
            &mut rig.receipt,
        )
    }

    #[test]
    fn pending_run_keeps_a_failed_check_unsettled() -> Result<(), LandingError> {
        let mut r = rig(false, false, 1);
        let checks = vec![
            CheckEvidence {
                name: "post".into(),
                issuer: None,
                result: GateResult::Failed,
            },
            CheckEvidence {
                name: "post".into(),
                issuer: None,
                result: GateResult::Pending,
            },
        ];
        r.forge.check_runs.insert(
            "l1".into(),
            vec![GateEvidence {
                checks,
                ..super::super::receipt::tests::evidence("l1")
            }],
        );
        assert_eq!(
            observe(
                &mut r.forge,
                "l1",
                &["post".into()],
                std::time::Duration::from_secs(1)
            )?,
            Observed::Unsettled {
                failed: vec!["post".into()],
                pending: vec!["post".into()]
            }
        );
        Ok(())
    }

    #[test]
    fn green_post_merge_check_raises_nothing() -> Result<(), LandingError> {
        let mut r = rig(true, true, 2);
        post_result(&mut r, "l2", GateResult::Passed);
        run(&mut r)?;
        assert!(r.alarms.0.is_empty() && r.receipt.alarms.is_empty());
        assert!(r.gate.observed.is_empty());
        Ok(())
    }

    #[test]
    fn lost_approval_alarms_after_publication() -> Result<(), LandingError> {
        let mut r = rig(false, false, 2);
        r.forge.approvals_at_head.remove(&(1, "h1".into()));
        post_result(&mut r, "l2", GateResult::Passed);
        run(&mut r)?;
        assert_eq!(r.alarms.0.len(), 1);
        assert_eq!(r.alarms.0[0].code, "approval_lost");
        assert_eq!(r.receipt.alarms, r.alarms.0);
        Ok(())
    }

    #[test]
    fn failure_without_bisect_alarms_but_names_no_culprit() -> Result<(), LandingError> {
        let mut r = rig(false, true, 3);
        post_result(&mut r, "l3", GateResult::Failed);
        run(&mut r)?;
        assert_eq!(r.alarms.0.len(), 1);
        assert_eq!(r.alarms.0[0].code, "post_merge_failed");
        assert!(r.gate.observed.is_empty());
        assert!(r.forge.reverts.is_empty());
        Ok(())
    }

    #[test]
    fn bisect_names_the_first_failing_prefix_and_leaves_no_scratch_refs() -> Result<(), LandingError>
    {
        let mut r = rig(true, false, 4);
        post_result(&mut r, "l4", GateResult::Failed);
        // Prefix 2 passes, prefix 3 fails: pull request 3 is the culprit.
        r.gate.results.push_back(verdict("s4", GateResult::Failed));
        r.gate.results.push_back(verdict("s2", GateResult::Passed));
        r.gate.results.push_back(verdict("s3", GateResult::Failed));
        run(&mut r)?;
        assert_eq!(r.gate.observed, ["s4", "s2", "s3"]);
        assert!(r.alarms.0[0].message.contains("#3"));
        assert!(r.forge.reverts.is_empty());
        assert!(r.repo.refs.is_empty());
        Ok(())
    }

    #[test]
    fn revert_forward_opens_only_when_enabled() -> Result<(), LandingError> {
        let mut r = rig(true, true, 2);
        post_result(&mut r, "l2", GateResult::Failed);
        r.gate.results.push_back(verdict("s2", GateResult::Failed));
        r.gate.results.push_back(verdict("s1", GateResult::Passed));
        run(&mut r)?;
        assert_eq!(
            r.forge.reverts,
            [(
                2,
                format!("{}revert/2", LandSettings::default().candidate_ref_prefix),
                "main".into(),
                "post".into()
            )]
        );
        assert_eq!(
            r.alarms.0.last().map(|a| a.code.as_str()),
            Some("revert_forward_opened")
        );
        Ok(())
    }

    #[test]
    fn inconclusive_bisection_blames_nobody_and_opens_no_revert() -> Result<(), LandingError> {
        let mut r = rig(true, true, 3);
        post_result(&mut r, "l3", GateResult::Failed);
        r.gate.results.push_back(verdict("s3", GateResult::Failed));
        r.gate.results.push_back(verdict("s2", GateResult::Pending));
        run(&mut r)?;
        assert!(r.forge.reverts.is_empty());
        assert!(r.alarms.0[0].message.contains("could not isolate"));
        Ok(())
    }

    #[test]
    fn follow_passes_remaining_budget_to_each_read() -> Result<(), LandingError> {
        let mut r = rig(false, false, 1);
        let mut settings = r.receipt.policy.settings().unwrap();
        settings.post_merge.timeout_seconds = 6;
        settings.poll_seconds = 3;
        r.receipt.policy = crate::pr::landing::policy::LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .unwrap();
        let elapsed = std::cell::Cell::new(std::time::Duration::ZERO);
        follow_with(
            &mut r.forge,
            &mut r.repo,
            Approvals::forge(),
            &mut r.gate,
            &mut r.alarms,
            &mut r.receipt,
            &|duration| elapsed.set(elapsed.get() + duration),
            &|| elapsed.get(),
        )?;
        assert_eq!(
            r.forge.check_timeouts,
            [
                std::time::Duration::from_secs(6),
                std::time::Duration::from_secs(3)
            ]
        );
        assert!(r.alarms.0.iter().any(|alarm| alarm.code == "unsettled"));
        Ok(())
    }

    #[test]
    fn passing_observation_after_deadline_is_passed() -> Result<(), LandingError> {
        let mut r = rig(false, false, 1);
        post_result(&mut r, "l1", GateResult::Passed);
        let calls = std::cell::Cell::new(0);
        follow_with(
            &mut r.forge,
            &mut r.repo,
            Approvals::forge(),
            &mut r.gate,
            &mut r.alarms,
            &mut r.receipt,
            &|_| {},
            &|| {
                let n = calls.get();
                calls.set(n + 1);
                std::time::Duration::from_secs(if n == 0 { 0 } else { 3601 })
            },
        )?;
        assert!(r.alarms.0.is_empty());
        Ok(())
    }

    #[test]
    fn failure_observed_at_deadline_is_not_unsettled() -> Result<(), LandingError> {
        let mut r = rig(false, false, 1);
        post_result(&mut r, "l1", GateResult::Failed);
        let calls = std::cell::Cell::new(0);
        follow_with(
            &mut r.forge,
            &mut r.repo,
            Approvals::forge(),
            &mut r.gate,
            &mut r.alarms,
            &mut r.receipt,
            &|_| {},
            &|| {
                let n = calls.get();
                calls.set(n + 1);
                std::time::Duration::from_secs(if n == 0 { 0 } else { 3601 })
            },
        )?;
        assert!(r.alarms.0.iter().all(|alarm| alarm.code != "unsettled"));
        assert!(
            r.alarms
                .0
                .iter()
                .any(|alarm| alarm.code == "post_merge_failed")
        );
        Ok(())
    }

    #[test]
    fn deadline_keeps_failure_while_other_check_is_pending() -> Result<(), LandingError> {
        let mut r = rig(true, false, 1);
        let mut settings = r.receipt.policy.settings().unwrap();
        settings.post_merge.timeout_seconds = 2;
        settings.post_merge.checks = vec!["failed".into(), "pending".into()];
        r.receipt.policy = crate::pr::landing::policy::LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .unwrap();
        r.forge.check_runs.insert(
            "l1".into(),
            vec![GateEvidence {
                checks: vec![
                    CheckEvidence {
                        name: "failed".into(),
                        issuer: None,
                        result: GateResult::Failed,
                    },
                    CheckEvidence {
                        name: "pending".into(),
                        issuer: None,
                        result: GateResult::Pending,
                    },
                ],
                ..super::super::receipt::tests::evidence("l1")
            }],
        );
        let elapsed = std::cell::Cell::new(std::time::Duration::ZERO);
        follow_with(
            &mut r.forge,
            &mut r.repo,
            Approvals::forge(),
            &mut r.gate,
            &mut r.alarms,
            &mut r.receipt,
            &|duration| elapsed.set(elapsed.get() + duration),
            &|| elapsed.get(),
        )?;
        assert!(
            r.alarms
                .0
                .iter()
                .any(|a| a.code == "unsettled" && a.message.contains("pending"))
        );
        assert!(r.alarms.0.iter().any(|a| a.code == "post_merge_failed"
            && a.message.contains("failed")
            && a.message.contains("#1")));
        Ok(())
    }

    #[test]
    fn timeout_raises_unsettled_and_accounts_for_pool() -> Result<(), LandingError> {
        let mut r = rig(true, false, 2);
        let mut settings = r.receipt.policy.settings().unwrap();
        settings.post_merge.timeout_seconds = 2;
        settings.post_merge.pool = Some("post".into());
        settings
            .budget
            .rate_micros_per_minute
            .insert("post".into(), 60);
        r.receipt.policy = crate::pr::landing::policy::LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .unwrap();
        r.receipt.cost = CostEstimate::Known { micros: 5 };
        follow_with(
            &mut r.forge,
            &mut r.repo,
            Approvals::forge(),
            &mut r.gate,
            &mut r.alarms,
            &mut r.receipt,
            &|_| {},
            &|| std::time::Duration::ZERO,
        )?;
        assert_eq!(r.alarms.0[0].code, "unsettled");
        assert_eq!(r.receipt.cost, CostEstimate::Known { micros: 7 });
        assert!(r.forge.reverts.is_empty());
        Ok(())
    }

    #[test]
    fn bisection_runs_are_included_in_post_merge_cost() -> Result<(), LandingError> {
        let mut r = rig(true, false, 2);
        let mut settings = r.receipt.policy.settings().unwrap();
        settings.post_merge.pool = Some("post".into());
        settings
            .budget
            .rate_micros_per_minute
            .insert("post".into(), 60);
        r.receipt.policy = crate::pr::landing::policy::LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .unwrap();
        r.receipt.cost = CostEstimate::Known { micros: 5 };
        post_result(&mut r, "l2", GateResult::Failed);
        for (commit, result, seconds) in
            [("s2", GateResult::Failed, 3), ("s1", GateResult::Passed, 2)]
        {
            let mut evidence = verdict(commit, result);
            evidence.pool = Some("post".into());
            evidence.duration_seconds = Some(seconds);
            r.gate.results.push_back(evidence);
        }
        run(&mut r)?;
        assert_eq!(r.receipt.cost, CostEstimate::Known { micros: 10 });
        Ok(())
    }

    #[test]
    fn unpriced_bisection_run_keeps_cost_unknown() -> Result<(), LandingError> {
        let mut r = rig(true, false, 2);
        let mut settings = r.receipt.policy.settings().unwrap();
        settings.post_merge.pool = Some("post".into());
        settings
            .budget
            .rate_micros_per_minute
            .insert("post".into(), 60);
        r.receipt.policy = crate::pr::landing::policy::LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .unwrap();
        r.receipt.cost = CostEstimate::Known { micros: 5 };
        post_result(&mut r, "l2", GateResult::Failed);
        r.gate.results.push_back(verdict("s2", GateResult::Failed));
        r.gate.results.push_back(verdict("s1", GateResult::Passed));
        run(&mut r)?;
        assert!(
            matches!(&r.receipt.cost, CostEstimate::Unknown { reason } if reason.contains("bisection"))
        );
        Ok(())
    }

    #[test]
    fn shadow_runs_do_nothing() -> Result<(), LandingError> {
        let mut r = rig(true, true, 2);
        post_result(&mut r, "l2", GateResult::Failed);
        r.receipt.shadow = true;
        run(&mut r)?;
        assert!(r.alarms.0.is_empty());
        Ok(())
    }
}
