//! Quota records and explicit policy boundaries. Inert until the command cut-over.
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::print_stderr
)]
#![forbid(unsafe_code)]

mod pending;
pub use pending::*;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SCHEMA: &str = "provenant.quota.v1";
pub const REPORT_SCHEMA: &str = "provenant.quota.report.v1";
pub const CACHE_SECONDS: u64 = 300;
pub const HTTP_TIMEOUT_SECONDS: f64 = 8.0;
pub const PROCESS_TIMEOUT_SECONDS: f64 = 10.0;
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const NEAR_EXHAUSTED: f64 = 95.0;

/// Non-secret account identity. Credentials are deliberately absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub provider: String,
    pub label: String,
    pub identity_dir: PathBuf,
    #[serde(default)]
    pub active: bool,
    pub plan: Option<String>,
    pub priority: Option<i64>,
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub signed_out: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    RateLimited,
    Network,
    Auth,
    Missing,
    Other,
}
impl ErrorKind {
    pub fn from_name(name: &str) -> Self {
        match name {
            "rate_limited" => Self::RateLimited,
            "network" => Self::Network,
            "auth" => Self::Auth,
            "missing" => Self::Missing,
            _ => Self::Other,
        }
    }
}

/// Messages must be fixed, non-secret text, as in the provider adapters.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Unavailable {
    pub message: String,
    pub kind: ErrorKind,
    pub retry_after: Option<f64>,
    pub status: Option<u16>,
    pub retry_at: Option<String>,
}
impl Unavailable {
    pub fn new(message: impl Into<String>, kind: &str) -> Self {
        Self {
            message: message.into(),
            kind: ErrorKind::from_name(kind),
            retry_after: None,
            status: None,
            retry_at: None,
        }
    }
}

/// The later provider-quota units own these decisions. Implementations receive explicit
/// values, never a Python-style live module namespace or mutable global registry.
pub trait QuotaPolicy {
    fn pace(&self, window: &Window, now: jiff::Timestamp) -> Pace;
    fn binding<'a>(&self, windows: &[&'a DisplayWindow]) -> Option<&'a DisplayWindow>;
    fn reset_passed(&self, window: &Window, now: jiff::Timestamp) -> bool;
    fn sets_state(&self, window: &Window) -> bool;
    fn allowance_spent(&self, snapshot: &Snapshot, worst: Option<&DisplayWindow>) -> bool;
    fn credit_draw(&self, snapshot: &Snapshot, spent: bool) -> Option<UsageCredit>;
    fn burn_rate(&self, snapshot: &Snapshot, credit: &UsageCredit) -> Option<f64>;
    fn credit_advice(
        &self,
        windows: &[DisplayWindow],
        banked: Option<&BankedCredits>,
        now: jiff::Timestamp,
    ) -> CreditAdvice;
    fn normalise_usage_credits(&self, credits: &[UsageCredit]) -> Vec<UsageCredit>;
    fn normalise_resets(&self, resets: Option<&Resets>) -> Option<Resets>;
    fn window_pool(&self, provider: &str, name: &str) -> Option<String>;
    fn fingerprinted(&self, provider: &str, label: &str) -> bool;
    fn freshest(&self, values: &[Snapshot]) -> Vec<Snapshot>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_has_fixed_message_and_unknown_kind_falls_back() {
        let err = Unavailable::new("provider quota response was too large", "unrecognised");
        assert_eq!(err.to_string(), "provider quota response was too large");
        assert_eq!(err.kind, ErrorKind::Other);
        assert!(err.retry_at.is_none() && err.status.is_none());
    }
    #[test]
    fn account_defaults_are_per_value() {
        let value =
            serde_json::json!({"provider":"example", "label":"work", "identity_dir":"/fixture"});
        let mut a: Account = serde_json::from_value(value.clone()).unwrap();
        let b: Account = serde_json::from_value(value).unwrap();
        a.options.insert("quota".into(), serde_json::json!({}));
        assert!(b.options.is_empty() && !b.active && !b.signed_out);
    }
}

pub mod display;
pub mod time;

#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("quota number is out of range")]
    NumberOutOfRange,
    #[error("invalid quota record")]
    Record(#[source] serde_json::Error),
    #[error("quota timestamp is out of range")]
    TimeRange,
    #[error("invalid quota timestamp")]
    InvalidTimestamp,
    #[error("could not write quota report")]
    Write(#[source] std::io::Error),
}

#[cfg(test)]
mod oracle;
#[cfg(test)]
mod policy_tests;
pub mod schedule;
