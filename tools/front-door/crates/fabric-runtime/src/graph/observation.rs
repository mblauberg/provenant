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
    OtherFailure,
    AuthRequired,
    UsageLimited,
    QuotaLimited,
    AdapterUnavailable,
}

impl AttemptOutcome {
    /// Only these three typed terminal results advance to the next route.
    pub(super) fn allows_route_fallback(self, access: AccessMode, launched: bool) -> bool {
        matches!(
            self,
            Self::AuthRequired | Self::UsageLimited | Self::QuotaLimited | Self::AdapterUnavailable
        ) && (!launched || access == AccessMode::ReadOnly)
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
