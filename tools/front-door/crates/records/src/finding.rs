use crate::PathWire;
use serde::Serialize;

/// Ordered check output. Paths are relative to the inspected root; locations
/// use one-based lines. Messages contain bounded diagnostics, never input values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: String,
    pub path: PathWire,
    pub line: usize,
    pub message: String,
}
