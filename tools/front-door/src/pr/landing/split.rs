use super::{LandingError, model::EligiblePr};
/// Plan ordered prefixes to isolate a red pull request.
pub fn red_prefixes(prs: &[EligiblePr]) -> Result<Vec<Vec<EligiblePr>>, LandingError> {
    let _ = prs;
    Err(LandingError::Unimplemented("split::red_prefixes"))
}
/// Split at a conflicting pull request, preserving the clean prefix.
pub fn conflict_prefix(
    prs: &[EligiblePr],
    conflict: u64,
) -> Result<(Vec<EligiblePr>, Vec<EligiblePr>), LandingError> {
    let _ = (prs, conflict);
    Err(LandingError::Unimplemented("split::conflict_prefix"))
}
/// Account for a target movement against the configured rebuild budget.
pub fn rebuild_allowed(used: u64, maximum: u64) -> Result<bool, LandingError> {
    let _ = (used, maximum);
    Err(LandingError::Unimplemented("split::rebuild_allowed"))
}
