//! Typed F3 run-owner seam and the run-only adapter used by the binary facade.
//!
//! The runtime owns the request and response contracts. The binary composition
//! layer implements `RunOwner` by binding the local run store, dispatcher and
//! home-host router. This module does not launch processes or invent records.
//!
//! TODO(chair): bind submit, cancel and resume to Lane B's dispatch/control API
//! once that API reaches main. The current run crate stores and reads records;
//! a durable queued record alone is not proof that work was dispatched.

use super::coordination::facade::{
    AttemptTarget, FacadeError, OutputInput, OutputResult, RunInput, SubmitInput, WaitCondition,
};
use provenant_core::{CancelToken, OperationId, WaitBudget};
use provenant_records::run::{AttemptRecord, RunEvent};
use serde::{Deserialize, Serialize};

/// A successful submission acknowledgement. The operation id is the
/// idempotency key; targets identify the attempts accepted by the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionReceipt {
    pub operation_id: OperationId,
    pub targets: Vec<AttemptTarget>,
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

/// The narrow execution boundary implemented by front-door composition.
///
/// Implementations scope every lookup and mutation to the authenticated
/// project and actor. Before launching a submitted task, the owner must enforce
/// its access mode and bind a writer to its verified registered worktree, or
/// refuse the submission. Declaring read-only access never grants write access.
/// Mutations use the supplied operation id for idempotency;
/// an uncertain launch returns `FacadeError::LaunchUnknown` with that id and
/// is reconciled by a receipt lookup, never by blindly retrying launch.
///
/// `Wait` snapshots its target set at entry, preserves that order, and only
/// observes those attempts for the requested Any/All condition. Cancelling an
/// observation does not cancel a launched attempt. `Cancel` is the explicit
/// run mutation. `Fetch` accepts only an artifact reference already bound to
/// the target attempt, not an arbitrary filesystem path.
pub trait RunOwner {
    fn capabilities(&self) -> RunCapabilities;

    fn submit(
        &mut self,
        input: SubmitInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<SubmissionReceipt, FacadeError>;

    fn run(
        &mut self,
        input: RunInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<RunResult, FacadeError>;

    fn output(
        &mut self,
        input: OutputInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<OutputResult, FacadeError>;

    fn events(
        &mut self,
        after: u64,
        limit: u16,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<RunEventsPage, FacadeError>;
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
