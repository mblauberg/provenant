//! Capability preferences and immutable quality evidence.
use crate::{ModelId, Workload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use provenant_records::route_selection::{AccountLoad, Capability};

pub type CapabilityPreferences = BTreeMap<String, BTreeMap<Capability, u16>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutcomePriorSettings {
    pub enabled: bool,
    pub exploration_percent: u8,
}
impl Default for OutcomePriorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            exploration_percent: 10,
        }
    }
}

/// Comparable, attributed outcomes in one pool and role. Infrastructure stops
/// and absent evidence must not be passed here as model failures.
pub type RoutePrior = provenant_records::route_selection::RoutePrior<ModelId>;

#[derive(Debug, Clone, Default)]
pub struct SelectionEvidence {
    pub role: Option<String>,
    pub live_lanes_known: bool,
    pub live_lanes: BTreeMap<(String, String), u64>,
    pub priors: Vec<RoutePrior>,
    pub source: String,
}
/// The one role key shared by outcome producers and route consumers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PriorKey(String);
impl PriorKey {
    pub fn for_attempt(workload: Workload, writes: bool, explicit_role: Option<&str>) -> Self {
        Self(
            explicit_role
                .unwrap_or(if workload == Workload::Review {
                    "review"
                } else if writes {
                    "writer"
                } else {
                    "task"
                })
                .to_owned(),
        )
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
