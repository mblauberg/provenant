//! Why a graph is paused for the chair, stored typed rather than as a code
//! string beside a sentence.
use crate::stop::StopCause;
use serde::{Deserialize, Serialize};

pub const PAUSE_REASON_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "stop", rename_all = "snake_case")]
pub enum PauseCause {
    /// A stop whose next step is the chair, after the table, any override,
    /// route availability and the continuation budget.
    Stop(StopCause),
    /// The continuation budget ran out before the stop's next step.
    ContinuationBudget,
    /// A graph budget was exceeded before the next node could start.
    BudgetExceeded,
    /// A plan expansion was refused.
    InvalidPlanExpansion,
    /// A loop decision was refused.
    InvalidLoopDecision,
    /// The loop's ask was closed without an answer.
    LoopAskClosed,
    /// A loop paused at its declared check.
    LoopPaused,
    /// A structural graph limit was reached.
    StructuralLimit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PauseWire")]
pub struct Pause {
    cause: PauseCause,
    /// The node that paused the graph, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<String>,
    /// One bounded sentence for `status`.
    reason: String,
    #[serde(flatten)]
    extra: crate::ExtraFields,
}

#[derive(Deserialize)]
struct PauseWire {
    cause: PauseCause,
    #[serde(default)]
    node: Option<String>,
    reason: String,
    #[serde(flatten)]
    extra: crate::ExtraFields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PauseError {
    #[error("pause reason must be 1..=4096 bytes")]
    Reason,
    #[error("pause node must be 1..=200 bytes of ASCII letters, digits, '-', '_' or '.'")]
    Node,
}

impl Pause {
    pub fn new(
        cause: PauseCause,
        node: Option<String>,
        reason: impl Into<String>,
    ) -> Result<Self, PauseError> {
        PauseWire {
            cause,
            node,
            reason: reason.into(),
            extra: Default::default(),
        }
        .try_into()
    }

    pub fn cause(&self) -> PauseCause {
        self.cause
    }

    pub fn node(&self) -> Option<&str> {
        self.node.as_deref()
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl TryFrom<PauseWire> for Pause {
    type Error = PauseError;

    fn try_from(wire: PauseWire) -> Result<Self, Self::Error> {
        if wire.reason.trim().is_empty() || wire.reason.len() > PAUSE_REASON_BYTES {
            return Err(PauseError::Reason);
        }
        if wire
            .node
            .as_deref()
            .is_some_and(|node| !crate::work_key::valid_node(node))
        {
            return Err(PauseError::Node);
        }
        Ok(Self {
            cause: wire.cause,
            node: wire.node,
            reason: wire.reason,
            extra: wire.extra,
        })
    }
}
