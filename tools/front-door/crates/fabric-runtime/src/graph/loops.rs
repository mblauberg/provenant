//! Bounded loop expansion uses the ordinary scheduler, attempts and journal.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopSpec {
    pub body: Vec<GraphNode>,
    pub until: String,
    pub max_rounds: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_session: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopRound {
    pub loop_id: String,
    pub round: usize,
    pub node_id: String,
    pub until: bool,
    pub previous_gate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopDecision {
    Stop,
    Extend { rounds: usize },
    Accept,
    Invalid { reason: String },
}

fn id(owner: &str, round: usize, node: &str) -> String {
    format!("{owner}--r{round}--{node}")
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

pub(super) fn validate_extension(document: &GraphDocument, extension: &str) -> Result<(), String> {
    let (key, value) = extension.split_once('=').ok_or("expected KEY=VALUE")?;
    let key = key.strip_prefix("loop.").ok_or("invalid loop extension")?;
    let (name, field) = key.rsplit_once('.').ok_or("invalid loop extension")?;
    let node = document
        .nodes
        .iter()
        .find(|n| n.id == name && n.loop_body.is_some())
        .ok_or("unknown loop")?;
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
) -> Result<(), String> {
    validate_extension(document, extension)?;
    if checkpoint.extensions.iter().any(|saved| saved == extension) {
        return Ok(());
    }
    let (key, value) = extension.split_once('=').ok_or("expected KEY=VALUE")?;
    let (name, field) = key
        .strip_prefix("loop.")
        .and_then(|key| key.rsplit_once('.'))
        .ok_or("invalid loop extension")?;
    if checkpoint
        .extensions
        .iter()
        .any(|e| e.starts_with(&format!("loop.{name}.decision=")))
    {
        return Err("loop already has a terminal decision".into());
    }
    let progress = checkpoint
        .nodes
        .iter()
        .find(|n| n.id == name)
        .ok_or("unknown loop")?;
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
        return Err("loop decisions and extensions require exhaustion".into());
    }
    if field == "rounds" {
        let original = document
            .nodes
            .iter()
            .find(|n| n.id == name)
            .ok_or("unknown loop")?;
        let current = allowance(original, &checkpoint.extensions);
        if value
            .parse::<usize>()
            .map_err(|_| "invalid loop allowance")?
            <= current
        {
            return Err("loop allowance must increase".into());
        }
    }
    Ok(())
}

pub(super) fn expand(
    document: &GraphDocument,
    extensions: &[String],
) -> Result<GraphDocument, &'static str> {
    let declared: std::collections::BTreeSet<_> = document.nodes.iter().map(|n| &n.id).collect();
    if document
        .nodes
        .iter()
        .any(|n| n.depends_on.iter().any(|parent| !declared.contains(parent)))
    {
        return Err("dependency");
    }
    if document.nodes.iter().any(|n| n.loop_body.is_some()) {
        let count = document.nodes.iter().try_fold(0usize, |total, n| {
            let count = match &n.loop_body {
                Some(spec) => spec
                    .body
                    .len()
                    .checked_mul(allowance(n, extensions))?
                    .checked_add(1)?,
                None => 1,
            };
            total.checked_add(count)
        });
        if count.is_none_or(|n| n > 16384) {
            return Err("loop expansion exceeds safety bound");
        }
    }
    let mut expanded = document.clone();
    expanded.nodes.clear();
    for node in &document.nodes {
        let Some(spec) = &node.loop_body else {
            if node.kind == Some(NodeKind::Loop) {
                return Err("loop requires a body");
            }
            expanded.nodes.push(node.clone());
            continue;
        };
        if node.kind != Some(NodeKind::Loop)
            || node.access != AccessMode::ReadOnly
            || node.worktree.is_some()
            || node.delivery.is_some()
            || node.condition.is_some()
            || node.on_fail.is_some()
            || node.route.is_some()
            || node.routes.is_some()
            || !node.prompt.is_empty()
            || !node.inputs.is_empty()
            || node.required_verdict.is_some()
            || spec.body.is_empty()
            || spec.max_rounds == 0
            || spec.max_rounds > 1024
            || spec
                .decision_session
                .as_ref()
                .is_some_and(|s| crate::inbox::session_id(s).is_err())
        {
            return Err("invalid loop configuration");
        }
        let rounds = allowance(node, extensions);
        if rounds > 1024
            || spec
                .body
                .len()
                .saturating_mul(rounds)
                .saturating_add(expanded.nodes.len())
                > 16384
        {
            return Err("loop expansion exceeds safety bound");
        }
        let mut body = spec.body.clone();
        let mut writer = None;
        for child in &mut body {
            if child.id.starts_with("previous.") {
                return Err("previous. is reserved for prior round inputs");
            }
            let mut selected = std::collections::BTreeSet::new();
            for input in &child.inputs {
                let mut kinds = Vec::new();
                if !selected.insert(&input.node) || input.outputs.is_empty() {
                    return Err("duplicate or empty loop input");
                }
                for kind in &input.outputs {
                    if kinds.contains(kind) {
                        return Err("duplicate input output");
                    }
                    kinds.push(*kind);
                }
            }
            if child.loop_body.is_some()
                || child.kind == Some(NodeKind::Loop)
                || child.on_fail.is_some()
                || child.delivery.is_some()
            {
                return Err("loop body cannot nest loops or deliver or route on_fail");
            }
            if child.access == AccessMode::WorktreeWrite {
                if writer
                    .as_ref()
                    .is_some_and(|path| Some(path) != child.worktree.as_ref())
                {
                    return Err("loop writers must share one worktree");
                }
                writer = child.worktree.clone();
            }
            for input in &child.inputs {
                if let Some(previous) = input.node.strip_prefix("previous.")
                    && !spec.body.iter().any(|n| n.id == previous)
                {
                    return Err("unknown previous round input");
                }
            }
            child.inputs.retain(|i| !i.node.starts_with("previous."));
        }
        let body_document = GraphDocument {
            nodes: body,
            delivery: None,
            ..document.clone()
        };
        let generous = crate::config::GraphLimits {
            nodes: 16384,
            edges: usize::MAX,
            graph_prompt_bytes: usize::MAX,
            node_prompt_bytes: usize::MAX,
            routes_per_node: u16::MAX as usize,
            ..Default::default()
        };
        let (plan, positions) = validate_flat_plan(&body_document, &generous)?;
        let Some(gate) = positions.get(&spec.until).map(|i| &plan.nodes[*i]) else {
            return Err("loop until must name a body gate");
        };
        if gate.kind != Some(NodeKind::Gate) {
            return Err("loop until must name a body gate");
        }
        let mut ancestors = std::collections::BTreeSet::new();
        let mut pending = vec![spec.until.clone()];
        while let Some(name) = pending.pop() {
            if ancestors.insert(name.clone()) {
                pending.extend(plan.nodes[positions[&name]].depends_on.clone());
            }
        }
        if ancestors.len() != spec.body.len() {
            return Err("loop gate must follow the entire body");
        }
        for round in 1..=rounds {
            for child in &spec.body {
                let mut child = child.clone();
                let local = child.id.clone();
                child.id = id(&node.id, round, &local);
                if !valid_identifier(&child.id) {
                    return Err("expanded loop node id exceeds 64 bytes");
                }
                child.round = Some(LoopRound {
                    loop_id: node.id.clone(),
                    round,
                    node_id: local.clone(),
                    until: local == spec.until,
                    previous_gate: (round > 1).then(|| id(&node.id, round - 1, &spec.until)),
                });
                child.depends_on = child
                    .depends_on
                    .iter()
                    .map(|name| id(&node.id, round, name))
                    .collect();
                if child.depends_on.is_empty() {
                    if round == 1 {
                        child.depends_on.extend(node.depends_on.clone());
                    } else {
                        child.depends_on.push(id(&node.id, round - 1, &spec.until));
                    }
                }
                child
                    .inputs
                    .retain(|i| round > 1 || !i.node.starts_with("previous."));
                for input in &mut child.inputs {
                    input.node = if let Some(previous) = input.node.strip_prefix("previous.") {
                        id(&node.id, round - 1, previous)
                    } else {
                        id(&node.id, round, &input.node)
                    };
                    if !child.depends_on.contains(&input.node) {
                        child.depends_on.push(input.node.clone());
                    }
                }
                expanded.nodes.push(child);
            }
        }
        let mut boundary = node.clone();
        boundary.depends_on.push(id(&node.id, rounds, &spec.until));
        expanded.nodes.push(boundary);
    }
    Ok(expanded)
}

pub(super) fn inactive_round(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    node.round
        .as_ref()
        .and_then(|r| r.previous_gate.as_ref())
        .is_some_and(|previous| {
            matches!(
                checkpoint.nodes[positions[previous]].state,
                NodeState::Succeeded | NodeState::Skipped
            )
        })
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
            r.until
                && checkpoint
                    .nodes
                    .iter()
                    .any(|n| n.id == r.loop_id && n.state == NodeState::Succeeded)
        })
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
            prior.remove(&n.id).unwrap_or_else(|| NodeProgress {
                round: n.round.clone(),
                loop_decision_actor: None,
                gate: None,
                title: Some(n.title.as_deref().unwrap_or(&n.id).into()),
                worktree: n.worktree.clone(),
                id: n.id.clone(),
                state: NodeState::Pending,
                advance_error: None,
                outputs: None,
                attempts: vec![],
            })
        })
        .collect();
}

pub(super) fn validate_progress(
    progress: &NodeProgress,
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Result<(), ()> {
    if progress.loop_decision_actor.is_none()
        || !progress.attempts.is_empty()
        || progress.gate.is_some()
        || progress.advance_error.is_some()
    {
        return Err(());
    }
    let body: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter(|n| n.round.as_ref().is_some_and(|r| r.loop_id == node.id))
        .collect();
    let passed = body
        .iter()
        .any(|n| n.round.as_ref().is_some_and(|r| r.until) && n.state == NodeState::Succeeded);
    let failed = body.iter().any(|n| {
        matches!(n.state, NodeState::Failed | NodeState::Blocked)
            && !n.round.as_ref().is_some_and(|r| r.until)
    });
    let settled = body.iter().all(|n| n.state.settled());
    let accepted = checkpoint
        .extensions
        .contains(&format!("loop.{}.decision=accept", node.id));
    let stopped = checkpoint
        .extensions
        .contains(&format!("loop.{}.decision=stop", node.id));
    let valid = match progress.state {
        NodeState::Succeeded => {
            !failed
                && settled
                && (passed || accepted)
                && progress.outputs == Some(gate_outputs(true))
        }
        NodeState::Failed => (failed || stopped) && progress.outputs == Some(gate_outputs(false)),
        NodeState::Pending => progress.outputs.is_none(),
        NodeState::Blocked | NodeState::Skipped => {
            progress.outputs.is_none()
                && node.depends_on.iter().any(|parent| {
                    matches!(
                        checkpoint.nodes[positions[parent]].state,
                        NodeState::Failed | NodeState::Blocked | NodeState::Skipped
                    )
                })
        }
        NodeState::Running => false,
    };
    if valid { Ok(()) } else { Err(()) }
}

pub(super) fn settle<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    spec: &LoopSpec,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<Option<String>, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[index];
    let mut external = node.clone();
    external
        .depends_on
        .retain(|parent| plan.nodes[positions[parent]].round.is_none());
    if let Some(state) = dependency_state(&external, plan, checkpoint, positions) {
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
    let failed = body.iter().any(|n| {
        matches!(n.state, NodeState::Failed | NodeState::Blocked)
            && !n.round.as_ref().is_some_and(|r| r.until)
    });
    let passed = body
        .iter()
        .any(|n| n.round.as_ref().is_some_and(|r| r.until) && n.state == NodeState::Succeeded);
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
    let state = if failed || decision.as_deref() == Some("stop") {
        Some(NodeState::Failed)
    } else if passed || decision.as_deref() == Some("accept") && settled {
        Some(NodeState::Succeeded)
    } else {
        None
    };
    if let Some(state) = state {
        let progress = &mut checkpoint.nodes[index];
        progress.state = state;
        progress.outputs = Some(gate_outputs(state == NodeState::Succeeded));
        if checkpoint
            .pause_reason
            .as_ref()
            .is_some_and(|r| r.starts_with(&format!("loop {} exhausted", node.id)))
        {
            checkpoint.pause_reason = None;
        }
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    } else if settled && dependency_state(node, plan, checkpoint, positions).is_none() {
        checkpoint.pause_reason = Some(format!(
            "loop {} exhausted after {rounds} rounds; stop, extend or accept as is",
            node.id
        ));
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        if let (Some(session), Some(actor)) = (
            &spec.decision_session,
            &checkpoint.nodes[index].loop_decision_actor,
        ) && let Some(decision) = owner
            .loop_decision(&checkpoint.graph_id, &node.id, rounds, session, actor)
            .map_err(GraphError::Run)?
        {
            let extension = match decision {
                LoopDecision::Stop => Ok(format!("loop.{}.decision=stop", node.id)),
                LoopDecision::Accept => Ok(format!("loop.{}.decision=accept", node.id)),
                LoopDecision::Extend { rounds: extra } => rounds
                    .checked_add(extra)
                    .filter(|_| extra > 0)
                    .map(|total| format!("loop.{}.rounds={total}", node.id))
                    .ok_or_else(|| {
                        "loop extension must be positive and cannot overflow".to_owned()
                    }),
                LoopDecision::Invalid { reason } => Err(reason),
            };
            match extension {
                Ok(extension) => return Ok(Some(extension)),
                Err(reason) => pause_invalid_decision(checkpoint, &node.id, &reason, journal)?,
            }
        }
    }
    Ok(None)
}

/// A rejected immutable answer remains inspectable; explicit resume can resolve it.
pub(super) fn pause_invalid_decision<R: Error + 'static, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    node: &str,
    reason: &str,
    journal: &mut J,
) -> Result<(), GraphError<R, J::Error>> {
    checkpoint.pause_reason = Some(format!(
        "loop {node} exhausted; inbox decision cannot be applied: {}; use graph resume --extend loop.{node}.rounds=N or --extend loop.{node}.decision=stop|accept",
        bounded(reason, MAX_ADVANCE_ERROR_BYTES)
    ));
    journal.save(checkpoint).map_err(GraphError::Journal)
}

pub(super) fn prepare_decision_extension(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    extension: &str,
    limits: &crate::config::GraphLimits,
) -> Result<(), String> {
    validate_resume_extension(document, checkpoint, extension)?;
    let mut extensions = checkpoint.extensions.clone();
    extensions.push(extension.to_owned());
    let expanded = expand(document, &extensions).map_err(str::to_owned)?;
    validate_flat_plan(&expanded, limits).map_err(str::to_owned)?;
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
