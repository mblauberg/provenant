//! Dispatch, attempts, admission, batch, retry and publication orchestration.
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::print_stderr
)]
#![forbid(unsafe_code)]

pub mod detached;
pub mod lease;
pub mod lineage;
mod pending;

#[cfg(test)]
mod oracle;
