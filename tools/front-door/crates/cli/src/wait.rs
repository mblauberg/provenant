//! Settings for the `wait` verb. Source cadence remains overridable.
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub timeout_seconds: u64,
    pub local_interval_ms: u64,
    pub pr_interval_ms: u64,
    pub pr_slow_interval_ms: u64,
    pub pr_near_end_window_ms: u64,
    pub pr_conditional_requests: bool,
    pub max_targets: u64,
    pub pr_read_timeout_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            local_interval_ms: 500,
            pr_interval_ms: 5000,
            pr_slow_interval_ms: 15000,
            pr_near_end_window_ms: 30000,
            pr_conditional_requests: true,
            max_targets: 64,
            pr_read_timeout_seconds: 60,
        }
    }
}
impl Settings {
    pub fn timeout(&self, override_seconds: Option<u64>) -> u64 {
        override_seconds
            .filter(|seconds| *seconds != 0)
            .unwrap_or(self.timeout_seconds)
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        [
            ("timeout_seconds", self.timeout_seconds, 1, 14400),
            ("local_interval_ms", self.local_interval_ms, 10, 60000),
            ("pr_interval_ms", self.pr_interval_ms, 10, 60000),
            ("pr_slow_interval_ms", self.pr_slow_interval_ms, 10, 60000),
            ("pr_near_end_window_ms", self.pr_near_end_window_ms, 0, 60000),
            ("max_targets", self.max_targets, 1, 1024),
            (
                "pr_read_timeout_seconds",
                self.pr_read_timeout_seconds,
                1,
                600,
            ),
        ]
        .into_iter()
        .filter(|(_, value, min, max)| !(min..=max).contains(&value))
        .map(|(field, ..)| SectionError {
            detail: None,
            field: field.into(),
            code: "range",
        })
        .collect()
    }
}
pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "wait",
    |_| {
        "timeout_seconds = 300\nlocal_interval_ms = 500\npr_interval_ms = 5000\npr_slow_interval_ms = 15000\npr_near_end_window_ms = 30000\npr_conditional_requests = true\nmax_targets = 64\npr_read_timeout_seconds = 60\n"
    },
)
.with_boundary_checks()
.with_fields(&[
    FieldMetadata::int("timeout_seconds", 1, 14400),
    FieldMetadata::int("local_interval_ms", 10, 60000),
    FieldMetadata::int("pr_interval_ms", 10, 60000),
    FieldMetadata::int("pr_slow_interval_ms", 10, 60000),
    FieldMetadata::int("pr_near_end_window_ms", 0, 60000),
    FieldMetadata::new("pr_conditional_requests", "bool"),
    FieldMetadata::int("max_targets", 1, 1024),
    FieldMetadata::int("pr_read_timeout_seconds", 1, 600),
]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_precedence_flag_over_config_over_default() {
        let default_settings = Settings::default();
        assert_eq!(default_settings.timeout_seconds, 300);
        assert_eq!(default_settings.timeout(None), 300);
        assert_eq!(default_settings.timeout(Some(0)), 300);
        assert_eq!(default_settings.timeout(Some(10)), 10);

        let custom_settings = Settings {
            timeout_seconds: 120,
            ..Settings::default()
        };
        assert_eq!(custom_settings.timeout(None), 120);
        assert_eq!(custom_settings.timeout(Some(0)), 120);
        assert_eq!(custom_settings.timeout(Some(45)), 45);
    }
}
