//! Inert argument and rendering boundaries. Command ownership has not switched.
#![forbid(unsafe_code)]

pub mod harness;
pub mod install;

/// Complete command result, kept separate from internal diagnostics.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
}

pub mod clean;
mod pending;

pub mod git_evidence;

pub mod lane_safety;

pub mod projection;

pub mod stale_checkout;

#[cfg(test)]
mod oracle;
