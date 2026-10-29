//! Static composites retain one settlement boundary and selected public outputs.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeSpec {
    pub nodes: Vec<GraphNode>,
    pub exports: Vec<GraphInput>,
}

/// Expand only at the execution boundary. The submitted document remains nested.
pub(super) fn expand(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<GraphDocument, String> {
    let mut expanded = document.clone();
    let (max_depth, max_nodes) = (limits.expansion_depth, limits.nodes);
    expanded.nodes = children(
        &document.nodes,
        "",
        &[],
        &[],
        None,
        &[],
        0,
        max_depth,
        max_nodes,
    )?;
    if expanded.nodes.iter().any(|node| node.composite.is_some())
        && expanded.nodes.len() > max_nodes
    {
        return Err(format!("composite expansion exceeds {max_nodes} nodes"));
    }
    Ok(expanded)
}

fn scoped(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_owned()
    } else {
        format!("{prefix}--{local}")
    }
}

fn reference(
    prefix: &str,
    name: &str,
    locals: &BTreeSet<&str>,
    incoming: &[String],
) -> Option<String> {
    if locals.contains(name) {
        return Some(scoped(prefix, name));
    }
    if incoming.iter().any(|parent| parent == name) {
        return Some(name.to_owned());
    }
    prefix
        .rsplit_once("--")
        .map(|(parent, _)| scoped(parent, name))
        .filter(|name| incoming.contains(name))
}

#[allow(
    clippy::too_many_arguments,
    reason = "bounded recursive composite expansion carries inherited execution bindings"
)]
fn children(
    nodes: &[GraphNode],
    prefix: &str,
    incoming: &[String],
    inputs: &[GraphInput],
    source: Option<&SourceBinding>,
    read_roots: &[String],
    depth: usize,
    max_depth: usize,
    max_nodes: usize,
) -> Result<Vec<GraphNode>, String> {
    if depth > max_depth {
        return Err("composite expansion depth exceeds the limit".into());
    }
    let locals: BTreeSet<_> = nodes.iter().map(|node| node.id.as_str()).collect();
    if locals.len() != nodes.len() {
        return Err("node id".into());
    }
    let composite_bound = depth > 0 || nodes.iter().any(|node| node.composite.is_some());
    let mut expanded = Vec::new();
    for original in nodes {
        let mut node = original.clone();
        node.id = scoped(prefix, &node.id);
        if !valid_identifier(&node.id) {
            return Err("expanded composite node id exceeds 64 bytes".into());
        }
        for parent in &mut node.depends_on {
            *parent = reference(prefix, parent, &locals, incoming).ok_or("dependency")?;
        }
        for input in &mut node.inputs {
            if !input.node.starts_with("previous.") {
                input.node = reference(prefix, &input.node, &locals, incoming)
                    .unwrap_or_else(|| scoped(prefix, &input.node));
            }
        }
        if let Some(target) = &mut node.on_fail {
            *target = scoped(prefix, target);
        }
        if let Some(SourceBinding::Node { node: source }) = &mut node.source
            && !source.starts_with("previous.")
        {
            *source = reference(prefix, source, &locals, incoming)
                .unwrap_or_else(|| scoped(prefix, source));
        }
        if original
            .depends_on
            .iter()
            .all(|parent| !locals.contains(parent.as_str()))
        {
            for parent in incoming {
                if !node.depends_on.contains(parent) {
                    node.depends_on.push(parent.clone());
                }
            }
            for input in inputs {
                if let Some(own) = node.inputs.iter_mut().find(|own| own.node == input.node) {
                    for kind in &input.outputs {
                        if !own.outputs.contains(kind) {
                            own.outputs.push(*kind);
                        }
                    }
                } else {
                    node.inputs.push(input.clone());
                }
            }
        }
        if node.source.is_none() {
            node.source = source.cloned();
            if let Some(SourceBinding::Node { node: source }) = &node.source
                && !node.depends_on.contains(source)
            {
                node.depends_on.push(source.clone());
            }
        }
        for root in read_roots {
            if !node.read_roots.contains(root) {
                node.read_roots.push(root.clone());
            }
        }
        if let Some(spec) = &mut node.loop_body {
            for child in &spec.body {
                if child.id.starts_with("previous.") {
                    return Err("previous. is reserved for prior round inputs".into());
                }
                if child.loop_body.is_some()
                    || child.kind == Some(NodeKind::Loop)
                    || child.on_fail.is_some()
                    || child.delivery.is_some()
                {
                    return Err("loop body cannot nest loops or deliver or route on_fail".into());
                }
            }
            spec.body = children(
                &spec.body,
                "",
                &[],
                &[],
                None,
                &[],
                depth,
                max_depth,
                max_nodes,
            )?;
        }
        if (node.kind == Some(NodeKind::Composite)) != node.composite.is_some() {
            return Err("composite requires nodes and exports".into());
        }
        if let Some(spec) = node.composite.take() {
            if spec.nodes.is_empty()
                || node.access != AccessMode::ReadOnly
                || node.route.is_some()
                || node.routes.is_some()
                || node.worktree.is_some()
                || node.delivery.is_some()
                || node.condition.is_some()
                || node.on_fail.is_some()
                || node.plan.is_some()
                || node.loop_body.is_some()
                || !node.prompt.is_empty()
                || node.required_verdict.is_some()
                || !node.write_scope.is_empty()
                || !node.acceptance_checks.is_empty()
            {
                return Err("composite cannot carry agent configuration".into());
            }
            let member_ids: BTreeSet<_> =
                spec.nodes.iter().map(|child| child.id.as_str()).collect();
            let mut selected = BTreeSet::new();
            let mut scalar = Vec::new();
            for export in &spec.exports {
                if !member_ids.contains(export.node.as_str())
                    || export.outputs.is_empty()
                    || !selected.insert(&export.node)
                {
                    return Err("composite export must select a unique direct child".into());
                }
                let mut kinds = Vec::new();
                for kind in &export.outputs {
                    if kinds.contains(kind) {
                        return Err("duplicate composite export output".into());
                    }
                    kinds.push(*kind);
                    if matches!(
                        kind,
                        OutputKind::Verdict | OutputKind::Branch | OutputKind::Commit
                    ) {
                        if scalar.contains(kind) {
                            return Err("composite scalar output has multiple sources".into());
                        }
                        scalar.push(*kind);
                    }
                }
            }
            let body = children(
                &spec.nodes,
                &node.id,
                &node.depends_on,
                &node.inputs,
                node.source.as_ref(),
                &node.read_roots,
                depth + 1,
                max_depth,
                max_nodes,
            )?;
            node.composite_members = spec
                .nodes
                .iter()
                .map(|child| scoped(&node.id, &child.id))
                .collect();
            node.after_expansion.extend(node.composite_members.clone());
            node.inputs.clear();
            node.source = None;
            node.read_roots.clear();
            node.composite = Some(CompositeSpec {
                nodes: Vec::new(),
                exports: spec
                    .exports
                    .into_iter()
                    .map(|mut export| {
                        export.node = scoped(&node.id, &export.node);
                        export
                    })
                    .collect(),
            });
            expanded.extend(body);
        }
        expanded.push(node);
        if composite_bound && expanded.len() > max_nodes {
            return Err(format!("composite expansion exceeds {max_nodes} nodes"));
        }
    }
    Ok(expanded)
}

fn outputs(
    spec: &CompositeSpec,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> NodeOutputs {
    let mut selected = NodeOutputs {
        extra: Default::default(),
        schema_version: 2,
        result: None,
        verdict: None,
        verdict_error: None,
        findings: Vec::new(),
        branch: None,
        commit: None,
        warnings: Vec::new(),
    };
    let mut results = Vec::new();
    for export in &spec.exports {
        let Some(source) = checkpoint.nodes[positions[&export.node]].outputs.as_ref() else {
            continue;
        };
        for kind in &export.outputs {
            match kind {
                OutputKind::Result => {
                    if let Some(result) = &source.result {
                        results.push((&export.node, result));
                    }
                }
                OutputKind::Verdict => {
                    selected.verdict = source.verdict.clone();
                    selected.verdict_error = source.verdict_error.clone();
                }
                OutputKind::Findings => selected.findings.extend(source.findings.clone()),
                OutputKind::Branch => selected.branch = source.branch.clone(),
                OutputKind::Commit => selected.commit = source.commit.clone(),
            }
        }
    }
    selected.result = match results.as_slice() {
        [] => None,
        [(_, result)] => Some((*result).clone()),
        _ => {
            let mut excerpt = String::new();
            let mut truncated = false;
            for (name, result) in results {
                excerpt.push_str(&format!("{name}:\n{}\n", result.excerpt));
                truncated |= result.truncated;
            }
            let digest = CanonicalV1::file_digest(excerpt.as_bytes()).to_string();
            truncated |= excerpt.len() > 65_536;
            Some(ResultOutput {
                extra: Default::default(),
                digest,
                excerpt: bounded(&excerpt, 65_536),
                truncated,
                path: None,
            })
        }
    };
    if selected.findings.len() > MAX_FINDINGS {
        selected.findings.truncate(MAX_FINDINGS);
        selected
            .warnings
            .push("composite exported findings exceed cap".into());
    }
    selected
}

pub(super) fn settle<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[index];
    let spec = node
        .composite
        .as_ref()
        .ok_or(GraphError::Invalid("composite boundary".into()))?;
    let state = state(node, plan, checkpoint, positions);
    let exported = (state == NodeState::Succeeded).then(|| outputs(spec, checkpoint, positions));
    let progress = &mut checkpoint.nodes[index];
    if progress.state != state || progress.outputs != exported {
        progress.state = state;
        progress.outputs = exported;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(())
}

fn state(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> NodeState {
    match dependency_state(node, plan, checkpoint, positions) {
        Some(NodeState::Pending)
            if node
                .composite_members
                .iter()
                .any(|child| checkpoint.nodes[positions[child]].state != NodeState::Pending) =>
        {
            NodeState::Running
        }
        Some(state) => state,
        None => NodeState::Succeeded,
    }
}

pub(super) fn validate_progress(
    progress: &NodeProgress,
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Result<(), ()> {
    let state = state(node, plan, checkpoint, positions);
    let valid = progress.attempts.is_empty()
        && progress.gate.is_none()
        && progress.ci.is_none()
        && progress.advance_error.is_none()
        && progress.source_commit.is_none()
        && match progress.state {
            NodeState::Pending => progress.outputs.is_none(),
            NodeState::Running => state == NodeState::Running && progress.outputs.is_none(),
            NodeState::Succeeded => {
                state == NodeState::Succeeded
                    && progress.outputs
                        == node
                            .composite
                            .as_ref()
                            .map(|spec| outputs(spec, checkpoint, positions))
            }
            NodeState::Blocked | NodeState::Skipped => {
                state == progress.state && progress.outputs.is_none()
            }
            _ => false,
        };
    if valid { Ok(()) } else { Err(()) }
}
