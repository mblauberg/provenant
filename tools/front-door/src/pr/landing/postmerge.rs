use super::{
    LandingError,
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
    Unsettled,
}

fn observe(
    forge: &mut dyn Forge,
    commit: &str,
    names: &[String],
) -> Result<Observed, LandingError> {
    let runs = forge.checks(commit)?;
    let mut failed = Vec::new();
    let mut unsettled = false;
    for name in names {
        let results: Vec<&GateResult> = runs
            .iter()
            .flat_map(|run| run.checks.iter())
            .filter(|check| &check.name == name)
            .map(|check| &check.result)
            .collect();
        if results.iter().any(|r| matches!(r, GateResult::Failed)) {
            failed.push(name.clone());
        } else if results.is_empty() || !results.iter().all(|r| matches!(r, GateResult::Passed)) {
            unsettled = true;
        }
    }
    Ok(if !failed.is_empty() {
        Observed::Failed(failed)
    } else if unsettled {
        Observed::Unsettled
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
) -> Option<u64> {
    let steps = &receipt.candidate_steps;
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
        match verdict.ok()?.result {
            GateResult::Passed => low = mid + 1,
            GateResult::Failed => high = mid,
            _ => return None,
        }
    }
    steps.get(low.checked_sub(1)?).map(|step| step.pr.number)
}

/// Recheck approvals after publication; follow configured checks, bisect failures,
/// send alarms and respect revert-forward policy.
pub fn follow(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    gate: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    receipt: &mut LandingReceipt,
) -> Result<(), LandingError> {
    if receipt.shadow {
        return Ok(());
    }
    let settings = receipt
        .policy
        .settings()
        .map_err(|e| LandingError::Failed(format!("policy settings: {e}")))?;
    let post = settings.post_merge;

    // Approvals must still stand at the approved head after publication.
    for pr in receipt.pull_requests.clone() {
        let standing = forge
            .approvals(pr.number, &pr.head)?
            .iter()
            .any(|approval| approval.reference == pr.approval);
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
    let failed = match observe(forge, &landed, &post.checks)? {
        Observed::Passed => return Ok(()),
        // Nothing to blame yet; a later run observes the settled result.
        Observed::Unsettled => return Ok(()),
        Observed::Failed(names) => names,
    };
    let mut message = format!(
        "post-merge check {} failed on {} at {landed}",
        failed.join(", "),
        receipt.publish_target()
    );
    let mut culprit = None;
    if post.bisect {
        culprit = match receipt.candidate_steps.len() {
            0 => None,
            1 => Some(receipt.candidate_steps[0].pr.number),
            _ => {
                let mut definition = receipt.gate_definition.clone();
                definition.checks = post.checks.clone();
                bisect(repo, gate, receipt, &definition, settings.poll_seconds)
            }
        };
        match culprit {
            Some(number) => message.push_str(&format!("; first failing pull request is #{number}")),
            None => message.push_str("; bisection could not isolate a pull request"),
        }
    }
    raise(alarms, receipt, alarm("post_merge_failed", message))?;

    if let (true, Some(number)) = (post.revert_forward, culprit) {
        let landed_commit = receipt
            .pull_requests
            .iter()
            .find(|pr| pr.number == number)
            .and_then(|pr| receipt.approved_to_landed.get(&pr.head))
            .cloned()
            .unwrap_or(landed);
        let outcome = match forge.open_revert(number, &landed_commit) {
            Ok(revert) => alarm(
                "revert_forward_opened",
                format!("opened pull request #{revert} reverting #{number}"),
            ),
            Err(error) => alarm(
                "revert_forward_failed",
                format!("could not open a revert for #{number}: {error}"),
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
            &mut rig.gate,
            &mut rig.alarms,
            &mut rig.receipt,
        )
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
        r.gate.results.push_back(verdict("s2", GateResult::Passed));
        r.gate.results.push_back(verdict("s3", GateResult::Failed));
        run(&mut r)?;
        assert_eq!(r.gate.observed, ["s2", "s3"]);
        assert!(r.alarms.0[0].message.contains("#3"));
        assert!(r.forge.reverts.is_empty());
        assert!(r.repo.refs.is_empty());
        Ok(())
    }

    #[test]
    fn revert_forward_opens_only_when_enabled() -> Result<(), LandingError> {
        let mut r = rig(true, true, 2);
        post_result(&mut r, "l2", GateResult::Failed);
        r.gate.results.push_back(verdict("s1", GateResult::Passed));
        run(&mut r)?;
        assert_eq!(r.forge.reverts, [(2, "l2".to_string())]);
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
        r.gate.results.push_back(verdict("s2", GateResult::Pending));
        run(&mut r)?;
        assert!(r.forge.reverts.is_empty());
        assert!(r.alarms.0[0].message.contains("could not isolate"));
        Ok(())
    }

    #[test]
    fn unsettled_checks_and_shadow_runs_do_nothing() -> Result<(), LandingError> {
        let mut r = rig(true, true, 2);
        run(&mut r)?;
        assert!(r.alarms.0.is_empty());
        post_result(&mut r, "l2", GateResult::Failed);
        r.receipt.shadow = true;
        run(&mut r)?;
        assert!(r.alarms.0.is_empty());
        Ok(())
    }
}
