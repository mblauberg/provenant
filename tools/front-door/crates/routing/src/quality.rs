//! Declared capability eligibility and immutable preference evidence.
use crate::{ModelId, Workload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    #[default]
    General,
    Strong,
    Frontier,
}

/// Advisory eligibility floor for a named route. Exact model requests are pins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CapabilityPolicy {
    #[default]
    Disabled,
    Minimum {
        minimum: Capability,
        fallback: CapabilityFallback,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFallback {
    /// Relax the floor only when it leaves no otherwise eligible member.
    WarnRelax,
}
impl CapabilityPolicy {
    pub fn minimum(self) -> Option<Capability> {
        match self {
            Self::Disabled => None,
            Self::Minimum { minimum, .. } => Some(minimum),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityDisposition {
    Excluded,
    Waived,
}

#[derive(Debug, Serialize)]
pub(crate) struct CapabilityAssessment {
    pub model: String,
    pub capability: Capability,
    pub minimum: Capability,
    pub disposition: CapabilityDisposition,
}
#[derive(Debug, Serialize)]
pub(crate) struct CapabilityDecision {
    pub policy: CapabilityPolicy,
    pub source: String,
    pub below_minimum: Vec<CapabilityAssessment>,
    pub relaxed: bool,
    pub warning: Option<String>,
}

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutePrior {
    pub model: ModelId,
    pub pool: String,
    pub role: String,
    pub successes: u64,
    pub failures: u64,
    pub source: String,
}
impl RoutePrior {
    /// Beta(1,1) shrinkage keeps low-confidence estimates close to neutral.
    pub fn factor_basis_points(&self) -> u32 {
        let success = u128::from(self.successes);
        let total = success + u128::from(self.failures);
        ((20_000 * (success + 1)) / (total + 2)).clamp(1, 20_000) as u32
    }
}

#[derive(Debug, Clone, Default)]
pub struct SelectionEvidence {
    pub role: Option<String>,
    pub live_lanes_known: bool,
    pub live_lanes: BTreeMap<(String, String), u64>,
    pub priors: Vec<RoutePrior>,
    pub source: String,
}
impl SelectionEvidence {
    pub fn role(&self, workload: Workload, writes: bool) -> &str {
        self.role
            .as_deref()
            .unwrap_or(if workload == Workload::Review {
                "review"
            } else if writes {
                "writer"
            } else {
                "task"
            })
    }
}

/// Reset-adjusted usable capacity per live lane. Unknown inputs remain absent
/// in receipts and use neutral defaults only for ranking.
#[derive(Debug, Clone, Serialize)]
pub struct AccountLoad {
    pub account_key: Option<String>,
    pub account_source: String,
    pub headroom_basis_points: Option<u64>,
    pub reset_eta_seconds: Option<u64>,
    pub live_lanes: Option<u64>,
    pub pacing: crate::PacingPolicy,
    pub pacing_factor_basis_points: u32,
    pub penalty_basis_points: u32,
    pub source: String,
    pub warnings: Vec<String>,
}
impl AccountLoad {
    pub fn score(&self) -> u64 {
        // Shorter reset times increase the capacity that can safely be spent.
        let headroom = self.headroom_basis_points.unwrap_or(10_000).min(10_000);
        let reset = self.reset_eta_seconds.unwrap_or(3_600).max(1);
        let lanes = self.live_lanes.unwrap_or(0).saturating_add(1);
        let score = u128::from(headroom) * 3_600 * u128::from(self.penalty_basis_points.max(1))
            / u128::from(reset)
            / u128::from(lanes)
            / 10_000;
        score.clamp(1, u128::from(u64::MAX)) as u64
    }
}
