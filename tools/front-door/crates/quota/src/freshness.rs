//! The one owner of expiry and grace rules for quota evidence.
//!
//! A rule lives here or it does not live at all. Consumers receive an
//! [`FreshnessPolicy`] resolved from the quota config domain; the policy also
//! carries the layer that supplied its values so reports can show the source.
use crate::schedule::{Power, Trigger};
use crate::{FreshnessSettings, QuotaError, QuotaPolicy, QuotaSnapshot};
use serde::{Deserialize, Serialize};

/// The layer that supplied the effective freshness values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessSource {
    Default,
    Config,
}

/// Every expiry and grace rule used to decide whether quota evidence still
/// holds. Built from config once and shared by reporting, routing and
/// admission; none of the consumers owns a lifetime of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FreshnessPolicy {
    pub snapshot_seconds: u64,
    pub login_verification_seconds: u64,
    pub chair_seconds: u64,
    pub availability_grace_seconds: u64,
    pub refusal_seconds: u64,
    pub free_pool_recheck_seconds: u64,
    pub readiness_min_seconds: u64,
    pub source: FreshnessSource,
}

/// The compiled default policy, for callers without a resolved config.
pub static DEFAULT_FRESHNESS: FreshnessPolicy = FreshnessPolicy::from_settings(FreshnessSettings {
    snapshot_seconds: crate::DEFAULT_SNAPSHOT_SECONDS,
    login_verification_seconds: crate::DEFAULT_LOGIN_VERIFICATION_SECONDS,
    chair_seconds: crate::DEFAULT_CHAIR_SECONDS,
    availability_grace_seconds: crate::DEFAULT_AVAILABILITY_GRACE_SECONDS,
    refusal_seconds: crate::DEFAULT_REFUSAL_SECONDS,
    free_pool_recheck_seconds: crate::DEFAULT_FREE_POOL_RECHECK_SECONDS,
    readiness_min_seconds: crate::DEFAULT_READINESS_MIN_SECONDS,
});

impl Default for FreshnessPolicy {
    fn default() -> Self {
        DEFAULT_FRESHNESS
    }
}

impl FreshnessPolicy {
    pub const fn from_settings(settings: FreshnessSettings) -> Self {
        Self {
            snapshot_seconds: settings.snapshot_seconds,
            login_verification_seconds: settings.login_verification_seconds,
            chair_seconds: settings.chair_seconds,
            availability_grace_seconds: settings.availability_grace_seconds,
            refusal_seconds: settings.refusal_seconds,
            free_pool_recheck_seconds: settings.free_pool_recheck_seconds,
            readiness_min_seconds: settings.readiness_min_seconds,
            source: FreshnessSource::Default,
        }
    }

    pub const fn with_source(mut self, source: FreshnessSource) -> Self {
        self.source = source;
        self
    }

    /// Lifetime of a snapshot for this adapter. Per-adapter polling remains the
    /// primary rule; the configured fallback applies to adapters without one.
    pub fn snapshot_ttl(&self, policies: &[QuotaPolicy], adapter: &str) -> i64 {
        crate::schedule::interval(policies, adapter, Trigger::Timer, Power::Ac)
            .map(|seconds| seconds as i64)
            .unwrap_or(self.snapshot_seconds as i64)
    }

    /// Whether a reading is old enough to use and not from the future.
    pub fn snapshot_fresh(
        &self,
        snapshot: &QuotaSnapshot,
        now: i64,
        policies: &[QuotaPolicy],
    ) -> bool {
        snapshot.observed_unix_seconds <= now
            && now.saturating_sub(snapshot.observed_unix_seconds)
                < self.snapshot_ttl(policies, &snapshot.account.adapter)
    }

    /// Whether a positive login verification still suppresses an ambient
    /// sign-in detection for this account.
    pub fn login_verification_fresh(&self, verified_at: i64, now: i64) -> bool {
        verified_at <= now
            && now.saturating_sub(verified_at) < self.login_verification_seconds as i64
    }

    /// Whether a refresh-pending auth fact, or a refresh-deferred attempt,
    /// observed at `at` still describes the account. It lives as long as a
    /// snapshot from the adapter's polling rule, so a fact older than one poll
    /// never overrides what the next reading would show.
    pub fn refresh_pending_fresh(
        &self,
        at: i64,
        now: i64,
        policies: &[QuotaPolicy],
        adapter: &str,
    ) -> bool {
        at <= now && now.saturating_sub(at) < self.snapshot_ttl(policies, adapter)
    }

    /// Whether a snapshot may drive an automatic chair switch.
    pub fn chair_fresh(&self, snapshot: &QuotaSnapshot, now: i64) -> bool {
        snapshot.validate().is_ok()
            && now >= snapshot.observed_unix_seconds
            && now.saturating_sub(snapshot.observed_unix_seconds) < self.chair_seconds as i64
    }

    /// Bound the lifetime of a routing decision that was already made on an
    /// expired observation. A stale observation is never renewed.
    pub fn expired_availability_expires(&self, now: i64) -> i64 {
        now.saturating_add(self.availability_grace_seconds as i64)
    }

    /// When a recorded refusal stops holding: its reset when the provider names
    /// one, otherwise the configured lifetime, bounded by any adapter override.
    pub fn refusal_expiry(
        &self,
        adapter: &str,
        observed: i64,
        reset: Option<i64>,
    ) -> Result<i64, QuotaError> {
        let revalidation = provenant_records::AdapterSpec::get(adapter)
            .and_then(|spec| spec.refusal_revalidation_seconds);
        if revalidation.is_none()
            && let Some(reset) = reset
        {
            return Ok(reset);
        }
        let bounded = observed
            .checked_add(revalidation.unwrap_or(self.refusal_seconds as i64))
            .ok_or(QuotaError::TimeRange)?;
        Ok(reset.map_or(bounded, |reset| reset.min(bounded)))
    }

    /// Expiry to store with an observation. The interval comes from the
    /// adapter's polling rule and is never shorter than the configured floor.
    pub fn readiness_expiry(&self, now: i64, interval_seconds: u64) -> i64 {
        let interval = interval_seconds.max(self.readiness_min_seconds) as i64;
        now.saturating_add(interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_expiry_comes_from_the_policy() {
        let settings = FreshnessSettings {
            snapshot_seconds: 17,
            login_verification_seconds: 18,
            chair_seconds: 19,
            availability_grace_seconds: 20,
            refusal_seconds: 21,
            free_pool_recheck_seconds: 22,
            readiness_min_seconds: 23,
        };
        let policy = FreshnessPolicy::from_settings(settings).with_source(FreshnessSource::Config);
        assert_eq!(policy.snapshot_seconds, 17);
        assert_eq!(policy.login_verification_seconds, 18);
        assert_eq!(policy.chair_seconds, 19);
        assert_eq!(policy.availability_grace_seconds, 20);
        assert_eq!(policy.refusal_seconds, 21);
        assert_eq!(policy.free_pool_recheck_seconds, 22);
        assert_eq!(policy.readiness_min_seconds, 23);
        assert_eq!(policy.source, FreshnessSource::Config);
        assert_eq!(policy.expired_availability_expires(100), 120);
        assert_eq!(policy.readiness_expiry(100, 1), 123);
        // A verification expires once its age reaches the configured lifetime.
        assert!(!policy.login_verification_fresh(82, 100));
        assert!(policy.login_verification_fresh(83, 100));
        assert!(!policy.login_verification_fresh(101, 100));
    }
}
