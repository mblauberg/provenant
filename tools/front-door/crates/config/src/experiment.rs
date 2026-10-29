//! Typed project experiment settings. The front door reads this partial view
//! of `domains.fabric`, leaving the Fabric runtime's host settings untouched.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FabricExperiments {
    pub pipelines: PipelineExperiments,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineExperiments {
    pub experiment: BTreeMap<String, ExperimentConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentConfig {
    pub control: String,
    pub arms: BTreeMap<String, String>,
    #[serde(default = "default_assign")]
    pub assign: String,
    #[serde(default)]
    pub shadow: bool,
    pub epsilon: Option<f64>,
    #[serde(default)]
    pub tiers: Vec<String>,
    pub budget_usd: Option<f64>,
    pub max_cost_ratio: Option<f64>,
    pub expires: Option<String>,
}

fn default_assign() -> String {
    "hash".into()
}
