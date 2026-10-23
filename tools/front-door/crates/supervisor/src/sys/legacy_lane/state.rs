#![forbid(unsafe_code)]

//! Policy-free transitions. No process, clock, filesystem or signal operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    Cancel,
    StartupDeadline,
    WallDeadline,
    Observer(ObserverReason),
    SupervisorSignal,
    PostExitCleanup,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserverReason(String);
impl ObserverReason {
    pub fn parse(s: &str) -> Option<Self> {
        // The live parser owns classification, but silence is never authority.
        if s.is_empty()
            || s.len() > 128
            || matches!(s, "stalled" | "silence" | "io_error")
            || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            None
        } else {
            Some(Self(s.into()))
        }
    }
}
impl StopReason {
    pub fn forced(&self) -> Option<&str> {
        match self {
            Self::Cancel => Some("cancelled"),
            Self::StartupDeadline => Some("startup_timeout"),
            Self::WallDeadline => Some("wall_timeout"),
            Self::SupervisorSignal => Some("supervisor_signal"),
            Self::Observer(reason) => Some(&reason.0),
            Self::PostExitCleanup => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    TerminalGrace,
    Settling,
    Stopping,
    Exited,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub phase: Phase,
    pub produced: bool,
    pub terminal: bool,
    pub forced: Option<StopReason>,
    pub observer_lost: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            phase: Phase::Running,
            produced: false,
            terminal: false,
            forced: None,
            observer_lost: false,
        }
    }
}
#[derive(Clone, Debug)]
pub enum Event {
    Stdout,
    Terminal,
    ObserverLost,
    Stop(StopReason),
    StartupDeadline,
    WallDeadline,
    GraceDeadline,
    ChildExit,
    SettleDeadline,
    CleanupDone,
    Silence,
    IoError,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    CloseStdin,
    StartGrace,
    StartSettle,
    Stop(StopReason),
    Observe,
    Publish,
}
pub fn step(mut s: State, event: Event) -> (State, Vec<Action>) {
    let mut actions = Vec::new();
    if s.phase == Phase::Exited {
        match event {
            Event::Terminal => s.terminal = true,
            Event::ObserverLost => s.observer_lost = true,
            _ => {}
        }
        return (s, actions);
    }
    match event {
        Event::Stdout => s.produced = true,
        Event::ObserverLost => s.observer_lost = true,
        Event::IoError => {}
        Event::Silence if matches!(s.phase, Phase::Running | Phase::TerminalGrace) => {
            actions.push(Action::Observe)
        }
        Event::Terminal => {
            s.terminal = true;
            if s.phase == Phase::Running {
                s.phase = Phase::TerminalGrace;
                actions.extend([Action::CloseStdin, Action::StartGrace]);
            }
        }
        Event::ChildExit if matches!(s.phase, Phase::Running | Phase::TerminalGrace) => {
            s.phase = Phase::Settling;
            actions.push(Action::StartSettle);
        }
        Event::CleanupDone if s.phase == Phase::Stopping => {
            s.phase = Phase::Exited;
            actions.push(Action::Publish);
        }
        event
            if matches!(
                s.phase,
                Phase::Running | Phase::TerminalGrace | Phase::Settling
            ) =>
        {
            let reason = match event {
                // Exit observation wins over late cancellation and deadlines.
                Event::Stop(_) | Event::StartupDeadline | Event::WallDeadline
                    if s.phase == Phase::Settling =>
                {
                    None
                }
                Event::Stop(reason) => Some(reason),
                Event::StartupDeadline if !s.produced => Some(StopReason::StartupDeadline),
                Event::WallDeadline => Some(StopReason::WallDeadline),
                Event::GraceDeadline if s.phase == Phase::TerminalGrace => {
                    Some(StopReason::PostExitCleanup)
                }
                Event::SettleDeadline if s.phase == Phase::Settling => {
                    Some(StopReason::PostExitCleanup)
                }
                _ => None,
            };
            if let Some(reason) = reason {
                if reason.forced().is_some() {
                    s.forced = Some(reason.clone());
                }
                s.phase = Phase::Stopping;
                actions.push(Action::Stop(reason));
            }
        }
        _ => {}
    }
    (s, actions)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_terminal_facts_survive_process_exit() {
        let (s, _) = step(State::default(), Event::ChildExit);
        let (s, a) = step(s, Event::Terminal);
        assert!(s.terminal);
        assert!(a.is_empty());
        assert!(step(s.clone(), Event::Silence).1.is_empty());
        let (s, _) = step(s, Event::SettleDeadline);
        let (s, _) = step(s, Event::CleanupDone);
        assert!(step(s, Event::Terminal).0.terminal);
    }
    #[test]
    fn transition_table() {
        let rows = [
            (
                Event::Stop(StopReason::Cancel),
                Phase::Stopping,
                vec![Action::Stop(StopReason::Cancel)],
            ),
            (
                Event::StartupDeadline,
                Phase::Stopping,
                vec![Action::Stop(StopReason::StartupDeadline)],
            ),
            (
                Event::WallDeadline,
                Phase::Stopping,
                vec![Action::Stop(StopReason::WallDeadline)],
            ),
            (
                Event::Terminal,
                Phase::TerminalGrace,
                vec![Action::CloseStdin, Action::StartGrace],
            ),
            (Event::ChildExit, Phase::Settling, vec![Action::StartSettle]),
            (Event::Silence, Phase::Running, vec![Action::Observe]),
            (Event::IoError, Phase::Running, vec![]),
            (Event::ObserverLost, Phase::Running, vec![]),
        ];
        for (event, phase, actions) in rows {
            let (s, a) = step(State::default(), event);
            assert_eq!(s.phase, phase);
            assert_eq!(a, actions);
        }
    }
    #[test]
    fn stdout_disarms_only_startup() {
        let (s, _) = step(State::default(), Event::Stdout);
        let (s, a) = step(s, Event::StartupDeadline);
        assert!(a.is_empty());
        assert_eq!(
            step(s, Event::WallDeadline).1,
            vec![Action::Stop(StopReason::WallDeadline)]
        );
    }
    #[test]
    fn natural_exit_wins_late_signal_and_cancel() {
        let (s, _) = step(State::default(), Event::ChildExit);
        for reason in [StopReason::Cancel, StopReason::SupervisorSignal] {
            let (s, a) = step(s.clone(), Event::Stop(reason));
            assert!(a.is_empty());
            assert!(s.forced.is_none());
        }
        let (s, a) = step(s, Event::SettleDeadline);
        assert_eq!(a, vec![Action::Stop(StopReason::PostExitCleanup)]);
        let (s, a) = step(s, Event::CleanupDone);
        assert_eq!(a, vec![Action::Publish]);
        assert!(step(s, Event::Stop(StopReason::Cancel)).1.is_empty());
    }
    #[test]
    fn terminal_cleanup_is_not_forced_and_stop_is_idempotent() {
        let (s, _) = step(State::default(), Event::Terminal);
        let (s, a) = step(s, Event::GraceDeadline);
        assert_eq!(a, vec![Action::Stop(StopReason::PostExitCleanup)]);
        assert!(s.forced.is_none());
        assert!(step(s, Event::Stop(StopReason::WallDeadline)).1.is_empty());
    }
    #[test]
    fn silence_and_io_are_not_observer_reasons() {
        for reason in ["stalled", "silence", "io_error", ""] {
            assert!(ObserverReason::parse(reason).is_none());
        }
        assert!(ObserverReason::parse("usage_limited").is_some());
    }
}
