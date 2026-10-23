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

pub mod capture;
pub mod exec;
pub mod fs;
pub mod identity;
pub mod pid;
#[allow(unsafe_code)]
pub mod sys;

mod legacy_json;
