//! Pure graph admission shared by every publisher. No journal writes here.
use super::refusal::{ExtensionCause, ExtensionRule, SecretCause};
use super::*;
use crate::coordination::{facade::GraphInput, input::ValidateInput};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CandidateError {
    #[error("invalid input field `extend`: extension syntax")]
    Input,
    #[error("graph candidate schema or topology is invalid")]
    Plan,
    #[error("{CLOUD_PLACEMENT_REMOVED}")]
    CloudPlacementRemoved,
    #[error("graph schema is invalid")]
    Schema,
    #[error("graph composite is invalid")]
    Composite,
    #[error("graph output kind is invalid")]
    OutputKind(GraphOutputField),
    #[error("node {node}: dependency cycle")]
    Cycle { node: refusal::GraphIdentifier },
    #[error("node {node}: writer worktree")]
    WriterWorktree { node: refusal::GraphIdentifier },
    #[error("graph candidate checkpoint is invalid")]
    Checkpoint,
    #[error("graph checkpoint node {node}: {invariant}")]
    CheckpointTopology {
        node: refusal::GraphIdentifier,
        invariant: &'static str,
    },
    #[error("{0}")]
    Secret(SecretCause),
    #[error("{0}")]
    Extension(ExtensionCause),
    #[error("{0}")]
    Structural(GraphStructuralLimit),
    #[error("node {node}: {limit}")]
    NodeStructural {
        node: String,
        limit: GraphStructuralLimit,
    },
    #[error("{0}")]
    Envelope(GraphEnvelopeLimit),
}

impl CandidateError {
    fn from_admission(error: AdmissionError) -> Self {
        match error {
            AdmissionError::CloudPlacementRemoved => Self::CloudPlacementRemoved,
            AdmissionError::Cycle { node } => Self::Cycle { node },
            AdmissionError::WriterWorktree { node } => Self::WriterWorktree { node },
            AdmissionError::Schema(_) => Self::Schema,
            AdmissionError::Composite(_) => Self::Composite,
            AdmissionError::OutputKind { field, .. } => Self::OutputKind(field),
            AdmissionError::StructuralLimit(limit) => Self::Structural(limit),
            AdmissionError::Envelope(limit) => Self::Envelope(limit),
            // Other admission errors can contain caller text. Retain only a
            // closed cause until their diagnostic facts have a typed owner.
            _ => Self::Plan,
        }
    }
}

pub struct AdmittedCandidate {
    pub(super) source: GraphDocument,
    pub(super) checkpoint: Option<GraphCheckpoint>,
    pub(super) extensions: Vec<String>,
    pub(super) expanded: GraphDocument,
    pub(super) plan: PlannedGraph,
    pub(super) effective_budget: crate::config::GraphBudget,
    pub(super) growth_limit: Option<GraphStructuralLimit>,
}

impl AdmittedCandidate {
    pub fn expanded(&self) -> &GraphDocument {
        &self.expanded
    }
}

/// Build and check the complete prospective view before publishing bindings or state.
/// The caller holds the journal lock from reading the checkpoint through publication.
pub fn admit_candidate(
    document: &GraphDocument,
    checkpoint: Option<&GraphCheckpoint>,
    extensions: &[String],
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    requester_path: Option<&[u8]>,
) -> Result<AdmittedCandidate, CandidateError> {
    admit(
        document,
        checkpoint,
        extensions,
        limits,
        budget,
        requester_path,
        false,
    )
}

/// Recheck an unchanged running graph; lowered structural limits pause it in the driver.
pub fn admit_running_candidate(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    extensions: &[String],
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    requester_path: Option<&[u8]>,
) -> Result<AdmittedCandidate, CandidateError> {
    admit(
        document,
        Some(checkpoint),
        extensions,
        limits,
        budget,
        requester_path,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn admit(
    document: &GraphDocument,
    checkpoint: Option<&GraphCheckpoint>,
    extensions: &[String],
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    requester_path: Option<&[u8]>,
    running_unchanged: bool,
) -> Result<AdmittedCandidate, CandidateError> {
    GraphInput::Resume {
        extra: Default::default(),
        document: document.graph_id.clone().into(),
        extend: extensions.to_vec(),
    }
    .validate_input(provenant_core::WaitTransport::Cli)
    .map_err(|_| CandidateError::Input)?;
    if let Some(path) = requester_path {
        patch::scan_for_secrets(&String::from_utf8_lossy(path)).map_err(|_| {
            CandidateError::Secret(SecretCause {
                field: refusal::SchemaPath::root(refusal::SchemaField::RequesterPath),
                rule: refusal::SecretRule::Secret,
            })
        })?;
    }
    let scan = |value: &serde_json::Value| {
        refusal::scan_value(value, &patch::scan_for_secrets).map_err(CandidateError::Secret)
    };
    scan(&serde_json::to_value(document).map_err(|_| CandidateError::Plan)?)?;
    let generous = crate::config::GraphLimits {
        nodes: usize::MAX,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        fan_out_width: usize::MAX,
        expansion_depth: usize::MAX,
        ..limits.clone()
    };
    let saved = checkpoint.map_or(&[][..], |c| c.extensions.as_slice());
    let expansions = checkpoint.map_or(&[][..], |c| c.expansions.as_slice());
    scan(&serde_json::to_value(expansions).map_err(|_| CandidateError::Plan)?)?;
    // Persisted data must be diagnosed before prospective plan validation can
    // collapse its invariant into a generic invalid candidate.
    if checkpoint.is_some() {
        plans::validate_persisted_topology(document, saved, expansions, &generous).map_err(
            |(node, invariant)| match node.try_into() {
                Ok(node) => CandidateError::CheckpointTopology { node, invariant },
                Err(_) => CandidateError::Checkpoint,
            },
        )?;
    }
    let mut all = saved.to_vec();
    let mut effective_budget = budget.clone();
    for (extension, replay) in saved.iter().map(|e| (e, true)).chain(
        extensions
            .iter()
            .map(|e| (e, e.starts_with("limits.") && saved.contains(e))),
    ) {
        if extension.starts_with("loop.") {
            loops::validate_extension(document, extension, &generous).map_err(|_| {
                CandidateError::Extension(ExtensionCause::loop_rule(
                    extension,
                    ExtensionRule::Value,
                ))
            })?;
        } else if extension.starts_with("limits.") {
            if !replay {
                return Err(CandidateError::Extension(ExtensionCause::host_limits(
                    extension,
                )));
            }
        } else {
            apply_extension(&mut effective_budget, extension, replay)
                .map_err(CandidateError::Extension)?;
        }
        if !all.contains(extension) {
            all.push(extension.clone());
        }
    }
    if let Some(checkpoint) = checkpoint {
        let mut restored = checkpoint.clone();
        restore_cancelled_nodes(&mut restored).map_err(|_| CandidateError::Checkpoint)?;
        let mut prospective = restored.clone();
        for extension in extensions.iter().filter(|e| e.starts_with("loop.")) {
            for view in [&restored, &prospective] {
                loops::validate_resume_extension(document, view, extension, &generous).map_err(
                    |rule| CandidateError::Extension(ExtensionCause::loop_rule(extension, rule)),
                )?;
            }
            if !prospective.extensions.contains(extension) {
                prospective.extensions.push(extension.clone());
            }
        }
    }
    validate_schema(document).map_err(CandidateError::from_admission)?;
    let expanded = plans::expand_typed(document, &all, expansions, &generous)
        .map_err(CandidateError::from_admission)?;
    validate_schema(&expanded).map_err(CandidateError::from_admission)?;
    if checkpoint.is_none()
        && expanded.nodes.iter().any(|node| {
            node.attempt
                .as_ref()
                .is_some_and(AttemptSpec::names_cloud_placement)
        })
    {
        return Err(CandidateError::CloudPlacementRemoved);
    }
    let (plan, _) =
        validate_flat_plan(&expanded, &generous).map_err(CandidateError::from_admission)?;
    validate_output_kinds(&expanded).map_err(CandidateError::from_admission)?;
    // Expansion with no substitutions commonly retains the exact source.
    // It has already passed both decoded-field and encoded-record scans.
    if expanded != *document {
        scan(&serde_json::to_value(&expanded).map_err(|_| CandidateError::Plan)?)?;
    }
    let topology_changes = if running_unchanged {
        let previous = plans::expand(document, saved, expansions, &generous)
            .map_err(|_| CandidateError::Plan)?;
        previous != expanded
    } else {
        false
    };
    let limit = structural_limit(&expanded, &plan, limits);
    let growth_limit = if running_unchanged && topology_changes {
        limit
    } else {
        None
    };
    if let Some(limit) = limit
        && !running_unchanged
    {
        let node = expanded.nodes.iter().find(|node| match limit.key {
            GraphLimitKey::NodePromptBytes => node.content_bytes() > limit.max,
            GraphLimitKey::RoutesPerNode => {
                node.routes
                    .as_ref()
                    .map_or(usize::from(node.route.is_some()), Vec::len)
                    > limit.max
            }
            _ => false,
        });
        if let Some(node) = node.filter(|node| valid_node_identifier(&node.id)) {
            return Err(CandidateError::NodeStructural {
                node: node.id.clone(),
                limit,
            });
        }
        return Err(CandidateError::Structural(limit));
    }
    let bound = graph_envelope_bound(document, expansions, &all, &plan).saturating_add(
        requester_path.map_or(0, |path| path.len().saturating_mul(4).saturating_add(2)),
    );
    if bound > MAX_CHECKPOINT_BYTES as usize {
        return Err(CandidateError::Envelope(GraphEnvelopeLimit { bound }));
    }
    Ok(AdmittedCandidate {
        source: document.clone(),
        checkpoint: checkpoint.cloned(),
        extensions: extensions.to_vec(),
        expanded,
        plan,
        effective_budget,
        growth_limit,
    })
}
