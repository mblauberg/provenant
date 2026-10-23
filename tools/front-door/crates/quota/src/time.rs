//! Python-compatible numeric and timestamp coercion at the quota boundary.
use jiff::{civil::{Date, DateTime}, Timestamp};
use serde_json::Value;

pub fn number(value: &Value) -> Option<f64> {
    let n = match value { Value::Number(n) => n.as_f64(), Value::String(s) => s.trim().replace('_', "").parse().ok(), _ => None }?;
    n.is_finite().then_some(n)
}
pub fn parse_time(value: &Value) -> Option<Timestamp> {
    let timestamp = if let Some(mut seconds) = number(value) {
        if seconds > 10_000_000_000.0 { seconds /= 1000.0; }
        if seconds <= 0.0 { return None; }
        Timestamp::from_microsecond((seconds * 1_000_000.0).round_ties_even() as i64).ok()?
    } else {
        let text = value.as_str()?.trim();
        text.parse::<Timestamp>().ok().or_else(|| text.parse::<DateTime>().ok().and_then(|d| d.to_zoned(jiff::tz::TimeZone::UTC).ok().map(|z| z.timestamp())))
            .or_else(|| text.parse::<Date>().ok().and_then(|d| d.at(0,0,0,0).to_zoned(jiff::tz::TimeZone::UTC).ok().map(|z| z.timestamp())))?
    };
    (timestamp.to_zoned(jiff::tz::TimeZone::UTC).year() >= 1).then_some(timestamp)
}
pub fn stamp(value: Option<&str>) -> Option<Timestamp> { value.and_then(|s| parse_time(&Value::String(s.into()))) }
pub fn iso(moment: Timestamp) -> String {
    let base = moment.to_zoned(jiff::tz::TimeZone::UTC).strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = moment.subsec_nanosecond() / 1000;
    if micros == 0 { format!("{base}Z") } else { format!("{base}.{micros:06}Z") }
}
pub fn seconds_between(a: Timestamp, b: Timestamp) -> f64 { (a.as_microsecond() - b.as_microsecond()) as f64 / 1_000_000.0 }
pub fn add_seconds(at: Timestamp, seconds: f64) -> Result<Timestamp, crate::QuotaError> {
    if !seconds.is_finite() { return Err(crate::QuotaError::TimeRange); }
    let micros = (seconds * 1_000_000.0).round_ties_even();
    if micros < i64::MIN as f64 || micros >= i64::MAX as f64 { return Err(crate::QuotaError::TimeRange); }
    let sum = at.as_microsecond().checked_add(micros as i64).ok_or(crate::QuotaError::TimeRange)?;
    Timestamp::from_microsecond(sum).map_err(|_| crate::QuotaError::TimeRange)
}
pub fn duration(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    let (minutes, hours) = (s / 60, s / 3600);
    if s < 3600 { format!("{minutes}m") }
    else if hours < 48 { format!("{hours}h{}", if minutes % 60 != 0 { format!(" {}m", minutes % 60) } else { String::new() }) }
    else { format!("{}d{}", hours / 24, if hours % 24 != 0 { format!(" {}h", hours % 24) } else { String::new() }) }
}
