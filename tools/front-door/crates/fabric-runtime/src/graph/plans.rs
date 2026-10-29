//! Dynamic plans and honest-stop continuations share the scheduler and journal.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub kinds: Vec<NodeKind>,
    pub max_nodes: usize,
}

/// Only validated, normalised additions enter the checkpoint. The original
/// document remains the digest authority, just as it does for loop rounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expansion {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub source: String,
    pub continuation: bool,
    pub nodes: Vec<GraphNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemainingBudgets {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub nodes: usize,
    pub edges: usize,
    pub continuations: usize,
}

#[derive(Deserialize)]
struct Fragment {
    schema_version: u64,
    nodes: Vec<GraphNode>,
}

#[derive(Deserialize)]
struct Handoff {
    schema_version: u64,
    status: String,
    note: String,
}

pub(super) fn prompt(node: &GraphNode) -> String {
    let mut prompt = node.prompt.clone();
    if let Some(spec) = &node.plan {
        let kinds = serde_json::to_string(&spec.kinds).unwrap_or_default();
        prompt.push_str(&format!("\n\nReturn one final fenced `fragment` JSON block, at most 12288 bytes: {{\"schema_version\":1,\"nodes\":[...]}}. Emit 1..{} nodes, using only {kinds}. Each agent node requires id, goal or brief, access, write_scope (relative literal files/directories), `acceptance_checks` and local depends_on. Route defaults to {{\"alias\":\"senior\"}}. Writers require the `worktree` key naming an existing registered worktree. Synthesise nodes require `inputs` selecting dependency outputs. Gate nodes use the graph gate condition schema. Dependencies and inputs name fragment-local IDs. Keep each namespaced ID (this node's ID + -- + child ID) within 64 bytes. Do not emit presets or review shapes.", spec.max_nodes));
    }
    prompt
}

pub(super) fn validate_spec(node: &GraphNode) -> Result<(), &'static str> {
    if (node.kind == Some(NodeKind::Plan)) != node.plan.is_some() {
        return Err("plan requires fragment kinds and max_nodes");
    }
    if let Some(spec) = &node.plan {
        let mut seen = Vec::new();
        if node.access != AccessMode::ReadOnly
            || node.required_verdict.is_some()
            || spec.max_nodes == 0
            || spec.max_nodes > 512
            || spec.kinds.is_empty()
            || spec.kinds.iter().any(|kind| {
                let duplicate = seen.contains(kind);
                seen.push(*kind);
                duplicate || *kind == NodeKind::Loop
            })
        {
            return Err(
                "invalid plan configuration; fragments support task, plan, synthesise and gate",
            );
        }
    }
    Ok(())
}

/// A complete fenced block must fit in one existing capture window. Never
/// parse a clipped prefix or infer an honest stop from prose.
fn block<'a>(
    capture: &'a crate::result_capture::CapturedResult,
    tag: &str,
) -> Result<Option<&'a str>, String> {
    let marker = format!("```{tag}\n");
    let tail = if capture.tail_truncated {
        capture.tail.split_once('\n').map_or("", |(_, tail)| tail)
    } else {
        &capture.tail
    };
    let text = if tail.contains(&marker) {
        tail
    } else if !capture.truncated {
        &capture.excerpt
    } else {
        ""
    };
    let Some((_, rest)) = text.split_once(&marker) else {
        return Ok(None);
    };
    if rest.contains(&marker) {
        return Err(format!("{tag} block repeated"));
    }
    let (json, _) = rest
        .split_once("\n```")
        .ok_or_else(|| format!("{tag} block incomplete"))?;
    if json.len() > 12 * 1024 {
        return Err(format!("{tag} block exceeds 12288 bytes"));
    }
    Ok(Some(json))
}

pub(super) fn handoff(
    capture: &crate::result_capture::CapturedResult,
) -> Result<Option<String>, String> {
    let Some(json) = block(capture, "handoff")? else {
        return Ok(None);
    };
    let handoff: Handoff =
        serde_json::from_str(json).map_err(|e| format!("invalid handoff: {e}"))?;
    if handoff.schema_version != 1
        || handoff.status != "stopped_early"
        || handoff.note.trim().is_empty()
        || handoff.note.len() > 4096
    {
        return Err(
            "handoff requires schema_version 1, status stopped_early and a note of 1..4096 bytes"
                .into(),
        );
    }
    Ok(Some(handoff.note))
}

pub(super) fn writer(node: &PlannedNode) -> bool {
    node.access == AccessMode::WorktreeWrite && node.kind.is_none_or(|kind| kind == NodeKind::Task)
}

pub(super) fn expand(
    document: &GraphDocument,
    extensions: &[String],
    expansions: &[Expansion],
) -> Result<GraphDocument, &'static str> {
    let mut expanded = loops::expand(document, extensions)?;
    let mut sources = std::collections::BTreeSet::new();
    for addition in expansions {
        if addition.nodes.is_empty()
            || !sources.insert(&addition.source)
            || !expanded.nodes.iter().any(|n| n.id == addition.source)
        {
            return Err("invalid expansion source");
        }
        append(&mut expanded, addition);
    }
    Ok(expanded)
}

fn append(document: &mut GraphDocument, expansion: &Expansion) {
    let round = document
        .nodes
        .iter()
        .find(|n| n.id == expansion.source)
        .and_then(|n| n.round.clone());
    // Retain the original dependency for selected upstream outputs, and wait
    // for the entire fragment before admitting any of its consumers.
    for node in &mut document.nodes {
        if node.dependencies().any(|name| name == &expansion.source) {
            node.after_expansion
                .extend(expansion.nodes.iter().map(|n| n.id.clone()));
            if expansion.continuation {
                for input in &mut node.inputs {
                    if input.node == expansion.source {
                        input.node = expansion.nodes[0].id.clone();
                        if !node.depends_on.contains(&input.node) {
                            node.depends_on.push(input.node.clone());
                        }
                    }
                }
            }
        }
    }
    document
        .nodes
        .extend(expansion.nodes.iter().cloned().map(|mut child| {
            child.round = round.clone().map(|mut round| {
                round.until = false;
                round.node_id = child.id.clone();
                round
            });
            child
        }));
}

fn valid_scope(scope: &str) -> bool {
    scope == "."
        || (!scope.is_empty()
            && !scope
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            && !scope.contains(['\\', '*', '?', '[', ']']))
}

fn overlaps(left: &[String], right: &[String]) -> bool {
    left.is_empty()
        || right.is_empty()
        || left.iter().chain(right).any(|scope| !valid_scope(scope))
        || left.iter().any(|a| {
            right.iter().any(|b| {
                a == "."
                    || b == "."
                    || a == b
                    || a.starts_with(&format!("{b}/"))
                    || b.starts_with(&format!("{a}/"))
            })
        })
}

pub(super) fn writers_conflict(left: &PlannedNode, right: &PlannedNode) -> bool {
    left.access == AccessMode::WorktreeWrite
        && right.access == AccessMode::WorktreeWrite
        && (left.worktree == right.worktree || overlaps(&left.write_scope, &right.write_scope))
}

fn fragment(
    node: &PlannedNode,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    json: &str,
) -> Result<Expansion, String> {
    let spec = node.plan.as_ref().ok_or("missing plan configuration")?;
    let mut fragment: Fragment =
        serde_json::from_str(json).map_err(|e| format!("invalid fragment: {e}"))?;
    if fragment.schema_version != 1
        || fragment.nodes.is_empty()
        || fragment.nodes.len() > spec.max_nodes
    {
        return Err(format!(
            "fragment requires 1..{} nodes and schema_version 1",
            spec.max_nodes
        ));
    }
    for child in &mut fragment.nodes {
        if !spec.kinds.contains(&child.kind.unwrap_or(NodeKind::Task)) {
            return Err(format!(
                "fragment kind {:?} was not declared",
                child.kind.unwrap_or(NodeKind::Task)
            ));
        }
        // Gate evidence lives in its condition. Agent nodes carry explicit
        // scope and acceptance prose in their submitted brief.
        if child.kind != Some(NodeKind::Gate) {
            if child.prompt.trim().is_empty()
                || child.write_scope.is_empty()
                || child.acceptance_checks.is_empty()
                || child.acceptance_checks.iter().any(|s| s.trim().is_empty())
                || child.write_scope.iter().any(|s| !valid_scope(s))
            {
                return Err("fragment agent requires goal or brief, relative literal write_scope and acceptance_checks".into());
            }
            child.prompt.push_str(&format!(
                "\n\nWrite scope: {}\nAcceptance checks:\n{}",
                child.write_scope.join(", "),
                child.acceptance_checks.join("\n")
            ));
            if child.route.is_none() && child.routes.is_none() {
                child.route = Some(GraphRoute {
                    extra: Default::default(),
                    alias: Some("senior".into()),
                    adapter: None,
                    model: None,
                    effort: None,
                    account: None,
                });
            }
        }
        if child.access == AccessMode::WorktreeWrite {
            if child.delivery.as_ref().is_some_and(|mode| {
                !matches!(
                    mode,
                    provenant_records::run::DeliveryMode::None
                        | provenant_records::run::DeliveryMode::Integrate(_)
                )
            }) {
                return Err("fragment writers use integrate delivery, never individual PRs".into());
            }
            if let Some(target) = &document.integration_branch {
                child.delivery = Some(provenant_records::run::DeliveryMode::Integrate(
                    target.clone(),
                ));
            }
        }
    }
    let local = GraphDocument {
        extra: Default::default(),
        schema_version: 1,
        graph_id: "fragment".into(),
        nodes: fragment.nodes,
        limits: None,
        delivery: None,
        integration_branch: document.integration_branch.clone(),
    };
    let (plan, _) = validate_flat_plan(&local, &generous()).map_err(str::to_owned)?;
    let mut nodes: Vec<GraphNode> = plan
        .nodes
        .iter()
        .filter_map(|n| local.nodes.iter().find(|child| child.id == n.id).cloned())
        .collect();
    // Union overlapping writer scopes, including transitive overlaps. Every
    // component uses one checkout; admission fences serialise its writers.
    let mut components: Vec<usize> = (0..nodes.len()).collect();
    for i in 0..nodes.len() {
        for j in 0..i {
            if nodes[i].access == AccessMode::WorktreeWrite
                && nodes[j].access == AccessMode::WorktreeWrite
                && (nodes[i].worktree == nodes[j].worktree
                    || overlaps(&nodes[i].write_scope, &nodes[j].write_scope))
            {
                let old = components[i].max(components[j]);
                let new = components[i].min(components[j]);
                for value in &mut components {
                    if *value == old {
                        *value = new;
                    }
                }
            }
        }
    }
    for i in 0..nodes.len() {
        if components[i] != i {
            let first = nodes[components[i]].clone();
            if document.integration_branch.is_none() {
                return Err("overlapping writers require an integration_branch".into());
            }
            nodes[i].worktree = first.worktree;
        }
    }
    let id = |name: &str| format!("{}--{name}", node.id);
    for child in &mut nodes {
        if id(&child.id).len() > 64 {
            return Err(format!(
                "child {} of planner {} exceeds the namespaced node ID limit of 64 bytes; shorten either ID",
                child.id, node.id
            ));
        }
        child.id = id(&child.id);
        child.depends_on = child.depends_on.iter().map(|name| id(name)).collect();
        if child.depends_on.is_empty() {
            child.depends_on.push(node.id.clone());
        }
        for input in &mut child.inputs {
            input.node = id(&input.node);
        }
        child.on_fail = child.on_fail.as_deref().map(id);
        child.round = None;
    }
    coordinate(&mut nodes, document, checkpoint)?;
    Ok(Expansion {
        extra: Default::default(),
        source: node.id.clone(),
        continuation: false,
        nodes,
    })
}

fn coordinate(
    nodes: &mut [GraphNode],
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
) -> Result<(), String> {
    let paths: std::collections::BTreeSet<_> = nodes
        .iter()
        .filter(|n| n.access == AccessMode::WorktreeWrite)
        .filter_map(|n| n.worktree.clone())
        .collect();
    for path in paths {
        let members: Vec<_> = nodes
            .iter()
            .filter(|n| n.worktree.as_ref() == Some(&path))
            .collect();
        let existing: Vec<_> = document
            .nodes
            .iter()
            .filter(|n| {
                n.access == AccessMode::WorktreeWrite
                    && !checkpoint.nodes.iter().any(|progress| {
                        progress.id == n.id
                            && matches!(progress.state, NodeState::Skipped | NodeState::Blocked)
                    })
                    && members.iter().any(|child| {
                        n.worktree == child.worktree || overlaps(&n.write_scope, &child.write_scope)
                    })
            })
            .collect();
        let checkouts: std::collections::BTreeSet<_> = existing
            .iter()
            .filter_map(|n| n.worktree.as_ref())
            .collect();
        if checkouts.len() > 1 {
            return Err("expansion connects existing writers on different branches; use one shared worktree".into());
        }
        if let Some(checkout) = checkouts.first() {
            if document.integration_branch.is_none() {
                return Err("overlapping writers require an integration_branch".into());
            }
            for child in nodes
                .iter_mut()
                .filter(|n| n.worktree.as_ref() == Some(&path))
            {
                child.worktree = Some((*checkout).clone());
            }
        }
    }
    Ok(())
}

fn generous() -> crate::config::GraphLimits {
    crate::config::GraphLimits {
        nodes: usize::MAX,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        ..Default::default()
    }
}

fn continuation(
    node: &PlannedNode,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    note: &str,
    limits: &crate::config::GraphLimits,
) -> Result<Expansion, String> {
    if checkpoint
        .expansions
        .iter()
        .filter(|e| e.continuation)
        .count()
        >= limits.continuations
    {
        return Err(format!(
            "limits.continuations={} reached; extend the continuation budget",
            limits.continuations
        ));
    }
    let mut child = document
        .nodes
        .iter()
        .find(|n| n.id == node.id)
        .cloned()
        .ok_or("continuation source missing")?;
    child.id = format!("{}--continue", node.id);
    if child.id.len() > 64 {
        return Err(format!(
            "continuation {} exceeds the node ID limit of 64 bytes; shorten the source ID",
            child.id
        ));
    }
    child.title = None;
    child.depends_on = vec![node.id.clone()];
    child.after_expansion.clear();
    child.inputs.clear();
    child.prompt.push_str(&format!(
        "\n\nContinue this work in the same worktree. Stopped-early handoff:\n{note}"
    ));
    // Integrate retains the shared checkout and leaves the graph owning its PR.
    if child.delivery == Some(provenant_records::run::DeliveryMode::Pr) {
        return Err("honest-stop continuation requires integrate or no delivery".into());
    }
    if let Some(target) = &document.integration_branch {
        child.delivery = Some(provenant_records::run::DeliveryMode::Integrate(
            target.clone(),
        ));
    }
    child.round = None;
    Ok(Expansion {
        extra: Default::default(),
        source: node.id.clone(),
        continuation: true,
        nodes: vec![child],
    })
}

pub(super) fn remaining(
    checkpoint: &mut GraphCheckpoint,
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) {
    // Preserve the historical wire shape for graphs that do not use expansion.
    if document
        .nodes
        .iter()
        .any(|n| n.plan.is_some() || n.access == AccessMode::WorktreeWrite)
    {
        checkpoint.remaining = Some(RemainingBudgets {
            extra: Default::default(),
            nodes: limits.nodes.saturating_sub(document.nodes.len()),
            edges: limits.edges.saturating_sub(
                document
                    .nodes
                    .iter()
                    .map(|n| n.dependencies().count())
                    .sum(),
            ),
            continuations: limits.continuations.saturating_sub(
                checkpoint
                    .expansions
                    .iter()
                    .filter(|e| e.continuation)
                    .count(),
            ),
        });
    }
}

/// Validate the entire prospective graph before publishing any added node.
pub(super) fn settle(
    node: &PlannedNode,
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    capture: Option<&crate::result_capture::CapturedResult>,
    limits: &crate::config::GraphLimits,
) -> Option<(
    GraphDocument,
    PlannedGraph,
    std::collections::BTreeMap<String, usize>,
)> {
    if checkpoint.expansions.iter().any(|e| e.source == node.id) {
        return None;
    }
    let index = checkpoint
        .nodes
        .iter()
        .position(|n| n.id == node.id && n.state == NodeState::Succeeded)?;
    let addition = if node.plan.is_some() {
        let json = if let Some(capture) = capture {
            block(capture, "fragment")
                .and_then(|json| json.ok_or_else(|| "fragment block missing".into()))
                .map(|json| {
                    checkpoint.nodes[index].fragment = Some(json.to_owned());
                    json.to_owned()
                })
        } else {
            checkpoint.nodes[index]
                .fragment
                .clone()
                .ok_or_else(|| "planner result missing".into())
        };
        json.and_then(|json| fragment(node, document, checkpoint, &json))
    } else if writer(node) {
        let handoff = capture
            .map(handoff)
            .unwrap_or_else(|| Ok(checkpoint.nodes[index].handoff.clone()));
        match handoff {
            Ok(Some(note)) => {
                checkpoint.nodes[index].handoff = Some(note.clone());
                continuation(node, document, checkpoint, &note, limits)
            }
            Ok(None) => return None,
            Err(reason) => {
                if let Some(outputs) = &mut checkpoint.nodes[index].outputs {
                    outputs.warnings.push(bounded(
                        &format!("ignored optional handoff: {reason}"),
                        MAX_ADVANCE_ERROR_BYTES,
                    ));
                }
                return None;
            }
        }
    } else {
        return None;
    };
    let result = addition.and_then(|addition| {
        let mut preview = document.clone();
        append(&mut preview, &addition);
        let mut loop_writers = std::collections::BTreeMap::new();
        for writer in preview
            .nodes
            .iter()
            .filter(|n| n.access == AccessMode::WorktreeWrite)
        {
            if let Some(round) = &writer.round
                && let Some(previous) = loop_writers.insert(&round.loop_id, &writer.worktree)
                && previous != &writer.worktree
            {
                return Err("loop writers must share one worktree".into());
            }
        }
        let (plan, positions) = validate_flat_plan(&preview, &generous()).map_err(str::to_owned)?;
        if let Some(reason) =
            structural_limit_reason(&preview, &plan, limits, &checkpoint.extensions)
        {
            return Err(reason);
        }
        Ok((addition, plan, positions, preview))
    });
    match result {
        Ok((addition, plan, positions, preview)) => {
            checkpoint.nodes[index].fragment = None;
            checkpoint.nodes[index].expansion_pause = None;
            if checkpoint
                .pause_reason
                .as_ref()
                .is_some_and(|r| r.starts_with("node ") && r.contains(" expansion paused:"))
            {
                checkpoint.pause_reason = checkpoint
                    .nodes
                    .iter()
                    .find_map(|n| n.expansion_pause.clone());
            }
            checkpoint.expansions.push(addition);
            loops::reconcile(checkpoint, &plan);
            remaining(checkpoint, &preview, limits);
            Some((preview, plan, positions))
        }
        Err(reason) => {
            let reason = bounded(
                &format!("node {} expansion paused: {reason}", node.id),
                MAX_ADVANCE_ERROR_BYTES,
            );
            checkpoint.nodes[index].expansion_pause = Some(reason.clone());
            checkpoint.pause_reason = Some(reason);
            None
        }
    }
}
