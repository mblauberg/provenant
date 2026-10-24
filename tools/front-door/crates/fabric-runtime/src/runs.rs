//! Typed F3 run records and helpers consumed by the binary facade.

use super::coordination::facade::{AttemptTarget, WaitCondition};
use provenant_core::OperationId;
use provenant_records::run::{AttemptRecord, RunEvent};
use serde::{Deserialize, Serialize};

/// A successful submission acknowledgement. The operation id is the
/// idempotency key; targets identify the attempts accepted by the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionReceipt {
    pub operation_id: OperationId,
    pub targets: Vec<AttemptTarget>,
    /// The writer worktree a peer created for this submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PeerWorktree>,
}

/// Where a peer writer's commits stay when nothing delivers them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerWorktree {
    pub path: String,
    pub branch: String,
}

/// A bounded page of attempt records. `next_after` is supplied only when a
/// caller can continue the same ordered selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptPage {
    pub items: Vec<AttemptRecord>,
    pub next_after: Option<AttemptTarget>,
    pub truncated: bool,
}

/// Typed result for the operations in `RunInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunResult {
    List {
        page: AttemptPage,
    },
    Status {
        attempt: AttemptRecord,
    },
    Wait {
        condition: WaitCondition,
        attempts: Vec<AttemptRecord>,
        satisfied: bool,
        timed_out: bool,
    },
    Cancel {
        attempt: AttemptRecord,
    },
    Resume {
        attempt: AttemptRecord,
    },
}

/// Run events are paged by their monotonic sequence. `cursor` is the sequence
/// a caller should pass as `after` on the next read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEventsPage {
    pub items: Vec<RunEvent>,
    pub cursor: u64,
    pub truncated: bool,
}

/// Reports operations the bound owner can actually serve in this context.
/// Host configuration by itself must not make an execution capability true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCapabilities {
    pub submit: bool,
    pub list: bool,
    pub status: bool,
    pub wait: bool,
    pub cancel: bool,
    pub resume: bool,
    pub output: bool,
    pub events: bool,
}

/// Input-required wakes either condition; other outcomes remain result data.
pub fn wait_satisfied<'a>(
    condition: WaitCondition,
    states: impl Iterator<Item = &'a provenant_records::run::AttemptState>,
) -> bool {
    use provenant_records::run::{AttemptState, Outcome};
    let mut any = false;
    let mut all = true;
    let mut input = false;
    for state in states {
        let terminal = matches!(state, AttemptState::Terminal { .. });
        any |= terminal;
        all &= terminal;
        input |= matches!(
            state,
            AttemptState::Terminal {
                outcome: Outcome::InputRequired,
                ..
            }
        );
    }
    input
        || match condition {
            WaitCondition::Any => any,
            WaitCondition::All => all,
        }
}
#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::run::{AttemptState, Outcome};
    #[test]
    fn input_required_wakes_all_without_waiting_for_a_sibling() {
        let input = AttemptState::Terminal {
            outcome: Outcome::InputRequired,
            exit_code: None,
            finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
            reason: None,
        };
        let states = [AttemptState::Running, input];
        assert!(wait_satisfied(WaitCondition::All, states.iter()));
        assert!(!wait_satisfied(
            WaitCondition::All,
            [AttemptState::Running, AttemptState::Queued].iter()
        ));
    }
}
