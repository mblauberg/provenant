#![forbid(unsafe_code)]

pub mod disclosure;
pub mod git_evidence;
pub mod output_custody;
pub mod permissions;
pub mod quarantine;
pub mod redact;
pub mod repo;
mod stable_read;
mod status;
#[cfg(test)]
mod test_support;
pub mod utility;
pub mod worktrees;
pub use disclosure::scan_disclosure;
pub use git_evidence::Git;
pub use output_custody::OutputCustody;
pub use permissions::ConfinementBackend;
pub use redact::Redactor;
pub use repo::{VerifiedRepo, VerifiedWorktree, verify_worktree};
pub use utility::{UtilityBudget, UtilityRead, UtilityRuntime, run_utility};
