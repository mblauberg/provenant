//! Pure provider lifecycle decisions. OS observations and effects stay outside
//! this module; callers supply monotonic time and apply returned effects.
pub use crate::exec::Termination;
use serde::{Deserialize, Serialize};

pub const DEFAULT_DESCENDANT_GRACE_MS: u64 = 500;
pub const DEFAULT_ROOT_FLUSH_MS: u64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    Running,
    TerminalGrace,
    Stopping,
    Cleaning,
    Exited,
    Published,
}

/// The only reasons that may authorise provider signalling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    ExplicitCancellation,
    WallDeadline,
    ProtocolRejected,
    SupervisorSignal,
    PostTerminalCleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DeliveryFailure {
    IdentityChanged,
    NotOwned,
    NoSuchProcess,
    PermissionDenied,
    OsError(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Delivered,
    Failed(DeliveryFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StopAttempt {
    pub reason: StopReason,
    pub delivery: Option<DeliveryOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupObservation {
    Unknown,
    Pending,
    DescendantsPresent,
    /// Cleanup exhausted its budget; publish the unresolved facts and retain the fence.
    Unresolved,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PublicationFailure {
    NoSpace,
    IoError(i32),
    ReservationUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PublicationOutcome {
    Durable,
    Pending(PublicationFailure),
    VisibleDurabilityUnconfirmed(PublicationFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadlineKind {
    Wall,
    RootFlush,
    DescendantGrace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Effect {
    CloseStdin,
    BeginCleanup(Termination),
    DeliverStop(StopReason),
    PublishExit,
    RetryExitPublication,
    RetryPublishedDirectorySync,
    DeadlineUnavailable(DeadlineKind),
    RejectDeliveryReceipt(StopReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Budgets {
    pub wall_deadline_ms: u64,
    pub root_flush_ms: u64,
    pub descendant_grace_ms: u64,
}
impl Budgets {
    /// `wall_deadline_ms` is an absolute monotonic deadline supplied by Run.
    /// There is deliberately no startup deadline.
    pub const fn new(wall_deadline_ms: u64) -> Self {
        Self {
            wall_deadline_ms,
            root_flush_ms: DEFAULT_ROOT_FLUSH_MS,
            descendant_grace_ms: DEFAULT_DESCENDANT_GRACE_MS,
        }
    }

    pub const fn with_cleanup(
        wall_deadline_ms: u64,
        root_flush_ms: u64,
        descendant_grace_ms: u64,
    ) -> Self {
        Self {
            wall_deadline_ms,
            root_flush_ms,
            descendant_grace_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct State {
    phase: Phase,
    budgets: Budgets,
    root_flush_deadline_ms: Option<u64>,
    descendant_deadline_ms: Option<u64>,
    exit: Option<Termination>,
    never_spawned_reason: Option<StopReason>,
    stop_attempts: Vec<StopAttempt>,
    publication: Option<PublicationOutcome>,
    last_progress_ms: Option<u64>,
}
impl State {
    pub fn prepared(budgets: Budgets) -> Self {
        Self {
            phase: Phase::Prepared,
            budgets,
            root_flush_deadline_ms: None,
            descendant_deadline_ms: None,
            exit: None,
            never_spawned_reason: None,
            stop_attempts: Vec::new(),
            publication: None,
            last_progress_ms: None,
        }
    }

    pub(crate) fn never_spawned() -> Self {
        Self {
            phase: Phase::Exited,
            ..Self::prepared(Budgets::new(0))
        }
    }

    pub const fn phase(&self) -> Phase {
        self.phase
    }

    pub const fn exit(&self) -> Option<Termination> {
        self.exit
    }

    pub const fn never_spawned_reason(&self) -> Option<StopReason> {
        self.never_spawned_reason
    }

    pub fn stop_attempts(&self) -> &[StopAttempt] {
        &self.stop_attempts
    }

    pub const fn publication(&self) -> Option<PublicationOutcome> {
        self.publication
    }

    pub const fn last_progress_ms(&self) -> Option<u64> {
        self.last_progress_ms
    }

    /// A timeout is reportable only after WallDeadline delivery succeeded.
    pub fn timed_out(&self) -> bool {
        self.stop_attempts.iter().any(|attempt| {
            attempt.reason == StopReason::WallDeadline
                && attempt.delivery == Some(DeliveryOutcome::Delivered)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Started,
    Progress,
    Terminal,
    Cancel,
    ProtocolRejected,
    SupervisorSignal,
    RetryStop(StopReason),
    Silence,
    CaptureFailure,
    ObservationFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub reason: StopReason,
    pub outcome: DeliveryOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    pub now_ms: u64,
    /// This is the WNOWAIT observation, supplied before stop requests are
    /// considered in the same turn.
    pub observed_exit: Option<Termination>,
    pub delivery: Option<DeliveryReceipt>,
    pub cleanup: Option<CleanupObservation>,
    pub publication: Option<PublicationOutcome>,
}
impl Tick {
    pub const fn at(now_ms: u64) -> Self {
        Self {
            now_ms,
            observed_exit: None,
            delivery: None,
            cleanup: None,
            publication: None,
        }
    }
}

/// Apply one event and one explicit OS-loop tick. Exit observation is handled
/// first, so a late cancel or deadline cannot signal an already-exited leader.
pub fn step(mut state: State, event: Event, tick: Tick) -> (State, Vec<Effect>) {
    let mut effects = Vec::new();

    if let Some(receipt) = tick.delivery {
        if let Some(attempt) = state
            .stop_attempts
            .iter_mut()
            .find(|attempt| attempt.reason == receipt.reason)
        {
            attempt.delivery = Some(receipt.outcome);
        } else {
            effects.push(Effect::RejectDeliveryReceipt(receipt.reason));
        }
    }

    if let Some(exit) = tick.observed_exit
        && matches!(
            state.phase,
            Phase::Running | Phase::TerminalGrace | Phase::Stopping
        )
    {
        state.exit = Some(exit);
        state.phase = Phase::Cleaning;
        state.descendant_deadline_ms = tick.now_ms.checked_add(state.budgets.descendant_grace_ms);
        if state.descendant_deadline_ms.is_none() {
            effects.push(Effect::DeadlineUnavailable(DeadlineKind::DescendantGrace));
        }
        effects.push(Effect::BeginCleanup(exit));
        return (state, effects);
    }

    if state.phase == Phase::Published {
        return (state, effects);
    }

    if state.phase == Phase::Exited {
        if let Some(publication) = tick.publication {
            state.publication = Some(publication);
            match publication {
                PublicationOutcome::Durable => state.phase = Phase::Published,
                PublicationOutcome::Pending(_) => effects.push(Effect::RetryExitPublication),
                PublicationOutcome::VisibleDurabilityUnconfirmed(_) => {
                    effects.push(Effect::RetryPublishedDirectorySync)
                }
            }
        }
        return (state, effects);
    }

    if state.phase == Phase::Prepared && tick.now_ms >= state.budgets.wall_deadline_ms {
        state.phase = Phase::Exited;
        state.never_spawned_reason = Some(StopReason::WallDeadline);
        effects.push(Effect::PublishExit);
        return (state, effects);
    }

    if matches!(state.phase, Phase::Running | Phase::TerminalGrace)
        && tick.now_ms >= state.budgets.wall_deadline_ms
    {
        request_stop(&mut state, StopReason::WallDeadline, &mut effects);
        return (state, effects);
    }

    match state.phase {
        Phase::Prepared => match event {
            Event::Started => state.phase = Phase::Running,
            Event::Cancel | Event::SupervisorSignal => {
                state.phase = Phase::Exited;
                state.never_spawned_reason = Some(if event == Event::Cancel {
                    StopReason::ExplicitCancellation
                } else {
                    StopReason::SupervisorSignal
                });
                effects.push(Effect::PublishExit);
            }
            Event::Progress
            | Event::Terminal
            | Event::ProtocolRejected
            | Event::RetryStop(_)
            | Event::Silence
            | Event::CaptureFailure
            | Event::ObservationFailure => {}
        },
        Phase::Running => match event {
            Event::Progress => state.last_progress_ms = Some(tick.now_ms),
            Event::Terminal => {
                state.phase = Phase::TerminalGrace;
                state.root_flush_deadline_ms = tick.now_ms.checked_add(state.budgets.root_flush_ms);
                effects.push(Effect::CloseStdin);
                if state.root_flush_deadline_ms.is_none() {
                    effects.push(Effect::DeadlineUnavailable(DeadlineKind::RootFlush));
                }
            }
            Event::Cancel => {
                request_stop(&mut state, StopReason::ExplicitCancellation, &mut effects)
            }
            Event::ProtocolRejected => {
                request_stop(&mut state, StopReason::ProtocolRejected, &mut effects)
            }
            Event::SupervisorSignal => {
                request_stop(&mut state, StopReason::SupervisorSignal, &mut effects)
            }
            Event::Silence
            | Event::CaptureFailure
            | Event::ObservationFailure
            | Event::RetryStop(_)
            | Event::Started => {}
        },
        Phase::TerminalGrace => {
            if event == Event::Progress {
                state.last_progress_ms = Some(tick.now_ms);
            }
            if let Some(deadline) = state.root_flush_deadline_ms
                && tick.now_ms >= deadline
            {
                request_stop(&mut state, StopReason::PostTerminalCleanup, &mut effects);
            } else {
                match event {
                    Event::Cancel => {
                        request_stop(&mut state, StopReason::ExplicitCancellation, &mut effects)
                    }
                    Event::ProtocolRejected => {
                        request_stop(&mut state, StopReason::ProtocolRejected, &mut effects)
                    }
                    Event::SupervisorSignal => {
                        request_stop(&mut state, StopReason::SupervisorSignal, &mut effects)
                    }
                    Event::RetryStop(reason) => retry_stop(&mut state, reason, &mut effects),
                    Event::Progress
                    | Event::Terminal
                    | Event::Silence
                    | Event::CaptureFailure
                    | Event::ObservationFailure
                    | Event::Started => {}
                }
            }
        }
        Phase::Stopping => {
            if let Event::RetryStop(reason) = event {
                retry_stop(&mut state, reason, &mut effects);
            }
            if state.exit.is_some() && tick.cleanup == Some(CleanupObservation::Complete) {
                state.phase = Phase::Exited;
                effects.push(Effect::PublishExit);
            }
        }
        Phase::Cleaning => {
            if matches!(
                tick.cleanup,
                Some(CleanupObservation::Complete | CleanupObservation::Unresolved)
            ) {
                state.phase = Phase::Exited;
                effects.push(Effect::PublishExit);
            } else if tick.cleanup == Some(CleanupObservation::DescendantsPresent)
                && state
                    .descendant_deadline_ms
                    .is_some_and(|deadline| tick.now_ms >= deadline)
            {
                request_stop(&mut state, StopReason::PostTerminalCleanup, &mut effects);
            }
        }
        Phase::Exited | Phase::Published => {}
    }
    (state, effects)
}

fn request_stop(state: &mut State, reason: StopReason, effects: &mut Vec<Effect>) {
    if state.phase == Phase::Stopping
        || (state.phase == Phase::Cleaning && reason != StopReason::PostTerminalCleanup)
    {
        return;
    }
    state.phase = Phase::Stopping;
    if let Some(attempt) = state
        .stop_attempts
        .iter_mut()
        .find(|attempt| attempt.reason == reason)
    {
        if attempt.delivery.is_some() {
            return;
        }
    } else {
        state.stop_attempts.push(StopAttempt {
            reason,
            delivery: None,
        });
    }
    effects.push(Effect::DeliverStop(reason));
}

fn retry_stop(state: &mut State, reason: StopReason, effects: &mut Vec<Effect>) {
    if state.phase != Phase::Stopping {
        return;
    }
    if let Some(attempt) = state
        .stop_attempts
        .iter_mut()
        .find(|attempt| attempt.reason == reason)
    {
        attempt.delivery = None;
        effects.push(Effect::DeliverStop(reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> State {
        let (state, _) = step(
            State::prepared(Budgets::with_cleanup(100, 10, 5)),
            Event::Started,
            Tick::at(0),
        );
        state
    }

    #[test]
    fn closed_reasons_and_phases_are_table_driven() {
        let cases = [
            (Event::Cancel, StopReason::ExplicitCancellation),
            (Event::ProtocolRejected, StopReason::ProtocolRejected),
            (Event::SupervisorSignal, StopReason::SupervisorSignal),
        ];
        for (event, reason) in cases {
            let (state, effects) = step(running(), event, Tick::at(1));
            assert_eq!(state.phase(), Phase::Stopping);
            assert_eq!(
                state.stop_attempts().first().map(|attempt| attempt.reason),
                Some(reason)
            );
            assert_eq!(effects, vec![Effect::DeliverStop(reason)]);
        }
    }

    #[test]
    fn prelaunch_cancel_and_wall_expiry_publish_never_spawned_without_signalling() {
        let (cancelled, effects) = step(
            State::prepared(Budgets::with_cleanup(100, 10, 5)),
            Event::Cancel,
            Tick::at(1),
        );
        assert_eq!(cancelled.phase(), Phase::Exited);
        assert_eq!(
            cancelled.never_spawned_reason(),
            Some(StopReason::ExplicitCancellation)
        );
        assert!(cancelled.stop_attempts().is_empty());
        assert_eq!(effects, vec![Effect::PublishExit]);

        let (expired, effects) = step(
            State::prepared(Budgets::with_cleanup(100, 10, 5)),
            Event::Started,
            Tick::at(100),
        );
        assert_eq!(expired.phase(), Phase::Exited);
        assert_eq!(
            expired.never_spawned_reason(),
            Some(StopReason::WallDeadline)
        );
        assert!(expired.stop_attempts().is_empty());
        assert_eq!(effects, vec![Effect::PublishExit]);
    }

    #[test]
    fn wall_deadline_is_the_only_execution_deadline() {
        let (prepared, effects) = step(
            State::prepared(Budgets::with_cleanup(100, 10, 5)),
            Event::Silence,
            Tick::at(99),
        );
        assert_eq!(prepared.phase(), Phase::Prepared);
        assert!(effects.is_empty());

        let (state, effects) = step(running(), Event::Silence, Tick::at(100));
        assert_eq!(state.phase(), Phase::Stopping);
        assert_eq!(effects, vec![Effect::DeliverStop(StopReason::WallDeadline)]);
        assert!(!state.timed_out());
    }

    #[test]
    fn a_wnowait_exit_wins_over_late_cancel_and_deadline() {
        let mut tick = Tick::at(100);
        tick.observed_exit = Some(Termination::Exit(0));
        let (state, effects) = step(running(), Event::Cancel, tick);
        assert_eq!(state.phase(), Phase::Cleaning);
        assert_eq!(state.exit(), Some(Termination::Exit(0)));
        assert_eq!(state.stop_attempts().len(), 0);
        assert_eq!(effects, vec![Effect::BeginCleanup(Termination::Exit(0))]);
    }

    #[test]
    fn failed_delivery_is_typed_and_never_claims_exit_or_timeout() {
        let (stopping, _) = step(running(), Event::Cancel, Tick::at(1));
        let mut tick = Tick::at(2);
        tick.delivery = Some(DeliveryReceipt {
            reason: StopReason::ExplicitCancellation,
            outcome: DeliveryOutcome::Failed(DeliveryFailure::PermissionDenied),
        });
        let (state, effects) = step(stopping, Event::Silence, tick);
        assert_eq!(state.phase(), Phase::Stopping);
        assert_eq!(
            state.stop_attempts()[0].delivery,
            tick.delivery.map(|r| r.outcome)
        );
        assert!(!state.timed_out());
        assert!(effects.is_empty());
    }

    #[test]
    fn wall_timeout_requires_successful_signal_delivery() {
        let (stopping, _) = step(running(), Event::Silence, Tick::at(100));
        let mut tick = Tick::at(101);
        tick.delivery = Some(DeliveryReceipt {
            reason: StopReason::WallDeadline,
            outcome: DeliveryOutcome::Delivered,
        });
        let (state, _) = step(stopping, Event::Silence, tick);
        assert!(state.timed_out());
    }

    #[test]
    fn terminal_flush_and_descendant_grace_have_distinct_transitions() {
        let (state, effects) = step(running(), Event::Terminal, Tick::at(1));
        assert_eq!(state.phase(), Phase::TerminalGrace);
        assert_eq!(effects, vec![Effect::CloseStdin]);

        let (state, effects) = step(state, Event::Silence, Tick::at(11));
        assert_eq!(state.phase(), Phase::Stopping);
        assert_eq!(
            effects,
            vec![Effect::DeliverStop(StopReason::PostTerminalCleanup)]
        );

        let mut tick = Tick::at(12);
        tick.observed_exit = Some(Termination::Signal(15));
        let (state, effects) = step(state, Event::Cancel, tick);
        assert_eq!(state.phase(), Phase::Cleaning);
        assert_eq!(effects, vec![Effect::BeginCleanup(Termination::Signal(15))]);

        let mut tick = Tick::at(17);
        tick.cleanup = Some(CleanupObservation::DescendantsPresent);
        let (state, effects) = step(state, Event::Silence, tick);
        assert_eq!(state.phase(), Phase::Stopping);
        assert_eq!(
            effects,
            vec![Effect::DeliverStop(StopReason::PostTerminalCleanup)]
        );
    }

    #[test]
    fn failed_or_unknown_census_does_not_authorise_cleanup_stop() {
        let mut tick = Tick::at(5);
        tick.observed_exit = Some(Termination::Exit(0));
        let (state, _) = step(running(), Event::Silence, tick);
        let mut tick = Tick::at(500);
        tick.cleanup = Some(CleanupObservation::Unknown);
        let (state, effects) = step(state, Event::ObservationFailure, tick);
        assert_eq!(state.phase(), Phase::Cleaning);
        assert!(effects.is_empty());
    }

    #[test]
    fn unresolved_cleanup_still_publishes_exit_without_signalling() {
        let mut tick = Tick::at(1);
        tick.observed_exit = Some(Termination::Exit(0));
        let (state, _) = step(running(), Event::Silence, tick);
        let mut tick = Tick::at(2);
        tick.cleanup = Some(CleanupObservation::Unresolved);
        let (state, effects) = step(state, Event::Silence, tick);
        assert_eq!(state.phase(), Phase::Exited);
        assert_eq!(effects, vec![Effect::PublishExit]);
        assert!(state.stop_attempts().is_empty());
    }

    #[test]
    fn cleanup_then_publication_pending_retains_exited_custody() {
        let mut tick = Tick::at(1);
        tick.observed_exit = Some(Termination::Exit(0));
        let (state, _) = step(running(), Event::Silence, tick);
        let mut tick = Tick::at(2);
        tick.cleanup = Some(CleanupObservation::Complete);
        let (state, effects) = step(state, Event::Silence, tick);
        assert_eq!(state.phase(), Phase::Exited);
        assert_eq!(effects, vec![Effect::PublishExit]);

        let mut tick = Tick::at(3);
        tick.publication = Some(PublicationOutcome::Pending(PublicationFailure::NoSpace));
        let (state, effects) = step(state, Event::Cancel, tick);
        assert_eq!(state.phase(), Phase::Exited);
        assert_eq!(effects, vec![Effect::RetryExitPublication]);
        assert!(state.stop_attempts().is_empty());

        let mut tick = Tick::at(4);
        tick.publication = Some(PublicationOutcome::VisibleDurabilityUnconfirmed(
            PublicationFailure::IoError(5),
        ));
        let (state, effects) = step(state, Event::Silence, tick);
        assert_eq!(state.phase(), Phase::Exited);
        assert_eq!(effects, vec![Effect::RetryPublishedDirectorySync]);

        let mut tick = Tick::at(5);
        tick.publication = Some(PublicationOutcome::Durable);
        let (state, effects) = step(state, Event::Silence, tick);
        assert_eq!(state.phase(), Phase::Published);
        assert!(effects.is_empty());
    }

    #[test]
    fn silence_capture_and_observation_failures_are_not_stop_reasons() {
        for event in [
            Event::Silence,
            Event::CaptureFailure,
            Event::ObservationFailure,
        ] {
            let (state, effects) = step(running(), event, Tick::at(1));
            assert_eq!(state.phase(), Phase::Running);
            assert!(state.stop_attempts().is_empty());
            assert!(effects.is_empty());
        }
    }
}
