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
    };
    expected == routing_outcome(outcome)
        && provenant_routing::FallbackPolicy {
            outcome: expected,
            handoff: evidence.handoff,
            custody_released: evidence.custody_released,
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
        let evidence = PreparationFallbackEvidence {
            failure: PreparationFailure::AuthRequired,
            handoff: provenant_routing::FallbackHandoff::Local,
            custody_released: true,
        };
        assert!(live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            true,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::UsageLimited,
            false,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::TaskFailed,
            false,
            Some(evidence)
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            None
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            Some(PreparationFallbackEvidence {
                custody_released: false,
                ..evidence
            })
        ));
        assert!(!live_fallback_allowed(
            AttemptOutcome::AuthRequired,
            false,
            Some(PreparationFallbackEvidence {
                handoff: provenant_routing::FallbackHandoff::UncertainOrLaunched,
                ..evidence
            })
        ));
    }
}
