//! Small compatibility contracts awaiting their shared owners.
use serde::{Deserialize, Serialize};

// port: belongs in provenant-fabric-runtime
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub project: String,
    pub agent_id: String,
    pub provider: String,
}

// port: belongs in provenant-core
/// ECMAScript trim, which differs from Rust's Unicode whitespace set.
pub(crate) fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

// port: belongs in provenant-supervisor
/// Runtime-supplied UUID generator. The store never reads a global random source.
pub trait IdSource {
    fn uuid(&self) -> crate::Result<String>;
}

// port: belongs in provenant-core
/// JavaScript Number(string) used by the legacy terminal-notice key.
pub(crate) fn js_number(text: &str) -> Option<f64> {
    let text = text.trim_matches(js_whitespace);
    if text.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }
    if let Some(bits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        return u64::from_str_radix(bits, 2).ok().map(|v| v as f64);
    }
    if let Some(octal) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        return u64::from_str_radix(octal, 8).ok().map(|v| v as f64);
    }
    if text == "Infinity" || text == "+Infinity" {
        return Some(f64::INFINITY);
    }
    if text == "-Infinity" {
        return Some(f64::NEG_INFINITY);
    }
    if text
        .bytes()
        .any(|b| !matches!(b, b'0'..=b'9' | b'.' | b'-' | b'+' | b'e' | b'E'))
    {
        return None;
    }
    text.parse().ok()
}

// port: belongs in provenant-core
/// A UTF-16 prefix, replacing lone surrogates as the UTF-8 SQLite binding does.
pub(crate) fn js_slice(text: &str, end: usize) -> String {
    String::from_utf16_lossy(&text.encode_utf16().take(end).collect::<Vec<_>>())
}

// port: belongs in provenant-core
/// ECMAScript Date.toISOString, including its signed six-digit years and TimeClip.
pub(crate) fn js_iso(ms: i64) -> crate::Result<String> {
    if !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&ms) {
        return Err(crate::Error::InvalidTime);
    }
    // Gregorian civil date from days since 1970-01-01. Division is Euclidean
    // for dates before the epoch; all intermediates are bounded by TimeClip.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+07}")
    };
    let day_ms = ms.rem_euclid(86_400_000);
    let hour = day_ms / 3_600_000;
    let minute = day_ms / 60_000 % 60;
    let second = day_ms / 1000 % 60;
    let millis = day_ms % 1000;
    Ok(format!(
        "{year}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}
