//! Settings for the mixed-target status wait. Source cadence remains overridable.
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub timeout_seconds: u64,
    pub local_interval_ms: u64,
    pub pr_interval_ms: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            local_interval_ms: 500,
            pr_interval_ms: 5000,
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
        ]
        .into_iter()
        .filter(|(_, value, min, max)| !(min..=max).contains(&value))
        .map(|(field, ..)| SectionError {
            field: field.into(),
            code: "range",
        })
        .collect()
    }
}
pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "wait",
    |_| "timeout_seconds = 300\nlocal_interval_ms = 500\npr_interval_ms = 5000\n",
)
.with_boundary_checks()
.with_fields(&[
    FieldMetadata::int("timeout_seconds", 1, 14400),
    FieldMetadata::int("local_interval_ms", 10, 60000),
    FieldMetadata::int("pr_interval_ms", 10, 60000),
]);
