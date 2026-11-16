//! Shared classification for provider session-identity evidence.

use provenant_supervisor::machine::StopReason;

/// The terminal identity evidence after applying the confirmed-stop EOF rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderIdentityEvidence {
    /// All observed provider identity evidence was complete and non-conflicting.
    Complete,
    /// Only the final incomplete record was excluded after a confirmed stop.
    IntentionalEofFragment,
    /// Provider identity evidence is lost, malformed or has an unconfirmed EOF tail.
    Incomplete,
    /// A provider identity contradicted the admitted route or session.
    Conflict,
    /// Conflicting identity and incomplete evidence were both observed.
    ConflictAndIncomplete,
}

impl ProviderIdentityEvidence {
    /// Classify existing exit fields without changing or bridging their wire format.
    /// Explicitly incomplete evidence always remains incomplete. An EOF fragment is
    /// tolerated only after a delivered cancellation or steering stop and without
    /// either incomplete or conflicting evidence.
    pub fn classify(
        conflict: bool,
        incomplete: bool,
        eof_fragment: bool,
        stop_reason: Option<StopReason>,
        cancel_delivered_before_exit: Option<bool>,
    ) -> Self {
        let intentional_stop = cancel_delivered_before_exit == Some(true)
            && matches!(
                stop_reason,
                Some(StopReason::ExplicitCancellation | StopReason::Steered)
            );
        let tolerated_eof_fragment = eof_fragment && intentional_stop && !conflict && !incomplete;
        let incomplete = incomplete || (eof_fragment && !tolerated_eof_fragment);

        match (conflict, incomplete, tolerated_eof_fragment) {
            (true, true, _) => Self::ConflictAndIncomplete,
            (true, false, _) => Self::Conflict,
            (false, true, _) => Self::Incomplete,
            (false, false, true) => Self::IntentionalEofFragment,
            (false, false, false) => Self::Complete,
        }
    }

    /// Whether a known identity contradiction was observed.
    pub fn has_conflict(self) -> bool {
        matches!(self, Self::Conflict | Self::ConflictAndIncomplete)
    }

    /// Whether evidence was lost or an EOF fragment was not tolerable.
    pub fn is_incomplete(self) -> bool {
        matches!(self, Self::Incomplete | Self::ConflictAndIncomplete)
    }

    /// Only this state permits trimming the final unterminated EOF fragment.
    pub fn tolerates_final_eof_fragment(self) -> bool {
        matches!(self, Self::IntentionalEofFragment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_confirmed_intentional_eof_fragments_are_tolerated() {
        let cases = [
            (
                false,
                false,
                true,
                Some(StopReason::Steered),
                Some(true),
                ProviderIdentityEvidence::IntentionalEofFragment,
            ),
            (
                false,
                false,
                true,
                Some(StopReason::ExplicitCancellation),
                Some(true),
                ProviderIdentityEvidence::IntentionalEofFragment,
            ),
            (
                false,
                false,
                true,
                Some(StopReason::Steered),
                Some(false),
                ProviderIdentityEvidence::Incomplete,
            ),
            (
                false,
                true,
                true,
                Some(StopReason::Steered),
                Some(true),
                ProviderIdentityEvidence::Incomplete,
            ),
            (
                true,
                false,
                true,
                Some(StopReason::Steered),
                Some(true),
                ProviderIdentityEvidence::ConflictAndIncomplete,
            ),
            (
                false,
                false,
                false,
                None,
                None,
                ProviderIdentityEvidence::Complete,
            ),
        ];

        for (conflict, incomplete, eof_fragment, reason, delivered, expected) in cases {
            assert_eq!(
                ProviderIdentityEvidence::classify(
                    conflict,
                    incomplete,
                    eof_fragment,
                    reason,
                    delivered,
                ),
                expected
            );
        }
    }
}
