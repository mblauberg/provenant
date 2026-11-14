//! Lifecycle acquisition uses the same host ledger as fabric claims.
use provenant_core::{ProjectId, StateRoot};
use provenant_fabric_store::{HostStore, Request, Result};
use provenant_records::custody::*;

pub fn acquire(
    root: &StateRoot,
    project: ProjectId,
    holder: &Holder,
    subjects: &[Subject],
    origin: Origin,
    call: &Request<'_>,
) -> Result<Vec<CustodyChange>> {
    let ledger = HostStore::open(root, call)?;
    ledger.acquire_custody_group(project, subjects, holder, origin, call)
}
/// Advisory diagnostics never change admission or custody policy.
pub fn warning(message: impl std::fmt::Display) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
}
/// A warning is an observation, never a custody lock.
pub fn warn(changes: &[CustodyChange]) {
    for change in changes {
        if let Some(holder) = &change.previous_holder {
            warning(format_args!(
                "{} was held by {holder:?}; custody changed and recorded",
                change.record.subject
            ));
        }
    }
}
