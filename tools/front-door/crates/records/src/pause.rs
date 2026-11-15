//! Why a graph is paused for the chair, stored typed rather than as a code
//! string beside a sentence.
use crate::stop::StopCause;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

pub const PAUSE_REASON_BYTES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PauseCause {
    /// A stop whose next step is the chair, after the table, any override,
    /// route availability and the continuation budget.
    Stop {
        stop: StopCause,
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// The continuation budget ran out before the stop's next step.
    ContinuationBudget {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// A graph budget was exceeded before the next node could start.
    BudgetExceeded {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// A plan expansion was refused.
    InvalidPlanExpansion {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// A loop decision was refused.
    InvalidLoopDecision {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// The loop's ask was closed without an answer.
    LoopAskClosed {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// A loop paused at its declared check.
    LoopPaused {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
    /// A structural graph limit was reached.
    StructuralLimit {
        #[serde(flatten)]
        extra: crate::ExtraFields,
    },
}

impl PauseCause {
    pub const ALL: [Self; 8] = [
        Self::Stop {
            stop: StopCause::Blocked,
            extra: std::collections::BTreeMap::new(),
        },
        Self::ContinuationBudget {
            extra: std::collections::BTreeMap::new(),
        },
        Self::BudgetExceeded {
            extra: std::collections::BTreeMap::new(),
        },
        Self::InvalidPlanExpansion {
            extra: std::collections::BTreeMap::new(),
        },
        Self::InvalidLoopDecision {
            extra: std::collections::BTreeMap::new(),
        },
        Self::LoopAskClosed {
            extra: std::collections::BTreeMap::new(),
        },
        Self::LoopPaused {
            extra: std::collections::BTreeMap::new(),
        },
        Self::StructuralLimit {
            extra: std::collections::BTreeMap::new(),
        },
    ];

    const fn identity_tag(&self) -> u8 {
        match self {
            Self::Stop { .. } => 0,
            Self::ContinuationBudget { .. } => 1,
            Self::BudgetExceeded { .. } => 2,
            Self::InvalidPlanExpansion { .. } => 3,
            Self::InvalidLoopDecision { .. } => 4,
            Self::LoopAskClosed { .. } => 5,
            Self::LoopPaused { .. } => 6,
            Self::StructuralLimit { .. } => 7,
        }
    }
}

impl PartialEq for PauseCause {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Stop { stop: left, .. }, Self::Stop { stop: right, .. }) => left == right,
            _ => self.identity_tag() == other.identity_tag(),
        }
    }
}
impl Eq for PauseCause {}

impl Hash for PauseCause {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity_tag().hash(state);
        if let Self::Stop { stop, .. } = self {
            stop.hash(state);
        }
    }
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
        self.cause.clone()
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
