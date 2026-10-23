//! Typed snapshots and boundaries for live model discovery.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveModel {
    pub id: String,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default, rename = "default")]
    pub is_default: bool,
    #[serde(default)]
    pub inherits: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySnapshot {
    /// Age is supplied by the caller's clock; this record has no wall-clock dependency.
    pub observed_age_seconds: u64,
    #[serde(default)]
    pub models: Vec<LiveModel>,
}

impl CapabilitySnapshot {
    pub fn is_fresh(&self, maximum_age_seconds: u64) -> bool {
        self.observed_age_seconds <= maximum_age_seconds
    }
}

#[derive(Debug, Error)]
pub enum LiveProbeError {
    #[error("provider listing unavailable")]
    Unavailable,
    #[error("provider listing is malformed")]
    Malformed,
    #[error("provider listing exceeded its time budget")]
    DeadlineExceeded,
}

/// Provider discovery is an injected edge; route decisions stay synchronous and deterministic.
pub trait LiveModelSource {
    fn cached_snapshot(&self, adapter: &str) -> Option<CapabilitySnapshot>;
    fn probe(
        &self,
        adapter: &str,
        deadline_seconds: Option<u64>,
    ) -> Result<CapabilitySnapshot, LiveProbeError>;
}

#[cfg(test)]
mod tests {
    use super::{CapabilitySnapshot, LiveModel};

    #[test]
    fn cached_capabilities_use_an_explicit_freshness_cutoff() {
        let snapshot = CapabilitySnapshot {
            observed_age_seconds: 7,
            models: vec![LiveModel {
                id: "gpt-6.2-sol".into(),
                names: Vec::new(),
                is_default: false,
                inherits: vec!["gpt-6.1-sol".into()],
            }],
        };
        assert!(snapshot.is_fresh(8));
        assert!(!snapshot.is_fresh(6));
        assert_eq!(snapshot.models[0].inherits, ["gpt-6.1-sol"]);
    }
}
