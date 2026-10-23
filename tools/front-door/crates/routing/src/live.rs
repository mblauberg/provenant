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

/// Cache freshness matches model_route_live._cached_live_models.
pub const CAPABILITY_MAX_AGE_SECONDS: i64 = 86400;
pub const OPENCODE_DISCOVERY_BUDGET: f64 = 12.0;
pub const OPENCODE_REFRESH_MIN_REMAINING: f64 = 4.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySnapshot {
    pub observed_at: String,
    #[serde(default)]
    pub models: Vec<String>,
}

impl CapabilitySnapshot {
    /// The caller supplies its clock; this record never reads wall time.
    /// Python's datetime has microsecond precision, including for ISO input.
    pub fn is_fresh(&self, now: jiff::Timestamp) -> bool {
        self.observed_at
            .parse::<jiff::Timestamp>()
            .is_ok_and(|observed| {
                let age = now.as_nanosecond().div_euclid(1000)
                    - observed.as_nanosecond().div_euclid(1000);
                age <= i128::from(CAPABILITY_MAX_AGE_SECONDS) * 1_000_000
            })
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
        deadline: Option<f64>,
    ) -> Result<CapabilitySnapshot, LiveProbeError>;
}

#[cfg(test)]
mod tests {
    use super::{CapabilitySnapshot, LiveModel};

    #[test]
    fn snapshots_read_the_python_capability_cache_shape() {
        let snapshot: CapabilitySnapshot = serde_json::from_str(
            r#"{"observed_at":"2026-10-23T00:00:00Z","models":["gpt-6.1-sol"]}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(snapshot).unwrap()["observed_at"],
            "2026-10-23T00:00:00Z"
        );
    }

    #[test]
    fn cached_capabilities_use_the_python_day_cutoff_with_an_explicit_clock() {
        let snapshot = CapabilitySnapshot {
            observed_at: "2026-10-23T00:00:00.000001Z".into(),
            models: vec!["gpt-6.1-sol".into()],
        };
        assert!(snapshot.is_fresh("2026-10-24T00:00:00.000001Z".parse().unwrap()));
        assert!(!snapshot.is_fresh("2026-10-24T00:00:00.000002Z".parse().unwrap()));
        assert!(snapshot.is_fresh("2026-10-22T00:00:00Z".parse().unwrap()));
        let bad = CapabilitySnapshot {
            observed_at: "invalid".into(),
            models: vec![],
        };
        assert!(!bad.is_fresh("2026-10-24T00:00:00Z".parse().unwrap()));
        let model = LiveModel {
            id: "gpt-6.2-sol".into(),
            inherits: vec!["gpt-6.1-sol".into()],
            ..Default::default()
        };
        assert_eq!(model.inherits, ["gpt-6.1-sol"]);
    }
}
