use crate::{ConfigError, loader::parse_typed};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Balanced,
    Economy,
    Rigorous,
}

/// Presets describe intent only. Domain owners enforce policy and authority;
/// these values cannot authorise an operation or produce a provider stop reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub concurrency_cap: Option<u64>,
    pub context_cap_tokens: Option<u64>,
    pub pool_selection: PoolSelection,
    pub effort: EffortPolicy,
    pub additional_reviews: u64,
    pub cross_family: CrossFamily,
    pub rereview_scope: RereviewScope,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolSelection {
    HighestWeight,
    WeightedRotation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortPolicy {
    Default,
    RaiseExceptMechanicalAndBulk,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossFamily {
    Prefer,
    Require,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RereviewScope {
    Delta,
    Full,
}

impl Profile {
    pub(crate) fn preset(self) -> Result<Preset, ConfigError> {
        let (name, asset) = match self {
            Self::Balanced => (
                "balanced",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/presets/balanced.toml"
                )),
            ),
            Self::Economy => (
                "economy",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/presets/economy.toml"
                )),
            ),
            Self::Rigorous => (
                "rigorous",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/presets/rigorous.toml"
                )),
            ),
        };
        parse_typed(asset, Path::new(name))
    }
}
