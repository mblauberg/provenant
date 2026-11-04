use super::{LandingError, model::CostEstimate};
use provenant_cli::domains::BudgetSettings;
/// Estimate the cost of observed pool durations, preserving unknown prices.
pub fn estimate(
    settings: &BudgetSettings,
    durations_seconds: &[(String, u64)],
) -> Result<CostEstimate, LandingError> {
    let _ = (settings, durations_seconds);
    Err(LandingError::Unimplemented("budget::estimate"))
}
/// Return an advisory warning or refuse an explicit cap excess.
pub fn check(
    settings: &BudgetSettings,
    estimate: &CostEstimate,
) -> Result<Option<String>, LandingError> {
    let _ = (settings, estimate);
    Err(LandingError::Unimplemented("budget::check"))
}
