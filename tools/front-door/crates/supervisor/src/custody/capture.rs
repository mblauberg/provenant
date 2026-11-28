//! Bounds a launch's capture plan must respect before custody hands it off.

use super::{CapturePlan, CustodyError, MAX_RETAINED_CUSTODY_SECONDS, Result};
use std::ops::RangeInclusive;

const STREAM_LIMIT: u64 = 64 * 1024 * 1024;
const WALL_LIMIT_MS: u64 = 86_400_000;
const GRACE_LIMIT_MS: u64 = 60_000;

impl CapturePlan {
    /// Refuse a plan outside its bounds, naming the field and value so a
    /// never-spawned launch says what to fix.
    pub(crate) fn validate(&self) -> Result<()> {
        let bounds: [(&'static str, u64, RangeInclusive<u64>); 6] = [
            ("stdout limit", self.stdout.limit(), 0..=STREAM_LIMIT),
            ("stderr limit", self.stderr.limit(), 0..=STREAM_LIMIT),
            ("wall_ms", self.wall_ms, 1..=WALL_LIMIT_MS),
            (
                "descendant_grace_ms",
                self.descendant_grace_ms,
                0..=GRACE_LIMIT_MS,
            ),
            ("root_flush_ms", self.root_flush_ms, 0..=GRACE_LIMIT_MS),
            (
                "retained_custody_ms",
                self.retained_custody_ms,
                1..=MAX_RETAINED_CUSTODY_SECONDS * 1_000,
            ),
        ];
        for (field, value, bound) in bounds {
            if !bound.contains(&value) {
                return Err(CustodyError::CapturePlan {
                    field,
                    value,
                    bound: format!("{}..={}", bound.start(), bound.end()),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_plan_names_its_field_and_value() {
        assert!(CapturePlan::default().validate().is_ok());
        let plan = CapturePlan {
            wall_ms: 0,
            ..CapturePlan::default()
        };
        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            CustodyError::CapturePlan {
                field: "wall_ms",
                value: 0,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "capture plan wall_ms is 0, outside 1..=86400000"
        );
        let plan = CapturePlan {
            retained_custody_ms: MAX_RETAINED_CUSTODY_SECONDS * 1_000 + 1,
            ..CapturePlan::default()
        };
        assert!(matches!(
            plan.validate(),
            Err(CustodyError::CapturePlan {
                field: "retained_custody_ms",
                ..
            })
        ));
    }
}
