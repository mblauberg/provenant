//! Typed readiness observations and their freshness boundary.
use crate::QuotaError;
use serde::{Deserialize, Serialize};

/// Identity of the route whose readiness was observed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReadinessScope {
    pub host: String,
    pub adapter: String,
    pub account_key: String,
    /// Canonical model or verified pool identifier.
    pub route: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Ready,
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessReason {
    NotProbed,
    Stale,
    ProbeTimeout,
    CommandFailure,
    MissingExecutable,
    SignedOut,
    AccountMismatch,
    Exhausted,
    ProviderRefusal,
    UncalibratedPartialCoverage,
    InvalidEvidence,
    UnreachablePeer,
    UnsupportedPeerVersion,
}

/// An observation is valid only for its exact scope and before its expiry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteReadiness {
    pub schema_version: u32,
    pub scope: ReadinessScope,
    pub state: ReadinessState,
    pub reason: Option<ReadinessReason>,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
    pub reset_unix_seconds: Option<i64>,
    pub evidence_ref: Option<String>,
}

impl RouteReadiness {
    /// Read an observation; absence, invalid scope and stale data are unknown.
    /// A reset does not change the observation's state.
    pub fn read(
        record: Option<&Self>,
        scope: &ReadinessScope,
        now: i64,
    ) -> (ReadinessState, Option<ReadinessReason>) {
        let Some(record) = record else {
            return (ReadinessState::Unknown, Some(ReadinessReason::NotProbed));
        };
        if record.schema_version != 1
            || record.scope != *scope
            || record.observed_unix_seconds > now
            || record.expires_unix_seconds <= record.observed_unix_seconds
            || record.evidence_ref.as_ref().is_some_and(|r| {
                r.is_empty()
                    || r.len() > 256
                    || !r
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            })
        {
            return (
                ReadinessState::Unknown,
                Some(ReadinessReason::InvalidEvidence),
            );
        }
        if now >= record.expires_unix_seconds {
            return (ReadinessState::Unknown, Some(ReadinessReason::Stale));
        }
        (record.state, record.reason)
    }
}

/// Host identity under which this machine publishes its own observations.
pub const LOCAL_HOST: &str = "local";
/// Route that stands for every route of an account.
pub const ACCOUNT_ROUTE: &str = "*";
/// State directory holding readiness, beside and never inside `quota/`:
/// older readers treat every JSON file under `quota/` as an account snapshot.
pub const READINESS_DIRECTORY: &str = "readiness";
pub const MAX_READINESS_RECORDS: usize = crate::MAX_WINDOWS;
const SCHEMA_VERSION: u32 = 1;

/// What a reader learns for one route: a verdict that is never ready by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessRead {
    pub state: ReadinessState,
    pub reason: Option<ReadinessReason>,
    pub expires_unix_seconds: Option<i64>,
    pub reset_unix_seconds: Option<i64>,
    pub evidence_ref: Option<String>,
}

impl ReadinessRead {
    /// Nothing is recorded for the route.
    pub fn absent() -> Self {
        Self::unknown(ReadinessReason::NotProbed)
    }
    /// Unknown, for the given reason.
    pub fn unknown(reason: ReadinessReason) -> Self {
        Self {
            state: ReadinessState::Unknown,
            reason: Some(reason),
            expires_unix_seconds: None,
            reset_unix_seconds: None,
            evidence_ref: None,
        }
    }
    /// Project onto the receipt's existing statuses. Unknown has no status of
    /// its own there (an older reader rejects one it does not know), so it
    /// reads as the established unknown or stale availability exclusion.
    pub fn project(&self) -> provenant_records::RouteReadiness {
        use provenant_records::{RouteExclusionReason as Excluded, RouteReadiness as Receipt};
        match (self.state, self.reason) {
            (ReadinessState::Ready, _) => match (&self.evidence_ref, self.expires_unix_seconds) {
                (Some(availability_ref), Some(expires_unix_seconds)) => Receipt::Ready {
                    extra: Default::default(),
                    availability_ref: availability_ref.clone(),
                    expires_unix_seconds,
                },
                _ => Receipt::unknown(),
            },
            (ReadinessState::Unavailable, reason) => Receipt::Unavailable {
                extra: Default::default(),
                reason: match reason {
                    Some(ReadinessReason::MissingExecutable) => Excluded::MissingExecutable,
                    Some(ReadinessReason::SignedOut) => Excluded::AuthRequired,
                    Some(ReadinessReason::AccountMismatch) => Excluded::AccountMismatch,
                    Some(ReadinessReason::Exhausted) => Excluded::Exhausted,
                    Some(ReadinessReason::ProviderRefusal) => Excluded::ProviderRefused,
                    _ => Excluded::UnknownAvailability,
                },
            },
            (ReadinessState::Unknown, Some(ReadinessReason::Stale)) => Receipt::stale(),
            (ReadinessState::Unknown, _) => Receipt::unknown(),
        }
    }
}

/// Every observation for one account and adapter, stored at
/// `<state>/readiness/<account-key>/<adapter>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessFile {
    pub schema_version: u32,
    pub records: Vec<RouteReadiness>,
}

impl Default for ReadinessFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

impl ReadinessFile {
    /// Decode a stored file whose path names `adapter` and `account_key`.
    pub fn decode(bytes: &[u8], adapter: &str, account_key: &str) -> Result<Self, QuotaError> {
        let file: Self =
            serde_json::from_slice(bytes).map_err(|_| QuotaError::Invalid("readiness record"))?;
        file.validate(adapter, account_key)?;
        Ok(file)
    }

    fn validate(&self, adapter: &str, account_key: &str) -> Result<(), QuotaError> {
        let mut seen = std::collections::BTreeSet::new();
        if self.schema_version != SCHEMA_VERSION
            || self.records.len() > MAX_READINESS_RECORDS
            || self.records.iter().any(|r| {
                r.scope.adapter != adapter
                    || r.scope.account_key != account_key
                    || r.scope.host.is_empty()
                    || r.scope.host.len() > 128
                    || r.scope.route.is_empty()
                    || r.scope.route.len() > 512
                    || r.scope.route.chars().any(char::is_control)
                    || !seen.insert(&r.scope)
            })
        {
            return Err(QuotaError::Invalid("readiness bounds"));
        }
        Ok(())
    }

    /// The readiness of one route. An account-wide unavailable observation
    /// covers every route of that account; any other gap reads as unknown.
    pub fn get(&self, scope: &ReadinessScope, now: i64) -> ReadinessRead {
        let read = |scope: &ReadinessScope| {
            let record = self.records.iter().find(|r| r.scope == *scope);
            let (state, reason) = RouteReadiness::read(record, scope, now);
            match record.filter(|_| state != ReadinessState::Unknown) {
                Some(r) => ReadinessRead {
                    state,
                    reason,
                    expires_unix_seconds: Some(r.expires_unix_seconds),
                    reset_unix_seconds: r.reset_unix_seconds,
                    evidence_ref: r.evidence_ref.clone(),
                },
                None => ReadinessRead::unknown(reason.unwrap_or(ReadinessReason::NotProbed)),
            }
        };
        if scope.route != ACCOUNT_ROUTE {
            let account = read(&ReadinessScope {
                route: ACCOUNT_ROUTE.into(),
                ..scope.clone()
            });
            if account.state == ReadinessState::Unavailable {
                return account;
            }
        }
        read(scope)
    }

    /// Publish an observation, replacing the one for its scope and dropping
    /// any that have expired.
    pub fn publish(&mut self, record: RouteReadiness, now: i64) -> Result<(), QuotaError> {
        self.records
            .retain(|r| r.scope != record.scope && r.expires_unix_seconds > now);
        self.records.push(record);
        self.check_bound()
    }

    /// Fresh comparable evidence clears an observation for exactly its scope.
    pub fn clear(&mut self, scope: &ReadinessScope) {
        self.records.retain(|r| r.scope != *scope);
    }

    /// Make this host's provider-refusal observations exactly the refusals
    /// that still hold for `account`: published when recorded, and cleared by
    /// the rule admission uses (expiry, or a fresh reading of the pool that
    /// proves headroom). A pool refusal covers only that pool, and
    /// a refusal without a pool only its model. Other observations stay.
    pub fn sync_refusals(
        &mut self,
        account: &crate::AccountRecord,
        host: &str,
        admission: &crate::Admission<'_>,
    ) -> Result<(), QuotaError> {
        let now = admission.now_unix_seconds;
        self.records.retain(|r| {
            r.expires_unix_seconds > now
                && !(r.scope.host == host
                    && r.state == ReadinessState::Unavailable
                    && r.reason == Some(ReadinessReason::ProviderRefusal))
        });
        let mut published: Vec<RouteReadiness> = Vec::new();
        for refusal in &account.refusals {
            if !admission.refusal_holds(&account.account.adapter, Some(account), refusal)? {
                continue;
            }
            let expires = refusal.expires_unix_seconds()?;
            let scope = ReadinessScope {
                host: host.into(),
                adapter: account.account.adapter.clone(),
                account_key: account.account.account_key.clone(),
                route: refusal
                    .pool
                    .clone()
                    .unwrap_or_else(|| format!("{}/{}", account.account.adapter, refusal.model)),
            };
            if let Some(held) = published.iter_mut().find(|r| r.scope == scope) {
                held.observed_unix_seconds = held
                    .observed_unix_seconds
                    .max(refusal.observed_unix_seconds);
                held.expires_unix_seconds = held.expires_unix_seconds.max(expires);
                held.reset_unix_seconds = held.reset_unix_seconds.max(refusal.resets_unix_seconds);
                continue;
            }
            published.push(RouteReadiness {
                schema_version: SCHEMA_VERSION,
                scope,
                state: ReadinessState::Unavailable,
                reason: Some(ReadinessReason::ProviderRefusal),
                observed_unix_seconds: refusal.observed_unix_seconds,
                expires_unix_seconds: expires,
                reset_unix_seconds: refusal.resets_unix_seconds,
                evidence_ref: Some("provider-refusal".into()),
            });
        }
        self.records.extend(published);
        self.check_bound()
    }

    /// Replace the account-wide probe observation after a completed attempt.
    /// A fresh success clears the previous failure observation.
    pub fn sync_probe_result(
        &mut self,
        account: &crate::AccountRecord,
        host: &str,
        now: i64,
        interval_seconds: u64,
        reason: Option<ReadinessReason>,
    ) -> Result<(), QuotaError> {
        self.records.retain(|record| {
            !(record.scope.host == host
                && record.scope.route == ACCOUNT_ROUTE
                && record.evidence_ref.as_deref() == Some("quota-probe"))
        });
        if let Some(reason) = reason {
            let interval = i64::try_from(interval_seconds).unwrap_or(i64::MAX);
            let expires = now.saturating_add(interval.max(1));
            self.records.push(RouteReadiness {
                schema_version: SCHEMA_VERSION,
                scope: ReadinessScope {
                    host: host.into(),
                    adapter: account.account.adapter.clone(),
                    account_key: account.account.account_key.clone(),
                    route: ACCOUNT_ROUTE.into(),
                },
                state: ReadinessState::Unavailable,
                reason: Some(reason),
                observed_unix_seconds: now,
                expires_unix_seconds: expires,
                reset_unix_seconds: None,
                evidence_ref: Some("quota-probe".into()),
            });
        }
        self.check_bound()
    }

    fn check_bound(&self) -> Result<(), QuotaError> {
        if self.records.len() > MAX_READINESS_RECORDS {
            return Err(QuotaError::Invalid("readiness bounds"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_round_trip_and_boundaries() {
        let scope = ReadinessScope {
            host: "host".into(),
            adapter: "agy".into(),
            account_key: "account".into(),
            route: "model".into(),
        };
        let record = RouteReadiness {
            schema_version: 1,
            scope: scope.clone(),
            state: ReadinessState::Unavailable,
            reason: Some(ReadinessReason::ProviderRefusal),
            observed_unix_seconds: 10,
            expires_unix_seconds: 20,
            reset_unix_seconds: Some(15),
            evidence_ref: Some("probe:1".into()),
        };
        let decoded: RouteReadiness =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(record, decoded);
        assert_eq!(
            RouteReadiness::read(None, &scope, 10).0,
            ReadinessState::Unknown
        );
        assert_eq!(
            RouteReadiness::read(Some(&record), &scope, 9).0,
            ReadinessState::Unknown
        );
        assert_eq!(
            RouteReadiness::read(Some(&record), &scope, 15).0,
            ReadinessState::Unavailable
        );
        assert_eq!(
            RouteReadiness::read(Some(&record), &scope, 20).0,
            ReadinessState::Unknown
        );
        let mut other = scope.clone();
        other.route = "other".into();
        assert_eq!(
            RouteReadiness::read(Some(&record), &other, 15).0,
            ReadinessState::Unknown
        );
    }

    fn refused(pool: Option<&str>, model: &str, resets: Option<i64>) -> crate::Refusal {
        crate::Refusal {
            extra: Default::default(),
            account: account(),
            model: model.into(),
            pool: pool.map(Into::into),
            observed_unix_seconds: 100,
            resets_unix_seconds: resets,
        }
    }
    fn account() -> crate::AccountScope {
        crate::AccountScope {
            adapter: "claude".into(),
            account_key: "key".into(),
            label: "label".into(),
        }
    }
    fn at(record: &crate::AccountRecord, now: i64) -> crate::Admission<'_> {
        crate::Admission {
            policies: crate::source::TEST_POLICIES,
            now_unix_seconds: now,
            records: std::slice::from_ref(record),
            pools: &[],
        }
    }
    fn route(route: &str) -> ReadinessScope {
        ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: route.into(),
        }
    }

    #[test]
    fn refusals_publish_by_scope_and_clear_with_the_account_record() {
        let mut file = ReadinessFile::default();
        let mut record = crate::AccountRecord::empty(account());
        // Nothing recorded reads as unknown, never ready.
        assert_eq!(file.get(&route("claude/m"), 150), ReadinessRead::absent());
        record.refusals = vec![
            refused(Some("pool"), "m", Some(500)),
            refused(None, "other", None),
        ];
        file.sync_refusals(&record, LOCAL_HOST, &at(&record, 150))
            .unwrap();
        let bytes = serde_json::to_vec(&file).unwrap();
        file = ReadinessFile::decode(&bytes, "claude", "key").unwrap();
        // A pool refusal covers its pool only; a model refusal only its model.
        let pool = file.get(&route("pool"), 150);
        assert_eq!(pool.state, ReadinessState::Unavailable);
        assert_eq!(pool.reason, Some(ReadinessReason::ProviderRefusal));
        assert_eq!(pool.reset_unix_seconds, Some(500));
        assert!(!pool.project().is_unknown());
        assert_eq!(
            file.get(&route("claude/m"), 150).state,
            ReadinessState::Unknown
        );
        assert_eq!(
            file.get(&route("claude/other"), 150).expires_unix_seconds,
            Some(3700)
        );
        // Expiry reads as unknown and the reset alone never makes it ready.
        let lapsed = file.get(&route("pool"), 500);
        assert_eq!(lapsed.state, ReadinessState::Unknown);
        assert!(lapsed.project().is_unknown());
        // Another account's identity is not covered.
        let mut elsewhere = route("pool");
        elsewhere.account_key = "other".into();
        assert_eq!(file.get(&elsewhere, 150).state, ReadinessState::Unknown);
        // A fresh reading with headroom in the pool clears only that pool.
        record.snapshot = Some(crate::QuotaSnapshot {
            schema_version: 1,
            account: account(),
            revision: 0,
            status: crate::QuotaStatus::Available,
            observed_unix_seconds: 190,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![crate::QuotaWindow {
                window_seconds: None,
                pool: "pool".into(),
                used_basis_points: 1000,
                resets_unix_seconds: Some(900),
            }],
        });
        file.sync_refusals(&record, LOCAL_HOST, &at(&record, 200))
            .unwrap();
        assert_eq!(file.get(&route("pool"), 200).state, ReadinessState::Unknown);
        assert_eq!(
            file.get(&route("claude/other"), 200).state,
            ReadinessState::Unavailable
        );
        // An account-wide observation covers every route of the account.
        let mut wide = file.records[0].clone();
        wide.scope.route = ACCOUNT_ROUTE.into();
        wide.reason = Some(ReadinessReason::Exhausted);
        file.publish(wide, 150).unwrap();
        let covered = file.get(&route("claude/any"), 150);
        assert_eq!(covered.reason, Some(ReadinessReason::Exhausted));
        file.clear(&route(ACCOUNT_ROUTE));
        assert_eq!(
            file.get(&route("claude/any"), 150).state,
            ReadinessState::Unknown
        );
        // The file refuses another account's records.
        assert!(ReadinessFile::decode(&bytes, "claude", "other").is_err());
    }
}
