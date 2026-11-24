//! Candidate and admission mapping for facade refusals. Kept beside facade.rs
//! so a validator's reason survives into the operator's refusal without growing
//! the facade production file past its line ceiling.
use super::{DispatchRefusalFact, FacadeError, InvalidInputDetail};

impl FacadeError {
    pub fn graph_candidate(error: crate::graph::CandidateError) -> Self {
        use crate::graph::CandidateError;
        match error {
            CandidateError::Plan(reason) => {
                Self::graph_admission(crate::graph::AdmissionError::Plan(reason))
            }
            CandidateError::CloudPlacementRemoved => {
                Self::invalid(InvalidInputDetail::CloudPlacementRemoved)
            }
            CandidateError::Checkpoint => Self::StateCorrupt,
            CandidateError::CheckpointTopology { node, invariant } => Self::StateCorruptDetail {
                detail: format!("graph checkpoint node {node}: {invariant}"),
            },
            CandidateError::Cycle { node } => {
                Self::graph_admission(crate::graph::AdmissionError::Cycle { node })
            }
            CandidateError::WriterWorktree { node } => {
                Self::graph_admission(crate::graph::AdmissionError::WriterWorktree { node })
            }
            CandidateError::Schema(reason) => {
                Self::graph_admission(crate::graph::AdmissionError::Schema(reason))
            }
            CandidateError::Composite(reason) => {
                Self::graph_admission(crate::graph::AdmissionError::Composite(reason))
            }
            CandidateError::OutputKind { field, reason } => {
                Self::graph_admission(crate::graph::AdmissionError::OutputKind { field, reason })
            }
            CandidateError::Extension(cause) => Self::DispatchRefused {
                reason: cause.to_string(),
                fact: Some(DispatchRefusalFact::GraphExtension(cause)),
                disk: None,
            },
            CandidateError::NodeStructural { node, limit } => Self::DispatchRefused {
                reason: format!("node {node}: {limit}"),
                fact: Some(DispatchRefusalFact::GraphNodeLimit { node, limit }),
                disk: None,
            },
            CandidateError::Structural(limit) => {
                Self::graph_admission(crate::graph::AdmissionError::StructuralLimit(limit))
            }
            CandidateError::Envelope(limit) => {
                Self::graph_admission(crate::graph::AdmissionError::Envelope(limit))
            }
            other => Self::DispatchRefused {
                fact: None,
                disk: None,
                reason: other.to_string(),
            },
        }
    }

    /// Retain the typed admission stage while keeping its original refusal text.
    pub fn graph_admission(error: crate::graph::AdmissionError) -> Self {
        use crate::graph::AdmissionError;
        // A validator rejection already names the node and the reason. Keep that
        // text instead of flattening it into a generic topology refusal so the
        // repair text can be followed. The writer-worktree spelling is a typed
        // fact and keeps its own detail.
        // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
        let named_reason = match &error {
            AdmissionError::Plan(reason)
            | AdmissionError::Composite(reason)
            | AdmissionError::Schema(reason) => Some(reason),
            AdmissionError::OutputKind { reason, .. } => Some(reason),
            _ => None,
        }
        .filter(|reason| {
            !reason.is_empty()
                && reason
                    .strip_prefix("node ")
                    .and_then(|reason| reason.strip_suffix(": writer worktree"))
                    .is_none()
        })
        .cloned();
        let fact = match &error {
            AdmissionError::CloudPlacementRemoved => {
                return Self::invalid(InvalidInputDetail::CloudPlacementRemoved);
            }
            AdmissionError::Schema(_) => DispatchRefusalFact::GraphSchema,
            AdmissionError::Composite(_) => DispatchRefusalFact::GraphComposite,
            AdmissionError::Plan(reason) => reason
                .strip_prefix("node ")
                .and_then(|reason| reason.strip_suffix(": writer worktree"))
                .and_then(|node| node.to_owned().try_into().ok())
                .map_or(DispatchRefusalFact::GraphPlan, |node| {
                    DispatchRefusalFact::GraphWriterWorktree { node }
                }),
            AdmissionError::WriterWorktree { node } => {
                DispatchRefusalFact::GraphWriterWorktree { node: node.clone() }
            }
            AdmissionError::Cycle { node } => {
                DispatchRefusalFact::GraphCycle { node: node.clone() }
            }
            AdmissionError::StructuralLimit(limit) => {
                DispatchRefusalFact::GraphStructuralLimit(*limit)
            }
            AdmissionError::Envelope(_) => DispatchRefusalFact::GraphEnvelope,
            AdmissionError::OutputKind { field, .. } => {
                DispatchRefusalFact::GraphOutputKind(*field)
            }
        };
        Self::DispatchRefused {
            reason: named_reason.unwrap_or_else(|| match &fact {
                DispatchRefusalFact::GraphStructuralLimit(limit) => limit.to_string(),
                DispatchRefusalFact::GraphCycle { node } => {
                    format!("node {node}: dependency cycle")
                }
                DispatchRefusalFact::GraphWriterWorktree { node } => {
                    format!("node {node}: writer worktree")
                }
                DispatchRefusalFact::GraphEnvelope => {
                    "graph checkpoint would exceed its size limit".into()
                }
                DispatchRefusalFact::GraphSchema => {
                    "graph document schema_version is invalid".into()
                }
                DispatchRefusalFact::GraphOutputKind(_) => {
                    "graph outputs select an unavailable output kind".into()
                }
                DispatchRefusalFact::GraphComposite => {
                    "graph composite or loop expansion is invalid".into()
                }
                _ => "graph plan schema or topology is invalid".into(),
            }),
            disk: None,
            fact: Some(fact),
        }
    }
}
