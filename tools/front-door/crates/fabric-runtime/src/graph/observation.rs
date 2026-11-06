use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Starting,
    Running,
    Unresolved,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    TaskFailed,
    /// A lane reported a declared stop; review roles may accept its result.
    DeclaredStop,
    OtherFailure,
    ReviewRequired,
    AuthRequired,
    UsageLimited,
    AdapterUnavailable,
    DeadlineExceeded,
}

/// Owner-observed facts used to decide whether another route is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparationFallbackEvidence {
    pub failure: provenant_records::run::PreparationFailure,
    pub handoff: provenant_routing::FallbackHandoff,
    pub custody_released: bool,
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
pub(super) fn fallback_candidate(
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

pub(super) fn live_fallback_allowed(
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

pub(super) fn fallback_denial(
    launched: bool,
    access: AccessMode,
    unreadable: bool,
    outcome: AttemptOutcome,
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
        AttemptOutcome::AuthRequired => "The lane requires authentication.",
        AttemptOutcome::UsageLimited => "The lane reached a usage limit.",
        AttemptOutcome::AdapterUnavailable => "The lane's adapter is unavailable.",
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptObservation {
    /// The same idempotency key has an accepted, nonterminal run.
    Pending { attempt_ref: Option<String> },
    /// Evidence is incomplete. Keep the attempt unresolved and observe the same
    /// key again; never consume another route on this result.
    Unknown { attempt_ref: Option<String> },
    Terminal {
        attempt_ref: Option<String>,
        outcome: AttemptOutcome,
        launched: bool,
        result: Option<Box<crate::result_capture::CapturedResult>>,
        result_warning: Option<String>,
        result_path: Option<String>,
        branch: Option<String>,
        commit: Option<String>,
    },
}

#[cfg(test)]
mod fallback_evidence_tests {
    use super::*;
    use provenant_records::run::PreparationFailure;

    #[test]
    fn contradictory_or_wrong_scope_evidence_cannot_switch() {
        let evidence = PreparationFallbackEvidence {
            failure: PreparationFailure::AuthRequired,
            handoff: provenant_routing::FallbackHandoff::Local,
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
                handoff: provenant_routing::FallbackHandoff::UncertainOrLaunched,
                ..evidence
            })
        ));
    }
}
