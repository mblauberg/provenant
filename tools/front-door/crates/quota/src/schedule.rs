//! Scheduling uses explicit integer seconds; it never sleeps or executes a probe.
use crate::*;
use provenant_core::retry::Backoff;
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
/// A probe must finish before its durable claim expires, even when the tick's
/// configured wall budget is longer than the claim window.
pub fn probe_deadline(tick_deadline: i64, started: i64) -> Result<i64, QuotaError> {
    let claim_expiry = add(started, TICK_BUDGET_SECONDS as u64)?;
    Ok(tick_deadline.min(claim_expiry - 1))
}
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
    let event_auth_retry = record.auth_retry_pending
        || (trigger == Trigger::Manual && record.last_error.is_some_and(ProbeFailure::needs_login));
    let mut due = match record.last_attempt_unix_seconds {
        Some(at) if !event_auth_retry && record.last_error != Some(ProbeFailure::Network) => {
            add(at, ttl)?
        }
        _ => i64::MIN,
    };
    if let Some(backoff) = record.backoff_until_unix_seconds
        && !event_auth_retry
    {
        due = due.max(backoff);
    }
    if let Some(claim) = &record.claim {
        due = due.max(claim.deadline_unix_seconds);
    }
    Ok(Some(due))
}
pub fn backoff(failure: ProbeFailure, failures: u64, retry_after_seconds: Option<u64>) -> u64 {
    let base = match failure {
        ProbeFailure::Network | ProbeFailure::RateLimited => {
            let (first, cap) = if failure == ProbeFailure::Network {
                (15, 300)
            } else {
                (120, 3600)
            };
            Backoff::new(
                std::time::Duration::from_secs(first),
                2,
                std::time::Duration::from_secs(cap),
            )
            .delay(failures.saturating_sub(1))
            .as_secs()
        }
        ProbeFailure::Auth | ProbeFailure::AccountMismatch => 900,
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
    for cooldown in &mut record.capacity_cooldowns {
        cooldown.account = account.clone();
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
    record.auth_retry_pending = false;
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
            if failure.needs_login() {
                next.auth_verified_unix_seconds = None;
            }
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
        // Keep past Google accounts so returning to a sign-in restores its cooldown.
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
        if selected(&record.account.adapter) && !retaining && absent_from_detection {
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
        .filter(|d| {
            selected(&d.adapter)
                // An unambiguous ambient auth failure without a recorded failure
                // needs no probe. A recorded failure still gets its paced retry.
                && !(d.status == QuotaStatus::AuthRequired
                    && d.accounts.as_ref().is_some_and(|a| a.len() == 1)
                    && !records.iter().any(|record| {
                        (record.auth_retry_pending
                            || record.last_error.is_some_and(ProbeFailure::needs_login)
                            || record.auth_verified_unix_seconds.is_some())
                                && d.accounts.as_ref().is_some_and(|accounts| {
                                    accounts.iter().any(|account| {
                                        account.adapter == record.account.adapter
                                            && account.account_key == record.account.account_key
                                    })
                                })
                        }))
        })
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
        let auth_required = record.last_error.is_some_and(ProbeFailure::needs_login)
            || (!record
                .auth_verified_unix_seconds
                .is_some_and(|at| at <= now && now.saturating_sub(at) < 900)
                && detections.iter().any(|d| {
                    d.adapter == record.account.adapter
                        && d.status == QuotaStatus::AuthRequired
                        && d.accounts.as_ref().is_some_and(|a| a.len() == 1)
                }));
        let fresh = record.snapshot.as_ref().is_some_and(|s| {
            s.observed_unix_seconds <= now
                && record.last_error.is_none()
                && !auth_required
                && age.is_some_and(|a| {
                    a < interval(policies, &record.account.adapter, Trigger::Timer, Power::Ac)
                        .unwrap_or(0)
                })
        });
        let free_refused = provenant_records::AdapterSpec::get(&record.account.adapter)
            .and_then(|spec| spec.free_account.map(|(key, _)| (key, spec.free_pool)))
            .is_some_and(|(key, pool)| {
                record.account.account_key == key
                    && record.refusals.iter().any(|refusal| {
                        refusal.pool.as_deref() == pool
                            && refusal.observed_unix_seconds <= now
                            && refusal
                                .resets_unix_seconds
                                .unwrap_or(refusal.observed_unix_seconds.saturating_add(3600))
                                > now
                    })
            });
        let status = match &record.snapshot {
            _ if free_refused => QuotaStatus::Exhausted,
            _ if auth_required => QuotaStatus::AuthRequired,
            Some(snapshot) if fresh => snapshot.status,
            Some(_) => QuotaStatus::Stale,
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
            capacity_cooldowns: record
                .capacity_cooldowns
                .iter()
                .filter(|c| c.observed_unix_seconds <= now && c.expires_unix_seconds > now)
                .map(|c| crate::CapacityCooldownRow {
                    model: c.model.clone(),
                    observed_unix_seconds: c.observed_unix_seconds,
                    expires_unix_seconds: c.expires_unix_seconds,
                    remaining_seconds: u64::try_from(c.expires_unix_seconds.saturating_sub(now))
                        .unwrap_or(u64::MAX),
                })
                .collect(),
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

    #[test]
    fn probe_deadline_respects_tick_wall_budget_and_claim_expiry() {
        assert_eq!(probe_deadline(700, 100).unwrap(), 154);
        assert_eq!(probe_deadline(130, 100).unwrap(), 130);
    }
    fn account() -> AccountScope {
        AccountScope {
            adapter: "claude".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        }
    }
    #[test]
    fn transient_failure_retries_at_backoff_without_waiting_for_normal_interval() {
        let mut record = AccountRecord::empty(account());
        record.last_attempt_unix_seconds = Some(100);
        record.last_error = Some(ProbeFailure::Network);
        record.backoff_until_unix_seconds = Some(115);
        assert_eq!(
            next_due(
                crate::source::TEST_POLICIES,
                &record,
                Trigger::Timer,
                Power::Ac
            )
            .unwrap(),
            Some(115)
        );
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
    fn manual_tick_reprobes_auth_backoff() {
        let mut record = AccountRecord::empty(account());
        record.last_attempt_unix_seconds = Some(100);
        record.last_error = Some(ProbeFailure::Auth);
        record.failures = 1;
        record.backoff_until_unix_seconds = Some(3700);
        let detection = Detection {
            adapter: "claude".into(),
            accounts: Some(vec![record.account.clone()]),
            status: QuotaStatus::AuthRequired,
        };
        let tick = QuotaTick {
            adapter: None,
            trigger: Trigger::Manual,
            power: Power::Ac,
        };
        let plan = plan_tick(
            crate::source::TEST_POLICIES,
            &[record.clone()],
            &[detection],
            &tick,
            110,
        )
        .unwrap();
        assert_eq!(plan.batches, vec![vec![record.account.clone()]]);
        assert!(
            reserve(
                crate::source::TEST_POLICIES,
                Some(&record),
                &record.account,
                110,
                "claim",
                tick.trigger,
                tick.power
            )
            .is_ok()
        );
        record.claim = Some(RefreshClaim {
            token: "active".into(),
            deadline_unix_seconds: 155,
        });
        assert!(
            reserve(
                crate::source::TEST_POLICIES,
                Some(&record),
                &record.account,
                110,
                "claim",
                tick.trigger,
                tick.power
            )
            .is_err()
        );
    }
    #[test]
    fn credential_event_retries_auth_once_and_keeps_active_claim() {
        let mut record = AccountRecord::empty(account());
        record.last_attempt_unix_seconds = Some(100);
        record.last_error = Some(ProbeFailure::Auth);
        record.failures = 1;
        record.backoff_until_unix_seconds = Some(1000);
        record.auth_retry_pending = true;
        let detection = Detection {
            adapter: "claude".into(),
            accounts: Some(vec![record.account.clone()]),
            status: QuotaStatus::AuthRequired,
        };
        for trigger in [Trigger::Panel, Trigger::Wake] {
            let tick = QuotaTick {
                adapter: None,
                trigger,
                power: Power::Ac,
            };
            let plan = plan_tick(
                crate::source::TEST_POLICIES,
                &[record.clone()],
                std::slice::from_ref(&detection),
                &tick,
                110,
            )
            .unwrap();
            assert_eq!(plan.batches, vec![vec![record.account.clone()]]);
            let reserved = reserve(
                crate::source::TEST_POLICIES,
                Some(&record),
                &record.account,
                110,
                "claim",
                trigger,
                Power::Ac,
            )
            .unwrap()
            .replacement
            .unwrap();
            assert!(!reserved.auth_retry_pending);
            let mut live = reserved.clone();
            live.auth_retry_pending = true;
            assert!(
                reserve(
                    crate::source::TEST_POLICIES,
                    Some(&live),
                    &record.account,
                    111,
                    "second",
                    trigger,
                    Power::Ac
                )
                .is_err()
            );
            let failed = complete(&reserved, "claim", 112, Err(ProbeFailure::Auth), None)
                .unwrap()
                .replacement
                .unwrap();
            assert_eq!(
                next_due(crate::source::TEST_POLICIES, &failed, trigger, Power::Ac).unwrap(),
                Some(1012)
            );
        }
    }
    #[test]
    fn single_account_auth_failure_gets_a_paced_fallback_retry() {
        let mut record = AccountRecord::empty(account());
        record.last_attempt_unix_seconds = Some(100);
        record.last_error = Some(ProbeFailure::Auth);
        record.failures = 1;
        record.backoff_until_unix_seconds = Some(1000);
        let detection = Detection {
            adapter: "claude".into(),
            accounts: Some(vec![record.account.clone()]),
            status: QuotaStatus::AuthRequired,
        };
        let tick = QuotaTick {
            adapter: None,
            trigger: Trigger::Timer,
            power: Power::Ac,
        };
        let before = plan_tick(
            crate::source::TEST_POLICIES,
            &[record.clone()],
            std::slice::from_ref(&detection),
            &tick,
            999,
        )
        .unwrap();
        assert!(before.batches.is_empty());
        let after = plan_tick(
            crate::source::TEST_POLICIES,
            &[record.clone()],
            &[detection],
            &tick,
            1000,
        )
        .unwrap();
        assert_eq!(after.batches, vec![vec![record.account]]);
    }
    #[test]
    fn lane_success_temporarily_overrides_ambient_auth_detection() {
        let mut record = AccountRecord::empty(account());
        record.auth_verified_unix_seconds = Some(100);
        let detection = Detection {
            adapter: "claude".into(),
            accounts: Some(vec![record.account.clone()]),
            status: QuotaStatus::AuthRequired,
        };
        let query = QuotaQuery { adapter: None };
        let recent = report(
            crate::source::TEST_POLICIES,
            &[record.clone()],
            std::slice::from_ref(&detection),
            &query,
            101,
            "/report".into(),
        )
        .unwrap();
        assert_eq!(recent.accounts[0].status, QuotaStatus::Unknown);
        let expired = report(
            crate::source::TEST_POLICIES,
            &[record],
            &[detection],
            &query,
            1000,
            "/report".into(),
        )
        .unwrap();
        assert_eq!(expired.accounts[0].status, QuotaStatus::AuthRequired);
    }
    #[test]
    fn backoff_has_bounded_exponential_and_retry_after_floor() {
        assert_eq!(backoff(ProbeFailure::Network, 1, None), 15);
        assert_eq!(backoff(ProbeFailure::Network, 2, None), 30);
        assert_eq!(backoff(ProbeFailure::Network, 3, None), 60);
        assert_eq!(backoff(ProbeFailure::Network, 4, None), 120);
        assert_eq!(backoff(ProbeFailure::Network, 5, None), 240);
        assert_eq!(backoff(ProbeFailure::Network, 6, None), 300);
        assert_eq!(backoff(ProbeFailure::Network, 50, None), 300);
        assert_eq!(backoff(ProbeFailure::RateLimited, 50, None), 3600);
        assert_eq!(backoff(ProbeFailure::Network, 1, Some(999999)), 86400);
        assert_eq!(backoff(ProbeFailure::Auth, 1, None), 900);
        assert_eq!(backoff(ProbeFailure::MissingCli, 1, None), 86400);
        assert_eq!(backoff(ProbeFailure::ProbeDirectory, 1, None), 120);
    }
    #[test]
    fn report_shows_only_live_capacity_cooldowns() {
        let account = account();
        let mut record = AccountRecord::empty(account.clone());
        record.capacity_cooldowns.push(CapacityCooldown {
            extra: Default::default(),
            account,
            model: "model".into(),
            observed_unix_seconds: 100,
            expires_unix_seconds: 700,
        });
        let row = |now| {
            report(
                crate::source::TEST_POLICIES,
                &[record.clone()],
                &[],
                &QuotaQuery { adapter: None },
                now,
                "report.json".into(),
            )
            .unwrap()
            .accounts
            .remove(0)
        };
        assert_eq!(row(400).capacity_cooldowns[0].remaining_seconds, 300);
        assert!(row(700).capacity_cooldowns.is_empty());
        assert!(row(99).capacity_cooldowns.is_empty());
    }

    #[test]
    fn reserve_adopts_a_renamed_label_for_the_same_account_key() {
        let a = account();
        let mut renamed = a.clone();
        renamed.label = "renamed".into();
        let r = record_capacity_cooldown(None, &a, "claude/model", 100, 600)
            .unwrap()
            .unwrap()
            .replacement
            .unwrap();
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
        assert_eq!(reserved.capacity_cooldowns.len(), 1);
        assert_eq!(reserved.capacity_cooldowns[0].account, renamed);
        assert_eq!(reserved.capacity_cooldowns[0].expires_unix_seconds, 700);
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
        assert_eq!(row.status, QuotaStatus::AuthRequired);
        assert_eq!(row.error, Some(ProbeFailure::Auth));
        assert_eq!(row.snapshot.unwrap().observed_unix_seconds, 50);
        assert!(failed.check(Some(&r)).is_err());
    }
    #[test]
    fn account_mismatch_reports_auth_required_with_its_own_error() {
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
        let failed = complete(
            &current,
            "claim",
            420,
            Err(ProbeFailure::AccountMismatch),
            None,
        )
        .unwrap()
        .replacement
        .unwrap();
        let row = report(
            crate::source::TEST_POLICIES,
            &[failed],
            &[],
            &QuotaQuery { adapter: None },
            420,
            "report.json".into(),
        )
        .unwrap()
        .accounts
        .remove(0);
        assert_eq!(row.status, QuotaStatus::AuthRequired);
        assert_eq!(row.error, Some(ProbeFailure::AccountMismatch));
        assert_eq!(serde_json::to_value(row.error).unwrap(), "account_mismatch");
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
    #[test]
    fn multi_account_ambient_auth_required_keeps_fresh_rows_available_and_known() {
        let first = account("claude", "first");
        let second = account("claude", "second");
        let mut r1 = AccountRecord::empty(first.clone());
        r1.revision = 1;
        r1.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: first.clone(),
            revision: 1,
            status: QuotaStatus::Available,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![],
            details: None,
        });
        let mut r2 = AccountRecord::empty(second.clone());
        r2.revision = 1;
        r2.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: second.clone(),
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
            accounts: Some(vec![first.clone(), second.clone()]),
            status: QuotaStatus::AuthRequired,
        }];
        let rep = report(
            crate::source::TEST_POLICIES,
            &[r1.clone(), r2.clone()],
            &detections,
            &QuotaQuery { adapter: None },
            110,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(rep.accounts.len(), 2);
        assert_eq!(rep.accounts[0].status, QuotaStatus::Available);
        assert_eq!(rep.accounts[1].status, QuotaStatus::Available);
        assert_eq!(rep.coverage.known, 2);
        assert_eq!(rep.coverage.total, 2);

        // Multi-account adapter with ambient AuthRequired is still batched for probing.
        let tick = QuotaTick {
            adapter: None,
            trigger: Trigger::Manual,
            power: Power::Ac,
        };
        let plan = plan_tick(
            crate::source::TEST_POLICIES,
            &[r1, r2],
            &detections,
            &tick,
            500,
        )
        .unwrap();
        assert_eq!(plan.batches.len(), 1);
        assert_eq!(plan.batches[0].len(), 2);
    }
    #[test]
    fn multi_account_ambient_auth_required_marks_only_account_with_auth_error() {
        let first = account("claude", "first");
        let second = account("claude", "second");
        let mut r1 = AccountRecord::empty(first.clone());
        r1.revision = 1;
        r1.last_error = Some(ProbeFailure::Auth);
        r1.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: first.clone(),
            revision: 1,
            status: QuotaStatus::Available,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![],
            details: None,
        });
        let mut r2 = AccountRecord::empty(second.clone());
        r2.revision = 1;
        r2.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: second.clone(),
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
            accounts: Some(vec![first.clone(), second.clone()]),
            status: QuotaStatus::AuthRequired,
        }];
        let rep = report(
            crate::source::TEST_POLICIES,
            &[r1, r2],
            &detections,
            &QuotaQuery { adapter: None },
            110,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(rep.accounts.len(), 2);
        assert_eq!(rep.accounts[0].account, first);
        assert_eq!(rep.accounts[0].status, QuotaStatus::AuthRequired);
        assert_eq!(rep.accounts[1].account, second);
        assert_eq!(rep.accounts[1].status, QuotaStatus::Available);
        assert_eq!(rep.coverage.known, 1);
        assert_eq!(rep.coverage.total, 2);
    }
}

#[cfg(test)]
mod zen_tests {
    use super::*;

    #[test]
    fn zen_quota_row_reflects_observed_refusals_separately_from_go() {
        let accounts: Vec<_> = ["go", "zen"]
            .into_iter()
            .map(|key| AccountScope {
                adapter: "opencode".into(),
                account_key: key.into(),
                label: key.into(),
            })
            .collect();
        let mut records: Vec<_> = accounts.iter().cloned().map(AccountRecord::empty).collect();
        records[1].refusals.push(crate::Refusal {
            extra: Default::default(),
            account: accounts[1].clone(),
            model: "opencode/beam-free".into(),
            pool: Some("opencode-free".into()),
            observed_unix_seconds: 100,
            resets_unix_seconds: None,
        });
        let detections = [Detection {
            adapter: "opencode".into(),
            accounts: Some(accounts),
            status: QuotaStatus::Available,
        }];
        for (now, expected) in [(101, QuotaStatus::Exhausted), (3700, QuotaStatus::Unknown)] {
            let report = report(
                crate::source::TEST_POLICIES,
                &records,
                &detections,
                &QuotaQuery { adapter: None },
                now,
                "report.json".into(),
            )
            .unwrap();
            assert_eq!(report.accounts.len(), 2);
            assert_eq!(report.accounts[0].status, QuotaStatus::Unknown);
            assert_eq!(report.accounts[1].status, expected);
        }
    }
}
