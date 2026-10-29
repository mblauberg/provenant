//! Operating-system primitives and bounded utility execution.
#![deny(unsafe_code)]

pub mod build_slots;
pub mod capture;
pub mod changes;
pub mod clock;
pub mod disk;
pub mod exec;
pub mod fs;
pub mod git;
pub mod home;
mod identity;
pub mod lane_ports;
pub mod pid;
pub mod provider_fence;
pub mod scratch;
#[allow(unsafe_code, clippy::disallowed_methods)]
pub mod sys;

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;
