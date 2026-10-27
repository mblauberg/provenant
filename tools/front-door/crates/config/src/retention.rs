//! Retention of terminal attempts' provider state and closed-run captures.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSettings {
    pub provider_state_days: u64,
    pub logs_days: u64,
    /// Revisions a session checkpoint keeps, the current one included.
    pub session_revisions: u64,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            provider_state_days: 7,
            logs_days: 30,
            session_revisions: 8,
        }
    }
}

impl RetentionSettings {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3650).contains(&self.provider_state_days) {
            return Err("provider_state_days_range");
        }
        if !(1..=3650).contains(&self.logs_days) {
            return Err("logs_days_range");
        }
        if !(1..=64).contains(&self.session_revisions) {
            return Err("session_revisions_range");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log_retention_defaults_to_thirty_days_and_rejects_invalid_windows() {
        assert_eq!(RetentionSettings::default().logs_days, 30);
        for days in [0, 3651, u64::MAX] {
            assert_eq!(
                RetentionSettings {
                    logs_days: days,
                    ..Default::default()
                }
                .validate(),
                Err("logs_days_range")
            );
        }
        for days in [1, 30, 3650] {
            assert!(
                RetentionSettings {
                    logs_days: days,
                    ..Default::default()
                }
                .validate()
                .is_ok()
            );
        }
    }
}
