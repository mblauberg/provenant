//! Production sampling of wall and monotonic time.
use provenant_core::{Clock, MonoTime, TimeError, WallTime};
use std::time::{Instant, SystemTime};

/// Samples UTC wall time through Jiff and elapsed time from a local monotonic origin.
#[derive(Debug, Clone)]
pub struct SystemClock {
    origin: Instant,
}
impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}
impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}
impl Clock for SystemClock {
    fn wall_now(&self) -> Result<WallTime, TimeError> {
        let timestamp =
            jiff::Timestamp::try_from(SystemTime::now()).map_err(|_| TimeError::WallRange)?;
        WallTime::from_micros(timestamp.as_microsecond())
    }

    fn mono_now(&self) -> MonoTime {
        let elapsed = self.origin.elapsed().as_micros();
        let micros = match u64::try_from(elapsed) {
            Ok(value) => value,
            Err(_) => u64::MAX,
        };
        MonoTime::from_micros(micros)
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
}
