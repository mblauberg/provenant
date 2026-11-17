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
                "{} was held by {}; custody changed and recorded",
                change.record.subject,
                holder_label(holder),
            ));
        }
    }
}

fn holder_label(holder: &provenant_records::custody::Holder) -> String {
    use provenant_records::custody::Holder;
    match holder {
        Holder::Chair { chair } => format!("chair {} on host {}", chair.actor, chair.host),
        Holder::Attempt { attempt, .. } => format!("attempt {attempt}"),
    }
}

#[cfg(test)]
mod tests {
    use super::holder_label;
    use provenant_core::AttemptId;
    use provenant_records::custody::Holder;

    #[test]
    fn holder_attempt_id_uses_the_hex_display() {
        let holder = Holder::Attempt {
            attempt: AttemptId::from_bytes([0x14; 16]),
            chair: None,
        };
        let label = holder_label(&holder);
        assert!(label.contains(&"14".repeat(16)));
        assert!(!label.contains("AttemptId("));
    }
}
