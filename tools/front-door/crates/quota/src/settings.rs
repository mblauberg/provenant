//! Config can lower execution ceilings, never relax the recorded safety limits.
use provenant_config::{DomainConfig, DomainSection, SectionError};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub max_parallel: u64,
    pub wall_budget_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_parallel: 3,
            wall_budget_seconds: 55,
        }
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        [
            ("max_parallel", (1..=3).contains(&self.max_parallel)),
            (
                "wall_budget_seconds",
                (1..=55).contains(&self.wall_budget_seconds),
            ),
        ]
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(field, _)| SectionError {
            field: field.into(),
            code: "ceiling",
        })
        .collect()
    }
}
pub const SECTION: DomainSection =
    DomainSection::new::<Settings>("quota", |_| "max_parallel = 3\nwall_budget_seconds = 55\n");
