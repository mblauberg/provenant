//! The ps declaration contract, independent of any process implementation.
use crate::identity::Environment;

/// Matches ps.d.mts: argv and an optional environment produce stdout or an
/// execution error. No new process owner or fallback policy is introduced.
pub trait PsOutput {
    type Error: std::error::Error;
    fn ps_output(&self, args: &[String], env: Option<&Environment>) -> Result<String, Self::Error>;
}
