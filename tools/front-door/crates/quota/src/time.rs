//! Python-compatible numeric and timestamp coercion at the quota boundary.
pub use crate::pending::from_timestamp;
use crate::pending::{decimal_digit, python_strip};
use jiff::{
    Timestamp,
    civil::{Date, ISOWeekDate, Time},
};
use serde_json::Value;

pub fn number(value: &Value) -> Option<f64> {
    let n = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let mapped = s
                .chars()
                .map(|c| decimal_digit(c).map(|d| char::from(b'0' + d)).unwrap_or(c))
                .collect::<String>();
            let text = mapped.trim();
            let bytes = text.as_bytes();
            if bytes.iter().enumerate().any(|(i, b)| {
                *b == b'_'
                    && (i == 0
                        || i + 1 == bytes.len()
                        || !bytes[i - 1].is_ascii_digit()
                        || !bytes[i + 1].is_ascii_digit())
            }) {
                return None;
            }
            text.replace('_', "").parse().ok()
        }
        _ => None,
    }?;
    n.is_finite().then_some(n)
}
pub fn parse_time(value: &Value) -> Option<Timestamp> {
    if let Some(mut seconds) = number(value) {
        if seconds > 10_000_000_000.0 {
            seconds /= 1000.0;
        }
        if seconds <= 0.0 || seconds >= 253402300800.0 {
            return None;
        }
        return from_timestamp(seconds).ok();
    }
    python_iso(python_strip(value.as_str()?))
}
// datetime.fromisoformat accepts ISO week dates, compact times, arbitrary single
// separators and second-resolution UTC offsets. Jiff's Temporal parser differs,
// so validate this boundary before using its calendar arithmetic.
fn python_iso(text: &str) -> Option<Timestamp> {
    let bytes = text.as_bytes();
    let year_text = text.get(..4)?;
    if !year_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year = year_text.parse::<i16>().ok()?;
    if year < 1 {
        return None;
    }
    let extended = bytes.get(4) == Some(&b'-');
    let week = bytes.get(if extended { 5 } else { 4 }) == Some(&b'W');
    let length = if week {
        if extended {
            if bytes.get(8) == Some(&b'-') { 10 } else { 8 }
        } else if bytes.get(7).is_some_and(u8::is_ascii_digit) {
            8
        } else {
            7
        }
    } else if extended {
        10
    } else {
        8
    };
    let date_text = text.get(..length)?;
    let date = if week {
        let date_text = if length == 7 || length == 8 && extended {
            format!("{date_text}{}1", if extended { "-" } else { "" })
        } else {
            date_text.into()
        };
        date_text.parse::<ISOWeekDate>().ok()?.date()
    } else {
        let month = date_text.get(if extended { 5..7 } else { 4..6 })?;
        let day = date_text.get(if extended { 8..10 } else { 6..8 })?;
        if !month.bytes().chain(day.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let month = month.parse().ok()?;
        let day = day.parse().ok()?;
        if extended && (bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-')) {
            return None;
        }
        Date::new(year, month, day).ok()?
    };
    let rest = text.get(length..)?;
    if rest.is_empty() {
        return date
            .at(0, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp());
    }
    let separator = rest.chars().next()?;
    let time = rest.get(separator.len_utf8()..)?;
    let tz_start = time.find(['+', '-', 'Z']);
    let (clock, offset) = if let Some(i) = tz_start {
        (&time[..i], &time[i..])
    } else {
        (time, "")
    };
    let (hour, minute, second, micro) = clock_parts(clock)?;
    let local = date
        .to_datetime(
            Time::new(
                hour.try_into().ok()?,
                minute.try_into().ok()?,
                second.try_into().ok()?,
                micro * 1000,
            )
            .ok()?,
        )
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp();
    if offset.is_empty() || offset == "Z" {
        return Some(local);
    }
    let sign = match offset.as_bytes().first()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let (h, m, s, micro) = clock_parts(offset.get(1..)?)?;
    let whole = h as i64 * 3600 + m as i64 * 60 + s as i64;
    // CPython treats a zero whole-second offset as UTC, ignoring its fraction.
    let seconds = whole as f64
        + if whole == 0 {
            0.0
        } else {
            micro as f64 / 1_000_000.0
        };
    if seconds >= 86400.0 {
        return None;
    }
    add_seconds(local, -sign * seconds)
        .ok()
        .filter(|t| t.to_zoned(jiff::tz::TimeZone::UTC).year() >= 1)
}
fn clock_parts(text: &str) -> Option<(i16, i16, i16, i32)> {
    let (clock, fraction) = text.split_once(['.', ',']).unwrap_or((text, ""));
    if clock.is_empty() || !clock.bytes().all(|b| b.is_ascii_digit() || b == b':') {
        return None;
    }
    if text.len() != clock.len()
        && (fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let parts = if clock.contains(':') {
        clock.split(':').collect::<Vec<_>>()
    } else {
        if ![2, 4, 6].contains(&clock.len()) {
            return None;
        }
        (0..clock.len())
            .step_by(2)
            .map(|i| &clock[i..i + 2])
            .collect()
    };
    if parts.is_empty()
        || parts.len() > 3
        || parts.iter().any(|p| p.len() != 2)
        || !fraction.is_empty() && parts.len() != 3
    {
        return None;
    }
    let hour = parts[0].parse().ok()?;
    let minute = parts
        .get(1)
        .map(|s| s.parse())
        .transpose()
        .ok()?
        .unwrap_or(0);
    let second = parts
        .get(2)
        .map(|s| s.parse())
        .transpose()
        .ok()?
        .unwrap_or(0);
    let digits = fraction.bytes().take(6).collect::<Vec<_>>();
    let micro = digits.iter().fold(0i32, |n, d| n * 10 + (d - b'0') as i32)
        * 10i32.pow(6 - digits.len() as u32);
    Some((hour, minute, second, micro))
}
pub fn stamp(value: Option<&str>) -> Option<Timestamp> {
    value.and_then(|s| parse_time(&Value::String(s.into())))
}
pub fn iso(moment: Timestamp) -> String {
    let base = moment
        .to_zoned(jiff::tz::TimeZone::UTC)
        .strftime("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let micros = moment.to_zoned(jiff::tz::TimeZone::UTC).subsec_nanosecond() / 1000;
    if micros == 0 {
        format!("{base}Z")
    } else {
        format!("{base}.{micros:06}Z")
    }
}
pub fn seconds_between(a: Timestamp, b: Timestamp) -> f64 {
    (a.as_microsecond() - b.as_microsecond()) as f64 / 1_000_000.0
}
pub fn add_seconds(at: Timestamp, seconds: f64) -> Result<Timestamp, crate::QuotaError> {
    if !seconds.is_finite() {
        return Err(crate::QuotaError::TimeRange);
    }
    let micros = (seconds * 1_000_000.0).round_ties_even();
    if micros < i64::MIN as f64 || micros >= i64::MAX as f64 {
        return Err(crate::QuotaError::TimeRange);
    }
    let sum = at
        .as_microsecond()
        .checked_add(micros as i64)
        .ok_or(crate::QuotaError::TimeRange)?;
    Timestamp::from_microsecond(sum).map_err(|_| crate::QuotaError::TimeRange)
}
pub fn duration(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    let (minutes, hours) = (s / 60, s / 3600);
    if s < 3600 {
        format!("{minutes}m")
    } else if hours < 48 {
        format!(
            "{hours}h{}",
            if minutes % 60 != 0 {
                format!(" {}m", minutes % 60)
            } else {
                String::new()
            }
        )
    } else {
        format!(
            "{}d{}",
            hours / 24,
            if hours % 24 != 0 {
                format!(" {}h", hours % 24)
            } else {
                String::new()
            }
        )
    }
}
