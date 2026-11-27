//! A bounded monotonic budget shared through nested work.
//!
//! Deadlines use the caller's sampled clock, so they remain deterministic in
//! tests and never read ambient time. A child can only shorten its parent's
//! remaining budget.

use crate::{DurationMicros, MonoTime, TimeError};

/// Process-local deadline; never persisted as a cross-process timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline(MonoTime);

impl Deadline {
    /// Construct a deadline at a sampled monotonic time.
    pub const fn at(deadline: MonoTime) -> Self {
        Self(deadline)
    }

    /// Construct a deadline `duration` after `now`.
    pub fn after(now: MonoTime, duration: DurationMicros) -> Result<Self, TimeError> {
        now.checked_add(duration).map(Self)
    }

    /// The monotonic instant at which this deadline expires.
    pub const fn as_mono_time(self) -> MonoTime {
        self.0
    }

    pub fn is_expired(self, now: MonoTime) -> bool {
        now >= self.0
    }

    /// Budget remaining at the caller's sampled monotonic time.
    pub fn remaining(self, now: MonoTime) -> DurationMicros {
        DurationMicros::from_micros(self.0.as_micros().saturating_sub(now.as_micros()))
    }

    /// A nested deadline capped by both the parent and the supplied duration.
    pub fn child(self, now: MonoTime, cap: DurationMicros) -> Self {
        let budget = self.remaining(now).as_micros().min(cap.as_micros());
        let capped = now.as_micros().saturating_add(budget);
        Self(MonoTime::from_micros(self.0.as_micros().min(capped)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_parent_does_not_move_child_deadline_forward() {
        let parent = Deadline::at(MonoTime::from_micros(10));
        let child = parent.child(MonoTime::from_micros(20), DurationMicros::from_micros(5));
        assert_eq!(child.as_mono_time(), MonoTime::from_micros(10));
        assert_eq!(
            child.remaining(MonoTime::from_micros(20)),
            DurationMicros::ZERO
        );
    }
}
