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
    pub read_only: bool,
    pub budget_remaining: bool,
    pub pinned: bool,
}

impl FallbackPolicy {
    pub fn allows_switch(self) -> bool {
        matches!(
            self.outcome,
            FallbackOutcome::AuthRequired
                | FallbackOutcome::UsageLimited
                | FallbackOutcome::ModelUnavailable
                | FallbackOutcome::AdapterUnavailable
        ) && ((self.handoff == FallbackHandoff::Local && self.custody_released)
            || (self.handoff == FallbackHandoff::Launched
                && self.read_only
                && matches!(
                    self.outcome,
                    FallbackOutcome::AuthRequired | FallbackOutcome::UsageLimited
                )))
            && self.budget_remaining
            && !self.pinned
    }

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
    fn switching_requires_safe_outcome_and_every_guard() {
        let base = FallbackPolicy {
            outcome: FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::LoginExpired,
            ),
            handoff: FallbackHandoff::Local,
            custody_released: true,
            read_only: false,
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
        assert_eq!(
            FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::UnverifiedModel,
            ),
            FallbackOutcome::ModelUnavailable
        );
        for reason in [
            provenant_records::RouteExclusionReason::UnknownAvailability,
            provenant_records::RouteExclusionReason::StaleAvailability,
            provenant_records::RouteExclusionReason::UnverifiedModel,
        ] {
            let outcome = FallbackOutcome::from_exclusion(&reason);
            assert!(
                FallbackPolicy { outcome, ..base }.allows_switch(),
                "{reason:?}"
            );
        }
        let denied = FallbackPolicy {
            outcome: FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::DisclosureDenied,
            ),
            ..base
        };
        assert!(!denied.allows_switch());
        // Preparation exclusions never ran, so even an untyped one advances
        // locally; pins, budget and uncertain handoff still stop it.
        for outcome in [FallbackOutcome::Other, denied.outcome] {
            let local = FallbackPolicy { outcome, ..base };
            assert!(local.allows_preparation_switch());
            for refused in [
                FallbackPolicy {
                    pinned: true,
                    ..local
                },
                FallbackPolicy {
                    budget_remaining: false,
                    ..local
                },
                FallbackPolicy {
                    custody_released: false,
                    ..local
                },
                FallbackPolicy {
                    handoff: FallbackHandoff::UncertainOrLaunched,
                    ..local
                },
                FallbackPolicy {
                    handoff: FallbackHandoff::Launched,
                    read_only: true,
                    ..local
                },
            ] {
                assert!(!refused.allows_preparation_switch(), "{refused:?}");
            }
        }
        for outcome in [
            FallbackOutcome::AuthRequired,
            FallbackOutcome::UsageLimited,
            FallbackOutcome::ModelUnavailable,
            FallbackOutcome::AdapterUnavailable,
            FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::ProviderUpdating,
            ),
        ] {
            assert!(FallbackPolicy { outcome, ..base }.allows_switch());
        }
        for outcome in [FallbackOutcome::AuthRequired, FallbackOutcome::UsageLimited] {
            let launched_reader = FallbackPolicy {
                outcome,
                handoff: FallbackHandoff::Launched,
                read_only: true,
                custody_released: false,
                ..base
            };
            assert!(launched_reader.allows_switch());
            assert!(
                !FallbackPolicy {
                    read_only: false,
                    ..launched_reader
                }
                .allows_switch()
            );
            assert!(
                !FallbackPolicy {
                    budget_remaining: false,
                    ..launched_reader
                }
                .allows_switch()
            );
            assert!(
                !FallbackPolicy {
                    pinned: true,
                    ..launched_reader
                }
                .allows_switch()
            );
            assert!(
                !FallbackPolicy {
                    handoff: FallbackHandoff::UncertainOrLaunched,
                    ..launched_reader
                }
                .allows_switch()
            );
        }
        for outcome in [
            FallbackOutcome::AdapterUnavailable,
            FallbackOutcome::ModelUnavailable,
        ] {
            assert!(
                !FallbackPolicy {
                    outcome,
                    handoff: FallbackHandoff::Launched,
                    read_only: true,
                    ..base
                }
                .allows_switch()
            );
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
