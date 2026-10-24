//! Admission facts are constructed from current validated evidence, never decoded.
use crate::*;
use provenant_core::OperationId;
use provenant_records::{ResolvedRoute, RouteReadiness};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    pub account: AccountScope,
    pub model: String,
    pub pool: Option<String>,
    pub observed_unix_seconds: i64,
    pub resets_unix_seconds: Option<i64>,
}
#[derive(Debug, Serialize)]
pub struct AdmissionPermit {
    operation_id: OperationId,
    adapter: String,
    model: String,
    account_key: String,
    snapshot_revision: Option<u64>,
    expires_unix_seconds: i64,
}
pub struct Admission<'a> {
    pub now_unix_seconds: i64,
    pub records: &'a [AccountRecord],
    pub refusals: &'a [Refusal],
    /// Only wire-verified model-to-pool bindings. An absent binding cannot
    /// turn an unrelated pool into a model-wide exhaustion.
    pub pools: &'a [String],
}
impl Admission<'_> {
    pub fn admit(
        &self,
        route: &ResolvedRoute,
        operation: &OperationId,
    ) -> Result<AdmissionPermit, QuotaError> {
        let expiry = match route.readiness {
            RouteReadiness::Ready {
                expires_unix_seconds,
                ..
            } if expires_unix_seconds > self.now_unix_seconds => expires_unix_seconds,
            _ => return Err(QuotaError::Refused("provider readiness")),
        };
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
        let snapshot = record.and_then(|r| r.snapshot.as_ref());
        let fresh = snapshot.is_some_and(|s| {
            s.observed_unix_seconds <= self.now_unix_seconds
                && self
                    .now_unix_seconds
                    .saturating_sub(s.observed_unix_seconds)
                    < schedule::interval(
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
                self.pools
                    .iter()
                    .any(|pool| matches_pool(&route.adapter, &w.pool, pool))
                    && w.used_basis_points >= 10000
                    && w.resets_unix_seconds
                        .is_none_or(|r| r > self.now_unix_seconds)
            }) {
                return Err(QuotaError::Refused("allowance exhausted"));
            }
        }
        for refusal in self
            .refusals
            .iter()
            .chain(record.into_iter().flat_map(|r| r.refusals.iter()))
            .filter(|r| {
                r.account.adapter == route.adapter
                    && r.account.account_key == *key
                    && r.model == route.model
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
                                    w.used_basis_points < 10000
                                        || w.resets_unix_seconds
                                            .is_some_and(|r| r <= self.now_unix_seconds)
                                })
                        })
                });
            if expires > self.now_unix_seconds
                && !comparable_newer
                && refusal.pool.as_ref().is_none_or(|p| self.pools.contains(p))
            {
                return Err(QuotaError::Refused("scoped provider refusal"));
            }
        }
        Ok(AdmissionPermit {
            operation_id: operation.clone(),
            adapter: route.adapter.clone(),
            model: route.model.clone(),
            account_key: key.clone(),
            snapshot_revision: snapshot.map(|s| s.revision),
            expires_unix_seconds: expiry.min(
                self.now_unix_seconds
                    .checked_add(55)
                    .ok_or(QuotaError::TimeRange)?,
            ),
        })
    }
}

fn matches_pool(adapter: &str, window: &str, pool: &str) -> bool {
    window == pool
        || (adapter == "agy"
            && window
                .split_once('.')
                .is_some_and(|(group, _)| group == pool))
}

impl AdmissionPermit {
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    pub fn adapter(&self) -> &str {
        &self.adapter
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn account_key(&self) -> &str {
        &self.account_key
    }
    pub fn snapshot_revision(&self) -> Option<u64> {
        self.snapshot_revision
    }
    pub fn expires_unix_seconds(&self) -> i64 {
        self.expires_unix_seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                availability_ref: "fixture".into(),
                expires_unix_seconds: 9999,
            },
            confidentiality: provenant_records::RouteConfidentiality {
                evidence_ref: "fixture".into(),
            },
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
    fn failed_probe_preserves_recent_exhaustion() {
        let mut record = record(QuotaStatus::Exhausted);
        record.last_error = Some(ProbeFailure::Auth);
        assert!(matches!(
            Admission {
                now_unix_seconds: 1000,
                records: &[record],
                refusals: &[],
                pools: &["pool-a".into()],
            }
            .admit(&route(), &"operation".parse().unwrap()),
            Err(QuotaError::Refused("allowance exhausted"))
        ));
    }
    #[test]
    fn stale_exhausted_snapshot_does_not_clear_newer_refusal() {
        let mut record = record(QuotaStatus::Exhausted);
        record.snapshot.as_mut().unwrap().observed_unix_seconds = 960;
        let refusal = Refusal {
            account: record.account.clone(),
            model: "exact/model".into(),
            pool: Some("pool-a".into()),
            observed_unix_seconds: 950,
            resets_unix_seconds: None,
        };
        assert!(matches!(
            Admission {
                now_unix_seconds: 1300,
                records: &[record],
                refusals: &[refusal],
                pools: &["pool-a".into()],
            }
            .admit(&route(), &"operation".parse().unwrap()),
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
                now_unix_seconds: now,
                records: &records,
                refusals: &[],
                pools: &pools,
            };
            assert!(
                context
                    .admit(&route(), &"operation".parse().unwrap())
                    .is_ok()
            );
        }
    }
    #[test]
    fn independent_pool_reset_and_refusal_scope() {
        let mut records = [record(QuotaStatus::Exhausted)];
        let mut pools = ["pool-b".into()];
        let op = "operation".parse().unwrap();
        let r = route();
        assert!(
            Admission {
                now_unix_seconds: 1000,
                records: &records,
                refusals: &[],
                pools: &pools
            }
            .admit(&r, &op)
            .is_ok()
        );
        pools[0] = "pool-a".into();
        assert!(
            Admission {
                now_unix_seconds: 1000,
                records: &records,
                refusals: &[],
                pools: &pools
            }
            .admit(&r, &op)
            .is_err()
        );
        records[0].snapshot.as_mut().unwrap().windows[0].resets_unix_seconds = Some(999);
        assert!(
            Admission {
                now_unix_seconds: 1000,
                records: &records,
                refusals: &[],
                pools: &pools
            }
            .admit(&r, &op)
            .is_ok()
        );
        let refusal = Refusal {
            account: records[0].account.clone(),
            model: r.model.clone(),
            pool: Some("pool-a".into()),
            observed_unix_seconds: 950,
            resets_unix_seconds: None,
        };
        records[0].snapshot.as_mut().unwrap().status = QuotaStatus::Unknown;
        assert!(
            Admission {
                now_unix_seconds: 1000,
                records: &records,
                refusals: std::slice::from_ref(&refusal),
                pools: &pools
            }
            .admit(&r, &op)
            .is_err()
        );
        assert!(
            Admission {
                now_unix_seconds: 4550,
                records: &records,
                refusals: &[refusal],
                pools: &pools
            }
            .admit(&r, &op)
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
                availability_ref: "fixture".into(),
                expires_unix_seconds: 5000,
            },
            confidentiality: provenant_records::RouteConfidentiality {
                evidence_ref: "fixture".into(),
            },
        };
        let account = AccountScope {
            adapter: "claude".into(),
            account_key: "work".into(),
            label: "work".into(),
        };
        let refusal = Refusal {
            account: account.clone(),
            model: "exact".into(),
            pool: Some("pool".into()),
            observed_unix_seconds: 950,
            resets_unix_seconds: Some(1100),
        };
        let op = "operation".parse().unwrap();
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
            refusal.account.account_key = key.into();
            let mut record = AccountRecord::empty(account.clone());
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
                    now_unix_seconds: now,
                    records: &records,
                    refusals: &[refusal],
                    pools: &pools
                }
                .admit(&route, &op)
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
                    now_unix_seconds: 1000,
                    records: &[record.clone()],
                    refusals: &[refusal.clone()],
                    pools: &["gemini".into()]
                }
                .admit(&route, &op)
                .is_ok(),
                allowed
            );
        }
    }
}
