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
        && decoded.outcome == run::Outcome::Succeeded
        && decoded.complete
        && failed_check
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
            fallback.filter(|fallback| task.account.is_none() || fallback.provider.as_deref() == record.applied_route.as_ref().and_then(|route| route.provider.as_deref())).and_then(|fallback| fallback.model.map(|model| run::PolicyRetry {
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

/// A capacity stop may continue its existing session within the configured
/// retry budget. A settled attempt never automatically switches routes.
pub(super) fn assess_settlement_retry(
    config: &provenant_routing::RoutingConfig,
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
    // A settled provider has left local preparation. Route or account
    // switching is denied by the shared policy. Same-session continuation is
    // not a route switch and remains available for a capacity stop.
    let session = decoded.session.as_deref().is_some_and(|s| !s.is_empty())
        && route
            .provider
            .as_deref()
            .and_then(provenant_records::AdapterSpec::get)
            .is_some_and(|spec| spec.launch.resume);
    if !capacity || !session {
        return Default::default();
    }
    let Some(route_choice) =
        route
            .provider
            .clone()
            .zip(route.model.clone())
            .map(|(provider, model)| run::RequestedRoute::Model {
                extra: Default::default(),
                provider,
                model,
                effort: route.effort.clone(),
            })
    else {
        return Default::default();
    };
    let resume = true;
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
    let instruction = "This session stopped because the provider was at capacity. Continue the original task from where it stopped and complete its verification.".to_owned();
    let next = policy_attempt + 1;
    let action = "retrying this session";
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
