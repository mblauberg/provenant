//! One bounded entry point for advancing a graph or a queue item.
//!
//! The scheduler can inject its subject-scoped driver through [`wake_with`].
//! The production adapter reports an explicit skipped outcome until that
//! subject-scoped driver is wired.

use provenant_core::{Deadline, ProjectId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WakeSubject {
    Graph {
        project: ProjectId,
        id: String,
    },
    QueueItem {
        project: ProjectId,
        queue: String,
        item: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WakeReason {
    Tick,
    Wait,
    Result,
    InlineRun,
    Status,
    Output,
    PullRequest,
    Lane,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum WakeOutcome {
    Advanced { transitions: usize },
    Busy { holder: String },
    Idle,
    Skipped { code: String },
    OverBudget { cursor: String },
}

/// Run one injectable wake driver. This seam lets scheduler contract tests
/// exercise a fake graph without opening host state or invoking a provider.
#[allow(dead_code)]
pub(crate) fn wake_with(
    subject: WakeSubject,
    reason: WakeReason,
    deadline: Deadline,
    advance: impl FnOnce(WakeSubject, WakeReason, Deadline) -> WakeOutcome,
) -> WakeOutcome {
    advance(subject, reason, deadline)
}

/// Advance one subject through the subject-scoped scheduler.
#[allow(dead_code)]
pub(crate) fn wake(_subject: WakeSubject, _reason: WakeReason, _deadline: Deadline) -> WakeOutcome {
    WakeOutcome::Skipped {
        code: "WAKE_SUBJECT_SCOPED_UNWIRED".into(),
    }
}
