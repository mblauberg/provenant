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

const INTERVALS: &[(&str, u64, u64)] = &[
    ("claude", 300, 120),
    ("codex", 900, 300),
    ("cursor", 1800, 600),
    ("agy", 1200, 900),
];
pub const TICK_BUDGET_SECONDS: i64 = 55;
pub fn interval(adapter: &str, trigger: Trigger, power: Power) -> Option<u64> {
    let (_, timer, panel) = INTERVALS.iter().find(|(id, _, _)| *id == adapter)?;
    if adapter == "agy" && power == Power::Low {
        return None;
    }
    let base = match trigger {
        Trigger::Manual => 30,
        Trigger::Panel => *panel,
        _ => *timer,
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
    record: &AccountRecord,
    trigger: Trigger,
    power: Power,
) -> Result<Option<i64>, QuotaError> {
    let Some(ttl) = interval(&record.account.adapter, trigger, power) else {
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
        ProbeFailure::Malformed => 120,
    };
    base.max(retry_after_seconds.unwrap_or(0).min(86400))
}
pub fn reserve(
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
    if record.account != *account || next_due(&record, trigger, power)?.is_none_or(|due| due > now)
    {
        return Err(QuotaError::Conflict);
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
        account: account.clone(),
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
    records: &[AccountRecord],
    detections: &[Detection],
    tick: &QuotaTick,
    now: i64,
) -> Result<TickPlan, QuotaError> {
    plan_tick_with_settings(records, detections, tick, now, &Settings::default())
}
pub fn plan_tick_with_settings(
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
        if selected(&record.account.adapter)
            && detections.iter().any(|d| {
                d.adapter == record.account.adapter
                    && d.accounts.as_ref().is_some_and(|accounts| {
                        !accounts.iter().any(|a| {
                            a.adapter == record.account.adapter
                                && a.account_key == record.account.account_key
                        })
                    })
            })
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
        .filter(|d| selected(&d.adapter))
        .filter_map(|d| d.accounts.as_ref())
        .flatten()
    {
        let empty = AccountRecord::empty(account.clone());
        let current = records
            .iter()
            .find(|r| {
                r.account.adapter == account.adapter && r.account.account_key == account.account_key
            })
            .unwrap_or(&empty);
        if next_due(current, tick.trigger, tick.power)?.is_some_and(|due| due <= now) {
            if account.adapter == "agy" {
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
                QuotaStatus::Unknown
                    | QuotaStatus::AuthRequired
                    | QuotaStatus::Unsupported
                    | QuotaStatus::Stale
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
        let age = record.snapshot.as_ref().map(|s| {
            u64::try_from(now.saturating_sub(s.observed_unix_seconds).max(0)).unwrap_or(u64::MAX)
        });
        let fresh = record.snapshot.as_ref().is_some_and(|s| {
            s.observed_unix_seconds <= now
                && age.is_some_and(|a| {
                    a < interval(&record.account.adapter, Trigger::Timer, Power::Ac).unwrap_or(0)
                })
        });
        let status = match &record.snapshot {
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
            next_due_unix_seconds: next_due(record, Trigger::Timer, Power::Ac)?.map(|d| d.max(now)),
            refreshing: record
                .claim
                .as_ref()
                .is_some_and(|c| c.deadline_unix_seconds > now),
            error: record.last_error,
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
            total: (accounts.len() + sources.len()) as u64,
            known,
        },
        accounts,
        sources,
        refreshing_ids,
        next_due_unix_seconds,
        report_path: path,
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
        assert_eq!(next_due(&r, Trigger::Manual, Power::Ac).unwrap(), Some(400));
        let claim = reserve(Some(&r), &r.account, 400, "new", Trigger::Manual, Power::Ac).unwrap();
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
            reserve(Some(&r), &r.account, 399, "new", Trigger::Manual, Power::Ac),
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
            windows: vec![],
        });
        let intent = reserve(Some(&r), &a, 400, "claim", Trigger::Manual, Power::Ac).unwrap();
        let current = intent.replacement.unwrap();
        assert!(matches!(
            complete(&current, "claim", 456, Err(ProbeFailure::Network), None),
            Err(QuotaError::Conflict)
        ));
        let failed = complete(&current, "claim", 420, Err(ProbeFailure::Network), None).unwrap();
        assert_eq!(failed.replacement.as_ref().unwrap().snapshot, r.snapshot);
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
        let records = [AccountRecord::empty(account("claude", "old"))];
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
            plan_tick(&records, &unknown, &tick, 100)
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
        let plan = plan_tick(&records, &detected, &tick, 100).unwrap();
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
            plan_tick(&records, &detected, &low, 100)
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
            Some(&r),
            &r.account,
            100,
            "first",
            Trigger::Manual,
            Power::Ac,
        )
        .unwrap();
        let second = reserve(
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
            &[record],
            &[],
            &QuotaQuery { adapter: None },
            100,
            "fixture".into(),
        )
    }
}
