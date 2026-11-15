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

pub const TICK_BUDGET_SECONDS: i64 = crate::DEFAULT_WALL_BUDGET_SECONDS as i64;
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
    // A failed reading is re-probed by the next owner tick whatever the
    // failure, so a repaired account (a new login, a created keychain, an
    // installed CLI) is routable at once. Only a provider rate limit keeps its
    // retry window, because probing it again cannot clear it.
    let event_auth_retry = record.auth_retry_pending
        || (trigger == Trigger::Manual && record.last_error.is_some_and(ProbeFailure::repairable));
    let mut due = match record.last_attempt_unix_seconds {
        Some(at)
            if !event_auth_retry
                && !matches!(
                    record.last_error,
                    Some(ProbeFailure::Network | ProbeFailure::RefreshDeferred)
                ) =>
        {
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

/// An absent readiness file can shorten the normal interval without bypassing
/// provider backoff, power policy or an outstanding claim.
pub fn trigger_for_readiness(
    policies: &[QuotaPolicy],
    record: Option<&AccountRecord>,
    trigger: Trigger,
    power: Power,
    missing: bool,
) -> Result<Trigger, QuotaError> {
    let Some(record) = record.filter(|_| missing) else {
        return Ok(trigger);
    };
    let normal = next_due(policies, record, trigger, power)?;
    let readiness = next_due(policies, record, Trigger::Panel, power)?;
    Ok(match (normal, readiness) {
        (Some(normal), Some(readiness)) if readiness < normal => Trigger::Panel,
        _ => trigger,
    })
}
#[derive(Clone, Copy, Debug)]
struct BackoffRule {
    failure: ProbeFailure,
    first_seconds: u64,
    cap_seconds: u64,
}

const RETRY_AFTER_CAP_SECONDS: u64 = 3600;
const BACKOFF_RULES: &[BackoffRule] = &[
    BackoffRule {
        failure: ProbeFailure::Network,
        first_seconds: 15,
        cap_seconds: 300,
    },
    BackoffRule {
        failure: ProbeFailure::RateLimited,
        first_seconds: 120,
        cap_seconds: 3600,
    },
    BackoffRule {
        failure: ProbeFailure::RefreshDeferred,
        first_seconds: 15,
        cap_seconds: 15,
    },
    BackoffRule {
        failure: ProbeFailure::Auth,
        first_seconds: 900,
        cap_seconds: 900,
    },
    BackoffRule {
        failure: ProbeFailure::LoginExpired,
        first_seconds: 900,
        cap_seconds: 900,
    },
    BackoffRule {
        failure: ProbeFailure::AccountMismatch,
        first_seconds: 900,
        cap_seconds: 900,
    },
    BackoffRule {
        failure: ProbeFailure::IsolatedKeychainUnavailable,
        first_seconds: 3600,
        cap_seconds: 3600,
    },
    BackoffRule {
        failure: ProbeFailure::MissingCli,
        first_seconds: 86400,
        cap_seconds: 86400,
    },
    BackoffRule {
        failure: ProbeFailure::ProbeDirectory,
        first_seconds: 120,
        cap_seconds: 120,
    },
    BackoffRule {
        failure: ProbeFailure::Disabled,
        first_seconds: 86400,
        cap_seconds: 86400,
    },
    BackoffRule {
        failure: ProbeFailure::Malformed,
        first_seconds: 120,
        cap_seconds: 120,
    },
];

pub fn backoff(failure: ProbeFailure, failures: u64, retry_after_seconds: Option<u64>) -> u64 {
    let rule = BACKOFF_RULES
        .iter()
        .find(|rule| rule.failure == failure)
        .expect("every probe failure declares a backoff rule");
    let base = if rule.first_seconds == rule.cap_seconds {
        rule.first_seconds
    } else {
        Backoff::new(
            std::time::Duration::from_secs(rule.first_seconds),
            2,
            std::time::Duration::from_secs(rule.cap_seconds),
        )
        .delay(failures.saturating_sub(1))
        .as_secs()
    };
    base.max(
        retry_after_seconds
            .unwrap_or(0)
            .min(RETRY_AFTER_CAP_SECONDS),
    )
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
    let stored_account = record.account.updated_from(account);
    record.account = stored_account.clone();
    if let Some(snapshot) = record.snapshot.as_mut() {
        snapshot.account = snapshot.account.updated_from(&stored_account);
    }
    for refusal in &mut record.refusals {
        refusal.account = refusal.account.updated_from(&stored_account);
    }
    for cooldown in &mut record.capacity_cooldowns {
        cooldown.account = cooldown.account.updated_from(&stored_account);
    }
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or(QuotaError::NumberOutOfRange)?;
    record.claim = Some(RefreshClaim {
        extra: Default::default(),
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
        readiness_probe_result: None,
        login_probe_expiry: None,
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
            if !snapshot.account.same_scope(&current.account)
                || snapshot.observed_unix_seconds > now
                || current
                    .last_attempt_unix_seconds
                    .is_some_and(|at| snapshot.observed_unix_seconds < at)
            {
                return Err(QuotaError::Invalid("probe observation"));
            }
            if let Some(previous) = &current.snapshot {
                snapshot.extra.extend(previous.extra.clone());
                snapshot.account = snapshot.account.updated_from(&previous.account);
                if let (Some(previous_details), Some(details)) =
                    (&previous.details, &mut snapshot.details)
                {
                    details.extra.extend(previous_details.extra.clone());
                }
                for window in &mut snapshot.windows {
                    if let Some(previous) =
                        previous.windows.iter().find(|row| row.pool == window.pool)
                    {
                        window.extra.extend(previous.extra.clone());
                    }
                }
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
            // A later failed probe cannot establish that the registered
            // identity matches again.
            if current.last_error != Some(ProbeFailure::AccountMismatch) {
                next.last_error = Some(failure);
            }
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
        readiness_probe_result: None,
        login_probe_expiry: None,
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
        &[],
    )
}
pub fn plan_tick_with_settings(
    policies: &[QuotaPolicy],
    records: &[AccountRecord],
    detections: &[Detection],
    tick: &QuotaTick,
    now: i64,
    settings: &Settings,
    missing_readiness: &[(String, String)],
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
                readiness_probe_result: None,
                login_probe_expiry: None,
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
                            || record.last_error.is_some()
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
        let missing = missing_readiness
            .iter()
            .any(|(adapter, key)| adapter == &account.adapter && key == &account.account_key);
        let trigger = trigger_for_readiness(policies, current, tick.trigger, tick.power, missing)?;
        let due = match current {
            Some(record) => next_due(policies, record, trigger, tick.power)?,
            None => interval(policies, &account.adapter, trigger, tick.power).map(|_| i64::MIN),
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
        let auth_refresh_pending = record.auth_state.as_ref().is_some_and(|auth| {
            auth.signed_in == Some(true)
                && auth.refresh_pending
                && auth.observed_unix_seconds.is_some_and(|at| {
                    at <= now
                        && (now.saturating_sub(at) as u64)
                            < interval(policies, &record.account.adapter, Trigger::Timer, Power::Ac)
                                .unwrap_or(0)
                })
        });
        let live: Vec<_> = record
            .refusals
            .iter()
            .filter(|refusal| {
                refusal.observed_unix_seconds <= now
                    && refusal
                        .expires_unix_seconds()
                        .is_ok_and(|expires| expires > now)
            })
            .collect();
        let refusals: Vec<_> = live
            .iter()
            .map(|refusal| crate::RefusalRow {
                model: refusal.model.clone(),
                pool: refusal.pool.clone(),
                resets_at: refusal
                    .resets_unix_seconds
                    .filter(|reset| *reset > now)
                    .and_then(|reset| jiff::Timestamp::from_second(reset).ok())
                    .map(|timestamp| timestamp.to_string()),
            })
            .collect();
        let free_pool = provenant_records::AdapterSpec::get(&record.account.adapter)
            .filter(|spec| {
                spec.is_free_plan(
                    record
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.details.as_ref())
                        .and_then(|details| details.plan.as_deref()),
                )
            })
            .and_then(|spec| spec.free_pool);
        let in_free = |refusal: &&crate::RefusalRow| {
            free_pool.is_some_and(|pool| refusal.pool.as_deref() == Some(pool))
        };
        let free_refused = refusals.iter().any(|refusal| in_free(&refusal));
        // A free-pool refusal or a spent plan usage limit makes the whole
        // account read exhausted until the latest named reset.
        // A newer reading that clears the hold clears it here too: the one
        // rule is `Admission::refusal_holds`.
        let admission = crate::Admission {
            policies,
            now_unix_seconds: now,
            records,
            pools: &[],
        };
        let mut account_refusals = Vec::new();
        for (refusal, row) in live.iter().zip(&refusals) {
            if (in_free(&row) || row.pool.as_deref() == Some(ACCOUNT_ROUTE))
                && admission.refusal_holds(&record.account.adapter, Some(record), refusal)?
            {
                account_refusals.push(row);
            }
        }
        let account_refused = !account_refusals.is_empty();
        let status = match &record.snapshot {
            _ if record.last_error == Some(ProbeFailure::IsolatedKeychainUnavailable) => {
                QuotaStatus::KeychainUnavailable
            }
            _ if account_refused => QuotaStatus::Exhausted,
            _ if auth_required => QuotaStatus::AuthRequired,
            // Preserve operational failures separately from the observed need
            // for renewal. Old auth facts expire and never override sign-out.
            _ if auth_refresh_pending
                || (record.last_error == Some(ProbeFailure::RefreshDeferred)
                    && record.last_attempt_unix_seconds.is_some_and(|at| {
                        at <= now
                            && (now.saturating_sub(at) as u64)
                                < interval(
                                    policies,
                                    &record.account.adapter,
                                    Trigger::Timer,
                                    Power::Ac,
                                )
                                .unwrap_or(0)
                    })) =>
            {
                QuotaStatus::RefreshPending
            }
            Some(snapshot) if fresh => snapshot.status,
            Some(_) => QuotaStatus::Stale,
            None if record.last_error == Some(ProbeFailure::Disabled) => QuotaStatus::Unsupported,
            None => QuotaStatus::Unknown,
        };
        let reset_seconds = if account_refused {
            account_refusals
                .iter()
                .map(|refusal| refusal.resets_at.as_deref())
                .collect::<Option<Vec<_>>>()
                .and_then(|resets| {
                    resets
                        .into_iter()
                        .filter_map(|reset| reset.parse::<jiff::Timestamp>().ok())
                        .max()
                })
                .map(|reset| reset.to_string())
        } else {
            None
        };
        accounts.push(AccountRow {
            account: record.account.clone(),
            readiness: None,
            login_expiry: None,
            auth_state: record.auth_state.clone(),
            snapshot: record.snapshot.clone(),
            status,
            resets_at: reset_seconds,
            refusals,
            free_pool_refusal: free_refused,
            age_seconds: age,
            next_due_unix_seconds: next_due(policies, record, Trigger::Timer, Power::Ac)?
                .map(|d| d.max(now)),
            refreshing: record
                .claim
                .as_ref()
                .is_some_and(|c| c.deadline_unix_seconds > now),
            error: record.last_error,
            repair: None,
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
    fn missing_readiness_shortens_interval_without_bypassing_backoff_or_claim() {
        let mut record = AccountRecord::empty(account());
        record.last_attempt_unix_seconds = Some(100);
        let trigger = trigger_for_readiness(
            crate::source::TEST_POLICIES,
            Some(&record),
            Trigger::Timer,
            Power::Ac,
            true,
        )
        .unwrap();
        assert_eq!(
            next_due(crate::source::TEST_POLICIES, &record, trigger, Power::Ac).unwrap(),
            Some(220)
        );
        record.backoff_until_unix_seconds = Some(250);
        record.claim = Some(RefreshClaim {
            extra: Default::default(),
            token: "fixture".into(),
            deadline_unix_seconds: 280,
        });
        let trigger = trigger_for_readiness(
            crate::source::TEST_POLICIES,
            Some(&record),
            Trigger::Timer,
            Power::Ac,
            true,
        )
        .unwrap();
        assert_eq!(
            next_due(crate::source::TEST_POLICIES, &record, trigger, Power::Ac).unwrap(),
            Some(280)
        );
        assert_eq!(
            trigger_for_readiness(
                crate::source::TEST_POLICIES,
                Some(&record),
                Trigger::Timer,
                Power::Ac,
                false,
            )
            .unwrap(),
            Trigger::Timer
        );
    }

    #[test]
    fn probe_deadline_respects_tick_wall_budget_and_claim_expiry() {
        assert_eq!(probe_deadline(700, 100).unwrap(), 154);
        assert_eq!(probe_deadline(130, 100).unwrap(), 130);
    }
    fn account() -> AccountScope {
        AccountScope {
            extra: Default::default(),
            adapter: "claude".into(),
            account_key: "opaque".into(),
            label: "work".into(),
        }
    }
    #[test]
    fn transient_failure_retries_at_backoff_without_waiting_for_normal_interval() {
        for failure in [ProbeFailure::Network, ProbeFailure::RefreshDeferred] {
            let mut record = AccountRecord::empty(account());
            record.last_attempt_unix_seconds = Some(100);
            record.last_error = Some(failure);
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
    }
    #[test]
    fn missing_isolated_keychain_has_its_own_quota_status() {
        let mut account = account();
        account.adapter = "agy".into();
        let mut record = AccountRecord::empty(account);
        record.last_error = Some(ProbeFailure::IsolatedKeychainUnavailable);
        let report = report(
            crate::source::TEST_POLICIES,
            &[record],
            &[],
            &QuotaQuery { adapter: None },
            100,
            "fixture".into(),
        )
        .unwrap();
        assert_eq!(report.accounts[0].status, QuotaStatus::KeychainUnavailable);
    }
    #[test]
    fn manual_respects_floor_backoff_and_expired_claims() {
        let mut r = AccountRecord::empty(account());
        r.last_attempt_unix_seconds = Some(100);
        r.backoff_until_unix_seconds = Some(400);
        r.claim = Some(RefreshClaim {
            extra: Default::default(),
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
            extra: Default::default(),
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
    fn next_owner_tick_reprobes_a_repaired_isolated_keychain() {
        let mut agy = account();
        agy.adapter = "agy".into();
        let mut record = AccountRecord::empty(agy.clone());
        let reserved = reserve(
            crate::source::TEST_POLICIES,
            Some(&record),
            &agy,
            100,
            "first",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap()
        .replacement
        .unwrap();
        record = complete(
            &reserved,
            "first",
            110,
            Err(ProbeFailure::IsolatedKeychainUnavailable),
            None,
        )
        .unwrap()
        .replacement
        .unwrap();
        assert_eq!(record.backoff_until_unix_seconds, Some(3710));
        let detection = Detection {
            adapter: "agy".into(),
            accounts: Some(vec![agy.clone()]),
            status: QuotaStatus::Available,
        };
        let plan = |record: &AccountRecord, trigger, now| {
            plan_tick(
                crate::source::TEST_POLICIES,
                std::slice::from_ref(record),
                std::slice::from_ref(&detection),
                &QuotaTick {
                    adapter: None,
                    trigger,
                    power: Power::Ac,
                },
                now,
            )
            .unwrap()
            .batches
        };
        // The keychain was created a minute later: the owner's next tick
        // probes it instead of waiting out the hour's backoff.
        assert_eq!(plan(&record, Trigger::Manual, 170), vec![vec![agy.clone()]]);
        // A timer tick keeps the backoff unless a credential change was seen.
        assert!(plan(&record, Trigger::Timer, 170).is_empty());
        let mut changed = record.clone();
        changed.auth_retry_pending = true;
        assert_eq!(plan(&changed, Trigger::Timer, 170), vec![vec![agy.clone()]]);
        let reserved = reserve(
            crate::source::TEST_POLICIES,
            Some(&record),
            &agy,
            170,
            "second",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap()
        .replacement
        .unwrap();
        let repaired = complete(
            &reserved,
            "second",
            180,
            Ok(QuotaSnapshot {
                extra: Default::default(),
                schema_version: 1,
                account: agy.clone(),
                revision: 0,
                status: QuotaStatus::Available,
                observed_unix_seconds: 175,
                source_ref: "fixture".into(),
                authority_ref: None,
                details: None,
                windows: vec![],
            }),
            None,
        )
        .unwrap()
        .replacement
        .unwrap();
        let row = report(
            crate::source::TEST_POLICIES,
            &[repaired],
            std::slice::from_ref(&detection),
            &QuotaQuery { adapter: None },
            180,
            "report.json".into(),
        )
        .unwrap()
        .accounts
        .remove(0);
        assert_eq!(row.status, QuotaStatus::Available);
        assert_eq!(row.error, None);
    }
    #[test]
    fn manual_tick_keeps_a_provider_rate_limit_window() {
        let mut record = AccountRecord::empty(account());
        record.last_attempt_unix_seconds = Some(100);
        record.last_error = Some(ProbeFailure::RateLimited);
        record.failures = 1;
        record.backoff_until_unix_seconds = Some(220);
        assert_eq!(
            next_due(
                crate::source::TEST_POLICIES,
                &record,
                Trigger::Manual,
                Power::Ac
            )
            .unwrap(),
            Some(220)
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
        for (failure, due) in [
            (ProbeFailure::Auth, 1000),
            (ProbeFailure::RefreshDeferred, 115),
        ] {
            let mut record = AccountRecord::empty(account());
            record.last_attempt_unix_seconds = Some(100);
            record.last_error = Some(failure);
            record.failures = 1;
            record.backoff_until_unix_seconds = Some(due);
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
                due - 1,
            )
            .unwrap();
            assert!(before.batches.is_empty());
            let after = plan_tick(
                crate::source::TEST_POLICIES,
                &[record.clone()],
                std::slice::from_ref(&detection),
                &tick,
                due,
            )
            .unwrap();
            assert_eq!(after.batches, vec![vec![record.account.clone()]]);
            let current = report(
                crate::source::TEST_POLICIES,
                &[record.clone()],
                std::slice::from_ref(&detection),
                &QuotaQuery { adapter: None },
                due,
                "report.json".into(),
            )
            .unwrap();
            assert_eq!(current.accounts[0].status, QuotaStatus::AuthRequired);
        }
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
        assert_eq!(BACKOFF_RULES.len(), ProbeFailure::ALL.len());
        for failure in ProbeFailure::ALL {
            assert_eq!(
                BACKOFF_RULES
                    .iter()
                    .filter(|rule| rule.failure == *failure)
                    .count(),
                1,
                "missing or duplicate rule for {failure:?}"
            );
        }
        for rule in BACKOFF_RULES {
            let encoded = provenant_records::encode(&rule.failure).unwrap();
            let decoded: ProbeFailure =
                provenant_records::decode(encoded.as_slice(), provenant_records::Limits::CONFIG).unwrap();
            assert_eq!(decoded, rule.failure);
            for attempt in [1u64, 2, 5, 50] {
                let exponent = attempt.saturating_sub(1) as u32;
                let expected_base = rule
                    .first_seconds
                    .saturating_mul(2u64.saturating_pow(exponent))
                    .min(rule.cap_seconds);
                for retry_after in [None, Some(0), Some(RETRY_AFTER_CAP_SECONDS), Some(u64::MAX)] {
                    let expected =
                        expected_base.max(retry_after.unwrap_or(0).min(RETRY_AFTER_CAP_SECONDS));
                    assert_eq!(
                        backoff(rule.failure, attempt, retry_after),
                        expected,
                        "failure={:?} attempt={attempt} retry_after={retry_after:?}",
                        rule.failure
                    );
                }
            }
        }
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
        let mut r = record_capacity_cooldown(None, &a, "claude/model", 100, 600)
            .unwrap()
            .unwrap()
            .replacement
            .unwrap();
        let future = serde_json::json!({"value": true});
        r.extra.insert("future_record".into(), future.clone());
        r.account
            .extra
            .insert("future_scope".into(), future.clone());
        r.capacity_cooldowns[0]
            .extra
            .insert("future_cooldown".into(), future.clone());
        r.capacity_cooldowns[0]
            .account
            .extra
            .insert("future_cooldown_scope".into(), future.clone());
        let mut snapshot = QuotaSnapshot::test_snapshot(
            a.clone(),
            QuotaStatus::Available,
            100,
            vec![crate::QuotaWindow {
                extra: Default::default(),
                pool: "claude-5h".into(),
                window_seconds: Some(18_000),
                used_basis_points: 100,
                resets_unix_seconds: Some(900),
            }],
        );
        snapshot
            .extra
            .insert("future_snapshot".into(), future.clone());
        snapshot
            .account
            .extra
            .insert("future_snapshot_scope".into(), future.clone());
        snapshot.windows[0]
            .extra
            .insert("future_window".into(), future.clone());
        let mut details = crate::QuotaDetails::default();
        details.extra.insert("future_details".into(), future);
        snapshot.details = Some(details);
        r.snapshot = Some(snapshot);
        let before_record = r.extra.clone();
        let before_account = r.account.extra.clone();
        let before_cooldown = r.capacity_cooldowns[0].clone();
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
        let before_snapshot = reserved.snapshot.clone().unwrap();
        assert_eq!(reserved.account.adapter, renamed.adapter);
        assert_eq!(reserved.account.account_key, renamed.account_key);
        assert_eq!(reserved.account.label, renamed.label);
        assert_eq!(reserved.account.extra, before_account);
        assert_eq!(reserved.capacity_cooldowns.len(), 1);
        assert_eq!(reserved.capacity_cooldowns[0].account.label, renamed.label);
        assert_eq!(reserved.capacity_cooldowns[0].extra, before_cooldown.extra);
        assert_eq!(
            reserved.capacity_cooldowns[0].account.extra,
            before_cooldown.account.extra
        );
        assert_eq!(reserved.capacity_cooldowns[0].expires_unix_seconds, 700);
        let replacement = QuotaSnapshot::test_snapshot(
            renamed,
            QuotaStatus::Available,
            410,
            vec![crate::QuotaWindow {
                extra: Default::default(),
                pool: "claude-5h".into(),
                window_seconds: Some(18_000),
                used_basis_points: 200,
                resets_unix_seconds: Some(900),
            }],
        );
        let done = complete(&reserved, "claim", 420, Ok(replacement), None).unwrap();
        let completed = done.replacement.unwrap();
        assert_eq!(completed.extra, before_record);
        assert_eq!(completed.account.extra, before_account);
        let completed_snapshot = completed.snapshot.unwrap();
        assert_eq!(completed_snapshot.extra, before_snapshot.extra);
        assert_eq!(
            completed_snapshot.account.extra,
            before_snapshot.account.extra
        );
        assert_eq!(
            completed_snapshot.windows[0].extra,
            before_snapshot.windows[0].extra
        );
        assert_eq!(
            completed_snapshot.details.unwrap().extra,
            before_snapshot.details.unwrap().extra
        );
    }
    #[test]
    fn completion_rejects_late_claim_and_keeps_good_snapshot_on_failure() {
        let a = account();
        let mut r = AccountRecord::empty(a.clone());
        r.snapshot = Some(QuotaSnapshot {
            extra: Default::default(),
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
    fn refresh_pending_auth_fact_preserves_errors_and_expires() {
        use crate::AuthState;
        for (failure, observed, expiry, signed_in, pending, expected) in [
            (
                ProbeFailure::Network,
                420,
                400,
                true,
                true,
                QuotaStatus::RefreshPending,
            ),
            (
                ProbeFailure::ProbeDirectory,
                420,
                4000,
                true,
                true,
                QuotaStatus::RefreshPending,
            ),
            (
                ProbeFailure::Auth,
                420,
                400,
                false,
                false,
                QuotaStatus::AuthRequired,
            ),
            (
                ProbeFailure::AccountMismatch,
                420,
                400,
                true,
                true,
                QuotaStatus::AuthRequired,
            ),
            (
                ProbeFailure::Network,
                1,
                400,
                true,
                true,
                QuotaStatus::Unknown,
            ),
        ] {
            let mut record = AccountRecord::empty(account());
            record.last_attempt_unix_seconds = Some(420);
            record.last_error = Some(failure);
            record.failures = 1;
            record.auth_state = Some(AuthState {
                extra: Default::default(),
                access_token_expiry_unix_seconds: Some(expiry),
                signed_in: Some(signed_in),
                refresh_pending: pending,
                observed_unix_seconds: Some(observed),
            });
            let report = report(
                crate::source::TEST_POLICIES,
                &[record],
                &[],
                &QuotaQuery { adapter: None },
                420,
                "report.json".into(),
            )
            .unwrap();
            assert_eq!(
                report.accounts[0].status, expected,
                "{failure:?}/{observed}"
            );
            assert_eq!(report.accounts[0].error, Some(failure));
        }
    }

    #[test]
    fn login_failure_or_refresh_deferral_preserves_snapshot_and_refusals() {
        for (failure, status) in [
            (ProbeFailure::LoginExpired, QuotaStatus::AuthRequired),
            (ProbeFailure::RefreshDeferred, QuotaStatus::RefreshPending),
        ] {
            let a = account();
            let mut record = AccountRecord::empty(a.clone());
            record.snapshot = Some(QuotaSnapshot {
                extra: Default::default(),
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
            let reserved = reserve(
                crate::source::TEST_POLICIES,
                Some(&record),
                &a,
                400,
                "claim",
                Trigger::Manual,
                Power::Ac,
            )
            .unwrap()
            .replacement
            .unwrap();
            let failed = complete(&reserved, "claim", 420, Err(failure), None)
                .unwrap()
                .replacement
                .unwrap();
            assert_eq!(failed.snapshot, record.snapshot);
            assert!(failed.refusals.is_empty());
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
            assert_eq!(row.status, status);
            assert_eq!(row.error, Some(failure));
            if failure == ProbeFailure::RefreshDeferred {
                assert!(!failure.needs_login());
                assert_eq!(backoff(failure, 50, None), 15);
            }
            assert_eq!(row.snapshot.unwrap().observed_unix_seconds, 50);
        }
    }

    #[test]
    fn expired_login_preserves_scoped_claude_refusal_until_its_reset() {
        let scope = account();
        let mut record = AccountRecord::empty(scope.clone());
        record.last_error = Some(ProbeFailure::LoginExpired);
        record.snapshot = Some(QuotaSnapshot {
            extra: Default::default(),
            schema_version: 1,
            account: scope.clone(),
            revision: 0,
            status: QuotaStatus::Exhausted,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![crate::QuotaWindow {
                extra: Default::default(),
                pool: "weekly_scoped:fixture".into(),
                window_seconds: Some(604800),
                used_basis_points: 10_000,
                resets_unix_seconds: Some(200),
            }],
            details: None,
        });
        record.refusals.push(crate::Refusal {
            extra: Default::default(),
            account: scope,
            model: "claude/fixture".into(),
            pool: Some("weekly_scoped:fixture".into()),
            observed_unix_seconds: 100,
            resets_unix_seconds: Some(200),
        });
        for (now, expected_refusals) in [(150, 1), (200, 0)] {
            let row = report(
                crate::source::TEST_POLICIES,
                &[record.clone()],
                &[],
                &QuotaQuery { adapter: None },
                now,
                "fixture".into(),
            )
            .unwrap()
            .accounts
            .remove(0);
            assert_eq!(row.status, QuotaStatus::AuthRequired);
            assert_eq!(row.error, Some(ProbeFailure::LoginExpired));
            assert_eq!(row.refusals.len(), expected_refusals);
            assert_eq!(row.snapshot.unwrap().observed_unix_seconds, 100);
            assert!(!row.free_pool_refusal);
        }
    }

    #[test]
    fn account_mismatch_reports_auth_required_with_its_own_error() {
        let a = account();
        let mut r = AccountRecord::empty(a.clone());
        r.snapshot = Some(QuotaSnapshot {
            extra: Default::default(),
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
            std::slice::from_ref(&failed),
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
        let claimed = reserve(
            crate::source::TEST_POLICIES,
            Some(&failed),
            &a,
            2000,
            "next",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap()
        .replacement
        .unwrap();
        let failed_again = complete(&claimed, "next", 2010, Err(ProbeFailure::Network), None)
            .unwrap()
            .replacement
            .unwrap();
        assert_eq!(failed_again.last_error, Some(ProbeFailure::AccountMismatch));
        let claimed = reserve(
            crate::source::TEST_POLICIES,
            Some(&failed_again),
            &a,
            4000,
            "verified",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap()
        .replacement
        .unwrap();
        let mut snapshot = claimed.snapshot.clone().unwrap();
        snapshot.observed_unix_seconds = 4000;
        let verified = complete(&claimed, "verified", 4010, Ok(snapshot), None)
            .unwrap()
            .replacement
            .unwrap();
        assert_eq!(verified.last_error, None);
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;
    fn account(adapter: &str, key: &str) -> AccountScope {
        AccountScope {
            extra: Default::default(),
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
        assert!(plan.drops.iter().any(|drop| {
            drop.account.adapter == "claude"
                && drop.account.account_key == "old"
                && drop.replacement.is_none()
        }));
        assert!(
            !plan
                .drops
                .iter()
                .any(|drop| { drop.account.adapter == "agy" && drop.account.account_key == "old" })
        );
        let settings = Settings::default();
        let expected_accounts = detected
            .iter()
            .flat_map(|detection| detection.accounts.iter().flatten())
            .map(|account| (account.adapter.clone(), account.account_key.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        let scheduled_accounts = plan
            .batches
            .iter()
            .flatten()
            .map(|account| (account.adapter.clone(), account.account_key.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(scheduled_accounts, expected_accounts);
        for batch in &plan.batches {
            assert!(batch.len() <= settings.max_parallel as usize);
            if batch.iter().any(|account| {
                crate::source::TEST_POLICIES
                    .iter()
                    .any(|policy| policy.adapter == account.adapter && policy.solo)
            }) {
                assert_eq!(batch.len(), 1);
            }
        }
        assert_eq!(
            plan.deadline_unix_seconds,
            100 + settings.wall_budget_seconds as i64
        );
        let low = QuotaTick {
            power: Power::Low,
            ..tick
        };
        let low_plan =
            plan_tick(crate::source::TEST_POLICIES, &records, &detected, &low, 100).unwrap();
        assert!(low_plan.batches.iter().flatten().all(|account| {
            crate::source::TEST_POLICIES
                .iter()
                .find(|policy| policy.adapter == account.adapter)
                .is_some_and(|policy| policy.low_power)
        }));
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
                    extra: Default::default(),
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
            extra: Default::default(),
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
    fn ambient_auth_status_uses_each_accounts_freshness_and_probe_failure() {
        let now = 1_000;
        let claude = crate::source::TEST_POLICIES
            .iter()
            .find(|policy| policy.adapter == "claude")
            .unwrap();
        let stale_age = claude.timer_seconds as i64;
        let cases = [
            (
                vec!["single"],
                stale_age,
                vec![],
                vec![("single", QuotaStatus::AuthRequired)],
                vec![],
            ),
            (
                vec!["single"],
                0,
                vec![],
                vec![("single", QuotaStatus::AuthRequired)],
                vec![],
            ),
            (
                vec!["first", "second"],
                0,
                vec![],
                vec![
                    ("first", QuotaStatus::Available),
                    ("second", QuotaStatus::Available),
                ],
                vec!["first", "second"],
            ),
            (
                vec!["first", "second"],
                0,
                vec!["first"],
                vec![
                    ("first", QuotaStatus::AuthRequired),
                    ("second", QuotaStatus::Available),
                ],
                vec!["first", "second"],
            ),
        ];

        for (keys, age, auth_errors, expected_rows, expected_probes) in cases {
            let records = keys
                .iter()
                .map(|key| {
                    let account = account("claude", key);
                    let mut record = AccountRecord::empty(account.clone());
                    record.snapshot = Some(QuotaSnapshot::test_snapshot(
                        account,
                        QuotaStatus::Available,
                        now - age,
                        Vec::new(),
                    ));
                    if auth_errors.contains(key) {
                        record.last_error = Some(ProbeFailure::Auth);
                    }
                    record
                })
                .collect::<Vec<_>>();
            let detections = [Detection {
                adapter: "claude".into(),
                accounts: Some(
                    records
                        .iter()
                        .map(|record| record.account.clone())
                        .collect(),
                ),
                status: QuotaStatus::AuthRequired,
            }];
            let report = report(
                crate::source::TEST_POLICIES,
                &records,
                &detections,
                &QuotaQuery { adapter: None },
                now,
                "report.json".into(),
            )
            .unwrap();
            let observed = report
                .accounts
                .iter()
                .map(|row| (row.account.account_key.as_str(), row.status))
                .collect::<std::collections::BTreeMap<_, _>>();
            let expected = expected_rows
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(observed, expected);
            assert_eq!(
                report.coverage.known,
                expected
                    .values()
                    .filter(|status| {
                        matches!(
                            status,
                            QuotaStatus::Available
                                | QuotaStatus::CreditOnly
                                | QuotaStatus::Exhausted
                        )
                    })
                    .count() as u64
            );
            assert_eq!(report.coverage.total, expected.len() as u64);

            let plan = plan_tick(
                crate::source::TEST_POLICIES,
                &records,
                &detections,
                &QuotaTick {
                    adapter: None,
                    trigger: Trigger::Manual,
                    power: Power::Ac,
                },
                now,
            )
            .unwrap();
            let observed_probes = plan
                .batches
                .iter()
                .flatten()
                .map(|account| account.account_key.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                observed_probes,
                expected_probes
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
            );
        }
    }
}

#[cfg(test)]
mod zen_tests {
    use super::*;

    fn registered_record(account: AccountScope, plan: &str, observed: i64) -> AccountRecord {
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(crate::QuotaSnapshot {
            extra: Default::default(),
            schema_version: 1,
            account,
            revision: 0,
            status: QuotaStatus::Unknown,
            observed_unix_seconds: observed,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: Some(crate::QuotaDetails {
                plan: Some(plan.into()),
                ..Default::default()
            }),
            windows: vec![],
        });
        record
    }

    #[test]
    fn free_refusal_reporting_follows_registered_plan_not_account_key() {
        let spec = provenant_records::AdapterSpec::get("opencode").unwrap();
        let prototype = crate::Refusal {
            extra: Default::default(),
            account: AccountScope {
                extra: Default::default(),
                adapter: spec.name.into(),
                account_key: "identity".into(),
                label: "fixture".into(),
            },
            model: "opencode/beam-free".into(),
            pool: spec.free_pool.map(str::to_owned),
            observed_unix_seconds: 100,
            resets_unix_seconds: None,
        };
        let expiry = prototype.expires_unix_seconds().unwrap();
        for now in [expiry - 1, expiry] {
            for error in [None, Some(ProbeFailure::Auth)] {
                let records: Vec<_> = spec
                    .account_model_plans
                    .iter()
                    .enumerate()
                    .map(|(index, plan)| {
                        let account = AccountScope {
                            extra: Default::default(),
                            adapter: spec.name.into(),
                            account_key: format!("identity-{index}"),
                            label: plan.plan.into(),
                        };
                        let mut record = registered_record(account.clone(), plan.plan, now);
                        record.last_error = error;
                        record.refusals.push(crate::Refusal {
                            account,
                            ..prototype.clone()
                        });
                        record
                    })
                    .collect();
                let detections = [Detection {
                    adapter: spec.name.into(),
                    accounts: Some(records.iter().map(|r| r.account.clone()).collect()),
                    status: QuotaStatus::Available,
                }];
                let report = report(
                    crate::source::TEST_POLICIES,
                    &records,
                    &detections,
                    &QuotaQuery { adapter: None },
                    now,
                    "report.json".into(),
                )
                .unwrap();
                for record in &records {
                    let row = report
                        .accounts
                        .iter()
                        .find(|row| row.account == record.account)
                        .unwrap();
                    let plan = record
                        .snapshot
                        .as_ref()
                        .unwrap()
                        .details
                        .as_ref()
                        .unwrap()
                        .plan
                        .as_deref();
                    let refused = spec.is_free_plan(plan) && expiry > now;
                    assert_eq!(row.free_pool_refusal, refused);
                    assert_eq!(
                        row.status,
                        if refused {
                            QuotaStatus::Exhausted
                        } else if error.is_some() {
                            QuotaStatus::AuthRequired
                        } else {
                            QuotaStatus::Unknown
                        }
                    );
                    assert_eq!(row.resets_at, None);
                }
            }
        }
    }

    #[test]
    fn account_usage_limit_reads_exhausted_until_its_reset() {
        let account = AccountScope {
            extra: Default::default(),
            adapter: "claude".into(),
            account_key: "work".into(),
            label: "work".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.refusals.push(crate::Refusal {
            extra: Default::default(),
            account: account.clone(),
            model: "some-model".into(),
            pool: Some(ACCOUNT_ROUTE.into()),
            observed_unix_seconds: 100,
            resets_unix_seconds: Some(5_000),
        });
        let detections = [Detection {
            adapter: "claude".into(),
            accounts: Some(vec![account]),
            status: QuotaStatus::Available,
        }];
        for (now, expected, resets_at) in [
            (101, QuotaStatus::Exhausted, Some("1970-01-01T01:23:20Z")),
            (5_000, QuotaStatus::Unknown, None),
        ] {
            let report = report(
                crate::source::TEST_POLICIES,
                std::slice::from_ref(&record),
                &detections,
                &QuotaQuery { adapter: None },
                now,
                "report.json".into(),
            )
            .unwrap();
            let row = &report.accounts[0];
            assert_eq!(row.status, expected, "now={now}");
            assert_eq!(row.resets_at.as_deref(), resets_at);
            assert!(!row.free_pool_refusal);
        }
    }

    #[test]
    fn zen_quota_row_carries_the_recorded_refusal_expiry_as_rfc3339() {
        let account = AccountScope {
            extra: Default::default(),
            adapter: "opencode".into(),
            account_key: "zen".into(),
            label: "zen".into(),
        };
        let mut record = registered_record(account.clone(), "zen", 100);
        record.refusals.push(crate::Refusal {
            extra: Default::default(),
            account: account.clone(),
            model: "opencode/beam-free".into(),
            pool: Some("opencode-free".into()),
            observed_unix_seconds: 100,
            resets_unix_seconds: Some(200),
        });
        let report_with_reset = report(
            crate::source::TEST_POLICIES,
            &[record.clone()],
            &[Detection {
                adapter: "opencode".into(),
                accounts: Some(vec![account.clone()]),
                status: QuotaStatus::Available,
            }],
            &QuotaQuery { adapter: None },
            101,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(report_with_reset.accounts[0].status, QuotaStatus::Exhausted);
        assert_eq!(
            report_with_reset.accounts[0].resets_at.as_deref(),
            Some("1970-01-01T00:03:20Z")
        );
        assert_eq!(
            serde_json::to_value(&report_with_reset.accounts[0]).unwrap()["resets_at"],
            "1970-01-01T00:03:20Z"
        );

        let mut mixed = record;
        mixed.refusals.push(crate::Refusal {
            extra: Default::default(),
            account: mixed.account.clone(),
            model: "opencode/beam-free-alternate".into(),
            pool: Some("opencode-free".into()),
            observed_unix_seconds: 100,
            resets_unix_seconds: None,
        });
        let mixed_report = report(
            crate::source::TEST_POLICIES,
            &[mixed],
            &[Detection {
                adapter: "opencode".into(),
                accounts: Some(vec![account]),
                status: QuotaStatus::Available,
            }],
            &QuotaQuery { adapter: None },
            101,
            "report.json".into(),
        )
        .unwrap();
        assert_eq!(mixed_report.accounts[0].status, QuotaStatus::Exhausted);
        assert_eq!(mixed_report.accounts[0].resets_at, None);
    }

    #[test]
    fn scoped_refusal_is_reported_without_exhausting_the_account() {
        let account = AccountScope {
            extra: Default::default(),
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.refusals.push(crate::Refusal {
            extra: Default::default(),
            account: account.clone(),
            model: "opencode/beam-pro".into(),
            pool: Some("opencode-go".into()),
            observed_unix_seconds: 100,
            resets_unix_seconds: Some(200),
        });
        let report = report(
            crate::source::TEST_POLICIES,
            &[record],
            &[Detection {
                adapter: "opencode".into(),
                accounts: Some(vec![account]),
                status: QuotaStatus::Available,
            }],
            &QuotaQuery { adapter: None },
            101,
            "report.json".into(),
        )
        .unwrap();
        let row = &report.accounts[0];
        assert_eq!(row.status, QuotaStatus::Unknown);
        assert!(!row.free_pool_refusal);
        assert_eq!(row.resets_at, None);
        assert_eq!(row.refusals.len(), 1);
        assert_eq!(row.refusals[0].pool.as_deref(), Some("opencode-go"));
        assert_eq!(
            row.refusals[0].resets_at.as_deref(),
            Some("1970-01-01T00:03:20Z")
        );
    }
}
