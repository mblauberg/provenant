//! Plan permissions, quarantine, secret redaction and safe Git and path policy.
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::print_stderr
)]
#![forbid(unsafe_code)]

pub mod agy_home;
pub mod instruction_quarantine;
pub mod lane_protection;
pub mod output_custody;
mod pending;
#[cfg(test)]
mod python;
pub mod trusted_git;
