use serde::{Deserialize, Serialize};

/// Selects the per-profile assets exported by each registered domain. Config
/// owns no run, routing or workflow policy and has no parallel preset channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Balanced,
    Economy,
    Rigorous,
}
