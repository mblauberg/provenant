//! Kernel decision of what runs next: route selection, fallback and launch retry.
use super::*;
use std::error::Error;

/// Kernel next-attempt disposition for a graph node.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NextAttempt {
    /// Persisted retry wake has not elapsed yet.
    WaitRetryWake,
    /// Persist a later read of the same attempt's local custody evidence.
    WaitFallbackEvidence(FallbackEvidenceRetry),
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

const FALLBACK_EVIDENCE_WINDOW_MICROS: u64 = 1_000_000;
const FALLBACK_EVIDENCE_RETRY_MICROS: u64 = 100_000;

pub(super) fn initial_fallback_evidence_retry(
    key: &AttemptKey,
    attempt_ref: &str,
    now: provenant_core::WallTime,
) -> Option<FallbackEvidenceRetry> {
    let deadline = now
        .checked_add(provenant_core::DurationMicros::from_micros(
            FALLBACK_EVIDENCE_WINDOW_MICROS,
        ))
        .ok()?;
    let wake_at = now
        .checked_add(provenant_core::DurationMicros::from_micros(
            FALLBACK_EVIDENCE_RETRY_MICROS,
        ))
        .ok()?;
    Some(FallbackEvidenceRetry {
        extra: Default::default(),
        key: key.clone(),
        attempt_ref: attempt_ref.to_owned(),
        deadline,
        wake_at: wake_at.min(deadline),
    })
}

fn next_fallback_evidence_retry(
    retry: &FallbackEvidenceRetry,
    now: provenant_core::WallTime,
) -> Option<FallbackEvidenceRetry> {
    if now >= retry.deadline {
        return None;
    }
    let wake_at = now
        .checked_add(provenant_core::DurationMicros::from_micros(
            FALLBACK_EVIDENCE_RETRY_MICROS,
        ))
        .ok()?
        .min(retry.deadline);
    Some(FallbackEvidenceRetry {
        wake_at,
        ..retry.clone()
    })
}

pub(super) fn custody_release_pending(
    node: &PlannedNode,
    route_index: usize,
    outcome: GraphAttemptOutcome,
    stop: Option<provenant_records::StopCause>,
    launched: bool,
    mut facts: FallbackEvidence<'_>,
) -> bool {
    if launched || route_index + 1 >= node.routes.len() || facts.review_limit_reached {
        return false;
    }
    let candidate = match facts.preparation {
        Some(evidence)
            if evidence.handoff == provenant_routing::FallbackHandoff::Local
                && !evidence.custody_released
                && (expected_preparation_failure(outcome) == Some(evidence.failure)
                    || (matches!(outcome, GraphAttemptOutcome::PreparationRefused(_))
                        && evidence.failure
                            == provenant_records::run::PreparationFailure::AdapterUnavailable)) =>
        {
            Some(evidence)
        }
        Some(_) => return false,
        None => expected_preparation_failure(outcome).map(|failure| PreparationFallbackEvidence {
            failure,
            handoff: provenant_routing::FallbackHandoff::Local,
            custody_released: true,
        }),
    };
    let Some(mut evidence) = candidate else {
        return false;
    };
    evidence.custody_released = true;
    facts.preparation = Some(evidence);
    facts.unreadable = false;
    matches!(
        next_step::settlement_fallback_decision(node, route_index, outcome, stop, launched, facts),
        SettlementFallback::Continue
    )
}

fn expected_preparation_failure(
    outcome: GraphAttemptOutcome,
) -> Option<provenant_records::run::PreparationFailure> {
    use provenant_records::run::PreparationFailure;
    match outcome {
        GraphAttemptOutcome::AuthRequired => Some(PreparationFailure::AuthRequired),
        GraphAttemptOutcome::UsageLimited => Some(PreparationFailure::UsageLimited),
        GraphAttemptOutcome::AdapterUnavailable
        | GraphAttemptOutcome::PreparationRefused(
            provenant_records::run::CauseKind::SpawnFailure,
        ) => Some(PreparationFailure::AdapterUnavailable),
        _ => None,
    }
}

/// Stable owner codes that make a launch wait out writer contention instead
/// of settling. Shared with checkpoint validation so the two cannot drift.
pub(crate) fn launch_contention_code(code: &str) -> bool {
    matches!(
        code,
        "busy" | "writer_lock_busy" | "writer_unknown" | "writer_busy" | "writer_lease_held"
    )
}

/// Writer-custody codes that prove the launch had not reached the host when it
/// failed. Generic `busy` is excluded: it also covers post-acceptance
/// filesystem lock timeouts, where a live guardian may already exist. A held
/// lease is included: a just-stopped worktree still publishing its terminal
/// record is a release in progress, not a foreign lane to refuse.
pub(crate) fn pre_host_contention(code: &str) -> bool {
    matches!(
        code,
        "writer_lock_busy" | "writer_unknown" | "writer_busy" | "writer_lease_held"
    )
}

/// Typed counterpart of `pre_host_contention`. Called at disposition time to
/// record the fact on the attempt instead of inferring it from stored codes.
pub(crate) fn launch_pre_host(error: &(dyn Error + 'static)) -> bool {
    use crate::coordination::facade::FacadeError;
    error
        .downcast_ref::<FacadeError>()
        .is_some_and(|error| pre_host_contention(error.static_code()))
}

/// The recorded fact that no non-terminal attempt crossed acceptance. The
/// blocked diagnostic and the retry gate both read it; neither infers it from
/// owner code strings. An empty attempt set is vacuously safe: writer-lock
/// waits before the first attempt never reached the host either.
pub(crate) fn all_attempts_refused_before_acceptance(progress: &NodeProgress) -> bool {
    progress
        .attempts
        .iter()
        .filter(|attempt| attempt.state != AttemptState::Terminal)
        .all(|attempt| attempt.host_refused_before_acceptance)
}

/// Contention waits for this lane's launch deadline, independently of the
/// retry-count policy used for transport failures.
pub(crate) fn launch_contention(error: &(dyn Error + 'static)) -> bool {
    use crate::coordination::facade::FacadeError;
    match error.downcast_ref::<FacadeError>() {
        Some(FacadeError::GitMutationFailed {
            lock_failure: true, ..
        }) => true,
        // A live holder is another lane's work, not a release in progress;
        // waiting it out would only delay the boundary-2 refusal.
        Some(FacadeError::WriterLeaseHeld {
            live: Some(true), ..
        }) => false,
        // static_code, not code: a dynamic relay refusal code must not be
        // mistaken for a typed writer-custody cause.
        Some(error) => launch_contention_code(error.static_code()),
        None => false,
    }
}

/// Classify whether a launch error is transient (eligible for retry) or permanent.
pub(crate) fn launch_retry_kind(error: &(dyn Error + 'static)) -> bool {
    use crate::coordination::facade::FacadeError;
    use provenant_core::retry::{RetryCause, is_transient};
    if launch_contention(error) {
        return true;
    }
    let cause = match error.downcast_ref::<FacadeError>() {
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
pub(crate) fn bounded_launch_retry_policy(mut policy: LaunchRetryPolicy) -> LaunchRetryPolicy {
    policy.limit_source = bounded(&policy.limit_source, 32);
    policy
}

/// Project a diagnostic from a launch error.
pub(crate) fn launch_diagnostic(
    error: &(dyn Error + 'static),
) -> provenant_records::diagnostic::Diagnostic {
    error
        .downcast_ref::<crate::coordination::facade::FacadeError>()
        .map(crate::coordination::facade::FacadeError::diagnostic)
        .unwrap_or_else(|| provenant_records::diagnostic::Diagnostic::new("launch_failed"))
}

pub(crate) fn writer_binding_reason(id: &str, error: &(dyn Error + 'static)) -> String {
    let diagnostic = launch_diagnostic(error);
    bounded(
        &format!(
            "node {}: cannot bind writer [{}]: {}; {}",
            provenant_records::diagnostic::safe_text(id),
            diagnostic.code,
            diagnostic
                .next
                .as_deref()
                .unwrap_or("inspect writer custody before retrying"),
            diagnostic.detail.as_deref().unwrap_or(&diagnostic.code)
        ),
        MAX_ADVANCE_NOTE_BYTES,
    )
}

/// Pure transition: callers persist the resulting checkpoint before reporting it.
pub(crate) fn apply_launch_disposition(
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    error: &(dyn Error + 'static),
    clock: Option<provenant_core::WallTime>,
    admission_seconds: u64,
    policy: LaunchRetryPolicy,
) -> Result<(), String> {
    let retrying = launch_retry_kind(error);
    let contention = launch_contention(error);
    if retrying {
        let Some(now) = clock else {
            return Err("wall clock unavailable; cannot record a launch retry wake".into());
        };
        let projected = launch_diagnostic(error);
        checkpoint.nodes[index].advance_owner_code = Some(projected.code.clone());
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
        retry.reason = Some(bounded(
            &format!(
                "[{}] {}; {}",
                projected.code,
                projected
                    .next
                    .as_deref()
                    .unwrap_or("inspect the current state before taking further action"),
                projected.detail.as_deref().unwrap_or(&projected.code)
            ),
            MAX_ADVANCE_NOTE_BYTES,
        ));
        if now < deadline && (contention || retry.failures <= retry.policy.limit) {
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
    progress.advance_code = Some(code.into());
    let projected = launch_diagnostic(error);
    progress.advance_owner_code = Some(projected.code.clone());
    let detail = projected.detail.as_deref().unwrap_or(&projected.code);
    let action = projected
        .next
        .as_deref()
        .unwrap_or("inspect the node's inputs and state before retrying");
    // A blocked node whose attempts are all proved never to have crossed
    // acceptance is safe to retry, and status must say so. This reads the same
    // recorded fact the retry gate reads; it is not re-derived from codes.
    let recovery = if error
        .downcast_ref::<crate::coordination::facade::FacadeError>()
        .is_some_and(|e| {
            matches!(
                e,
                crate::coordination::facade::FacadeError::LaunchUnknown { .. }
            )
        })
        || !all_attempts_refused_before_acceptance(progress)
    {
        String::new()
    } else if [graph_id.as_str(), id.as_str()].iter().all(|value| {
        value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    }) {
        format!("; after correcting the cause, retry with `provenant retry {graph_id}/{id}`")
    } else {
        "; inspect the graph, then use `provenant retry` with its graph and node selectors".into()
    };
    progress.advance_note = Some(bounded(
        &format!(
            "node {}: the launch failed [{}]: {action}; {detail}{recovery}",
            provenant_records::diagnostic::safe_text(id),
            projected.code
        ),
        MAX_ADVANCE_NOTE_BYTES,
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
pub(crate) struct LaunchTiming {
    pub clock: Option<provenant_core::WallTime>,
    pub admission_seconds: u64,
    pub policy: LaunchRetryPolicy,
}

pub(crate) fn launch_disposition<R: AttemptOwner, J: GraphJournal>(
    owner: &mut R,
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    error: &R::Error,
    timing: LaunchTiming,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    // Writer-custody contention is itself proof the host never accepted the
    // launch: the failure happened while binding or provisioning the writer.
    // Record the fact from the typed error now; do not infer it later from
    // stored owner codes, which also cover post-acceptance lock timeouts.
    let proved_by_owner = !launch_retry_kind(error)
        && checkpoint.nodes[index]
            .attempts
            .last()
            .is_some_and(|attempt| {
                matches!(owner.launch_refused_before_acceptance(attempt), Ok(true))
            });
    let proof = launch_pre_host(error) || proved_by_owner;
    if let Some(attempt) = checkpoint.nodes[index].attempts.last_mut() {
        attempt.host_refused_before_acceptance = proof;
    }
    apply_launch_disposition(
        checkpoint,
        index,
        error,
        timing.clock,
        timing.admission_seconds,
        timing.policy,
    )
    .map_err(GraphError::Refused)?;
    journal.save(checkpoint).map_err(GraphError::Journal)
}

fn routing_outcome(outcome: GraphAttemptOutcome) -> provenant_routing::FallbackOutcome {
    match outcome {
        GraphAttemptOutcome::AuthRequired => provenant_routing::FallbackOutcome::AuthRequired,
        GraphAttemptOutcome::UsageLimited => provenant_routing::FallbackOutcome::UsageLimited,
        GraphAttemptOutcome::ModelUnavailable => {
            provenant_routing::FallbackOutcome::ModelUnavailable
        }
        // Keep the pre-launch refusal's existing route policy. Its graph
        // outcome carries the real cause; this is only fallback eligibility.
        // A local preparation failure needs a local repair, not another route.
        GraphAttemptOutcome::PreparationRefused(
            provenant_records::run::CauseKind::PreparationRefused,
        ) => provenant_routing::FallbackOutcome::Other,
        GraphAttemptOutcome::AdapterUnavailable | GraphAttemptOutcome::PreparationRefused(_) => {
            provenant_routing::FallbackOutcome::AdapterUnavailable
        }
        _ => provenant_routing::FallbackOutcome::Other,
    }
}

/// A persisted route may precede a fallback only when it did not launch.
/// Launching the next route additionally requires live preparation evidence.
pub(crate) fn fallback_candidate(
    outcome: GraphAttemptOutcome,
    _access: AccessMode,
    launched: bool,
) -> bool {
    (launched && matches!(outcome, GraphAttemptOutcome::ProviderStalledRetry))
        || (!launched
            && fallback_cause(outcome)
                .is_some_and(|cause| cause.default_next() == provenant_records::NextStep::NewRoute))
}

/// The stop class that decides a route fallback. A host refusal before launch
/// is a route that cannot serve the node, whatever refused it.
pub(super) fn fallback_cause(outcome: GraphAttemptOutcome) -> Option<provenant_records::StopCause> {
    use provenant_records::StopCause;
    match outcome {
        GraphAttemptOutcome::AuthRequired => Some(StopCause::AuthRequired),
        GraphAttemptOutcome::UsageLimited => Some(StopCause::UsageLimited),
        GraphAttemptOutcome::ModelUnavailable => Some(StopCause::ModelUnavailable),
        GraphAttemptOutcome::PreparationRefused(
            provenant_records::run::CauseKind::SpawnFailure,
        ) => Some(StopCause::SpawnFailure),
        GraphAttemptOutcome::PreparationRefused(
            provenant_records::run::CauseKind::PreparationRefused,
        ) => Some(StopCause::PreparationRefused),
        GraphAttemptOutcome::AdapterUnavailable | GraphAttemptOutcome::PreparationRefused(_) => {
            Some(StopCause::AdapterUnavailable)
        }
        GraphAttemptOutcome::Unclassified => Some(StopCause::Unclassified),
        _ => None,
    }
}

/// A settled review task can be independently re-attempted without changing
/// the panel topology. An unresolved launch is never eligible.
pub(super) fn review_substitution(node: &PlannedNode, attempt: &GraphAttempt) -> bool {
    review_member(node)
        && attempt.state == AttemptState::Terminal
        && attempt.launched
        && attempt.outcome == Some(GraphAttemptOutcome::TaskFailed)
}

pub(super) fn review_member(node: &PlannedNode) -> bool {
    node.access == AccessMode::ReadOnly
        && (matches!(
            template::review_behaviour(node),
            Some(template::ReviewBehaviour::Lens | template::ReviewBehaviour::Verify)
        ) || node.role.as_deref().is_some_and(|role| {
            role == "lens" || role.starts_with("lens.") || role == "review.verify"
        }))
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

pub(crate) fn live_fallback_allowed(
    outcome: GraphAttemptOutcome,
    launched: bool,
    evidence: Option<PreparationFallbackEvidence>,
) -> bool {
    use provenant_records::run::PreparationFailure;
    if launched {
        return false;
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
        // A local Git setup refusal needs a declaration or an upstream, which
        // another route cannot supply. Leave it with the chair.
        PreparationFailure::GitSetup => provenant_routing::FallbackOutcome::Other,
    };
    expected == routing_outcome(outcome)
        && provenant_routing::FallbackPolicy {
            outcome: expected,
            handoff: evidence.handoff,
            custody_released: evidence.custody_released,
            budget_remaining: true,
            pinned: false,
        }
        .allows_preparation_switch()
}

pub(crate) fn fallback_denial(
    launched: bool,
    access: AccessMode,
    unreadable: bool,
    outcome: GraphAttemptOutcome,
    cause: Option<&provenant_records::RouteExclusionReason>,
) -> (&'static str, String) {
    let (code, denial) = if launched && access == AccessMode::WorktreeWrite {
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
    };
    let reason = match outcome {
        GraphAttemptOutcome::AuthRequired => "The lane requires authentication.",
        GraphAttemptOutcome::UsageLimited => "The lane reached a usage limit.",
        GraphAttemptOutcome::ModelUnavailable => {
            "The model is unavailable for this account; another candidate may take over."
        }
        GraphAttemptOutcome::AdapterUnavailable => "The lane's adapter is unavailable.",
        GraphAttemptOutcome::PreparationRefused(cause) => cause.detail(),
        GraphAttemptOutcome::Unclassified => "The provider reported an unclassified error.",
        _ => "The lane failed.",
    };
    let detail = cause.map(provenant_routing::exclusion_detail);
    (
        code,
        match detail {
            Some(detail) => format!("{reason} {detail}. {denial}"),
            None => format!("{reason} {denial}"),
        },
    )
}

/// The sealed allowance bounds all launched attempts across automatic redos.
pub(super) fn quota_redraw_budget(progress: &NodeProgress) -> bool {
    progress.retry.as_ref().is_some_and(|allowance| {
        let archived = progress.quota_redraw_spent;
        let current = progress
            .attempts
            .iter()
            .filter(|attempt| attempt.launched)
            .count() as u64
            + u64::from(
                progress
                    .attempts
                    .last()
                    .is_some_and(|attempt| !attempt.launched),
            );
        archived.saturating_add(current) <= u64::from(allowance.retries)
    })
}

/// Facts are requested lazily: waiting nodes never probe a host or consume a
/// refusal, and a deadline check precedes fallback evidence exactly as on replay.
#[derive(Default)]
pub(super) struct NextAttemptFacts {
    pub launch_accepted: Option<bool>,
    pub fallback: Option<PreparationObservation>,
}

pub(super) struct PreparationObservation {
    pub evidence: Option<PreparationFallbackEvidence>,
    pub unreadable: bool,
    pub cause: Option<provenant_records::RouteExclusionReason>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum NextAttemptDecision {
    ObserveLaunchAcceptance,
    ObservePreparationFallback,
    Ready(NextAttempt),
}

/// Pure next-attempt transition. The scheduler fulfils any observation request
/// then reapplies this function; errors from those reads never mutate progress.
pub(super) fn next_attempt_transition(
    node: &PlannedNode,
    progress: &NodeProgress,
    limits: &crate::config::GraphLimits,
    launch_clock: Option<provenant_core::WallTime>,
    prep_retries: Option<usize>,
    facts: &NextAttemptFacts,
) -> NextAttemptDecision {
    if let Some(retry) = &progress.fallback_evidence_retry {
        let same_attempt = progress.attempts.last().is_some_and(|attempt| {
            attempt.key == retry.key && attempt.attempt_ref.as_deref() == Some(&retry.attempt_ref)
        });
        if !same_attempt {
            return NextAttemptDecision::Ready(NextAttempt::Fail {
                code: Some("fallback_evidence_unavailable"),
                message: Some(
                    "Cannot verify local preparation and released custody for route fallback."
                        .into(),
                ),
            });
        }
        let Some(now) = launch_clock else {
            return NextAttemptDecision::Ready(NextAttempt::Fail {
                code: Some("fallback_evidence_unavailable"),
                message: Some(
                    "Cannot verify local preparation and released custody for route fallback."
                        .into(),
                ),
            });
        };
        if retry.wake_at > retry.deadline
            || retry
                .deadline
                .as_micros()
                .saturating_sub(retry.wake_at.as_micros())
                > FALLBACK_EVIDENCE_WINDOW_MICROS as i64
            || retry.deadline.as_micros().saturating_sub(now.as_micros())
                > FALLBACK_EVIDENCE_WINDOW_MICROS as i64
        {
            return NextAttemptDecision::Ready(NextAttempt::Fail {
                code: Some("fallback_evidence_unavailable"),
                message: Some(
                    "Cannot verify local preparation and released custody for route fallback."
                        .into(),
                ),
            });
        }
        if now < retry.wake_at {
            return NextAttemptDecision::Ready(NextAttempt::WaitRetryWake);
        }
    }

    // 1. A persisted launch wait defers this node until its own wake.
    if progress
        .launch_retry
        .as_ref()
        .and_then(|retry| retry.wake_at)
        .is_some_and(|wake| launch_clock.is_some_and(|now| now < wake))
    {
        return NextAttemptDecision::Ready(NextAttempt::WaitRetryWake);
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
            Some(_) => match facts.launch_accepted {
                Some(accepted) => accepted,
                None => return NextAttemptDecision::ObserveLaunchAcceptance,
            },
            None => false,
        };
        if !accepted {
            return NextAttemptDecision::Ready(NextAttempt::LaunchRetryExhausted {
                reason: retry.reason.clone().unwrap_or_else(|| {
                    "inspect the current state before taking further action".into()
                }),
            });
        }
    }

    // 3. Determine next route index.
    let route_index = match progress.attempts.last() {
        None => 0,
        Some(last) if last.state != AttemptState::Terminal => usize::from(last.key.route_index),
        Some(last)
            if prep_retries.is_some()
                && last.outcome.is_some_and(|outcome| {
                    matches!(outcome, GraphAttemptOutcome::PreparationRefused(_))
                })
                && !last.launched
                && last.host_refused_before_acceptance =>
        {
            let proved_refusals = progress
                .attempts
                .iter()
                .filter(|attempt| {
                    attempt.state == AttemptState::Terminal
                        && !attempt.launched
                        && attempt.host_refused_before_acceptance
                        && attempt.outcome.is_some_and(|outcome| {
                            matches!(outcome, GraphAttemptOutcome::PreparationRefused(_))
                        })
                })
                .count();
            if prep_retries.is_some_and(|limit| proved_refusals > limit) {
                return NextAttemptDecision::Ready(NextAttempt::Fail {
                    code: Some("prep_retries_exhausted"),
                    message: Some(
                        "Preparation was refused before launch acceptance after the retry limit."
                            .into(),
                    ),
                });
            }
            // A refusal proven to precede owner acceptance does not consume a
            // route fallback or delivery repair; reconcile and relaunch this
            // same route with a new durable AttemptKey.
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
            usize::from(last.key.route_index).saturating_add(1)
        }
        Some(last) if review_substitution(node, last) => {
            // A lowered limit can withdraw a retry that was saved
            // but had not yet started when the graph stopped.
            return NextAttemptDecision::Ready(NextAttempt::Fail {
                code: None,
                message: None,
            });
        }
        Some(last)
            if last
                .outcome
                .is_some_and(|outcome| fallback_candidate(outcome, node.access, last.launched)) =>
        {
            let Some(outcome) = last.outcome else {
                return NextAttemptDecision::Ready(NextAttempt::None);
            };
            let Some(fallback) = &facts.fallback else {
                return NextAttemptDecision::ObservePreparationFallback;
            };
            match next_step::settlement_fallback_decision(
                node,
                usize::from(last.key.route_index),
                outcome,
                last.stop,
                last.launched,
                FallbackEvidence {
                    preparation: fallback.evidence,
                    unreadable: fallback.unreadable,
                    review_limit_reached: review_preparation_substitution(node, last)
                        && progress
                            .attempts
                            .iter()
                            .filter(|attempt| attempt.review_substitution)
                            .count()
                            >= limits.review_substitutions,
                    cause: fallback.cause.as_ref(),
                    skipped_accounts: &[],
                },
            ) {
                SettlementFallback::Continue => {}
                SettlementFallback::Fail { code, message } => {
                    return NextAttemptDecision::Ready(NextAttempt::Fail { code, message });
                }
            }
            usize::from(last.key.route_index).saturating_add(1)
        }
        Some(_) => return NextAttemptDecision::Ready(NextAttempt::None),
    };

    // 4. Retry accounting. Preparation refusals have not launched a provider
    // and do not consume the settlement retry allowance. Each launched
    // attempt that needs another route consumes one infrastructure retry.
    let switching = route_index > 0
        && progress.attempts.last().is_some_and(|attempt| {
            attempt.state == AttemptState::Terminal
                && (attempt.launched
                    || (attempt.outcome == Some(GraphAttemptOutcome::UsageLimited)
                        && progress.quota_redraw_spent > 0))
        });
    let allowance = progress
        .retry
        .as_ref()
        .map(|allowance| allowance.retries as u64);
    let automatic = progress.quota_redraw_spent;
    let launched_switches = progress
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.state == AttemptState::Terminal
                && (attempt.launched
                    || (automatic > 0
                        && attempt.outcome == Some(GraphAttemptOutcome::UsageLimited)))
        })
        .count() as u64;
    let spent = automatic.saturating_add(launched_switches);
    if switching && allowance.is_some_and(|limit| spent > limit) {
        return NextAttemptDecision::Ready(NextAttempt::Fail {
            code: Some("fallback_budget_exhausted"),
            message: Some("Route fallback retry budget exhausted.".into()),
        });
    }
    NextAttemptDecision::Ready(NextAttempt::Candidate {
        route_index,
        settlement_retries: allowance.map(|limit| limit.saturating_sub(spent) as u8),
    })
}

/// Observe only the facts requested by the pure next-attempt transition.
pub(super) fn decide_next_attempt<R: AttemptOwner>(
    node: &PlannedNode,
    progress: &NodeProgress,
    limits: &crate::config::GraphLimits,
    prep_retries: Option<usize>,
    owner: &mut R,
    launch_clock: Option<provenant_core::WallTime>,
) -> Result<NextAttempt, R::Error> {
    let mut facts = NextAttemptFacts::default();
    loop {
        match next_attempt_transition(node, progress, limits, launch_clock, prep_retries, &facts) {
            NextAttemptDecision::Ready(decision) => return Ok(decision),
            NextAttemptDecision::ObserveLaunchAcceptance => {
                facts.launch_accepted = Some(match progress.attempts.last() {
                    Some(attempt) => owner.launch_accepted(attempt)?,
                    None => false,
                });
            }
            NextAttemptDecision::ObservePreparationFallback => {
                let Some(last) = progress.attempts.last() else {
                    // A missing attempt cannot authorise any further launch.
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
                let denied = last.outcome.is_some_and(|outcome| {
                    !live_fallback_allowed(outcome, last.launched, evidence)
                });
                let cause = denied
                    .then(|| {
                        last.attempt_ref.as_deref().and_then(|reference| {
                            owner
                                .preparation_refusal_cause(&last.key, reference)
                                .ok()
                                .flatten()
                        })
                    })
                    .flatten();
                let review_limit_reached = review_preparation_substitution(node, last)
                    && progress
                        .attempts
                        .iter()
                        .filter(|attempt| attempt.review_substitution)
                        .count()
                        >= limits.review_substitutions;
                if denied
                    && launch_clock.is_some()
                    && custody_release_pending(
                        node,
                        usize::from(last.key.route_index),
                        last.outcome.unwrap_or(GraphAttemptOutcome::Unclassified),
                        last.stop,
                        last.launched,
                        FallbackEvidence {
                            preparation: evidence,
                            unreadable,
                            review_limit_reached,
                            cause: cause.as_ref(),
                            skipped_accounts: &[],
                        },
                    )
                {
                    let Some(now) = launch_clock else {
                        unreachable!("clock checked above")
                    };
                    let retry = progress
                        .fallback_evidence_retry
                        .as_ref()
                        .map(|retry| next_fallback_evidence_retry(retry, now))
                        .unwrap_or_else(|| {
                            last.attempt_ref.as_deref().and_then(|reference| {
                                initial_fallback_evidence_retry(&last.key, reference, now)
                            })
                        });
                    if let Some(retry) = retry {
                        return Ok(NextAttempt::WaitFallbackEvidence(retry));
                    }
                }
                facts.fallback = Some(PreparationObservation {
                    evidence,
                    unreadable,
                    cause,
                });
            }
        }
    }
}

/// Fail a node. A decision without its own code or reason, such as a withdrawn
/// review substitution, keeps the advance error already recorded.
pub(super) fn fail_node(
    progress: &mut NodeProgress,
    code: Option<String>,
    message: Option<String>,
) {
    progress.state = NodeState::Failed;
    if code.is_some() {
        progress.advance_code = code;
        progress.advance_owner_code = None;
    }
    if message.is_some() {
        progress.advance_note = message;
    }
}

#[cfg(test)]
mod tests {
    use super::super::next_step::settlement_fallback_decision;
    use super::*;
    use crate::coordination::facade::FacadeError;
    use provenant_records::run::PreparationFailure;
    use provenant_routing::FallbackHandoff;

    #[test]
    fn writer_binding_io_failure_names_path_and_operation() {
        // Drive the conversion `bind_child_directory` uses: opening a regular
        // file as a directory fails, and `FacadeError::filesystem` must own the
        // operation, known path and source in its one local detail field.
        let root = tempfile::tempdir().expect("create a temporary worktree parent");
        let blocker = root.path().join("not-a-directory");
        std::fs::write(&blocker, b"occupied").expect("plant a regular file as the parent");
        let fs_error = provenant_os::fs::Directory::open_anchor(&blocker)
            .expect_err("opening a regular file as a directory must fail");
        let source_text = match &fs_error {
            provenant_os::fs::FsError::Io { source, .. } => source.to_string(),
            other => other.to_string(),
        };
        let path_text = blocker.to_str().expect("test path is UTF-8");
        let error = FacadeError::filesystem(&fs_error);

        let diagnostic = error.diagnostic();
        let detail = diagnostic
            .detail
            .as_deref()
            .expect("binding io failure has local detail");
        assert!(detail.contains("open anchor"), "{detail}");
        assert!(detail.contains(path_text), "{detail}");
        assert!(detail.contains(&source_text), "{detail}");

        let reason = writer_binding_reason("node-7", &error);
        assert!(reason.contains("cannot bind writer [io]"), "{reason}");
        assert!(reason.contains("open anchor"), "{reason}");
        assert!(reason.contains(path_text), "{reason}");
        assert!(reason.contains(&source_text), "{reason}");

        // The local detail is never published: the path is wire-projected and
        // the composed detail is serde-skipped.
        let wire = serde_json::to_string(&error).expect("serialise facade error");
        assert!(!wire.contains(path_text), "{wire}");
        assert!(!wire.contains(&source_text), "{wire}");
    }

    #[test]
    fn contradictory_or_wrong_scope_evidence_cannot_switch() {
        let evidence = PreparationFallbackEvidence {
            failure: PreparationFailure::AuthRequired,
            handoff: FallbackHandoff::Local,
            custody_released: true,
        };
        assert!(live_fallback_allowed(
            GraphAttemptOutcome::AuthRequired,
            false,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            GraphAttemptOutcome::AuthRequired,
            true,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            GraphAttemptOutcome::UsageLimited,
            false,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            GraphAttemptOutcome::TaskFailed,
            false,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            GraphAttemptOutcome::AuthRequired,
            false,
            None
        ));
        assert!(!live_fallback_allowed(
            GraphAttemptOutcome::AuthRequired,
            false,
            Some(PreparationFallbackEvidence {
                custody_released: false,
                ..evidence
            })
        ));
        assert!(!live_fallback_allowed(
            GraphAttemptOutcome::AuthRequired,
            false,
            Some(PreparationFallbackEvidence {
                handoff: FallbackHandoff::UncertainOrLaunched,
                ..evidence
            })
        ));
    }

    #[test]
    fn fallback_evidence_retry_respects_the_sealed_stop_cause() {
        let node = planned(serde_json::json!({
            "id": "n", "prompt": "p", "access": "worktree_write", "worktree": "/w",
            "routes": [{"adapter": "a", "model": "a/m"}, {"adapter": "b", "model": "b/m"}]
        }));
        let facts = || FallbackEvidence {
            preparation: None,
            unreadable: false,
            review_limit_reached: false,
            cause: None,
            skipped_accounts: &[],
        };

        assert!(custody_release_pending(
            &node,
            0,
            GraphAttemptOutcome::AuthRequired,
            None,
            false,
            facts(),
        ));
        assert!(!custody_release_pending(
            &node,
            0,
            GraphAttemptOutcome::AuthRequired,
            Some(provenant_records::StopCause::PermissionDenied),
            false,
            facts(),
        ));
    }

    fn at(seconds: i64) -> provenant_core::WallTime {
        provenant_core::WallTime::from_micros(1_800_000_000_000_000 + seconds * 1_000_000).unwrap()
    }

    fn retry_of(checkpoint: &GraphCheckpoint) -> &LaunchRetry {
        checkpoint.nodes[0]
            .launch_retry
            .as_ref()
            .expect("a pending launch retry")
    }

    struct Owner;

    impl AttemptOwner for Owner {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }

        fn actor(&self) -> String {
            "actor".into()
        }

        fn observe_or_start(
            &mut self,
            _key: &AttemptKey,
            _node: NodeInput<'_>,
            _route: &GraphRoute,
            _access: AccessMode,
            _worktree: Option<&str>,
            _replay: Replay<'_>,
        ) -> Result<AttemptObservation, Self::Error> {
            unreachable!("the kernel decision never launches")
        }
    }

    fn planned(node: serde_json::Value) -> PlannedNode {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "g", "nodes": [node]
        }))
        .unwrap();
        validate_plan(&document).unwrap().0.nodes.remove(0)
    }

    fn attempt(
        route_index: u16,
        launched: bool,
        state: &str,
        outcome: Option<&str>,
    ) -> serde_json::Value {
        let outcome = outcome.map(|outcome| match outcome {
            "preparation_refused" => serde_json::json!({
                "preparation_refused": "preparation_refused"
            }),
            outcome => serde_json::json!(outcome),
        });
        serde_json::json!({
            "launched": launched,
            "key": {"graph_id": "g", "node_id": "n", "route_index": route_index, "attempt": route_index + 1},
            "route": {"adapter": "a", "model": "a/m"},
            "state": state,
            "attempt_ref": null,
            "outcome": outcome,
            "launch_actor": "actor"
        })
    }

    fn progress(attempts: Vec<serde_json::Value>, extra: serde_json::Value) -> NodeProgress {
        let mut value = serde_json::json!({
            "id": "n", "state": "running", "worktree": null, "attempts": attempts
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    /// A decision without its own reason fails the node but keeps the
    /// advance error already recorded.
    #[test]
    fn reasonless_failures_keep_the_recorded_advance_error() {
        let lens = planned(serde_json::json!({
            "id": "n", "prompt": "p", "access": "read_only", "role": "lens",
            "routes": [{"adapter": "a", "model": "a/m"}, {"adapter": "b", "model": "b/m"}]
        }));
        let limits = crate::config::GraphLimits {
            review_substitutions: 0,
            ..Default::default()
        };
        let mut withdrawn = progress(
            vec![attempt(0, true, "terminal", Some("task_failed"))],
            serde_json::json!({
                "advance_code": "missing_verdict",
                "advance_note": "the review returned no verdict"
            }),
        );
        let decision =
            decide_next_attempt(&lens, &withdrawn, &limits, None, &mut Owner, None).unwrap();
        assert_eq!(
            decision,
            NextAttempt::Fail {
                code: None,
                message: None
            }
        );
        let NextAttempt::Fail { code, message } = decision else {
            unreachable!()
        };
        fail_node(&mut withdrawn, code.map(Into::into), message);
        assert_eq!(withdrawn.state, NodeState::Failed);
        assert_eq!(withdrawn.advance_code.as_deref(), Some("missing_verdict"));
        assert_eq!(
            withdrawn.advance_note.as_deref(),
            Some("the review returned no verdict")
        );

        // The last route retains its cause even without a fallback route.
        let single = planned(serde_json::json!({
            "id": "n", "prompt": "p", "access": "worktree_write", "worktree": "/w",
            "routes": [{"adapter": "a", "model": "a/m"}]
        }));
        let decision = settlement_fallback_decision(
            &single,
            0,
            GraphAttemptOutcome::AuthRequired,
            None,
            false,
            FallbackEvidence {
                preparation: None,
                unreadable: false,
                review_limit_reached: false,
                cause: None,
                skipped_accounts: &[],
            },
        );
        assert_eq!(
            decision,
            SettlementFallback::Fail {
                code: Some("auth_required"),
                message: Some("The lane requires authentication.".into())
            }
        );
        // A quota refusal before launch names every account it passed over
        // with its own reason, instead of claiming the allowance is spent.
        let skipped = [
            provenant_records::SkippedAccount {
                extra: Default::default(),
                account: "agy-work".into(),
                reason: provenant_records::RouteExclusionReason::ProviderRefused,
            },
            provenant_records::SkippedAccount {
                extra: Default::default(),
                account: "agy-second".into(),
                reason: provenant_records::RouteExclusionReason::StaleAvailability,
            },
        ];
        let named = settlement_fallback_decision(
            &single,
            0,
            GraphAttemptOutcome::UsageLimited,
            None,
            false,
            FallbackEvidence {
                preparation: None,
                unreadable: false,
                review_limit_reached: false,
                cause: None,
                skipped_accounts: &skipped,
            },
        );
        let SettlementFallback::Fail {
            code,
            message: Some(message),
        } = &named
        else {
            panic!("expected a named refusal: {named:?}");
        };
        assert_eq!(code.as_deref(), Some("usage_limited"));
        assert!(
            message.contains("agy-work: the provider refused"),
            "{message}"
        );
        assert!(message.contains("agy-second: "), "{message}");
        assert!(
            !message.contains("quota or rate limit reached"),
            "{message}"
        );
        let SettlementFallback::Fail { code, message } = decision else {
            unreachable!()
        };
        let mut settled = progress(
            Vec::new(),
            serde_json::json!({"advance_code": "earlier", "advance_note": "kept"}),
        );
        fail_node(&mut settled, code.map(str::to_owned), message);
        assert_eq!(settled.state, NodeState::Failed);
        assert_eq!(settled.advance_code.as_deref(), Some("auth_required"));
        assert_eq!(
            settled.advance_note.as_deref(),
            Some("The lane requires authentication.")
        );
    }

    #[test]
    fn proven_preparation_refusal_retries_the_same_route_with_a_bounded_allowance() {
        let node = planned(serde_json::json!({
            "id": "n", "prompt": "p", "access": "worktree_write", "worktree": "/w",
            "routes": [{"adapter": "a", "model": "a/m"}, {"adapter": "b", "model": "b/m"}]
        }));
        let limits = crate::config::GraphLimits::default();
        let mut first = attempt(0, false, "terminal", Some("preparation_refused"));
        first["host_refused_before_acceptance"] = serde_json::json!(true);
        let mut second = first.clone();
        second["key"]["attempt"] = serde_json::json!(2);
        let within_budget = progress(vec![first.clone(), second.clone()], serde_json::json!({}));

        assert_eq!(
            next_attempt_transition(
                &node,
                &within_budget,
                &limits,
                None,
                Some(2),
                &NextAttemptFacts::default(),
            ),
            NextAttemptDecision::Ready(NextAttempt::Candidate {
                route_index: 0,
                settlement_retries: None,
            })
        );

        let mut third = first.clone();
        third["key"]["attempt"] = serde_json::json!(3);
        let exhausted = progress(vec![first, second, third], serde_json::json!({}));
        assert_eq!(
            next_attempt_transition(
                &node,
                &exhausted,
                &limits,
                None,
                Some(2),
                &NextAttemptFacts::default(),
            ),
            NextAttemptDecision::Ready(NextAttempt::Fail {
                code: Some("prep_retries_exhausted"),
                message: Some(
                    "Preparation was refused before launch acceptance after the retry limit."
                        .into()
                ),
            })
        );
    }

    #[test]
    fn later_route_index_ignores_same_route_preparation_rows() {
        let node = planned(serde_json::json!({
            "id": "n", "prompt": "p", "access": "worktree_write", "worktree": "/w",
            "routes": [{"adapter": "a", "model": "a/m"}, {"adapter": "b", "model": "b/m"}]
        }));
        let mut first = attempt(0, false, "terminal", Some("preparation_refused"));
        first["host_refused_before_acceptance"] = serde_json::json!(true);
        let mut second = first.clone();
        second["key"]["attempt"] = serde_json::json!(2);
        let mut route_unavailable = attempt(0, false, "terminal", Some("auth_required"));
        route_unavailable["key"]["attempt"] = serde_json::json!(3);
        let progress = progress(
            vec![first, second, route_unavailable],
            serde_json::json!({}),
        );
        let facts = NextAttemptFacts {
            fallback: Some(PreparationObservation {
                evidence: Some(PreparationFallbackEvidence {
                    failure: PreparationFailure::AuthRequired,
                    handoff: FallbackHandoff::Local,
                    custody_released: true,
                }),
                unreadable: false,
                cause: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            next_attempt_transition(
                &node,
                &progress,
                &crate::config::GraphLimits::default(),
                None,
                Some(2),
                &facts
            ),
            NextAttemptDecision::Ready(NextAttempt::Candidate {
                route_index: 1,
                settlement_retries: None,
            })
        );
    }

    #[test]
    fn review_substitution_uses_the_next_declared_route_after_preparation_rows() {
        let node = planned(serde_json::json!({
            "id": "n", "prompt": "p", "access": "read_only", "role": "lens",
            "routes": [
                {"adapter": "a", "model": "a/m"},
                {"adapter": "b", "model": "b/m"},
                {"adapter": "c", "model": "c/m"}
            ]
        }));
        let mut first = attempt(0, false, "terminal", Some("preparation_refused"));
        first["host_refused_before_acceptance"] = serde_json::json!(true);
        let mut second = first.clone();
        second["key"]["attempt"] = serde_json::json!(2);
        let mut failed_review = attempt(0, true, "terminal", Some("task_failed"));
        failed_review["key"]["attempt"] = serde_json::json!(3);
        let progress = progress(vec![first, second, failed_review], serde_json::json!({}));
        assert_eq!(
            next_attempt_transition(
                &node,
                &progress,
                &crate::config::GraphLimits::default(),
                None,
                Some(2),
                &NextAttemptFacts::default(),
            ),
            NextAttemptDecision::Ready(NextAttempt::Candidate {
                route_index: 1,
                settlement_retries: None,
            })
        );
    }

    #[test]
    fn launch_disposition_transition_needs_no_owner_or_journal() {
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"g-test","plan_digest":"digest",
            "nodes":[{"id":"one","state":"running","worktree":null,"attempts":[]}]
        }))
        .unwrap();
        let before = checkpoint.clone();
        let busy = crate::coordination::facade::FacadeError::WriterLockBusy { holder: None };
        assert!(
            apply_launch_disposition(
                &mut checkpoint,
                0,
                &busy,
                None,
                60,
                LaunchRetryPolicy::default()
            )
            .is_err()
        );
        assert_eq!(checkpoint, before);
        let policy = LaunchRetryPolicy {
            limit: 1,
            ..Default::default()
        };
        apply_launch_disposition(&mut checkpoint, 0, &busy, Some(at(0)), 60, policy.clone())
            .unwrap();
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        assert_eq!(retry_of(&checkpoint).failures, 1);
        assert_eq!(retry_of(&checkpoint).wake_at, Some(at(5)));
        apply_launch_disposition(&mut checkpoint, 0, &busy, Some(at(5)), 60, policy).unwrap();
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        assert_eq!(retry_of(&checkpoint).failures, 2);
        assert_eq!(retry_of(&checkpoint).deadline, Some(at(60)));
        apply_launch_disposition(
            &mut checkpoint,
            0,
            &busy,
            Some(at(60)),
            60,
            LaunchRetryPolicy::default(),
        )
        .unwrap();
        assert_eq!(checkpoint.nodes[0].state, NodeState::Blocked);
        assert_eq!(
            checkpoint.nodes[0].advance_code.as_deref(),
            Some("launch_retry_exhausted")
        );
        assert_eq!(
            checkpoint.nodes[0]
                .last_launch_retry
                .as_ref()
                .unwrap()
                .failures,
            3
        );
    }

    #[test]
    fn launch_retry_kinds_are_decided_from_the_typed_error_only() {
        use crate::coordination::facade::FacadeError;
        let operation = provenant_core::OperationId::parse("op").unwrap();
        for transient in [
            FacadeError::Busy,
            FacadeError::Timeout,
            FacadeError::HomeUnavailable,
            FacadeError::WriterUnknown,
            FacadeError::WriterBusy { holder: None },
            FacadeError::WriterLockBusy { holder: None },
            FacadeError::Io {
                delivery_limit: None,
                delivery_stage: None,
                resource: None,
                input_failure: None,
                cause: None,
                operation: "read file".into(),
                path: None,
            },
            FacadeError::LaunchUnknown {
                operation_id: operation,
            },
        ] {
            assert!(launch_retry_kind(&transient), "{transient}");
        }
        for permanent in [
            FacadeError::InvalidInput { detail: None },
            FacadeError::ConfinementRefused { resource: None },
            FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: "why".into(),
            },
            FacadeError::NotFound,
            FacadeError::Cancelled,
            FacadeError::OwnerUnavailable,
            FacadeError::StateCorrupt,
        ] {
            assert!(!launch_retry_kind(&permanent), "{permanent}");
        }
        #[derive(Debug)]
        struct Untyped;
        impl std::fmt::Display for Untyped {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("untyped")
            }
        }
        impl Error for Untyped {}
        assert!(!launch_retry_kind(&Untyped));
    }

    #[test]
    fn pre_host_contention_is_classified_from_the_typed_error() {
        use crate::coordination::facade::FacadeError;
        // Writer custody codes prove the launch never reached the host.
        assert!(pre_host_contention("writer_lock_busy"));
        assert!(pre_host_contention("writer_unknown"));
        assert!(pre_host_contention("writer_busy"));
        assert!(pre_host_contention("writer_lease_held"));
        // Generic busy also covers post-acceptance filesystem lock timeouts.
        assert!(!pre_host_contention("busy"));
        assert!(!pre_host_contention("io"));
        // The wider wait classifier still includes generic busy.
        assert!(launch_contention_code("busy"));
        assert!(launch_contention_code("writer_lock_busy"));
        assert!(launch_contention_code("writer_lease_held"));
        // A caller reusing an operation ID with a different request is a
        // permanent conflict, not a writer release to wait out.
        assert!(!launch_contention_code("operation_conflict"));

        let holder = provenant_core::AttemptId::from_bytes([31; 16]);
        assert!(launch_pre_host(&FacadeError::WriterLockBusy {
            holder: Some(holder)
        }));
        assert!(launch_pre_host(&FacadeError::WriterBusy { holder: None }));
        assert!(launch_pre_host(&FacadeError::WriterUnknown));
        assert!(launch_pre_host(&FacadeError::WriterLeaseHeld {
            holder: None,
            branch: None,
            worktree: None,
            age: None,
            live: None,
        }));
        assert!(!launch_pre_host(&FacadeError::Busy));
        assert!(!launch_pre_host(&FacadeError::LaunchUnknown {
            operation_id: provenant_core::OperationId::parse("op").unwrap()
        }));

        // The gate reads the recorded fact, not an owner-code string.
        let mut node = progress(
            vec![attempt(0, false, "starting", None)],
            serde_json::json!({"advance_code": "launch_retry_exhausted"}),
        );
        assert!(!all_attempts_refused_before_acceptance(&node));
        node.attempts[0].host_refused_before_acceptance = true;
        assert!(all_attempts_refused_before_acceptance(&node));
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
            "capabilities": ["postgres"],
            "write_roots": ["/tmp/write"]
        });
        let spec: AttemptSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.account.as_deref(), Some("acct-1"));
        assert!(spec.build_heavy);
        assert_eq!(spec.capabilities, vec!["postgres".to_string()]);
        assert_eq!(spec.write_roots, vec!["/tmp/write".to_string()]);
    }
}
