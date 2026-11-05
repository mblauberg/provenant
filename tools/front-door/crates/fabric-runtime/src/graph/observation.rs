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

/// Legacy checkpoint validation accepts the historical rule, never as launch authority.
pub(super) fn historical_fallback_admissible(
    outcome: AttemptOutcome,
    access: AccessMode,
    launched: bool,
) -> bool {
    provenant_routing::historical_fallback_admissible(
        routing_outcome(outcome),
        access == AccessMode::WorktreeWrite,
        launched,
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
            .is_some_and(|outcome| historical_fallback_admissible(outcome, node.access, false))
}

pub(super) fn live_fallback_allowed(
    outcome: AttemptOutcome,
    launched: bool,
    evidence: Option<provenant_records::run::PreparationFailure>,
) -> bool {
    use provenant_records::run::PreparationFailure;
    if launched {
        return false;
    }
    let expected = match evidence {
        Some(PreparationFailure::AuthRequired) => provenant_routing::FallbackOutcome::AuthRequired,
        Some(PreparationFailure::UsageLimited) => provenant_routing::FallbackOutcome::UsageLimited,
        Some(PreparationFailure::AdapterUnavailable) => {
            provenant_routing::FallbackOutcome::AdapterUnavailable
        }
        None => return false,
    };
    expected == routing_outcome(outcome)
        && provenant_routing::FallbackPolicy {
            outcome: expected,
            handoff: provenant_routing::FallbackHandoff::Local,
            custody_released: true,
            budget_remaining: true,
            pinned: false,
        }
        .allows_switch()
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
        assert!(live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            true,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::UsageLimited,
            false,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::TaskFailed,
            false,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            None
        ));
    }
}
