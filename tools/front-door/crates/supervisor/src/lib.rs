//! Sans-IO process policy and the safe operating-system boundary.
//!
//! Utilities run through bounded execution; providers remain in guardian custody.
#![deny(unsafe_code)]

pub mod clock;
pub mod exec;
pub mod fs;
mod identity;
pub mod pid;
// This is the sole syscall boundary, including nonblocking child/lock waits.
#[allow(
    unsafe_code,
    clippy::disallowed_methods,
    clippy::multiple_unsafe_ops_per_block
)]
pub mod sys;

pub mod capture;
pub mod census;
pub mod custody;
#[cfg(feature = "fixture")]
pub mod fixture;
mod guardian;
pub mod machine;
pub mod scratch;
pub mod utility;

pub use guardian::{ChildRegistry, ExitPublisher, GuardianHooks, RegisteredChild, supervise};
