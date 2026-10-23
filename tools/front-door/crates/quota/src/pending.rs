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
