//! Kernel decision of what runs next: route selection, fallback and launch retry.
use super::*;
use std::error::Error;

/// Kernel next-attempt disposition for a graph node.
#[derive(Debug, PartialEq, Eq)]
pub enum NextAttempt {
    /// Persisted retry wake has not elapsed yet.
    WaitRetryWake,
    /// Launch retry deadline passed without launch acceptance.
    LaunchRetryExhausted { reason: String },
    /// Node should fail with an optional error code and reason.
    Fail {
        code: Option<&'static str>,
        message: Option<String>,
    },
    /// Route candidate selected to observe or run.
    Candidate {
        route_index: usize,
        settlement_retries: Option<u8>,
    },
    /// No further attempt for this node.
    None,
}

/// Classify whether a launch error is transient (eligible for retry) or permanent.
pub fn launch_retry_kind(error: &(dyn Error + 'static)) -> bool {
    use crate::coordination::facade::FacadeError;
    use provenant_core::retry::{RetryCause, is_transient};
    let cause = match error.downcast_ref::<FacadeError>() {
        Some(FacadeError::Busy | FacadeError::WriterLockBusy | FacadeError::WriterUnknown) => {
            RetryCause::Contention
        }
        Some(FacadeError::Timeout) => RetryCause::Timeout,
        Some(
            FacadeError::HomeUnavailable
            | FacadeError::Io { .. }
            | FacadeError::DispatchUnobserved { .. }
            | FacadeError::HostDeliveryStage { .. }
            | FacadeError::HostDeliveryLimit { .. }
            | FacadeError::LaunchUnknown { .. },
        ) => RetryCause::Transport,
        _ => RetryCause::Permanent,
    };
    is_transient(cause)
}

/// Cap retry policy limits and strings to bounds.
pub fn bounded_launch_retry_policy(mut policy: LaunchRetryPolicy) -> LaunchRetryPolicy {
    policy.limit_source = bounded(&policy.limit_source, 32);
    policy
}

/// Project a diagnostic from a launch error.
pub fn launch_diagnostic(error: &(dyn Error + 'static)) -> provenant_records::diagnostic::Diagnostic {
    error
        .downcast_ref::<crate::coordination::facade::FacadeError>()
        .map(crate::coordination::facade::FacadeError::diagnostic)
        .unwrap_or_else(|| provenant_records::diagnostic::Diagnostic::new("launch_failed"))
}

/// Pure transition: callers persist the resulting checkpoint before reporting it.
pub fn apply_launch_disposition(
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    error: &(dyn Error + 'static),
    clock: Option<provenant_core::WallTime>,
    admission_seconds: u64,
    policy: LaunchRetryPolicy,
) -> Result<(), String> {
    let retrying = launch_retry_kind(error);
    if retrying {
        let Some(now) = clock else {
            return Err("wall clock unavailable; cannot record a launch retry wake".into());
        };
        let retry = checkpoint.nodes[index]
            .launch_retry
            .get_or_insert_with(|| LaunchRetry {
                extra: Default::default(),
                policy: bounded_launch_retry_policy(policy),
                failures: 0,
                deadline: None,
                wake_at: None,
                reason: None,
            });
        retry.failures = retry.failures.saturating_add(1);
        let deadline = match retry.deadline {
            Some(deadline) => deadline,
            None => {
                let Some(deadline) = DurationMicros::from_seconds(admission_seconds)
                    .ok()
                    .and_then(|duration| now.checked_add(duration).ok())
                else {
                    return Err(
                        "launch retry deadline unavailable; cannot record a retry wake".into(),
                    );
                };
                retry.deadline = Some(deadline);
                deadline
            }
        };
        let projected = launch_diagnostic(error);
        retry.reason = Some(bounded(
            projected.detail.as_deref().unwrap_or(&projected.code),
            MAX_ADVANCE_ERROR_BYTES,
        ));
        if now < deadline && retry.failures <= retry.policy.limit {
            let delay = provenant_core::retry::Backoff::new(
                std::time::Duration::from_secs(retry.policy.first_seconds),
                2,
                std::time::Duration::from_secs(
                    retry.policy.max_seconds.max(retry.policy.first_seconds),
                ),
            )
            .delay(u64::from(retry.failures) - 1);
            retry.wake_at = Some(
                DurationMicros::from_seconds(delay.as_secs())
                    .ok()
                    .and_then(|duration| now.checked_add(duration).ok())
                    .map_or(deadline, |wake| wake.min(deadline)),
            );
            asks::refresh_wake(checkpoint);
            return Ok(());
        }
    }
    // A permanent refusal, or a retry budget spent: block the node with the
    // reason and a remedy, and keep the refusal visible in graph status.
    let graph_id = checkpoint.graph_id.clone();
    let progress = &mut checkpoint.nodes[index];
    let id = &progress.id;
    let code = if retrying {
        "launch_retry_exhausted"
    } else {
        "launch_refused"
    };
    progress.advance_error_code = Some(code.into());
    let projected = launch_diagnostic(error);
    let detail = projected.detail.as_deref().unwrap_or(&projected.code);
    let action = projected
        .next
        .as_deref()
        .unwrap_or("inspect the node's inputs and state before retrying");
    let recovery = if error
        .downcast_ref::<crate::coordination::facade::FacadeError>()
        .is_some_and(|e| {
            matches!(
                e,
                crate::coordination::facade::FacadeError::LaunchUnknown { .. }
            )
        }) {
        String::new()
    } else if graph_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        format!(
            "; after correcting the cause, cancel with `provenant fabric graph cancel {graph_id}` and rerun with a new graph_id (for a preset, use `provenant run --preset <name> --fresh`)"
        )
    } else {
        "; inspect and cancel the graph before rerunning with a new graph_id (for a preset, use `provenant run --preset <name> --fresh`)".into()
    };
    progress.advance_error = Some(bounded(
        &format!(
            "node {}: the launch failed [{}]: {detail}; {action}{recovery}",
            provenant_records::diagnostic::safe_text(id),
            projected.code
        ),
        MAX_ADVANCE_ERROR_BYTES,
    ));
    progress.state = NodeState::Blocked;
    // Keep the attempt row even when launch handoff is uncertain. A guardian
    // may already exist, and cancellation needs its identity and custody.
    let prior_retry = progress.launch_retry.take();
    if retrying {
        progress.last_launch_retry = prior_retry.map(|mut retry| {
            retry.wake_at = None;
            retry
        });
    }
    asks::refresh_wake(checkpoint);
    Ok(())
}

/// Apply a persisted disposition to a node whose launch errored while its
/// attempt was still starting: a bounded retry with backoff for a transient
/// kind, a blocked node with reason and remedy otherwise and after the
/// admission deadline. Saves the checkpoint either way.
pub fn launch_disposition<R: AttemptOwner, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    error: &R::Error,
    clock: Option<provenant_core::WallTime>,
    admission_seconds: u64,
    policy: LaunchRetryPolicy,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    apply_launch_disposition(checkpoint, index, error, clock, admission_seconds, policy)
        .map_err(GraphError::Refused)?;
    journal.save(checkpoint).map_err(GraphError::Journal)
}

fn routing_outcome(outcome: AttemptOutcome) -> provenant_routing::FallbackOutcome {
    match outcome {
        AttemptOutcome::AuthRequired => provenant_routing::FallbackOutcome::AuthRequired,
        AttemptOutcome::UsageLimited => provenant_routing::FallbackOutcome::UsageLimited,
        AttemptOutcome::AdapterUnavailable => {
            provenant_routing::FallbackOutcome::AdapterUnavailable
        }
        _ => provenant_routing::FallbackOutcome::Other,
    }
}

/// A persisted route may precede a fallback only when it did not launch.
/// Launching the next route additionally requires live preparation evidence.
pub fn fallback_candidate(
    outcome: AttemptOutcome,
    access: AccessMode,
    launched: bool,
) -> bool {
    (!launched || access == AccessMode::ReadOnly)
        && matches!(
            outcome,
            AttemptOutcome::AuthRequired
                | AttemptOutcome::UsageLimited
                | AttemptOutcome::AdapterUnavailable
        )
}

/// A settled review task can be independently re-attempted without changing
/// the panel topology. An unresolved launch is never eligible.
pub(super) fn review_substitution(node: &PlannedNode, attempt: &GraphAttempt) -> bool {
    review_member(node)
        && attempt.state == AttemptState::Terminal
        && attempt.launched
        && attempt.outcome == Some(AttemptOutcome::TaskFailed)
}

pub(super) fn review_member(node: &PlannedNode) -> bool {
    node.access == AccessMode::ReadOnly
        && node.role.as_deref().is_some_and(|role| {
            role == "lens" || role.starts_with("lens.") || role == "review.verify"
        })
}

/// Preparation fallback still needs live evidence before a new route starts.
pub(super) fn review_preparation_substitution(node: &PlannedNode, attempt: &GraphAttempt) -> bool {
    review_member(node)
        && attempt.state == AttemptState::Terminal
        && !attempt.launched
        && attempt
            .outcome
            .is_some_and(|outcome| fallback_candidate(outcome, node.access, false))
}

pub fn live_fallback_allowed(
    outcome: AttemptOutcome,
    launched: bool,
    access: AccessMode,
    evidence: Option<PreparationFallbackEvidence>,
) -> bool {
    use provenant_records::run::PreparationFailure;
    if launched {
        return provenant_routing::FallbackPolicy {
            outcome: routing_outcome(outcome),
            handoff: provenant_routing::FallbackHandoff::Launched,
            custody_released: false,
            read_only: access == AccessMode::ReadOnly,
            budget_remaining: true,
            pinned: false,
        }
        .allows_switch();
    }
    let Some(evidence) = evidence else {
        return false;
    };
    let expected = match evidence.failure {
        PreparationFailure::AuthRequired => provenant_routing::FallbackOutcome::AuthRequired,
        PreparationFailure::UsageLimited => provenant_routing::FallbackOutcome::UsageLimited,
        PreparationFailure::AdapterUnavailable => {
            provenant_routing::FallbackOutcome::AdapterUnavailable
        }
    };
    expected == routing_outcome(outcome)
        && provenant_routing::FallbackPolicy {
            outcome: expected,
            handoff: evidence.handoff,
            custody_released: evidence.custody_released,
            read_only: access == AccessMode::ReadOnly,
            budget_remaining: true,
            pinned: false,
        }
        .allows_switch()
}

pub fn fallback_denial(
    launched: bool,
    access: AccessMode,
    unreadable: bool,
) -> (&'static str, &'static str) {
    if launched && access == AccessMode::WorktreeWrite {
        (
            "fallback_launched_writer",
            "A launched writer may have effects; route fallback is denied.",
        )
    } else if unreadable {
        (
            "fallback_evidence_unreadable",
            "Cannot read preparation fallback evidence.",
        )
    } else {
        (
            "fallback_evidence_unavailable",
            "Cannot verify local preparation and released custody for route fallback.",
        )
    }
}

/// Decide the next attempt disposition for a node during graph advancement.
pub(super) fn decide_next_attempt<R: AttemptOwner>(
    node: &PlannedNode,
    progress: &NodeProgress,
    limits: &crate::config::GraphLimits,
    owner: &mut R,
    launch_clock: Option<provenant_core::WallTime>,
) -> Result<NextAttempt, R::Error> {
    // 1. A persisted launch wait defers this node until its own wake.
    if progress
        .launch_retry
        .as_ref()
        .and_then(|retry| retry.wake_at)
        .is_some_and(|wake| launch_clock.is_some_and(|now| now < wake))
    {
        return Ok(NextAttempt::WaitRetryWake);
    }

    // 2. Launch retry deadline expiry check.
    if let Some(retry) = &progress.launch_retry
        && retry
            .deadline
            .zip(launch_clock)
            .is_some_and(|(end, now)| now >= end)
    {
        let accepted = match progress.attempts.last() {
            Some(attempt) if attempt.attempt_ref.is_some() => true,
            Some(attempt) => owner.launch_accepted(attempt)?,
            None => false,
        };
        if !accepted {
            return Ok(NextAttempt::LaunchRetryExhausted {
                reason: retry.reason.clone().unwrap_or_default(),
            });
        }
    }

    // 3. Determine next route index.
    let route_index = match progress.attempts.last() {
        None => 0,
        Some(last) if last.state != AttemptState::Terminal => {
            usize::from(last.key.route_index)
        }
        Some(last)
            if review_substitution(node, last)
                && progress
                    .attempts
                    .iter()
                    .filter(|attempt| attempt.review_substitution)
                    .count()
                    < limits.review_substitutions =>
        {
            progress.attempts.len()
        }
        Some(last) if review_substitution(node, last) => {
            // A lowered limit can withdraw a retry that was saved
            // but had not yet started when the graph stopped.
            return Ok(NextAttempt::Fail {
                code: None,
                message: None,
            });
        }
        Some(last)
            if last.outcome.is_some_and(|outcome| {
                fallback_candidate(outcome, node.access, last.launched)
            }) =>
        {
            let Some(outcome) = last.outcome else {
                return Ok(NextAttempt::None);
            };
            let (evidence, unreadable) = match last.attempt_ref.as_deref() {
                Some(reference) if !last.launched => {
                    match owner.preparation_fallback_evidence(&last.key, reference) {
                        Ok(evidence) => (evidence, false),
                        Err(_) => (None, true),
                    }
                }
                _ => (None, false),
            };
            if !live_fallback_allowed(outcome, last.launched, node.access, evidence) {
                let (code, message) = fallback_denial(last.launched, node.access, unreadable);
                return Ok(NextAttempt::Fail {
                    code: Some(code),
                    message: Some(message.into()),
                });
            }
            if review_preparation_substitution(node, last)
                && progress
                    .attempts
                    .iter()
                    .filter(|attempt| attempt.review_substitution)
                    .count()
                    >= limits.review_substitutions
            {
                return Ok(NextAttempt::Fail {
                    code: Some("review_substitution_limit"),
                    message: Some("review substitution limit reached".into()),
                });
            }
            progress.attempts.len()
        }
        Some(_) => return Ok(NextAttempt::None),
    };

    // 4. Fallback checks when switching routes after a launched terminal attempt.
    if route_index > 0
        && progress.attempts.last().is_some_and(|attempt| {
            attempt.launched && attempt.state == AttemptState::Terminal
        })
    {
        if node
            .routes
            .get(route_index - 1)
            .and_then(|route| route.account.as_ref())
            .is_some_and(|account| {
                node.routes
                    .get(route_index)
                    .is_some_and(|next| next.account.as_ref() != Some(account))
            })
        {
            return Ok(NextAttempt::Fail {
                code: Some("fallback_account_pinned"),
                message: Some("Route fallback cannot change the pinned account.".into()),
            });
        }

        let allowance = progress
            .extra
            .get("retry")
            .and_then(|value| value["retries"].as_u64());
        let mut spent = 0u64;
        for reference in progress
            .attempts
            .iter()
            .filter_map(|attempt| attempt.attempt_ref.as_deref())
        {
            spent = spent.saturating_add(owner.retry_successors(reference)?);
        }
        let launched_switches = progress
            .attempts
            .iter()
            .filter(|attempt| attempt.launched && attempt.state == AttemptState::Terminal)
            .count() as u64;
        spent = spent.saturating_add(launched_switches);
        if allowance.is_some_and(|limit| spent > limit) {
            return Ok(NextAttempt::Fail {
                code: Some("fallback_budget_exhausted"),
                message: Some("Route fallback retry budget exhausted.".into()),
            });
        }
        let settlement_retries = allowance.map(|limit| limit.saturating_sub(spent) as u8);
        return Ok(NextAttempt::Candidate {
            route_index,
            settlement_retries,
        });
    }

    let allowance = progress
        .extra
        .get("retry")
        .and_then(|value| value["retries"].as_u64());
    let mut spent = 0u64;
    for reference in progress
        .attempts
        .iter()
        .filter_map(|attempt| attempt.attempt_ref.as_deref())
    {
        spent = spent.saturating_add(owner.retry_successors(reference)?);
    }
    let settlement_retries = allowance.map(|limit| limit.saturating_sub(spent) as u8);

    Ok(NextAttempt::Candidate {
        route_index,
        settlement_retries,
    })
}

/// Settlement fallback decision outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum SettlementFallback {
    Continue,
    Fail {
        code: Option<String>,
        message: Option<String>,
    },
}

/// Settle an attempt outcome, evaluating whether fallback should fire or node should fail.
pub(super) fn settlement_fallback_decision(
    node: &PlannedNode,
    route_index: usize,
    outcome: AttemptOutcome,
    launched: bool,
    fallback_evidence: Option<PreparationFallbackEvidence>,
    evidence_unreadable: bool,
    review_limit_reached: bool,
) -> SettlementFallback {
    let route = node.routes.get(route_index);

    if !live_fallback_allowed(outcome, launched, node.access, fallback_evidence)
        || route_index + 1 == node.routes.len()
        || review_limit_reached
    {
        let mut advance_error_code = None;
        let mut advance_error = None;

        if outcome == AttemptOutcome::UsageLimited {
            advance_error_code = Some("usage_limited".into());
            let pool = route
                .and_then(|r| r.alias.as_deref())
                .map_or_else(String::new, |alias| format!(" for pool {alias}"));
            advance_error = Some(bounded(
                &format!(
                    "Provider quota or rate limit reached{pool}; wait for the allowance to reset or choose an eligible route."
                ),
                MAX_ADVANCE_ERROR_BYTES,
            ));
        }
        if route_index + 1 == node.routes.len()
            && launched
            && node.access == AccessMode::ReadOnly
            && route.is_some_and(|r| r.account.is_some() || r.model.is_some())
        {
            advance_error_code = Some("fallback_route_pinned".into());
            advance_error =
                Some("The selected route is pinned and has no fallback.".into());
        }
        if !live_fallback_allowed(outcome, launched, node.access, fallback_evidence)
            && route_index + 1 < node.routes.len()
        {
            let (code, message) = fallback_denial(launched, node.access, evidence_unreadable);
            advance_error_code = Some(code.into());
            advance_error = Some(message.into());
        }
        if review_limit_reached
            && route_index + 1 < node.routes.len()
            && live_fallback_allowed(outcome, launched, node.access, fallback_evidence)
        {
            advance_error_code = Some("review_substitution_limit".into());
            advance_error = Some("review substitution limit reached".into());
        }
        SettlementFallback::Fail {
            code: advance_error_code,
            message: advance_error,
        }
    } else {
        SettlementFallback::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::run::PreparationFailure;
    use provenant_routing::FallbackHandoff;

    #[test]
    fn contradictory_or_wrong_scope_evidence_cannot_switch() {
        let evidence = PreparationFallbackEvidence {
            failure: PreparationFailure::AuthRequired,
            handoff: FallbackHandoff::Local,
            custody_released: true,
        };
        assert!(live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            AccessMode::WorktreeWrite,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            true,
            AccessMode::WorktreeWrite,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::UsageLimited,
            false,
            AccessMode::WorktreeWrite,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::TaskFailed,
            false,
            AccessMode::WorktreeWrite,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            AccessMode::WorktreeWrite,
            None
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            AccessMode::WorktreeWrite,
            Some(PreparationFallbackEvidence {
                custody_released: false,
                ..evidence
            })
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            AccessMode::WorktreeWrite,
            Some(PreparationFallbackEvidence {
                handoff: FallbackHandoff::UncertainOrLaunched,
                ..evidence
            })
        ));
    }

    #[test]
    fn launch_retry_kinds_are_decided_from_the_typed_error_only() {
        use crate::coordination::facade::FacadeError;
        let transient = FacadeError::Busy;
        assert!(launch_retry_kind(&transient), "{transient}");
        let permanent = FacadeError::StateCorrupt;
        assert!(!launch_retry_kind(&permanent), "{permanent}");
    }

    #[test]
    fn attempt_spec_round_trips_and_defaults() {
        let default_spec = AttemptSpec::default();
        assert!(default_spec.is_empty());
        assert_eq!(default_spec.capabilities, Vec::<String>::new());
        assert!(!default_spec.build_heavy);

        let json = serde_json::json!({
            "account": "acct-1",
            "build_heavy": true,
            "capability": ["postgres"],
            "write_root": ["/tmp/write"]
        });
        let spec: AttemptSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.account.as_deref(), Some("acct-1"));
        assert!(spec.build_heavy);
        assert_eq!(spec.capabilities, vec!["postgres".to_string()]);
        assert_eq!(spec.write_roots, vec!["/tmp/write".to_string()]);
    }
}
