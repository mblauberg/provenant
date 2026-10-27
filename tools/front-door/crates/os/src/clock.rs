//! Production sampling of wall and monotonic time.
use provenant_core::{Clock, MonoTime, TimeError, WallTime};
use std::time::SystemTime;

/// Current boot time as UTC seconds since the Unix epoch, independent of the
/// process environment's time zone.
pub fn boot_unix_seconds() -> std::io::Result<i64> {
    crate::sys::clock::boot_unix_seconds()
}

/// Microseconds on the host's monotonic clock since boot. Every process on the
/// host reads the same coordinate, which no wall-clock step moves and which
/// keeps counting while the host sleeps. Compare two readings only when
/// [`boot_identity`] matched at both.
pub fn host_monotonic_micros() -> std::io::Result<u64> {
    crate::sys::clock::host_monotonic_micros()
}

/// Names the current boot, so a [`host_monotonic_micros`] reading is only
/// compared with one from the same boot.
pub fn boot_identity() -> std::io::Result<String> {
    crate::sys::clock::boot_identity()
}

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

    fn host_monotonic_micros(&self) -> Result<u64, TimeError> {
        host_monotonic_micros().map_err(|_| TimeError::HostClock)
    }

    fn boot_identity(&self) -> Result<String, TimeError> {
        boot_identity().map_err(|_| TimeError::HostClock)
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
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_system_clock_reports_the_host_monotonic_clock_and_boot() {
        let clock = SystemClock::new();
        let first = clock.host_monotonic_micros().unwrap();
        assert!(clock.host_monotonic_micros().unwrap() >= first);
        assert_eq!(clock.boot_identity().unwrap(), boot_identity().unwrap());
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
