use crate::{AccountScope, QuotaError, QuotaSnapshot, QuotaStatus};
use provenant_records::ExtraFields;
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
pub(crate) fn reference(text: &str) -> bool {
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
                    .source
                    .as_ref()
                    .is_some_and(|s| !detail_text(s, 128))
                    || details
                        .coverage
                        .as_ref()
                        .is_some_and(|s| !detail_text(s, 128))
                    || details.calibrated_windows.len() > MAX_WINDOWS
                    || details
                        .calibrated_windows
                        .iter()
                        .any(|s| !detail_text(s, 128))
                    || details
                        .plan
                        .as_ref()
                        .is_some_and(|value| !detail_text(value, 128))
                    || details
                        .tier
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
pub struct RefreshClaim {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub token: String,
    pub deadline_unix_seconds: i64,
}
/// Metadata only. Change detection never opens credential or profile files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialMetadata {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub path: std::path::PathBuf,
    pub stat: Option<CredentialStat>,
}
impl CredentialMetadata {
    /// Compare the observed credential identity without interpreting forward fields.
    pub fn same_observation(&self, other: &Self) -> bool {
        self.path == other.path
            && match (&self.stat, &other.stat) {
                (Some(left), Some(right)) => left.same_observation(right),
                (None, None) => true,
                _ => false,
            }
    }

    /// Adopt a fresh observation at the same path while retaining opaque fields.
    pub fn updated_from(&self, current: &Self) -> Self {
        if self.path != current.path {
            return current.clone();
        }
        let mut updated = current.clone();
        preserve_extra(&self.extra, &mut updated.extra);
        if let (Some(previous), Some(stat)) = (&self.stat, &mut updated.stat) {
            preserve_extra(&previous.extra, &mut stat.extra);
        }
        updated
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialStat {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub uid: u32,
    pub size: u64,
    pub modified_ns: i128,
}
impl CredentialStat {
    /// Compare file metadata without treating opaque forward fields as identity.
    pub fn same_observation(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.mode == other.mode
            && self.uid == other.uid
            && self.size == other.size
            && self.modified_ns == other.modified_ns
    }
}

fn preserve_extra(previous: &ExtraFields, current: &mut ExtraFields) {
    for (key, value) in previous {
        current.entry(key.clone()).or_insert_with(|| value.clone());
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthState {
    #[serde(flatten)]
    pub extra: ExtraFields,
    /// Provider CLI access-token expiry, informational only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token_expiry_unix_seconds: Option<i64>,
    /// Provider CLI `loggedIn`; only false establishes signed-out state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_in: Option<bool>,
    /// Observed need for renewal or revalidation, including an API 401.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub refresh_pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_unix_seconds: Option<i64>,
}
impl AuthState {
    fn valid(&self) -> bool {
        self.access_token_expiry_unix_seconds
            .is_none_or(|expiry| jiff::Timestamp::from_second(expiry).is_ok())
            && self
                .observed_unix_seconds
                .is_none_or(|at| jiff::Timestamp::from_second(at).is_ok())
            && (!self.refresh_pending || self.signed_in == Some(true))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRecord {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub schema_version: u64,
    pub account: AccountScope,
    pub revision: u64,
    pub snapshot: Option<QuotaSnapshot>,
    pub claim: Option<RefreshClaim>,
    pub last_attempt_unix_seconds: Option<i64>,
    pub backoff_until_unix_seconds: Option<i64>,
    pub failures: u64,
    pub last_error: Option<ProbeFailure>,
    /// A successful lane is newer account-scoped evidence than ambient detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_verified_unix_seconds: Option<i64>,
    /// Safe provider auth facts; no credential material is retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_state: Option<AuthState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_metadata: Vec<CredentialMetadata>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auth_retry_pending: bool,
    pub refusals: Vec<crate::Refusal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capacity_cooldowns: Vec<crate::CapacityCooldown>,
}
impl AccountRecord {
    pub fn empty(account: AccountScope) -> Self {
        Self {
            extra: Default::default(),
            schema_version: 1,
            account,
            revision: 0,
            snapshot: None,
            claim: None,
            last_attempt_unix_seconds: None,
            backoff_until_unix_seconds: None,
            failures: 0,
            last_error: None,
            auth_verified_unix_seconds: None,
            auth_state: None,
            credential_metadata: Vec::new(),
            auth_retry_pending: false,
            refusals: Vec::new(),
            capacity_cooldowns: Vec::new(),
        }
    }
    pub fn validate(&self) -> Result<(), QuotaError> {
        self.account.validate()?;
        if self.schema_version != 1 || self.claim.as_ref().is_some_and(|c| !component(&c.token)) {
            return Err(QuotaError::Invalid("record header or claim"));
        }
        if self.auth_state.as_ref().is_some_and(|state| !state.valid()) {
            return Err(QuotaError::Invalid("authentication metadata"));
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
        if self.credential_metadata.len() > 8
            || self
                .credential_metadata
                .iter()
                .any(|metadata| !metadata.path.is_absolute())
        {
            return Err(QuotaError::Invalid("credential metadata bounds"));
        }
        if self.refusals.len() > MAX_WINDOWS
            || self.refusals.iter().any(|r| {
                !r.account.same_scope(&self.account)
                    || r.model.is_empty()
                    || r.model.len() > 256
                    || r.pool
                        .as_ref()
                        .is_some_and(|p| p.is_empty() || p.len() > 256)
            })
        {
            return Err(QuotaError::Invalid("refusal scope or bounds"));
        }
        if self.capacity_cooldowns.len() > MAX_WINDOWS
            || self.capacity_cooldowns.iter().any(|c| {
                !c.account.same_scope(&self.account)
                    || c.model.is_empty()
                    || c.model.len() > 256
                    || c.model.chars().any(char::is_control)
                    || c.expires_unix_seconds <= c.observed_unix_seconds
            })
        {
            return Err(QuotaError::Invalid("capacity cooldown scope or bounds"));
        }
        if let Some(snapshot) = &self.snapshot {
            snapshot.validate()?;
            if !snapshot.account.same_scope(&self.account) || snapshot.revision > self.revision {
                return Err(QuotaError::Invalid("snapshot identity or revision"));
            }
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, QuotaError> {
        let value: Self = provenant_records::decode(bytes, provenant_records::Limits::CONFIG)?;
        value.validate()?;
        Ok(value)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeFailure {
    Network,
    RateLimited,
    /// The selected account is signed in but access-token renewal is pending.
    RefreshDeferred,
    Auth,
    /// The selected login's recorded credential has expired.
    LoginExpired,
    /// The CLI is signed in as a different account than the registered key.
    AccountMismatch,
    /// An agy isolated account has no usable private login keychain.
    IsolatedKeychainUnavailable,
    MissingCli,
    ProbeDirectory,
    Disabled,
    Malformed,
}
impl ProbeFailure {
    /// Every persisted probe-failure class, in declaration order.
    pub const ALL: &[Self] = &[
        Self::Network,
        Self::RateLimited,
        Self::RefreshDeferred,
        Self::Auth,
        Self::LoginExpired,
        Self::AccountMismatch,
        Self::IsolatedKeychainUnavailable,
        Self::MissingCli,
        Self::ProbeDirectory,
        Self::Disabled,
        Self::Malformed,
    ];

    /// Default readiness classification, before provider execution detail.
    pub fn readiness_reason(self) -> crate::ReadinessReason {
        match self {
            Self::MissingCli | Self::Disabled => crate::ReadinessReason::MissingExecutable,
            Self::Auth => crate::ReadinessReason::SignedOut,
            Self::LoginExpired => crate::ReadinessReason::LoginExpired,
            Self::AccountMismatch => crate::ReadinessReason::AccountMismatch,
            _ => crate::ReadinessReason::CommandFailure,
        }
    }

    /// Failures the owner can repair locally, so a credential change or an
    /// owner tick re-probes them at once instead of waiting out the backoff.
    /// A provider rate limit is the one failure only time clears.
    pub fn repairable(self) -> bool {
        self != Self::RateLimited
    }

    /// Failures only a login or re-registration repairs; the account is
    /// reported as `auth_required` until then.
    pub fn needs_login(self) -> bool {
        matches!(
            self,
            Self::Auth | Self::LoginExpired | Self::AccountMismatch
        )
    }
}

/// A compare-and-swap intent. The edge locks, re-reads, checks and publishes;
/// there is no lock or filesystem access inside quota.
#[derive(Clone, Debug)]
pub struct UpdateIntent {
    pub account: AccountScope,
    pub expected: Option<AccountRecord>,
    pub replacement: Option<AccountRecord>,
    /// Probe result metadata stays in the in-memory write intent, never the
    /// v1 quota account JSON. `Some(None)` means a successful probe cleared it.
    pub readiness_probe_result: Option<Option<crate::ReadinessReason>>,
    /// Safe expiry metadata and its freshness interval from this probe. Outer
    /// None means no probe completed; inner None means expiry is unknown.
    pub login_probe_expiry: Option<(Option<i64>, u64)>,
}
impl UpdateIntent {
    pub fn check(&self, current: Option<&AccountRecord>) -> Result<(), QuotaError> {
        self.account.validate()?;
        if current != self.expected.as_ref()
            || current.is_some_and(|r| !r.account.same_scope(&self.account))
        {
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
pub struct AccountRow {
    pub account: AccountScope,
    /// Readiness scoped to this account, not to any particular model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<crate::ReadinessRead>,
    /// Account-scoped login expiry evidence, independent of quota status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_expiry: Option<LoginExpiryView>,
    /// Safe authentication facts from the provider CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_state: Option<AuthState>,
    pub snapshot: Option<QuotaSnapshot>,
    pub status: QuotaStatus,
    /// RFC 3339 expiry for an active free-pool refusal, when the provider recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    /// Active provider refusals scoped to a pool or model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<RefusalRow>,
    /// Whether a recorded refusal currently holds this account's free pool.
    #[serde(default, skip_serializing_if = "is_false")]
    pub free_pool_refusal: bool,
    pub age_seconds: Option<u64>,
    pub next_due_unix_seconds: Option<i64>,
    pub refreshing: bool,
    pub error: Option<ProbeFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    /// Live provider capacity cooldowns for this account.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capacity_cooldowns: Vec<CapacityCooldownRow>,
    /// Menu bar choices from the matching registered account, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<RowDisplay>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginExpiryView {
    pub state: crate::LoginExpiryState,
    pub expires_unix_seconds: Option<i64>,
    pub observed_unix_seconds: Option<i64>,
}
fn is_false(value: &bool) -> bool {
    !value
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalRow {
    pub model: String,
    pub pool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityCooldownRow {
    pub model: String,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
    pub remaining_seconds: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowDisplay {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<crate::Headline>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRow {
    pub adapter: String,
    pub status: QuotaStatus,
    pub error: Option<ProbeFailure>,
    pub next_due_unix_seconds: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub total: u64,
    pub known: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaReport {
    pub schema_version: u64,
    pub generated_unix_seconds: i64,
    pub accounts: Vec<AccountRow>,
    pub sources: Vec<SourceRow>,
    pub coverage: Coverage,
    pub refreshing_ids: Vec<String>,
    pub next_due_unix_seconds: Option<i64>,
    pub report_path: String,
    #[serde(default)]
    pub display: crate::Display,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_icon_warning: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_account_accepts_future_fields_and_keeps_version_validation()
    -> Result<(), Box<dyn std::error::Error>> {
        let account = AccountScope {
            extra: Default::default(),
            adapter: "claude".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(QuotaSnapshot::test_snapshot(
            account.clone(),
            QuotaStatus::Available,
            10,
            vec![crate::QuotaWindow {
                extra: Default::default(),
                pool: "claude-5h".into(),
                window_seconds: Some(18_000),
                used_basis_points: 100,
                resets_unix_seconds: Some(100),
            }],
        ));
        record.snapshot.as_mut().unwrap().details = Some(crate::QuotaDetails::default());
        record.last_attempt_unix_seconds = Some(10);
        record.claim = Some(RefreshClaim {
            extra: Default::default(),
            token: "claim".into(),
            deadline_unix_seconds: 50,
        });
        record.auth_state = Some(AuthState::default());
        record.credential_metadata = vec![CredentialMetadata {
            extra: Default::default(),
            path: "/fixture/credential".into(),
            stat: Some(CredentialStat {
                extra: Default::default(),
                device: 1,
                inode: 2,
                mode: 0o600,
                uid: 3,
                size: 4,
                modified_ns: 5,
            }),
        }];
        record.refusals.push(crate::Refusal {
            extra: Default::default(),
            account: account.clone(),
            model: "claude/model".into(),
            pool: Some("claude-5h".into()),
            observed_unix_seconds: 10,
            resets_unix_seconds: Some(100),
        });
        record.capacity_cooldowns.push(crate::CapacityCooldown {
            extra: Default::default(),
            account,
            model: "claude/model".into(),
            observed_unix_seconds: 10,
            expires_unix_seconds: 100,
        });
        let mut value = serde_json::to_value(&record)?;
        value["record"] = serde_json::json!({"value": [1, 2]});
        value["account"]["account"] = serde_json::json!({"value": [1, 2]});
        value["snapshot"]["snapshot"] = serde_json::json!({"value": [1, 2]});
        value["snapshot"]["account"]["snapshot_account"] = serde_json::json!({"value": [1, 2]});
        value["snapshot"]["windows"][0]["window"] = serde_json::json!({"value": [1, 2]});
        value["snapshot"]["details"]["details"] = serde_json::json!({"value": [1, 2]});
        value["claim"]["claim"] = serde_json::json!({"value": [1, 2]});
        value["auth_state"]["auth_state"] = serde_json::json!({"value": [1, 2]});
        value["credential_metadata"][0]["credential_metadata"] =
            serde_json::json!({"value": [1, 2]});
        value["credential_metadata"][0]["stat"]["credential_stat"] =
            serde_json::json!({"value": [1, 2]});
        value["refusals"][0]["refusal"] = serde_json::json!({"value": [1, 2]});
        value["refusals"][0]["account"]["refusal_account"] = serde_json::json!({"value": [1, 2]});
        value["capacity_cooldowns"][0]["cooldown"] = serde_json::json!({"value": [1, 2]});
        value["capacity_cooldowns"][0]["account"]["cooldown_account"] =
            serde_json::json!({"value": [1, 2]});
        let bytes = serde_json::to_vec(&value)?;
        let decoded = AccountRecord::decode(&bytes)?;
        let encoded = provenant_records::encode(&decoded)?;
        let round_trip: AccountRecord =
            provenant_records::decode(&encoded, provenant_records::Limits::CONFIG)?;
        assert_eq!(serde_json::to_value(&round_trip)?, value);
        round_trip.validate()?;
        let observed = &round_trip.credential_metadata[0];
        let mut refreshed = observed.clone();
        refreshed
            .extra
            .insert("new_metadata".into(), serde_json::json!(true));
        refreshed.stat.as_mut().unwrap().size += 1;
        refreshed
            .stat
            .as_mut()
            .unwrap()
            .extra
            .insert("new_stat".into(), serde_json::json!(true));
        let mut same_observation = observed.clone();
        same_observation.extra.extend(refreshed.extra.clone());
        same_observation
            .stat
            .as_mut()
            .unwrap()
            .extra
            .extend(refreshed.stat.as_ref().unwrap().extra.clone());
        assert!(observed.same_observation(&same_observation));
        assert!(!observed.same_observation(&refreshed));
        let updated = observed.updated_from(&refreshed);
        let mut expected_extra = refreshed.extra.clone();
        for (key, value) in &observed.extra {
            expected_extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        let mut expected_stat_extra = refreshed.stat.as_ref().unwrap().extra.clone();
        for (key, value) in &observed.stat.as_ref().unwrap().extra {
            expected_stat_extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        assert_eq!(updated.extra, expected_extra);
        assert_eq!(updated.stat.unwrap().extra, expected_stat_extra);
        for &status in QuotaStatus::ALL {
            let mut row = round_trip.clone();
            row.snapshot.as_mut().unwrap().status = status;
            let encoded = provenant_records::encode(&row)?;
            let decoded = AccountRecord::decode(&encoded)?;
            assert_eq!(decoded.snapshot.unwrap().status, status);
        }
        value["schema_version"] = serde_json::json!(2);
        let future_version = serde_json::to_vec(&value)?;
        assert!(AccountRecord::decode(&future_version).is_err());
        Ok(())
    }

    #[test]
    fn account_record_round_trip_preserves_auth_failure_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut record = AccountRecord::empty(AccountScope {
            extra: Default::default(),
            adapter: "agy".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        });
        record.last_error = Some(ProbeFailure::Network);
        record.auth_state = Some(AuthState {
            signed_in: Some(true),
            observed_unix_seconds: Some(10),
            ..Default::default()
        });
        let bytes = provenant_records::encode(&record)?;
        let decoded = AccountRecord::decode(&bytes)?;
        assert_eq!(decoded, record);
        Ok(())
    }

    #[test]
    fn persisted_claim_cannot_exceed_the_tick_budget_or_exist_without_an_attempt() {
        let mut record = AccountRecord::empty(AccountScope {
            extra: Default::default(),
            adapter: "claude".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        });
        record.claim = Some(RefreshClaim {
            extra: Default::default(),
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
