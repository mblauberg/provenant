//! Admission checks use validated evidence, including persisted provider refusals.
use crate::*;
use provenant_records::{ExtraFields, ResolvedRoute, RouteReadiness};
use serde::{Deserialize, Serialize};

/// A newer reading clears a refusal only below this use (95%).
const CLEAR_HEADROOM_BASIS_POINTS: u64 = 9500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub account: AccountScope,
    pub model: String,
    pub pool: Option<String>,
    pub observed_unix_seconds: i64,
    pub resets_unix_seconds: Option<i64>,
}
/// Short-lived provider capacity evidence, separate from quota admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityCooldown {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub account: AccountScope,
    pub model: String,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
}

pub fn record_capacity_cooldown(
    current: Option<&AccountRecord>,
    account: &AccountScope,
    model: &str,
    now: i64,
    duration_seconds: u64,
) -> Result<Option<UpdateIntent>, QuotaError> {
    if duration_seconds == 0 {
        return Ok(None);
    }
    account.validate()?;
    let prefix = format!("{}/", account.adapter);
    let model = model
        .strip_prefix(&prefix)
        .ok_or(QuotaError::Invalid("capacity model"))?;
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        return Err(QuotaError::Invalid("capacity model"));
    }
    if current.is_some_and(|record| record.account != *account) {
        return Err(QuotaError::Conflict);
    }
    let expires = now
        .checked_add(i64::try_from(duration_seconds).map_err(|_| QuotaError::TimeRange)?)
        .ok_or(QuotaError::TimeRange)?;
    if expires <= now {
        return Err(QuotaError::Invalid("capacity duration"));
    }
    let mut record = current
        .cloned()
        .unwrap_or_else(|| AccountRecord::empty(account.clone()));
    record.validate()?;
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or(QuotaError::NumberOutOfRange)?;
    let extra = record
        .capacity_cooldowns
        .iter()
        .find(|cooldown| cooldown.model == model)
        .map(|cooldown| cooldown.extra.clone())
        .unwrap_or_default();
    record
        .capacity_cooldowns
        .retain(|c| c.model != model && c.expires_unix_seconds > now);
    if record.capacity_cooldowns.len() >= MAX_WINDOWS {
        return Err(QuotaError::Invalid("capacity bound"));
    }
    record.capacity_cooldowns.push(CapacityCooldown {
        extra,
        account: account.clone(),
        model: model.into(),
        observed_unix_seconds: now,
        expires_unix_seconds: expires,
    });
    record.validate()?;
    Ok(Some(UpdateIntent {
        account: account.clone(),
        expected: current.cloned(),
        replacement: Some(record),
    }))
}

pub struct Admission<'a> {
    pub policies: &'a [QuotaPolicy],
    pub now_unix_seconds: i64,
    pub records: &'a [AccountRecord],
    /// Only wire-verified model-to-pool bindings. An absent binding cannot
    /// turn an unrelated pool into a model-wide exhaustion.
    pub pools: &'a [String],
}
impl Admission<'_> {
    /// Check provider readiness, current allowance and the account record's
    /// persisted refusals.
    pub fn check(&self, route: &ResolvedRoute) -> Result<(), QuotaError> {
        match route.readiness {
            RouteReadiness::Ready {
                expires_unix_seconds,
                ..
            } if expires_unix_seconds > self.now_unix_seconds => {}
            _ => return Err(QuotaError::Refused("provider readiness")),
        }
        let key = route
            .account_key
            .as_ref()
            .ok_or(QuotaError::Refused("account identity unknown"))?;
        let record = self
            .records
            .iter()
            .find(|r| r.account.adapter == route.adapter && r.account.account_key == *key);
        if let Some(r) = record {
            r.validate()?;
        }
        let free = free_pool(&route.adapter);
        // Only the adapter's own keyless free pool spans models.
        let in_free = |pool: Option<&str>| free.is_some() && pool == free;
        let snapshot = record.and_then(|r| r.snapshot.as_ref());
        let go_model = route.adapter == "opencode" && route.model.starts_with("opencode-go/");
        let fresh = snapshot.is_some_and(|s| {
            s.observed_unix_seconds <= self.now_unix_seconds
                && self
                    .now_unix_seconds
                    .saturating_sub(s.observed_unix_seconds)
                    < schedule::interval(
                        self.policies,
                        &route.adapter,
                        schedule::Trigger::Timer,
                        schedule::Power::Ac,
                    )
                    .unwrap_or(0) as i64
        });
        if fresh && let Some(snapshot) = snapshot {
            if snapshot.status == QuotaStatus::AuthRequired {
                return Err(QuotaError::Refused("authentication required"));
            }
            if matches!(
                snapshot.status,
                QuotaStatus::Available | QuotaStatus::CreditOnly | QuotaStatus::Exhausted
            ) && snapshot.windows.iter().any(|w| {
                (self
                    .pools
                    .iter()
                    .any(|pool| matches_pool(&route.adapter, &w.pool, pool))
                    || (go_model
                        && ["five_hour", "seven_day", "thirty_day"].contains(&w.pool.as_str())))
                    && w.used_basis_points >= 10000
                    && (w
                        .resets_unix_seconds
                        .is_none_or(|r| r > self.now_unix_seconds)
                        || snapshot
                            .details
                            .as_ref()
                            .is_some_and(|d| d.calibration_active(&w.pool, self.now_unix_seconds)))
            }) {
                return Err(QuotaError::Refused("allowance exhausted"));
            }
        }
        for refusal in record
            .into_iter()
            .flat_map(|r| r.refusals.iter())
            .filter(|r| {
                r.account.adapter == route.adapter
                    && r.account.account_key == *key
                    && (r.model == route.model
                        || (in_free(r.pool.as_deref())
                            && self.pools.iter().any(|pool| Some(pool.as_str()) == free)))
            })
        {
            let expires = refusal.resets_unix_seconds.unwrap_or(
                refusal
                    .observed_unix_seconds
                    .checked_add(3600)
                    .ok_or(QuotaError::TimeRange)?,
            );
            let comparable_newer = fresh
                && record.is_some_and(|r| r.last_error.is_none())
                && snapshot.is_some_and(|s| {
                    s.observed_unix_seconds > refusal.observed_unix_seconds
                        && matches!(
                            s.status,
                            QuotaStatus::Available
                                | QuotaStatus::CreditOnly
                                | QuotaStatus::Exhausted
                        )
                        && refusal.pool.as_ref().is_some_and(|pool| {
                            let mut windows = s
                                .windows
                                .iter()
                                .filter(|w| matches_pool(&route.adapter, &w.pool, pool))
                                .peekable();
                            windows.peek().is_some()
                                && windows.all(|w| {
                                    // A lapsed reset alone cannot clear a refusal.
                                    w.used_basis_points < CLEAR_HEADROOM_BASIS_POINTS
                                })
                        })
                });
            if expires > self.now_unix_seconds
                && !comparable_newer
                && ((in_free(refusal.pool.as_deref()) && refusal.model == route.model)
                    || refusal.pool.as_ref().is_none_or(|p| self.pools.contains(p)))
            {
                return Err(QuotaError::Refused(if in_free(refusal.pool.as_deref()) {
                    "free tier exhausted; use a paid route or wait for its allowance reset"
                } else {
                    "scoped provider refusal"
                }));
            }
        }
        Ok(())
    }
}

/// Build a compare-and-swap update for structured provider quota evidence.
/// The provider message is intentionally excluded from durable quota state.
/// Without a pool the refusal holds only the model, until `resets_unix_seconds`
/// or the one-hour backoff.
pub fn record_refusal(
    current: Option<&AccountRecord>,
    account: &AccountScope,
    model: &str,
    pool: Option<&str>,
    now_unix_seconds: i64,
    resets_unix_seconds: Option<i64>,
) -> Result<UpdateIntent, QuotaError> {
    account.validate()?;
    let prefix = format!("{}/", account.adapter);
    if !model.starts_with(&prefix)
        || model.len() > 512
        || model.chars().any(char::is_control)
        || pool.is_some_and(|pool| {
            pool.is_empty() || pool.len() > 256 || pool.chars().any(char::is_control)
        })
        || resets_unix_seconds.is_some_and(|resets| resets <= now_unix_seconds)
    {
        return Err(QuotaError::Invalid("provider refusal"));
    }
    let model = model
        .strip_prefix(&prefix)
        .ok_or(QuotaError::Invalid("provider refusal model"))?;
    if current.is_some_and(|record| record.account != *account) {
        return Err(QuotaError::Conflict);
    }
    let mut record = current
        .cloned()
        .unwrap_or_else(|| AccountRecord::empty(account.clone()));
    record.validate()?;
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or(QuotaError::NumberOutOfRange)?;
    let extra = record
        .refusals
        .iter()
        .find(|refusal| refusal.model == model && refusal.pool.as_deref() == pool)
        .map(|refusal| refusal.extra.clone())
        .unwrap_or_default();
    record
        .refusals
        .retain(|refusal| refusal.model != model || refusal.pool.as_deref() != pool);
    if record.refusals.len() >= MAX_WINDOWS {
        return Err(QuotaError::Invalid("provider refusal bound"));
    }
    record.refusals.push(Refusal {
        extra,
        account: account.clone(),
        model: model.into(),
        pool: pool.map(Into::into),
        observed_unix_seconds: now_unix_seconds,
        resets_unix_seconds,
    });
    record.validate()?;
    Ok(UpdateIntent {
        account: account.clone(),
        expected: current.cloned(),
        replacement: Some(record),
    })
}

/// The keyless free pool an adapter draws on, if it has one.
fn free_pool(adapter: &str) -> Option<&'static str> {
    provenant_records::AdapterSpec::get(adapter).and_then(|spec| spec.free_pool)
}

fn matches_pool(adapter: &str, window: &str, pool: &str) -> bool {
    window == pool
        || (provenant_records::AdapterSpec::get(adapter)
            .is_some_and(|spec| spec.grouped_quota_pools)
            && window
                .split_once('.')
                .is_some_and(|(group, _)| group == pool))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_admission_evidence_preserves_unknown_fields_on_refresh() {
        let account = AccountScope {
            adapter: "codex".into(),
            account_key: "key".into(),
            label: "label".into(),
        };
        let future = serde_json::json!({"nested": [null, true, u64::MAX]});
        let refusal: Refusal = serde_json::from_value(serde_json::json!({
            "account": account, "model": "model", "pool": null,
            "observed_unix_seconds": 100, "resets_unix_seconds": 500,
            "future": future,
        }))
        .unwrap();
        let cooldown: CapacityCooldown = serde_json::from_value(serde_json::json!({
            "account": account, "model": "model",
            "observed_unix_seconds": 100, "expires_unix_seconds": 500,
            "future": future,
        }))
        .unwrap();
        let mut record = AccountRecord::empty(account.clone());
        record.refusals.push(refusal);
        record.capacity_cooldowns.push(cooldown);
        let record = record_refusal(Some(&record), &account, "codex/model", None, 200, Some(600))
            .unwrap()
            .replacement
            .unwrap();
        let record = record_capacity_cooldown(Some(&record), &account, "codex/model", 200, 600)
            .unwrap()
            .unwrap()
            .replacement
            .unwrap();
        assert_eq!(
            serde_json::to_value(&record.refusals[0]).unwrap()["future"],
            future
        );
        assert_eq!(
            serde_json::to_value(&record.capacity_cooldowns[0]).unwrap()["future"],
            future
        );
        assert_eq!(record.refusals[0].observed_unix_seconds, 200);
        assert_eq!(record.capacity_cooldowns[0].observed_unix_seconds, 200);
    }

    #[test]
    fn capacity_evidence_is_scoped_and_replaced_after_expiry() {
        let account = AccountScope {
            adapter: "codex".into(),
            account_key: "key".into(),
            label: "label".into(),
        };
        let intent = record_capacity_cooldown(None, &account, "codex/model", 100, 600)
            .unwrap()
            .unwrap();
        let first = intent.replacement.unwrap();
        assert_eq!(first.capacity_cooldowns[0].expires_unix_seconds, 700);
        assert_eq!(first.capacity_cooldowns[0].model, "model");
        let intent = record_capacity_cooldown(Some(&first), &account, "codex/other", 700, 600)
            .unwrap()
            .unwrap();
        let second = intent.replacement.unwrap();
        assert_eq!(second.capacity_cooldowns.len(), 1);
        assert_eq!(second.capacity_cooldowns[0].model, "other");
        assert!(record_capacity_cooldown(None, &account, "claude/model", 100, 600).is_err());
        assert!(
            record_capacity_cooldown(None, &account, "codex/model", 100, 0)
                .unwrap()
                .is_none()
        );
        let before = first.clone();
        assert!(
            record_capacity_cooldown(Some(&first), &account, "codex/model", 100, 0)
                .unwrap()
                .is_none()
        );
        assert_eq!(first, before);
    }
    #[test]
    fn dst_reset_rejects_gap_and_fold() {
        let zone = jiff::tz::TimeZone::get("America/New_York").unwrap();
        assert!(local_reset("2026-03-08T02:30:00".parse().unwrap(), zone.clone()).is_err());
        assert!(local_reset("2026-11-01T01:30:00".parse().unwrap(), zone.clone()).is_err());
        assert!(local_reset("2026-11-01T03:30:00".parse().unwrap(), zone).is_ok());
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    fn route() -> ResolvedRoute {
        ResolvedRoute {
            extra: Default::default(),
            adapter: "claude".into(),
            model: "exact/model".into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: Some("opaque".into()),
            host: "host".into(),
            alternatives: Default::default(),
            exclusions: vec![],
            eligibility: provenant_records::RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                extra: Default::default(),
                availability_ref: "fixture".into(),
                expires_unix_seconds: 9999,
            },
            confidentiality: provenant_records::RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        }
    }
    fn record(status: QuotaStatus) -> AccountRecord {
        let account = AccountScope {
            adapter: "claude".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status,
            observed_unix_seconds: 900,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![QuotaWindow {
                window_seconds: None,
                pool: "pool-a".into(),
                used_basis_points: 10000,
                resets_unix_seconds: Some(2000),
            }],
        });
        record
    }
    #[test]
    fn go_calibration_is_rechecked_before_launch_without_free_pool_bindings() {
        let mut route = route();
        route.adapter = "opencode".into();
        route.model = "opencode-go/mimo-v2.6-flash".into();
        route.account_key = Some("go".into());
        route.pool_selection = Some(provenant_records::RouteSelectionReceipt {
            extra: Default::default(),
            pool: "senior".into(),
            candidates: vec![],
            roll: 0,
            total_weight: 1,
            pick: "opencode/opencode-go/mimo-v2.6-flash".into(),
            reason: None,
            redraws: vec![],
        });
        let mut record = record(QuotaStatus::Available);
        record.account = AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        };
        let snapshot = record.snapshot.as_mut().unwrap();
        snapshot.account = record.account.clone();
        snapshot.windows.clear();
        let raw = [record];
        let check = |records: &[AccountRecord], now| {
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: now,
                records,
                pools: &[],
            }
            .check(&route)
        };
        assert!(check(&raw, 1000).is_ok());
        let mut settings = Settings::default();
        settings.calibration.insert(
            "go".into(),
            Calibration {
                five_hour: Some(43),
                seven_day: Some(17),
                thirty_day: Some(59),
                expires: "1970-01-01T00:18:20Z".into(),
            },
        );
        let mut active = raw.clone();
        settings.apply_calibration(&mut active, 1000);
        assert!(check(&active, 1000).is_ok());
        assert!(check(&active, 1100).is_ok());
        settings.calibration.get_mut("go").unwrap().five_hour = Some(100);
        let mut exhausted = raw.clone();
        settings.apply_calibration(&mut exhausted, 1000);
        exhausted[0].snapshot.as_mut().unwrap().windows[0].resets_unix_seconds = Some(999);
        assert!(matches!(
            check(&exhausted, 1000),
            Err(QuotaError::Refused("allowance exhausted"))
        ));
    }

    #[test]
    fn opencode_free_refusal_cools_the_identity_pool_across_models() {
        let mut route = route();
        route.adapter = "opencode".into();
        route.model = "opencode/second-free".into();
        let refusal = Refusal {
            extra: Default::default(),
            account: AccountScope {
                adapter: "opencode".into(),
                account_key: "opaque".into(),
                label: "free".into(),
            },
            model: "opencode/first-free".into(),
            pool: Some("opencode-free".into()),
            observed_unix_seconds: 900,
            resets_unix_seconds: None,
        };
        let mut record = AccountRecord::empty(refusal.account.clone());
        record.refusals.push(refusal.clone());
        for (key, pools, blocked) in [
            ("opaque", vec!["opencode-free".into()], true),
            ("other", vec!["opencode-free".into()], false),
            ("opaque", vec![], false),
        ] {
            route.account_key = Some(key.into());
            assert_eq!(
                Admission {
                    policies: crate::source::TEST_POLICIES,
                    now_unix_seconds: 1000,
                    records: std::slice::from_ref(&record),
                    pools: &pools
                }
                .check(&route)
                .is_err(),
                blocked
            );
        }
        route.account_key = Some("opaque".into());
        route.model.clone_from(&refusal.model);
        assert!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 1000,
                records: &[record],
                pools: &["opencode-free".into()]
            }
            .check(&route)
            .is_err()
        );
    }
    #[test]
    fn failed_probe_preserves_recent_exhaustion() {
        let mut record = record(QuotaStatus::Exhausted);
        record.last_error = Some(ProbeFailure::Auth);
        assert!(matches!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 1000,
                records: &[record],
                pools: &["pool-a".into()],
            }
            .check(&route()),
            Err(QuotaError::Refused("allowance exhausted"))
        ));
    }
    #[test]
    fn refusal_without_pool_keeps_bounded_wait() {
        let mut record = record(QuotaStatus::Available);
        let snapshot = record.snapshot.as_mut().unwrap();
        snapshot.observed_unix_seconds = 1000;
        snapshot.windows[0].used_basis_points = 3000;
        snapshot.windows[0].resets_unix_seconds = Some(-2600);
        snapshot.windows.push(QuotaWindow {
            window_seconds: None,
            pool: "pool-b".into(),
            used_basis_points: 1000,
            resets_unix_seconds: Some(303400),
        });
        record.refusals.push(Refusal {
            extra: Default::default(),
            account: record.account.clone(),
            model: route().model,
            pool: None,
            observed_unix_seconds: 1000,
            resets_unix_seconds: None,
        });
        for (now, allowed) in [(1000, false), (4599, false), (4600, true)] {
            assert_eq!(
                Admission {
                    policies: crate::source::TEST_POLICIES,
                    now_unix_seconds: now,
                    records: std::slice::from_ref(&record),
                    pools: &["pool-a".into(), "pool-b".into()],
                }
                .check(&route())
                .is_ok(),
                allowed,
                "now={now}"
            );
        }
    }
    #[test]
    fn recorded_model_refusal_holds_until_its_stated_reset() {
        let account = record(QuotaStatus::Available).account;
        let mut route = route();
        route.readiness = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 99_999,
        };
        let model = format!("{}/{}", account.adapter, route.model);
        for (resets, until) in [(Some(30_000), 30_000), (None, 4600)] {
            let intent =
                record_refusal(None, &account, &model, None, 1000, resets).expect("refusal");
            let record = intent.replacement.expect("record");
            for (now, allowed) in [(1000, false), (until - 1, false), (until, true)] {
                assert_eq!(
                    Admission {
                        policies: crate::source::TEST_POLICIES,
                        now_unix_seconds: now,
                        records: std::slice::from_ref(&record),
                        pools: &[],
                    }
                    .check(&route)
                    .is_ok(),
                    allowed,
                    "resets={resets:?} now={now}"
                );
            }
        }
        assert!(record_refusal(None, &account, &model, None, 1000, Some(1000)).is_err());
    }
    #[test]
    fn capacity_evidence_does_not_refuse_admission_but_quota_refusal_does() {
        let account = record(QuotaStatus::Available).account;
        let mut route = route();
        route.readiness = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 99_999,
        };
        let model = format!("{}/{}", account.adapter, route.model);
        let capacity = record_capacity_cooldown(None, &account, &model, 1000, 600)
            .unwrap()
            .unwrap()
            .replacement
            .unwrap();
        assert!(capacity.refusals.is_empty());
        let admit = |record: &AccountRecord, now| {
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: now,
                records: std::slice::from_ref(record),
                pools: &[],
            }
            .check(&route)
        };
        assert!(admit(&capacity, 1001).is_ok());
        let both = record_refusal(Some(&capacity), &account, &model, None, 1000, Some(3000))
            .unwrap()
            .replacement
            .unwrap();
        assert_eq!(both.capacity_cooldowns.len(), 1);
        assert_eq!(both.refusals.len(), 1);
        assert!(matches!(
            admit(&both, 1001),
            Err(QuotaError::Refused("scoped provider refusal"))
        ));
        assert!(matches!(
            admit(&both, 1600),
            Err(QuotaError::Refused("scoped provider refusal"))
        ));
        assert!(admit(&both, 3000).is_ok());
    }
    #[test]
    fn lapsed_window_requires_headroom_to_clear_a_newer_refusal() {
        for (used, allowed) in [(9700, false), (9600, false), (9500, false), (9499, true)] {
            let mut record = record(QuotaStatus::Available);
            let snapshot = record.snapshot.as_mut().unwrap();
            snapshot.observed_unix_seconds = 4990;
            snapshot.windows[0].used_basis_points = used;
            snapshot.windows[0].resets_unix_seconds = Some(1400);
            record.refusals.push(Refusal {
                extra: Default::default(),
                account: record.account.clone(),
                model: route().model,
                pool: Some("pool-a".into()),
                observed_unix_seconds: 4980,
                resets_unix_seconds: None,
            });
            assert_eq!(
                Admission {
                    policies: crate::source::TEST_POLICIES,
                    now_unix_seconds: 5000,
                    records: &[record],
                    pools: &["pool-a".into()],
                }
                .check(&route())
                .is_ok(),
                allowed,
                "used={used}"
            );
        }
    }
    #[test]
    fn stale_exhausted_snapshot_does_not_clear_newer_refusal() {
        let mut record = record(QuotaStatus::Exhausted);
        record.snapshot.as_mut().unwrap().observed_unix_seconds = 960;
        record.refusals.push(Refusal {
            extra: Default::default(),
            account: record.account.clone(),
            model: "exact/model".into(),
            pool: Some("pool-a".into()),
            observed_unix_seconds: 950,
            resets_unix_seconds: None,
        });
        assert!(matches!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 1300,
                records: &[record],
                pools: &["pool-a".into()],
            }
            .check(&route()),
            Err(QuotaError::Refused("scoped provider refusal"))
        ));
    }
    #[test]
    fn unknown_and_stale_do_not_manufacture_exhaustion() {
        for (now, status) in [
            (1000, QuotaStatus::Unknown),
            (1000, QuotaStatus::Stale),
            (1300, QuotaStatus::Exhausted),
        ] {
            let records = [record(status)];
            let pools = ["pool-a".into()];
            let context = Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: now,
                records: &records,
                pools: &pools,
            };
            assert!(context.check(&route()).is_ok());
        }
    }
    #[test]
    fn independent_pool_reset_and_refusal_scope() {
        let mut records = [record(QuotaStatus::Exhausted)];
        let mut pools = ["pool-b".into()];
        let r = route();
        assert!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 1000,
                records: &records,
                pools: &pools
            }
            .check(&r)
            .is_ok()
        );
        pools[0] = "pool-a".into();
        assert!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 1000,
                records: &records,
                pools: &pools
            }
            .check(&r)
            .is_err()
        );
        records[0].snapshot.as_mut().unwrap().windows[0].resets_unix_seconds = Some(999);
        assert!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 1000,
                records: &records,
                pools: &pools
            }
            .check(&r)
            .is_ok()
        );
        records[0].refusals.push(Refusal {
            extra: Default::default(),
            account: records[0].account.clone(),
            model: r.model.clone(),
            pool: Some("pool-a".into()),
            observed_unix_seconds: 950,
            resets_unix_seconds: None,
        });
        records[0].snapshot.as_mut().unwrap().status = QuotaStatus::Unknown;
        assert!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 1000,
                records: &records,
                pools: &pools
            }
            .check(&r)
            .is_err()
        );
        assert!(
            Admission {
                policies: crate::source::TEST_POLICIES,
                now_unix_seconds: 4550,
                records: &records,
                pools: &pools
            }
            .check(&r)
            .is_ok()
        );
    }
}

#[cfg(test)]
mod refusal_scope_tests {
    use super::*;
    #[test]
    fn refusals_expire_only_for_matching_identity_pool_observation_or_reset() {
        let route = ResolvedRoute {
            extra: Default::default(),
            adapter: "claude".into(),
            model: "exact".into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: Some("work".into()),
            host: "host".into(),
            alternatives: Default::default(),
            exclusions: vec![],
            eligibility: provenant_records::RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                extra: Default::default(),
                availability_ref: "fixture".into(),
                expires_unix_seconds: 5000,
            },
            confidentiality: provenant_records::RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        };
        let account = AccountScope {
            adapter: "claude".into(),
            account_key: "work".into(),
            label: "work".into(),
        };
        let refusal = Refusal {
            extra: Default::default(),
            account: account.clone(),
            model: "exact".into(),
            pool: Some("pool".into()),
            observed_unix_seconds: 950,
            resets_unix_seconds: Some(1100),
        };
        let pools = ["pool".into()];
        for (model, key, pool, observed, now, allowed) in [
            ("other", "work", "pool", 900, 1000, true),
            ("exact", "other", "pool", 900, 1000, true),
            ("exact", "work", "pool", 900, 1100, true),
            ("exact", "work", "pool", 960, 1000, true),
            ("exact", "work", "other", 960, 1000, false),
            ("exact", "work", "pool", 2000, 1000, false),
        ] {
            let mut refusal = refusal.clone();
            refusal.model = model.into();
            let mut route = route.clone();
            route.account_key = Some(key.into());
            let mut record = AccountRecord::empty(account.clone());
            record.refusals.push(refusal);
            record.snapshot = Some(QuotaSnapshot {
                schema_version: 1,
                account: account.clone(),
                revision: 0,
                status: QuotaStatus::Available,
                observed_unix_seconds: observed,
                source_ref: "fixture".into(),
                authority_ref: None,
                details: None,
                windows: vec![QuotaWindow {
                    window_seconds: None,
                    pool: pool.into(),
                    used_basis_points: 0,
                    resets_unix_seconds: Some(2000),
                }],
            });
            let records = [record];
            assert_eq!(
                Admission {
                    policies: crate::source::TEST_POLICIES,
                    now_unix_seconds: now,
                    records: &records,
                    pools: &pools
                }
                .check(&route)
                .is_ok(),
                allowed,
                "{model}/{key}/{pool}/{observed}/{now}"
            );
        }
        let mut route = route.clone();
        route.adapter = "agy".into();
        let mut account = account;
        account.adapter = "agy".into();
        let mut refusal = refusal;
        refusal.account = account.clone();
        refusal.pool = Some("gemini".into());
        let mut record = AccountRecord::empty(account.clone());
        record.refusals.push(refusal);
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status: QuotaStatus::Available,
            observed_unix_seconds: 960,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![
                QuotaWindow {
                    pool: "gemini.weekly".into(),
                    window_seconds: Some(604800),
                    used_basis_points: 170,
                    resets_unix_seconds: Some(3000),
                },
                QuotaWindow {
                    pool: "gemini.5h".into(),
                    window_seconds: Some(18000),
                    used_basis_points: 670,
                    resets_unix_seconds: Some(2000),
                },
            ],
        });
        for (weekly, allowed) in [(170, true), (10000, false)] {
            record.snapshot.as_mut().unwrap().windows[0].used_basis_points = weekly;
            assert_eq!(
                Admission {
                    policies: crate::source::TEST_POLICIES,
                    now_unix_seconds: 1000,
                    records: &[record.clone()],
                    pools: &["gemini".into()]
                }
                .check(&route)
                .is_ok(),
                allowed
            );
        }
    }
    #[test]
    fn grouped_pool_exhaustion_blocks_only_models_in_that_pool() {
        let route = |model: &str| ResolvedRoute {
            extra: Default::default(),
            adapter: "agy".into(),
            model: model.into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: Some("work".into()),
            host: "host".into(),
            alternatives: Default::default(),
            exclusions: vec![],
            eligibility: provenant_records::RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                extra: Default::default(),
                availability_ref: "fixture".into(),
                expires_unix_seconds: 5000,
            },
            confidentiality: provenant_records::RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        };
        let account = AccountScope {
            adapter: "agy".into(),
            account_key: "work".into(),
            label: "work".into(),
        };
        let window = |pool: &str, used| QuotaWindow {
            pool: pool.into(),
            window_seconds: Some(604800),
            used_basis_points: used,
            resets_unix_seconds: Some(3000),
        };
        for (gemini, claude) in [(10000, 0), (0, 10000)] {
            let mut record = AccountRecord::empty(account.clone());
            record.snapshot = Some(QuotaSnapshot {
                schema_version: 1,
                account: account.clone(),
                revision: 0,
                status: QuotaStatus::Available,
                observed_unix_seconds: 960,
                source_ref: "fixture".into(),
                authority_ref: None,
                details: None,
                windows: vec![
                    window("gemini.weekly", gemini),
                    window("claude.weekly", claude),
                ],
            });
            for (model, pool) in [
                ("gemini-3.8-flash", "gemini"),
                ("claude-sonnet-4-6", "claude"),
            ] {
                let admitted = Admission {
                    policies: crate::source::TEST_POLICIES,
                    now_unix_seconds: 1000,
                    records: &[record.clone()],
                    pools: &[pool.into()],
                }
                .check(&route(model))
                .is_ok();
                let exhausted = if pool == "gemini" { gemini } else { claude } >= 10000;
                assert_eq!(
                    admitted, !exhausted,
                    "{model} gemini={gemini} claude={claude}"
                );
            }
        }
    }
}
