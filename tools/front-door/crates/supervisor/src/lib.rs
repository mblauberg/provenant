//! Sans-IO process policy and the safe operating-system boundary.
//!
//! These APIs are inert until the command and S2 custody integration passes.
#![deny(unsafe_code)]

pub mod clock;
pub mod exec;
pub mod fs;
mod identity;
pub mod pid;
#[allow(unsafe_code, clippy::disallowed_methods)]
pub mod sys;

#[cfg(feature = "fixture")]
pub mod fixture;
pub mod utility;
