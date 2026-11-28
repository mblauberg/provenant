//! Pure graph admission shared by every publisher. No journal writes here.
use super::refusal::{ExtensionCause, ExtensionRule};
use super::*;
use crate::coordination::{facade::GraphInput, input::ValidateInput};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CandidateError {
    #[error("invalid input field `extend`: extension syntax")]
    Input,
    #[error("{0}")]
    Plan(String),
    #[error("{CLOUD_PLACEMENT_REMOVED}")]
    CloudPlacementRemoved,
    #[error("{0}")]
    Schema(String),
    #[error("{0}")]
    Composite(String),
    #[error("{reason}")]
    OutputKind {
        field: GraphOutputField,
        reason: String,
    },
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
    Extension(ExtensionCause),
    #[error("{0}")]
    Structural(GraphStructuralLimit),
    #[error("node {node}: {limit}")]
    NodeStructural {
        node: refusal::GraphIdentifier,
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
            AdmissionError::Schema(reason) => Self::Schema(reason),
            AdmissionError::Composite(reason) => Self::Composite(reason),
            AdmissionError::OutputKind { field, reason } => Self::OutputKind { field, reason },
            AdmissionError::StructuralLimit(limit) => Self::Structural(limit),
            AdmissionError::Envelope(limit) => Self::Envelope(limit),
            // A plan rejection names the node and the validator reason. Carry
            // that text through so the dispatch refusal can be followed.
            // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
            AdmissionError::Plan(reason) => Self::Plan(reason),
        }
    }
}

pub struct AdmittedCandidate {
    pub(super) source: GraphDocument,
    pub(super) checkpoint: Option<GraphCheckpoint>,
    pub(super) review_policy: ReviewPolicy,
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
    admit_candidate_with_review_policy(
        document,
        checkpoint,
        extensions,
        limits,
        budget,
        requester_path,
        &ReviewPolicy::default(),
    )
}

/// Build and check a candidate with the review settings resolved by its owner.
/// Existing checkpoints always keep their sealed policy on replay.
pub fn admit_candidate_with_review_policy(
    document: &GraphDocument,
    checkpoint: Option<&GraphCheckpoint>,
    extensions: &[String],
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    requester_path: Option<&[u8]>,
    review_policy: &ReviewPolicy,
) -> Result<AdmittedCandidate, CandidateError> {
    admit(
        document,
        checkpoint,
        extensions,
        limits,
        budget,
        requester_path,
        Recheck {
            running_unchanged: false,
            review_policy,
        },
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
        Recheck {
            running_unchanged: true,
            review_policy: &checkpoint.review_policy(),
        },
    )
}

/// How a candidate is admitted: against a fresh request, or as the unchanged
/// running graph, and which review settings apply to a graph with no sealed
/// policy.
struct Recheck<'a> {
    running_unchanged: bool,
    review_policy: &'a ReviewPolicy,
}

fn admit(
    document: &GraphDocument,
    checkpoint: Option<&GraphCheckpoint>,
    extensions: &[String],
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    requester_path: Option<&[u8]>,
    recheck: Recheck<'_>,
) -> Result<AdmittedCandidate, CandidateError> {
    let Recheck {
        running_unchanged,
        review_policy: requested_review_policy,
    } = recheck;
    GraphInput::Resume {
        extra: Default::default(),
        document: document.graph_id.clone().into(),
        extend: extensions.to_vec(),
    }
    .validate_input(provenant_core::WaitTransport::Cli)
    .map_err(|_| CandidateError::Input)?;
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
    validate_delivery_contracts(document).map_err(CandidateError::from_admission)?;
    let mut review_policy = checkpoint
        .map(GraphCheckpoint::review_policy)
        .unwrap_or_else(|| requested_review_policy.clone());
    apply_review_policy_override(
        &mut review_policy,
        document.review_policy.as_ref(),
        provenant_records::Layer::Launch,
        "launch",
    )?;
    let mut expanded = plans::expand_typed(document, &all, expansions, &generous)
        .map_err(CandidateError::from_admission)?;
    resolve_review_policies(&mut expanded, &review_policy)?;
    // Route models are canonicalised once by the host entry point that holds
    // the effective catalogue. Admission reads the stored ids verbatim and
    // never rewrites them. Spec: docs/specs/fabric.md#one-model-spelling-per-route
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
    let topology_changes = if running_unchanged {
        let mut previous =
            plans::expand(document, saved, expansions, &generous).map_err(CandidateError::Plan)?;
        resolve_review_policies(&mut previous, &review_policy)?;
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
        if let Some(node) = node.filter(|node| valid_node_identifier(&node.id))
            && let Ok(node_id) = refusal::GraphIdentifier::try_from(node.id.clone())
        {
            return Err(CandidateError::NodeStructural {
                node: node_id,
                limit,
            });
        }
        return Err(CandidateError::Structural(limit));
    }
    let bound = graph_envelope_bound_with_policy(document, expansions, &all, &plan, &review_policy)
        .saturating_add(
            requester_path.map_or(0, |path| path.len().saturating_mul(4).saturating_add(2)),
        );
    if bound > MAX_CHECKPOINT_BYTES as usize {
        return Err(CandidateError::Envelope(GraphEnvelopeLimit { bound }));
    }
    Ok(AdmittedCandidate {
        source: document.clone(),
        checkpoint: checkpoint.cloned(),
        review_policy,
        extensions: extensions.to_vec(),
        expanded,
        plan,
        effective_budget,
        growth_limit,
    })
}

fn layer_rank(layer: provenant_records::Layer) -> usize {
    provenant_records::Layer::ALL
        .iter()
        .position(|candidate| *candidate == layer)
        .unwrap_or(usize::MAX)
}

fn scope_allows_review_setting(layer: provenant_records::Layer, field: &str) -> bool {
    use provenant_records::Layer;
    match layer {
        Layer::Template => true,
        Layer::Route => matches!(field, "adjudicator_route" | "lens_retry"),
        Layer::Launch => field != "lens_retry",
        _ => false,
    }
}

fn apply_review_field<T>(
    target: &mut provenant_records::Sourced<T>,
    value: T,
    layer: provenant_records::Layer,
    origin: &str,
    field: &str,
) {
    if layer_rank(layer) >= layer_rank(target.source.layer) {
        *target = provenant_records::Sourced {
            value,
            source: provenant_records::Source {
                layer,
                origin: Some(format!("{origin}#domains.review.{field}")),
            },
        };
    }
}

fn apply_review_policy_override(
    policy: &mut ReviewPolicy,
    override_: Option<&ReviewPolicyOverride>,
    layer: provenant_records::Layer,
    origin: &str,
) -> Result<(), CandidateError> {
    let Some(override_) = override_ else {
        return Ok(());
    };
    macro_rules! apply {
        ($field:ident, $name:literal) => {
            if let Some(value) = &override_.$field {
                if !scope_allows_review_setting(layer, $name) {
                    return Err(CandidateError::Schema(format!(
                        "domains.review.{} does not accept the {} scope",
                        $name,
                        match layer {
                            provenant_records::Layer::Template => "template",
                            provenant_records::Layer::Route => "route",
                            provenant_records::Layer::Launch => "launch",
                            _ => "selected",
                        }
                    )));
                }
                apply_review_field(&mut policy.$field, value.clone(), layer, origin, $name);
            }
        };
    }
    apply!(later_rounds, "later_rounds");
    apply!(adjudicator_route, "adjudicator_route");
    apply!(accept_below, "accept_below");
    apply!(evidence_retries, "evidence_retries");
    apply!(lens_retry, "lens_retry");
    policy
        .validate()
        .map_err(|reason| CandidateError::Schema(reason.into()))
}

fn template_review_policy(
    template_library: &std::collections::BTreeMap<String, Template>,
    node: &GraphNode,
) -> Option<(String, ReviewPolicyOverride)> {
    let name = node.template.as_deref()?;
    let policy = template_library
        .get(name)
        .and_then(|template| template.review_policy.clone())?;
    Some((name.to_owned(), policy))
}

fn resolve_review_node(
    template_library: &std::collections::BTreeMap<String, Template>,
    node: &mut GraphNode,
    inherited: &ReviewPolicy,
) -> Result<(), CandidateError> {
    let mut policy = inherited.clone();
    if let Some((template, override_)) = template_review_policy(template_library, node) {
        apply_review_policy_override(
            &mut policy,
            Some(&override_),
            provenant_records::Layer::Template,
            &format!("template:{template}"),
        )?;
    }
    apply_review_policy_override(
        &mut policy,
        node.review_policy.as_ref(),
        provenant_records::Layer::Launch,
        &format!("node:{}", node.id),
    )?;
    node.effective_review_policy = Some(policy.clone());
    if let Some(route) = node.route.as_mut() {
        let mut route_policy = policy.clone();
        apply_review_policy_override(
            &mut route_policy,
            route.review_policy.as_ref(),
            provenant_records::Layer::Route,
            &format!(
                "route:{}",
                route
                    .alias
                    .as_deref()
                    .or(route.name.as_deref())
                    .unwrap_or("unnamed")
            ),
        )?;
        route.effective_review_policy = Some(route_policy);
    }
    if let Some(routes) = node.routes.as_mut() {
        for route in routes {
            let mut route_policy = policy.clone();
            apply_review_policy_override(
                &mut route_policy,
                route.review_policy.as_ref(),
                provenant_records::Layer::Route,
                &format!(
                    "route:{}",
                    route
                        .alias
                        .as_deref()
                        .or(route.name.as_deref())
                        .unwrap_or("unnamed")
                ),
            )?;
            route.effective_review_policy = Some(route_policy);
        }
    }
    if let Some(composite) = node.composite.as_mut() {
        for child in &mut composite.nodes {
            resolve_review_node(template_library, child, &policy)?;
        }
    }
    if let Some(loop_body) = node.loop_body.as_mut() {
        for child in &mut loop_body.body {
            resolve_review_node(template_library, child, &policy)?;
        }
    }
    Ok(())
}

/// The policy one route runs under: the inherited policy with the route's own
/// override applied. A sealed route policy wins; an unsealed route resolves
/// here, so a review decision never depends on whether admission sealed it.
pub(super) fn route_review_policy(
    inherited: &ReviewPolicy,
    route: &GraphRoute,
) -> Result<ReviewPolicy, CandidateError> {
    if let Some(sealed) = &route.effective_review_policy {
        return Ok(sealed.clone());
    }
    let mut policy = inherited.clone();
    apply_review_policy_override(
        &mut policy,
        route.review_policy.as_ref(),
        provenant_records::Layer::Route,
        &format!(
            "route:{}",
            route
                .alias
                .as_deref()
                .or(route.name.as_deref())
                .unwrap_or("unnamed")
        ),
    )?;
    Ok(policy)
}

pub(super) fn resolve_review_policies(
    document: &mut GraphDocument,
    base: &ReviewPolicy,
) -> Result<(), CandidateError> {
    let template_library = &document.template_library;
    for node in &mut document.nodes {
        resolve_review_node(template_library, node, base)?;
    }
    Ok(())
}
