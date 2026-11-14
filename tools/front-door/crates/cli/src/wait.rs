//! Settings for the `wait` verb. Source cadence remains overridable.
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub timeout_seconds: u64,
    pub max_timeout_seconds: Option<u64>,
    pub local_interval_ms: u64,
    pub pr_interval_ms: u64,
    pub max_targets: u64,
    pub pr_read_timeout_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            max_timeout_seconds: None,
            local_interval_ms: 500,
            pr_interval_ms: 5000,
            max_targets: 64,
            pr_read_timeout_seconds: 60,
        }
    }
}
impl Settings {
    pub fn timeout(
        &self,
        override_seconds: Option<u64>,
        origin: Option<&provenant_config::Origin>,
    ) -> u64 {
        let requested = override_seconds
            .filter(|seconds| *seconds != 0)
            .unwrap_or(self.timeout_seconds);
        let maximum = self
            .max_timeout_seconds
            .unwrap_or(provenant_core::MAX_LANE_RUNTIME_SECONDS);
        if requested > maximum {
            use std::io::Write;
            let source = if self.max_timeout_seconds.is_none() {
                "shipped default".to_owned()
            } else {
                origin
                    .map(|origin| format!("{origin:?}"))
                    .unwrap_or_else(|| "explicit settings".into())
            };
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: wait budget clamped to {maximum}s; domains.wait.max_timeout_seconds (source: {source})"
            );
        }
        requested.min(maximum)
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        [
            ("timeout_seconds", self.timeout_seconds, 1, u32::MAX as u64),
            (
                "max_timeout_seconds",
                self.max_timeout_seconds
                    .unwrap_or(provenant_core::MAX_LANE_RUNTIME_SECONDS),
                1,
                u32::MAX as u64,
            ),
            ("local_interval_ms", self.local_interval_ms, 10, 60000),
            ("pr_interval_ms", self.pr_interval_ms, 10, 60000),
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
        "timeout_seconds = 300\nlocal_interval_ms = 500\npr_interval_ms = 5000\nmax_targets = 64\npr_read_timeout_seconds = 60\n"
    },
)
.with_boundary_checks()
.with_fields(&[
    FieldMetadata::int("timeout_seconds", 1, u32::MAX as i64),
    FieldMetadata::int("max_timeout_seconds", 1, u32::MAX as i64),
    FieldMetadata::int("local_interval_ms", 10, 60000),
    FieldMetadata::int("pr_interval_ms", 10, 60000),
    FieldMetadata::int("max_targets", 1, 1024),
    FieldMetadata::int("pr_read_timeout_seconds", 1, 600),
]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_wait_is_bounded_by_the_effective_maximum() {
        for configured in [None, Some(1), Some(90), Some(86400)] {
            let settings = Settings {
                max_timeout_seconds: configured,
                ..Settings::default()
            };
            let maximum = configured.unwrap_or(provenant_core::MAX_LANE_RUNTIME_SECONDS);
            for requested in [
                None,
                Some(0),
                Some(1),
                Some(60),
                Some(21600),
                Some(u64::MAX),
            ] {
                let effective = requested
                    .filter(|n| *n != 0)
                    .unwrap_or(settings.timeout_seconds);
                assert_eq!(settings.timeout(requested, None), effective.min(maximum));
            }
        }
    }
}
