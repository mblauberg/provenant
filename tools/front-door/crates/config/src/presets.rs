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
impl Profile {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "balanced" => Some(Self::Balanced),
            "economy" => Some(Self::Economy),
            "rigorous" => Some(Self::Rigorous),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Economy => "economy",
            Self::Rigorous => "rigorous",
        }
    }
}
