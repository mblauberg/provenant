//! Collect candidate refs and pending records left by lands whose pull
//! requests are merged or closed.
use super::{
    ports::{Forge, PullRequestState, Repo},
    receipt,
};

fn terminal(forge: &mut dyn Forge, number: u64) -> bool {
    forge.pull_request(number).is_ok_and(|info| {
        matches!(
            info.state,
            PullRequestState::Merged | PullRequestState::Closed
        )
    })
}

/// The pull request a candidate ref was built for: `<prefix><number>-...`.
fn ref_member(name: &str, prefix: &str) -> Option<u64> {
    name.strip_prefix(prefix)?.split('-').next()?.parse().ok()
}

/// Delete pending records and candidate refs whose pull requests are all
/// merged or closed. A failed collection is a warning, never a failed land.
pub fn collect_stale(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    state: &std::path::Path,
    repository: &str,
    target: &str,
    prefix: &str,
) -> Vec<String> {
    let mut warnings = Vec::new();
    match receipt::pending(state, repository, target) {
        Ok(attempts) => {
            for attempt in attempts {
                let candidate = &attempt.candidate;
                if !candidate
                    .steps
                    .iter()
                    .all(|step| terminal(forge, step.pr.number))
                {
                    continue;
                }
                let result = repo
                    .delete_ref(&candidate.ref_name, &candidate.commit)
                    .and_then(|()| receipt::clear_pending(state, repository, candidate));
                if let Err(error) = result {
                    warnings.push(format!(
                        "stale landing record {} not collected: {error}",
                        candidate.ref_name
                    ));
                }
            }
        }
        Err(error) => warnings.push(format!("stale landing records not read: {error}")),
    }
    match repo.list_refs(prefix) {
        Ok(refs) => {
            for (name, commit) in refs {
                if ref_member(&name, prefix).is_some_and(|number| terminal(forge, number))
                    && let Err(error) = repo.delete_ref(&name, &commit)
                {
                    warnings.push(format!("stale candidate ref {name} not removed: {error}"));
                }
            }
        }
        Err(error) => warnings.push(format!("candidate refs not listed: {error}")),
    }
    warnings
}
