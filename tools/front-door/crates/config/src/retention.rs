//! Retention of terminal attempts' resumable provider state.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSettings {
    pub provider_state_days: u64,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            provider_state_days: 7,
        }
    }
}

impl RetentionSettings {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3650).contains(&self.provider_state_days) {
            return Err("provider_state_days_range");
        }
        Ok(())
    }
}
