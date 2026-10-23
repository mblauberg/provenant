//! Decisions over observations; unavailable identity never authorises a signal.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: i64,
    #[serde(rename = "startedAt")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedGroup {
    pub pid: i64,
    pub pgid: i64,
    pub started_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminationOutcome {
    pub run_dir: String,
    pub signalled: bool,
    pub escalated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Only ESRCH proves absence. Permission errors are Unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Alive,
    Gone,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupState {
    Exited,
    Attributable,
    Fenced,
}

/// Both observations refer to the same PID; the adapter owns probing order.
#[derive(Clone, Copy, Debug)]
pub struct StartObservation<'a> {
    pub canonical: Option<&'a str>,
    pub legacy: Option<&'a str>,
}

impl StartObservation<'_> {
    pub fn matches(self, recorded: &str) -> bool {
        self.canonical.is_some()
            && (self.canonical == Some(recorded) || self.legacy == Some(recorded))
    }
}

pub fn process_matches(
    pid: i64,
    recorded: Option<&str>,
    presence: Presence,
    start: StartObservation<'_>,
) -> bool {
    pid > 1 && presence == Presence::Alive && recorded.is_some_and(|value| start.matches(value))
}

pub fn observed_alive(
    pid: i64,
    recorded: Option<&str>,
    presence: Presence,
    start: StartObservation<'_>,
) -> bool {
    if pid <= 0 || presence == Presence::Gone {
        return false;
    }
    presence == Presence::Unknown
        || start.canonical.is_none()
        || recorded.is_none_or(|value| start.matches(value))
}

pub fn group_state(
    group: &RecordedGroup,
    self_pid: i64,
    members: Presence,
    leader: Presence,
    start: StartObservation<'_>,
) -> GroupState {
    if group.pgid <= 0 || group.pgid != group.pid || group.pgid == self_pid {
        return GroupState::Fenced;
    }
    match (members, leader) {
        (Presence::Gone, _) => GroupState::Exited,
        (Presence::Unknown, _) => GroupState::Fenced,
        (_, Presence::Gone) => GroupState::Attributable,
        (_, Presence::Unknown) => GroupState::Fenced,
        _ => match (&group.started_at, start.canonical) {
            (Some(recorded), Some(_)) if start.matches(recorded) => GroupState::Attributable,
            (Some(_), Some(_)) => GroupState::Exited,
            _ => GroupState::Fenced,
        },
    }
}

/// Validate the ps shim's result: '?' and other unreadable markers are unknown.
pub fn started_at_output(text: &str) -> Option<&str> {
    let value = text.trim_matches(|ch: char| {
        matches!(ch,
        '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' |
        '\u{3000}' | '\u{feff}')
    });
    let tail = value.as_bytes().get(value.len().checked_sub(4)?..)?;
    tail.iter().all(u8::is_ascii_digit).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_identity_excludes_reaping_but_never_allows_signalling() {
        for presence in [Presence::Alive, Presence::Unknown] {
            let start = StartObservation {
                canonical: None,
                legacy: Some("old"),
            };
            assert!(observed_alive(42, Some("old"), presence, start));
            assert!(!process_matches(42, Some("old"), presence, start));
        }
    }

    #[test]
    fn legacy_match_requires_an_available_canonical_identity() {
        let start = StartObservation {
            canonical: Some("C time"),
            legacy: Some("local time"),
        };
        assert!(process_matches(
            42,
            Some("local time"),
            Presence::Alive,
            start
        ));
        assert!(!observed_alive(
            42,
            Some("different"),
            Presence::Alive,
            start
        ));
        assert!(!process_matches(1, Some("C time"), Presence::Alive, start));
        assert!(!observed_alive(42, None, Presence::Gone, start));
    }

    #[test]
    fn leaderless_group_remains_attributable_but_recycled_leader_is_exited() {
        let group = RecordedGroup {
            pid: 42,
            pgid: 42,
            started_at: Some("before".into()),
        };
        let start = StartObservation {
            canonical: Some("after"),
            legacy: None,
        };
        assert_eq!(
            group_state(&group, 100, Presence::Alive, Presence::Gone, start),
            GroupState::Attributable
        );
        assert_eq!(
            group_state(&group, 100, Presence::Alive, Presence::Alive, start),
            GroupState::Exited
        );
        assert_eq!(
            group_state(&group, 42, Presence::Gone, Presence::Gone, start),
            GroupState::Fenced
        );
        assert_eq!(
            group_state(&group, 100, Presence::Unknown, Presence::Gone, start),
            GroupState::Fenced
        );
    }

    #[test]
    fn unreadable_start_and_absent_token_stay_absent() {
        assert_eq!(started_at_output("?\n"), None);
        assert_eq!(started_at_output("\u{feff}2024\u{feff}"), Some("2024"));
        assert_eq!(
            started_at_output(" Mon Jan  1 00:00:00 2024\n"),
            Some("Mon Jan  1 00:00:00 2024")
        );
        let identity = ProcessIdentity {
            pid: 42,
            started_at: None,
            token: None,
        };
        assert_eq!(
            serde_json::to_string(&identity).unwrap(),
            r#"{"pid":42,"startedAt":null}"#
        );
    }
}

/// Reasons are explicit inputs. Observation failures and silence have no conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    CancelRequested,
    RunControl,
    OrphanReaper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Terminate,
    Kill,
}

/// Sans-IO counterpart of signalRecordedGroup: each step uses a fresh attribution
/// observation. The OS owner must keep custody until the final observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupTermination {
    Ready,
    /// A non-ESRCH signal failure fences the group without further signals.
    SignalFailed,
    Grace {
        deadline_ms: u64,
    },
    Confirm {
        deadline_ms: u64,
    },
    Done(GroupState),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupEffect {
    Signal(Signal),
    ObserveAfter(u64),
    Finished(GroupState),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupSignalResult {
    Sent,
    NoSuchProcess,
    Failed,
}

impl GroupTermination {
    /// Feed back every OS signal result before the next observation step.
    pub fn signal_result(&mut self, result: GroupSignalResult) {
        if result == GroupSignalResult::Failed && !matches!(self, Self::Done(_)) {
            *self = Self::SignalFailed;
        }
    }

    pub fn step(&mut self, now_ms: u64, observed: GroupState, grace_ms: u64) -> GroupEffect {
        if *self == Self::SignalFailed {
            *self = Self::Done(GroupState::Fenced);
        }
        if let Self::Done(state) = *self {
            return GroupEffect::Finished(state);
        }
        if observed != GroupState::Attributable {
            *self = Self::Done(observed);
            return GroupEffect::Finished(observed);
        }
        match *self {
            Self::Ready => {
                *self = Self::Grace {
                    deadline_ms: now_ms.saturating_add(grace_ms),
                };
                GroupEffect::Signal(Signal::Terminate)
            }
            Self::Grace { deadline_ms } if now_ms >= deadline_ms => {
                *self = Self::Confirm {
                    deadline_ms: now_ms.saturating_add(2_000),
                };
                GroupEffect::Signal(Signal::Kill)
            }
            Self::Confirm { deadline_ms } if now_ms >= deadline_ms => {
                *self = Self::Done(GroupState::Fenced);
                GroupEffect::Finished(GroupState::Fenced)
            }
            Self::Grace { deadline_ms } | Self::Confirm { deadline_ms } => {
                GroupEffect::ObserveAfter(50.min(deadline_ms.saturating_sub(now_ms)))
            }
            Self::SignalFailed => GroupEffect::Finished(GroupState::Fenced),
            Self::Done(state) => GroupEffect::Finished(state),
        }
    }
}

/// Registry and stamped status identities both count, deduplicated by PID/token,
/// rather than start time (a failed record replacement can leave different stamps).
pub fn run_owners(
    record: Option<ProcessIdentity>,
    stamped: Option<ProcessIdentity>,
) -> Vec<ProcessIdentity> {
    let mut owners: Vec<_> = record.into_iter().collect();
    if let Some(stamp) = stamped.filter(|stamp| stamp.pid > 0) {
        if !owners
            .iter()
            .any(|owner| owner.pid == stamp.pid && owner.token == stamp.token)
        {
            owners.push(stamp);
        }
    }
    owners
}

#[cfg(test)]
mod interface_tests {
    use super::*;

    #[test]
    fn termination_rechecks_attribution_and_fences_unconfirmed_kill() {
        let mut state = GroupTermination::Ready;
        assert_eq!(
            state.step(10, GroupState::Attributable, 100),
            GroupEffect::Signal(Signal::Terminate)
        );
        assert_eq!(
            state.step(109, GroupState::Attributable, 100),
            GroupEffect::ObserveAfter(1)
        );
        assert_eq!(
            state.step(110, GroupState::Attributable, 100),
            GroupEffect::Signal(Signal::Kill)
        );
        assert_eq!(
            state.step(2110, GroupState::Attributable, 100),
            GroupEffect::Finished(GroupState::Fenced)
        );
        let mut state = GroupTermination::Ready;
        state.step(0, GroupState::Attributable, 100);
        assert_eq!(
            state.step(100, GroupState::Fenced, 100),
            GroupEffect::Finished(GroupState::Fenced)
        );
        assert_eq!(
            state.step(200, GroupState::Attributable, 100),
            GroupEffect::Finished(GroupState::Fenced)
        );
    }

    #[test]
    fn signal_failures_fence_instead_of_authorising_another_signal() {
        for initial in [
            GroupTermination::Ready,
            GroupTermination::Grace { deadline_ms: 100 },
            GroupTermination::Confirm { deadline_ms: 100 },
        ] {
            let mut state = initial;
            state.signal_result(GroupSignalResult::Failed);
            assert_eq!(
                state.step(0, GroupState::Attributable, 100),
                GroupEffect::Finished(GroupState::Fenced)
            );
            assert_eq!(
                state.step(5000, GroupState::Attributable, 100),
                GroupEffect::Finished(GroupState::Fenced)
            );
        }
        let mut state = GroupTermination::Grace { deadline_ms: 100 };
        state.signal_result(GroupSignalResult::NoSuchProcess);
        assert_eq!(
            state.step(0, GroupState::Exited, 100),
            GroupEffect::Finished(GroupState::Exited)
        );
        assert!(serde_json::from_str::<StopReason>("\"deadline\"").is_err());
        for (reason, wire) in [
            (StopReason::CancelRequested, "cancel_requested"),
            (StopReason::RunControl, "run_control"),
            (StopReason::OrphanReaper, "orphan_reaper"),
        ] {
            assert_eq!(
                serde_json::to_string(&reason).unwrap(),
                format!("\"{wire}\"")
            );
        }
    }

    #[test]
    fn record_and_stamp_are_distinct_only_by_pid_and_token() {
        let owner = ProcessIdentity {
            pid: 42,
            started_at: Some("old".into()),
            token: Some("a".into()),
        };
        let mut stamp = owner.clone();
        stamp.started_at = None;
        assert_eq!(
            run_owners(Some(owner.clone()), Some(stamp.clone())).len(),
            1
        );
        stamp.token = Some("b".into());
        assert_eq!(run_owners(Some(owner), Some(stamp)).len(), 2);
        assert!(run_owners(None, None).is_empty());
    }
}
