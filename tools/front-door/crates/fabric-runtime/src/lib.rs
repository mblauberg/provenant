//! Inert Fabric runtime contracts and policy. No command is switched to this crate.
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::expect_used,
    clippy::print_stderr
)]
#![forbid(unsafe_code)]

pub mod batch_execution;
pub mod execution;
pub mod fabric_policy;
pub mod field;
pub mod gates;
pub mod hosts;
pub mod identity;
pub mod json_subprocess;
pub mod lane_watch;
pub mod run_registry;
pub mod run_root;
pub mod success_status;

mod pending;
