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
    /// The host settled a writer against its attempt-level unchanged Git head.
    StoppedNoWork,
    OtherFailure,
    ReviewRequired,
    AuthRequired,
    UsageLimited,
    AdapterUnavailable,
    /// A local refusal before provider launch, retaining its safe typed cause.
    PreparationRefused(provenant_records::run::TerminalCause),
    DeadlineExceeded,
}

/// Owner-observed facts used to decide whether another route is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparationFallbackEvidence {
    pub failure: provenant_records::run::PreparationFailure,
    pub handoff: provenant_routing::FallbackHandoff,
    pub custody_released: bool,
}

#[allow(unused_imports)]
pub(super) use super::next_attempt::{
    fallback_candidate, fallback_denial, live_fallback_allowed, review_member,
    review_preparation_substitution, review_substitution,
};

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
