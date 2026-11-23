//! Host setting for per-change warning memory. Kept out of `ResolvedConfig`'s
//! serialised shape because that aggregate already uses the `warnings` key for
//! ignored-setting findings; the section is recovered separately and reported
//! through `origins` and `config schema`.
//!
//! Spec: docs/specs/cli.md#warnings-are-typed-values
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How long a per-change warning key is remembered in the state root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WarningsSettings {
    /// Days to keep a seen key before pruning it from `warnings.sqlite3`.
    pub dedup_retention_days: u64,
}

impl Default for WarningsSettings {
    fn default() -> Self {
        Self {
            dedup_retention_days: 30,
        }
    }
}

impl WarningsSettings {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3650).contains(&self.dedup_retention_days) {
            return Err("dedup_retention_days_range");
        }
        Ok(())
    }
}

/// Effective `warnings.dedup_retention_days` for a state root. The compiled
/// default applies when the config is absent or unreadable; a malformed value
/// is a config finding elsewhere, never a reason to stop emitting warnings.
pub fn warnings_dedup_retention_days(state: &Path) -> u64 {
    crate::loader::read_settings::<WarningsSettings>(state, "warnings", WarningsSettings::validate)
        .map(|settings| settings.dedup_retention_days)
        .unwrap_or_else(|_| WarningsSettings::default().dedup_retention_days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_defaults_to_thirty_days_and_is_bounded() {
        assert_eq!(WarningsSettings::default().dedup_retention_days, 30);
        assert!(WarningsSettings::default().validate().is_ok());
        for days in [0, 3651, u64::MAX] {
            assert_eq!(
                WarningsSettings {
                    dedup_retention_days: days
                }
                .validate(),
                Err("dedup_retention_days_range")
            );
        }
    }
}
