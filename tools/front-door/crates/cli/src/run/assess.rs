use super::*;

pub(super) fn assess_model_result(
    policy: &provenant_routing::ModelPolicy,
    task: &run::TaskRequest,
    record: &run::AttemptRecord,
    decoded: &run::Decoded,
    handoff: Option<&provenant_records::handoff::Handoff>,
    policy_attempt: u64,
) -> Result<run::ResultAssessment, String> {
    let failed_check = !policy.result_valid(&decoded.result);
    if policy.result.check == Some(true)
        && matches!(
            decoded.outcome,
            run::Outcome::Succeeded | run::Outcome::Failed | run::Outcome::Uncommitted
        )
        && (failed_check || decoded.outcome == run::Outcome::Failed)
    {
        let retry = if !run::Service::has_writes(task.writer.is_some(), &task.access.write_roots)
            && record.continuation.as_deref() != Some("policy_fallback")
        {
            // Caller alternatives take precedence over the bundled fallback.
            let fallback = task.fallback.first().cloned().or_else(|| {
                policy
                    .result
                    .fallback
                    .as_ref()
                    .map(|model| run::RouteRecord {
                        provider: Some(model.adapter().into()),
                        model: Some(model.as_str().into()),
                        effort: policy.result.fallback_effort.clone(),
                        ..Default::default()
                    })
            });
            fallback.and_then(|fallback| fallback.model.map(|model| run::PolicyRetry {
                schema_version: 1,
                route: run::RequestedRoute::Model { extra: Default::default(), provider: fallback.provider.unwrap_or_default(), model, effort: fallback.effort },
                resume: false, max_attempts: 2,
                instruction: "The previous read-only route failed its result check. Complete the original task and include concrete evidence in your final answer.".into(),
                not_before_unix_seconds: None,
            }))
        } else {
            None
        };
        return Ok(run::ResultAssessment {
            refusal: (failed_check && decoded.outcome == run::Outcome::Succeeded).then(|| {
                "model policy result check failed: empty answer or missing expected sections".into()
            }),
            retry,
            note: None,
        });
    }
    if policy.lane.stop_early_ok == Some(false)
        && matches!(
            decoded.outcome,
            run::Outcome::Succeeded | run::Outcome::Uncommitted
        )
        && decoded.complete
        && handoff.is_some_and(|handoff| {
            handoff.status == provenant_records::handoff::HandoffStatus::StoppedEarly
        })
    {
        let route = record
            .applied_route
            .as_ref()
            .ok_or("continuation route missing")?;
        let model = route.model.clone().ok_or("continuation model missing")?;
        return Ok(run::ResultAssessment {
            refusal: Some("model reported unfinished work".into()),
            retry: (policy_attempt <= u64::from(policy.lane.max_continuations.unwrap_or(0))).then(|| run::PolicyRetry {
                schema_version: 1,
                route: run::RequestedRoute::Model { extra: Default::default(), provider: route.provider.clone().unwrap_or_default(), model, effort: route.effort.clone() },
                resume: record.applied_route.as_ref().and_then(|route| route.provider.as_deref()).and_then(provenant_records::AdapterSpec::get).is_some_and(|spec| spec.launch.resume)
                    && decoded.session.as_deref().or(record.session.as_deref()).is_some_and(|session| !session.is_empty()), max_attempts: u64::from(policy.lane.max_continuations.unwrap_or(0)) + 1,
                instruction: "Continue the unfinished task using the handoff and existing worktree. Carry out the remaining work and verification, keep handoff.json current, and end with the updated schema v2 handoff fence.".into(),
                not_before_unix_seconds: None,
            }),
            note: None,
        });
    }
    Ok(Default::default())
}

/// The reason prefix of a provider capacity settlement, shared by decode,
/// retry and `lanes`.
pub(super) const CAPACITY: &str = "provider capacity";

pub(super) fn capacity_reason(reason: &str) -> bool {
    reason
        .strip_prefix(CAPACITY)
        .is_some_and(|rest| rest.starts_with(": "))
}

/// A lane that settled for provider capacity or a spent account is typed, then
/// retried within `domains.routing.settlement_retries`: a capacity stop resumes
/// its session after a growing backoff, or redraws when it has none, and a
/// quota stop redraws at once, where admission skips the spent account.
/// Whether another registered account for `model`'s adapter, other than
/// `spent_key`, serves the model and is not known to be spent: no persisted
/// mismatch or live refusal, as quota's `account_block` assesses them, and no
/// reading that routing's rule calls spent. Invalid evidence is not ready.
pub(super) fn unspent_alternative<'k>(
    keys: impl IntoIterator<Item = &'k str>,
    quota: &[provenant_quota::AccountRecord],
    policies: &[provenant_quota::QuotaPolicy],
    spent_key: &str,
    model: &ModelId,
    catalogue: &provenant_routing::Catalogue,
    now: i64,
) -> bool {
    let adapter = model.adapter();
    let model_pool = catalogue.quota_pool(model);
    let pools = catalogue.admission_pools(model);
    let admission = provenant_quota::Admission {
        policies,
        now_unix_seconds: now,
        records: quota,
        pools: &pools,
    };
    let spent = |key: &str| {
        admission
            .account_block(adapter, key, model.model())
            .map_or(true, |block| block.is_some())
            || quota.iter().any(|record| {
                record.account.adapter == adapter
                    && record.account.account_key == key
                    && record.snapshot.as_ref().is_some_and(|snapshot| {
                        provenant_routing::quota_reading_spent(snapshot, model, model_pool, now)
                    })
            })
    };
    keys.into_iter().any(|key| {
        key != spent_key
            && provenant_records::AdapterSpec::get(adapter)
                .is_none_or(|spec| spec.account_serves_model(key, model.model()))
            && !spent(key)
    })
}

pub(super) fn assess_settlement_retry(
    config: &provenant_routing::RoutingConfig,
    other_account: impl Fn(&str, &str) -> bool,
    task: &run::TaskRequest,
    record: &run::AttemptRecord,
    decoded: &run::Decoded,
    policy_attempt: u64,
    now_unix_seconds: i64,
) -> run::ResultAssessment {
    let max_attempts = u64::from(config.settlement_retries) + 1;
    let capacity = decoded.outcome == run::Outcome::ModelUnavailable
        && decoded.reason.as_deref().is_some_and(capacity_reason);
    let quota = decoded.outcome == run::Outcome::QuotaLimited;
    let Some(route) = record.applied_route.as_ref() else {
        return Default::default();
    };
    if !(capacity || quota) || config.settlement_retries == 0 || policy_attempt >= max_attempts {
        return Default::default();
    }
    let writes = run::Service::has_writes(task.writer.is_some(), &task.access.write_roots);
    let applied = || {
        Some(run::RequestedRoute::Model {
            extra: Default::default(),
            provider: route.provider.clone()?,
            model: route.model.clone()?,
            effort: route.effort.clone(),
        })
    };
    // Resume only where the adapter's launch table says it can.
    let session = decoded.session.as_deref().is_some_and(|s| !s.is_empty())
        && route
            .provider
            .as_deref()
            .and_then(provenant_records::AdapterSpec::get)
            .is_some_and(|spec| spec.launch.resume);
    let (route_choice, resume) = if capacity && session {
        (applied(), true)
    } else if quota {
        // A pinned account is the caller's choice. Otherwise another unspent
        // account must serve the applied model, and the retry keeps that model
        // so a redraw cannot reach the spent account through another model;
        // admission then refuses the spent account and resolution falls through.
        let elsewhere = task.account.is_none()
            && route
                .provider
                .as_deref()
                .zip(route.account_key.as_deref())
                .is_some_and(|(adapter, key)| other_account(adapter, key));
        (elsewhere.then(applied).flatten(), false)
    } else if !writes && let Some(fallback) = task.fallback.first() {
        let fallback = fallback
            .model
            .clone()
            .map(|model| run::RequestedRoute::Model {
                extra: Default::default(),
                provider: fallback.provider.clone().unwrap_or_default(),
                model,
                effort: fallback.effort.clone(),
            });
        (fallback, false)
    } else {
        (Some(task.route.clone()), false)
    };
    let Some(route_choice) = route_choice else {
        return Default::default();
    };
    // Linear backoff with up to a quarter more, drawn from the attempt ID so
    // lanes that hit the same capacity error do not retry in step.
    let backoff_seconds = if capacity {
        let base = config.capacity_retry_seconds.saturating_mul(policy_attempt);
        let draw = u64::from_le_bytes(
            record.attempt_id.as_bytes()[..8]
                .try_into()
                .unwrap_or_default(),
        );
        base.saturating_add(draw % (base / 4 + 1))
            .min(config.capacity_retry_max_seconds)
            .min(config.retry_delay_ceiling_seconds)
    } else {
        0
    };
    let cause = if capacity {
        "the provider was at capacity"
    } else {
        "the account's usage limit was reached"
    };
    let instruction = if resume {
        format!(
            "This session stopped because {cause}. Continue the original task from where it stopped and complete its verification."
        )
    } else if writes {
        format!(
            "A previous attempt at this task stopped because {cause}. The worktree may hold its partial work: inspect `git status` and `git log` first, and before pushing or opening a pull request check whether the branch was already pushed and a pull request already exists. Keep what is useful, then complete the original task."
        )
    } else {
        format!(
            "A previous attempt at this task stopped because {cause}. Complete the original task."
        )
    };
    let next = policy_attempt + 1;
    let action = match (resume, quota) {
        (true, _) => "retrying this session",
        (false, true) => "retrying on the next eligible account",
        (false, false) => "redrawing the route",
    };
    let wait = if backoff_seconds == 0 {
        String::new()
    } else {
        format!(" in {backoff_seconds} s")
    };
    run::ResultAssessment {
        refusal: None,
        note: Some(format!("{action}{wait} (attempt {next} of {max_attempts})")),
        retry: Some(run::PolicyRetry {
            schema_version: 1,
            route: route_choice,
            resume,
            max_attempts,
            instruction,
            not_before_unix_seconds: (backoff_seconds > 0).then(|| {
                now_unix_seconds.saturating_add(i64::try_from(backoff_seconds).unwrap_or(600))
            }),
        }),
    }
}
