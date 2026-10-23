use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TimeError {
    #[error("time arithmetic overflow or backwards monotonic time")]
    Overflow,
    #[error("wall time is outside the supported RFC3339 microsecond range")]
    WallRange,
    #[error("wall time must use canonical RFC3339 UTC with six fractional digits")]
    WallSpelling,
    #[error("wait budget must be between 1 and {maximum_seconds} seconds")]
    WaitLimit { maximum_seconds: u64 },
}

/// A UTC timestamp of exact microsecond precision. No system clock is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UtcMicros(jiff::Timestamp);
pub type WallTime = UtcMicros;
impl UtcMicros {
    pub fn from_micros(micros: i64) -> Result<Self, TimeError> {
        // RFC3339 has a four-digit nonnegative year. Jiff also supports earlier
        // signed years, which are outside this wire contract.
        if micros < -62_167_219_200_000_000 {
            return Err(TimeError::WallRange);
        }
        jiff::Timestamp::from_microsecond(micros)
            .map(Self)
            .map_err(|_| TimeError::WallRange)
    }
    pub fn as_micros(self) -> i64 {
        self.0.as_microsecond()
    }
    pub fn from_timestamp(timestamp: jiff::Timestamp) -> Result<Self, TimeError> {
        if timestamp.subsec_nanosecond() % 1000 != 0 {
            return Err(TimeError::WallRange);
        }
        Self::from_micros(timestamp.as_microsecond())
    }
    pub fn as_timestamp(self) -> jiff::Timestamp {
        self.0
    }
    pub fn parse(text: &str) -> Result<Self, TimeError> {
        let timestamp: jiff::Timestamp = text.parse().map_err(|_| TimeError::WallSpelling)?;
        let value = Self::from_timestamp(timestamp)?;
        if value.to_string() != text {
            return Err(TimeError::WallSpelling);
        }
        Ok(value)
    }
    pub fn checked_add(self, duration: DurationMicros) -> Result<Self, TimeError> {
        let sum = i128::from(self.as_micros()) + i128::from(duration.0);
        Self::from_micros(i64::try_from(sum).map_err(|_| TimeError::Overflow)?)
    }
}
impl std::fmt::Display for UtcMicros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.6}", self.0)
    }
}
impl TryFrom<String> for UtcMicros {
    type Error = TimeError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}
impl From<UtcMicros> for String {
    fn from(time: UtcMicros) -> Self {
        time.to_string()
    }
}

/// Nonnegative duration, never a floating point number of seconds.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DurationMicros(u64);
impl DurationMicros {
    pub const ZERO: Self = Self(0);
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }
    pub fn from_seconds(seconds: u64) -> Result<Self, TimeError> {
        seconds
            .checked_mul(1_000_000)
            .map(Self)
            .ok_or(TimeError::Overflow)
    }
    pub const fn as_micros(self) -> u64 {
        self.0
    }
    pub fn checked_add(self, other: Self) -> Result<Self, TimeError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(TimeError::Overflow)
    }
}

/// Process-local monotonic sample. Deliberately has no serde representation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoTime(u64);
impl MonoTime {
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }
    pub const fn as_micros(self) -> u64 {
        self.0
    }
    pub fn checked_add(self, duration: DurationMicros) -> Result<Self, TimeError> {
        self.0
            .checked_add(duration.0)
            .map(Self)
            .ok_or(TimeError::Overflow)
    }
    pub fn checked_duration_since(self, earlier: Self) -> Result<DurationMicros, TimeError> {
        self.0
            .checked_sub(earlier.0)
            .map(DurationMicros)
            .ok_or(TimeError::Overflow)
    }
}

/// Process-local deadline; never persisted as a cross-process timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline(MonoTime);
impl Deadline {
    pub const fn at(time: MonoTime) -> Self {
        Self(time)
    }
    pub fn after(now: MonoTime, duration: DurationMicros) -> Result<Self, TimeError> {
        now.checked_add(duration).map(Self)
    }
    pub const fn as_mono_time(self) -> MonoTime {
        self.0
    }
    pub fn is_expired(self, now: MonoTime) -> bool {
        now >= self.0
    }
    pub fn remaining(self, now: MonoTime) -> DurationMicros {
        DurationMicros(self.0 .0.saturating_sub(now.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitTransport {
    Cli,
    Mcp,
}
impl WaitTransport {
    pub const fn maximum_seconds(self) -> u64 {
        match self {
            Self::Cli => 14_400,
            Self::Mcp => 55,
        }
    }
}
/// Explicit bounded wait. Expiration never requests a provider stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitBudget {
    deadline: Deadline,
}
impl WaitBudget {
    pub fn new(
        now: MonoTime,
        duration: DurationMicros,
        transport: WaitTransport,
    ) -> Result<Self, TimeError> {
        let maximum_seconds = transport.maximum_seconds();
        if duration.0 < 1_000_000 || duration.0 > maximum_seconds * 1_000_000 {
            return Err(TimeError::WaitLimit { maximum_seconds });
        }
        Ok(Self {
            deadline: Deadline::after(now, duration)?,
        })
    }
    pub fn cli(now: MonoTime, seconds: u64) -> Result<Self, TimeError> {
        Self::new(
            now,
            DurationMicros::from_seconds(seconds)?,
            WaitTransport::Cli,
        )
    }
    pub fn mcp(now: MonoTime, seconds: u64) -> Result<Self, TimeError> {
        Self::new(
            now,
            DurationMicros::from_seconds(seconds)?,
            WaitTransport::Mcp,
        )
    }
    pub const fn deadline(self) -> Deadline {
        self.deadline
    }
    pub fn remaining(self, now: MonoTime) -> DurationMicros {
        self.deadline.remaining(now)
    }
}

/// Sampled by the edge; state machines receive values, never ambient time.
pub trait Clock {
    fn wall_now(&self) -> Result<WallTime, TimeError>;
    fn mono_now(&self) -> MonoTime;
}
/// Explicit, locally-owned clock for deterministic simulations and tests.
#[derive(Debug, Clone)]
pub struct TestClock {
    wall: WallTime,
    mono: MonoTime,
}
impl TestClock {
    pub const fn new(wall: WallTime, mono: MonoTime) -> Self {
        Self { wall, mono }
    }
    pub fn advance(&mut self, duration: DurationMicros) -> Result<(), TimeError> {
        let wall = self.wall.checked_add(duration)?;
        let mono = self.mono.checked_add(duration)?;
        self.wall = wall;
        self.mono = mono;
        Ok(())
    }
    pub fn set_wall(&mut self, wall: WallTime) {
        self.wall = wall;
    }
}
impl Clock for TestClock {
    fn wall_now(&self) -> Result<WallTime, TimeError> {
        Ok(self.wall)
    }
    fn mono_now(&self) -> MonoTime {
        self.mono
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wall_time_is_exact_and_has_only_one_wire_spelling() {
        let before_epoch = UtcMicros::from_micros(-1).unwrap();
        assert_eq!(before_epoch.to_string(), "1969-12-31T23:59:59.999999Z");
        assert_eq!(
            UtcMicros::parse(&before_epoch.to_string()).unwrap(),
            before_epoch
        );
        assert_eq!(
            serde_json::to_string(&before_epoch).unwrap(),
            "\"1969-12-31T23:59:59.999999Z\""
        );
        for input in [
            "1970-01-01T00:00:00Z",
            "1970-01-01T00:00:00.000000+00:00",
            "1970-01-01T00:00:00.000000001Z",
            "NaN",
        ] {
            assert!(UtcMicros::parse(input).is_err());
        }
        assert!(UtcMicros::from_micros(i64::MAX).is_err());
    }
    #[test]
    fn budgets_and_deadlines_check_caps_overflow_and_expiry() {
        let now = MonoTime::from_micros(10);
        assert_eq!(
            WaitBudget::cli(now, 14_400)
                .unwrap()
                .remaining(now)
                .as_micros(),
            14_400_000_000
        );
        assert!(WaitBudget::cli(now, 14_401).is_err());
        assert!(WaitBudget::mcp(now, 55).is_ok());
        assert!(WaitBudget::mcp(now, 56).is_err());
        assert!(WaitBudget::new(
            now,
            DurationMicros::from_micros(55_000_001),
            WaitTransport::Mcp
        )
        .is_err());
        assert!(WaitBudget::cli(MonoTime::from_micros(u64::MAX), 1).is_err());
        assert!(DurationMicros::from_seconds(u64::MAX).is_err());
        assert!(DurationMicros::from_micros(u64::MAX)
            .checked_add(DurationMicros::from_micros(1))
            .is_err());
        assert!(now
            .checked_duration_since(MonoTime::from_micros(11))
            .is_err());
        assert!(WaitBudget::cli(now, 0).is_err());
        assert!(WaitBudget::mcp(now, 0).is_err());
        assert!(WaitBudget::new(
            now,
            DurationMicros::from_micros(999_999),
            WaitTransport::Mcp
        )
        .is_err());
        let wait = WaitBudget::mcp(now, 1).unwrap();
        assert!(wait.deadline().is_expired(MonoTime::from_micros(1_000_010)));
        assert_eq!(
            wait.remaining(MonoTime::from_micros(1_000_011)),
            DurationMicros::ZERO
        );
    }
    #[test]
    fn test_clock_advances_atomically_and_wall_adjustments_do_not_extend_waits() {
        let start = UtcMicros::from_micros(0).unwrap();
        let mut clock = TestClock::new(start, MonoTime::from_micros(u64::MAX));
        assert!(clock.advance(DurationMicros::from_micros(1)).is_err());
        assert_eq!(clock.wall_now().unwrap(), start);
        let mut clock = TestClock::new(start, MonoTime::from_micros(0));
        let wait = WaitBudget::mcp(clock.mono_now(), 1).unwrap();
        clock
            .advance(DurationMicros::from_seconds(1).unwrap())
            .unwrap();
        clock.set_wall(start);
        assert!(wait.deadline().is_expired(clock.mono_now()));
    }
}
