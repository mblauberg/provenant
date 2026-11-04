use super::{LandingError, model::*};
use std::path::Path;
/// Persist a receipt, including approved-to-landed mappings and shadow decisions.
pub fn write_receipt(path: &Path, receipt: &LandingReceipt) -> Result<(), LandingError> {
    let _ = (path, receipt);
    Err(LandingError::Unimplemented("receipt::write_receipt"))
}
/// Persist the candidate and policy evidence before a publication can become uncertain.
pub fn record_pending(state: &Path, attempt: &PendingAttempt) -> Result<(), LandingError> {
    let _ = (state, attempt);
    Err(LandingError::Unimplemented("receipt::record_pending"))
}
/// Read pending publication attempts for a target before any retry or cleanup.
pub fn pending(state: &Path, target: &str) -> Result<Vec<PendingAttempt>, LandingError> {
    let _ = (state, target);
    Err(LandingError::Unimplemented("receipt::pending"))
}
/// Remove the durable pending marker only after its receipt is settled.
pub fn clear_pending(state: &Path, candidate: &Candidate) -> Result<(), LandingError> {
    let _ = (state, candidate);
    Err(LandingError::Unimplemented("receipt::clear_pending"))
}
