use super::{LandingError, model::*};
/// Validate provenance and binding, returning the observed gate result; invalid evidence is an error.
pub fn validate_evidence(
    candidate: &Candidate,
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> Result<GateResult, LandingError> {
    let _ = (candidate, definition, evidence);
    Err(LandingError::Unimplemented("gate::validate_evidence"))
}
/// Prove whether a pull request run covers the candidate tree and definition.
pub fn pr_run_covers(
    candidate: &Candidate,
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> Result<bool, LandingError> {
    let _ = (candidate, definition, evidence);
    Err(LandingError::Unimplemented("gate::pr_run_covers"))
}
/// Execute a configured command plan in an isolated candidate checkout.
pub fn execute_commands(
    project: &std::path::Path,
    candidate: &Candidate,
    definition: &GateDefinition,
) -> Result<GateEvidence, LandingError> {
    let _ = (project, candidate, definition);
    Err(LandingError::Unimplemented("gate::execute_commands"))
}

/// Follow the configured pull-request CI evidence policy without bypassing forge requirements.
pub fn await_pr_ci(
    forge: &mut dyn super::ports::Forge,
    prs: &[EligiblePr],
    policy: &provenant_cli::domains::PrCiSettings,
    poll_seconds: u64,
) -> Result<(), LandingError> {
    let _ = (forge, prs, policy, poll_seconds);
    Err(LandingError::Unimplemented("gate::await_pr_ci"))
}
