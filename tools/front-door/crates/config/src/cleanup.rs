//! Removal of what finished lanes leave behind.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CleanupSettings {
    /// Days a ref Provenant created stays when it has not landed, counted
    /// from when the ledger recorded it.
    pub unlanded_days: u64,
}

impl Default for CleanupSettings {
    fn default() -> Self {
        Self { unlanded_days: 14 }
    }
}

impl CleanupSettings {
    pub fn unlanded_seconds(&self) -> i64 {
        i64::try_from(self.unlanded_days.saturating_mul(86_400)).unwrap_or(i64::MAX)
    }
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3650).contains(&self.unlanded_days) {
            return Err("unlanded_days_range");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlanded_days_defaults_to_fourteen_and_is_bounded() {
        assert_eq!(CleanupSettings::default().unlanded_days, 14);
        assert_eq!(CleanupSettings::default().unlanded_seconds(), 14 * 86_400);
        for days in [0, 3651] {
            assert_eq!(
                CleanupSettings {
                    unlanded_days: days
                }
                .validate(),
                Err("unlanded_days_range")
            );
        }
        let parsed: CleanupSettings = toml_edit::de::from_str("unlanded_days = 30\n").unwrap();
        assert_eq!(parsed.unlanded_days, 30);
        assert!(parsed.validate().is_ok());
    }
}
