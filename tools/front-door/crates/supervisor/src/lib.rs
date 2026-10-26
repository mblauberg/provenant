//! Sans-IO process policy and provider custody.
//!
//! Utilities run through bounded execution; providers remain in guardian custody.
#![deny(unsafe_code)]

// Custody-specific syscalls remain alongside the process policy.
#[allow(unsafe_code, clippy::disallowed_methods)]
mod process_os;

pub mod census;
pub mod custody;
#[cfg(feature = "fixture")]
pub mod fixture;
mod guardian;
pub mod machine;
#[cfg(feature = "fixture")]
pub use guardian::ExitPublisher;
pub use guardian::{
    ChildRegistry, GuardianHooks, Holder, NestedChildStore, RegisteredChild, supervise,
};
