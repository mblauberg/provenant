//! Provider-owned wire decoding and supervisor effect contracts.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
pub struct QuotaPolicy {
    pub adapter: &'static str,
    pub timer_seconds: u64,
    pub panel_seconds: u64,
    pub probe_seconds: u64,
    pub low_power: bool,
    pub solo: bool,
}

#[cfg(test)]
pub(crate) const TEST_POLICIES: &[QuotaPolicy] = &[
    QuotaPolicy {
        adapter: "claude",
        timer_seconds: 300,
        panel_seconds: 120,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    },
    QuotaPolicy {
        adapter: "agy",
        timer_seconds: 1200,
        panel_seconds: 900,
        probe_seconds: 30,
        low_power: false,
        solo: true,
    },
    QuotaPolicy {
        adapter: "opencode",
        timer_seconds: 300,
        panel_seconds: 120,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    },
];

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

/// What the supervisor does to obtain a capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaEffectKind {
    HttpGet,
    Command,
    LocalLedger,
}

/// Effects are interpreted by supervisor, never executed by a source.
/// Credential references are the account scope. Secret values never enter plans.
#[derive(Debug)]
pub struct QuotaProbePlan {
    pub kind: QuotaEffectKind,
    pub account: AccountScope,
    pub input: QuotaProbeInput,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration_expires_unix_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calibrated_windows: Vec<String>,
    pub plan: Option<String>,
    /// Subscription tier when the provider response reports one.
    pub tier: Option<String>,
    pub billing_anchor: Option<String>,
    pub fingerprint: Option<String>,
    pub priority: Option<u64>,
    pub credit_balance: Option<String>,
    pub reset_credits: Option<u64>,
    pub reset_expiries_unix_seconds: Vec<i64>,
    pub note: Option<String>,
}
impl QuotaDetails {
    pub fn calibration_active(&self, pool: &str, now: i64) -> bool {
        self.calibration_expires_unix_seconds
            .is_some_and(|expiry| now < expiry)
            && self.calibrated_windows.iter().any(|window| window == pool)
    }
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
