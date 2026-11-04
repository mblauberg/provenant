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
) -> Result<Vec<LandingReceipt>, LandingError> {
    let mut recovered = Vec::new();
    for attempt in receipt::pending(state, repository, target)? {
        let tip = if shadow {
            repo.fetch_target_untracked(target)?
        } else {
            repo.fetch_target(target)?
        };
        let exact = repo.is_ancestor(&attempt.candidate.commit, &tip)?;
        let mut mapping = BTreeMap::new();
        let mut outcomes = BTreeMap::new();
        let mut landed_prs = Vec::new();
        for step in &attempt.candidate.steps {
            let landed = if exact {
                Some(step.commit.clone())
            } else {
                let pr = forge.pull_request(step.pr.number)?;
                match (pr.state, pr.merged_commit) {
                    (PullRequestState::Merged, Some(commit))
                        if pr.head == step.pr.head && repo.is_ancestor(&commit, &tip)? =>
                    {
                        Some(commit)
                    }
                    (PullRequestState::Open | PullRequestState::Closed, _) => None,
                    _ => {
                        return Err(LandingError::PublicationUnknownAt {
                            marker: receipt::pending_path(state, repository, &attempt.candidate)?,
                        });
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
        let mut record = LandingReceipt {
            policy: attempt.policy,
            base: attempt.candidate.base.clone(),
            candidate: attempt.candidate.commit.clone(),
            tree: attempt.candidate.tree.clone(),
            candidate_ref: attempt.candidate.ref_name.clone(),
            candidate_steps: attempt.candidate.steps.clone(),
            pull_requests: landed_prs,
            gate_definition: attempt.gate_definition,
            gate_evidence: attempt.gate_evidence,
            publish: attempt.publish,
            outcomes,
            split: Vec::new(),
            approved_to_landed: mapping,
            cost: attempt.cost,
            alarms: Vec::new(),
            shadow: false,
        };
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
        receipt::write_receipt(state, &record)?;
        for alarm in &record.alarms {
            alarms.post(alarm)?;
        }
        if !record.pull_requests.is_empty() {
            let followed = postmerge::follow(forge, repo, runner, alarms, &mut record);
            receipt::write_receipt(state, &record)?;
            followed?;
        }
        repo.delete_ref(&attempt.candidate.ref_name, &attempt.candidate.commit)?;
        receipt::clear_pending(state, repository, &attempt.candidate)?;
        recovered.push(record);
    }
    Ok(recovered)
}
