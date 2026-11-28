//! The invocation deadline on the process monotonic clock.
//!
//! A command's `--timeout` becomes one [`Deadline`] when it enters the
//! fabric, and every nested read takes what remains from it, so no reader
//! samples ambient time of its own.

use provenant_core::{Clock, Deadline, DurationMicros, MonoTime};
use provenant_os::clock::SystemClock;
use std::time::Duration;

/// A deadline `timeout` from now.
pub(crate) fn after(timeout: Duration) -> Deadline {
    let micros = u64::try_from(timeout.as_micros()).unwrap_or(u64::MAX);
    Deadline::after(
        SystemClock::new().mono_now(),
        DurationMicros::from_micros(micros),
    )
    .unwrap_or(Deadline::at(MonoTime::from_micros(u64::MAX)))
}

/// What remains before `deadline`; zero once it has passed.
pub(crate) fn left(deadline: Deadline) -> Duration {
    Duration::from_micros(
        deadline
            .remaining(SystemClock::new().mono_now())
            .as_micros(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_reports_only_what_remains_and_never_goes_negative() {
        assert!(left(after(Duration::from_secs(60))) <= Duration::from_secs(60));
        assert!(left(after(Duration::from_secs(60))) > Duration::from_secs(50));
        assert_eq!(left(after(Duration::ZERO)), Duration::ZERO);
    }
}
