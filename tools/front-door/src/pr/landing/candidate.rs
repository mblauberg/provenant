use super::{
    LandingError,
    model::*,
    ports::{Forge, Repo},
};
/// Select eligible pull requests in requested order, up to the configured batch limit.
pub fn select_batch(
    forge: &mut dyn Forge,
    numbers: &[u64],
    target: &str,
    limit: usize,
    batch_wait_seconds: u64,
) -> Result<Vec<EligiblePr>, LandingError> {
    let _ = (forge, numbers, target, limit, batch_wait_seconds);
    Err(LandingError::Unimplemented("candidate::select_batch"))
}
/// Build and push an exact candidate chain from the fetched target and approved heads.
pub fn build_candidate(
    repo: &mut dyn Repo,
    base: &str,
    prs: &[EligiblePr],
    ref_prefix: &str,
) -> Result<Candidate, LandingError> {
    let _ = (repo, base, prs, ref_prefix);
    Err(LandingError::Unimplemented("candidate::build_candidate"))
}
