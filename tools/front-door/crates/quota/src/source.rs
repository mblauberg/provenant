//! Provider-owned wire decoding and supervisor effect contracts.
use serde::{Deserialize, Serialize};

/// Opaque, non-secret provider identity; never an email, credential or auth path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountScope {
    pub adapter: String,
    pub account_key: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaProbeInput {
    pub now_unix_seconds: i64,
    pub deadline_unix_seconds: i64,
    /// Reference to recorded authority, never a credential.
    pub authority_ref: Option<String>,
}

/// Effects are interpreted by supervisor, never executed by a source.
/// Credential references are the account scope. Secret values never enter plans.
#[derive(Debug)]
pub enum QuotaProbePlan {
    HttpGet {
        account: AccountScope,
        input: QuotaProbeInput,
    },
    Command {
        account: AccountScope,
        input: QuotaProbeInput,
    },
    LocalLedger {
        account: AccountScope,
        input: QuotaProbeInput,
    },
}
impl QuotaProbePlan {
    pub fn account(&self) -> &AccountScope {
        match self {
            Self::HttpGet { account, .. }
            | Self::Command { account, .. }
            | Self::LocalLedger { account, .. } => account,
        }
    }
    pub fn input(&self) -> &QuotaProbeInput {
        match self {
            Self::HttpGet { input, .. }
            | Self::Command { input, .. }
            | Self::LocalLedger { input, .. } => input,
        }
    }
}

/// The process edge discovers configured accounts and interprets bounded effects.
pub trait QuotaEffects {
    fn accounts(&self) -> Result<Vec<AccountScope>, crate::QuotaError>;
    fn capture(
        &self,
        plan: &QuotaProbePlan,
        previous: Option<&crate::AccountRecord>,
    ) -> Result<ProbeCapture, crate::ProbeFailure>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuotaDetails {
    pub plan: Option<String>,
    pub fingerprint: Option<String>,
    pub priority: Option<u64>,
    pub credit_balance: Option<String>,
    pub reset_credits: Option<u64>,
    pub reset_expiries_unix_seconds: Vec<i64>,
    pub note: Option<String>,
}

/// Memory-only bytes, deliberately not Debug or Serialize. Never spool a capture.
pub struct ProbeCapture {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i64>,
    pub overflowed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Available,
    CreditOnly,
    Exhausted,
    Stale,
    Unknown,
    AuthRequired,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaWindow {
    pub pool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_seconds: Option<u64>,
    pub used_basis_points: u64,
    pub resets_unix_seconds: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaSnapshot {
    pub schema_version: u64,
    pub account: AccountScope,
    pub revision: u64,
    pub status: QuotaStatus,
    pub observed_unix_seconds: i64,
    pub source_ref: String,
    pub authority_ref: Option<String>,
    pub windows: Vec<QuotaWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<QuotaDetails>,
}

pub trait QuotaSource: Sync {
    fn adapter_id(&self) -> &'static str;
    fn probe(
        &self,
        account: &AccountScope,
        input: &QuotaProbeInput,
    ) -> Result<QuotaProbePlan, crate::QuotaError>;
    fn decode(
        &self,
        plan: &QuotaProbePlan,
        capture: &ProbeCapture,
    ) -> Result<QuotaSnapshot, crate::QuotaError>;
}

/// The binary injects provider registrations. Quota never imports providers.
pub fn plan_probe(
    sources: &[&dyn QuotaSource],
    account: &AccountScope,
    input: &QuotaProbeInput,
) -> Result<QuotaProbePlan, crate::QuotaError> {
    account.validate()?;
    let duration = input
        .deadline_unix_seconds
        .checked_sub(input.now_unix_seconds)
        .ok_or(crate::QuotaError::TimeRange)?;
    if !(1..=crate::schedule::TICK_BUDGET_SECONDS).contains(&duration) {
        return Err(crate::QuotaError::Invalid("probe deadline"));
    }
    sources
        .iter()
        .find(|s| s.adapter_id() == account.adapter)
        .ok_or(crate::QuotaError::Disabled)?
        .probe(account, input)
}
