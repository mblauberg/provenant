//! Bounded loop expansion uses the ordinary scheduler, attempts and journal.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub body: Vec<GraphNode>,
    pub until: String,
    pub max_rounds: usize,
    /// Explicit initial nodes for `previous.<body-node>` bindings in round one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub previous_bindings: BTreeMap<String, String>,
    /// Typed delivery outcome edge; the node id names the loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_edge: Option<provenant_records::graph::LoopOutcomeEdge>,
    #[serde(
        default = "default_extension_rounds",
        skip_serializing_if = "is_default_extension_rounds"
    )]
    pub max_extension_rounds: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_session: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review_repair: bool,
    /// Typed gates whose failure admits the next bounded repair round. The
    /// selected gates remain failed in the checkpoint for evidence and replay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retry_on: Vec<String>,
    /// Local publication whose exact receipt is exported by a successful loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_publication: Option<String>,
    /// Initial publication selected when all entry gates pass without repair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_publication: Option<String>,
}

fn default_extension_rounds() -> usize {
    5
}
fn is_default_extension_rounds(value: &usize) -> bool {
    *value == 5
}

pub(super) fn tier_route(base: Option<&GraphRoute>, alias: &str) -> GraphRoute {
    let mut route = base.cloned().unwrap_or_default();
    route.model = None;
    route.name = None;
    route.adapter = None;
    route.alias = Some(alias.to_owned());
    route.tier = Some(alias.to_owned());
    route
}

/// Nearest writer ancestor through delivery gates, hygiene and publication
/// nodes. Dependencies are traversed breadth-first; ties use plan order so the
/// most recently declared upstream writer owns the route.
pub(super) fn nearest_writer_ancestor(node_index: usize, plan: &PlannedGraph) -> Option<usize> {
    let node = plan.nodes.get(node_index)?;
    let mut frontier: Vec<usize> = node
        .dependencies()
        .filter_map(|dependency| {
            plan.nodes
                .iter()
                .position(|candidate| candidate.id == *dependency)
        })
        .collect();
    let mut visited = std::collections::BTreeSet::new();
    while !frontier.is_empty() {
        frontier.sort_unstable();
        frontier.dedup();
        frontier.retain(|index| visited.insert(*index));
        if let Some(writer) = frontier
            .iter()
            .rev()
            .copied()
            .find(|index| plans::writer(&plan.nodes[*index]))
        {
            return Some(writer);
        }
        frontier = frontier
            .into_iter()
            .flat_map(|index| {
                plan.nodes[index]
                    .dependencies()
                    .filter_map(|dependency| {
                        plan.nodes
                            .iter()
                            .position(|candidate| candidate.id == *dependency)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    None
}

pub(super) fn escalation_owner(
    node_index: usize,
    plan: &PlannedGraph,
) -> Option<(usize, &EscalateSpec)> {
    let node = plan.nodes.get(node_index)?;
    if let Some(round) = &node.round
        && let Some((index, owner)) = plan
            .nodes
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.id == round.loop_id)
        && let Some(spec) = owner.escalate.as_ref()
    {
        return Some((index, spec));
    }
    if plans::writer(node)
        && let Some(spec) = node.escalate.as_ref()
    {
        return Some((node_index, spec));
    }
    let writer_index = nearest_writer_ancestor(node_index, plan)?;
    let writer = &plan.nodes[writer_index];
    if let Some(round) = &writer.round
        && let Some((owner_index, owner)) = plan
            .nodes
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.id == round.loop_id)
        && let Some(spec) = owner.escalate.as_ref()
    {
        return Some((owner_index, spec));
    }
    writer.escalate.as_ref().map(|spec| (writer_index, spec))
}

pub(super) fn escalation_rung(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> Option<usize> {
    let (owner, spec) = escalation_owner(node_index, plan)?;
    let rung = checkpoint
        .nodes
        .get(owner)?
        .escalation
        .as_ref()
        .map_or(0, |state| state.rung);
    (rung < spec.ladder.len()).then_some(rung)
}

pub(super) fn delivery_route(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> Option<(GraphRoute, usize)> {
    let node = plan.nodes.get(node_index)?;
    if node.access != AccessMode::WorktreeWrite
        || matches!(node.kind, Some(NodeKind::Hygiene | NodeKind::Publish))
    {
        return None;
    }
    let (_, spec) = escalation_owner(node_index, plan)?;
    let rung = escalation_rung(node_index, plan, checkpoint)?;
    let alias = spec.ladder.get(rung)?;
    Some((tier_route(node.routes.first(), alias), rung))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRound {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub loop_id: String,
    pub round: usize,
    pub node_id: String,
    pub until: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retry_on: bool,
    pub previous_gate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopDecision {
    Stop,
    Extend { rounds: usize },
    Accept,
}

#[derive(Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
enum LoopReply {
    Stop {},
    Extend { rounds: usize },
    Accept {},
}

/// An answer that cannot select a loop decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}", asks::LOOP_ANSWER_HELP)]
pub struct InvalidLoopDecision;

impl InvalidLoopDecision {
    pub fn code(&self) -> &'static str {
        "invalid_loop_decision"
    }
}

impl LoopDecision {
    /// Read a loop ask's answer: an option, or a JSON reply to extend by K rounds.
    pub fn parse(option: Option<&str>, text: Option<&str>) -> Result<Self, InvalidLoopDecision> {
        match (option, text) {
            (Some("stop"), None) => Ok(Self::Stop),
            (Some("extend"), None) => Ok(Self::Extend { rounds: 1 }),
            (Some("accept"), None) => Ok(Self::Accept),
            (None, Some(text)) => match serde_json::from_str(text) {
                Ok(LoopReply::Stop {}) => Ok(Self::Stop),
                Ok(LoopReply::Extend { rounds }) if rounds > 0 => Ok(Self::Extend { rounds }),
                Ok(LoopReply::Accept {}) => Ok(Self::Accept),
                _ => Err(InvalidLoopDecision),
            },
            _ => Err(InvalidLoopDecision),
        }
    }
}

fn id(owner: &str, round: usize, node: &str) -> String {
    format!("{owner}--r{round}--{node}")
}

fn validate_previous_bindings(
    node: &GraphNode,
    spec: &LoopSpec,
    document: &GraphDocument,
) -> Result<(), String> {
    let body_ids: std::collections::BTreeSet<_> =
        spec.body.iter().map(|member| member.id.clone()).collect();
    let mut references = std::collections::BTreeSet::new();
    let mut published_bindings = std::collections::BTreeSet::new();
    for member in &spec.body {
        for input in &member.inputs {
            if let Some(previous) = input.node.strip_prefix("previous.") {
                if spec.previous_bindings.contains_key(previous) {
                    references.insert(previous.to_owned());
                }
            }
        }
        match member.source.as_ref() {
            Some(SourceBinding::Head { node: source }) => {
                if let Some(previous) = source.strip_prefix("previous.") {
                    references.insert(previous.to_owned());
                    published_bindings.insert(previous.to_owned());
                }
            }
            Some(SourceBinding::Node { node: source }) => {
                if let Some(previous) = source.strip_prefix("previous.") {
                    references.insert(previous.to_owned());
                }
            }
            _ => {}
        }
        if let Some(
            GateCondition::Ci {
                target: CiTarget::Node { node: target, .. },
                ..
            }
            | GateCondition::Conflict {
                target: CiTarget::Node { node: target, .. },
                ..
            },
        ) = member.condition.as_ref()
            && let Some(previous) = target.strip_prefix("previous.")
        {
            references.insert(previous.to_owned());
            published_bindings.insert(previous.to_owned());
        }
    }
    for previous in &references {
        if !body_ids.contains(previous) {
            return Err(format!("unknown previous round node {previous}"));
        }
        if !spec.previous_bindings.contains_key(previous) {
            return Err(format!(
                "previous.{previous} requires an explicit loop.previous_bindings entry"
            ));
        }
    }
    for (body_node, initial_node) in &spec.previous_bindings {
        if !body_ids.contains(body_node) || !references.contains(body_node) {
            return Err(format!(
                "loop.previous_bindings entry {body_node} must name a referenced body node"
            ));
        }
        let Some(initial) = document
            .nodes
            .iter()
            .find(|candidate| candidate.id == *initial_node)
        else {
            return Err(format!(
                "loop.previous_bindings entry {body_node} names unknown outer node {initial_node}"
            ));
        };
        if body_ids.contains(initial_node) || !node.depends_on.contains(initial_node) {
            return Err(format!(
                "loop.previous_bindings entry {body_node} must bind a direct outer dependency"
            ));
        }
        if published_bindings.contains(body_node) && initial.kind != Some(NodeKind::Publish) {
            return Err(format!(
                "loop.previous_bindings entry {body_node} must bind a publication node"
            ));
        }
    }
    Ok(())
}

fn previous_round_node(
    spec: &LoopSpec,
    loop_id: &str,
    round: usize,
    body_node: &str,
) -> Result<String, String> {
    if round > 1 {
        Ok(id(loop_id, round - 1, body_node))
    } else {
        spec.previous_bindings
            .get(body_node)
            .cloned()
            .ok_or_else(|| {
                format!("previous.{body_node} requires an explicit loop.previous_bindings entry")
            })
    }
}

fn allowance(node: &GraphNode, extensions: &[String]) -> usize {
    let prefix = format!("loop.{}.rounds=", node.id);
    extensions
        .iter()
        .filter_map(|e| e.strip_prefix(&prefix)?.parse::<usize>().ok())
        .fold(
            node.loop_body.as_ref().map_or(0, |s| s.max_rounds),
            usize::max,
        )
}

/// Count the nodes produced by loop expansion without requiring late-bound
/// execution context, using the same allowances as the actual expansion.
pub(super) fn expanded_node_count(
    document: &GraphDocument,
    extensions: &[String],
) -> Result<usize, String> {
    for node in &document.nodes {
        let Some(spec) = &node.loop_body else {
            continue;
        };
        if spec.max_rounds == 0 {
            return Err(format!(
                "node {}: loop.max_rounds must be positive",
                node.id
            ));
        }
        if spec.max_extension_rounds == 0 {
            return Err(format!(
                "node {}: loop.max_extension_rounds must be positive",
                node.id
            ));
        }
        if let Some(reason) = crate::config::GraphLimits::loop_extension_rounds_bound()
            .refusal(spec.max_extension_rounds)
        {
            return Err(format!(
                "node {}: loop.max_extension_rounds: {reason}",
                node.id
            ));
        }
        if let Some(reason) = crate::config::GraphLimits::loop_expansion_rounds_bound()
            .refusal(allowance(node, extensions))
        {
            return Err(reason);
        }
    }
    let count = document.nodes.iter().try_fold(0usize, |total, node| {
        let count = match &node.loop_body {
            Some(spec) => spec
                .body
                .len()
                .checked_mul(allowance(node, extensions))?
                .checked_add(1)?,
            None => 1,
        };
        total.checked_add(count)
    });
    let count = count.unwrap_or(usize::MAX);
    if document.nodes.iter().any(|node| node.loop_body.is_some())
        && let Some(reason) =
            crate::config::GraphLimits::loop_expansion_nodes_bound().refusal(count)
    {
        return Err(reason);
    }
    Ok(count)
}

pub(super) fn validate_extension(
    document: &GraphDocument,
    extension: &str,
    limits: &crate::config::GraphLimits,
) -> Result<(), String> {
    let flattened = composites::expand(document, limits).map_err(|error| error.to_string())?;
    let document = &flattened;
    let (key, value) = extension.split_once('=').ok_or("expected KEY=VALUE")?;
    let key = key.strip_prefix("loop.").ok_or("invalid loop extension")?;
    let (name, field) = key.rsplit_once('.').ok_or("invalid loop extension")?;
    let node = document
        .nodes
        .iter()
        .find(|n| n.id == name && n.loop_body.is_some())
        .ok_or("unknown loop")?;
    if node.loop_body.as_ref().is_some_and(repair::spec_is_repair) {
        return Err("review repair cycles cannot be extended or accepted without review".into());
    }
    match field {
        "rounds" if value.parse::<usize>().is_ok_and(|n| n > node.loop_body.as_ref().map_or(0, |s| s.max_rounds) && n <= 1024) => Ok(()),
        "decision" if matches!(value, "stop" | "accept") => Ok(()),
        _ => Err("expected loop.ID.rounds=N (larger absolute allowance, at most 1024) or loop.ID.decision=stop|accept".into()),
    }
}

pub(super) fn validate_resume_extension(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    extension: &str,
    limits: &crate::config::GraphLimits,
) -> Result<(), refusal::ExtensionRule> {
    validate_extension(document, extension, limits).map_err(|_| refusal::ExtensionRule::Value)?;
    let flattened =
        composites::expand(document, limits).map_err(|_| refusal::ExtensionRule::Value)?;
    let document = &flattened;
    if checkpoint.extensions.iter().any(|saved| saved == extension) {
        return Ok(());
    }
    let (key, value) = extension
        .split_once('=')
        .ok_or(refusal::ExtensionRule::Syntax)?;
    let (name, field) = key
        .strip_prefix("loop.")
        .and_then(|key| key.rsplit_once('.'))
        .ok_or(refusal::ExtensionRule::Key)?;
    if checkpoint
        .extensions
        .iter()
        .any(|e| e.starts_with(&format!("loop.{name}.decision=")))
    {
        return Err(refusal::ExtensionRule::TerminalDecision);
    }
    let progress = checkpoint
        .nodes
        .iter()
        .find(|n| n.id == name)
        .ok_or(refusal::ExtensionRule::Key)?;
    let body: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter(|n| n.round.as_ref().is_some_and(|r| r.loop_id == name))
        .collect();
    if progress.state != NodeState::Pending
        || body.is_empty()
        || body.iter().any(|n| {
            !n.state.settled()
                || n.round.as_ref().is_some_and(|r| r.until) && n.state != NodeState::Failed
                || n.round.as_ref().is_some_and(|r| !r.until) && n.state != NodeState::Succeeded
        })
    {
        return Err(refusal::ExtensionRule::Exhaustion);
    }
    if field == "rounds" {
        let original = document
            .nodes
            .iter()
            .find(|n| n.id == name)
            .ok_or(refusal::ExtensionRule::Key)?;
        let current = allowance(original, &checkpoint.extensions);
        let target = value
            .parse::<usize>()
            .map_err(|_| refusal::ExtensionRule::Value)?;
        if target <= current {
            return Err(refusal::ExtensionRule::NonIncreasing);
        }
        if target - current
            > original
                .loop_body
                .as_ref()
                .map_or(5, |spec| spec.max_extension_rounds)
        {
            return Err(refusal::ExtensionRule::MaxExtensionRounds);
        }
    }
    Ok(())
}

pub(super) fn expand(
    document: &GraphDocument,
    extensions: &[String],
    limits: &crate::config::GraphLimits,
) -> Result<GraphDocument, AdmissionError> {
    let flattened = composites::expand(document, limits)?;
    check_expanded_ids(&flattened, extensions).map_err(AdmissionError::Composite)?;
    expand_loops(&flattened, extensions).map_err(AdmissionError::Composite)
}

/// Check the longest round ID before lowering, using the same spelling and
/// validation as execution. A later round cannot have fewer decimal digits.
pub(super) fn check_expanded_ids(
    document: &GraphDocument,
    extensions: &[String],
) -> Result<(), String> {
    for node in &document.nodes {
        let Some(spec) = &node.loop_body else {
            continue;
        };
        let rounds = allowance(node, extensions);
        if !(1..=1024).contains(&rounds) {
            continue;
        }
        for child in &spec.body {
            if !valid_node_identifier(&id(&node.id, rounds, &child.id)) {
                return Err("expanded loop node id exceeds 200 bytes".into());
            }
        }
    }
    Ok(())
}

fn expand_loops(document: &GraphDocument, extensions: &[String]) -> Result<GraphDocument, String> {
    let declared: std::collections::BTreeSet<_> = document.nodes.iter().map(|n| &n.id).collect();
    if document.nodes.iter().any(|n| {
        n.depends_on
            .iter()
            .chain(&n.after_settled)
            .any(|parent| !declared.contains(parent))
    }) {
        return Err("dependency".into());
    }
    let outcome_edges: Vec<_> = document
        .nodes
        .iter()
        .filter_map(|node| {
            node.loop_body
                .as_ref()?
                .outcome_edge
                .as_ref()
                .map(|edge| (node.id.clone(), edge.node.clone()))
        })
        .collect();
    for (loop_id, target) in &outcome_edges {
        if target == loop_id || !declared.contains(target) {
            return Err("loop outcome edge target must name a graph node outside its body".into());
        }
    }
    expanded_node_count(document, extensions)?;
    let mut expanded = document.clone();
    expanded.nodes.clear();
    for node in &document.nodes {
        let Some(spec) = &node.loop_body else {
            if node.kind == Some(NodeKind::Loop) {
                return Err("loop requires a body".into());
            }
            let mut node = node.clone();
            if let Some(escalation) = &node.escalate
                && node.access == AccessMode::WorktreeWrite
                && let Some(alias) = escalation.ladder.first()
            {
                let base = node
                    .route
                    .as_ref()
                    .or_else(|| node.routes.as_ref().and_then(|routes| routes.first()));
                node.route = Some(tier_route(base, alias));
                node.routes = None;
            }
            expanded.nodes.push(node);
            continue;
        };
        if node.kind != Some(NodeKind::Loop)
            || node.access != AccessMode::ReadOnly
            || node.worktree.is_some()
            || node.delivery.is_some()
            || node.condition.is_some()
            || node.on_fail.is_some() && !repair::spec_is_repair(spec)
            || node.route.is_some()
            || node.routes.is_some()
            || !node.prompt.is_empty()
            || node.required_verdict.is_some()
            || spec.body.is_empty()
            || spec
                .decision_session
                .as_ref()
                .is_some_and(|s| crate::inbox::session_id(s).is_err())
        {
            return Err("invalid loop configuration".into());
        }
        validate_previous_bindings(node, spec, document)?;
        let rounds = allowance(node, extensions);
        let mut body = spec.body.clone();
        let previous_head_sources: BTreeMap<_, _> = body
            .iter()
            .filter_map(|child| {
                let Some(SourceBinding::Head { node: source }) = &child.source else {
                    return None;
                };
                source
                    .strip_prefix("previous.")
                    .map(|source| (child.id.clone(), source.to_owned()))
            })
            .collect();
        let previous_node_sources: BTreeMap<_, _> = body
            .iter()
            .filter_map(|child| {
                let Some(SourceBinding::Node { node: source }) = &child.source else {
                    return None;
                };
                source
                    .strip_prefix("previous.")
                    .map(|source| (child.id.clone(), source.to_owned()))
            })
            .collect();
        let previous_gate_targets: BTreeMap<_, _> = body
            .iter()
            .filter_map(|child| {
                let (target, extra) = match child.condition.as_ref()? {
                    GateCondition::Ci {
                        target: CiTarget::Node { node, extra },
                        ..
                    }
                    | GateCondition::Conflict {
                        target: CiTarget::Node { node, extra },
                        ..
                    } => (node, extra),
                    _ => return None,
                };
                target
                    .strip_prefix("previous.")
                    .map(|target| (child.id.clone(), (target.to_owned(), extra.clone())))
            })
            .collect();
        if let Some(initial) = spec.initial_publication.as_deref() {
            let valid = !spec.retry_on.is_empty()
                && spec.selected_publication.as_deref().is_some_and(|local| {
                    spec.previous_bindings
                        .get(local)
                        .is_some_and(|seed| seed == initial)
                })
                && node
                    .depends_on
                    .iter()
                    .any(|dependency| dependency == initial)
                && document.nodes.iter().any(|candidate| {
                    candidate.id == initial && candidate.kind == Some(NodeKind::Publish)
                })
                && node.depends_on.iter().any(|dependency| {
                    document.nodes.iter().any(|candidate| {
                        candidate.id == *dependency
                            && candidate.on_fail.as_deref() == Some(&node.id)
                    })
                });
            if !valid {
                return Err(format!(
                    "loop {} initial_publication must be its directly bound publication seed and have an entry failure gate",
                    node.id
                ));
            }
        }
        let mut retry_nodes = std::collections::BTreeSet::new();
        for retry in &spec.retry_on {
            let Some(gate) = spec.body.iter().find(|member| member.id == *retry) else {
                return Err(format!("loop {} retry_on must name a body gate", node.id));
            };
            if !retry_nodes.insert(retry)
                || gate.kind != Some(NodeKind::Gate)
                || !matches!(
                    &gate.condition,
                    Some(
                        GateCondition::Ci { .. }
                            | GateCondition::Conflict { .. }
                            | GateCondition::Verdict { .. }
                    )
                )
            {
                return Err(format!(
                    "loop {} retry_on must uniquely name typed CI, conflict or verdict gates",
                    node.id
                ));
            }
        }
        if let Some(publication) = spec.selected_publication.as_deref()
            && !spec
                .body
                .iter()
                .any(|member| member.id == publication && member.kind == Some(NodeKind::Publish))
        {
            return Err(format!(
                "loop {} selected_publication must name a typed publication in its body",
                node.id
            ));
        }
        let external_head_sources: BTreeMap<_, _> = body
            .iter()
            .filter_map(|child| {
                let Some(SourceBinding::Head { node: source }) = &child.source else {
                    return None;
                };
                (!source.starts_with("previous.")
                    && !spec.body.iter().any(|member| &member.id == source))
                .then(|| (child.id.clone(), source.clone()))
            })
            .collect();
        let external_node_sources: BTreeMap<_, _> = body
            .iter()
            .filter_map(|child| {
                let Some(SourceBinding::Node { node: source }) = &child.source else {
                    return None;
                };
                (!source.starts_with("previous.")
                    && !spec.body.iter().any(|member| &member.id == source))
                .then(|| (child.id.clone(), source.clone()))
            })
            .collect();
        for child in &mut body {
            if child.access != AccessMode::WorktreeWrite {
                continue;
            }
            if let Some(escalation) = child.escalate.as_ref().or(node.escalate.as_ref())
                && let Some(alias) = escalation.ladder.first()
            {
                let base = child
                    .route
                    .as_ref()
                    .or_else(|| child.routes.as_ref().and_then(|routes| routes.first()));
                child.route = Some(tier_route(base, alias));
                child.routes = None;
            }
        }
        let mut writer = None;
        for child in &mut body {
            if child.id.starts_with("previous.") {
                return Err("previous. is reserved for prior round inputs".into());
            }
            let mut selected = std::collections::BTreeSet::new();
            for input in &child.inputs {
                if let Some(previous) = input.node.strip_prefix("previous.")
                    && !input.required_outputs.is_empty()
                    && !spec.previous_bindings.contains_key(previous)
                {
                    return Err(
                        "required previous-round outputs cannot be enforced in the first round"
                            .into(),
                    );
                }
                let mut kinds = Vec::new();
                if !selected.insert(&input.node) || input.outputs.is_empty() {
                    return Err("duplicate or empty loop input".into());
                }
                for kind in &input.outputs {
                    if kinds.contains(kind) {
                        return Err("duplicate input output".into());
                    }
                    kinds.push(*kind);
                }
            }
            if child.loop_body.is_some()
                || child.kind == Some(NodeKind::Loop)
                || child.delivery.as_ref().is_some_and(|mode| {
                    !matches!(mode, provenant_records::run::DeliveryMode::Integrate(_))
                })
            {
                return Err(
                    "loop body cannot nest loops and may only deliver by integrate:".into(),
                );
            }
            if child
                .on_fail
                .as_ref()
                .is_some_and(|target| !spec.body.iter().any(|member| &member.id == target))
            {
                return Err("loop on_fail target must name a node in the same round".into());
            }
            if let Some(provenant_records::run::DeliveryMode::Integrate(target)) = &child.delivery
                && document.integration_branch.as_deref() != Some(target.as_str())
            {
                return Err("loop body integrate: requires graph integration_branch equal to the node target".into());
            }
            if child.land.is_some() {
                return Err("loop body cannot land".into());
            }
            // A continued writer is outside the body; the expanded plan checks it.
            child.continues = None;
            if child.access == AccessMode::WorktreeWrite {
                if writer
                    .as_ref()
                    .is_some_and(|path| Some(path) != child.worktree.as_ref())
                {
                    return Err("loop writers must share one worktree".into());
                }
                writer = child.worktree.clone();
            }
            for input in &child.inputs {
                if let Some(previous) = input.node.strip_prefix("previous.")
                    && !spec.body.iter().any(|n| n.id == previous)
                {
                    return Err("unknown previous round input".into());
                }
            }
            // Prior-round and outer inputs are checked against the expanded
            // graph; the isolated body check has only the local namespace.
            child
                .inputs
                .retain(|i| spec.body.iter().any(|node| node.id == i.node));
            // A head source naming a writer outside the body is checked
            // against the expanded graph, as an external CI subject is.
            if let Some(SourceBinding::Head { node: writer }) = &child.source
                && (writer.starts_with("previous.")
                    || !spec.body.iter().any(|node| &node.id == writer))
            {
                child.source = None;
            }
            // A node source outside the body is checked against the expanded
            // graph; the isolated body has only its local namespace.
            if let Some(SourceBinding::Node { node: source }) = &child.source
                && (source.starts_with("previous.")
                    || !spec.body.iter().any(|node| &node.id == source))
            {
                child.source = None;
            }
            // External CI subjects are checked against the expanded graph.
            // The isolated body check below has only the local namespace.
            if let Some(GateCondition::Ci { target, .. } | GateCondition::Conflict { target, .. }) =
                &mut child.condition
                && let CiTarget::Node {
                    node: target_node, ..
                } = target
                && (target_node.starts_with("previous.")
                    || !spec.body.iter().any(|node| &node.id == target_node))
            {
                *target = CiTarget::Ref(target_node.clone());
            }
        }
        let body_document = GraphDocument {
            // Outer publication bindings are checked after expansion; the
            // isolated body has only local names and uses temporary refs.
            queue: None,
            review: None,
            review_subject_missing: false,
            nodes: body,
            delivery: None,
            ..document.clone()
        };
        let generous = crate::config::GraphLimits {
            nodes: crate::config::GraphLimits::loop_expansion_nodes_bound().maximum,
            edges: usize::MAX,
            graph_prompt_bytes: usize::MAX,
            node_prompt_bytes: usize::MAX,
            routes_per_node: u16::MAX as usize,
            ..Default::default()
        };
        let (plan, positions) =
            validate_flat_plan(&body_document, &generous).map_err(|error| error.to_string())?;
        let Some(gate) = positions.get(&spec.until).map(|i| &plan.nodes[*i]) else {
            return Err("loop until must name a body gate".into());
        };
        if gate.kind != Some(NodeKind::Gate) {
            return Err("loop until must name a body gate".into());
        }
        let mut ancestors = std::collections::BTreeSet::new();
        let mut pending = vec![spec.until.clone()];
        while let Some(name) = pending.pop() {
            if ancestors.insert(name.clone()) {
                pending.extend(plan.nodes[positions[&name]].dependencies().cloned());
            }
        }
        if ancestors.len() != spec.body.len() {
            return Err("loop gate must follow the entire body".into());
        }
        for round in 1..=rounds {
            for child in &spec.body {
                let mut child = child.clone();
                let local = child.id.clone();
                if child.source.is_none()
                    && let Some(source) = previous_head_sources.get(&local)
                {
                    child.source = Some(SourceBinding::Head {
                        node: previous_round_node(spec, &node.id, round, source)?,
                    });
                }
                if child.source.is_none()
                    && let Some(source) = external_head_sources.get(&local)
                {
                    child.source = Some(SourceBinding::Head {
                        node: source.clone(),
                    });
                }
                child.id = id(&node.id, round, &local);
                child.round = Some(LoopRound {
                    extra: Default::default(),
                    loop_id: node.id.clone(),
                    round,
                    node_id: local.clone(),
                    until: local == spec.until,
                    retry_on: spec.retry_on.iter().any(|retry| retry == &local),
                    previous_gate: (round > 1).then(|| id(&node.id, round - 1, &spec.until)),
                });
                child.depends_on = child
                    .depends_on
                    .iter()
                    .map(|name| id(&node.id, round, name))
                    .collect();
                child.on_fail = child
                    .on_fail
                    .as_deref()
                    .map(|target| id(&node.id, round, target));
                child.after_settled = child
                    .after_settled
                    .iter()
                    .map(|name| id(&node.id, round, name))
                    .collect();
                child.after_expansion = child
                    .after_expansion
                    .iter()
                    .map(|name| id(&node.id, round, name))
                    .collect();
                child.composite_members = child
                    .composite_members
                    .iter()
                    .map(|name| id(&node.id, round, name))
                    .collect();
                if let Some(composite) = &mut child.composite {
                    for export in &mut composite.exports {
                        export.node = id(&node.id, round, &export.node);
                    }
                }
                // The first local node inherits the loop's entry gates (or
                // the prior round's outcome gate) before source and input
                // edges are added below.
                if child.depends_on.is_empty() && child.after_settled.is_empty() {
                    if round == 1 {
                        child.depends_on.extend(node.depends_on.clone());
                        child.after_settled.extend(node.after_settled.clone());
                    } else {
                        let previous = id(&node.id, round - 1, &spec.until);
                        if spec.retry_on.is_empty() || repair::spec_is_repair(spec) {
                            child.depends_on.push(previous);
                        } else {
                            child.after_settled.push(previous);
                        }
                    }
                }
                if child.source.is_none()
                    && let Some(source) = previous_node_sources.get(&local)
                {
                    child.source = Some(SourceBinding::Node {
                        node: previous_round_node(spec, &node.id, round, source)?,
                    });
                }
                if child.source.is_none()
                    && let Some(source) = external_node_sources.get(&local)
                {
                    child.source = Some(SourceBinding::Node {
                        node: source.clone(),
                    });
                }
                if let Some(SourceBinding::Node { node: source }) = &mut child.source {
                    *source = if let Some(previous) = source.strip_prefix("previous.") {
                        previous_round_node(spec, &node.id, round, previous)?
                    } else if spec.body.iter().any(|member| &member.id == source) {
                        id(&node.id, round, source)
                    } else {
                        source.clone()
                    };
                    if !child.depends_on.contains(source) {
                        child.depends_on.push(source.clone());
                    }
                }
                if let Some(SourceBinding::Head { node: writer }) = &mut child.source {
                    *writer = if let Some(previous) = writer.strip_prefix("previous.") {
                        previous_round_node(spec, &node.id, round, previous)?
                    } else if spec.body.iter().any(|body_node| &body_node.id == writer) {
                        id(&node.id, round, writer)
                    } else {
                        writer.clone()
                    };
                    if !child.depends_on.contains(writer) {
                        child.depends_on.push(writer.clone());
                    }
                }
                if let Some((previous, extra)) = previous_gate_targets.get(&local) {
                    let target = previous_round_node(spec, &node.id, round, previous)?;
                    match child.condition.as_mut() {
                        Some(GateCondition::Ci {
                            target: binding, ..
                        })
                        | Some(GateCondition::Conflict {
                            target: binding, ..
                        }) => {
                            *binding = CiTarget::Node {
                                node: target,
                                extra: extra.clone(),
                            };
                        }
                        _ => {
                            return Err(
                                "previous gate target requires a CI or conflict gate".into()
                            );
                        }
                    }
                }
                if let Some(GateCondition::Ci {
                    target: CiTarget::Node { node: target, .. },
                    ..
                }) = &mut child.condition
                {
                    *target = if let Some(previous) = target.strip_prefix("previous.") {
                        previous_round_node(spec, &node.id, round, previous)?
                    } else if spec.body.iter().any(|node| &node.id == target) {
                        id(&node.id, round, target)
                    } else {
                        target.clone()
                    };
                }
                if let Some(GateCondition::Conflict {
                    target: CiTarget::Node { node: target, .. },
                    ..
                }) = &mut child.condition
                {
                    *target = if let Some(previous) = target.strip_prefix("previous.") {
                        previous_round_node(spec, &node.id, round, previous)?
                    } else if spec.body.iter().any(|node| &node.id == target) {
                        id(&node.id, round, target)
                    } else {
                        target.clone()
                    };
                }
                if let Some(
                    GateCondition::Ci {
                        target: CiTarget::Node { node: target, .. },
                        ..
                    }
                    | GateCondition::Conflict {
                        target: CiTarget::Node { node: target, .. },
                        ..
                    },
                ) = child.condition.as_ref()
                    && !child.depends_on.contains(target)
                {
                    child.depends_on.push(target.clone());
                }
                child.inputs.retain(|input| {
                    round > 1
                        || !input.node.starts_with("previous.")
                        || spec
                            .previous_bindings
                            .contains_key(input.node.trim_start_matches("previous."))
                });
                for input in &mut child.inputs {
                    let previous_round_input = input.node.starts_with("previous.");
                    input.node = if let Some(previous) = input.node.strip_prefix("previous.") {
                        previous_round_node(spec, &node.id, round, previous)?
                    } else if spec.body.iter().any(|body| body.id == input.node) {
                        id(&node.id, round, &input.node)
                    } else {
                        // An outer node, such as the writer a repair continues,
                        // is selected afresh in every round.
                        input.node.clone()
                    };
                    if !child.depends_on.contains(&input.node)
                        && !child.after_settled.contains(&input.node)
                    {
                        if previous_round_input
                            && !spec.retry_on.is_empty()
                            && !repair::spec_is_repair(spec)
                        {
                            child.after_settled.push(input.node.clone());
                        } else {
                            child.depends_on.push(input.node.clone());
                        }
                    }
                }
                if repair::spec_is_repair(spec) && child.repair_lens.is_some() {
                    // Select from the findings being repaired in this round.
                    let previous = ["synthesis", "check"]
                        .into_iter()
                        .find(|name| spec.body.iter().any(|body| body.id == *name));
                    let selections = if round > 1
                        && let Some(previous) = previous
                    {
                        vec![GraphInput {
                            extra: Default::default(),
                            node: id(&node.id, round - 1, previous),
                            outputs: vec![OutputKind::Findings],
                            required_outputs: vec![],
                        }]
                    } else {
                        node.inputs.clone()
                    };
                    for input in &selections {
                        if !child.inputs.iter().any(|own| own.node == input.node) {
                            child.inputs.push(input.clone());
                        }
                        if !child.depends_on.contains(&input.node) {
                            child.depends_on.push(input.node.clone());
                        }
                    }
                }
                if round == 1
                    && spec
                        .body
                        .iter()
                        .find(|n| n.id == local)
                        .is_some_and(|n| n.depends_on.is_empty() && n.after_settled.is_empty())
                {
                    child.skip_without_findings =
                        node.skip_without_findings.or(child.skip_without_findings);
                    // The loop's own inputs merge with a body selection of
                    // the same outer node.
                    for input in &node.inputs {
                        if let Some(own) =
                            child.inputs.iter_mut().find(|own| own.node == input.node)
                        {
                            for kind in &input.outputs {
                                if !own.outputs.contains(kind) {
                                    own.outputs.push(*kind);
                                }
                            }
                            for kind in &input.required_outputs {
                                if !own.required_outputs.contains(kind) {
                                    own.required_outputs.push(*kind);
                                }
                            }
                        } else {
                            child.inputs.push(input.clone());
                        }
                    }
                }
                expanded.nodes.push(child);
            }
        }
        let mut boundary = node.clone();
        boundary.depends_on.push(id(&node.id, rounds, &spec.until));
        expanded.nodes.push(boundary);
    }
    for (loop_id, target_id) in outcome_edges {
        let target = expanded
            .nodes
            .iter_mut()
            .find(|node| node.id == target_id)
            .ok_or_else(|| {
                "loop outcome edge target must name a graph node outside its body".to_owned()
            })?;
        if !target.depends_on.contains(&loop_id) {
            target.depends_on.push(loop_id);
        }
    }
    Ok(expanded)
}

/// The selected branch, if `parent` is the loop boundary for `child`.
pub(super) fn outcome_edge_for(
    child: &PlannedNode,
    parent: &PlannedNode,
) -> Option<provenant_records::graph::LoopOutcome> {
    parent
        .loop_body
        .as_ref()?
        .outcome_edge
        .as_ref()
        .filter(|edge| edge.node == child.id)
        .map(|edge| edge.outcome)
}

pub(super) fn inactive_round(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    if repair::inactive_round(node, plan, checkpoint, positions) {
        return true;
    }
    let Some(previous) = node
        .round
        .as_ref()
        .and_then(|round| round.previous_gate.as_ref())
    else {
        return false;
    };
    let index = positions[previous];
    if let Some(round) = &node.round
        && let Some(spec) = plan.nodes[positions[&round.loop_id]].loop_body.as_ref()
        && !spec.retry_on.is_empty()
        && !repair::spec_is_repair(spec)
    {
        return match checkpoint.nodes[index].state {
            NodeState::Succeeded => true,
            NodeState::Failed | NodeState::Blocked | NodeState::Skipped => !plan
                .nodes
                .iter()
                .zip(&checkpoint.nodes)
                .any(|(candidate, progress)| {
                    candidate.round.as_ref().is_some_and(|member| {
                        member.loop_id == round.loop_id
                            && member.round + 1 == round.round
                            && member.retry_on
                    }) && progress.state == NodeState::Failed
                }),
            NodeState::Pending | NodeState::Running => false,
        };
    }
    match checkpoint.nodes[index].state {
        NodeState::Succeeded => true,
        NodeState::Skipped => {
            match conflict_recovery_round_state(index, plan, checkpoint, positions) {
                Some(ConflictRecoveryRoundState::Succeeded) | None => true,
                Some(ConflictRecoveryRoundState::Waiting | ConflictRecoveryRoundState::Retry) => {
                    false
                }
                Some(ConflictRecoveryRoundState::Failed) => true,
            }
        }
        NodeState::Pending | NodeState::Running | NodeState::Failed | NodeState::Blocked => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConflictRecoveryRoundState {
    Waiting,
    Succeeded,
    Retry,
    Failed,
}

/// A skipped until gate normally closes the loop. When a failed conflict
/// selected a validated repair branch, its fresh conflict and CI gates decide
/// whether that skip was an alternative success or a bounded retry.
pub(super) fn conflict_recovery_round_state(
    until_index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Option<ConflictRecoveryRoundState> {
    let round = plan.nodes[until_index].round.as_ref()?;
    let routes: Vec<_> = land::bounded_conflict_rechecks(&plan.nodes, positions)
        .into_iter()
        .filter(|route| {
            route.loop_id == positions[&round.loop_id]
                && route.round == round.round
                && checkpoint.nodes[route.initial_conflict].state == NodeState::Failed
        })
        .collect();
    if routes.is_empty() {
        return None;
    }
    if routes.iter().any(|route| {
        matches!(
            checkpoint.nodes[route.fresh_conflict].state,
            NodeState::Pending | NodeState::Running
        ) || matches!(
            checkpoint.nodes[route.fresh_ci].state,
            NodeState::Pending | NodeState::Running
        )
    }) {
        return Some(ConflictRecoveryRoundState::Waiting);
    }
    if routes
        .iter()
        .all(|route| checkpoint.nodes[route.fresh_ci].state == NodeState::Succeeded)
    {
        Some(ConflictRecoveryRoundState::Succeeded)
    } else if routes.iter().any(|route| {
        checkpoint.nodes[route.fresh_conflict].state == NodeState::Succeeded
            && checkpoint.nodes[route.fresh_ci].state == NodeState::Failed
    }) {
        Some(ConflictRecoveryRoundState::Failed)
    } else {
        Some(ConflictRecoveryRoundState::Retry)
    }
}

pub(super) fn retryable_round_failure_is_terminal(
    index: usize,
    loop_id: &str,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    if !land::retryable_conflict_failure(index, &plan.nodes, positions) {
        return true;
    }
    let Some(round) = plan.nodes[index].round.as_ref() else {
        return true;
    };
    let until_index = plan.nodes.iter().position(|node| {
        node.round.as_ref().is_some_and(|member| {
            member.loop_id == loop_id && member.round == round.round && member.until
        })
    });
    match until_index
        .and_then(|until| conflict_recovery_round_state(until, plan, checkpoint, positions))
    {
        Some(ConflictRecoveryRoundState::Waiting | ConflictRecoveryRoundState::Succeeded) => false,
        Some(ConflictRecoveryRoundState::Failed) => true,
        Some(ConflictRecoveryRoundState::Retry) => !plan.nodes.iter().any(|node| {
            node.round
                .as_ref()
                .is_some_and(|member| member.loop_id == loop_id && member.round > round.round)
        }),
        None => true,
    }
}

pub(super) fn round_failure_edge(child: &PlannedNode, parent: &PlannedNode) -> bool {
    parent.round.as_ref().is_some_and(|r| {
        r.until
            && (child
                .round
                .as_ref()
                .is_some_and(|c| c.loop_id == r.loop_id && c.round == r.round + 1)
                || child.loop_body.is_some() && child.id == r.loop_id)
    })
}

pub(super) fn handled_gate(checkpoint: &GraphCheckpoint, progress: &NodeProgress) -> bool {
    progress.state == NodeState::Failed
        && progress.round.as_ref().is_some_and(|r| {
            (r.until || r.retry_on)
                && checkpoint
                    .nodes
                    .iter()
                    .any(|n| n.id == r.loop_id && checkpoint.node_successful(n))
        })
}

fn passed(node: &PlannedNode, checkpoint: &GraphCheckpoint, plan: &PlannedGraph) -> bool {
    let positions: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    let gate_passed = checkpoint
        .nodes
        .iter()
        .enumerate()
        .any(|(index, progress)| {
            progress
                .round
                .as_ref()
                .is_some_and(|round| round.loop_id == node.id && round.until)
                && (progress.state == NodeState::Succeeded
                    || conflict_recovery_round_state(index, plan, checkpoint, &positions)
                        == Some(ConflictRecoveryRoundState::Succeeded))
        });
    (gate_passed
        || node
            .loop_body
            .as_ref()
            .is_some_and(|spec| spec.initial_publication.is_some())
            && repair::selected_publication(node, checkpoint, plan).is_some())
        && node
            .loop_body
            .as_ref()
            .and_then(|spec| spec.selected_publication.as_deref())
            .is_none_or(|_| repair::selected_publication(node, checkpoint, plan).is_some())
}

fn failed_member(
    progress: &NodeProgress,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    if !matches!(progress.state, NodeState::Failed | NodeState::Blocked) {
        return false;
    }
    let Some(round) = progress.round.as_ref() else {
        return true;
    };
    if round.until || round.retry_on && progress.state == NodeState::Failed {
        return false;
    }
    let Some(index) = positions.get(&progress.id) else {
        return true;
    };
    if plan.nodes[positions[&round.loop_id]]
        .loop_body
        .as_ref()
        .is_none_or(|spec| spec.retry_on.is_empty())
    {
        return retryable_round_failure_is_terminal(
            *index,
            &round.loop_id,
            plan,
            checkpoint,
            positions,
        );
    }
    let triggered = plan
        .nodes
        .iter()
        .zip(&checkpoint.nodes)
        .any(|(gate, evidence)| {
            gate.round.as_ref().is_some_and(|member| {
                member.loop_id == round.loop_id && member.round == round.round && member.retry_on
            }) && evidence.state == NodeState::Failed
                && (gate.id == progress.id
                    || progress.state == NodeState::Blocked
                        && descendant_of(&plan.nodes[*index], &gate.id, plan, positions))
        });
    !triggered
}

fn descendant_of(
    node: &PlannedNode,
    ancestor: &str,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
) -> bool {
    let mut pending = node.dependencies().cloned().collect::<Vec<_>>();
    let mut visited = std::collections::BTreeSet::new();
    while let Some(parent) = pending.pop() {
        if parent == ancestor {
            return true;
        }
        if !visited.insert(parent.clone()) {
            continue;
        }
        if let Some(index) = positions.get(&parent) {
            pending.extend(plan.nodes[*index].dependencies().cloned());
        }
    }
    false
}

pub(super) fn reconcile(checkpoint: &mut GraphCheckpoint, plan: &PlannedGraph) {
    let mut prior: BTreeMap<_, _> = std::mem::take(&mut checkpoint.nodes)
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();
    checkpoint.nodes = plan
        .nodes
        .iter()
        .map(|n| {
            if let Some(mut progress) = prior.remove(&n.id) {
                if let Some(composite) = &n.composite {
                    progress.children.clone_from(&n.composite_members);
                    progress.min_successful = composite.min_successful;
                }
                return progress;
            }
            let mut fresh = NodeProgress {
                delivery_loop: None,
                escalation: None,
                delivery_hygiene: None,
                delivery_publication: None,
                delivery_park: None,
                conflict: None,
                extra: Default::default(),
                removed_requirements: n.removed_requirements.clone(),
                children: n.composite_members.clone(),
                min_successful: n.composite.as_ref().and_then(|spec| spec.min_successful),
                source_binding: n.source.clone(),
                source_commit: match &n.source {
                    Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
                    _ => None,
                },
                writer_binding: None,
                writer_binding_pending: plans::writer(n)
                    && !super::validation::peer_owned_writer(
                        n.access,
                        n.worktree.as_deref(),
                        n.attempt.as_ref(),
                    ),
                review_base: None,
                review_base_pending: n.access == AccessMode::ReadOnly && n.review_base.is_some(),
                owner_notified: false,
                children_cancelled: false,
                detached_from_graph_cancel: false,
                subtree_cancel: None,
                yielded_slot: false,
                stop_fence: false,
                retry: None,
                quota_redraw_spent: 0,
                quota_redraw_archived: 0,
                retry_generation: None,
                container_retry: false,
                concurrency_warning: None,
                repair_growth: None,
                review_verdict_correction_refused: None,
                provisioned_source: None,
                provisioned_owner: None,
                warnings: Vec::new(),
                fragment: None,
                handoff: None,
                blocked: false,
                decisions: Vec::new(),
                review_settlement: None,
                expansion_pause: None,
                expansion_cause: None,
                loop_pause: None,
                loop_pause_code: None,
                loop_cause: None,
                truncation_cause: None,
                round: n.round.clone(),
                gate: None,
                separation: None,
                ci: None,
                land: None,
                title: Some(n.title.as_deref().unwrap_or(&n.id).into()),
                worktree: n.worktree.clone(),
                settled_dependency_snapshots: Default::default(),
                new_worktree_base: None,
                id: n.id.clone(),
                // A node a patch adds is skipped by its own condition, as at
                // submission.
                state: if when::holds(&n.when) {
                    NodeState::Pending
                } else {
                    NodeState::Skipped
                },
                cancelled_from: None,
                wait_reason: None,
                skip_reason: when::skip_reason(&n.when),
                advance_error: None,
                advance_error_code: None,
                advance_owner_code: None,
                launch_retry: None,
                fallback_evidence_retry: None,
                last_launch_retry: None,
                outputs: None,
                previous_attempts: Vec::new(),
                attempts: vec![],
            };
            super::retire::rebind(&checkpoint.expansions, &mut fresh);
            fresh
        })
        .collect();
}

pub(super) fn validate_progress(
    progress: &NodeProgress,
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    plan: &PlannedGraph,
    queue: bool,
) -> Result<(), ()> {
    if checkpoint.ask_actor.is_none()
        || !progress.attempts.is_empty()
        || progress.gate.is_some() && !repair::is_repair(node)
        || progress.advance_error.is_some()
    {
        return Err(());
    }
    let body: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter(|n| n.round.as_ref().is_some_and(|r| r.loop_id == node.id))
        .collect();
    let max_round = body
        .iter()
        .filter_map(|n| n.round.as_ref().map(|r| r.round))
        .max()
        .unwrap_or(0);
    if checkpoint.asks.iter().any(|ask| {
        ask.node.as_deref() == Some(node.id.as_str())
            && ask.rounds.is_none_or(|rounds| rounds > max_round)
    }) {
        return Err(());
    }

    let clean = repair::clean_start(node, checkpoint, positions);
    let escalated = repair::escalation(node, checkpoint, positions);
    let passed = passed(node, checkpoint, plan);
    let failed = body
        .iter()
        .any(|progress| failed_member(progress, plan, checkpoint, positions));
    let settled = body.iter().all(|n| n.state.settled());
    let accepted = checkpoint
        .extensions
        .contains(&format!("loop.{}.decision=accept", node.id));
    let stopped = checkpoint
        .extensions
        .contains(&format!("loop.{}.decision=stop", node.id));
    let queue_exhausted = queue && settled && !passed && !clean && !accepted;
    let matches_output = |passed| {
        let expected = repair::terminal_outputs(node, checkpoint, positions, plan, passed);
        progress.outputs == Some(expected)
    };
    let park_pending = queue
        && progress.delivery_park.as_ref().is_some_and(|park| {
            park.reason == provenant_records::graph::ParkReason::LoopExhausted
                && park.request_id.is_none()
        });
    let park_received = queue
        && progress.delivery_park.as_ref().is_some_and(|park| {
            park.reason == provenant_records::graph::ParkReason::LoopExhausted
                && park.request_id.is_some()
        });
    let park_valid = match progress.delivery_park.as_ref() {
        None => true,
        Some(park) => {
            queue
                && park.reason == provenant_records::graph::ParkReason::LoopExhausted
                && (park.request_id.is_none() && progress.state == NodeState::Running
                    || park.request_id.is_some() && progress.state == NodeState::Blocked)
        }
    };
    let valid = match progress.state {
        NodeState::Succeeded => {
            !failed && settled && (passed || accepted || clean) && matches_output(true)
        }
        NodeState::Failed => {
            (failed
                || stopped
                || escalated.is_some()
                || repair::is_repair(node) && settled
                || queue_exhausted)
                && matches_output(false)
        }
        NodeState::Pending => progress.outputs.is_none(),
        NodeState::Blocked if park_received => matches_output(false),
        NodeState::Blocked | NodeState::Skipped => {
            progress.outputs.is_none()
                && node.depends_on.iter().any(|parent| {
                    matches!(
                        checkpoint.nodes[positions[parent]].state,
                        NodeState::Failed | NodeState::Blocked | NodeState::Skipped
                    )
                })
        }
        NodeState::Running => park_pending && settled && matches_output(false),
    };
    let valid = valid
        && park_valid
        && (!repair::is_repair(node)
            || match (&progress.gate, progress.state) {
                (Some(gate), NodeState::Succeeded | NodeState::Failed) => {
                    gate.passed == (progress.state == NodeState::Succeeded)
                        && gate.on_fail == node.on_fail
                        && gate.reason.is_none() == gate.passed
                }
                (None, NodeState::Running) if park_pending => true,
                (None, NodeState::Pending | NodeState::Skipped | NodeState::Blocked) => true,
                _ => false,
            });
    if valid { Ok(()) } else { Err(()) }
}

#[allow(
    clippy::too_many_arguments,
    reason = "loop settlement coordinates graph state and journal"
)]
pub(super) fn settle<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    index: usize,
    spec: &LoopSpec,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<Option<String>, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[index];
    if let Some(park) = checkpoint.nodes[index].delivery_park.clone() {
        super::park_delivery_transition(
            document,
            index,
            park.reason,
            park.attempt_key.as_ref(),
            park.attempt_ref.as_deref(),
            &park.handoff,
            park.head.as_deref(),
            &park.evidence,
            owner,
            journal,
            checkpoint,
        )?;
        return Ok(None);
    }
    let mut external = node.clone();
    external
        .depends_on
        .retain(|parent| plan.nodes[positions[parent]].round.is_none());
    if let Some(state) = dependency_state(&external, plan, checkpoint, positions) {
        if state == NodeState::Skipped && spec.initial_publication.is_some() {
            let rounds_settled = checkpoint
                .nodes
                .iter()
                .filter(|progress| {
                    progress
                        .round
                        .as_ref()
                        .is_some_and(|round| round.loop_id == node.id)
                })
                .all(|progress| progress.state.settled());
            if rounds_settled && repair::selected_publication(node, checkpoint, plan).is_some() {
                let outputs = repair::terminal_outputs(node, checkpoint, positions, plan, true);
                checkpoint.nodes[index].state = NodeState::Succeeded;
                checkpoint.nodes[index].outputs = Some(outputs);
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
            return Ok(None);
        }
        if checkpoint.nodes[index].state != state {
            checkpoint.nodes[index].state = state;
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        return Ok(None);
    }
    let body: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter(|n| n.round.as_ref().is_some_and(|r| r.loop_id == node.id))
        .collect();
    let failed = body
        .iter()
        .any(|progress| failed_member(progress, plan, checkpoint, positions));
    let clean = repair::clean_start(node, checkpoint, positions);
    let escalation = repair::escalation(node, checkpoint, positions);
    let passed = passed(node, checkpoint, plan);
    let settled = body.iter().all(|n| n.state.settled());
    let rounds = body
        .iter()
        .filter_map(|n| n.round.as_ref().map(|r| r.round))
        .max()
        .unwrap_or(0);
    let decision = checkpoint
        .extensions
        .iter()
        .rev()
        .find_map(|e| e.strip_prefix(&format!("loop.{}.decision=", node.id)))
        .map(str::to_owned);
    let queue_exhausted = document.queue.is_some()
        && settled
        && !passed
        && !clean
        && decision.as_deref() != Some("accept");
    let mut loop_evidence: Vec<String> = if queue_exhausted {
        body.iter()
            .filter_map(|progress| progress.advance_error.clone())
            .take(8)
            .collect()
    } else {
        Vec::new()
    };
    if queue_exhausted && loop_evidence.is_empty() {
        loop_evidence.push(format!("bounded repair loop reached {rounds} rounds"));
    }
    let state = if failed
        || decision.as_deref() == Some("stop")
        || repair::is_repair(node) && settled && !passed && !clean
        || queue_exhausted
    {
        Some(NodeState::Failed)
    } else if passed || clean && settled || decision.as_deref() == Some("accept") && settled {
        Some(NodeState::Succeeded)
    } else {
        None
    };
    if let Some(state) = state {
        let outputs = repair::terminal_outputs(
            node,
            checkpoint,
            positions,
            plan,
            state == NodeState::Succeeded,
        );
        let progress = &mut checkpoint.nodes[index];
        progress.state = state;
        progress.outputs = Some(outputs);
        if repair::is_repair(node) {
            let passed = state == NodeState::Succeeded;
            progress.gate = Some(GateResult {
                extra: Default::default(),
                passed,
                reason: (!passed).then(|| {
                    escalation
                        .unwrap_or("repair cycle limit reached")
                        .to_owned()
                }),
                on_fail: node.on_fail.clone(),
            });
        }
        checkpoint.nodes[index].loop_pause = None;
        checkpoint.nodes[index].loop_pause_code = None;
        checkpoint.nodes[index].loop_cause = None;
        refresh_pause(checkpoint);
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        if queue_exhausted
            && checkpoint.nodes[index]
                .delivery_park
                .as_ref()
                .is_none_or(|park| {
                    park.reason == provenant_records::graph::ParkReason::LoopExhausted
                })
        {
            let attempt_key = checkpoint.nodes[index]
                .delivery_loop
                .as_ref()
                .map(|progress| progress.last_attempt.clone());
            let source_index = attempt_key.as_ref().and_then(|key| {
                plan.nodes
                    .iter()
                    .position(|candidate| candidate.id == key.node_id)
            });
            let source = source_index.and_then(|source_index| checkpoint.nodes.get(source_index));
            let attempt_ref = attempt_key.as_ref().and_then(|key| {
                source?
                    .recorded_attempts()
                    .find(|attempt| &attempt.key == key)
                    .and_then(|attempt| attempt.attempt_ref.clone())
            });
            let head = source.and_then(|progress| {
                progress
                    .outputs
                    .as_ref()
                    .and_then(|outputs| outputs.commit.clone())
                    .or_else(|| {
                        progress
                            .delivery_hygiene
                            .as_ref()
                            .and_then(|hygiene| hygiene.outcome.as_ref())
                            .and_then(|outcome| outcome.after_head.clone())
                    })
            });
            let handoff = source
                .and_then(|progress| {
                    progress
                        .handoff
                        .as_ref()
                        .map(|handoff| handoff.note.clone())
                })
                .or_else(|| {
                    source
                        .and_then(|progress| progress.delivery_hygiene.as_ref())
                        .and_then(|hygiene| hygiene.handoff.clone())
                })
                .unwrap_or_else(|| format!("loop {} exhausted after {rounds} rounds", node.id));
            super::park_delivery_transition(
                document,
                index,
                provenant_records::graph::ParkReason::LoopExhausted,
                attempt_key.as_ref(),
                attempt_ref.as_deref(),
                &handoff,
                head.as_deref(),
                &loop_evidence,
                owner,
                journal,
                checkpoint,
            )?;
        }
    } else if settled && dependency_state(node, plan, checkpoint, positions).is_none() {
        if checkpoint.nodes[index].loop_pause.is_none() {
            checkpoint.nodes[index].loop_pause_code = Some("loop_paused".into());
            checkpoint.nodes[index].loop_cause = Some(NodeDecisionCause::LoopPaused);
            checkpoint.nodes[index].loop_pause = Some(format!(
                "loop {} exhausted after {rounds} rounds; stop, extend or accept as is",
                node.id
            ));
        }
        refresh_pause(checkpoint);
        let Some(session) = &spec.decision_session else {
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            return Ok(None);
        };
        let ask_id = format!(
            "{}.rounds-{rounds}{}",
            node.id,
            asks::retry_suffix(&checkpoint.nodes[index])
        );
        let paused = format!(
            "loop {} exhausted after {rounds} rounds; stop, extend or accept as is",
            node.id
        );
        if asks::withdrawn(checkpoint, &node.id) {
            return ask_closed(index, &node.id, rounds, checkpoint, journal);
        }
        let position = match checkpoint.asks.iter().position(|ask| ask.ask_id == ask_id) {
            Some(position) => position,
            None => {
                let mut request = asks::loop_ask(&checkpoint.graph_id, &node.id, rounds, session);
                request.ask_id = ask_id.clone();
                match asks::file(checkpoint, owner, journal, &request) {
                    Ok((_, true)) => {
                        // The request was closed with no answer. Another
                        // decision comes through graph resume.
                        return ask_closed(index, &node.id, rounds, checkpoint, journal);
                    }
                    Ok((link, false)) => {
                        checkpoint.asks.push(link);
                        checkpoint.asks.len() - 1
                    }
                    Err(GraphError::Run(error)) => {
                        pause_invalid_decision(
                            checkpoint,
                            &node.id,
                            &format!("inbox unavailable: {error}"),
                            journal,
                        )?;
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        match asks::poll(checkpoint, position, owner) {
            Ok(reply) => {
                checkpoint.nodes[index].loop_pause_code = Some("loop_paused".into());
                checkpoint.nodes[index].loop_cause = Some(NodeDecisionCause::LoopPaused);
                checkpoint.nodes[index].loop_pause = Some(paused);
                refresh_pause(checkpoint);
                let Some(reply) = reply else {
                    journal.save(checkpoint).map_err(GraphError::Journal)?;
                    return Ok(None);
                };
                // The inbox keeps the answer, so a crash replays this read.
                let answer = asks::answered(reply);
                return apply(&node.id, rounds, &answer, checkpoint, journal);
            }
            Err(error) => {
                pause_invalid_decision(
                    checkpoint,
                    &node.id,
                    &format!("inbox unavailable: {error}"),
                    journal,
                )?;
                return Ok(None);
            }
        }
    }
    Ok(None)
}

/// The ask for an exhausted loop was closed without an answer.
fn ask_closed<R: Error + 'static, J: GraphJournal>(
    index: usize,
    node: &str,
    rounds: usize,
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
) -> Result<Option<String>, GraphError<R, J::Error>> {
    if checkpoint.nodes[index].loop_cause != Some(NodeDecisionCause::LoopAskClosed) {
        checkpoint.nodes[index].loop_pause_code = Some("loop_ask_closed".into());
        checkpoint.nodes[index].loop_cause = Some(NodeDecisionCause::LoopAskClosed);
        checkpoint.nodes[index].loop_pause = Some(format!(
            "loop {node} exhausted after {rounds} rounds; ask closed; use graph resume --extend loop.{node}.rounds=N or --extend loop.{node}.decision=stop|accept"
        ));
        refresh_pause(checkpoint);
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(None)
}

/// Turn a recorded loop answer into its extension, or pause on a bad one.
fn apply<R: Error + 'static, J: GraphJournal>(
    node: &str,
    rounds: usize,
    answer: &asks::Answer,
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
) -> Result<Option<String>, GraphError<R, J::Error>> {
    let extension = match &answer.rejected {
        Some(reason) => Err(reason.clone()),
        None => LoopDecision::parse(answer.option.as_deref(), answer.text.as_deref())
            .map_err(|error| error.to_string())
            .and_then(|decision| match decision {
                LoopDecision::Stop => Ok(format!("loop.{node}.decision=stop")),
                LoopDecision::Accept => Ok(format!("loop.{node}.decision=accept")),
                LoopDecision::Extend { rounds: extra } => rounds
                    .checked_add(extra)
                    .map(|total| format!("loop.{node}.rounds={total}"))
                    .ok_or_else(|| "loop extension cannot overflow".to_owned()),
            }),
    };
    match extension {
        Ok(extension) => Ok(Some(extension)),
        Err(reason) => {
            pause_invalid_decision(checkpoint, node, &reason, journal)?;
            Ok(None)
        }
    }
}

/// A rejected immutable answer remains inspectable; explicit resume can resolve it.
pub(super) fn pause_invalid_decision<R: Error + 'static, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    node: &str,
    reason: &str,
    journal: &mut J,
) -> Result<(), GraphError<R, J::Error>> {
    if let Some(progress) = checkpoint.nodes.iter_mut().find(|n| n.id == node) {
        progress.loop_pause_code = Some("invalid_loop_decision".into());
        progress.loop_cause = Some(NodeDecisionCause::InvalidLoopDecision);
        progress.loop_pause = Some(format!(
            "loop {node} exhausted; decision cannot be applied: {}; use graph resume --extend loop.{node}.rounds=N or --extend loop.{node}.decision=stop|accept",
            bounded(reason, MAX_ADVANCE_ERROR_BYTES)
        ));
    }
    refresh_pause(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)
}

pub(super) enum DecisionExtensionError {
    Invalid(String),
    Structural(GraphStructuralLimit),
}

pub(super) fn prepare_decision_extension(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    extension: &str,
    limits: &crate::config::GraphLimits,
) -> Result<(), DecisionExtensionError> {
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
    validate_resume_extension(document, checkpoint, extension, &generous).map_err(|rule| {
        DecisionExtensionError::Invalid(
            refusal::ExtensionCause::loop_rule(extension, rule).to_string(),
        )
    })?;
    if extension
        .split_once('=')
        .is_some_and(|(key, _)| key.ends_with(".decision"))
    {
        return Ok(());
    }
    let mut extensions = checkpoint.extensions.clone();
    extensions.push(extension.to_owned());
    let expanded = plans::expand(document, &extensions, &checkpoint.expansions, &generous)
        .map_err(DecisionExtensionError::Invalid)?;
    let (plan, _) = validate_flat_plan(&expanded, &generous)
        .map_err(|error| DecisionExtensionError::Invalid(error.to_string()))?;
    if let Some(limit) = structural_limit(&expanded, &plan, limits) {
        return Err(DecisionExtensionError::Structural(limit));
    }
    if let Some(reason) =
        graph_envelope_reason(document, &checkpoint.expansions, &extensions, &plan)
    {
        return Err(DecisionExtensionError::Invalid(reason));
    }
    Ok(())
}

pub(super) fn reopen_after_delivery(
    checkpoint: &mut GraphCheckpoint,
    loops: &std::collections::BTreeSet<String>,
) {
    for node in &mut checkpoint.nodes {
        if loops.contains(&node.id)
            && node.state == NodeState::Failed
            && !checkpoint
                .extensions
                .contains(&format!("loop.{}.decision=stop", node.id))
        {
            node.state = NodeState::Pending;
            node.outputs = None;
        }
    }
}

pub(super) fn refresh_pause(checkpoint: &mut GraphCheckpoint) {
    let reasons: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter_map(|n| n.loop_pause.as_deref())
        .collect();
    if checkpoint.pause.is_none() || asks::loop_pause(checkpoint) {
        let cause = checkpoint
            .nodes
            .iter()
            .filter_map(|node| node.loop_cause.as_ref())
            .find_map(|cause| match cause {
                NodeDecisionCause::LoopAskClosed => Some(PauseCause::LoopAskClosed),
                NodeDecisionCause::InvalidLoopDecision => Some(PauseCause::InvalidLoopDecision),
                NodeDecisionCause::LoopPaused
                | NodeDecisionCause::PlanExpansionPaused { .. }
                | NodeDecisionCause::CompositeFindingsTruncated { .. } => None,
            })
            .unwrap_or(PauseCause::LoopPaused);
        if reasons.is_empty() {
            checkpoint.pause = None;
        } else {
            let reason = format!("loops paused: {}", reasons.join("; "));
            checkpoint.set_pause(cause, &reason);
        }
    }
}
