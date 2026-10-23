//! Sans-IO process policy and the safe operating-system boundary.
//!
//! These APIs are inert until the command and S2 custody integration passes.
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::expect_used,
    clippy::multiple_unsafe_ops_per_block,
    clippy::print_stderr,
    clippy::undocumented_unsafe_blocks,
    clippy::unwrap_used
)]
#![deny(unsafe_code)]

pub mod bounded;
pub mod capture;
pub mod exec;
pub mod fabric_ps;
pub mod fs;
pub mod identity;
mod pending;
pub mod pid;
pub mod process_info;
pub mod ps;
#[allow(unsafe_code)]
pub mod sys;
#[cfg(test)]
mod test_support;
pub mod worker_liveness;

mod legacy_json;
