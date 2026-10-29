use serde::{Deserialize, Serialize};
use std::sync::Mutex;
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
    #[error("the host monotonic clock or boot identity cannot be read")]
    HostClock,
}

/// A UTC timestamp of exact microsecond precision. No system clock is read.
/// Uses jiff's timestamp range, narrowed to RFC3339's nonnegative four-digit
/// years: 0000-01-01T00:00:00.000000Z through jiff::Timestamp::MAX
/// rounded down to microseconds (9999-12-30T22:00:00.999999Z).
/// Jiff enforces the upper bound; construction rejects its signed-year range.
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
    pub fn as_timestamp(self) -> jiff::Timestamp {
        self.0
    }
    pub fn from_unix_seconds(seconds: i64) -> Result<Self, TimeError> {
        Self::from_micros(seconds.checked_mul(1_000_000).ok_or(TimeError::WallRange)?)
    }
    /// Whole seconds since the Unix epoch, truncated toward zero.
    pub fn unix_seconds(self) -> i64 {
        self.0.as_second()
    }
    /// Whole milliseconds since the Unix epoch, truncated toward zero.
    pub fn unix_millis(self) -> i64 {
        self.0.as_millisecond()
    }
    pub fn parse(text: &str) -> Result<Self, TimeError> {
        let timestamp: jiff::Timestamp = text.parse().map_err(|_| TimeError::WallSpelling)?;
        if timestamp.subsec_nanosecond() % 1000 != 0 {
            return Err(TimeError::WallRange);
        }
        let value = Self::from_micros(timestamp.as_microsecond())?;
        if value.to_string() != text {
            return Err(TimeError::WallSpelling);
        }
        Ok(value)
    }
    /// Any RFC 3339 spelling of an instant, in any offset,
    /// for hand-written configuration. Fractions truncate towards zero to microseconds.
    /// [`Self::parse`] takes only the
    /// canonical spelling that [`Display`](std::fmt::Display) writes.
    pub fn parse_rfc3339(text: &str) -> Result<Self, TimeError> {
        // RFC 3339 allows arbitrarily many fractional digits; jiff accepts nine.
        let normalised = text.find('.').and_then(|dot| {
            let digits = text[dot + 1..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            (digits > 9).then(|| format!("{}{}", &text[..dot + 10], &text[dot + 1 + digits..]))
        });
        let text = normalised.as_deref().unwrap_or(text);
        let timestamp: jiff::Timestamp = text.parse().map_err(|_| TimeError::WallSpelling)?;
        Self::from_micros(timestamp.as_microsecond())
    }
    pub fn checked_add(self, duration: DurationMicros) -> Result<Self, TimeError> {
        let sum = i128::from(self.as_micros()) + i128::from(duration.0);
        Self::from_micros(i64::try_from(sum).map_err(|_| TimeError::Overflow)?)
    }
}

/// Serde for a configured instant, such as an expiry: reads any RFC 3339
/// spelling and writes the canonical one. Use with
/// `#[serde(with = "provenant_core::rfc3339")]`, or `rfc3339::option` for an
/// optional field.
pub mod rfc3339 {
    use super::WallTime;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &WallTime, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<WallTime, D::Error> {
        let text = String::deserialize(deserializer)?;
        WallTime::parse_rfc3339(&text).map_err(|_| {
            D::Error::custom(format_args!("expected an RFC 3339 timestamp, not {text:?}"))
        })
    }

    pub mod option {
        use super::WallTime;
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            value: &Option<WallTime>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                Some(value) => serializer.collect_str(value),
                None => serializer.serialize_none(),
            }
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<WallTime>, D::Error> {
            #[derive(Deserialize)]
            struct Wrapped(#[serde(with = "super")] WallTime);
            Ok(Option::<Wrapped>::deserialize(deserializer)?.map(|Wrapped(value)| value))
        }
    }
}
impl std::fmt::Display for UtcMicros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.6}", self.0)
    }
}
crate::ids::text_wire!(UtcMicros, TimeError);

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
}

/// Process-local deadline; never persisted as a cross-process timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline(MonoTime);
impl Deadline {
    pub const fn as_mono_time(self) -> MonoTime {
        self.0
    }
    pub fn is_expired(self, now: MonoTime) -> bool {
        now >= self.0
    }
}

/// The longest a lane, provider deadline or CLI wait may run, in seconds.
pub const MAX_LANE_RUNTIME_SECONDS: u64 = 14_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitTransport {
    Cli,
    Mcp,
}
impl WaitTransport {
    pub const fn maximum_seconds(self) -> u64 {
        match self {
            Self::Cli => MAX_LANE_RUNTIME_SECONDS,
            Self::Mcp => 55,
        }
    }
}
/// Explicit bounded wait. Expiration never requests a provider stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitBudget {
    deadline: Deadline,
    nonblocking: bool,
}
impl WaitBudget {
    /// One immediate attempt, with no retry wait. Used for writer probes while
    /// holding repository metadata exclusion; never wait on a later lock.
    pub const fn nonblocking(now: MonoTime) -> Self {
        Self {
            deadline: Deadline(now),
            nonblocking: true,
        }
    }
    /// Explicitly allow one immediate attempt, even though the deadline is now.
    /// An ordinary budget that expires does not become a nonblocking probe.
    pub const fn is_nonblocking(self) -> bool {
        self.nonblocking
    }
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
            deadline: Deadline(now.checked_add(duration)?),
            nonblocking: false,
        })
    }
    /// A wait of up to `duration`, shortened to the CLI maximum. Unlike
    /// [`Self::new`] it accepts sub-second waits, for internal lock bounds
    /// rather than requested waits.
    pub fn up_to(now: MonoTime, duration: DurationMicros) -> Self {
        let maximum = DurationMicros(MAX_LANE_RUNTIME_SECONDS * 1_000_000);
        Self {
            deadline: Deadline(
                now.checked_add(DurationMicros(duration.0.min(maximum.0)))
                    .unwrap_or(MonoTime(u64::MAX)),
            ),
            nonblocking: false,
        }
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
    /// Shorten a validated budget to an external deadline, including subsecond
    /// or expired bounds. This never turns expiry into an immediate probe.
    pub fn cap_at(self, deadline: MonoTime) -> Self {
        Self {
            deadline: Deadline(self.deadline.0.min(deadline)),
            ..self
        }
    }

    pub const fn deadline(self) -> Deadline {
        self.deadline
    }
    pub fn remaining(self, now: MonoTime) -> DurationMicros {
        DurationMicros(self.deadline.0.0.saturating_sub(now.0))
    }
}

/// Sampled by the edge; state machines receive values, never ambient time.
pub trait Clock {
    fn wall_now(&self) -> Result<WallTime, TimeError>;
    /// Whole Unix seconds now, for second-granular records; zero when the
    /// wall clock is outside the supported range.
    fn unix_seconds(&self) -> i64 {
        self.wall_now().map_or(0, WallTime::unix_seconds)
    }
    fn mono_now(&self) -> MonoTime;
    /// Wait until a monotonic deadline according to this clock's time source.
    fn wait_until(&self, deadline: MonoTime) -> Result<(), TimeError>;
    /// Microseconds on the host's monotonic clock since boot: one coordinate
    /// every process on the host shares, unlike `mono_now`, and one no
    /// wall-clock step moves. Only comparable with another reading taken under
    /// the same `boot_identity`. A clock that cannot say refuses.
    fn host_monotonic_micros(&self) -> Result<u64, TimeError> {
        Err(TimeError::HostClock)
    }
    /// Names the current boot. A clock that cannot say refuses.
    fn boot_identity(&self) -> Result<String, TimeError> {
        Err(TimeError::HostClock)
    }
}
/// Explicit, locally-owned clock for deterministic simulations and tests.
#[derive(Debug)]
pub struct TestClock {
    times: Mutex<(WallTime, MonoTime)>,
}
impl Clone for TestClock {
    fn clone(&self) -> Self {
        let times = *self
            .times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self {
            times: Mutex::new(times),
        }
    }
}
impl TestClock {
    pub const fn new(wall: WallTime, mono: MonoTime) -> Self {
        Self {
            times: Mutex::new((wall, mono)),
        }
    }
    pub fn advance(&mut self, duration: DurationMicros) -> Result<(), TimeError> {
        let mut times = self
            .times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *times = Self::shifted(*times, duration)?;
        Ok(())
    }
    /// Both sums are checked before either field changes.
    fn shifted(
        (wall, mono): (WallTime, MonoTime),
        duration: DurationMicros,
    ) -> Result<(WallTime, MonoTime), TimeError> {
        Ok((wall.checked_add(duration)?, mono.checked_add(duration)?))
    }
}
impl Clock for TestClock {
    fn wall_now(&self) -> Result<WallTime, TimeError> {
        Ok(self
            .times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0)
    }
    fn mono_now(&self) -> MonoTime {
        self.times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
    }
    fn wait_until(&self, deadline: MonoTime) -> Result<(), TimeError> {
        let mut times = self
            .times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if deadline > times.1 {
            let duration = DurationMicros::from_micros(deadline.as_micros() - times.1.as_micros());
            *times = Self::shifted(*times, duration)?;
        }
        Ok(())
    }
    /// The one simulated monotonic coordinate stands for the host's.
    fn host_monotonic_micros(&self) -> Result<u64, TimeError> {
        Ok(self.mono_now().as_micros())
    }
    fn boot_identity(&self) -> Result<String, TimeError> {
        Ok("test-boot".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_deadline_caps_subsecond_waits_without_reopening_a_probe() {
        let now = MonoTime::from_micros(1_000_000);
        let later = MonoTime::from_micros(1_250_000);
        let budget = WaitBudget::mcp(now, 5).unwrap().cap_at(later);
        assert_eq!(budget.remaining(now).as_micros(), 250_000);
        assert_eq!(budget.remaining(later).as_micros(), 0);
        assert!(!budget.is_nonblocking());
        assert_eq!(budget.cap_at(now).remaining(now).as_micros(), 0);
        assert_eq!(
            budget
                .cap_at(MonoTime::from_micros(9_000_000))
                .deadline()
                .as_mono_time(),
            later
        );
    }
    #[test]
    fn host_time_refuses_unless_a_clock_provides_it() {
        struct Plain;
        impl Clock for Plain {
            fn wall_now(&self) -> Result<WallTime, TimeError> {
                WallTime::from_micros(0)
            }
            fn mono_now(&self) -> MonoTime {
                MonoTime::from_micros(0)
            }
            fn wait_until(&self, _: MonoTime) -> Result<(), TimeError> {
                Ok(())
            }
        }
        assert!(matches!(
            Plain.host_monotonic_micros(),
            Err(TimeError::HostClock)
        ));
        assert!(matches!(Plain.boot_identity(), Err(TimeError::HostClock)));
        let test = TestClock::new(WallTime::from_micros(0).unwrap(), MonoTime::from_micros(7));
        assert_eq!(test.host_monotonic_micros().unwrap(), 7);
        assert_eq!(Plain.unix_seconds(), 0);
        let mut later = test.clone();
        later
            .advance(DurationMicros::from_seconds(90).unwrap())
            .unwrap();
        assert_eq!(later.unix_seconds(), 90);
        assert_eq!(test.boot_identity().unwrap(), "test-boot");
    }
    #[test]
    fn an_internal_bound_accepts_sub_second_waits_and_caps_long_ones() {
        let start = MonoTime::from_micros(0);
        let short = WaitBudget::up_to(start, DurationMicros::from_micros(50_000));
        assert_eq!(short.remaining(start).as_micros(), 50_000);
        assert!(!short.is_nonblocking());
        let long = WaitBudget::up_to(start, DurationMicros::from_micros(u64::MAX));
        assert_eq!(
            long.remaining(start).as_micros(),
            MAX_LANE_RUNTIME_SECONDS * 1_000_000
        );
    }
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
            WaitBudget::cli(now, MAX_LANE_RUNTIME_SECONDS)
                .unwrap()
                .remaining(now)
                .as_micros(),
            MAX_LANE_RUNTIME_SECONDS * 1_000_000
        );
        assert!(WaitBudget::cli(now, MAX_LANE_RUNTIME_SECONDS + 1).is_err());
        assert!(WaitBudget::mcp(now, 55).is_ok());
        assert!(WaitBudget::mcp(now, 56).is_err());
        assert!(
            WaitBudget::new(
                now,
                DurationMicros::from_micros(55_000_001),
                WaitTransport::Mcp
            )
            .is_err()
        );
        assert!(WaitBudget::cli(MonoTime::from_micros(u64::MAX), 1).is_err());
        assert!(DurationMicros::from_seconds(u64::MAX).is_err());
        assert!(WaitBudget::cli(now, 0).is_err());
        assert!(
            WaitBudget::new(
                now,
                DurationMicros::from_micros(999_999),
                WaitTransport::Mcp
            )
            .is_err()
        );
        let wait = WaitBudget::mcp(now, 1).unwrap();
        assert!(wait.deadline().is_expired(MonoTime::from_micros(1_000_010)));
        assert_eq!(
            wait.remaining(MonoTime::from_micros(1_000_011)),
            DurationMicros::ZERO
        );
    }
    #[test]
    fn configured_instants_read_any_rfc3339_spelling_and_write_one() {
        let canonical = UtcMicros::from_unix_seconds(1_793_491_200).unwrap();
        for input in [
            "2026-11-01T00:00:00Z",
            "2026-11-01T00:00:00.000000Z",
            "2026-11-01T10:00:00+10:00",
            "2026-11-01T00:00:00.000000001Z",
            "2026-11-01T00:00:00.00000000123456789Z",
        ] {
            assert_eq!(UtcMicros::parse_rfc3339(input).unwrap(), canonical);
        }
        for input in ["2026-11-01", "1793491200"] {
            assert!(UtcMicros::parse_rfc3339(input).is_err(), "{input}");
        }
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Configured {
            #[serde(with = "rfc3339")]
            at: WallTime,
            #[serde(default, with = "rfc3339::option")]
            expires: Option<WallTime>,
        }
        let read: Configured =
            serde_json::from_str(r#"{"at":"2026-11-01T10:00:00+10:00","expires":null}"#).unwrap();
        assert_eq!(
            read,
            Configured {
                at: canonical,
                expires: None
            }
        );
        let read: Configured = serde_json::from_str(r#"{"at":"2026-11-01T00:00:00Z"}"#).unwrap();
        assert_eq!(read.expires, None);
        let fractional: Configured = serde_json::from_str(r#"{"at":"2026-11-01T00:00:00.123456789Z","expires":"2026-11-01T10:00:00.000000001+10:00"}"#).unwrap();
        assert_eq!(fractional.at.as_micros(), canonical.as_micros() + 123_456);
        assert_eq!(fractional.expires, Some(canonical));
        let written = Configured {
            at: canonical,
            expires: Some(canonical),
        };
        assert_eq!(
            serde_json::to_string(&written).unwrap(),
            r#"{"at":"2026-11-01T00:00:00.000000Z","expires":"2026-11-01T00:00:00.000000Z"}"#
        );
        assert!(serde_json::from_str::<Configured>(r#"{"at":1793491200}"#).is_err());
        let error = serde_json::from_str::<Configured>(r#"{"at":"next year"}"#).unwrap_err();
        assert!(error.to_string().contains("RFC 3339"), "{error}");
    }
    #[test]
    fn wall_time_converts_whole_unix_seconds_and_millis() {
        let at = UtcMicros::from_unix_seconds(1_700_000_000).unwrap();
        assert_eq!(at.unix_seconds(), 1_700_000_000);
        assert_eq!(at.unix_millis(), 1_700_000_000_000);
        let later = at.checked_add(DurationMicros::from_micros(1_999)).unwrap();
        assert_eq!(later.unix_seconds(), 1_700_000_000);
        assert_eq!(later.unix_millis(), 1_700_000_000_001);
        assert!(UtcMicros::from_unix_seconds(i64::MAX).is_err());
        assert!(UtcMicros::from_unix_seconds(-62_167_219_201).is_err());
    }
    #[test]
    fn test_clock_advance_is_atomic_on_overflow() {
        let start = UtcMicros::from_micros(0).unwrap();
        let mut clock = TestClock::new(start, MonoTime::from_micros(u64::MAX));
        assert!(clock.advance(DurationMicros::from_micros(1)).is_err());
        assert_eq!(clock.wall_now().unwrap(), start);
        assert_eq!(clock.mono_now(), MonoTime::from_micros(u64::MAX));
    }

    #[test]
    fn test_clock_wait_advances_virtual_time_to_deadline() {
        let start = UtcMicros::from_micros(0).unwrap();
        let clock = TestClock::new(start, MonoTime::from_micros(10));
        clock.wait_until(MonoTime::from_micros(25)).unwrap();
        assert_eq!(clock.mono_now(), MonoTime::from_micros(25));
        assert_eq!(clock.wall_now().unwrap().as_micros(), 15);
        clock.wait_until(MonoTime::from_micros(20)).unwrap();
        assert_eq!(clock.mono_now(), MonoTime::from_micros(25));
    }
}
