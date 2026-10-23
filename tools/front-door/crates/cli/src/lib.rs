//! Inert argument and rendering boundaries. Command ownership has not switched.
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::byte_char_slices,
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::print_stderr
)]
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

pub use pending::IntegerError;

pub mod git_evidence;

pub mod lane_safety;

pub mod projection;

pub mod stale_checkout;

#[cfg(test)]
mod oracle;

pub mod pr_checks;

pub mod retry_install;

/// Executable output bytes; preserves Unix filesystem paths at Python's
/// surrogateescape text boundary without admitting invalid Rust strings.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CommandBytes {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: u8,
}
impl From<CommandOutput> for CommandBytes {
    fn from(output: CommandOutput) -> Self {
        Self {
            stdout: output.stdout.into_bytes(),
            stderr: output.stderr.into_bytes(),
            exit_code: output.exit_code,
        }
    }
}

pub mod public_release;

pub mod worktree;

pub mod reap;

pub mod timebomb;
