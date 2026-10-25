//! Sans-IO process policy and the safe operating-system boundary.
//!
//! Utilities run through bounded execution; providers remain in guardian custody.
#![deny(unsafe_code)]

pub mod clock;
pub mod disk;
pub mod exec;
pub mod fs;
pub mod git;
pub mod home;
mod identity;
pub mod pid;
// This is the sole syscall boundary, including nonblocking child/lock waits.
#[allow(unsafe_code, clippy::disallowed_methods)]
pub mod sys;

pub mod capture;
pub mod census;
pub mod custody;
#[cfg(feature = "fixture")]
pub mod fixture;
mod guardian;
pub mod machine;
pub mod scratch;
#[cfg(test)]
mod test_support;

#[cfg(feature = "fixture")]
pub use guardian::ExitPublisher;
pub use guardian::{
    ChildRegistry, GuardianHooks, Holder, NestedChildStore, RegisteredChild, supervise,
};
