//! The format-2 stop: one typed cause and the writer's handoff, sealed at
//! settlement, and the one table of what follows each cause.
//!
//! [`STOP_TABLE`] owns the default next step of every cause and how many
//! fresh same-route attempts precede it. The override point is the graph
//! node's `on_stop` entry for a cause ([`OnStop`]), applied by
//! [`StopCause::next_step`]; a pinned cause ignores it, and a stop whose
//! custody is not proved released always goes to the chair. Route
//! availability and the continuation budget are applied after this, by the
//! graph runtime's one decision function, and can only turn a step into
//! [`NextStep::Chair`]. `docs/specs/run.md#what-follows-a-stop` mirrors the
//! table and a test keeps the two equal.
use crate::handoff::{HANDOFF_BYTES, Handoff, HandoffStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Why an attempt ended without finishing its brief. A finished attempt seals
/// no stop. Each cause is one fact; facts about the worktree, such as whether
/// it holds a new commit, live beside the stop, not in its cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    // Declared by the writer or read from its final answer.
    /// The handoff says `stopped_early`: work remains and the writer could
    /// not finish it in this attempt.
    StoppedEarly,
    /// The handoff says `blocked`: an owner decision, a missing capability
    /// or an external change prevents progress. The stop carries the
    /// handoff, so its note names what blocks it.
    Blocked,
    /// A writer on a model route ended without a valid final handoff.
    HandoffMissing,
    /// The provider ended with an empty final message.
    EmptyResult,
    /// A writer left edits it did not commit.
    Uncommitted,
    /// The lane asked for input; the ask is an open inbox request.
    InputRequired,

    // The host stopped a running writer.
    /// The writer reached `lanes.context_handoff_tokens`; a fresh attempt
    /// continues from its handoff on the same worktree.
    ContextHandoff,
    /// The writer reached its token budget.
    TokenBudget,
    /// The attempt reached its runtime deadline.
    Deadline,
    /// The owner or the chair cancelled the attempt.
    Cancelled,

    // The provider ended the attempt.
    /// The provider reported a stop other than a finished turn.
    ProviderStop,
    ProcessCrash,
    AuthRequired,
    /// A rate, quota or free-pool limit.
    UsageLimited,
    ModelUnavailable,
    AdapterUnavailable,
    PermissionDenied,
    IdentityUnobserved,
    /// The lane's launch-time access token was revoked or expired mid-run
    /// while the account login stayed usable. A writer continues from its
    /// handoff on a new token; the successor's launch re-probes the account.
    LaneTokenLost,

    // The host refused before the provider started.
    SourceViewRefused,
    RouteRefused,
    AccessPolicyRefused,
    DiskFloor,
    DiskUnobserved,
    PromptUnreadable,
    /// The attempt asked for cloud placement, which was removed; cloud
    /// sessions launch with `provenant cloud launch`.
    CloudPlacementRemoved,
    WriteGrantsRefused,
    WorktreeBusy,
    LaunchHandoffRefused,
    RetainedWriterImport,
    UnsupportedPlatform,
    UnknownPacingAccount,
    UnregisteredAccount,
    CredentialHelperFailed,
    CommandUnavailable,
    HostNativeUnconfigured,
    SpawnFailure,

    // Settlement could not establish the attempt's end.
    SettlementFailed,
    Lost,
    Unsettled,
    Quarantined,
}

/// How the next attempt starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Continuation {
    /// Continue the provider session of the stopped attempt.
    Resume,
    /// Start a new session on the same worktree, briefed with the handoff.
    Fresh,
}

/// What follows a stop. A session never crosses routes, so a new route is
/// always a fresh continuation; the chair is not a continuation at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum NextStep {
    /// Another attempt on the route that stopped.
    SameRoute { continuation: Continuation },
    /// Another attempt on a different route: another declared route, or a
    /// redraw of the alias pool without the routes that stopped.
    NewRoute,
    /// No automatic attempt. The graph pauses for the chair.
    Chair,
}

impl NextStep {
    pub const fn continuation(self) -> Option<Continuation> {
        match self {
            Self::SameRoute { continuation } => Some(continuation),
            Self::NewRoute => Some(Continuation::Fresh),
            Self::Chair => None,
        }
    }
}

/// Facts the table reads beside the cause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StopFacts {
    /// Fresh same-route attempts already taken after stops with this cause.
    pub retries: u8,
    /// No process of the stopped attempt can still write: settlement proved
    /// its custody released. The default, `false`, sends every stop to the
    /// chair.
    pub custody_released: bool,
}

/// A graph node's `on_stop` policy: the step to take for a cause instead of
/// its default. A value is a step object (`{"step": "same_route",
/// "continuation": "fresh"}`) or its name (`"same_route"` is fresh).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OnStop(#[serde(deserialize_with = "on_stop_steps")] BTreeMap<StopCause, NextStep>);

impl OnStop {
    pub fn new(steps: BTreeMap<StopCause, NextStep>) -> Self {
        Self(steps)
    }

    pub fn get(&self, cause: StopCause) -> Option<NextStep> {
        self.0.get(&cause).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (StopCause, NextStep)> + '_ {
        self.0.iter().map(|(cause, step)| (*cause, *step))
    }
}

fn on_stop_steps<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<StopCause, NextStep>, D::Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Named {
        SameRoute,
        NewRoute,
        Chair,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Named(Named),
        Step(NextStep),
    }
    let wire = BTreeMap::<StopCause, Wire>::deserialize(deserializer)?;
    Ok(wire
        .into_iter()
        .map(|(cause, step)| {
            let step = match step {
                Wire::Named(Named::SameRoute) => NextStep::SameRoute {
                    continuation: Continuation::Fresh,
                },
                Wire::Named(Named::NewRoute) => NextStep::NewRoute,
                Wire::Named(Named::Chair) => NextStep::Chair,
                Wire::Step(step) => step,
            };
            (cause, step)
        })
        .collect())
}

/// One row of [`STOP_TABLE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopRule {
    pub cause: StopCause,
    pub default: NextStep,
    /// A pinned cause always takes its default; `on_stop` cannot move it.
    pub pinned: bool,
    /// Fresh attempts on the same route before the default applies. A
    /// transient cause takes one.
    pub retries: u8,
}

const NEW: NextStep = NextStep::NewRoute;
const CHAIR: NextStep = NextStep::Chair;

const fn rule(cause: StopCause, default: NextStep) -> StopRule {
    StopRule {
        cause,
        default,
        pinned: false,
        retries: 0,
    }
}

/// A transient cause: one fresh attempt on the same route, then `default`.
const fn once(cause: StopCause, default: NextStep) -> StopRule {
    StopRule {
        cause,
        default,
        pinned: false,
        retries: 1,
    }
}

const fn pinned(cause: StopCause) -> StopRule {
    StopRule {
        cause,
        default: CHAIR,
        pinned: true,
        retries: 0,
    }
}

/// The one table of what follows a stop: exactly one row per cause.
pub const STOP_TABLE: &[StopRule] = {
    use StopCause::*;
    &[
        rule(StoppedEarly, NEW),
        pinned(Blocked),
        rule(HandoffMissing, CHAIR),
        once(EmptyResult, NEW),
        rule(Uncommitted, CHAIR),
        pinned(InputRequired),
        rule(ContextHandoff, NEW),
        rule(TokenBudget, CHAIR),
        rule(Deadline, CHAIR),
        pinned(Cancelled),
        once(ProviderStop, NEW),
        once(ProcessCrash, NEW),
        rule(AuthRequired, NEW),
        rule(UsageLimited, NEW),
        rule(ModelUnavailable, NEW),
        rule(AdapterUnavailable, NEW),
        rule(PermissionDenied, CHAIR),
        rule(IdentityUnobserved, CHAIR),
        rule(LaneTokenLost, NEW),
        rule(SourceViewRefused, CHAIR),
        rule(RouteRefused, CHAIR),
        rule(AccessPolicyRefused, CHAIR),
        rule(DiskFloor, CHAIR),
        once(DiskUnobserved, NEW),
        rule(PromptUnreadable, CHAIR),
        rule(CloudPlacementRemoved, CHAIR),
        rule(WriteGrantsRefused, CHAIR),
        rule(WorktreeBusy, CHAIR),
        rule(LaunchHandoffRefused, CHAIR),
        rule(RetainedWriterImport, CHAIR),
        rule(UnsupportedPlatform, CHAIR),
        rule(UnknownPacingAccount, CHAIR),
        rule(UnregisteredAccount, CHAIR),
        rule(CredentialHelperFailed, CHAIR),
        rule(CommandUnavailable, CHAIR),
        rule(HostNativeUnconfigured, CHAIR),
        once(SpawnFailure, NEW),
        rule(SettlementFailed, CHAIR),
        rule(Lost, CHAIR),
        rule(Unsettled, CHAIR),
        rule(Quarantined, CHAIR),
    ]
};

impl StopCause {
    /// Every cause. `ordinal` matches exhaustively, so a new variant fails to
    /// compile there, and the assertion below keeps this list in step.
    pub const ALL: [StopCause; 41] = {
        use StopCause::*;
        [
            StoppedEarly,
            Blocked,
            HandoffMissing,
            EmptyResult,
            Uncommitted,
            InputRequired,
            ContextHandoff,
            TokenBudget,
            Deadline,
            Cancelled,
            ProviderStop,
            ProcessCrash,
            AuthRequired,
            UsageLimited,
            ModelUnavailable,
            AdapterUnavailable,
            PermissionDenied,
            IdentityUnobserved,
            LaneTokenLost,
            SourceViewRefused,
            RouteRefused,
            AccessPolicyRefused,
            DiskFloor,
            DiskUnobserved,
            PromptUnreadable,
            CloudPlacementRemoved,
            WriteGrantsRefused,
            WorktreeBusy,
            LaunchHandoffRefused,
            RetainedWriterImport,
            UnsupportedPlatform,
            UnknownPacingAccount,
            UnregisteredAccount,
            CredentialHelperFailed,
            CommandUnavailable,
            HostNativeUnconfigured,
            SpawnFailure,
            SettlementFailed,
            Lost,
            Unsettled,
            Quarantined,
        ]
    };

    const fn ordinal(self) -> usize {
        use StopCause::*;
        match self {
            StoppedEarly => 0,
            Blocked => 1,
            HandoffMissing => 2,
            EmptyResult => 3,
            Uncommitted => 4,
            InputRequired => 5,
            ContextHandoff => 6,
            TokenBudget => 7,
            Deadline => 8,
            Cancelled => 9,
            ProviderStop => 10,
            ProcessCrash => 11,
            AuthRequired => 12,
            UsageLimited => 13,
            ModelUnavailable => 14,
            AdapterUnavailable => 15,
            PermissionDenied => 16,
            IdentityUnobserved => 17,
            LaneTokenLost => 18,
            SourceViewRefused => 19,
            RouteRefused => 20,
            AccessPolicyRefused => 21,
            DiskFloor => 22,
            DiskUnobserved => 23,
            PromptUnreadable => 24,
            CloudPlacementRemoved => 25,
            WriteGrantsRefused => 26,
            WorktreeBusy => 27,
            LaunchHandoffRefused => 28,
            RetainedWriterImport => 29,
            UnsupportedPlatform => 30,
            UnknownPacingAccount => 31,
            UnregisteredAccount => 32,
            CredentialHelperFailed => 33,
            CommandUnavailable => 34,
            HostNativeUnconfigured => 35,
            SpawnFailure => 36,
            SettlementFailed => 37,
            Lost => 38,
            Unsettled => 39,
            Quarantined => 40,
        }
    }

    /// This cause's row. A cause without a row goes to the chair; the table
    /// test makes that unreachable.
    pub fn rule(self) -> StopRule {
        STOP_TABLE
            .iter()
            .copied()
            .find(|rule| rule.cause == self)
            .unwrap_or(StopRule {
                cause: self,
                default: CHAIR,
                pinned: true,
                retries: 0,
            })
    }

    pub fn default_next(self) -> NextStep {
        self.rule().default
    }

    /// What follows a stop with this cause: the chair while custody is not
    /// proved released (one writer per worktree is a real boundary), else the
    /// node's `on_stop` override unless the cause is pinned, else a fresh
    /// same-route attempt while the cause's retries last, else the default.
    pub fn next_step(self, on_stop: &OnStop, facts: StopFacts) -> NextStep {
        if !facts.custody_released {
            return CHAIR;
        }
        let rule = self.rule();
        match on_stop.get(self) {
            Some(step) if !rule.pinned => step,
            _ if facts.retries < rule.retries => NextStep::SameRoute {
                continuation: Continuation::Fresh,
            },
            _ => rule.default,
        }
    }

    /// A usage limit, an unavailable model or a missing login: the route
    /// cannot serve the work now, so it moves to another agent at once.
    pub const fn depleted(self) -> bool {
        matches!(
            self,
            Self::UsageLimited | Self::ModelUnavailable | Self::AuthRequired
        )
    }

    /// The cause a terminal outcome names by itself, for an attempt that
    /// sealed no more specific stop. A failed task is not a stop.
    pub const fn of_outcome(outcome: crate::run::Outcome) -> Option<Self> {
        use crate::run::Outcome as O;
        match outcome {
            O::Succeeded | O::ReviewRequired | O::Failed | O::Rejected => None,
            O::Cancelled => Some(Self::Cancelled),
            O::TimedOut => Some(Self::Deadline),
            O::RateLimited | O::QuotaLimited => Some(Self::UsageLimited),
            O::AuthRequired => Some(Self::AuthRequired),
            O::ModelUnavailable => Some(Self::ModelUnavailable),
            O::PermissionDenied => Some(Self::PermissionDenied),
            O::Quarantined => Some(Self::Quarantined),
            O::Lost => Some(Self::Lost),
            O::Unsettled => Some(Self::Unsettled),
            O::Uncommitted => Some(Self::Uncommitted),
            O::InputRequired => Some(Self::InputRequired),
        }
    }

    /// The cause a handoff declares, when it declares unfinished work.
    pub const fn declared(status: HandoffStatus) -> Option<Self> {
        match status {
            HandoffStatus::Done => None,
            HandoffStatus::StoppedEarly => Some(Self::StoppedEarly),
            HandoffStatus::Blocked => Some(Self::Blocked),
        }
    }

    pub const fn as_str(self) -> &'static str {
        use StopCause::*;
        match self {
            StoppedEarly => "stopped_early",
            Blocked => "blocked",
            HandoffMissing => "handoff_missing",
            EmptyResult => "empty_result",
            Uncommitted => "uncommitted",
            InputRequired => "input_required",
            ContextHandoff => "context_handoff",
            TokenBudget => "token_budget",
            Deadline => "deadline",
            Cancelled => "cancelled",
            ProviderStop => "provider_stop",
            ProcessCrash => "process_crash",
            AuthRequired => "auth_required",
            UsageLimited => "usage_limited",
            ModelUnavailable => "model_unavailable",
            AdapterUnavailable => "adapter_unavailable",
            PermissionDenied => "permission_denied",
            IdentityUnobserved => "identity_unobserved",
            LaneTokenLost => "lane_token_lost",
            SourceViewRefused => "source_view_refused",
            RouteRefused => "route_refused",
            AccessPolicyRefused => "access_policy_refused",
            DiskFloor => "disk_floor",
            DiskUnobserved => "disk_unobserved",
            PromptUnreadable => "prompt_unreadable",
            CloudPlacementRemoved => "cloud_placement_removed",
            WriteGrantsRefused => "write_grants_refused",
            WorktreeBusy => "worktree_busy",
            LaunchHandoffRefused => "launch_handoff_refused",
            RetainedWriterImport => "retained_writer_import",
            UnsupportedPlatform => "unsupported_platform",
            UnknownPacingAccount => "unknown_pacing_account",
            UnregisteredAccount => "unregistered_account",
            CredentialHelperFailed => "credential_helper_failed",
            CommandUnavailable => "command_unavailable",
            HostNativeUnconfigured => "host_native_unconfigured",
            SpawnFailure => "spawn_failure",
            SettlementFailed => "settlement_failed",
            Lost => "lost",
            Unsettled => "unsettled",
            Quarantined => "quarantined",
        }
    }
}

const _: () = {
    let mut i = 0;
    while i < StopCause::ALL.len() {
        assert!(StopCause::ALL[i].ordinal() == i);
        i += 1;
    }
};

/// The stop cause of a terminal cause. Exhaustive, so a new terminal cause
/// must name its stop cause. A host refusal without a more specific cause has
/// no entry: preparation names the cause that refused.
impl From<crate::run::TerminalCause> for StopCause {
    fn from(cause: crate::run::TerminalCause) -> Self {
        use crate::run::TerminalCause as T;
        match cause {
            T::SourceViewRefused => Self::SourceViewRefused,
            T::AccountLoginRequired => Self::AuthRequired,
            T::RouteResolutionRefused => Self::RouteRefused,
            T::AccessPolicyRefused => Self::AccessPolicyRefused,
            T::DiskFloor => Self::DiskFloor,
            T::DiskObservationUnavailable => Self::DiskUnobserved,
            T::StoredPromptUnreadable => Self::PromptUnreadable,
            T::CloudPlacementRemoved => Self::CloudPlacementRemoved,
            T::WriteGrantsRefused => Self::WriteGrantsRefused,
            T::WorktreeBusy => Self::WorktreeBusy,
            T::HandoffRefused => Self::LaunchHandoffRefused,
            T::RetainedWriterImport => Self::RetainedWriterImport,
            T::UnsupportedPlatform => Self::UnsupportedPlatform,
            T::UnknownPacingAccount => Self::UnknownPacingAccount,
            T::UnregisteredAccount => Self::UnregisteredAccount,
            T::DeliveryPrepareCredentialHelper => Self::CredentialHelperFailed,
            T::SpawnFailure => Self::SpawnFailure,
            T::ProcessCrash => Self::ProcessCrash,
            T::TokenBudget => Self::TokenBudget,
            T::SettlementFailure => Self::SettlementFailed,
            T::ProviderEarlyStop => Self::ProviderStop,
            T::FreePoolQuota => Self::UsageLimited,
            T::CommandUnavailable => Self::CommandUnavailable,
            T::HostNativeUnconfigured => Self::HostNativeUnconfigured,
            T::IdentityUnobserved => Self::IdentityUnobserved,
            T::StoppedNoWork => Self::StoppedEarly,
            T::EmptyWriterResult => Self::EmptyResult,
            T::ContextHandoff => Self::ContextHandoff,
            T::Blocked => Self::Blocked,
            T::LaneTokenLost => Self::LaneTokenLost,
        }
    }
}

impl std::fmt::Display for StopCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A sealed stop. The handoff is decoded once, at settlement, and stored
/// typed; nothing re-reads it from result text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StopWire")]
pub struct Stop {
    cause: StopCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handoff: Option<Handoff>,
    #[serde(flatten)]
    extra: crate::ExtraFields,
}

#[derive(Deserialize)]
struct StopWire {
    cause: StopCause,
    #[serde(default)]
    handoff: Option<Handoff>,
    #[serde(flatten)]
    extra: crate::ExtraFields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StopError {
    #[error("{0}")]
    Handoff(&'static str),
    #[error("handoff exceeds 12288 bytes")]
    HandoffBytes,
    #[error("a {0} stop needs a handoff with status {0}")]
    Declared(StopCause),
}

impl Stop {
    /// A declared cause (`stopped_early`, `blocked`) needs the handoff that
    /// declared it. Any other cause may carry the writer's last handoff.
    pub fn new(cause: StopCause, handoff: Option<Handoff>) -> Result<Self, StopError> {
        Self::with_extra(cause, handoff, Default::default())
    }

    fn with_extra(
        cause: StopCause,
        handoff: Option<Handoff>,
        extra: crate::ExtraFields,
    ) -> Result<Self, StopError> {
        if let Some(handoff) = &handoff {
            handoff.validate().map_err(StopError::Handoff)?;
            let bytes = serde_json::to_vec(handoff).map_err(|_| StopError::HandoffBytes)?;
            if bytes.len() > HANDOFF_BYTES {
                return Err(StopError::HandoffBytes);
            }
        }
        let declared = handoff
            .as_ref()
            .and_then(|handoff| StopCause::declared(handoff.status));
        if matches!(cause, StopCause::StoppedEarly | StopCause::Blocked) && declared != Some(cause)
        {
            return Err(StopError::Declared(cause));
        }
        Ok(Self {
            cause,
            handoff,
            extra,
        })
    }

    /// The stop a handoff declares: none for a `done` handoff, an error for
    /// one that cannot be sealed.
    pub fn declared(handoff: Handoff) -> Result<Option<Self>, StopError> {
        let Some(cause) = StopCause::declared(handoff.status) else {
            return Ok(None);
        };
        Self::new(cause, Some(handoff)).map(Some)
    }

    /// The stop settlement seals: the recorded terminal cause if any, else the
    /// cause the handoff declares, else none (a finished attempt).
    pub fn sealed(
        cause: Option<crate::run::TerminalCause>,
        handoff: Option<Handoff>,
    ) -> Result<Option<Self>, StopError> {
        match (cause, handoff) {
            (Some(cause), handoff) => Self::new(cause.into(), handoff).map(Some),
            (None, Some(handoff)) => Self::declared(handoff),
            (None, None) => Ok(None),
        }
    }

    pub fn cause(&self) -> StopCause {
        self.cause
    }

    pub fn handoff(&self) -> Option<&Handoff> {
        self.handoff.as_ref()
    }

    /// The writer's own one-line account, such as what blocks a `blocked`
    /// stop, for `status` and `output`.
    pub fn note(&self) -> Option<&str> {
        self.handoff.as_ref().map(|handoff| handoff.note.as_str())
    }

    pub fn next_step(&self, on_stop: &OnStop, facts: StopFacts) -> NextStep {
        self.cause.next_step(on_stop, facts)
    }
}

impl TryFrom<StopWire> for Stop {
    type Error = StopError;

    fn try_from(wire: StopWire) -> Result<Self, Self::Error> {
        Self::with_extra(wire.cause, wire.handoff, wire.extra)
    }
}
