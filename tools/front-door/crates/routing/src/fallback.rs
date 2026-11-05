//! Pure policy for advancing a route after an unsuccessful attempt.

/// Outcomes that may be considered for another route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackOutcome {
    AuthRequired,
    UsageLimited,
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
            Reason::MissingExecutable => Self::AdapterUnavailable,
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
    pub fn allows_switch(self) -> bool {
        matches!(
            self.outcome,
            FallbackOutcome::AuthRequired
                | FallbackOutcome::UsageLimited
                | FallbackOutcome::AdapterUnavailable
        ) && self.handoff == FallbackHandoff::Local
            && self.custody_released
            && self.budget_remaining
            && !self.pinned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_requires_safe_outcome_and_every_guard() {
        let base = FallbackPolicy {
            outcome: FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::LoginExpired,
            ),
            handoff: FallbackHandoff::Local,
            custody_released: true,
            budget_remaining: true,
            pinned: false,
        };
        assert_eq!(base.outcome, FallbackOutcome::AuthRequired);
        assert_eq!(
            FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::AccountMismatch,
            ),
            FallbackOutcome::AuthRequired
        );
        for outcome in [
            FallbackOutcome::AuthRequired,
            FallbackOutcome::UsageLimited,
            FallbackOutcome::AdapterUnavailable,
        ] {
            assert!(FallbackPolicy { outcome, ..base }.allows_switch());
        }
        for policy in [
            FallbackPolicy {
                outcome: FallbackOutcome::Other,
                ..base
            },
            FallbackPolicy {
                handoff: FallbackHandoff::UncertainOrLaunched,
                ..base
            },
            FallbackPolicy {
                custody_released: false,
                ..base
            },
            FallbackPolicy {
                budget_remaining: false,
                ..base
            },
            FallbackPolicy {
                pinned: true,
                ..base
            },
        ] {
            assert!(!policy.allows_switch());
        }
    }
}
