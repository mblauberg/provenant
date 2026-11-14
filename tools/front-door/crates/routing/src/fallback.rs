//! Pure policy for advancing a route after an unsuccessful attempt.

/// Outcomes that may be considered for another route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackOutcome {
    AuthRequired,
    UsageLimited,
    /// The provider does not serve the model for this account, or the model is
    /// not verified there. Another account or route may.
    ModelUnavailable,
    AdapterUnavailable,
    Other,
}

impl FallbackOutcome {
    /// Classify a pre-launch readiness exclusion without treating login
    /// failures as quota exhaustion.
    pub fn from_exclusion(reason: &provenant_records::RouteExclusionReason) -> Self {
        use provenant_records::RouteExclusionReason as Reason;
        match reason {
            Reason::AuthRequired | Reason::LoginExpired | Reason::AccountMismatch => {
                Self::AuthRequired
            }
            Reason::Exhausted | Reason::FreeTierRefused | Reason::ProviderRefused => {
                Self::UsageLimited
            }
            // The route cannot run here now and nothing has started, so a later
            // route may. One account with stale evidence must not strand the rest.
            Reason::UnverifiedModel => Self::ModelUnavailable,
            Reason::MissingExecutable
            | Reason::ProviderUpdating
            | Reason::Disabled
            | Reason::UnknownAvailability
            | Reason::StaleAvailability
            | Reason::UnknownAccount
            | Reason::UnknownPartialHeadroom
            | Reason::ConfinementUnavailable
            | Reason::UnsupportedEffort => Self::AdapterUnavailable,
            _ => Self::Other,
        }
    }
}

/// Evidence about whether the attempt left local launch preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackHandoff {
    /// The attempt is confirmed to have stayed in local preparation.
    Local,
    /// A process may have been launched, or its handoff is uncertain.
    UncertainOrLaunched,
    /// The adapter confirms that a process launched and reported an outcome.
    Launched,
}

/// Inputs to route switching. Absence of evidence must be represented as false
/// or uncertain, never as proof of local preparation or released custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackPolicy {
    pub outcome: FallbackOutcome,
    pub handoff: FallbackHandoff,
    pub custody_released: bool,
    pub budget_remaining: bool,
    pub pinned: bool,
}

impl FallbackPolicy {
    /// A candidate that local preparation excluded never ran, so any typed
    /// exclusion may advance to the next permitted candidate. Pins and budget
    /// still apply; uncertain or launched handoff never qualifies.
    pub fn allows_preparation_switch(self) -> bool {
        self.handoff == FallbackHandoff::Local
            && self.custody_released
            && self.budget_remaining
            && !self.pinned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_switch_requires_local_custody_budget_and_unpinned_route() {
        let base = FallbackPolicy {
            outcome: FallbackOutcome::AuthRequired,
            handoff: FallbackHandoff::Local,
            custody_released: true,
            budget_remaining: true,
            pinned: false,
        };
        assert!(base.allows_preparation_switch());
        // A local preparation exclusion may advance regardless of its typed
        // outcome; the graph owns whether its evidence permits that decision.
        assert!(
            FallbackPolicy {
                outcome: FallbackOutcome::Other,
                ..base
            }
            .allows_preparation_switch()
        );
        for refused in [
            FallbackPolicy {
                pinned: true,
                ..base
            },
            FallbackPolicy {
                budget_remaining: false,
                ..base
            },
            FallbackPolicy {
                custody_released: false,
                ..base
            },
            FallbackPolicy {
                handoff: FallbackHandoff::UncertainOrLaunched,
                ..base
            },
            FallbackPolicy {
                handoff: FallbackHandoff::Launched,
                ..base
            },
        ] {
            assert!(!refused.allows_preparation_switch(), "{refused:?}");
        }
    }

    #[test]
    fn exclusion_reasons_keep_their_preparation_categories() {
        assert_eq!(
            FallbackOutcome::from_exclusion(&provenant_records::RouteExclusionReason::LoginExpired,),
            FallbackOutcome::AuthRequired
        );
        assert_eq!(
            FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::AccountMismatch,
            ),
            FallbackOutcome::AuthRequired
        );
        assert_eq!(
            FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::UnverifiedModel,
            ),
            FallbackOutcome::ModelUnavailable
        );
        for reason in [
            provenant_records::RouteExclusionReason::UnknownAvailability,
            provenant_records::RouteExclusionReason::StaleAvailability,
        ] {
            assert_eq!(
                FallbackOutcome::from_exclusion(&reason),
                FallbackOutcome::AdapterUnavailable,
                "{reason:?}"
            );
        }
    }
}
