use super::{
    LandingError,
    model::*,
    ports::{Forge, PullRequestState, Repo},
    receipt,
};
use std::collections::BTreeMap;
/// Resolve an unknown publication using ancestry and forge state before retries or cleanup.
pub fn reconcile(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    target: &str,
    candidate: &Candidate,
) -> Result<Vec<(u64, Outcome)>, LandingError> {
    let tip = repo.fetch_target(target)?;
    let candidate_landed = repo.is_ancestor(&candidate.commit, &tip)?;
    let mut outcomes = Vec::new();
    for step in &candidate.steps {
        let landed = if candidate_landed {
            true
        } else {
            let pr = forge.pull_request(step.pr.number)?;
            if pr.state == PullRequestState::Merged && pr.head == step.pr.head {
                match pr.merged_commit {
                    Some(commit) => repo.is_ancestor(&commit, &tip)?,
                    None => false,
                }
            } else {
                false
            }
        };
        outcomes.push((
            step.pr.number,
            if landed {
                Outcome::Landed
            } else {
                Outcome::PendingUnknown
            },
        ));
    }
    Ok(outcomes)
}
/// Recover pending publications before selection, retaining unprovable outcomes.
/// In shadow mode, refuse when recovery would mutate a ref.
pub fn recover_pending(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    state: &std::path::Path,
    target: &str,
    shadow: bool,
) -> Result<(), LandingError> {
    for attempt in receipt::pending(state, target)? {
        let outcomes = reconcile(forge, repo, target, &attempt.candidate)?;
        if shadow
            || outcomes
                .iter()
                .any(|(_, outcome)| *outcome != Outcome::Landed)
        {
            return Err(LandingError::PublicationUnknown);
        }
        let tip = repo.fetch_target(target)?;
        let exact = repo.is_ancestor(&attempt.candidate.commit, &tip)?;
        let mut mapping = BTreeMap::new();
        for step in &attempt.candidate.steps {
            let landed = if exact {
                step.commit.clone()
            } else {
                let pr = forge.pull_request(step.pr.number)?;
                if pr.state != PullRequestState::Merged || pr.head != step.pr.head {
                    return Err(LandingError::PublicationUnknown);
                }
                let commit = pr.merged_commit.ok_or(LandingError::PublicationUnknown)?;
                if !repo.is_ancestor(&commit, &tip)? {
                    return Err(LandingError::PublicationUnknown);
                }
                commit
            };
            mapping.insert(step.pr.head.clone(), landed.clone());
            mapping.insert(step.commit.clone(), landed);
        }
        let last = attempt
            .candidate
            .steps
            .last()
            .and_then(|step| mapping.get(&step.commit))
            .ok_or(LandingError::PublicationUnknown)?;
        let tree = repo.tree_of(last)?;
        let alarms = if tree == attempt.candidate.tree {
            Vec::new()
        } else {
            vec![Alarm {
                code: "LAND_TREE_MISMATCH".into(),
                message: format!(
                    "gated candidate {} tree {} differs from landed commit {} tree {}",
                    attempt.candidate.commit, attempt.candidate.tree, last, tree
                ),
            }]
        };
        let record = LandingReceipt {
            policy: attempt.policy,
            base: attempt.candidate.base.clone(),
            candidate: attempt.candidate.commit.clone(),
            tree: attempt.candidate.tree.clone(),
            candidate_ref: attempt.candidate.ref_name.clone(),
            candidate_steps: attempt.candidate.steps.clone(),
            pull_requests: attempt
                .candidate
                .steps
                .iter()
                .map(|step| step.pr.clone())
                .collect(),
            gate_definition: attempt.gate_definition,
            gate_evidence: attempt.gate_evidence,
            publish: attempt.publish,
            outcomes: outcomes.into_iter().collect(),
            approved_to_landed: mapping,
            cost: attempt.cost,
            alarms,
            shadow: false,
        };
        receipt::write_receipt(state, &record)?;
        // A mismatch requires operator attention; retain the pending marker.
        if !record.alarms.is_empty() {
            return Err(LandingError::PublicationUnknown);
        }
        receipt::clear_pending(state, &attempt.candidate)?;
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
    use crate::pr::landing::{
        fakes::{FakeForge, FakeRepo},
        ports::PullRequestInfo,
    };

    #[test]
    fn reconciliation_requires_candidate_ancestry_or_pinned_forge_merge() -> Result<(), LandingError>
    {
        let pr = EligiblePr {
            number: 1,
            head: "approved".into(),
            approval: "review".into(),
            base_branch: "main".into(),
        };
        let candidate = Candidate {
            base: "base".into(),
            steps: vec![CandidateStep {
                pr,
                commit: "candidate".into(),
                tree: "tree".into(),
            }],
            commit: "candidate".into(),
            tree: "tree".into(),
            ref_name: "land/test".into(),
        };
        let mut forge = FakeForge::default();
        forge.pull_requests.insert(
            1,
            PullRequestInfo {
                number: 1,
                head: "approved".into(),
                base_branch: "main".into(),
                author: "author".into(),
                state: PullRequestState::Merged,
                merged_commit: Some("landed".into()),
            },
        );
        let mut repo = FakeRepo {
            target: "tip".into(),
            ..FakeRepo::default()
        };
        assert_eq!(
            reconcile(&mut forge, &mut repo, "main", &candidate)?,
            vec![(1, Outcome::PendingUnknown)]
        );
        repo.parents.insert("tip".into(), vec!["landed".into()]);
        assert_eq!(
            reconcile(&mut forge, &mut repo, "main", &candidate)?,
            vec![(1, Outcome::Landed)]
        );
        forge
            .pull_requests
            .get_mut(&1)
            .ok_or(LandingError::PublicationUnknown)?
            .head = "other".into();
        assert_eq!(
            reconcile(&mut forge, &mut repo, "main", &candidate)?,
            vec![(1, Outcome::PendingUnknown)]
        );
        repo.parents.insert("tip".into(), vec!["candidate".into()]);
        assert_eq!(
            reconcile(&mut forge, &mut repo, "main", &candidate)?,
            vec![(1, Outcome::Landed)]
        );
        Ok(())
    }
}
