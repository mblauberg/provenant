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

pub fn process_matches(pid: i64, recorded: Option<&str>, presence: Presence, start: StartObservation<'_>) -> bool {
    pid > 1 && presence == Presence::Alive && recorded.is_some_and(|value| start.matches(value))
}

pub fn observed_alive(pid: i64, recorded: Option<&str>, presence: Presence, start: StartObservation<'_>) -> bool {
    if pid <= 0 || presence == Presence::Gone {
        return false;
    }
    presence == Presence::Unknown || start.canonical.is_none() || recorded.is_none_or(|value| start.matches(value))
}

pub fn group_state(group: &RecordedGroup, self_pid: i64, members: Presence, leader: Presence, start: StartObservation<'_>) -> GroupState {
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
    let value = text.trim();
    let tail = value.as_bytes().get(value.len().checked_sub(4)?..)?;
    tail.iter().all(u8::is_ascii_digit).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_identity_excludes_reaping_but_never_allows_signalling() {
        for presence in [Presence::Alive, Presence::Unknown] {
            let start = StartObservation { canonical: None, legacy: Some("old") };
            assert!(observed_alive(42, Some("old"), presence, start));
            assert!(!process_matches(42, Some("old"), presence, start));
        }
    }

    #[test]
    fn legacy_match_requires_an_available_canonical_identity() {
        let start = StartObservation { canonical: Some("C time"), legacy: Some("local time") };
        assert!(process_matches(42, Some("local time"), Presence::Alive, start));
        assert!(!observed_alive(42, Some("different"), Presence::Alive, start));
        assert!(!process_matches(1, Some("C time"), Presence::Alive, start));
        assert!(!observed_alive(42, None, Presence::Gone, start));
    }

    #[test]
    fn leaderless_group_remains_attributable_but_recycled_leader_is_exited() {
        let group = RecordedGroup { pid: 42, pgid: 42, started_at: Some("before".into()) };
        let start = StartObservation { canonical: Some("after"), legacy: None };
        assert_eq!(group_state(&group, 100, Presence::Alive, Presence::Gone, start), GroupState::Attributable);
        assert_eq!(group_state(&group, 100, Presence::Alive, Presence::Alive, start), GroupState::Exited);
        assert_eq!(group_state(&group, 42, Presence::Gone, Presence::Gone, start), GroupState::Fenced);
        assert_eq!(group_state(&group, 100, Presence::Unknown, Presence::Gone, start), GroupState::Fenced);
    }

    #[test]
    fn unreadable_start_and_absent_token_stay_absent() {
        assert_eq!(started_at_output("?\n"), None);
        assert_eq!(started_at_output(" Mon Jan  1 00:00:00 2024\n"), Some("Mon Jan  1 00:00:00 2024"));
        let identity = ProcessIdentity { pid: 42, started_at: None, token: None };
        assert_eq!(serde_json::to_string(&identity).unwrap(), r#"{"pid":42,"startedAt":null}"#);
    }
}
