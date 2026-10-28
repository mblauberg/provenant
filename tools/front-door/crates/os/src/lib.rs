//! Operating-system primitives and bounded utility execution.
#![deny(unsafe_code)]

pub mod capture;
pub mod clock;
pub mod disk;
pub mod exec;
pub mod fs;
pub mod git;
pub mod home;
mod identity;
pub mod pid;
pub mod provider_fence;
pub mod scratch;
#[allow(unsafe_code, clippy::disallowed_methods)]
pub mod sys;

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;
