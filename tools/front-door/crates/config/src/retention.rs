//! Retention of terminal attempts' resumable provider state.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSettings {
    pub provider_state_days: u64,
    /// Revisions a session checkpoint keeps, the current one included.
    pub session_revisions: u64,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            provider_state_days: 7,
            session_revisions: 8,
        }
    }
}

impl RetentionSettings {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3650).contains(&self.provider_state_days) {
            return Err("provider_state_days_range");
        }
        if !(1..=64).contains(&self.session_revisions) {
            return Err("session_revisions_range");
        }
        Ok(())
    }
}
