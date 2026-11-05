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
    LoginExpired,
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

/// Host identity under which this machine publishes its own observations.
pub const LOCAL_HOST: &str = "local";
/// Route that stands for every route of an account.
pub const ACCOUNT_ROUTE: &str = "*";
/// State directory holding readiness, beside and never inside `quota/`:
/// older readers treat every JSON file under `quota/` as an account snapshot.
pub const READINESS_DIRECTORY: &str = "readiness";
pub const MAX_READINESS_RECORDS: usize = crate::MAX_WINDOWS;
const SCHEMA_VERSION: u32 = 1;
const QUOTA_PROBE: &str = "quota-probe";

/// Non-secret expiry metadata observed for this account's login. It is not
/// quota evidence and never creates or clears a provider refusal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginObservation {
    pub scope: ReadinessScope,
    pub observed_unix_seconds: i64,
    pub valid_until_unix_seconds: i64,
    pub expires_unix_seconds: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginExpiryState {
    Unknown,
    Fresh,
    Expiring,
    Expired,
}

impl LoginObservation {
    /// Stale, future and absent expiry metadata remain unknown.
    pub fn state(&self, now: i64, warning_seconds: u64) -> LoginExpiryState {
        if self.observed_unix_seconds > now
            || self.valid_until_unix_seconds <= self.observed_unix_seconds
            || now >= self.valid_until_unix_seconds
        {
            return LoginExpiryState::Unknown;
        }
        let Some(expiry) = self.expires_unix_seconds else {
            return LoginExpiryState::Unknown;
        };
        if jiff::Timestamp::from_second(expiry).is_err() {
            return LoginExpiryState::Unknown;
        }
        if expiry <= now {
            LoginExpiryState::Expired
        } else if warning_seconds > 0
            && expiry.saturating_sub(now) <= i64::try_from(warning_seconds).unwrap_or(i64::MAX)
        {
            LoginExpiryState::Expiring
        } else {
            LoginExpiryState::Fresh
        }
    }
}

/// What a reader learns for one route: a verdict that is never ready by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessRead {
    pub state: ReadinessState,
    pub reason: Option<ReadinessReason>,
    /// Time of the matching, valid scoped evidence, including stale observations.
    pub observed_unix_seconds: Option<i64>,
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
            observed_unix_seconds: None,
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
                    Some(ReadinessReason::LoginExpired) => Excluded::LoginExpired,
                    Some(ReadinessReason::AccountMismatch) => Excluded::AccountMismatch,
                    Some(ReadinessReason::Exhausted | ReadinessReason::ProviderRefusal) => {
                        Excluded::ProviderRefused
                    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<LoginObservation>,
}

impl Default for ReadinessFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            records: Vec::new(),
            login: None,
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
            || self.login.as_ref().is_some_and(|login| {
                login.scope.adapter != adapter
                    || login.scope.account_key != account_key
                    || login.scope.host.is_empty()
                    || login.scope.host.len() > 128
                    || login.scope.route != ACCOUNT_ROUTE
            })
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
            let Some(r) = self.records.iter().find(|r| r.scope == *scope) else {
                return ReadinessRead::absent();
            };
            if self.schema_version != SCHEMA_VERSION
                || r.schema_version != SCHEMA_VERSION
                || r.observed_unix_seconds > now
                || r.expires_unix_seconds <= r.observed_unix_seconds
                || r.evidence_ref
                    .as_ref()
                    .is_some_and(|r| !crate::state::reference(r))
            {
                return ReadinessRead::unknown(ReadinessReason::InvalidEvidence);
            }
            if now >= r.expires_unix_seconds {
                return ReadinessRead {
                    observed_unix_seconds: Some(r.observed_unix_seconds),
                    expires_unix_seconds: Some(r.expires_unix_seconds),
                    ..ReadinessRead::unknown(ReadinessReason::Stale)
                };
            }
            if r.state == ReadinessState::Unknown {
                return ReadinessRead {
                    observed_unix_seconds: Some(r.observed_unix_seconds),
                    expires_unix_seconds: Some(r.expires_unix_seconds),
                    ..ReadinessRead::unknown(r.reason.unwrap_or(ReadinessReason::NotProbed))
                };
            }
            ReadinessRead {
                state: r.state,
                reason: r.reason,
                observed_unix_seconds: Some(r.observed_unix_seconds),
                expires_unix_seconds: Some(r.expires_unix_seconds),
                reset_unix_seconds: r.reset_unix_seconds,
                evidence_ref: r.evidence_ref.clone(),
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
        let scoped = read(scope);
        if scoped.state == ReadinessState::Unavailable {
            return scoped;
        }
        if let Some(login) = &self.login
            && login.scope.host == scope.host
            && login.scope.adapter == scope.adapter
            && login.scope.account_key == scope.account_key
            && login.state(now, 0) == LoginExpiryState::Expired
        {
            return ReadinessRead {
                state: ReadinessState::Unavailable,
                reason: Some(ReadinessReason::LoginExpired),
                observed_unix_seconds: Some(login.observed_unix_seconds),
                expires_unix_seconds: Some(login.valid_until_unix_seconds),
                reset_unix_seconds: None,
                evidence_ref: Some("login-expiry".into()),
            };
        }
        scoped
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
        let scope = ReadinessScope {
            host: host.into(),
            adapter: account.account.adapter.clone(),
            account_key: account.account.account_key.clone(),
            route: ACCOUNT_ROUTE.into(),
        };
        // The account-wide failure supersedes this host's per-route facts.
        // The account record remains the source for restoring held refusals.
        let account_read = self.get(&scope, now);
        if account_read.state == ReadinessState::Unavailable
            && account_read.evidence_ref.as_deref() == Some(QUOTA_PROBE)
        {
            return self.check_bound();
        }
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
        self.records
            .retain(|r| !published.iter().any(|new| new.scope == r.scope));
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
            record.expires_unix_seconds > now
                && !(record.scope.host == host
                    && record.scope.adapter == account.account.adapter
                    && record.scope.account_key == account.account.account_key
                    && (reason.is_some()
                        || (record.scope.route == ACCOUNT_ROUTE
                            && record.evidence_ref.as_deref() == Some(QUOTA_PROBE))))
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
                evidence_ref: Some(QUOTA_PROBE.into()),
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
    fn login_expiry_uses_fixed_clock_threshold_and_freshness() {
        let mut login = LoginObservation {
            scope: ReadinessScope {
                host: LOCAL_HOST.into(),
                adapter: "claude".into(),
                account_key: "key".into(),
                route: ACCOUNT_ROUTE.into(),
            },
            observed_unix_seconds: 100,
            valid_until_unix_seconds: 1000,
            expires_unix_seconds: Some(500),
        };
        assert_eq!(login.state(200, 299), LoginExpiryState::Fresh);
        assert_eq!(login.state(200, 300), LoginExpiryState::Expiring);
        assert_eq!(login.state(200, 0), LoginExpiryState::Fresh);
        assert_eq!(login.state(500, 300), LoginExpiryState::Expired);
        assert_eq!(login.state(99, 300), LoginExpiryState::Unknown);
        assert_eq!(login.state(1000, 300), LoginExpiryState::Unknown);
        login.expires_unix_seconds = None;
        assert_eq!(login.state(200, 300), LoginExpiryState::Unknown);
        login.expires_unix_seconds = Some(800);
        assert_eq!(login.state(200, 300), LoginExpiryState::Fresh);
        login.expires_unix_seconds = Some(i64::MAX);
        assert_eq!(login.state(200, 300), LoginExpiryState::Unknown);
    }

    #[test]
    fn fresh_expiry_excludes_only_after_expiration_and_expires_with_evidence() {
        let file = ReadinessFile {
            login: Some(LoginObservation {
                scope: ReadinessScope {
                    host: LOCAL_HOST.into(),
                    adapter: "claude".into(),
                    account_key: "key".into(),
                    route: ACCOUNT_ROUTE.into(),
                },
                observed_unix_seconds: 100,
                valid_until_unix_seconds: 300,
                expires_unix_seconds: Some(200),
            }),
            ..Default::default()
        };
        let route = ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: "claude/model".into(),
        };
        assert_eq!(
            file.get(&route, 199).reason,
            Some(ReadinessReason::NotProbed)
        );
        let expired = file.get(&route, 200);
        assert_eq!(expired.state, ReadinessState::Unavailable);
        assert_eq!(expired.reason, Some(ReadinessReason::LoginExpired));
        assert!(matches!(
            expired.project(),
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::LoginExpired,
                ..
            }
        ));
        assert_eq!(
            file.get(&route, 300).reason,
            Some(ReadinessReason::NotProbed)
        );
        let other = ReadinessScope {
            account_key: "other".into(),
            ..route.clone()
        };
        assert_eq!(
            file.get(&other, 200).reason,
            Some(ReadinessReason::NotProbed)
        );
        let remote = ReadinessScope {
            host: "other-host".into(),
            ..route
        };
        assert_eq!(
            file.get(&remote, 200).reason,
            Some(ReadinessReason::NotProbed)
        );
    }

    #[test]
    fn login_failure_expires_at_the_probe_interval() {
        let account_scope = crate::AccountScope {
            adapter: "claude".into(),
            account_key: "key".into(),
            label: "work".into(),
        };
        let mut account = crate::AccountRecord::empty(account_scope);
        account.backoff_until_unix_seconds = Some(500);
        let mut file = ReadinessFile::default();
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            100,
            60,
            Some(ReadinessReason::LoginExpired),
        )
        .unwrap();
        let scope = ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: "claude/model".into(),
        };
        assert_eq!(
            file.get(&scope, 159).reason,
            Some(ReadinessReason::LoginExpired)
        );
        let account_scope = ReadinessScope {
            route: ACCOUNT_ROUTE.into(),
            ..scope.clone()
        };
        assert_eq!(
            file.get(&account_scope, 160).reason,
            Some(ReadinessReason::Stale)
        );
        file.sync_probe_result(&account, LOCAL_HOST, 501, 60, None)
            .unwrap();
        assert_eq!(
            file.get(&scope, 501).reason,
            Some(ReadinessReason::NotProbed)
        );
    }
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
        let mut file = ReadinessFile {
            records: vec![decoded],
            ..Default::default()
        };
        for (now, reason) in [
            (9, Some(ReadinessReason::InvalidEvidence)),
            (10, None),
            (15, None),
            (19, None),
            (20, Some(ReadinessReason::Stale)),
        ] {
            let read = file.get(&scope, now);
            if let Some(reason) = reason {
                assert_eq!(read.state, ReadinessState::Unknown);
                assert_eq!(read.reason, Some(reason));
                assert_eq!(
                    read.observed_unix_seconds,
                    (reason == ReadinessReason::Stale).then_some(10)
                );
                if reason == ReadinessReason::Stale {
                    assert_eq!(read.project(), provenant_records::RouteReadiness::stale());
                }
            } else {
                assert_eq!(read.state, ReadinessState::Unavailable);
                assert_eq!(read.observed_unix_seconds, Some(10));
            }
        }
        let mut other = scope.clone();
        other.route = "other".into();
        assert_eq!(file.get(&other, 15), ReadinessRead::absent());
        for evidence in ["".into(), "x".repeat(257), "bad/path".into()] {
            file.records[0].evidence_ref = Some(evidence);
            assert_eq!(
                file.get(&scope, 15),
                ReadinessRead::unknown(ReadinessReason::InvalidEvidence)
            );
        }
        file.records[0].evidence_ref = Some("x".repeat(256));
        assert_eq!(file.get(&scope, 15).state, ReadinessState::Unavailable);
        file.records[0].expires_unix_seconds = 10;
        assert_eq!(
            file.get(&scope, 15),
            ReadinessRead::unknown(ReadinessReason::InvalidEvidence)
        );
        file.records[0].expires_unix_seconds = 20;
        file.records[0].schema_version = 2;
        assert_eq!(
            file.get(&scope, 15),
            ReadinessRead::unknown(ReadinessReason::InvalidEvidence)
        );
    }

    #[test]
    fn readiness_ready_evidence_requires_fresh_matching_scope() {
        let scope = ReadinessScope {
            host: "host".into(),
            adapter: "fixture".into(),
            account_key: "account".into(),
            route: "model".into(),
        };
        let file = ReadinessFile {
            records: vec![RouteReadiness {
                schema_version: 1,
                scope: scope.clone(),
                state: ReadinessState::Ready,
                reason: None,
                observed_unix_seconds: 10,
                expires_unix_seconds: 20,
                reset_unix_seconds: None,
                evidence_ref: Some("probe:1".into()),
            }],
            ..Default::default()
        };
        assert_eq!(file.get(&scope, 10).state, ReadinessState::Ready);
        assert_eq!(file.get(&scope, 10).observed_unix_seconds, Some(10));
        assert_eq!(file.get(&scope, 9).observed_unix_seconds, None);
        assert_eq!(file.get(&scope, 20).observed_unix_seconds, Some(10));
        assert_eq!(file.get(&scope, 9).state, ReadinessState::Unknown);
        assert_eq!(file.get(&scope, 20).state, ReadinessState::Unknown);
        for other in [
            ReadinessScope {
                host: "other".into(),
                ..scope.clone()
            },
            ReadinessScope {
                adapter: "other".into(),
                ..scope.clone()
            },
            ReadinessScope {
                account_key: "other".into(),
                ..scope.clone()
            },
            ReadinessScope {
                route: "other".into(),
                ..scope.clone()
            },
        ] {
            assert_eq!(file.get(&other, 10).state, ReadinessState::Unknown);
        }
    }

    #[test]
    fn receipt_projection_covers_every_reason_and_ready() {
        use ReadinessReason::*;
        use provenant_records::{RouteExclusionReason as E, RouteReadiness as R};
        for (reason, expected) in [
            (NotProbed, E::UnknownAvailability),
            (Stale, E::UnknownAvailability),
            (ProbeTimeout, E::UnknownAvailability),
            (CommandFailure, E::UnknownAvailability),
            (MissingExecutable, E::MissingExecutable),
            (SignedOut, E::AuthRequired),
            (LoginExpired, E::LoginExpired),
            (AccountMismatch, E::AccountMismatch),
            (Exhausted, E::ProviderRefused),
            (ProviderRefusal, E::ProviderRefused),
            (UncalibratedPartialCoverage, E::UnknownAvailability),
            (InvalidEvidence, E::UnknownAvailability),
            (UnreachablePeer, E::UnknownAvailability),
            (UnsupportedPeerVersion, E::UnknownAvailability),
        ] {
            let mut read = ReadinessRead::unknown(reason);
            read.state = ReadinessState::Unavailable;
            assert!(
                matches!(read.project(), R::Unavailable { reason: actual, .. } if actual == expected),
                "{reason:?}"
            );
            let expected = if reason == Stale {
                E::StaleAvailability
            } else {
                E::UnknownAvailability
            };
            assert!(
                matches!(ReadinessRead::unknown(reason).project(), R::Unavailable { reason: actual, .. } if actual == expected)
            );
        }
        let mut ready = ReadinessRead::absent();
        ready.state = ReadinessState::Ready;
        assert!(ready.project().is_unknown());
        ready.evidence_ref = Some("probe:1".into());
        ready.expires_unix_seconds = Some(20);
        assert!(
            matches!(ready.project(), R::Ready { availability_ref, expires_unix_seconds: 20, .. } if availability_ref == "probe:1")
        );
    }

    #[test]
    fn full_refusal_file_accepts_probe_failure_and_restores_refusals() {
        let mut account = crate::AccountRecord::empty(account());
        account.refusals = (0..MAX_READINESS_RECORDS)
            .map(|i| refused(Some(&format!("pool-{i}")), "m", Some(500)))
            .collect();
        let mut file = ReadinessFile::default();
        file.sync_refusals(&account, LOCAL_HOST, &at(&account, 150))
            .unwrap();
        assert_eq!(file.records.len(), MAX_READINESS_RECORDS);
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            150,
            300,
            Some(ReadinessReason::ProbeTimeout),
        )
        .unwrap();
        file = ReadinessFile::decode(&serde_json::to_vec(&file).unwrap(), "claude", "key").unwrap();
        assert_eq!(
            file.get(&route("pool-0"), 150).reason,
            Some(ReadinessReason::ProbeTimeout)
        );
        file.sync_refusals(&account, LOCAL_HOST, &at(&account, 160))
            .unwrap();
        assert!(file.records.len() <= MAX_READINESS_RECORDS);
        file.sync_probe_result(&account, LOCAL_HOST, 170, 300, None)
            .unwrap();
        file.sync_refusals(&account, LOCAL_HOST, &at(&account, 170))
            .unwrap();
        assert_eq!(file.records.len(), MAX_READINESS_RECORDS);
        assert_eq!(
            file.get(&route("pool-0"), 170).reason,
            Some(ReadinessReason::ProviderRefusal)
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
        let ready = RouteReadiness {
            schema_version: 1,
            scope: route("pool"),
            state: ReadinessState::Ready,
            reason: None,
            observed_unix_seconds: 100,
            expires_unix_seconds: 500,
            reset_unix_seconds: None,
            evidence_ref: Some("fixture".into()),
        };
        let mut retained = ready.clone();
        retained.scope.route = "retained".into();
        let mut peer = ready.clone();
        peer.scope.host = "peer".into();
        peer.state = ReadinessState::Unavailable;
        peer.reason = Some(ReadinessReason::ProviderRefusal);
        let mut probe = ready.clone();
        probe.scope.route = ACCOUNT_ROUTE.into();
        probe.state = ReadinessState::Unknown;
        probe.reason = Some(ReadinessReason::CommandFailure);
        probe.evidence_ref = Some("quota-probe".into());
        file.records = vec![ready, retained.clone(), peer.clone(), probe.clone()];
        file.sync_refusals(&record, LOCAL_HOST, &at(&record, 150))
            .unwrap();
        assert_eq!(
            file.get(&route("pool"), 150).state,
            ReadinessState::Unavailable
        );
        for observation in [retained, peer, probe] {
            assert!(file.records.contains(&observation));
        }
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
        // The file refuses another account's records.
        assert!(ReadinessFile::decode(&bytes, "claude", "other").is_err());
    }
}
