//! Typed routing evidence. ExtraFields carries only unknown forward fields.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionRule {
    WeightedDraw,
    ExplorationDraw,
    ModelChoiceOverride,
    FirstZeroWeightReady,
    FirstStandbyReady,
    FirstRecoverableUnavailable,
    FirstUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionSource {
    Flag,
    Graph,
    Config,
}
impl ExclusionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Graph => "graph",
            Self::Config => "config",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExclusionRule {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub source: ExclusionSource,
    pub selector: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exploration {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub selected: bool,
    pub percent: u8,
    pub enabled: bool,
    pub source: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeightSource {
    Config,
    Catalogue,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PacingPolicy {
    Pace,
    Spend,
    Max,
    Off,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    #[default]
    General,
    Strong,
    Frontier,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workload {
    #[default]
    General,
    ReadHeavy,
    EditHeavy,
    Review,
}

/// The exact routing inputs used to derive comparable prior roles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteAttemptContext {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub workload: Workload,
    pub writes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortWarning {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub requested: String,
    pub applied: Option<String>,
    pub model: String,
    pub source: String,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowanceWarning {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub warning: String,
    pub source: String,
}
/// Reset-adjusted usable capacity per live lane. Unknown inputs remain absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLoad {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub account_key: Option<String>,
    pub account_source: String,
    pub headroom_basis_points: Option<u64>,
    pub reset_eta_seconds: Option<u64>,
    pub live_lanes: Option<u64>,
    pub pacing: PacingPolicy,
    pub pacing_factor_basis_points: u32,
    pub penalty_basis_points: u32,
    pub source: String,
    pub warnings: Vec<String>,
}
impl AccountLoad {
    pub fn score(&self) -> u64 {
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteDiagnostics {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub effort_warning: Option<EffortWarning>,
    pub allowance_warning: Option<AllowanceWarning>,
    pub pacing_reserve_fact: Option<crate::diagnostic::ResourceFact>,
    pub account_load: Option<AccountLoad>,
    pub account_exclusions: Vec<String>,
    /// Warnings produced while preparing this exact route for launch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launch_warnings: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityPreference {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub capability: Capability,
    pub weight: u16,
    pub selector: String,
    pub source: WeightSource,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePrior<Model = String> {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub model: Model,
    pub pool: String,
    pub role: String,
    pub successes: u64,
    pub failures: u64,
    pub source: String,
}
impl<Model> RoutePrior<Model> {
    /// Beta(1,1) shrinkage keeps low-confidence estimates close to neutral.
    pub fn factor_basis_points(&self) -> u32 {
        let success = u128::from(self.successes);
        let total = success + u128::from(self.failures);
        ((20_000 * (success + 1)) / (total + 2)).clamp(1, 20_000) as u32
    }
    pub fn map_model<Other>(self, map: impl FnOnce(Model) -> Other) -> RoutePrior<Other> {
        RoutePrior {
            extra: self.extra,
            model: map(self.model),
            pool: self.pool,
            role: self.role,
            successes: self.successes,
            failures: self.failures,
            source: self.source,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorSource {
    #[serde(rename = "config:disabled")]
    Disabled,
    RouteOutcomes,
    NoEvidence,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomePrior {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub enabled: bool,
    pub role: String,
    pub pool: String,
    pub factor_basis_points: u32,
    pub evidence_count: u64,
    pub evidence: Option<RoutePrior>,
    pub source: PriorSource,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRanking {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub capability_preference: Option<CapabilityPreference>,
    pub outcome_prior: Option<OutcomePrior>,
    pub effort_warning: Option<EffortWarning>,
    pub ranking_factor_basis_points: Option<u32>,
    pub account_load: Option<AccountLoad>,
}

impl WeightSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Catalogue => "catalogue",
        }
    }
}
impl PriorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "config:disabled",
            Self::RouteOutcomes => "route_outcomes",
            Self::NoEvidence => "no_evidence",
        }
    }
}
