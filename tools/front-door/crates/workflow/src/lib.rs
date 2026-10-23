//! Pure workflow policy and minimal receipt constructors.
// Rewrite pending: E/G retain the subprocess oracle until their own steps.
#![allow(
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::print_stderr
)]
#![forbid(unsafe_code)]

pub mod compact_reinject;
pub mod receipt;
pub mod review;
pub mod risk;
pub mod run_gates;

#[cfg(test)]
mod test_support;
