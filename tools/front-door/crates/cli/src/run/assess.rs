use super::*;

// Shared with settlement assessment so the composition wrapper cannot lose a
// stop cause when the model assessment has neither a refusal nor a successor.
fn failed_result_check(policy: &provenant_routing::ModelPolicy, decoded: &run::Decoded) -> bool {
    policy.result.check == Some(true)
        && matches!(
            decoded.outcome,
            run::Outcome::Succeeded | run::Outcome::Failed | run::Outcome::Uncommitted
        )
        && (decoded.outcome != run::Outcome::Succeeded || decoded.complete)
        && !policy.result_valid(&decoded.result)
}

pub(super) fn assess_model_result(
    policy: &provenant_routing::ModelPolicy,
    task: &run::TaskRequest,
    record: &run::AttemptRecord,
    decoded: &run::Decoded,
    handoff: Option<&provenant_records::handoff::Handoff>,
    policy_attempt: u64,
) -> Result<run::ResultAssessment, String> {
    if failed_result_check(policy, decoded) {
        let retry = if decoded.outcome == run::Outcome::Succeeded
            && decoded.complete
            && !run::Service::has_writes(task.writer.is_some(), &task.access.write_roots)
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
                schema_version: 2,
                cause: run::PolicyCause::ResultCheck, action: run::PolicyAction::Retry,
                route: run::RequestedRoute::Model { extra: Default::default(), provider: fallback.provider.unwrap_or_default(), model, effort: fallback.effort },
                resume: false, max_attempts: 2,
                instruction: "The previous read-only route failed its result check. Complete the original task and include concrete evidence in your final answer.".into(),
                not_before_unix_seconds: None,
            }))
        } else {
            None
        };
        return Ok(run::ResultAssessment {
            cause: Some(run::PolicyCause::ResultCheck),
            refusal: (decoded.outcome == run::Outcome::Succeeded).then(|| {
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
        let resume = route
            .provider
            .as_deref()
            .and_then(provenant_records::AdapterSpec::get)
            .is_some_and(|spec| spec.launch.resume)
            && decoded
                .session
                .as_deref()
                .or(record.session.as_deref())
                .is_some_and(|session| !session.is_empty());
        return Ok(run::ResultAssessment {
            cause: Some(run::PolicyCause::Unfinished),
            refusal: Some("model reported unfinished work".into()),
            retry: (policy_attempt <= u64::from(policy.lane.max_continuations.unwrap_or(0))).then(|| run::PolicyRetry {
                schema_version: 2,
                cause: run::PolicyCause::Unfinished, action: if resume { run::PolicyAction::Resume } else { run::PolicyAction::Retry },
                route: run::RequestedRoute::Model { extra: Default::default(), provider: route.provider.clone().unwrap_or_default(), model, effort: route.effort.clone() },
                resume, max_attempts: u64::from(policy.lane.max_continuations.unwrap_or(0)) + 1,
                instruction: "Your turn ended with work remaining. Start the next remaining item now and write the code; do not re-audit what is done or report without changing anything. Carry on through every remaining item and its verification, keep handoff.json current, and end with the updated schema v2 handoff fence.".into(),
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

/// Check that another registered account can continue on the same model.
/// Only a quota-stopped writer on an unpinned account with a recorded,
/// unfinished handoff may continue its retained worktree on another account.
pub(super) fn quota_handoff_recovery_eligible(
    outcome: run::Outcome,
    task: &run::TaskRequest,
    handoff: Option<&provenant_records::handoff::Handoff>,
) -> bool {
    outcome == run::Outcome::QuotaLimited
        && task.writer.is_some()
        && task.account.is_none()
        && handoff.is_some_and(provenant_records::handoff::Handoff::unfinished)
}

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
    task: &run::TaskRequest,
    record: &run::AttemptRecord,
    decoded: &run::Decoded,
    policy_attempt: u64,
    now_unix_seconds: i64,
    quota_handoff_recovery: bool,
) -> run::ResultAssessment {
    // The graph seals its remaining allowance in the task profile. Ordinary
    // runs use the host routing policy directly.
    let allowance = task
        .profile
        .settlement_retry_limit
        .map(u64::from)
        .unwrap_or(u64::from(config.settlement_retries))
        .min(u64::from(provenant_routing::MAX_SETTLEMENT_RETRIES));
    let max_attempts = allowance + 1;
    let capacity = decoded.outcome == run::Outcome::ModelUnavailable
        && decoded.reason.as_deref().is_some_and(capacity_reason);
    let quota = decoded.outcome == run::Outcome::QuotaLimited;
    if !(capacity || quota) {
        let cause = match decoded.outcome {
            run::Outcome::AuthRequired => Some(run::PolicyCause::AuthRequired),
            run::Outcome::ModelUnavailable => Some(run::PolicyCause::AdapterUnavailable),
            run::Outcome::RateLimited => Some(run::PolicyCause::Quota),
            _ => record.applied_route.as_ref().and_then(|route| {
                let model = ModelId::parse(route.model.as_deref()?).ok()?;
                let now = WallTime::from_unix_seconds(now_unix_seconds).ok()?;
                let policy = config
                    .model_policy
                    .effective(&model, route.account_key.as_deref(), now)
                    .policy;
                failed_result_check(&policy, decoded).then_some(run::PolicyCause::ResultCheck)
            }),
        };
        return run::ResultAssessment {
            cause,
            ..Default::default()
        };
    }
    let typed_cause = if capacity {
        run::PolicyCause::Capacity
    } else {
        run::PolicyCause::Quota
    };
    let Some(route) = record.applied_route.as_ref() else {
        return run::ResultAssessment {
            cause: Some(typed_cause),
            ..Default::default()
        };
    };
    if allowance == 0 || policy_attempt >= max_attempts {
        return run::ResultAssessment {
            cause: Some(typed_cause),
            ..Default::default()
        };
    }
    // A settled provider has left local preparation. Route or account
    // switching is denied by the shared policy. Verified handoff recovery and
    // same-session continuation use the existing model and retained work.
    let session = decoded.session.as_deref().is_some_and(|s| !s.is_empty())
        && route
            .provider
            .as_deref()
            .and_then(provenant_records::AdapterSpec::get)
            .is_some_and(|spec| spec.launch.resume);
    if !(capacity && session || quota && quota_handoff_recovery) {
        return run::ResultAssessment {
            cause: Some(typed_cause),
            ..Default::default()
        };
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
        return run::ResultAssessment {
            cause: Some(typed_cause),
            ..Default::default()
        };
    };
    let resume = capacity;
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
    let instruction = if resume {
        "This session stopped because the provider was at capacity. Continue the original task from where it stopped and complete its verification."
    } else {
        "The previous attempt stopped at the account usage limit. Inspect the retained worktree and continue the unfinished task using its handoff; complete its verification."
    }.to_owned();
    let next = policy_attempt + 1;
    let action = if resume {
        "retrying this session"
    } else {
        "continuing from the handoff on the next eligible account"
    };
    let wait = if backoff_seconds == 0 {
        String::new()
    } else {
        format!(" in {backoff_seconds} s")
    };
    run::ResultAssessment {
        cause: Some(typed_cause),
        refusal: None,
        note: Some(format!("{action}{wait} (attempt {next} of {max_attempts})")),
        retry: Some(run::PolicyRetry {
            schema_version: 2,
            cause: typed_cause,
            action: if resume {
                run::PolicyAction::Resume
            } else {
                run::PolicyAction::Retry
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assessment_records_typed_causes() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        std::fs::create_dir_all(root.join("home"))?;
        std::fs::create_dir_all(root.join("checkout"))?;
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(root.join("state"));
        let context = run::ExecutionContext {
            state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: provenant_core::ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|error| error.message)?;
        let store = run::RunStore::open(&context, &call)?;
        let input =
            super::super::tests::trace_run(40, AttemptId::from_bytes([41; 16]), clock.wall_now()?)?;
        store.create(&input, &call)?;
        let mut record = store.record(input.attempts[0].attempt_id, &call)?;
        record.applied_route = Some(run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6-luna".into()),
            ..Default::default()
        });
        let task = &input.request.tasks[0];
        let mut decoded = run::Decoded {
            outcome: run::Outcome::Succeeded,
            result: String::new(),
            complete: true,
            observed_route: None,
            usage: Default::default(),
            session: None,
            reason: None,
            partial_signals: Vec::new(),
        };
        let policy = provenant_routing::ModelPolicies::default()
            .effective(
                &ModelId::parse("agy/gemini-3.8-flash")?,
                None,
                provenant_core::WallTime::from_unix_seconds(0).unwrap(),
            )
            .policy;
        assert_eq!(
            assess_model_result(&policy, task, &record, &decoded, None, 1)?.cause,
            Some(run::PolicyCause::ResultCheck)
        );
        // Exercise the composition wrapper after the fallback has been consumed.
        // It used to discard a stop cause when no successor remained.
        record.continuation = Some("policy_fallback".into());
        record.applied_route.as_mut().unwrap().model = Some("agy/gemini-3.8-flash".into());
        for outcome in [run::Outcome::Failed, run::Outcome::Uncommitted] {
            decoded.outcome = outcome;
            let assessment = run::Providers::assess_result(
                &Registry::default(),
                task,
                &record,
                &decoded,
                None,
                None,
                1,
            )?;
            assert_eq!(assessment.cause, Some(run::PolicyCause::ResultCheck));
            assert!(assessment.retry.is_none());
            assert!(assessment.refusal.is_none());
        }
        // A provider failure with a valid answer did not fail a result check.
        decoded.outcome = run::Outcome::Failed;
        decoded.result = "A complete answer with concrete verification evidence.".into();
        assert!(policy.result_valid(&decoded.result));
        let assessment = run::Providers::assess_result(
            &Registry::default(),
            task,
            &record,
            &decoded,
            None,
            None,
            1,
        )?;
        assert!(assessment.cause.is_none());
        assert!(assessment.retry.is_none());
        let mut unfinished_policy = policy.clone();
        unfinished_policy.result.check = Some(false);
        unfinished_policy.lane.stop_early_ok = Some(false);
        decoded.outcome = run::Outcome::Uncommitted;
        let handoff = provenant_records::handoff::Handoff::from_json(br#"{"schema_version":2,"status":"stopped_early","note":"Work remains","remaining":["Verify"],"decisions":[],"verify":[]}"#)?;
        let assessment = assess_model_result(
            &unfinished_policy,
            task,
            &record,
            &decoded,
            Some(&handoff),
            1,
        )?;
        assert_eq!(assessment.cause, Some(run::PolicyCause::Unfinished));
        assert!(assessment.refusal.is_some());
        decoded.outcome = run::Outcome::QuotaLimited;
        let decision = assess_settlement_retry(
            &provenant_routing::RoutingConfig::default(),
            task,
            &record,
            &decoded,
            1,
            0,
            false,
        );
        assert_eq!(decision.cause, Some(run::PolicyCause::Quota));
        decoded.outcome = run::Outcome::ModelUnavailable;
        decoded.reason = Some("provider capacity: busy".into());
        decoded.session = Some("fixture".into());
        let decision = assess_settlement_retry(
            &provenant_routing::RoutingConfig::default(),
            task,
            &record,
            &decoded,
            1,
            0,
            false,
        );
        assert_eq!(decision.cause, Some(run::PolicyCause::Capacity));
        assert_eq!(decision.retry.unwrap().cause, run::PolicyCause::Capacity);
        Ok(())
    }
}
