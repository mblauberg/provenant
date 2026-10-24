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

pub const SCHEMA: &str = "provenant.quota.v1";
pub const REPORT_SCHEMA: &str = "provenant.quota.report.v1";
pub const CACHE_SECONDS: u64 = 300;
pub const NEAR_EXHAUSTED: f64 = 95.0;

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

mod source;
pub use source::*;

pub mod display;
pub mod time;

#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("quota source has no qualifying recording")]
    Disabled,
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
