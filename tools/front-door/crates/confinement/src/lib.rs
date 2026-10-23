#![forbid(unsafe_code)]

pub mod disclosure;
pub mod git_evidence;
pub mod output_custody;
pub mod permissions;
pub mod quarantine;
pub mod redact;
pub mod repo;
mod status;
pub mod utility;
pub mod worktrees;
pub use disclosure::scan_disclosure;
pub use git_evidence::Git;
pub use output_custody::OutputCustody;
pub use permissions::ConfinementBackend;
pub use redact::Redactor;
pub use repo::{VerifiedRepo, VerifiedWorktree, verify_worktree};
pub use utility::{UtilityBudget, UtilityRead, UtilityRuntime, run_utility};

/// Settle classified launch scratch while supervisor retains writer exclusion
/// and verifies both quiescence and Run's durable settlement attestations.
/// Unclassified content and a fenced attempt remain retained.
pub fn settle_scratch(
    recovery: provenant_supervisor::custody::Recovery,
    commit: provenant_supervisor::custody::SettlementCommit,
    scratch: &mut provenant_supervisor::scratch::Scratch,
) -> Result<(), provenant_supervisor::custody::CustodyError> {
    recovery.settle(commit, scratch)
}
