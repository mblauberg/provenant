//! A lane or graph limit with where its value came from and whether the host
//! enforces it. The lane preamble states only what the host will enforce.
use serde::{Deserialize, Serialize};

/// Where a limit's value was set, from weakest to strongest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitSource {
    /// The built-in default.
    Default,
    /// The host configuration.
    Host,
    /// The project configuration.
    Project,
    /// The template the node expands from.
    Template,
    /// The launch request, set by the chair or the owner.
    Launch,
}

/// What reaching the limit does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitStrength {
    /// The host stops or refuses the work at the limit.
    Enforced,
    /// The host reports the limit to the agent and does not act on it.
    Advisory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limit {
    pub value: u64,
    pub source: LimitSource,
    pub strength: LimitStrength,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

impl Limit {
    pub fn new(value: u64, source: LimitSource, strength: LimitStrength) -> Self {
        Self {
            value,
            source,
            strength,
            extra: Default::default(),
        }
    }

    pub fn enforced(&self) -> bool {
        self.strength == LimitStrength::Enforced
    }
}
