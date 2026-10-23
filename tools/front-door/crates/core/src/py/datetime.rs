use jiff::{
    Timestamp,
    civil::{Date, DateTime, ISOWeekDate, Weekday},
};
use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DateTimeError {
    #[error("invalid ISO datetime")]
    InvalidIso,
    #[error("datetime is outside Python's year 1..9999 range")]
    TimeRange,
    #[error("a naive datetime has no UTC timestamp")]
    Naive,
}

/// A Python datetime's wall clock and optional exact UTC offset in microseconds.
/// Unlike Jiff's Offset, Python offsets can include fractional seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythonDateTime {
    pub datetime: DateTime,
    pub offset_microseconds: Option<i64>,
}
impl PythonDateTime {
    pub fn timestamp(self) -> Result<PythonTimestamp, DateTimeError> {
        let offset = self.offset_microseconds.ok_or(DateTimeError::Naive)?;
        let epoch = Date::new(1970, 1, 1)
            .map_err(|_| DateTimeError::TimeRange)?
            .at(0, 0, 0, 0);
        let local = i64::try_from(self.datetime.duration_since(epoch).as_micros())
            .map_err(|_| DateTimeError::TimeRange)?;
        let micros = local.checked_sub(offset).ok_or(DateTimeError::TimeRange)?;
        // astimezone(UTC), like datetime.fromtimestamp, must stay in Python's range.
        timestamp_micros(micros)
    }
}
/// Exact UTC microseconds, retaining Python's full year range (Jiff reserves
/// the final twelve hours of year 9999 for offset conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PythonTimestamp(i64);
impl PythonTimestamp {
    pub fn as_microsecond(self) -> i64 {
        self.0
    }
    pub fn to_jiff(self) -> Result<Timestamp, DateTimeError> {
        Timestamp::from_microsecond(self.0).map_err(|_| DateTimeError::TimeRange)
    }
    /// UTC ISO 8601 with a Z suffix and Python's auto microsecond precision.
    /// Civil arithmetic covers Python's entire range, including the final
    /// twelve hours of year 9999 that Jiff Timestamp deliberately excludes.
    pub fn iso_utc(self) -> Result<String, DateTimeError> {
        let days = self.0.div_euclid(86_400_000_000);
        let time = self.0.rem_euclid(86_400_000_000);
        let date = Date::new(1970, 1, 1)
            .map_err(|_| DateTimeError::TimeRange)?
            .checked_add(jiff::Span::new().days(days))
            .map_err(|_| DateTimeError::TimeRange)?;
        let hour = time / 3_600_000_000;
        let minute = time / 60_000_000 % 60;
        let second = time / 1_000_000 % 60;
        let micros = time % 1_000_000;
        let mut text = format!(
            "{:04}-{:02}-{:02}T{hour:02}:{minute:02}:{second:02}",
            date.year(),
            date.month(),
            date.day()
        );
        if micros != 0 {
            text.push_str(&format!(".{micros:06}"));
        }
        text.push('Z');
        Ok(text)
    }
}
fn timestamp_micros(micros: i64) -> Result<PythonTimestamp, DateTimeError> {
    if !(-62_135_596_800_000_000..253_402_300_800_000_000).contains(&micros) {
        return Err(DateTimeError::TimeRange);
    }
    Ok(PythonTimestamp(micros))
}

/// CPython datetime.fromtimestamp(seconds, UTC): split with modf, then round the
/// fractional part half-even to microseconds. Multiplying seconds by 1e6 first
/// loses precision. Timedelta conversion is a different operation.
pub fn from_timestamp(seconds: f64) -> Result<PythonTimestamp, DateTimeError> {
    if !seconds.is_finite() || seconds < i64::MIN as f64 || seconds >= i64::MAX as f64 {
        return Err(DateTimeError::TimeRange);
    }
    let micros = (seconds.trunc() as i64)
        .checked_mul(1_000_000)
        .and_then(|whole| {
            whole.checked_add((seconds.fract() * 1_000_000.0).round_ties_even() as i64)
        })
        .ok_or(DateTimeError::TimeRange)?;
    timestamp_micros(micros)
}

fn digits(s: &str) -> Option<i16> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}
fn date_prefix(s: &str) -> Option<(Date, usize)> {
    let year = digits(s.get(..4)?)?;
    if year < 1 {
        return None;
    }
    let extended = s.as_bytes().get(4) == Some(&b'-');
    let start = if extended { 5 } else { 4 };
    if s.as_bytes().get(start) == Some(&b'W') {
        let week = digits(s.get(start + 1..start + 3)?)? as i8;
        let mut end = start + 3;
        let day = if extended && s.as_bytes().get(end) == Some(&b'-') {
            end += 2;
            digits(s.get(end - 1..end)?)? as i8
        } else if !extended && s.as_bytes().get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
            digits(s.get(end - 1..end)?)? as i8
        } else {
            1
        };
        let weekday = match day {
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            6 => Weekday::Saturday,
            7 => Weekday::Sunday,
            _ => return None,
        };
        let date = ISOWeekDate::new(year, week, weekday).ok()?.date();
        return (date.year() >= 1 && date.year() <= 9999).then_some((date, end));
    }
    let month = digits(s.get(start..start + 2)?)? as i8;
    let day_start = start + 2 + usize::from(extended);
    if extended && s.as_bytes().get(start + 2) != Some(&b'-') {
        return None;
    }
    let day = digits(s.get(day_start..day_start + 2)?)? as i8;
    Some((Date::new(year, month, day).ok()?, day_start + 2))
}

// Returns hours (which may be 24), minutes, seconds and truncated microseconds.
fn clock(s: &str) -> Option<(i16, i8, i8, i32)> {
    let hour = digits(s.get(..2)?)?;
    if s.len() == 2 {
        return Some((hour, 0, 0, 0));
    }
    let colon = s.as_bytes().get(2) == Some(&b':');
    let minute_start = 2 + usize::from(colon);
    let minute = digits(s.get(minute_start..minute_start + 2)?)? as i8;
    let end = minute_start + 2;
    if s.len() == end {
        return Some((hour, minute, 0, 0));
    }
    if colon && s.as_bytes().get(end) != Some(&b':') {
        return None;
    }
    let second_start = end + usize::from(colon);
    let second = digits(s.get(second_start..second_start + 2)?)? as i8;
    let end = second_start + 2;
    let mut micros = 0;
    if s.len() != end {
        if !matches!(s.as_bytes().get(end), Some(b'.' | b',')) {
            return None;
        }
        let fraction = s.get(end + 1..)?;
        if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        for (i, b) in fraction.bytes().take(6).enumerate() {
            micros += i32::from(b - b'0') * 10i32.pow(5 - i as u32);
        }
    }
    Some((hour, minute, second, micros))
}

/// CPython changed zero-second fractional UTC offsets in a 3.14 patch release.
/// Keep the source runtime's semantics explicit; the default matches CI's 3.14.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoformatSemantics {
    Python3144,
    Python3148,
}

/// Parse CPython ISO date/datetimes, including basic/week dates, any single
/// Unicode separator, comma/long fractions, naive clocks and fractional offsets.
/// Fractions require explicit seconds (CPython 3.14.8); leap seconds are invalid.
pub fn python_fromisoformat(value: &str) -> Result<PythonDateTime, DateTimeError> {
    python_fromisoformat_with(value, IsoformatSemantics::Python3144)
}

pub fn python_fromisoformat_with(
    value: &str,
    semantics: IsoformatSemantics,
) -> Result<PythonDateTime, DateTimeError> {
    parse(value, semantics).ok_or(DateTimeError::InvalidIso)
}
fn parse(value: &str, semantics: IsoformatSemantics) -> Option<PythonDateTime> {
    // CPython disambiguates digit separators in ISO week dates before parsing.
    let chars: Vec<_> = value.chars().collect();
    let n = chars.len();
    if n < 7 {
        return None;
    }
    let separator = if n == 7 {
        7
    } else if chars.get(4) == Some(&'-') {
        if chars.get(5) == Some(&'W') {
            if chars.get(8) == Some(&'-') {
                if n == 9 {
                    return None;
                }
                if chars.get(10).is_some_and(char::is_ascii_digit) {
                    8
                } else {
                    10
                }
            } else {
                8
            }
        } else {
            10
        }
    } else if chars.get(4) == Some(&'W') {
        let mut i = 7;
        while chars.get(i).is_some_and(char::is_ascii_digit) {
            i += 1;
        }
        if i < 9 {
            i
        } else if i % 2 == 0 {
            7
        } else {
            8
        }
    } else {
        8
    };
    let end = value
        .char_indices()
        .nth(separator)
        .map_or(value.len(), |(i, _)| i);
    let (mut date, parsed_end) = date_prefix(value.get(..end)?)?;
    if parsed_end != end {
        return None;
    }
    let mut datetime = date.at(0, 0, 0, 0);
    if end == value.len() {
        return Some(PythonDateTime {
            datetime,
            offset_microseconds: None,
        });
    }
    let tail = value.get(end..)?;
    let time = tail.get(tail.chars().next()?.len_utf8()..)?;
    let offset_start = time.find(['+', '-', 'Z']);
    let (time, offset) = offset_start.map_or((time, None), |i| (&time[..i], Some(&time[i..])));
    let (hour, minute, second, micros) = clock(time)?;
    if !(0..=24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }
    if hour == 24 {
        if minute != 0 || second != 0 || micros != 0 {
            return None;
        }
        date = date.tomorrow().ok()?;
        if date.year() > 9999 {
            return None;
        }
    }
    datetime = date.at((hour % 24) as i8, minute, second, micros * 1000);
    let offset_microseconds = match offset {
        None => None,
        Some("Z") => Some(0),
        Some(offset) => {
            let (h, m, s, us) = clock(offset.get(1..)?)?;
            // CPython normalises offset minutes and seconds, then bounds the total.
            let total = ((i64::from(h) * 60 + i64::from(m)) * 60 + i64::from(s)) * 1_000_000
                + i64::from(us);
            if total >= 86_400_000_000 {
                return None;
            }
            let total = if semantics == IsoformatSemantics::Python3144 && h == 0 && m == 0 && s == 0
            {
                0
            } else {
                total
            };
            Some(if offset.starts_with('-') {
                -total
            } else {
                total
            })
        }
    };
    Some(PythonDateTime {
        datetime,
        offset_microseconds,
    })
}

/// Aware-only adapter used by provider/config policy. Invalid or naive inputs
/// have no UTC timestamp; use python_fromisoformat to preserve naive datetimes.
pub fn python_iso_timestamp(value: &str) -> Option<PythonTimestamp> {
    python_fromisoformat(value).ok()?.timestamp().ok()
}

#[cfg(test)]
mod form_tests {
    use super::*;
    #[test]
    fn documented_iso_forms_retain_wall_clock_fraction_and_offset() {
        for (text, hour, minute, micros, offset) in [
            ("2026-10-23", 0, 0, 0, None),
            ("20261023", 0, 0, 0, None),
            ("2026-W43-5", 0, 0, 0, None),
            ("2026W435", 0, 0, 0, None),
            ("2026-10-23T12", 12, 0, 0, None),
            ("2026-10-23 12:30", 12, 30, 0, None),
            ("20261023T1230", 12, 30, 0, None),
            ("20261023T123000", 12, 30, 0, None),
            ("2026-10-23💩12:30:00", 12, 30, 0, None),
            ("2026-10-23T12:30:00.1", 12, 30, 100_000, None),
            ("2026-10-23T12:30:00,123456789", 12, 30, 123_456, None),
            ("2026-10-23T12Z", 12, 0, 0, Some(0)),
            ("2026-10-23T12+01", 12, 0, 0, Some(3_600_000_000)),
            ("2026-10-23T12-0130", 12, 0, 0, Some(-5_400_000_000)),
            ("2026-10-23T12+01:30", 12, 0, 0, Some(5_400_000_000)),
            ("2026-10-23T12+013000", 12, 0, 0, Some(5_400_000_000)),
            (
                "2026-10-23T12-01:02:03.123456789",
                12,
                0,
                0,
                Some(-3_723_123_456),
            ),
        ] {
            let value = python_fromisoformat(text).unwrap();
            assert_eq!(
                (
                    value.datetime.year(),
                    value.datetime.month(),
                    value.datetime.day()
                ),
                (2026, 10, 23),
                "{text}"
            );
            assert_eq!(
                (
                    value.datetime.hour(),
                    value.datetime.minute(),
                    value.datetime.second(),
                    value.datetime.subsec_nanosecond() / 1000
                ),
                (hour, minute, 0, micros),
                "{text}"
            );
            assert_eq!(value.offset_microseconds, offset, "{text}");
        }
        for text in [
            "2026-10",
            "2026-296",
            "2026-10-23T12.5",
            "2026-10-23T12:30.5",
            "2026-10-23T12:30:60",
            "2026-10-23T12+24:00",
        ] {
            assert_eq!(
                python_fromisoformat(text),
                Err(DateTimeError::InvalidIso),
                "{text}"
            );
        }
    }
}

#[cfg(test)]
mod utc_tests {
    use super::*;
    #[test]
    fn shared_utc_parse_and_format_match_source_telemetry() {
        let inputs = serde_json::json!([
            "2026-10-23",
            "20261023T123000",
            "2026-W43-5T12:30:00Z",
            "2026-10-23 12:30:00.123456789-01:30",
            "0001-01-01T00:00:00Z",
            "9999-12-31T23:59:59.999999Z",
            "1969-12-31T23:59:59.999999Z",
            "2026-10-23Z12",
            "bad"
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.json");
        std::fs::write(&path, serde_json::to_vec(&inputs).unwrap()).unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/telemetry.py",
            r#"
out=[]
for s in json.loads(pathlib.Path(sys.argv[3]).read_text()):
    try: out.append(m.iso(m.since_time(s)))
    except (ValueError,OverflowError): out.append(None)
print(json.dumps(out))
"#,
            &[&path],
        ) else {
            return;
        };
        let actual: Vec<_> = inputs
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                parse_time(v.as_str().unwrap())
                    .and_then(PythonTimestamp::iso_utc)
                    .ok()
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}

/// Source telemetry `since_time`: Python fromisoformat after replacing `Z`,
/// treating a naive clock as UTC. Other policies should keep using the parser
/// directly so their naive-clock policy stays explicit.
pub fn parse_time(value: &str) -> Result<PythonTimestamp, DateTimeError> {
    let mut parsed = python_fromisoformat(&value.replace('Z', "+00:00"))?;
    parsed.offset_microseconds.get_or_insert(0);
    parsed.timestamp()
}

#[cfg(test)]
mod jiff_iso_tests {
    use super::*;
    #[test]
    fn jiff_iso_adapter_preserves_negative_submicrosecond_floor_and_python_range() {
        let nanos = [
            -62_135_596_800_000_000_001_i128,
            -62_135_596_800_000_000_000,
            -1001,
            -1000,
            -1,
            0,
            999,
            1000,
            1_700_000_000_123_456_789,
        ];
        let inputs: Vec<_> = nanos.iter().map(ToString::to_string).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.json");
        std::fs::write(&path, serde_json::to_vec(&inputs).unwrap()).unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/telemetry.py",
            r#"
from datetime import datetime, timedelta, UTC
out=[]
for s in json.loads(pathlib.Path(sys.argv[3]).read_text()):
    try: out.append(m.iso(datetime(1970,1,1,tzinfo=UTC)+timedelta(microseconds=int(s)//1000)))
    except OverflowError: out.append(None)
print(json.dumps(out))
"#,
            &[&path],
        ) else {
            return;
        };
        let actual: Vec<_> = nanos
            .iter()
            .map(|&n| iso(Timestamp::from_nanosecond(n).unwrap()).ok())
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}

/// Exact adapter for telemetry's Jiff timestamps. Python clocks have microsecond
/// precision; discard lower nanoseconds within the civil second, including
/// before the epoch. This preserves the source's datetime ISO spelling.
pub fn iso(value: Timestamp) -> Result<String, DateTimeError> {
    let micros = i64::try_from(value.as_nanosecond().div_euclid(1000))
        .map_err(|_| DateTimeError::TimeRange)?;
    timestamp_micros(micros)?.iso_utc()
}
