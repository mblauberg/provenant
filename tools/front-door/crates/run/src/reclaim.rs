//! Deadline-aware interface for resumable cleanup work.

use provenant_core::Deadline;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimCursor {
    pub page_start_seq: u64,
    pub page_end_seq: u64,
    /// Stable candidate identities visited in the current page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visited: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimReport {
    pub scanned: usize,
    pub reclaimed: usize,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ReclaimCursor>,
}

/// A bounded cleanup owner. Implementations keep their own candidate policy;
/// the shared driver owns deadline checks and cursor persistence.
#[allow(dead_code)]
pub trait Reclaimable {
    type Plan;
    type Error;

    fn class(&self) -> &'static str;

    fn scan(
        &mut self,
        cursor: Option<&ReclaimCursor>,
        deadline: &Deadline,
    ) -> Result<(Self::Plan, ReclaimReport), Self::Error>;

    fn apply(
        &mut self,
        plan: Self::Plan,
        deadline: &Deadline,
    ) -> Result<ReclaimReport, Self::Error>;
}
