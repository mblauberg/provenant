//! The native hook timeout domain. Installed hooks and the hook runtime share
//! one setting so the local budget stays below the harness's kill timeout.
use crate::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The installed hook timeout, and the ceiling the hook's own work never reaches.
pub const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 10;
/// One budget for every hook, comfortably below its installed timeout so the
/// hook can warn and return before the harness kills it.
pub const HOOK_BUDGET_PERCENT: u64 = 60;

/// The local wall budget derived from one installed timeout. Integer maths keeps
/// the result identical on every host and avoids floating-point rounding.
pub fn hook_budget(timeout_secs: u64) -> Duration {
    Duration::from_millis(timeout_secs.saturating_mul(HOOK_BUDGET_PERCENT * 10))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Seconds written as the native hook timeout. Must leave room for the
    /// local budget, which is a fraction of it.
    pub timeout_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_HOOK_TIMEOUT_SECS,
        }
    }
}

impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        if self.timeout_secs == 0 {
            return vec![SectionError {
                field: "timeout_secs".into(),
                code: "out_of_range",
                detail: Some("timeout_secs must be at least one second"),
            }];
        }
        Vec::new()
    }
}

pub const SECTION: DomainSection =
    DomainSection::new::<Settings>("hooks", |_| "timeout_secs = 10\n")
        .with_fields(&[FieldMetadata::int("timeout_secs", 1, 300)]);
