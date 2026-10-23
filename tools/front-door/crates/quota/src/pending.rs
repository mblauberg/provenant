//! Temporary wire models until the records crate owns the J2 quota records.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use serde_json::Value;
use crate::ErrorKind;

// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub name: String,
    pub raw_kind: Option<String>,
    pub used_percent: f64,
    pub window_minutes: Option<f64>,
    pub reset_at: Option<String>,
    pub window_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub used_amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub limit_amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub currency: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub advisory: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub pool: Option<String>,
    #[serde(flatten)] pub extra: BTreeMap<String, Value>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pace {
    pub state: String,
    pub summary: String,
    #[serde(flatten)] pub details: BTreeMap<String, Value>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayWindow {
    #[serde(flatten)] pub window: Window,
    pub pace: Pace,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub pool_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub reset_passed: Option<bool>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BankedCredits { pub reset_credits: i64, #[serde(skip_serializing_if = "Option::is_none")] pub source: Option<String> }
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageCredit {
    #[serde(default)] pub kind: String,
    pub balance: Option<f64>,
    #[serde(default)] pub unlimited: bool,
    pub currency: Option<String>,
    pub unit: Option<String>,
    pub expires_at: Option<String>,
    #[serde(default)] pub source: String,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Resets { pub available: i64, #[serde(default)] pub expires_at: Vec<String>, #[serde(default)] pub source: String }
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Allowance {
    #[serde(skip_serializing_if = "Option::is_none")] pub ordinary_usage_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")] pub spend_control_reached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")] pub limit_reached_type: Option<String>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fetch {
    pub trigger: Option<String>, pub source_kind: Option<String>, pub last_duration_ms: Option<f64>,
    #[serde(default)] pub failures: f64, pub error_kind: Option<String>, pub backoff_until: Option<Value>,
    pub retry_after_s: Option<f64>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub next_due_at: String, pub reason: String, pub ttl_seconds: f64, pub trigger: Option<String>,
    pub failures: i64, pub backoff_until: Option<String>, pub error_kind: Option<ErrorKind>,
    pub last_duration_ms: Option<f64>, pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub notes: Vec<String>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)] pub schema: String,
    pub provider: String, pub account: String, pub plan: Option<String>, pub host: Option<String>,
    pub active: Option<bool>, pub priority: Option<i64>, pub observed_at: Option<Value>,
    pub attempted_at: Option<Value>, pub source: Option<String>, pub status: Option<String>, pub error: Option<String>,
    #[serde(default)] pub windows: Vec<Window>, #[serde(default)] pub unreported: Vec<String>,
    pub credits: Option<BankedCredits>, #[serde(default)] pub usage_credits: Vec<UsageCredit>, pub resets: Option<Resets>,
    pub allowance: Option<Allowance>, pub schedule: Option<Schedule>, pub fetch: Option<Fetch>,
    #[serde(flatten)] pub extra: BTreeMap<String, Value>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CreditAdvice { pub action: String, pub reset_credits: i64, #[serde(flatten)] pub details: BTreeMap<String, Value> }
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayAccount {
    pub provider: String, pub account: String, pub plan: Option<String>, pub host: Option<String>,
    pub active: Option<bool>, pub priority: Option<i64>, pub observed_at: Option<Value>, pub attempted_at: Option<Value>,
    pub status: Option<String>, pub error: Option<String>, pub source: Option<String>, pub provider_name: String,
    pub display_name: Option<String>, pub plan_label: Option<String>, pub display_window: Option<String>,
    pub back_at: Option<String>, pub state: String, pub summary: String, pub windows: Vec<DisplayWindow>,
    pub unreported: Vec<String>, pub coverage_note: String, pub credits: Option<BankedCredits>,
    pub credit_advice: CreditAdvice, pub use_it_or_lose_it: Option<ExpiringHeadroom>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)] pub usage_credits: Vec<UsageCredit>,
    #[serde(skip_serializing_if = "Option::is_none")] pub resets: Option<Resets>,
    #[serde(skip_serializing_if = "Option::is_none")] pub drawing_credits: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")] pub credit_burn_per_hour: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub schedule: Option<Schedule>,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpiringHeadroom { pub window: String, pub reset_at: Option<String>, pub unused_percent: Value, pub expiring_headroom_percent: Value }
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Headline { pub mode: String, pub account: Option<String>, pub window: Option<String> }
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplaySettings { pub short_window_alert_percent: f64 }
// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema: String, pub generated_at: String, pub host: String, pub enabled: bool,
    pub next_due_at: Option<String>, pub refreshing: Vec<String>, pub providers: Vec<String>,
    pub accounts: Vec<DisplayAccount>, pub display: DisplaySettings, pub headline: Headline,
    pub placement_preference: BTreeMap<String, Vec<String>>,
}
