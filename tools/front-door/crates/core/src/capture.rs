//! Streaming capture-filter contract shared by the executor and redaction.
//!
//! Kept below both `os` and `redact` so the executor can install a default
//! local-disclosure mask without a dependency cycle, while redaction still
//! implements the filter it needs to install.

use thiserror::Error;

/// Maximum bytes in one frame emitted by a capture filter, and the maximum
/// source bytes in a structured provider protocol line.
pub const FRAME_LIMIT: usize = 64 * 1024;

/// A capture filter could not decide a frame. The capture drops its buffered
/// bytes and marks the stream as failed rather than passing raw bytes on.
#[derive(Debug, Error)]
#[error("capture filter failed")]
pub struct FilterError;

/// A filter returns zero or more frames per call, each at most `FRAME_LIMIT` bytes.
/// Output is pushed to the caller's sink as soon as it is decided.
pub trait CaptureFilter {
    fn filter(&mut self, bytes: &[u8], sink: &mut dyn FnMut(&[u8])) -> Result<(), FilterError>;

    /// Flush bytes held until the end of the stream. Filters without buffered
    /// state can keep the default implementation.
    fn finish(&mut self, sink: &mut dyn FnMut(&[u8])) -> Result<(), FilterError> {
        let _ = sink;
        Ok(())
    }
}
