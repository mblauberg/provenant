//! One derivation of an account's effective status and the models it can
//! serve now. Reporting, routing previews and admission call this function;
//! no consumer assembles its own verdict from a snapshot alone.
use crate::schedule::Detection;
use crate::*;
use serde::{Deserialize, Serialize};

/// Why one offered model cannot be served right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelExclusion {
    AuthRequired,
    Refused,
    Exhausted,
    Stale,
    Unknown,
}
impl ModelExclusion {
    /// All persisted exclusions, for exhaustive stored-format tables.
    pub const ALL: &[Self] = &[
        Self::AuthRequired,
        Self::Refused,
        Self::Exhausted,
        Self::Stale,
        Self::Unknown,
    ];
}

/// What the menu can show for one model without any logic of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAvailability {
    pub model: String,
    pub servable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ModelExclusion>,
}

/// One account's stored readiness file, passed to reporting by the caller
/// that owns the state root.
pub struct ReadinessInput<'a> {
    pub adapter: &'a str,
    pub account_key: &'a str,
    pub file: &'a ReadinessFile,
}

/// Inputs owned by the caller. Readiness is the account's whole file: the
/// account-wide observation and every model and pool scope it contains.
pub struct StatusInput<'a> {
    pub record: &'a AccountRecord,
    pub detections: &'a [Detection],
    pub readiness: Option<&'a ReadinessFile>,
    pub pools: &'a [String],
    pub now: i64,
    pub freshness: &'a FreshnessPolicy,
    pub policies: &'a [QuotaPolicy],
    pub policy: &'a provenant_records::policy::Policy,
}

/// The single verdict, plus the display rows that must agree with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveStatus {
    pub status: QuotaStatus,
    pub models: Vec<ModelAvailability>,
    pub refusals: Vec<RefusalRow>,
    pub free_pool_refusal: bool,
    pub resets_at: Option<String>,
}

fn exclusion(reason: Option<ReadinessReason>) -> ModelExclusion {
    match reason {
        Some(
            ReadinessReason::SignedOut
            | ReadinessReason::LoginExpired
            | ReadinessReason::AccountMismatch,
        ) => ModelExclusion::AuthRequired,
        Some(ReadinessReason::Exhausted | ReadinessReason::ProviderRefusal) => {
            ModelExclusion::Exhausted
        }
        Some(ReadinessReason::Stale) => ModelExclusion::Stale,
        _ => ModelExclusion::Unknown,
    }
}

/// Derive the account's status and its per-model service list from one
/// snapshot, all readiness scopes and the models the account offers.
pub fn effective_status(input: &StatusInput<'_>) -> Result<EffectiveStatus, QuotaError> {
    let StatusInput {
        record,
        detections,
        readiness,
        pools,
        now,
        freshness,
        policies,
        policy,
    } = *input;
    let adapter = &record.account.adapter;
    let account_key = &record.account.account_key;
    let auth_required = record.last_error.is_some_and(ProbeFailure::needs_login)
        || (!record
            .auth_verified_unix_seconds
            .is_some_and(|at| freshness.login_verification_fresh(at, now))
            && detections.iter().any(|d| {
                d.adapter == *adapter
                    && d.status == QuotaStatus::AuthRequired
                    && d.accounts.as_ref().is_some_and(|a| a.len() == 1)
            }));
    let fresh = record.snapshot.as_ref().is_some_and(|s| {
        s.observed_unix_seconds <= now
            && record.last_error.is_none()
            && !auth_required
            && freshness.snapshot_fresh(s, now, policies)
    });
    let auth_refresh_pending = record.auth_state.as_ref().is_some_and(|auth| {
        auth.signed_in == Some(true)
            && auth.refresh_pending
            && auth
                .observed_unix_seconds
                .is_some_and(|at| freshness.refresh_pending_fresh(at, now, policies, adapter))
    });
    let live: Vec<_> = record
        .refusals
        .iter()
        .filter(|refusal| {
            refusal.observed_unix_seconds <= now
                && refusal
                    .expires_unix_seconds(freshness)
                    .is_ok_and(|expires| expires > now)
        })
        .collect();
    let refusals: Vec<_> = live
        .iter()
        .map(|refusal| RefusalRow {
            model: refusal.model.clone(),
            pool: refusal.pool.clone(),
            resets_at: refusal
                .resets_unix_seconds
                .filter(|reset| *reset > now)
                .and_then(|reset| jiff::Timestamp::from_second(reset).ok())
                .map(|timestamp| timestamp.to_string()),
        })
        .collect();
    let free_pool = provenant_records::AdapterSpec::get(adapter)
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
    let in_free =
        |refusal: &&RefusalRow| free_pool.is_some_and(|pool| refusal.pool.as_deref() == Some(pool));
    let free_refused = refusals.iter().any(|row| in_free(&row));
    let account_admission = Admission {
        policy,
        policies,
        freshness,
        now_unix_seconds: now,
        records: std::slice::from_ref(record),
        pools: &[],
    };
    let mut account_refusals = Vec::new();
    for (refusal, row) in live.iter().zip(&refusals) {
        if (in_free(&row) || row.pool.as_deref() == Some(ACCOUNT_ROUTE))
            && account_admission.refusal_holds(adapter, Some(record), refusal)?
        {
            account_refusals.push(row);
        }
    }
    let account_refused = !account_refusals.is_empty();
    let mut status = match &record.snapshot {
        _ if record.last_error == Some(ProbeFailure::IsolatedKeychainUnavailable) => {
            QuotaStatus::KeychainUnavailable
        }
        _ if account_refused => QuotaStatus::Exhausted,
        _ if auth_required => QuotaStatus::AuthRequired,
        _ if auth_refresh_pending
            || (record.last_error == Some(ProbeFailure::RefreshDeferred)
                && record.last_attempt_unix_seconds.is_some_and(|at| {
                    freshness.refresh_pending_fresh(at, now, policies, adapter)
                })) =>
        {
            QuotaStatus::RefreshPending
        }
        Some(snapshot) if fresh => snapshot.status,
        Some(_) => QuotaStatus::Stale,
        None if record.last_error == Some(ProbeFailure::Disabled) => QuotaStatus::Unsupported,
        None => QuotaStatus::Unknown,
    };
    let resets_at = if account_refused {
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

    // Readiness is read at every scope, not only the account-wide one. An
    // account-wide authentication or exhaustion verdict is an account verdict;
    // model and pool scopes qualify individual models below.
    let account_scope = ReadinessScope {
        extra: Default::default(),
        host: LOCAL_HOST.into(),
        adapter: adapter.clone(),
        account_key: account_key.clone(),
        route: ReadinessRoute::Account,
    };
    let account_read = readiness.map(|file| file.get(&account_scope, now, freshness));
    if let Some(read) = &account_read
        && read.state == ReadinessState::Unavailable
        && read
            .reason
            .is_some_and(ReadinessReason::is_authentication_failure)
    {
        status = QuotaStatus::AuthRequired;
    } else if let Some(read) = &account_read
        && read.state == ReadinessState::Unavailable
        && matches!(
            read.reason,
            Some(ReadinessReason::Exhausted | ReadinessReason::ProviderRefusal)
        )
    {
        status = QuotaStatus::Exhausted;
    } else if let Some(read) = &account_read
        && read.observed_scope.is_some()
        && read.reason == Some(ReadinessReason::Stale)
        && matches!(status, QuotaStatus::Available | QuotaStatus::CreditOnly)
    {
        // An expired account observation is stale here as it is in routing.
        status = QuotaStatus::Stale;
    }

    let offered: &[String] = record
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.details.as_ref())
        .map_or(&[], |details| details.models.as_slice());
    let admission = Admission {
        policy,
        policies,
        freshness,
        now_unix_seconds: now,
        records: std::slice::from_ref(record),
        pools,
    };
    let mut models = Vec::with_capacity(offered.len());
    for model in offered {
        let scope = ReadinessScope {
            route: ReadinessRoute::model(adapter, model),
            ..account_scope.clone()
        };
        let read = readiness
            .map(|file| file.get(&scope, now, freshness))
            .unwrap_or_else(ReadinessRead::absent);
        let mut reason = if read.state == ReadinessState::Unavailable {
            Some(exclusion(read.reason))
        } else if read.observed_scope.is_some() && read.reason == Some(ReadinessReason::Stale) {
            Some(ModelExclusion::Stale)
        } else {
            None
        };
        for pool in pools {
            let pool_scope = ReadinessScope {
                route: ReadinessRoute::pool(pool.clone()),
                ..account_scope.clone()
            };
            let pool_read = readiness
                .map(|file| file.get(&pool_scope, now, freshness))
                .unwrap_or_else(ReadinessRead::absent);
            if pool_read.state == ReadinessState::Unavailable {
                reason = Some(exclusion(pool_read.reason));
                break;
            }
            if pool_read.observed_scope.is_some()
                && pool_read.reason == Some(ReadinessReason::Stale)
            {
                reason = Some(ModelExclusion::Stale);
                break;
            }
        }
        if reason.is_none()
            && let Some(block) = admission.account_block(adapter, account_key, model)?
        {
            reason = Some(match block {
                AccountBlock::Mismatch => ModelExclusion::AuthRequired,
                AccountBlock::FreeTierRefusal | AccountBlock::ScopedRefusal => {
                    ModelExclusion::Refused
                }
            });
        }
        if reason.is_none()
            && let Some(snapshot) = record.snapshot.as_ref()
            && matches!(
                partial_headroom(snapshot, adapter, model, pools, now),
                PartialHeadroom::Exhausted
            )
        {
            reason = Some(ModelExclusion::Exhausted);
        }
        if reason.is_none()
            && !matches!(
                status,
                QuotaStatus::Available | QuotaStatus::CreditOnly | QuotaStatus::Exhausted
            )
        {
            reason = Some(if status == QuotaStatus::AuthRequired {
                ModelExclusion::AuthRequired
            } else {
                ModelExclusion::Stale
            });
        }
        models.push(ModelAvailability {
            model: model.clone(),
            servable: reason.is_none(),
            reason,
        });
    }
    // A status that says available cannot disagree with every model being
    // excluded: the status follows the models when an offer list is known.
    if !offered.is_empty()
        && models.iter().all(|model| !model.servable)
        && matches!(status, QuotaStatus::Available | QuotaStatus::CreditOnly)
    {
        status = if models
            .iter()
            .any(|model| model.reason == Some(ModelExclusion::AuthRequired))
        {
            QuotaStatus::AuthRequired
        } else if models
            .iter()
            .all(|model| model.reason == Some(ModelExclusion::Exhausted))
        {
            QuotaStatus::Exhausted
        } else {
            QuotaStatus::Stale
        };
    }
    Ok(EffectiveStatus {
        status,
        models,
        refusals,
        free_pool_refusal: free_refused,
        resets_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> AccountScope {
        AccountScope {
            extra: Default::default(),
            adapter: "claude".into(),
            account_key: "key".into(),
            label: "work".into(),
        }
    }

    fn record(status: QuotaStatus, observed: i64, models: &[&str]) -> AccountRecord {
        let account = scope();
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(QuotaSnapshot::test_snapshot(
            account,
            status,
            observed,
            Vec::new(),
        ));
        if let Some(snapshot) = record.snapshot.as_mut() {
            snapshot.details = Some(QuotaDetails {
                models: models.iter().map(|model| (*model).into()).collect(),
                ..Default::default()
            });
        }
        record
    }

    fn observation(state: ReadinessState, reason: Option<ReadinessReason>) -> ReadinessFile {
        ReadinessFile {
            extra: Default::default(),
            schema_version: 1,
            records: vec![RouteReadiness {
                extra: Default::default(),
                schema_version: 1,
                scope: ReadinessScope {
                    extra: Default::default(),
                    host: LOCAL_HOST.into(),
                    adapter: "claude".into(),
                    account_key: "key".into(),
                    route: ReadinessRoute::Account,
                },
                state,
                reason,
                observed_unix_seconds: 100,
                expires_unix_seconds: 200,
                reset_unix_seconds: None,
                evidence_ref: Some("fixture".into()),
            }],
            login: None,
        }
    }

    fn status_of<'a>(
        record: &'a AccountRecord,
        file: Option<&'a ReadinessFile>,
        now: i64,
        policy: &'a provenant_records::policy::Policy,
        freshness: &'a FreshnessPolicy,
    ) -> Result<EffectiveStatus, Box<dyn std::error::Error>> {
        effective_status(&StatusInput {
            record,
            detections: &[],
            readiness: file,
            pools: &[],
            now,
            freshness,
            policies: crate::source::TEST_POLICIES,
            policy,
        })
        .map_err(Into::into)
    }

    #[test]
    fn status_and_admission_agree_for_the_same_snapshot_and_readiness()
    -> Result<(), Box<dyn std::error::Error>> {
        let record = record(QuotaStatus::Available, 100, &["model"]);
        let file = observation(ReadinessState::Ready, None);
        let policy = provenant_records::policy::Policy::default();
        let freshness = DEFAULT_FRESHNESS;
        let effective = status_of(&record, Some(&file), 150, &policy, &freshness)?;
        assert_eq!(effective.status, QuotaStatus::Available);
        assert_eq!(effective.models.len(), 1);
        assert!(effective.models[0].servable);
        let admission = Admission {
            policy: &policy,
            policies: crate::source::TEST_POLICIES,
            freshness: &freshness,
            now_unix_seconds: 150,
            records: std::slice::from_ref(&record),
            pools: &[],
        };
        assert_eq!(admission.account_block("claude", "key", "model")?, None);

        // A provider refusal in readiness makes both verdicts unavailable.
        let refused = observation(
            ReadinessState::Unavailable,
            Some(ReadinessReason::ProviderRefusal),
        );
        let blocked = status_of(&record, Some(&refused), 150, &policy, &freshness)?;
        assert_eq!(blocked.status, QuotaStatus::Exhausted);
        assert!(blocked.models.iter().all(|model| !model.servable));
        Ok(())
    }

    #[test]
    fn an_expired_observation_is_stale_for_readiness_and_status()
    -> Result<(), Box<dyn std::error::Error>> {
        let record = record(QuotaStatus::Available, 100, &["model"]);
        let file = observation(ReadinessState::Ready, None);
        let freshness = DEFAULT_FRESHNESS;
        let scope = ReadinessScope {
            extra: Default::default(),
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: ReadinessRoute::Account,
        };
        let read = file.get(&scope, 250, &freshness);
        assert_ne!(read.state, ReadinessState::Ready);
        assert_eq!(read.reason, Some(ReadinessReason::Stale));
        assert!(!matches!(
            read.project(),
            provenant_records::RouteReadiness::Ready { .. }
        ));
        let policy = provenant_records::policy::Policy::default();
        let effective = status_of(&record, Some(&file), 250, &policy, &freshness)?;
        assert_ne!(effective.status, QuotaStatus::Available);
        assert!(effective.models.iter().all(|model| !model.servable));
        Ok(())
    }
}
