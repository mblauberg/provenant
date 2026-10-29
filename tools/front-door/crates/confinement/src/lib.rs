#![forbid(unsafe_code)]

pub mod cache;
pub mod disclosure;
pub mod disk;
pub mod git_evidence;
pub mod guard;
pub mod output_custody;
pub mod permissions;
pub mod plan;
pub mod quarantine;
pub mod redact;
pub mod repo;
pub mod run_config;
pub mod scratch;
mod stable_read;
mod status;
#[cfg(test)]
mod test_support;
pub mod transport;
pub mod utility;
pub mod worktrees;
pub use git_evidence::Git;
pub use output_custody::OutputCustody;
pub use permissions::{
    ConfinementBackend, MAX_WRITE_GRANTS, WriteGrant, WriteSource, write_grants,
};
pub use plan::{
    CodexPolicy, ConfinedPlan, ExtensionPolicy, MAX_READ_ONLY_WRITABLE, PlanError, PlanRequest,
    valid_read_only_writable,
};
pub use redact::Redactor;
pub use repo::{ReadOnlyRepo, VerifiedRepo, VerifiedWorktree, verify_worktree};
pub use utility::{GitRead, UtilityBudget, UtilityRuntime, run_read_utility, run_utility};
pub mod push;
pub mod shell_command;
