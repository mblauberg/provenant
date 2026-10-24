use crate::{AccountScope, QuotaError, QuotaSnapshot, QuotaStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_ACCOUNTS: usize = 128;
pub const MAX_WINDOWS: usize = 64;
pub const MAX_RECORD_BYTES: u64 = 1_048_576;

pub fn component(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn reference(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 256
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn detail_text(text: &str, maximum: usize) -> bool {
    !text.is_empty()
        && text.len() <= maximum
        && !text.chars().any(char::is_control)
        && !text.contains(['@', '/', '\\'])
}
fn fingerprint(text: &str) -> bool {
    let (hex, length) = text
        .strip_prefix("google-")
        .map_or((text, 16), |hex| (hex, 12));
    hex.len() == length
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl AccountScope {
    pub fn validate(&self) -> Result<(), QuotaError> {
        if !component(&self.adapter)
            || !component(&self.account_key)
            || self.label.is_empty()
            || self.label.len() > 128
            || self.label.chars().any(char::is_control)
            || self.label.contains(['@', '/', '\\'])
        {
            return Err(QuotaError::Invalid("account identity"));
        }
        Ok(())
    }
}
impl QuotaSnapshot {
    pub fn validate(&self) -> Result<(), QuotaError> {
        self.account.validate()?;
        if self.schema_version != 1
            || self.windows.len() > MAX_WINDOWS
            || !reference(&self.source_ref)
            || self.authority_ref.as_ref().is_some_and(|r| !reference(r))
            || self.details.as_ref().is_some_and(|details| {
                details
                    .plan
                    .as_ref()
                    .is_some_and(|value| !detail_text(value, 128))
                    || details
                        .fingerprint
                        .as_ref()
                        .is_some_and(|value| !fingerprint(value))
                    || details
                        .credit_balance
                        .as_ref()
                        .is_some_and(|value| !detail_text(value, 128))
                    || details
                        .note
                        .as_ref()
                        .is_some_and(|value| !detail_text(value, 256))
                    || details.reset_expiries_unix_seconds.len() > MAX_WINDOWS
            })
        {
            return Err(QuotaError::Invalid("snapshot header or bounds"));
        }
        let mut pools = BTreeSet::new();
        for window in &self.windows {
            if window.pool.is_empty() || window.pool.len() > 256 || !pools.insert(&window.pool) {
                return Err(QuotaError::Invalid("allowance pool"));
            }
            // Usage may exceed 100%; it never passes through floating point.
            if window.used_basis_points > 1_000_000 {
                return Err(QuotaError::NumberOutOfRange);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshClaim {
    pub token: String,
    pub deadline_unix_seconds: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountRecord {
    pub schema_version: u64,
    pub account: AccountScope,
    pub revision: u64,
    pub snapshot: Option<QuotaSnapshot>,
    pub claim: Option<RefreshClaim>,
    pub last_attempt_unix_seconds: Option<i64>,
    pub backoff_until_unix_seconds: Option<i64>,
    pub failures: u64,
    pub last_error: Option<ProbeFailure>,
    pub refusals: Vec<crate::Refusal>,
}
impl AccountRecord {
    pub fn empty(account: AccountScope) -> Self {
        Self {
            schema_version: 1,
            account,
            revision: 0,
            snapshot: None,
            claim: None,
            last_attempt_unix_seconds: None,
            backoff_until_unix_seconds: None,
            failures: 0,
            last_error: None,
            refusals: Vec::new(),
        }
    }
    pub fn validate(&self) -> Result<(), QuotaError> {
        self.account.validate()?;
        if self.schema_version != 1 || self.claim.as_ref().is_some_and(|c| !component(&c.token)) {
            return Err(QuotaError::Invalid("record header or claim"));
        }
        if let Some(claim) = &self.claim {
            let started = self
                .last_attempt_unix_seconds
                .ok_or(QuotaError::Invalid("claim without attempt"))?;
            let maximum = started
                .checked_add(crate::schedule::TICK_BUDGET_SECONDS)
                .ok_or(QuotaError::TimeRange)?;
            if claim.deadline_unix_seconds <= started || claim.deadline_unix_seconds > maximum {
                return Err(QuotaError::Invalid("claim deadline"));
            }
        }
        if let Some(backoff) = self.backoff_until_unix_seconds {
            let started = self
                .last_attempt_unix_seconds
                .ok_or(QuotaError::Invalid("backoff without attempt"))?;
            let maximum = started
                .checked_add(crate::schedule::TICK_BUDGET_SECONDS)
                .and_then(|s| s.checked_add(86400))
                .ok_or(QuotaError::TimeRange)?;
            if backoff <= started || backoff > maximum {
                return Err(QuotaError::Invalid("backoff deadline"));
            }
        }
        if self.refusals.len() > MAX_WINDOWS
            || self.refusals.iter().any(|r| {
                r.account != self.account
                    || r.model.is_empty()
                    || r.model.len() > 256
                    || r.pool
                        .as_ref()
                        .is_some_and(|p| p.is_empty() || p.len() > 256)
            })
        {
            return Err(QuotaError::Invalid("refusal scope or bounds"));
        }
        if let Some(snapshot) = &self.snapshot {
            snapshot.validate()?;
            if snapshot.account != self.account || snapshot.revision > self.revision {
                return Err(QuotaError::Invalid("snapshot identity or revision"));
            }
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, QuotaError> {
        let value: Self = provenant_records::decode_v1(bytes, provenant_records::Limits::CONFIG)?;
        value.validate()?;
        Ok(value)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeFailure {
    Network,
    RateLimited,
    Auth,
    MissingCli,
    Disabled,
    Malformed,
}

/// A compare-and-swap intent. The edge locks, re-reads, checks and publishes;
/// there is no lock or filesystem access inside quota.
#[derive(Clone, Debug)]
pub struct UpdateIntent {
    pub account: AccountScope,
    pub expected: Option<AccountRecord>,
    pub replacement: Option<AccountRecord>,
}
impl UpdateIntent {
    pub fn check(&self, current: Option<&AccountRecord>) -> Result<(), QuotaError> {
        self.account.validate()?;
        if current != self.expected.as_ref() || current.is_some_and(|r| r.account != self.account) {
            return Err(QuotaError::Conflict);
        }
        if let Some(next) = &self.replacement {
            next.validate()?;
            // The display label may change; the adapter and key identify the account.
            if next.account.adapter != self.account.adapter
                || next.account.account_key != self.account.account_key
                || next.revision
                    != self
                        .expected
                        .as_ref()
                        .map_or(0, |r| r.revision)
                        .checked_add(1)
                        .ok_or(QuotaError::NumberOutOfRange)?
            {
                return Err(QuotaError::Invalid("update revision"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountRow {
    pub account: AccountScope,
    pub snapshot: Option<QuotaSnapshot>,
    pub status: QuotaStatus,
    pub age_seconds: Option<u64>,
    pub next_due_unix_seconds: Option<i64>,
    pub refreshing: bool,
    pub error: Option<ProbeFailure>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRow {
    pub adapter: String,
    pub status: QuotaStatus,
    pub error: Option<ProbeFailure>,
    pub next_due_unix_seconds: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub total: u64,
    pub known: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaReport {
    pub schema_version: u64,
    pub generated_unix_seconds: i64,
    pub accounts: Vec<AccountRow>,
    pub sources: Vec<SourceRow>,
    pub coverage: Coverage,
    pub refreshing_ids: Vec<String>,
    pub next_due_unix_seconds: Option<i64>,
    pub report_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_claim_cannot_exceed_the_tick_budget_or_exist_without_an_attempt() {
        let mut record = AccountRecord::empty(AccountScope {
            adapter: "claude".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        });
        record.claim = Some(RefreshClaim {
            token: "claim".into(),
            deadline_unix_seconds: i64::MAX,
        });
        assert!(record.validate().is_err());
        record.last_attempt_unix_seconds = Some(100);
        assert!(record.validate().is_err());
        record.claim.as_mut().unwrap().deadline_unix_seconds = 155;
        assert!(record.validate().is_ok());
        record.backoff_until_unix_seconds = Some(i64::MAX);
        assert!(record.validate().is_err());
        record.backoff_until_unix_seconds = Some(86555);
        assert!(record.validate().is_ok());
    }
}
