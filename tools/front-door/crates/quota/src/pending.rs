//! Temporary wire models until the records crate owns the J2 quota records.
use crate::ErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub name: String,
    pub raw_kind: Option<String>,
    pub used_percent: Value,
    pub window_minutes: Option<Number>,
    pub reset_at: Option<String>,
    pub window_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_amount: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_amount: Option<Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub currency: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pace {
    pub state: String,
    pub summary: String,
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayWindow {
    #[serde(flatten)]
    pub window: Window,
    pub pace: Pace,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_passed: Option<bool>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BankedCredits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageCredit {
    #[serde(default)]
    pub kind: String,
    pub balance: Option<Number>,
    #[serde(default)]
    pub unlimited: bool,
    pub currency: Option<String>,
    pub unit: Option<String>,
    pub expires_at: Option<String>,
    #[serde(default)]
    pub source: String,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Resets {
    pub available: i64,
    #[serde(default)]
    pub expires_at: Vec<String>,
    #[serde(default)]
    pub source: String,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Allowance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinary_usage_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_control_reached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_reached_type: Option<String>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fetch {
    pub trigger: Option<String>,
    pub source_kind: Option<String>,
    pub last_duration_ms: Option<Number>,
    #[serde(default)]
    pub failures: Option<Number>,
    pub error_kind: Option<String>,
    pub backoff_until: Option<Value>,
    pub retry_after_s: Option<Number>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub next_due_at: String,
    pub reason: String,
    pub ttl_seconds: Number,
    pub trigger: Option<String>,
    pub failures: Number,
    pub backoff_until: Option<String>,
    pub error_kind: Option<ErrorKind>,
    pub last_duration_ms: Option<Number>,
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub schema: String,
    pub provider: String,
    pub account: String,
    pub plan: Option<String>,
    pub host: Option<String>,
    pub active: Option<bool>,
    pub priority: Option<i64>,
    pub observed_at: Option<Value>,
    pub attempted_at: Option<Value>,
    pub source: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub windows: Vec<Window>,
    #[serde(default)]
    pub unreported: Vec<String>,
    pub credits: Option<BankedCredits>,
    #[serde(default, skip_serializing_if = "OptionalRecord::is_missing")]
    pub usage_credits: OptionalRecord<Vec<UsageCredit>>,
    #[serde(default, skip_serializing_if = "OptionalRecord::is_missing")]
    pub resets: OptionalRecord<Resets>,
    #[serde(default, skip_serializing_if = "OptionalRecord::is_missing")]
    pub allowance: OptionalRecord<Allowance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch: Option<Fetch>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CreditAdvice {
    pub action: String,
    pub reset_credits: i64,
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayAccount {
    pub provider: String,
    pub account: String,
    pub plan: Option<String>,
    pub host: Option<String>,
    pub active: Option<bool>,
    pub priority: Option<i64>,
    pub observed_at: Option<Value>,
    pub attempted_at: Option<Value>,
    pub status: Option<String>,
    pub error: Option<String>,
    pub source: Option<String>,
    pub provider_name: String,
    pub display_name: Option<String>,
    pub plan_label: Option<String>,
    pub display_window: Option<String>,
    pub back_at: Option<String>,
    pub state: String,
    pub summary: String,
    pub windows: Vec<DisplayWindow>,
    pub unreported: Vec<String>,
    pub coverage_note: String,
    pub credits: Option<BankedCredits>,
    pub credit_advice: CreditAdvice,
    pub use_it_or_lose_it: Option<ExpiringHeadroom>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub usage_credits: Vec<UsageCredit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets: Option<Resets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drawing_credits: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_burn_per_hour: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpiringHeadroom {
    pub window: String,
    pub reset_at: Option<String>,
    pub unused_percent: Value,
    pub expiring_headroom_percent: Value,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    pub mode: String,
    pub account: Option<String>,
    pub window: Option<String>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplaySettings {
    pub short_window_alert_percent: Number,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema: String,
    pub generated_at: String,
    pub host: String,
    pub enabled: bool,
    pub next_due_at: Option<String>,
    pub refreshing: Vec<String>,
    pub providers: Vec<String>,
    pub accounts: Vec<DisplayAccount>,
    pub display: DisplaySettings,
    pub headline: Headline,
    pub placement_preference: BTreeMap<String, Vec<String>>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

// port: belongs in provenant-records
/// Optional wire blocks distinguish an absent key from an explicit null.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum OptionalRecord<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}
impl<T> OptionalRecord<T> {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
    pub fn as_ref(&self) -> Option<&T> {
        if let Self::Value(v) = self {
            Some(v)
        } else {
            None
        }
    }
}
impl<T> From<Option<T>> for OptionalRecord<T> {
    fn from(value: Option<T>) -> Self {
        value.map(Self::Value).unwrap_or(Self::Null)
    }
}
impl<T: Serialize> Serialize for OptionalRecord<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(v) => v.serialize(serializer),
            _ => serializer.serialize_none(),
        }
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for OptionalRecord<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(Self::from)
    }
}

// port: belongs in provenant-records
/// Python numbers retain their integer or floating point wire form.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Number {
    Int(i64),
    Unsigned(u64),
    Float(f64),
}
impl Default for Number {
    fn default() -> Self {
        Self::Int(0)
    }
}
impl Number {
    /// Floating point is used only for calculations, never for retained integers.
    pub fn get(self) -> f64 {
        match self {
            Self::Int(n) => n as f64,
            Self::Unsigned(n) => n as f64,
            Self::Float(n) => n,
        }
    }
    pub(crate) fn integer_value(self) -> Option<i128> {
        match self {
            Self::Int(n) => Some(n as i128),
            Self::Unsigned(n) => Some(n as i128),
            Self::Float(_) => None,
        }
    }
    pub(crate) fn numeric_cmp(self, other: Self) -> std::cmp::Ordering {
        fn integer_float(integer: i128, float: f64) -> std::cmp::Ordering {
            let whole = float.trunc() as i128;
            integer.cmp(&whole).then_with(|| {
                if float.fract() > 0.0 {
                    std::cmp::Ordering::Less
                } else if float.fract() < 0.0 {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
        }
        match (self.integer_value(), other.integer_value()) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(a), None) => integer_float(a, other.get()),
            (None, Some(b)) => integer_float(b, self.get()).reverse(),
            _ => self
                .get()
                .partial_cmp(&other.get())
                .unwrap_or(std::cmp::Ordering::Equal),
        }
    }
    pub(crate) fn min(self, other: Self) -> Self {
        if self.numeric_cmp(other).is_gt() {
            other
        } else {
            self
        }
    }
    pub(crate) fn max(self, other: Self) -> Self {
        if self.numeric_cmp(other).is_lt() {
            other
        } else {
            self
        }
    }
    pub(crate) fn multiply(self, other: Self) -> Result<Self, crate::QuotaError> {
        if let (Some(a), Some(b)) = (self.integer_value(), other.integer_value()) {
            let product = a
                .checked_mul(b)
                .ok_or(crate::QuotaError::NumberOutOfRange)?;
            if let Ok(n) = i64::try_from(product) {
                return Ok(Self::Int(n));
            }
            if let Ok(n) = u64::try_from(product) {
                return Ok(Self::Unsigned(n));
            }
            return Err(crate::QuotaError::NumberOutOfRange);
        }
        let product = self.get() * other.get();
        if !product.is_finite() {
            return Err(crate::QuotaError::NumberOutOfRange);
        }
        Ok(Self::Float(product))
    }
    /// Positive schedule inputs can be capped before a large intermediate
    /// product has to fit the retained integer range.
    pub(crate) fn multiply_capped(self, other: Self, cap: Self) -> Result<Self, crate::QuotaError> {
        if let (Some(a), Some(b)) = (self.integer_value(), other.integer_value()) {
            if a >= 0 && b >= 0 {
                // Both factors fit u64; their product always fits u128.
                let product = (a as u128) * (b as u128);
                if product > u64::MAX as u128 {
                    return if cap.numeric_cmp(Self::Unsigned(u64::MAX)).is_le() {
                        Ok(cap)
                    } else {
                        Err(crate::QuotaError::NumberOutOfRange)
                    };
                }
                return Ok(cap.min(Self::Unsigned(product as u64)));
            }
            return self.multiply(other).map(|product| cap.min(product));
        }
        // Python permits an infinite float intermediate that min then caps.
        Ok(cap.min(Self::Float(self.get() * other.get())))
    }
    pub(crate) fn rounded_integer(self) -> Result<Self, crate::QuotaError> {
        match self {
            Self::Float(n) => Self::checked_integer(n.round_ties_even()),
            n => Ok(n),
        }
    }
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        let n = value.as_number()?;
        n.as_i64()
            .map(Self::Int)
            .or_else(|| n.as_u64().map(Self::Unsigned))
            .or_else(|| n.is_f64().then(|| n.as_f64().map(Self::Float)).flatten())
    }
    pub fn integer_part(self) -> Result<Self, crate::QuotaError> {
        match self {
            Self::Float(n) => Self::checked_integer(n.trunc()),
            n => Ok(n),
        }
    }
    pub fn checked_integer(n: f64) -> Result<Self, crate::QuotaError> {
        if !n.is_finite() || n < i64::MIN as f64 || n >= 18446744073709551616.0 {
            return Err(crate::QuotaError::NumberOutOfRange);
        }
        Ok(if n < 0.0 {
            Self::Int(n as i64)
        } else {
            Self::Unsigned(n as u64)
        })
    }
}
impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = serde_json::Number::deserialize(deserializer)?;
        if let Some(n) = n.as_i64() {
            return Ok(Self::Int(n));
        }
        if let Some(n) = n.as_u64() {
            return Ok(Self::Unsigned(n));
        }
        if n.is_f64() {
            if let Some(n) = n.as_f64() {
                return Ok(Self::Float(n));
            }
        }
        Err(serde::de::Error::custom("quota number is out of range"))
    }
}

// port: belongs in provenant-records
/// Decode before constructing typed records, including numbers in unknown fields.
/// Check raw integer tokens before serde_json can coerce an oversized one to f64.
/// All record ingress must use this decoder; already-decoded Values lack token provenance.
pub fn decode_record<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T, crate::QuotaError> {
    let input = check_integer_tokens(input)?;
    serde_json::from_slice(&input).map_err(crate::QuotaError::Record)
}

impl Window {
    pub(crate) fn compare_used(&self, other: &Self) -> std::cmp::Ordering {
        match (
            Number::from_value(&self.used_percent),
            Number::from_value(&other.used_percent),
        ) {
            (Some(a), Some(b)) => a.numeric_cmp(b),
            _ => self.used().total_cmp(&other.used()),
        }
    }
    pub fn used(&self) -> f64 {
        self.used_percent.as_f64().unwrap_or(f64::NAN)
    }
}

// port: belongs in provenant-records
fn check_integer_tokens(input: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, crate::QuotaError> {
    let mut checked = std::borrow::Cow::Borrowed(input);
    let mut index = 0;
    while let Some(&byte) = input.get(index) {
        match byte {
            b'"' => {
                // Numbers in keys, text, and escaped quotes are never numeric tokens.
                index += 1;
                while let Some(&byte) = input.get(index) {
                    index += 1;
                    match byte {
                        b'\\' => index += 1,
                        b'"' => break,
                        _ => {}
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                index += 1;
                while input.get(index).is_some_and(|b| {
                    b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')
                }) {
                    index += 1;
                }
                let token = &input[start..index];
                if token == b"-0" {
                    // CPython json.loads('-0') is int(0); serde otherwise makes it -0.0.
                    checked.to_mut()[start] = b' ';
                }
                let digits = token.strip_prefix(b"-").unwrap_or(token);
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                    // This is ASCII by construction; JSON syntax is validated by serde below.
                    let text = std::str::from_utf8(token)
                        .map_err(|_| crate::QuotaError::NumberOutOfRange)?;
                    if text.parse::<i64>().is_err() && text.parse::<u64>().is_err() {
                        return Err(crate::QuotaError::NumberOutOfRange);
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(checked)
}

// port: belongs in provenant-core
// Unicode decimal digits, as accepted by Python re \d and int.
pub(crate) fn decimal_digit(c: char) -> Option<u8> {
    let code = u32::from(c);
    // port: belongs in provenant-core
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
        0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
        0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
        0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    ZEROS
        .iter()
        .find_map(|&zero| code.checked_sub(zero).filter(|&d| d < 10).map(|d| d as u8))
}

// port: belongs in provenant-core
pub(crate) fn python_digit(c: char) -> bool {
    decimal_digit(c).is_some()
        || matches!(
            u32::from(c),
            0xb2 | 0xb3
                | 0xb9
                | 0x1369
                | 0x136a
                | 0x136b
                | 0x136c
                | 0x136d
                | 0x136e
                | 0x136f
                | 0x1370
                | 0x1371
                | 0x19da
                | 0x2070
                | 0x2074
                | 0x2075
                | 0x2076
                | 0x2077
                | 0x2078
                | 0x2079
                | 0x2080
                | 0x2081
                | 0x2082
                | 0x2083
                | 0x2084
                | 0x2085
                | 0x2086
                | 0x2087
                | 0x2088
                | 0x2089
                | 0x2460
                | 0x2461
                | 0x2462
                | 0x2463
                | 0x2464
                | 0x2465
                | 0x2466
                | 0x2467
                | 0x2468
                | 0x2474
                | 0x2475
                | 0x2476
                | 0x2477
                | 0x2478
                | 0x2479
                | 0x247a
                | 0x247b
                | 0x247c
                | 0x2488
                | 0x2489
                | 0x248a
                | 0x248b
                | 0x248c
                | 0x248d
                | 0x248e
                | 0x248f
                | 0x2490
                | 0x24ea
                | 0x24f5
                | 0x24f6
                | 0x24f7
                | 0x24f8
                | 0x24f9
                | 0x24fa
                | 0x24fb
                | 0x24fc
                | 0x24fd
                | 0x24ff
                | 0x2776
                | 0x2777
                | 0x2778
                | 0x2779
                | 0x277a
                | 0x277b
                | 0x277c
                | 0x277d
                | 0x277e
                | 0x2780
                | 0x2781
                | 0x2782
                | 0x2783
                | 0x2784
                | 0x2785
                | 0x2786
                | 0x2787
                | 0x2788
                | 0x278a
                | 0x278b
                | 0x278c
                | 0x278d
                | 0x278e
                | 0x278f
                | 0x2790
                | 0x2791
                | 0x2792
                | 0x10a40
                | 0x10a41
                | 0x10a42
                | 0x10a43
                | 0x10e60
                | 0x10e61
                | 0x10e62
                | 0x10e63
                | 0x10e64
                | 0x10e65
                | 0x10e66
                | 0x10e67
                | 0x10e68
                | 0x11052
                | 0x11053
                | 0x11054
                | 0x11055
                | 0x11056
                | 0x11057
                | 0x11058
                | 0x11059
                | 0x1105a
                | 0x1f100
                | 0x1f101
                | 0x1f102
                | 0x1f103
                | 0x1f104
                | 0x1f105
                | 0x1f106
                | 0x1f107
                | 0x1f108
                | 0x1f109
                | 0x1f10a
        )
}

// port: belongs in provenant-core
pub(crate) fn python_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

// port: belongs in provenant-core
pub(crate) fn python_strip(text: &str) -> &str {
    text.trim_matches(python_whitespace)
}

// port: belongs in provenant-core
/// CPython datetime.fromtimestamp splits with modf before rounding half-even.
/// Duration arithmetic intentionally uses a separate timedelta-style conversion.
pub fn from_timestamp(seconds: f64) -> Result<jiff::Timestamp, crate::QuotaError> {
    if !seconds.is_finite() || seconds < i64::MIN as f64 || seconds >= i64::MAX as f64 {
        return Err(crate::QuotaError::TimeRange);
    }
    let micros = (seconds.trunc() as i64)
        .checked_mul(1_000_000)
        .and_then(|whole| {
            whole.checked_add((seconds.fract() * 1_000_000.0).round_ties_even() as i64)
        })
        .ok_or(crate::QuotaError::TimeRange)?;
    // CPython datetime supports years 1 through 9999, narrower than Jiff.
    if !(-62_135_596_800_000_000..253_402_300_800_000_000).contains(&micros) {
        return Err(crate::QuotaError::TimeRange);
    }
    jiff::Timestamp::from_microsecond(micros).map_err(|_| crate::QuotaError::TimeRange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checked_codec_distinguishes_integer_zero_strings_and_float_tokens() {
        let value: Value = decode_record(br#"{"n":-0,"text":"\"18446744073709551616\\","18446744073709551616":[1e100,18446744073709551616.0]}"#).unwrap();
        assert_eq!(value["n"].to_string(), "0");
        assert_eq!(value["text"], "\"18446744073709551616\\");
        for text in ["[1,]", "{", "01", "\"unterminated\\"] {
            assert!(matches!(
                decode_record::<Value>(text.as_bytes()),
                Err(crate::QuotaError::Record(_))
            ));
        }
    }

    #[test]
    fn fromtimestamp_matches_cpython_rounding_and_datetime_bounds() {
        let inputs = json!([
            0.0,
            0.0000005,
            -0.0000005,
            -0.0000015,
            1700000000.0000014,
            1735689600.7916253,
            -62135596800.0,
            -62135596801.0,
            253402300800.0
        ]);
        let actual: Vec<_> = inputs
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                from_timestamp(v.as_f64().unwrap())
                    .ok()
                    .map(crate::time::iso)
            })
            .collect();
        assert_eq!(actual[7], None);
        assert_eq!(actual[8], None);
        let Some(expected): Option<Vec<Option<String>>> = crate::oracle::run(
            crate::oracle::Interpreter::Python,
            &[
                "-c".as_ref(),
                r#"import json,sys
from datetime import datetime,timezone
result=[]
for value in json.load(sys.stdin):
    try: result.append(datetime.fromtimestamp(value,timezone.utc).isoformat().replace('+00:00','Z'))
    except (ValueError,OverflowError,OSError): result.append(None)
json.dump(result,sys.stdout)"#
                    .as_ref(),
            ],
            &inputs,
        ) else {
            return;
        };
        assert_eq!(actual, expected);
    }
}
