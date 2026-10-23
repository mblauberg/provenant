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
/// arbitrary_precision keeps oversized integer tokens intact until this check.
pub fn decode_record<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T, crate::QuotaError> {
    fn check(value: &Value) -> Result<(), crate::QuotaError> {
        match value {
            Value::Number(n) if !n.is_i64() && !n.is_u64() && !n.is_f64() => {
                return Err(crate::QuotaError::NumberOutOfRange);
            }
            Value::Array(items) => {
                for value in items {
                    check(value)?;
                }
            }
            Value::Object(items) => {
                for value in items.values() {
                    check(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let value: Value = serde_json::from_slice(input).map_err(crate::QuotaError::Record)?;
    check(&value)?;
    serde_json::from_value(value).map_err(crate::QuotaError::Record)
}

impl Window {
    pub fn used(&self) -> f64 {
        self.used_percent.as_f64().unwrap_or(f64::NAN)
    }
}
