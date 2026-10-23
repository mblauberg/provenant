//! Fabric identity and coordination, with retained run/peer owners awaiting F3b/F6.
#![forbid(unsafe_code)]

pub mod batch_execution;
pub mod config;
pub mod coordination;
pub mod execution;
// Rewrite pending: these oracle tests retire with their F3b policy owner.
#[allow(clippy::disallowed_methods, clippy::print_stderr)]
pub mod fabric_policy;
pub mod field;
pub mod gates;
pub mod hosts;
pub mod identity;
pub mod json_subprocess;
// Rewrite pending: the run-query owner replaces this old reader.
#[allow(clippy::disallowed_methods)]
pub mod lane_watch;
pub mod run_registry;
pub mod run_root;
pub mod success_status;

// Rewrite pending: used only by retained F3b/F6 modules.
#[allow(clippy::expect_used)]
mod pending;
