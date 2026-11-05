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

pub(super) fn live_fallback_allowed(
    outcome: AttemptOutcome,
    launched: bool,
    access: AccessMode,
    evidence: Option<provenant_records::run::PreparationFailure>,
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
            AccessMode::WorktreeWrite,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            true,
            AccessMode::WorktreeWrite,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::UsageLimited,
            false,
            AccessMode::WorktreeWrite,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::TaskFailed,
            false,
            AccessMode::WorktreeWrite,
            Some(PreparationFailure::AuthRequired)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            AccessMode::WorktreeWrite,
            None
        ));
    }
}
