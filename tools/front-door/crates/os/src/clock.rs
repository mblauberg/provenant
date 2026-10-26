//! Production sampling of wall and monotonic time.
use provenant_core::{Clock, MonoTime, TimeError, WallTime};
use std::time::SystemTime;

/// Samples UTC wall time through Jiff and one process-wide monotonic origin.
#[derive(Debug, Clone)]
pub struct SystemClock;
impl SystemClock {
    pub fn new() -> Self {
        Self
    }
}
impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}
impl Clock for SystemClock {
    // Wall-clock sampling is the production clock boundary.
    #[allow(clippy::disallowed_methods)]
    fn wall_now(&self) -> Result<WallTime, TimeError> {
        let timestamp =
            jiff::Timestamp::try_from(SystemTime::now()).map_err(|_| TimeError::WallRange)?;
        WallTime::from_micros(timestamp.as_microsecond())
    }

    fn mono_now(&self) -> MonoTime {
        crate::sys::monotonic_now()
    }

    // Waiting uses the same real monotonic coordinate sampled by this clock.
    // It ignores unpark notifications and returns only at the deadline.
    #[allow(clippy::disallowed_methods)]
    fn wait_until(&self, deadline: MonoTime) -> Result<(), TimeError> {
        loop {
            let remaining = deadline
                .as_micros()
                .saturating_sub(self.mono_now().as_micros());
            if remaining == 0 {
                return Ok(());
            }
            std::thread::park_timeout(std::time::Duration::from_micros(remaining));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_wall_and_non_decreasing_elapsed_time() {
        let clock = SystemClock::new();
        let _first = clock.wall_now().unwrap();
        let monotonic = clock.mono_now();
        let _second = clock.wall_now().unwrap();
        assert!(clock.mono_now() >= monotonic);
    }

    #[test]
    fn separate_instances_share_one_monotonic_coordinate() {
        let first = SystemClock::new();
        let started = first.mono_now().as_micros();
        while first.mono_now().as_micros().saturating_sub(started) < 1_000 {
            std::thread::yield_now();
        }
        let before = first.mono_now();
        let second = SystemClock::new();
        assert!(second.mono_now() >= before);
    }
}
