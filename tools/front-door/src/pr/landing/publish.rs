use super::{
    LandingError,
    model::*,
    ports::{Forge, Repo},
};
use provenant_cli::domains::{MergeMethod, Publication};
/// Choose the configured publication mode, checking direct-push authority for auto.
pub fn decide(
    forge: &mut dyn Forge,
    target: &str,
    requested: Publication,
    method: MergeMethod,
) -> Result<PublishDecision, LandingError> {
    let _ = (forge, target, requested, method);
    Err(LandingError::Unimplemented("publish::decide"))
}
/// Return PRs whose pinned head or approval changed immediately before publication.
pub fn revalidate(
    forge: &mut dyn Forge,
    candidate: &Candidate,
) -> Result<Vec<EligiblePr>, LandingError> {
    let _ = (forge, candidate);
    Err(LandingError::Unimplemented("publish::revalidate"))
}
/// Advance the target from the candidate base with an expected-old-value push.
pub fn fast_forward(
    repo: &mut dyn Repo,
    target: &str,
    candidate: &Candidate,
) -> Result<PublishResult, LandingError> {
    let _ = (repo, target, candidate);
    Err(LandingError::Unimplemented("publish::fast_forward"))
}
/// Merge in order at pinned heads and record an alarm if the final tree differs from the gated tree.
pub fn rest_ordered(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    target: &str,
    candidate: &Candidate,
    method: MergeMethod,
) -> Result<PublishResult, LandingError> {
    let _ = (forge, repo, target, candidate, method);
    Err(LandingError::Unimplemented("publish::rest_ordered"))
}
