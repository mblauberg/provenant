//! Typed readiness observations and their freshness boundary.
use crate::QuotaError;
use provenant_records::ExtraFields;
use serde::{Deserialize, Serialize};

/// The scope an observation covers. Model routes carry the canonical
/// `adapter/model` id; pool routes carry a verified pool name. The persisted
/// shape is a tagged map with readable names, never an ambiguous bare string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ReadinessRoute {
    Account,
    Model(String),
    Pool(String),
}
impl ReadinessRoute {
    /// Every route kind, for exhaustive stored-format tables.
    pub const ALL: &[Self] = &[
        Self::Account,
        Self::Model(String::new()),
        Self::Pool(String::new()),
    ];
    /// The account-wide route.
    pub fn account() -> Self {
        Self::Account
    }
    /// A canonical model id, `adapter/model`.
    pub fn model(adapter: &str, model: &str) -> Self {
        Self::Model(format!("{adapter}/{model}"))
    }
    /// A verified pool name.
    pub fn pool(name: impl Into<String>) -> Self {
        Self::Pool(name.into())
    }
    /// Stable readable text for reports and diagnostics.
    pub fn as_text(&self) -> &str {
        match self {
            Self::Account => ACCOUNT_ROUTE,
            Self::Model(model) | Self::Pool(model) => model,
        }
    }
    /// The model id when this scope names one.
    pub fn as_model(&self) -> Option<&str> {
        match self {
            Self::Model(model) => Some(model),
            _ => None,
        }
    }
    /// Classify opaque route text by shape. Callers with a known model or pool
    /// should use [`Self::model`] or [`Self::pool`] instead.
    pub fn from_text(text: &str) -> Self {
        if text == ACCOUNT_ROUTE {
            Self::Account
        } else if text.contains('/') {
            Self::Model(text.into())
        } else {
            Self::Pool(text.into())
        }
    }
    /// Bounds and control characters of the carried identifier.
    pub fn valid(&self) -> bool {
        match self {
            Self::Account => true,
            Self::Model(model) | Self::Pool(model) => {
                !model.is_empty() && model.len() <= 512 && !model.chars().any(char::is_control)
            }
        }
    }
    /// Uniqueness key including the route kind.
    pub fn key(&self) -> (u8, &str) {
        match self {
            Self::Account => (0, ACCOUNT_ROUTE),
            Self::Model(model) => (1, model),
            Self::Pool(pool) => (2, pool),
        }
    }
}

/// Identity of the route whose readiness was observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessScope {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub host: String,
    pub adapter: String,
    pub account_key: String,
    pub route: ReadinessRoute,
}

/// Build the account-wide scope for an account on this host.
pub fn account_scope(host: &str, adapter: &str, account_key: &str) -> ReadinessScope {
    ReadinessScope {
        extra: Default::default(),
        host: host.into(),
        adapter: adapter.into(),
        account_key: account_key.into(),
        route: ReadinessRoute::Account,
    }
}

impl ReadinessScope {
    fn same_scope(&self, other: &Self) -> bool {
        self.host == other.host
            && self.adapter == other.adapter
            && self.account_key == other.account_key
            && self.route == other.route
    }

    fn updated_from(&self, current: &Self) -> Self {
        let mut updated = current.clone();
        for (key, value) in &self.extra {
            updated
                .extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        updated
    }
}

macro_rules! declare_readiness_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            /// Every declared variant, for exhaustive stored-format tables.
            pub const ALL: &[Self] = &[$(Self::$variant),+];
        }
    };
}

declare_readiness_enum! {
    ReadinessState { Ready, Unavailable, Unknown }
}

declare_readiness_enum! {
    ReadinessReason {
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
        ProbedNoFigures,
    }
}

impl ReadinessReason {
    /// Authentication refusals survive time alone; other availability expires.
    pub fn is_authentication_failure(self) -> bool {
        matches!(
            self,
            Self::SignedOut | Self::LoginExpired | Self::AccountMismatch
        )
    }
}

/// An observation is valid only for its exact scope and before its expiry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteReadiness {
    #[serde(flatten)]
    pub extra: ExtraFields,
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
    /// Authentication failures require recovery evidence, not just elapsed time.
    pub fn authentication_failure(&self) -> bool {
        self.scope.route == ReadinessRoute::Account
            && self.state == ReadinessState::Unavailable
            && self
                .reason
                .is_some_and(ReadinessReason::is_authentication_failure)
    }

    // A provider without a login observation must eventually be retried. An
    // identity mismatch still needs positive identity evidence to clear it.
    fn holds_after_expiry(&self) -> bool {
        self.authentication_failure()
            && (self.reason == Some(ReadinessReason::AccountMismatch)
                || !provenant_records::AdapterSpec::get(&self.scope.adapter).is_some_and(|spec| {
                    matches!(
                        spec.login.observation,
                        provenant_records::LoginObservation::ExecutableOnly
                            | provenant_records::LoginObservation::None
                    )
                }))
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
/// The readiness shape: 2 identifies the tagged route scope, which replaced the
/// scope's string route. Older files do not decode and are dropped until the
/// next observation rewrites them.
pub const READINESS_SCHEMA_VERSION: u32 = 2;
const QUOTA_PROBE: &str = "quota-probe";

/// Non-secret expiry metadata observed for this account's login. It is not
/// quota evidence and never creates or clears a provider refusal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginObservation {
    #[serde(flatten)]
    pub extra: ExtraFields,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessRead {
    /// Scope that supplied this evidence; absent when no valid record matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_scope: Option<ReadinessScope>,
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
            observed_scope: None,
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
                    availability_warning: None,
                    extra: Default::default(),
                    availability_ref: availability_ref.clone(),
                    expires_unix_seconds,
                },
                _ => Receipt::unknown(),
            },
            (ReadinessState::Unavailable, reason) => Receipt::Unavailable {
                skipped_accounts: Vec::new(),
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
pub struct ReadinessFile {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub schema_version: u32,
    pub records: Vec<RouteReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<LoginObservation>,
}

impl Default for ReadinessFile {
    fn default() -> Self {
        Self {
            extra: Default::default(),
            schema_version: READINESS_SCHEMA_VERSION,
            records: Vec::new(),
            login: None,
        }
    }
}

impl ReadinessFile {
    /// Decode a stored file whose path names `adapter` and `account_key`.
    pub fn decode(bytes: &[u8], adapter: &str, account_key: &str) -> Result<Self, QuotaError> {
        let file: Self = provenant_records::decode(bytes, provenant_records::Limits::CONFIG)
            .map_err(|_| QuotaError::Invalid("readiness record"))?;
        file.validate(adapter, account_key)?;
        Ok(file)
    }

    fn validate(&self, adapter: &str, account_key: &str) -> Result<(), QuotaError> {
        let mut seen = std::collections::BTreeSet::new();
        if self.schema_version != READINESS_SCHEMA_VERSION
            || self.records.len() > MAX_READINESS_RECORDS
            || self.login.as_ref().is_some_and(|login| {
                login.scope.adapter != adapter
                    || login.scope.account_key != account_key
                    || login.scope.host.is_empty()
                    || login.scope.host.len() > 128
                    || login.scope.route != ReadinessRoute::Account
            })
            || self.records.iter().any(|r| {
                r.scope.adapter != adapter
                    || r.scope.account_key != account_key
                    || r.scope.host.is_empty()
                    || r.scope.host.len() > 128
                    || !r.scope.route.valid()
                    || !seen.insert((
                        r.scope.host.as_str(),
                        r.scope.adapter.as_str(),
                        r.scope.account_key.as_str(),
                        r.scope.route.key(),
                    ))
            })
        {
            return Err(QuotaError::Invalid("readiness bounds"));
        }
        Ok(())
    }

    /// The readiness of one route under the caller's resolved
    /// [`crate::FreshnessPolicy`], so one rule set decides every
    /// observation's lifetime. An account-wide unavailable observation covers
    /// every route of that account; any other gap reads as unknown.
    pub fn get(
        &self,
        scope: &ReadinessScope,
        now: i64,
        freshness: &crate::FreshnessPolicy,
    ) -> ReadinessRead {
        let read = |scope: &ReadinessScope| {
            let Some(r) = self.records.iter().find(|r| r.scope.same_scope(scope)) else {
                return ReadinessRead::absent();
            };
            if self.schema_version != READINESS_SCHEMA_VERSION
                || r.schema_version != READINESS_SCHEMA_VERSION
                || r.observed_unix_seconds > now
                || r.expires_unix_seconds <= r.observed_unix_seconds
                || r.evidence_ref
                    .as_ref()
                    .is_some_and(|r| !crate::state::reference(r))
            {
                return ReadinessRead::unknown(ReadinessReason::InvalidEvidence);
            }
            let expires = if r.reason == Some(ReadinessReason::ProviderRefusal) {
                freshness
                    .refusal_expiry(
                        &r.scope.adapter,
                        r.observed_unix_seconds,
                        Some(r.expires_unix_seconds),
                    )
                    .unwrap_or(r.observed_unix_seconds)
            } else {
                r.expires_unix_seconds
            };
            if now >= expires && !r.holds_after_expiry() {
                return ReadinessRead {
                    observed_scope: Some(r.scope.clone()),
                    observed_unix_seconds: Some(r.observed_unix_seconds),
                    expires_unix_seconds: Some(expires),
                    evidence_ref: r.evidence_ref.clone(),
                    ..ReadinessRead::unknown(ReadinessReason::Stale)
                };
            }
            if r.state == ReadinessState::Unknown {
                return ReadinessRead {
                    observed_scope: Some(r.scope.clone()),
                    observed_unix_seconds: Some(r.observed_unix_seconds),
                    expires_unix_seconds: Some(expires),
                    evidence_ref: r.evidence_ref.clone(),
                    ..ReadinessRead::unknown(r.reason.unwrap_or(ReadinessReason::NotProbed))
                };
            }
            ReadinessRead {
                observed_scope: Some(r.scope.clone()),
                state: r.state,
                reason: r.reason,
                observed_unix_seconds: Some(r.observed_unix_seconds),
                expires_unix_seconds: Some(expires),
                reset_unix_seconds: r.reset_unix_seconds,
                evidence_ref: r.evidence_ref.clone(),
            }
        };
        let mut account_fallback = None;
        if scope.route != ReadinessRoute::Account {
            let account = read(&ReadinessScope {
                extra: Default::default(),
                route: ReadinessRoute::Account,
                ..scope.clone()
            });
            if account.state == ReadinessState::Unavailable
                || (account.observed_scope.is_some()
                    && matches!(
                        account.reason,
                        Some(
                            ReadinessReason::SignedOut
                                | ReadinessReason::LoginExpired
                                | ReadinessReason::AccountMismatch
                        )
                    ))
            {
                return account;
            }
            if account.observed_scope.is_some()
                && account.reason == Some(ReadinessReason::ProbedNoFigures)
            {
                account_fallback = Some(account);
            }
        }
        let scoped = read(scope);
        if scoped.state == ReadinessState::Unavailable {
            return scoped;
        }
        if scoped.observed_scope.is_none() {
            return account_fallback.unwrap_or(scoped);
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
        let previous = self.records.clone();
        let now = admission.now_unix_seconds;
        self.records.retain(|r| {
            (r.expires_unix_seconds > now || r.holds_after_expiry())
                && !(r.scope.host == host
                    && r.state == ReadinessState::Unavailable
                    && r.reason == Some(ReadinessReason::ProviderRefusal))
        });
        let scope = ReadinessScope {
            extra: Default::default(),
            host: host.into(),
            adapter: account.account.adapter.clone(),
            account_key: account.account.account_key.clone(),
            route: ReadinessRoute::Account,
        };
        // The account-wide failure supersedes this host's per-route facts.
        // The account record remains the source for restoring held refusals.
        let account_read = self.get(&scope, now, admission.freshness);
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
            let expires = refusal.expires_unix_seconds(admission.freshness)?;
            let scope = ReadinessScope {
                extra: Default::default(),
                host: host.into(),
                adapter: account.account.adapter.clone(),
                account_key: account.account.account_key.clone(),
                route: if refusal.account_wide() {
                    ReadinessRoute::Account
                } else {
                    refusal.pool.as_ref().map_or_else(
                        || ReadinessRoute::model(&account.account.adapter, &refusal.model),
                        |pool| ReadinessRoute::pool(pool.clone()),
                    )
                },
            };
            if let Some(held) = published.iter_mut().find(|r| r.scope.same_scope(&scope)) {
                held.observed_unix_seconds = held
                    .observed_unix_seconds
                    .max(refusal.observed_unix_seconds);
                held.expires_unix_seconds = held.expires_unix_seconds.max(expires);
                held.reset_unix_seconds = held.reset_unix_seconds.max(refusal.resets_unix_seconds);
                continue;
            }
            let old = previous
                .iter()
                .find(|record| record.scope.same_scope(&scope));
            published.push(RouteReadiness {
                extra: old.map_or_else(Default::default, |record| record.extra.clone()),
                schema_version: READINESS_SCHEMA_VERSION,
                scope: old
                    .map_or_else(|| scope.clone(), |record| record.scope.updated_from(&scope)),
                state: ReadinessState::Unavailable,
                reason: Some(ReadinessReason::ProviderRefusal),
                observed_unix_seconds: refusal.observed_unix_seconds,
                expires_unix_seconds: expires,
                reset_unix_seconds: refusal.resets_unix_seconds,
                evidence_ref: Some("provider-refusal".into()),
            });
        }
        self.records
            .retain(|r| !published.iter().any(|new| new.scope.same_scope(&r.scope)));
        self.records.extend(published);
        // Held refusals outrank the informational "probed, no figures" note.
        while self.records.len() > MAX_READINESS_RECORDS {
            let Some(index) = self
                .records
                .iter()
                .position(|r| r.reason == Some(ReadinessReason::ProbedNoFigures))
            else {
                break;
            };
            self.records.remove(index);
        }
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
        freshness: &crate::FreshnessPolicy,
    ) -> Result<(), QuotaError> {
        let account_scope = ReadinessScope {
            extra: Default::default(),
            host: host.into(),
            adapter: account.account.adapter.clone(),
            account_key: account.account.account_key.clone(),
            route: ReadinessRoute::Account,
        };
        let previous = self
            .records
            .iter()
            .find(|record| record.scope.same_scope(&account_scope))
            .cloned();
        // A failed executable/transport probe cannot undo a known login failure.
        // Nor can an older or simultaneous successful observation establish recovery.
        if previous.as_ref().is_some_and(|record| {
            let newer_login = account
                .successful_login_observed_at()
                .is_some_and(|at| at > record.observed_unix_seconds && at <= now);
            record.authentication_failure()
                && (record.expires_unix_seconds > now || record.holds_after_expiry())
                && (now <= record.observed_unix_seconds
                    || (!reason.is_some_and(ReadinessReason::is_authentication_failure)
                        && !newer_login))
        }) {
            return Ok(());
        }
        self.records.retain(|record| {
            (record.expires_unix_seconds > now || record.holds_after_expiry())
                && !(record.scope.host == host
                    && record.scope.adapter == account.account.adapter
                    && record.scope.account_key == account.account.account_key
                    && (reason.is_some()
                        || (record.scope.route == ReadinessRoute::Account
                            && record.evidence_ref.as_deref() == Some(QUOTA_PROBE))))
        });
        if let Some(reason) = reason {
            let expires = freshness.readiness_expiry(now, interval_seconds);
            self.records.push(RouteReadiness {
                extra: previous
                    .as_ref()
                    .map_or_else(Default::default, |record| record.extra.clone()),
                schema_version: READINESS_SCHEMA_VERSION,
                scope: previous.as_ref().map_or_else(
                    || account_scope.clone(),
                    |record| record.scope.updated_from(&account_scope),
                ),
                state: ReadinessState::Unavailable,
                reason: Some(reason),
                observed_unix_seconds: now,
                expires_unix_seconds: expires,
                reset_unix_seconds: None,
                evidence_ref: Some(QUOTA_PROBE.into()),
            });
        } else if self.records.len() < MAX_READINESS_RECORDS
            // A deferred refresh completed no probe, so it reports nothing.
            && account.last_error.is_none()
        {
            // A completed probe with figures is positive readiness evidence:
            // without it a successful tick still reads as "not probed", and
            // telling the owner to tick again cannot help. A probe with no
            // figures stays informational rather than claiming readiness.
            let (state, reason) = match account.snapshot.as_ref() {
                Some(snapshot) if !snapshot.windows.is_empty() => (ReadinessState::Ready, None),
                _ => (
                    ReadinessState::Unknown,
                    Some(ReadinessReason::ProbedNoFigures),
                ),
            };
            let expires = freshness.readiness_expiry(now, interval_seconds);
            self.records.push(RouteReadiness {
                extra: previous
                    .as_ref()
                    .map_or_else(Default::default, |record| record.extra.clone()),
                schema_version: READINESS_SCHEMA_VERSION,
                scope: previous.as_ref().map_or_else(
                    || account_scope.clone(),
                    |record| record.scope.updated_from(&account_scope),
                ),
                state,
                reason,
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
            extra: Default::default(),
            scope: ReadinessScope {
                extra: Default::default(),
                host: LOCAL_HOST.into(),
                adapter: "claude".into(),
                account_key: "key".into(),
                route: ReadinessRoute::Account,
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
    fn login_expiry_metadata_never_synthesises_a_routing_veto() {
        let file = ReadinessFile {
            extra: Default::default(),
            login: Some(LoginObservation {
                extra: Default::default(),
                scope: ReadinessScope {
                    extra: Default::default(),
                    host: LOCAL_HOST.into(),
                    adapter: "claude".into(),
                    account_key: "key".into(),
                    route: ReadinessRoute::Account,
                },
                observed_unix_seconds: 100,
                valid_until_unix_seconds: 300,
                expires_unix_seconds: Some(200),
            }),
            ..Default::default()
        };
        let route = ReadinessScope {
            extra: Default::default(),
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: ReadinessRoute::from_text("claude/model"),
        };
        assert_eq!(
            file.get(&route, 199, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::NotProbed)
        );
        let expired = file.get(&route, 200, &crate::DEFAULT_FRESHNESS);
        assert_eq!(expired.state, ReadinessState::Unknown);
        assert_eq!(expired.reason, Some(ReadinessReason::NotProbed));
        assert!(expired.project().is_unknown());
        assert_eq!(
            file.get(&route, 300, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::NotProbed)
        );
        let other = ReadinessScope {
            extra: Default::default(),
            account_key: "other".into(),
            ..route.clone()
        };
        assert_eq!(
            file.get(&other, 200, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::NotProbed)
        );
        let remote = ReadinessScope {
            extra: Default::default(),
            host: "other-host".into(),
            ..route
        };
        assert_eq!(
            file.get(&remote, 200, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::NotProbed)
        );
    }

    #[test]
    fn unobservable_login_failures_expire_but_identity_mismatch_does_not() {
        let account = crate::AccountRecord::empty(crate::AccountScope {
            extra: Default::default(),
            adapter: "opencode".into(),
            account_key: "key".into(),
            label: "work".into(),
        });
        let scope = ReadinessScope {
            extra: Default::default(),
            host: LOCAL_HOST.into(),
            adapter: "opencode".into(),
            account_key: "key".into(),
            route: ReadinessRoute::from_text("opencode/model"),
        };
        for reason in [
            ReadinessReason::SignedOut,
            ReadinessReason::LoginExpired,
            ReadinessReason::AccountMismatch,
        ] {
            let mut file = ReadinessFile::default();
            file.sync_probe_result(
                &account,
                LOCAL_HOST,
                100,
                60,
                Some(reason),
                &crate::DEFAULT_FRESHNESS,
            )
            .unwrap();
            file.sync_probe_result(
                &account,
                LOCAL_HOST,
                120,
                60,
                None,
                &crate::DEFAULT_FRESHNESS,
            )
            .unwrap();
            assert_eq!(
                file.get(&scope, 159, &crate::DEFAULT_FRESHNESS).reason,
                Some(reason)
            );
            if reason == ReadinessReason::AccountMismatch {
                file.sync_probe_result(
                    &account,
                    LOCAL_HOST,
                    161,
                    60,
                    None,
                    &crate::DEFAULT_FRESHNESS,
                )
                .unwrap();
                assert_eq!(
                    file.get(&scope, 161, &crate::DEFAULT_FRESHNESS).reason,
                    Some(reason)
                );
            } else {
                assert_ne!(
                    file.get(&scope, 160, &crate::DEFAULT_FRESHNESS).state,
                    ReadinessState::Unavailable
                );
                file.sync_probe_result(
                    &account,
                    LOCAL_HOST,
                    161,
                    60,
                    None,
                    &crate::DEFAULT_FRESHNESS,
                )
                .unwrap();
                assert_ne!(
                    file.get(&scope, 161, &crate::DEFAULT_FRESHNESS).state,
                    ReadinessState::Unavailable
                );
            }
        }
    }

    #[test]
    fn readiness_lifetime_follows_the_configured_freshness() {
        let account = crate::AccountRecord::empty(crate::AccountScope {
            extra: Default::default(),
            adapter: "opencode".into(),
            account_key: "key".into(),
            label: "label".into(),
        });
        let configured = crate::FreshnessPolicy::from_settings(crate::FreshnessSettings {
            readiness_min_seconds: 500,
            ..Default::default()
        });
        let expiry = |freshness: &crate::FreshnessPolicy| {
            let mut file = ReadinessFile::default();
            file.sync_probe_result(
                &account,
                LOCAL_HOST,
                100,
                60,
                Some(ReadinessReason::SignedOut),
                freshness,
            )
            .unwrap();
            file.records[0].expires_unix_seconds
        };
        assert_eq!(expiry(&crate::DEFAULT_FRESHNESS), 160);
        assert_eq!(expiry(&configured), 600);
    }

    #[test]
    fn login_failure_survives_expiry_until_a_newer_successful_probe() {
        let account_scope = crate::AccountScope {
            extra: Default::default(),
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
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        let scope = ReadinessScope {
            extra: Default::default(),
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: ReadinessRoute::from_text("claude/model"),
        };
        assert_eq!(
            file.get(&scope, 159, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::LoginExpired)
        );
        let account_scope = ReadinessScope {
            extra: Default::default(),
            route: ReadinessRoute::Account,
            ..scope.clone()
        };
        assert_eq!(
            file.get(&account_scope, 160, &crate::DEFAULT_FRESHNESS)
                .reason,
            Some(ReadinessReason::LoginExpired)
        );
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            500,
            60,
            Some(ReadinessReason::ProbeTimeout),
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        file.sync_refusals(&account, LOCAL_HOST, &at(&account, 500))
            .unwrap();
        assert_eq!(
            file.get(&scope, 500, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::LoginExpired)
        );
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            99,
            60,
            None,
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            100,
            60,
            None,
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        assert_eq!(
            file.get(&scope, 500, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::LoginExpired)
        );
        // #3342: a successful probe with no figures reads as probed, not as
        // "not probed", so the hint to run `quota tick` is not repeated.
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            501,
            60,
            None,
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        assert_eq!(
            file.get(&scope, 501, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::LoginExpired)
        );
        account.auth_state = Some(crate::AuthState {
            signed_in: Some(true),
            observed_unix_seconds: Some(100),
            ..Default::default()
        });
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            501,
            60,
            None,
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        assert_eq!(
            file.get(&scope, 501, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::LoginExpired)
        );
        account.auth_state = Some(crate::AuthState {
            signed_in: Some(true),
            observed_unix_seconds: Some(501),
            ..Default::default()
        });
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            501,
            60,
            None,
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        assert_eq!(
            file.get(&scope, 501, &crate::DEFAULT_FRESHNESS).reason,
            Some(ReadinessReason::ProbedNoFigures)
        );
    }
    #[test]
    fn readiness_round_trip_and_boundaries() {
        let scope = ReadinessScope {
            extra: Default::default(),
            host: "host".into(),
            adapter: "agy".into(),
            account_key: "account".into(),
            route: ReadinessRoute::from_text("model"),
        };
        let record = RouteReadiness {
            extra: Default::default(),
            schema_version: crate::READINESS_SCHEMA_VERSION,
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
            extra: Default::default(),
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
            let read = file.get(&scope, now, &crate::DEFAULT_FRESHNESS);
            if let Some(reason) = reason {
                assert_eq!(read.state, ReadinessState::Unknown);
                assert_eq!(read.reason, Some(reason));
                assert_eq!(
                    read.observed_unix_seconds,
                    (reason == ReadinessReason::Stale).then_some(10)
                );
                if reason == ReadinessReason::Stale {
                    assert_eq!(read.observed_scope.as_ref(), Some(&scope));
                    assert_eq!(read.evidence_ref.as_deref(), Some("probe:1"));
                    assert_eq!(read.project(), provenant_records::RouteReadiness::stale());
                }
            } else {
                assert_eq!(read.state, ReadinessState::Unavailable);
                assert_eq!(read.observed_scope.as_ref(), Some(&scope));
                assert_eq!(read.observed_unix_seconds, Some(10));
            }
        }
        let mut other = scope.clone();
        other.route = ReadinessRoute::from_text("other");
        assert_eq!(
            file.get(&other, 15, &crate::DEFAULT_FRESHNESS),
            ReadinessRead::absent()
        );
        for evidence in ["".into(), "x".repeat(257), "bad/path".into()] {
            file.records[0].evidence_ref = Some(evidence);
            assert_eq!(
                file.get(&scope, 15, &crate::DEFAULT_FRESHNESS),
                ReadinessRead::unknown(ReadinessReason::InvalidEvidence)
            );
        }
        file.records[0].evidence_ref = Some("x".repeat(256));
        assert_eq!(
            file.get(&scope, 15, &crate::DEFAULT_FRESHNESS).state,
            ReadinessState::Unavailable
        );
        file.records[0].expires_unix_seconds = 10;
        assert_eq!(
            file.get(&scope, 15, &crate::DEFAULT_FRESHNESS),
            ReadinessRead::unknown(ReadinessReason::InvalidEvidence)
        );
        file.records[0].expires_unix_seconds = 20;
        file.records[0].schema_version = READINESS_SCHEMA_VERSION + 1;
        assert_eq!(
            file.get(&scope, 15, &crate::DEFAULT_FRESHNESS),
            ReadinessRead::unknown(ReadinessReason::InvalidEvidence)
        );
    }

    #[test]
    fn readiness_ready_evidence_requires_fresh_matching_scope() {
        let scope = ReadinessScope {
            extra: Default::default(),
            host: "host".into(),
            adapter: "fixture".into(),
            account_key: "account".into(),
            route: ReadinessRoute::from_text("model"),
        };
        let file = ReadinessFile {
            extra: Default::default(),
            records: vec![RouteReadiness {
                extra: Default::default(),
                schema_version: crate::READINESS_SCHEMA_VERSION,
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
        assert_eq!(
            file.get(&scope, 10, &crate::DEFAULT_FRESHNESS).state,
            ReadinessState::Ready
        );
        assert_eq!(
            file.get(&scope, 10, &crate::DEFAULT_FRESHNESS)
                .observed_unix_seconds,
            Some(10)
        );
        assert_eq!(
            file.get(&scope, 9, &crate::DEFAULT_FRESHNESS)
                .observed_unix_seconds,
            None
        );
        assert_eq!(
            file.get(&scope, 20, &crate::DEFAULT_FRESHNESS)
                .observed_unix_seconds,
            Some(10)
        );
        assert_eq!(
            file.get(&scope, 9, &crate::DEFAULT_FRESHNESS).state,
            ReadinessState::Unknown
        );
        assert_eq!(
            file.get(&scope, 20, &crate::DEFAULT_FRESHNESS).state,
            ReadinessState::Unknown
        );
        let mut account_only = file.clone();
        account_only.records[0].scope.route = ReadinessRoute::Account;
        let model_read = account_only.get(&scope, 10, &crate::DEFAULT_FRESHNESS);
        assert_eq!(model_read.state, ReadinessState::Unknown);
        assert!(model_read.observed_scope.is_none());
        for other in [
            ReadinessScope {
                extra: Default::default(),
                host: "other".into(),
                ..scope.clone()
            },
            ReadinessScope {
                extra: Default::default(),
                adapter: "other".into(),
                ..scope.clone()
            },
            ReadinessScope {
                extra: Default::default(),
                account_key: "other".into(),
                ..scope.clone()
            },
            ReadinessScope {
                extra: Default::default(),
                route: ReadinessRoute::from_text("other"),
                ..scope.clone()
            },
        ] {
            assert_eq!(
                file.get(&other, 10, &crate::DEFAULT_FRESHNESS).state,
                ReadinessState::Unknown
            );
        }
    }

    #[test]
    fn readiness_stored_shapes_round_trip_future_fields() -> Result<(), Box<dyn std::error::Error>>
    {
        let scope = ReadinessScope {
            extra: Default::default(),
            host: "host".into(),
            adapter: "fixture".into(),
            account_key: "account".into(),
            route: ReadinessRoute::from_text("model"),
        };
        let mut file = ReadinessFile::default();
        file.records.push(RouteReadiness {
            extra: Default::default(),
            schema_version: READINESS_SCHEMA_VERSION,
            scope: scope.clone(),
            state: ReadinessState::Ready,
            reason: None,
            observed_unix_seconds: 10,
            expires_unix_seconds: 20,
            reset_unix_seconds: None,
            evidence_ref: Some("probe:1".into()),
        });
        file.login = Some(LoginObservation {
            extra: Default::default(),
            scope: ReadinessScope {
                route: ReadinessRoute::Account,
                ..scope
            },
            observed_unix_seconds: 10,
            valid_until_unix_seconds: 20,
            expires_unix_seconds: Some(30),
        });
        let mut value = serde_json::to_value(&file)?;
        value["file"] = serde_json::json!({"future": true});
        value["records"][0]["record"] = serde_json::json!({"future": true});
        value["records"][0]["scope"]["scope"] = serde_json::json!({"future": true});
        value["login"]["login"] = serde_json::json!({"future": true});
        value["login"]["scope"]["scope"] = serde_json::json!({"future": true});
        let bytes = serde_json::to_vec(&value)?;
        let decoded = ReadinessFile::decode(&bytes, "fixture", "account")?;
        let encoded = provenant_records::encode(&decoded)?;
        let round_trip: ReadinessFile =
            provenant_records::decode(encoded.as_slice(), provenant_records::Limits::CONFIG)?;
        assert_eq!(serde_json::to_value(&round_trip)?, value);
        for &state in ReadinessState::ALL {
            let encoded = provenant_records::encode(&state)?;
            let decoded: ReadinessState =
                provenant_records::decode(encoded.as_slice(), provenant_records::Limits::CONFIG)?;
            assert_eq!(decoded, state);
        }
        Ok(())
    }

    #[test]
    fn receipt_projection_covers_every_reason_and_ready() {
        use provenant_records::{RouteExclusionReason as E, RouteReadiness as R};
        fn unavailable_projection(reason: ReadinessReason) -> E {
            match reason {
                ReadinessReason::NotProbed
                | ReadinessReason::Stale
                | ReadinessReason::ProbeTimeout
                | ReadinessReason::CommandFailure
                | ReadinessReason::UncalibratedPartialCoverage
                | ReadinessReason::InvalidEvidence
                | ReadinessReason::UnreachablePeer
                | ReadinessReason::UnsupportedPeerVersion
                | ReadinessReason::ProbedNoFigures => E::UnknownAvailability,
                ReadinessReason::MissingExecutable => E::MissingExecutable,
                ReadinessReason::SignedOut => E::AuthRequired,
                ReadinessReason::LoginExpired => E::LoginExpired,
                ReadinessReason::AccountMismatch => E::AccountMismatch,
                ReadinessReason::Exhausted | ReadinessReason::ProviderRefusal => E::ProviderRefused,
            }
        }
        for &reason in ReadinessReason::ALL {
            let encoded = provenant_records::encode(&reason).unwrap();
            let decoded: ReadinessReason =
                provenant_records::decode(encoded.as_slice(), provenant_records::Limits::CONFIG)
                    .unwrap();
            assert_eq!(decoded, reason);
            let expected = unavailable_projection(reason);
            let mut read = ReadinessRead::unknown(reason);
            read.state = ReadinessState::Unavailable;
            assert!(
                matches!(read.project(), R::Unavailable { reason: actual, .. } if actual == expected),
                "{reason:?}"
            );
            let expected = if reason == ReadinessReason::Stale {
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
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        file = ReadinessFile::decode(&serde_json::to_vec(&file).unwrap(), "claude", "key").unwrap();
        assert_eq!(
            file.get(&route("pool-0"), 150, &crate::DEFAULT_FRESHNESS)
                .reason,
            Some(ReadinessReason::ProbeTimeout)
        );
        file.sync_refusals(&account, LOCAL_HOST, &at(&account, 160))
            .unwrap();
        assert!(file.records.len() <= MAX_READINESS_RECORDS);
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            170,
            300,
            None,
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();
        file.sync_refusals(&account, LOCAL_HOST, &at(&account, 170))
            .unwrap();
        assert_eq!(file.records.len(), MAX_READINESS_RECORDS);
        assert_eq!(
            file.get(&route("pool-0"), 170, &crate::DEFAULT_FRESHNESS)
                .reason,
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
            extra: Default::default(),
            adapter: "claude".into(),
            account_key: "key".into(),
            label: "label".into(),
        }
    }
    fn at(record: &crate::AccountRecord, now: i64) -> crate::Admission<'_> {
        static POLICY: std::sync::OnceLock<provenant_records::policy::Policy> =
            std::sync::OnceLock::new();
        crate::Admission {
            freshness: &crate::DEFAULT_FRESHNESS,
            policy: POLICY.get_or_init(Default::default),
            policies: crate::source::TEST_POLICIES,
            now_unix_seconds: now,
            records: std::slice::from_ref(record),
            pools: &[],
        }
    }
    fn route(route: &str) -> ReadinessScope {
        ReadinessScope {
            extra: Default::default(),
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: ReadinessRoute::from_text(route),
        }
    }

    #[test]
    fn refusals_publish_by_scope_and_clear_with_the_account_record() {
        let mut file = ReadinessFile::default();
        let mut record = crate::AccountRecord::empty(account());
        // Nothing recorded reads as unknown, never ready.
        assert_eq!(
            file.get(&route("claude/m"), 150, &crate::DEFAULT_FRESHNESS),
            ReadinessRead::absent()
        );
        record.refusals = vec![
            refused(Some("pool"), "m", Some(500)),
            refused(None, "other", None),
        ];
        let mut ready = RouteReadiness {
            extra: Default::default(),
            schema_version: crate::READINESS_SCHEMA_VERSION,
            scope: route("pool"),
            state: ReadinessState::Ready,
            reason: None,
            observed_unix_seconds: 100,
            expires_unix_seconds: 500,
            reset_unix_seconds: None,
            evidence_ref: Some("fixture".into()),
        };
        let future = serde_json::json!({"value": true});
        ready.extra.insert("future_readiness".into(), future);
        ready
            .scope
            .extra
            .insert("future_scope".into(), serde_json::json!({"value": true}));
        let row_extra = ready.extra.clone();
        let scope_extra = ready.scope.extra.clone();
        let mut retained = ready.clone();
        retained.scope.route = ReadinessRoute::from_text("retained");
        let mut peer = ready.clone();
        peer.scope.host = "peer".into();
        peer.state = ReadinessState::Unavailable;
        peer.reason = Some(ReadinessReason::ProviderRefusal);
        let mut probe = ready.clone();
        probe.scope.route = ReadinessRoute::Account;
        probe.state = ReadinessState::Unknown;
        probe.reason = Some(ReadinessReason::CommandFailure);
        probe.evidence_ref = Some("quota-probe".into());
        file.records = vec![ready, retained.clone(), peer.clone(), probe.clone()];
        file.sync_refusals(&record, LOCAL_HOST, &at(&record, 150))
            .unwrap();
        assert_eq!(
            file.get(&route("pool"), 150, &crate::DEFAULT_FRESHNESS)
                .state,
            ReadinessState::Unavailable
        );
        file.sync_refusals(&record, LOCAL_HOST, &at(&record, 160))
            .unwrap();
        let published = file
            .records
            .iter()
            .find(|r| r.scope.same_scope(&route("pool")))
            .unwrap();
        assert_eq!(published.extra, row_extra);
        assert_eq!(published.scope.extra, scope_extra);
        for observation in [retained, peer, probe] {
            assert!(file.records.contains(&observation));
        }
        let bytes = serde_json::to_vec(&file).unwrap();
        file = ReadinessFile::decode(&bytes, "claude", "key").unwrap();
        // A pool refusal covers its pool only; a model refusal only its model.
        let pool = file.get(&route("pool"), 150, &crate::DEFAULT_FRESHNESS);
        assert_eq!(pool.state, ReadinessState::Unavailable);
        assert_eq!(pool.reason, Some(ReadinessReason::ProviderRefusal));
        assert_eq!(pool.reset_unix_seconds, Some(500));
        assert!(!pool.project().is_unknown());
        assert_eq!(
            file.get(&route("claude/m"), 150, &crate::DEFAULT_FRESHNESS)
                .state,
            ReadinessState::Unknown
        );
        assert_eq!(
            file.get(&route("claude/other"), 150, &crate::DEFAULT_FRESHNESS)
                .expires_unix_seconds,
            Some(3700)
        );
        // Expiry reads as unknown and the reset alone never makes it ready.
        let lapsed = file.get(&route("pool"), 500, &crate::DEFAULT_FRESHNESS);
        assert_eq!(lapsed.state, ReadinessState::Unknown);
        assert!(lapsed.project().is_unknown());
        // Another account's identity is not covered.
        let mut elsewhere = route("pool");
        elsewhere.account_key = "other".into();
        assert_eq!(
            file.get(&elsewhere, 150, &crate::DEFAULT_FRESHNESS).state,
            ReadinessState::Unknown
        );
        // Pool headroom clears that pool's refusal. A model with no pool
        // remains refused while any reported window lacks headroom (#3450).
        record.snapshot = Some(crate::QuotaSnapshot {
            extra: Default::default(),
            schema_version: 1,
            account: account(),
            revision: 0,
            status: crate::QuotaStatus::Available,
            observed_unix_seconds: 190,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![
                crate::QuotaWindow {
                    extra: Default::default(),
                    window_seconds: None,
                    pool: "pool".into(),
                    used_basis_points: 1000,
                    resets_unix_seconds: Some(900),
                },
                crate::QuotaWindow {
                    extra: Default::default(),
                    window_seconds: None,
                    pool: "other".into(),
                    used_basis_points: 9800,
                    resets_unix_seconds: Some(900),
                },
            ],
        });
        file.sync_refusals(&record, LOCAL_HOST, &at(&record, 200))
            .unwrap();
        assert_eq!(
            file.get(&route("pool"), 200, &crate::DEFAULT_FRESHNESS)
                .state,
            ReadinessState::Unknown
        );
        assert_eq!(
            file.get(&route("claude/other"), 200, &crate::DEFAULT_FRESHNESS)
                .state,
            ReadinessState::Unavailable
        );
        // Once every window has headroom, the unbound model refusal clears
        // too. Clearing evidence becomes unknown, never a ready observation.
        record.snapshot.as_mut().unwrap().windows[1].used_basis_points = 1000;
        file.sync_refusals(&record, LOCAL_HOST, &at(&record, 210))
            .unwrap();
        assert_eq!(
            file.get(&route("claude/other"), 210, &crate::DEFAULT_FRESHNESS)
                .state,
            ReadinessState::Unknown
        );
        // The file refuses another account's records.
        assert!(ReadinessFile::decode(&bytes, "claude", "other").is_err());
    }

    #[test]
    fn probed_no_figures_does_not_mask_route_unavailable() {
        let mut account_record = crate::AccountRecord::empty(account());
        let mut file = ReadinessFile::default();
        file.sync_probe_result(
            &account_record,
            LOCAL_HOST,
            100,
            60,
            None,
            &crate::DEFAULT_FRESHNESS,
        )
        .unwrap();

        let refused_scope = route("claude/refused");
        file.records.push(RouteReadiness {
            extra: Default::default(),
            schema_version: READINESS_SCHEMA_VERSION,
            scope: refused_scope.clone(),
            state: ReadinessState::Unavailable,
            reason: Some(ReadinessReason::ProviderRefusal),
            observed_unix_seconds: 100,
            expires_unix_seconds: 200,
            reset_unix_seconds: None,
            evidence_ref: Some("provider-refusal".into()),
        });

        let unrecorded_scope = route("claude/unrecorded");

        // #3364: account at ProbedNoFigures plus a route record Unavailable reads
        // Unavailable for that route; account at ProbedNoFigures with no route record
        // still reads unknown.
        for (query_scope, expected_state, expected_reason) in [
            (
                &refused_scope,
                ReadinessState::Unavailable,
                Some(ReadinessReason::ProviderRefusal),
            ),
            (
                &unrecorded_scope,
                ReadinessState::Unknown,
                Some(ReadinessReason::ProbedNoFigures),
            ),
        ] {
            let read = file.get(query_scope, 120, &crate::DEFAULT_FRESHNESS);
            assert_eq!(read.state, expected_state);
            assert_eq!(read.reason, expected_reason);
        }

        // Also verify via sync_refusals
        account_record.refusals = vec![refused(None, "synced-refused", None)];
        file.sync_refusals(&account_record, LOCAL_HOST, &at(&account_record, 100))
            .unwrap();
        let synced_refused_scope = route("claude/synced-refused");

        for (query_scope, expected_state, expected_reason) in [
            (
                &synced_refused_scope,
                ReadinessState::Unavailable,
                Some(ReadinessReason::ProviderRefusal),
            ),
            (
                &unrecorded_scope,
                ReadinessState::Unknown,
                Some(ReadinessReason::ProbedNoFigures),
            ),
        ] {
            let read = file.get(query_scope, 120, &crate::DEFAULT_FRESHNESS);
            assert_eq!(read.state, expected_state);
            assert_eq!(read.reason, expected_reason);
        }
    }
}
