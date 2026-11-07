//! Sans-IO process policy and provider custody.
//!
//! Utilities run through bounded execution; providers remain in guardian custody.
#![deny(unsafe_code)]

// Custody-specific syscalls remain alongside the process policy.
#[allow(unsafe_code, clippy::disallowed_methods)]
mod process_os;
pub use process_os::profile::{ProfileFile, ProfileIdentity};

mod build_pause;
pub mod census;
pub mod custody;
#[cfg(feature = "fixture")]
pub mod fixture;
mod guardian;
pub mod machine;
pub mod process_tree;
#[cfg(feature = "fixture")]
pub use guardian::ExitPublisher;

/// Test process whose lifetime owns fixture-only guardians.
#[cfg(feature = "fixture")]
pub const FIXTURE_OWNER_PID_ENV: &str = "PROVENANT_TEST_FIXTURE_OWNER_PID";
#[cfg(feature = "fixture")]
pub(crate) const FIXTURE_OWNER_ENV: &str = "PROVENANT_TEST_FIXTURE_OWNER";
pub use guardian::{
    ChildRegistry, DiskPressure, GuardianHooks, Holder, RegisteredChild, supervise,
};
