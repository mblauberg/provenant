use super::{
    LandingError,
    model::*,
    ports::{Forge, Repo},
};
/// Resolve an unknown publication using ancestry and forge state before retries or cleanup.
pub fn reconcile(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    target: &str,
    candidate: &Candidate,
) -> Result<Vec<(u64, Outcome)>, LandingError> {
    let _ = (forge, repo, target, candidate);
    Err(LandingError::Unimplemented("reconcile::reconcile"))
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
    let _ = (forge, repo, state, target, shadow);
    Err(LandingError::Unimplemented("reconcile::recover_pending"))
}
