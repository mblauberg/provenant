//! Bounded evidence-process capture policy.
//!
//! Process creation, process-group signalling, descriptor draining and reaping
//! belong to the supervisor safe OS owner. This module contains the request
//! contract and byte-tail reducer used at that boundary; it does not signal a
//! PID or start an independently owned child.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

pub const POST_EXIT_DRAIN: Duration = Duration::from_millis(100);
pub const READ_CHUNK_BYTES: usize = 8192;
pub const SELECT_POLL_MAX: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{0}")]
    Refused(String),
    #[error("supervisor process-group API is unavailable")]
    SupervisorApiUnavailable,
    #[error("evidence process failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceProcessRequest {
    pub command: Vec<OsString>,
    pub cwd: PathBuf,
    pub timeout_seconds: f64,
    pub max_log_bytes: i64,
}

impl EvidenceProcessRequest {
    pub fn validate(&self) -> Result<(), ProcessError> {
        if self.command.is_empty() {
            return Err(ProcessError::Refused(
                "evidence run requires a command after --".into(),
            ));
        }
        Ok(())
    }
}

/// A retained suffix of one process stream, matching Python's
/// `del buffer[:-max_log_bytes]` semantics (including zero and negative bounds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTail {
    max_log_bytes: i64,
    bytes: Vec<u8>,
}

impl CaptureTail {
    pub fn new(max_log_bytes: i64) -> Self {
        Self {
            max_log_bytes,
            bytes: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        let len = self.bytes.len();
        if (len as i128) > i128::from(self.max_log_bytes) {
            let remove = if self.max_log_bytes > 0 {
                len.saturating_sub(self.max_log_bytes as usize)
            } else if self.max_log_bytes < 0 {
                len.min(self.max_log_bytes.unsigned_abs() as usize)
            } else {
                0
            };
            self.bytes.drain(..remove);
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The supervisor adapter owns process groups and must apply the recorded
/// timing/capture rules. No implementation is supplied until that safe API is
/// exposed through the allowed confinement edge.
pub trait EvidenceProcessHost {
    fn execute_bounded(
        &self,
        request: &EvidenceProcessRequest,
        post_exit_drain: Duration,
        read_chunk_bytes: usize,
        poll_max: Duration,
    ) -> Result<EvidenceProcessOutput, ProcessError>;
}

pub fn execute_bounded(
    request: &EvidenceProcessRequest,
    host: &impl EvidenceProcessHost,
) -> Result<EvidenceProcessOutput, ProcessError> {
    request.validate()?;
    host.execute_bounded(request, POST_EXIT_DRAIN, READ_CHUNK_BYTES, SELECT_POLL_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_keeps_a_byte_suffix_and_replaces_invalid_utf8() {
        let mut capture = CaptureTail::new(4);
        capture.push(b"abc");
        capture.push(b"\xffdef");
        assert_eq!(capture.bytes(), b"\xffdef");
        assert_eq!(capture.text_lossy(), "\u{fffd}def");
    }

    #[test]
    fn zero_limit_matches_python_slice_no_op() {
        let mut capture = CaptureTail::new(0);
        capture.push(b"unbounded-by-source-expression");
        assert_eq!(capture.bytes(), b"unbounded-by-source-expression");
    }

    #[test]
    fn empty_command_is_refused_before_host_effects() {
        let request = EvidenceProcessRequest {
            command: vec![],
            cwd: PathBuf::from("."),
            timeout_seconds: 30.0,
            max_log_bytes: 1024,
        };
        assert_eq!(
            request.validate().unwrap_err().to_string(),
            "evidence run requires a command after --"
        );
    }
}
