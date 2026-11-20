//! Build report aggregation seam; S4 supplies ledger analysis.

use serde::{Deserialize, Serialize};

/// Aggregated build-soak report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub rows: u64,
    pub warnings: Vec<String>,
}

/// Empty S0 report until S4 implements the soak thresholds.
pub fn empty() -> Report {
    Report::default()
}
