//! Probe contracts only. Scheduling, admission and persistence belong to step 2.
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
/// No effect form is enabled until its wire and memory-only capture are recorded.
#[derive(Debug)]
pub enum QuotaProbePlan {}

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
