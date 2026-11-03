use super::*;

// Shared with settlement assessment so the composition wrapper cannot lose a
// stop cause when the model assessment has neither a refusal nor a successor.
fn failed_result_check(policy: &provenant_routing::ModelPolicy, decoded: &run::Decoded) -> bool {
    policy.result.check == Some(true)
        && matches!(
            decoded.outcome,
            run::Outcome::Succeeded | run::Outcome::Failed | run::Outcome::Uncommitted
        )
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
                cause: Some(run::PolicyCause::ResultCheck), action: Some(run::PolicyAction::Retry),
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
                schema_version: 1,
                cause: Some(run::PolicyCause::Unfinished), action: Some(if resume { run::PolicyAction::Resume } else { run::PolicyAction::Retry }),
                route: run::RequestedRoute::Model { extra: Default::default(), provider: route.provider.clone().unwrap_or_default(), model, effort: route.effort.clone() },
                resume, max_attempts: u64::from(policy.lane.max_continuations.unwrap_or(0)) + 1,
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
    if !(capacity || quota) {
        let cause = record.applied_route.as_ref().and_then(|route| {
            let model = ModelId::parse(route.model.as_deref()?).ok()?;
            let now = WallTime::from_unix_seconds(now_unix_seconds).ok()?;
            let policy = config
                .model_policy
                .effective(&model, route.account_key.as_deref(), now)
                .policy;
            failed_result_check(&policy, decoded).then_some(run::PolicyCause::ResultCheck)
        });
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
    if config.settlement_retries == 0 || policy_attempt >= max_attempts {
        return run::ResultAssessment {
            cause: Some(typed_cause),
            ..Default::default()
        };
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
    // A graph writer may only redraw after the graph has verified released
    // custody, its pinned source, a clean checkout and no publication. The
    // settlement assessor has none of that evidence, so it must not redraw an
    // alias or switch accounts on its own. Family-constrained routes must not
    // be redrawn either.
    // Session continuation stays on the applied model.
    if task
        .context
        .as_ref()
        .is_some_and(|context| context.graph_id.is_some())
        && ((quota && writes)
            || (capacity
                && !session
                && (matches!(task.route, run::RequestedRoute::Review { .. })
                    || (writes && matches!(task.route, run::RequestedRoute::Named { .. })))))
    {
        return run::ResultAssessment {
            cause: Some(typed_cause),
            ..Default::default()
        };
    }
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
        return run::ResultAssessment {
            cause: Some(typed_cause),
            ..Default::default()
        };
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
        cause: Some(typed_cause),
        refusal: None,
        note: Some(format!("{action}{wait} (attempt {next} of {max_attempts})")),
        retry: Some(run::PolicyRetry {
            schema_version: 1,
            cause: Some(typed_cause),
            action: Some(if resume {
                run::PolicyAction::Resume
            } else {
                run::PolicyAction::Retry
            }),
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
            |_, _| false,
            task,
            &record,
            &decoded,
            1,
            0,
        );
        assert_eq!(decision.cause, Some(run::PolicyCause::Quota));
        decoded.outcome = run::Outcome::ModelUnavailable;
        decoded.reason = Some("provider capacity: busy".into());
        let decision = assess_settlement_retry(
            &provenant_routing::RoutingConfig::default(),
            |_, _| false,
            task,
            &record,
            &decoded,
            1,
            0,
        );
        assert_eq!(decision.cause, Some(run::PolicyCause::Capacity));
        assert_eq!(
            decision.retry.unwrap().cause,
            Some(run::PolicyCause::Capacity)
        );
        Ok(())
    }
}
