use super::{
    LandingError,
    model::*,
    ports::{AlarmSink, Forge, GateRunner, Repo},
};
/// Recheck approvals after publication; follow configured checks, bisect failures,
/// send alarms and respect revert-forward policy.
pub fn follow(
    forge: &mut dyn Forge,
    repo: &mut dyn Repo,
    gate: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
    receipt: &mut LandingReceipt,
) -> Result<(), LandingError> {
    let _ = (forge, repo, gate, alarms, receipt);
    Err(LandingError::Unimplemented("postmerge::follow"))
}
