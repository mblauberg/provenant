//! Scheduling uses explicit integer seconds; it never sleeps or executes a probe.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Timer,
    Panel,
    Manual,
    Wake,
    Power,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    Ac,
    Battery,
    Low,
}
#[derive(Clone, Debug)]
pub struct QuotaQuery {
    pub adapter: Option<String>,
}
#[derive(Clone, Debug)]
pub struct QuotaTick {
    pub adapter: Option<String>,
    pub trigger: Trigger,
    pub power: Power,
}

pub const TICK_BUDGET_SECONDS: i64 = 55;
pub fn interval(
    policies: &[QuotaPolicy],
    adapter: &str,
    trigger: Trigger,
    power: Power,
) -> Option<u64> {
    let policy = policies.iter().find(|policy| policy.adapter == adapter)?;
    if !policy.low_power && power == Power::Low {
        return None;
    }
    let base = match trigger {
        Trigger::Manual => 30,
        Trigger::Panel => policy.panel_seconds,
        _ => policy.timer_seconds,
    };
    Some(
        base * match power {
            Power::Ac => 1,
            Power::Battery => 2,
            Power::Low => 4,
        },
    )
}
fn add(now: i64, seconds: u64) -> Result<i64, QuotaError> {
    now.checked_add(i64::try_from(seconds).map_err(|_| QuotaError::TimeRange)?)
        .ok_or(QuotaError::TimeRange)
}
pub fn next_due(
    policies: &[QuotaPolicy],
    record: &AccountRecord,
    trigger: Trigger,
    power: Power,
) -> Result<Option<i64>, QuotaError> {
    let Some(ttl) = interval(policies, &record.account.adapter, trigger, power) else {
        return Ok(None);
    };
    let mut due = match record.last_attempt_unix_seconds {
        Some(at) => add(at, ttl)?,
        None => i64::MIN,
    };
    if let Some(backoff) = record.backoff_until_unix_seconds {
        due = due.max(backoff);
    }
    if let Some(claim) = &record.claim {
        due = due.max(claim.deadline_unix_seconds);
    }
    Ok(Some(due))
}
pub fn backoff(failure: ProbeFailure, failures: u64, retry_after_seconds: Option<u64>) -> u64 {
    let base = match failure {
        ProbeFailure::Network | ProbeFailure::RateLimited => 120_u64
            .saturating_mul(1_u64 << failures.saturating_sub(1).min(5))
            .min(3600),
        ProbeFailure::Auth => 3600,
        ProbeFailure::MissingCli | ProbeFailure::Disabled => 86400,
        ProbeFailure::Malformed | ProbeFailure::ProbeDirectory => 120,
    };
    base.max(retry_after_seconds.unwrap_or(0).min(86400))
}
pub fn reserve(
    policies: &[QuotaPolicy],
    current: Option<&AccountRecord>,
    account: &AccountScope,
    now: i64,
    token: &str,
    trigger: Trigger,
    power: Power,
) -> Result<UpdateIntent, QuotaError> {
    account.validate()?;
    if !component(token) {
        return Err(QuotaError::Invalid("claim token"));
    }
    let mut record = current
        .cloned()
        .unwrap_or_else(|| AccountRecord::empty(account.clone()));
    record.validate()?;
    // Identity is adapter and key; a changed display label is adopted below.
    if record.account.adapter != account.adapter
        || record.account.account_key != account.account_key
        || next_due(policies, &record, trigger, power)?.is_none_or(|due| due > now)
    {
        return Err(QuotaError::Conflict);
    }
    let stored_account = record.account.clone();
    record.account = account.clone();
    if let Some(snapshot) = record.snapshot.as_mut() {
        snapshot.account = account.clone();
    }
    for refusal in &mut record.refusals {
        refusal.account = account.clone();
    }
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or(QuotaError::NumberOutOfRange)?;
    record.claim = Some(RefreshClaim {
        token: token.into(),
        deadline_unix_seconds: add(now, TICK_BUDGET_SECONDS as u64)?,
    });
    record.backoff_until_unix_seconds = None;
    record.last_attempt_unix_seconds = Some(now);
    record.validate()?;
    Ok(UpdateIntent {
        account: stored_account,
        expected: current.cloned(),
        replacement: Some(record),
    })
}
pub fn complete(
    current: &AccountRecord,
    token: &str,
    now: i64,
    outcome: Result<QuotaSnapshot, ProbeFailure>,
    retry_after_seconds: Option<u64>,
) -> Result<UpdateIntent, QuotaError> {
    current.validate()?;
    if current.last_attempt_unix_seconds.is_none_or(|t| t > now)
        || current
            .claim
            .as_ref()
            .is_none_or(|c| c.token != token || c.deadline_unix_seconds <= now)
    {
        return Err(QuotaError::Conflict);
    }
    let mut next = current.clone();
    next.revision = next
        .revision
        .checked_add(1)
        .ok_or(QuotaError::NumberOutOfRange)?;
    next.claim = None;
    match outcome {
        Ok(mut snapshot) => {
            snapshot.validate()?;
            if snapshot.account != current.account
                || snapshot.observed_unix_seconds > now
                || current
                    .last_attempt_unix_seconds
                    .is_some_and(|at| snapshot.observed_unix_seconds < at)
            {
                return Err(QuotaError::Invalid("probe observation"));
            }
            snapshot.revision = next.revision;
            next.snapshot = Some(snapshot);
            next.failures = 0;
            next.last_error = None;
            next.backoff_until_unix_seconds = None;
        }
        Err(failure) => {
            next.failures = next.failures.saturating_add(1);
            next.last_error = Some(failure);
            next.backoff_until_unix_seconds = Some(add(
                now,
                backoff(failure, next.failures, retry_after_seconds),
            )?);
        }
    }
    next.validate()?;
    Ok(UpdateIntent {
        account: current.account.clone(),
        expected: Some(current.clone()),
        replacement: Some(next),
    })
}
/// `None` is unknown detection, never an authoritative empty account list.
#[derive(Clone, Debug)]
pub struct Detection {
    pub adapter: String,
    pub accounts: Option<Vec<AccountScope>>,
    pub status: QuotaStatus,
}
pub struct TickPlan {
    pub drops: Vec<UpdateIntent>,
    /// Three cheap probes per batch; Agy alone. The edge observes the shared deadline.
    pub batches: Vec<Vec<AccountScope>>,
    pub deadline_unix_seconds: i64,
}
pub fn plan_tick(
    policies: &[QuotaPolicy],
    records: &[AccountRecord],
    detections: &[Detection],
    tick: &QuotaTick,
    now: i64,
) -> Result<TickPlan, QuotaError> {
    plan_tick_with_settings(
        policies,
        records,
        detections,
        tick,
        now,
        &Settings::default(),
    )
}
pub fn plan_tick_with_settings(
    policies: &[QuotaPolicy],
    records: &[AccountRecord],
    detections: &[Detection],
    tick: &QuotaTick,
    now: i64,
    settings: &Settings,
) -> Result<TickPlan, QuotaError> {
    use provenant_config::DomainConfig;
    if !settings.validate().is_empty() {
        return Err(QuotaError::Invalid("scheduling settings"));
    }
    validate_inputs(records, detections)?;
    let selected = |id: &str| tick.adapter.as_ref().is_none_or(|s| s == id);
    let mut drops = Vec::new();
    for record in records {
        // Keep past Google accounts so returning to a sign-in restores its cooldown,
        // but drop duplicate legacy keys that share a fingerprint with a live account.
        let retaining = provenant_records::AdapterSpec::get(&record.account.adapter)
            .is_some_and(|spec| spec.retain_quota_accounts);
        let absent_from_detection = detections.iter().any(|d| {
            d.adapter == record.account.adapter
                && d.accounts
                    .as_ref()
                    .map_or(d.status == QuotaStatus::Unsupported, |accounts| {
                        !accounts.iter().any(|a| {
                            a.adapter == record.account.adapter
                                && a.account_key == record.account.account_key
                        })
                    })
        });
        let duplicate_fingerprint = retaining
            && record
                .snapshot
                .as_ref()
                .and_then(|s| s.details.as_ref())
                .and_then(|d| d.fingerprint.as_deref())
                .is_some_and(|fp| {
                    records.iter().any(|other| {
                        other.account != record.account
                            && other.account.adapter == record.account.adapter
                            && detections.iter().any(|d| {
                                d.adapter == other.account.adapter
                                    && d.accounts.as_ref().is_some_and(|accounts| {
                                        accounts.iter().any(|a| {
                                            a.adapter == other.account.adapter
                                                && a.account_key == other.account.account_key
                                        })
                                    })
                            })
                            && other
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.details.as_ref())
                                .and_then(|d| d.fingerprint.as_deref())
                                == Some(fp)
                    })
                });
        if selected(&record.account.adapter)
            && ((!retaining && absent_from_detection)
                || (absent_from_detection && duplicate_fingerprint))
        {
            drops.push(UpdateIntent {
                account: record.account.clone(),
                expected: Some(record.clone()),
                replacement: None,
            });
        }
    }
    let mut cheap = Vec::new();
    let mut solo = Vec::new();
    for account in detections
        .iter()
        .filter(|d| selected(&d.adapter) && d.status != QuotaStatus::AuthRequired)
        .filter_map(|d| d.accounts.as_ref())
        .flatten()
    {
        let current = records.iter().find(|r| {
            r.account.adapter == account.adapter && r.account.account_key == account.account_key
        });
        let due = match current {
            Some(record) => next_due(policies, record, tick.trigger, tick.power)?,
            None => {
                interval(policies, &account.adapter, tick.trigger, tick.power).map(|_| i64::MIN)
            }
        };
        if due.is_some_and(|due| due <= now) {
            if policies
                .iter()
                .any(|policy| policy.adapter == account.adapter && policy.solo)
            {
                solo.push(vec![account.clone()]);
            } else {
                cheap.push(account.clone());
            }
        }
    }
    let mut batches = cheap
        .chunks(settings.max_parallel as usize)
        .map(<[AccountScope]>::to_vec)
        .collect::<Vec<_>>();
    batches.extend(solo);
    Ok(TickPlan {
        drops,
        batches,
        deadline_unix_seconds: add(now, settings.wall_budget_seconds)?,
    })
}
fn validate_inputs(records: &[AccountRecord], detections: &[Detection]) -> Result<(), QuotaError> {
    if records.len() > MAX_ACCOUNTS || detections.len() > MAX_ACCOUNTS {
        return Err(QuotaError::Invalid("account bound"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for record in records {
        record.validate()?;
        if !seen.insert((&record.account.adapter, &record.account.account_key)) {
            return Err(QuotaError::Invalid("duplicate account"));
        }
    }
    seen.clear();
    let mut adapters = std::collections::BTreeSet::new();
    let mut count = 0;
    for detection in detections {
        if detection.accounts.is_some()
            && matches!(
                detection.status,
                QuotaStatus::Unknown | QuotaStatus::Unsupported | QuotaStatus::Stale
            )
        {
            return Err(QuotaError::Invalid(
                "failed detection cannot assert accounts",
            ));
        }
        if !component(&detection.adapter) || !adapters.insert(&detection.adapter) {
            return Err(QuotaError::Invalid("duplicate adapter"));
        }
        for account in detection.accounts.iter().flatten() {
            account.validate()?;
            count += 1;
            if count > MAX_ACCOUNTS
                || account.adapter != detection.adapter
                || !seen.insert((&account.adapter, &account.account_key))
            {
                return Err(QuotaError::Invalid("detected account"));
            }
        }
    }
    Ok(())
}
pub fn report(
    policies: &[QuotaPolicy],
    records: &[AccountRecord],
    detections: &[Detection],
    query: &QuotaQuery,
    now: i64,
    path: String,
) -> Result<QuotaReport, QuotaError> {
    validate_inputs(records, detections)?;
    let selected = |id: &str| query.adapter.as_ref().is_none_or(|s| s == id);
    let mut accounts = Vec::new();
    for record in records.iter().filter(|r| selected(&r.account.adapter)) {
        if !detections.is_empty()
            && !detections.iter().any(|d| {
                d.adapter == record.account.adapter
                    && d.accounts.as_ref().is_some_and(|accounts| {
                        accounts.iter().any(|a| {
                            a.adapter == record.account.adapter
                                && a.account_key == record.account.account_key
                        })
                    })
            })
        {
            continue;
        }
        let age = record.snapshot.as_ref().map(|s| {
            u64::try_from(now.saturating_sub(s.observed_unix_seconds).max(0)).unwrap_or(u64::MAX)
        });
        let auth_required = detections
            .iter()
            .any(|d| d.adapter == record.account.adapter && d.status == QuotaStatus::AuthRequired);
        let fresh = record.snapshot.as_ref().is_some_and(|s| {
            s.observed_unix_seconds <= now
                && record.last_error.is_none()
                && !auth_required
                && age.is_some_and(|a| {
                    a < interval(policies, &record.account.adapter, Trigger::Timer, Power::Ac)
                        .unwrap_or(0)
                })
        });
        let status = match &record.snapshot {
            _ if auth_required => QuotaStatus::AuthRequired,
            Some(snapshot) if fresh => snapshot.status,
            Some(_) => QuotaStatus::Stale,
            None if record.last_error == Some(ProbeFailure::Auth) => QuotaStatus::AuthRequired,
            None if record.last_error == Some(ProbeFailure::Disabled) => QuotaStatus::Unsupported,
            None => QuotaStatus::Unknown,
        };
        accounts.push(AccountRow {
            account: record.account.clone(),
            snapshot: record.snapshot.clone(),
            status,
            age_seconds: age,
            next_due_unix_seconds: next_due(policies, record, Trigger::Timer, Power::Ac)?
                .map(|d| d.max(now)),
            refreshing: record
                .claim
                .as_ref()
                .is_some_and(|c| c.deadline_unix_seconds > now),
            error: record.last_error,
            display: None,
        });
    }
    let sources = detections
        .iter()
        .filter(|d| selected(&d.adapter) && d.accounts.as_ref().is_none_or(Vec::is_empty))
        .map(|d| SourceRow {
            adapter: d.adapter.clone(),
            status: d.status,
            error: (d.status == QuotaStatus::Unsupported).then_some(ProbeFailure::Disabled),
            next_due_unix_seconds: None,
        })
        .collect::<Vec<_>>();
    let known = accounts
        .iter()
        .filter(|a| {
            matches!(
                a.status,
                QuotaStatus::Available | QuotaStatus::CreditOnly | QuotaStatus::Exhausted
            )
        })
        .count() as u64;
    let refreshing_ids = accounts
        .iter()
        .filter(|a| a.refreshing)
        .map(|a| format!("{}/{}", a.account.account_key, a.account.adapter))
        .collect();
    let next_due_unix_seconds = accounts
        .iter()
        .filter_map(|a| a.next_due_unix_seconds)
        .min();
    Ok(QuotaReport {
        schema_version: 1,
        generated_unix_seconds: now,
        coverage: Coverage {
            total: accounts.len() as u64,
            known,
        },
        accounts,
        sources,
        refreshing_ids,
        next_due_unix_seconds,
        report_path: path,
        display: crate::Display::default(),
        display_icon_warning: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn account() -> AccountScope {
        AccountScope {
            adapter: "claude".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        }
    }
    #[test]
    fn manual_respects_floor_backoff_and_expired_claims() {
        let mut r = AccountRecord::empty(account());
        r.last_attempt_unix_seconds = Some(100);
        r.backoff_until_unix_seconds = Some(400);
        r.claim = Some(RefreshClaim {
            token: "old".into(),
            deadline_unix_seconds: 155,
        });
        assert_eq!(
            next_due(crate::source::TEST_POLICIES, &r, Trigger::Manual, Power::Ac).unwrap(),
            Some(400)
        );
        let claim = reserve(
            crate::source::TEST_POLICIES,
            Some(&r),
            &r.account,
            400,
            "new",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap();
        assert!(claim.check(Some(&r)).is_ok());
        assert_eq!(
            claim
                .replacement
                .as_ref()
                .unwrap()
                .claim
                .as_ref()
                .unwrap()
                .deadline_unix_seconds,
            455
        );
        assert!(matches!(
            reserve(
                crate::source::TEST_POLICIES,
                Some(&r),
                &r.account,
                399,
                "new",
                Trigger::Manual,
                Power::Ac
            ),
            Err(QuotaError::Conflict)
        ));
    }
    #[test]
    fn backoff_has_bounded_exponential_and_retry_after_floor() {
        assert_eq!(backoff(ProbeFailure::Network, 1, None), 120);
        assert_eq!(backoff(ProbeFailure::RateLimited, 50, None), 3600);
        assert_eq!(backoff(ProbeFailure::Network, 1, Some(999999)), 86400);
        assert_eq!(backoff(ProbeFailure::Auth, 1, None), 3600);
        assert_eq!(backoff(ProbeFailure::MissingCli, 1, None), 86400);
        assert_eq!(backoff(ProbeFailure::ProbeDirectory, 1, None), 120);
    }
    #[test]
    fn reserve_adopts_a_renamed_label_for_the_same_account_key() {
        let a = account();
        let mut renamed = a.clone();
        renamed.label = "renamed".into();
        let r = AccountRecord::empty(a.clone());
        let intent = reserve(
            crate::source::TEST_POLICIES,
            Some(&r),
            &renamed,
            400,
            "claim",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap();
        intent.check(Some(&r)).unwrap();
        let reserved = intent.replacement.unwrap();
        assert_eq!(reserved.account, renamed);
        let done = complete(&reserved, "claim", 420, Err(ProbeFailure::Auth), None).unwrap();
        assert_eq!(done.replacement.unwrap().account, renamed);
    }
    #[test]
    fn completion_rejects_late_claim_and_keeps_good_snapshot_on_failure() {
        let a = account();
        let mut r = AccountRecord::empty(a.clone());
        r.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: a.clone(),
            revision: 0,
            status: QuotaStatus::Available,
            observed_unix_seconds: 50,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![],
        });
        let intent = reserve(
            crate::source::TEST_POLICIES,
            Some(&r),
            &a,
            400,
            "claim",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap();
        let current = intent.replacement.unwrap();
        assert!(matches!(
            complete(&current, "claim", 456, Err(ProbeFailure::Network), None),
            Err(QuotaError::Conflict)
        ));
        let failed = complete(&current, "claim", 420, Err(ProbeFailure::Auth), None).unwrap();
        assert_eq!(failed.replacement.as_ref().unwrap().snapshot, r.snapshot);
        let row = report(
            crate::source::TEST_POLICIES,
            &[failed.replacement.clone().unwrap()],
            &[],
            &QuotaQuery { adapter: None },
            420,
            "report.json".into(),
        )
        .unwrap()
        .accounts
        .remove(0);
        assert_eq!(row.status, QuotaStatus::Stale);
        assert_eq!(row.error, Some(ProbeFailure::Auth));
        assert_eq!(row.snapshot.unwrap().observed_unix_seconds, 50);
        assert!(failed.check(Some(&r)).is_err());
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;
    fn account(adapter: &str, key: &str) -> AccountScope {
        AccountScope {
            adapter: adapter.into(),
            account_key: key.into(),
            label: "work".into(),
        }
    }
    #[test]
    fn label_change_keeps_account_and_refresh_schedule() {
        let mut record = AccountRecord::empty(account("claude", "work"));
        record.last_attempt_unix_seconds = Some(90);
        let mut renamed = record.account.clone();
        renamed.label = "renamed".into();
        let plan = plan_tick(
            crate::source::TEST_POLICIES,
            &[record],
            &[Detection {
                adapter: "claude".into(),
                accounts: Some(vec![renamed]),
                status: QuotaStatus::Available,
            }],
            &QuotaTick {
                adapter: None,
                trigger: Trigger::Timer,
                power: Power::Ac,
            },
            100,
        )
        .unwrap();
        assert!(plan.drops.is_empty());
        assert!(plan.batches.is_empty());
    }
    #[test]
    fn detection_removes_only_authoritatively_absent_accounts_and_batches_probes() {
        let records = [
            AccountRecord::empty(account("claude", "old")),
            AccountRecord::empty(account("agy", "old")),
        ];
        let tick = QuotaTick {
            adapter: None,
            trigger: Trigger::Manual,
            power: Power::Ac,
        };
        let unknown = [Detection {
            adapter: "claude".into(),
            accounts: None,
            status: QuotaStatus::Unknown,
        }];
        assert!(
            plan_tick(crate::source::TEST_POLICIES, &records, &unknown, &tick, 100)
                .unwrap()
                .drops
                .is_empty()
        );
        let detected = [
            Detection {
                adapter: "claude".into(),
                accounts: Some(
                    ["a", "b", "c", "d"]
                        .map(|key| account("claude", key))
                        .into(),
                ),
                status: QuotaStatus::Available,
            },
            Detection {
                adapter: "agy".into(),
                accounts: Some(vec![account("agy", "a")]),
                status: QuotaStatus::Available,
            },
        ];
        let plan = plan_tick(
            crate::source::TEST_POLICIES,
            &records,
            &detected,
            &tick,
            100,
        )
        .unwrap();
        assert_eq!(plan.drops.len(), 1);
        assert_eq!(
            plan.batches.iter().map(Vec::len).collect::<Vec<_>>(),
            [3, 1, 1]
        );
        assert_eq!(plan.deadline_unix_seconds, 155);
        let low = QuotaTick {
            power: Power::Low,
            ..tick
        };
        assert!(
            plan_tick(crate::source::TEST_POLICIES, &records, &detected, &low, 100)
                .unwrap()
                .batches
                .iter()
                .flatten()
                .all(|a| a.adapter != "agy")
        );
    }
    #[test]
    fn competing_claims_and_stale_completions_cannot_overwrite() {
        let r = AccountRecord::empty(account("claude", "work"));
        let first = reserve(
            crate::source::TEST_POLICIES,
            Some(&r),
            &r.account,
            100,
            "first",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap();
        let second = reserve(
            crate::source::TEST_POLICIES,
            Some(&r),
            &r.account,
            100,
            "second",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap();
        let claimed = first.replacement.as_ref().unwrap();
        assert!(second.check(Some(claimed)).is_err());
        assert!(
            reserve(
                crate::source::TEST_POLICIES,
                Some(claimed),
                &r.account,
                110,
                "third",
                Trigger::Manual,
                Power::Ac
            )
            .is_err()
        );
        let next = reserve(
            crate::source::TEST_POLICIES,
            Some(claimed),
            &r.account,
            160,
            "third",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap()
        .replacement
        .unwrap();
        let late = complete(claimed, "first", 120, Err(ProbeFailure::Network), None).unwrap();
        assert!(late.check(Some(&next)).is_err());
        // Detection removed the first claim; the same account now starts a new
        // refresh with the same revision but a different claim identity.
        let recreated = reserve(
            crate::source::TEST_POLICIES,
            None,
            &r.account,
            125,
            "recreated",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap()
        .replacement
        .unwrap();
        assert_eq!(recreated.revision, claimed.revision);
        assert!(late.check(Some(&recreated)).is_err());

        assert!(matches!(
            reserve(
                crate::source::TEST_POLICIES,
                Some(&AccountRecord {
                    revision: u64::MAX,
                    ..r.clone()
                }),
                &r.account,
                100,
                "overflow",
                Trigger::Manual,
                Power::Ac
            ),
            Err(QuotaError::NumberOutOfRange)
        ));
    }
    #[test]
    fn expired_claim_is_not_rendered_refreshing_and_bad_state_is_rejected() {
        let mut record = AccountRecord::empty(account("claude", "work"));
        record.claim = Some(RefreshClaim {
            token: "old".into(),
            deadline_unix_seconds: 100,
        });
        record.last_attempt_unix_seconds = Some(45);
        let report = report(
            crate::source::TEST_POLICIES,
            &[record.clone()],
            &[],
            &QuotaQuery { adapter: None },
            100,
            "fixture".into(),
        )
        .unwrap();
        assert!(report.refreshing_ids.is_empty());
        assert!(!report.accounts[0].refreshing);
        let mut value = serde_json::to_value(&record).unwrap();
        value["revision"] = serde_json::json!("18446744073709551616");
        assert!(AccountRecord::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        record.account.account_key = "../escape".into();
        assert!(report_for_bad(record).is_err());
    }
    fn report_for_bad(record: AccountRecord) -> Result<QuotaReport, QuotaError> {
        report(
            crate::source::TEST_POLICIES,
            &[record],
            &[],
            &QuotaQuery { adapter: None },
            100,
            "fixture".into(),
        )
    }
    #[test]
    fn report_omits_unregistered_account_record_and_excludes_it_from_coverage_total() {
        let live = account("claude", "live");
        let unregistered = account("claude", "old");
        let records = [
            AccountRecord::empty(live.clone()),
            AccountRecord::empty(unregistered),
        ];
        let detections = [Detection {
            adapter: "claude".into(),
            accounts: Some(vec![live.clone()]),
            status: QuotaStatus::Available,
        }];
        let rep = report(
            crate::source::TEST_POLICIES,
            &records,
            &detections,
            &QuotaQuery { adapter: None },
            100,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(rep.accounts.len(), 1);
        assert_eq!(rep.accounts[0].account, live);
        assert_eq!(rep.coverage.total, 1);
    }
    #[test]
    fn plan_tick_drops_non_retaining_record_whose_adapter_has_no_registered_account() {
        let opencode = account("opencode", "go");
        let records = [AccountRecord::empty(opencode.clone())];
        let detections = [Detection {
            adapter: "opencode".into(),
            accounts: None,
            status: QuotaStatus::Unsupported,
        }];
        let tick = QuotaTick {
            adapter: None,
            trigger: Trigger::Manual,
            power: Power::Ac,
        };
        let plan = plan_tick(
            crate::source::TEST_POLICIES,
            &records,
            &detections,
            &tick,
            100,
        )
        .unwrap();
        assert_eq!(plan.drops.len(), 1);
        assert_eq!(plan.drops[0].account, opencode);
        assert!(plan.drops[0].replacement.is_none());
    }
    #[test]
    fn legacy_keyed_agy_record_sharing_fingerprint_with_live_registered_account_is_not_rendered_as_second_row()
     {
        let live_account = AccountScope {
            adapter: "agy".into(),
            account_key: "agy-google-662390bb3131".into(),
            label: "google-662390bb3131".into(),
        };
        let legacy_account = AccountScope {
            adapter: "agy".into(),
            account_key: "agy-agy".into(),
            label: "agy".into(),
        };
        let mut live_record = AccountRecord::empty(live_account.clone());
        live_record.revision = 62;
        live_record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: live_account.clone(),
            revision: 62,
            status: QuotaStatus::Available,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: Some(QuotaDetails {
                fingerprint: Some("google-662390bb3131".into()),
                ..Default::default()
            }),
            windows: vec![],
        });
        let mut legacy_record = AccountRecord::empty(legacy_account.clone());
        legacy_record.revision = 14;
        legacy_record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: legacy_account,
            revision: 14,
            status: QuotaStatus::Stale,
            observed_unix_seconds: 50,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: Some(QuotaDetails {
                fingerprint: Some("google-662390bb3131".into()),
                ..Default::default()
            }),
            windows: vec![],
        });
        let records = [live_record.clone(), legacy_record.clone()];
        let detections = [Detection {
            adapter: "agy".into(),
            accounts: Some(vec![live_account.clone()]),
            status: QuotaStatus::Available,
        }];
        let rep = report(
            crate::source::TEST_POLICIES,
            &records,
            &detections,
            &QuotaQuery { adapter: None },
            100,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(rep.accounts.len(), 1);
        assert_eq!(rep.accounts[0].account, live_account);

        let tick = QuotaTick {
            adapter: None,
            trigger: Trigger::Manual,
            power: Power::Ac,
        };
        let plan = plan_tick(
            crate::source::TEST_POLICIES,
            &records,
            &detections,
            &tick,
            100,
        )
        .unwrap();
        assert_eq!(plan.drops.len(), 1);
        assert_eq!(plan.drops[0].account, legacy_record.account);
    }
    #[test]
    fn auth_required_adapter_with_registered_account_shows_stale_cached_reading() {
        let live = account("claude", "live");
        let mut record = AccountRecord::empty(live.clone());
        record.revision = 1;
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: live.clone(),
            revision: 1,
            status: QuotaStatus::Available,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![],
            details: None,
        });
        let detections = [Detection {
            adapter: "claude".into(),
            accounts: Some(vec![live.clone()]),
            status: QuotaStatus::AuthRequired,
        }];
        let rep = report(
            crate::source::TEST_POLICIES,
            &[record.clone()],
            &detections,
            &QuotaQuery { adapter: None },
            500,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(rep.accounts.len(), 1);
        assert_eq!(rep.accounts[0].account, live);
        assert_eq!(rep.accounts[0].status, QuotaStatus::AuthRequired);
        assert!(rep.accounts[0].snapshot.is_some());
        assert_eq!(rep.coverage.known, 0);
        assert_eq!(rep.coverage.total, 1);

        let tick = QuotaTick {
            adapter: None,
            trigger: Trigger::Manual,
            power: Power::Ac,
        };
        let plan = plan_tick(
            crate::source::TEST_POLICIES,
            &[record],
            &detections,
            &tick,
            500,
        )
        .unwrap();
        assert!(plan.drops.is_empty());
        assert!(plan.batches.is_empty());
    }
    #[test]
    fn auth_required_adapter_with_fresh_snapshot_shows_auth_required_and_not_current() {
        let live = account("claude", "live");
        let mut record = AccountRecord::empty(live.clone());
        record.revision = 1;
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: live.clone(),
            revision: 1,
            status: QuotaStatus::Available,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![],
            details: None,
        });
        let detections = [Detection {
            adapter: "claude".into(),
            accounts: Some(vec![live.clone()]),
            status: QuotaStatus::AuthRequired,
        }];
        let rep = report(
            crate::source::TEST_POLICIES,
            &[record],
            &detections,
            &QuotaQuery { adapter: None },
            110,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(rep.accounts.len(), 1);
        assert_eq!(rep.accounts[0].account, live);
        assert_eq!(rep.accounts[0].status, QuotaStatus::AuthRequired);
        assert!(rep.accounts[0].snapshot.is_some());
        assert_eq!(rep.coverage.known, 0);
        assert_eq!(rep.coverage.total, 1);
    }
}
