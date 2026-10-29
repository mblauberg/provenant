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
    #[serde(
        default = "default_extension_rounds",
        skip_serializing_if = "is_default_extension_rounds"
    )]
    pub max_extension_rounds: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_session: Option<String>,
}

fn default_extension_rounds() -> usize {
    5
}
fn is_default_extension_rounds(value: &usize) -> bool {
    *value == 5
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRound {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
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

pub(super) fn validate_extension(
    document: &GraphDocument,
    extension: &str,
    limits: &crate::config::GraphLimits,
) -> Result<(), String> {
    let flattened = composites::expand(document, limits)?;
    let document = &flattened;
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
    limits: &crate::config::GraphLimits,
) -> Result<(), String> {
    validate_extension(document, extension, limits)?;
    let flattened = composites::expand(document, limits)?;
    let document = &flattened;
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
        let target = value
            .parse::<usize>()
            .map_err(|_| "invalid loop allowance")?;
        if target <= current {
            return Err("loop allowance must increase".into());
        }
        if target - current
            > original
                .loop_body
                .as_ref()
                .map_or(5, |spec| spec.max_extension_rounds)
        {
            return Err("loop extension exceeds max_extension_rounds".into());
        }
    }
    Ok(())
}

pub(super) fn expand(
    document: &GraphDocument,
    extensions: &[String],
    limits: &crate::config::GraphLimits,
) -> Result<GraphDocument, String> {
    let flattened = composites::expand(document, limits)?;
    Ok(expand_loops(&flattened, extensions)?)
}

fn expand_loops(
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
            || node.required_verdict.is_some()
            || spec.body.is_empty()
            || spec.max_rounds == 0
            || spec.max_rounds > 1024
            || spec.max_extension_rounds == 0
            || spec.max_extension_rounds > 1024
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
            if child.land.is_some() {
                return Err("loop body cannot land");
            }
            // A continued writer is outside the body; the expanded plan checks it.
            child.continues = None;
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
            // Prior-round and outer inputs are checked against the expanded
            // graph; the isolated body check has only the local namespace.
            child
                .inputs
                .retain(|i| spec.body.iter().any(|node| node.id == i.node));
            // A head source naming a writer outside the body is checked
            // against the expanded graph, as an external CI subject is.
            if let Some(SourceBinding::Head { node: writer }) = &child.source
                && !spec.body.iter().any(|node| &node.id == writer)
            {
                child.source = None;
            }
            // External CI subjects are checked against the expanded graph.
            // The isolated body check below has only the local namespace.
            if let Some(GateCondition::Ci { target, .. }) = &mut child.condition
                && let CiTarget::Node {
                    node: target_node, ..
                } = target
                && !spec.body.iter().any(|node| &node.id == target_node)
            {
                *target = CiTarget::Ref(target_node.clone());
            }
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
                pending.extend(plan.nodes[positions[&name]].dependencies().cloned());
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
                    extra: Default::default(),
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
                if let Some(SourceBinding::Node { node: source }) = &mut child.source {
                    *source = if let Some(previous) = source.strip_prefix("previous.") {
                        if round == 1 {
                            return Err("previous source requires an initial commit binding");
                        }
                        id(&node.id, round - 1, previous)
                    } else {
                        id(&node.id, round, source)
                    };
                    if !child.depends_on.contains(source) {
                        child.depends_on.push(source.clone());
                    }
                }
                if let Some(SourceBinding::Head { node: writer }) = &mut child.source
                    && spec.body.iter().any(|node| &node.id == writer)
                {
                    *writer = id(&node.id, round, writer);
                }
                if let Some(GateCondition::Ci {
                    target: CiTarget::Node { node: target, .. },
                    ..
                }) = &mut child.condition
                {
                    *target = if let Some(previous) = target.strip_prefix("previous.") {
                        if round == 1 {
                            return Err("previous CI target requires an initial node binding");
                        }
                        id(&node.id, round - 1, previous)
                    } else if spec.body.iter().any(|node| &node.id == target) {
                        id(&node.id, round, target)
                    } else {
                        target.clone()
                    };
                }
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
                    } else if spec.body.iter().any(|body| body.id == input.node) {
                        id(&node.id, round, &input.node)
                    } else {
                        // An outer node, such as the writer a repair continues,
                        // is selected afresh in every round.
                        input.node.clone()
                    };
                    if !child.depends_on.contains(&input.node) {
                        child.depends_on.push(input.node.clone());
                    }
                }
                if round == 1
                    && spec
                        .body
                        .iter()
                        .find(|n| n.id == local)
                        .is_some_and(|n| n.depends_on.is_empty())
                {
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
                extra: Default::default(),
                children: n.composite_members.clone(),
                source_commit: None,
                fragment: None,
                handoff: None,
                expansion_pause: None,
                loop_pause: None,
                round: n.round.clone(),
                gate: None,
                separation: None,
                ci: None,
                land: None,
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

/// Node fields that held loop decisions before node asks. A checkpoint that
/// still carries them is not migrated: it can be cancelled, then rerun.
const LEGACY_LOOP_FIELDS: [&str; 3] = ["loop_asks", "loop_inbox_error", "loop_decision_actor"];

/// An inbox request a loop raised before node asks, not yet closed. Cancel
/// closes it under its original operation identity; it is never migrated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyLoopAsk {
    pub node_id: String,
    pub rounds: u64,
    pub session: String,
    /// The actor the request was raised under.
    pub actor: String,
}

/// Unclosed loop requests recorded in pre-ask node fields.
pub fn legacy_loop_asks(checkpoint: &GraphCheckpoint) -> Vec<LegacyLoopAsk> {
    let mut asks = Vec::new();
    for node in &checkpoint.nodes {
        let Some(actor) = node
            .extra
            .get("loop_decision_actor")
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let stored = node.extra.get("loop_asks").and_then(|v| v.as_array());
        for ask in stored.into_iter().flatten() {
            if ask.get("closed").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            if let (Some(rounds), Some(session)) = (
                ask.get("rounds").and_then(|v| v.as_u64()),
                ask.get("session").and_then(|v| v.as_str()),
            ) {
                asks.push(LegacyLoopAsk {
                    node_id: node.id.clone(),
                    rounds,
                    session: session.to_owned(),
                    actor: actor.to_owned(),
                });
            }
        }
    }
    asks
}

/// Why a checkpoint written before node asks cannot advance, if it was.
pub(super) fn legacy(checkpoint: &GraphCheckpoint) -> Option<String> {
    checkpoint
        .nodes
        .iter()
        .any(|node| LEGACY_LOOP_FIELDS.iter().any(|key| node.extra.contains_key(*key)))
        .then(|| {
            let id = &checkpoint.graph_id;
            format!(
                "graph {id} checkpoint predates node asks and cannot advance; cancel it with `provenant fabric graph cancel {id}`, then rerun the document under a new graph_id"
            )
        })
}

pub(super) fn validate_progress(
    progress: &NodeProgress,
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Result<(), ()> {
    if checkpoint.ask_actor.is_none()
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
        checkpoint.nodes[index].loop_pause = None;
        refresh_pause(checkpoint);
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    } else if settled && dependency_state(node, plan, checkpoint, positions).is_none() {
        if checkpoint.nodes[index].loop_pause.is_none() {
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
        asks::identity::<R, J::Error>(checkpoint, owner)?;
        let ask_id = format!("{}.rounds-{rounds}", node.id);
        let position = match checkpoint.asks.iter().position(|ask| ask.ask_id == ask_id) {
            Some(position) => position,
            None => {
                let ask = asks::loop_ask(&checkpoint.graph_id, &node.id, rounds, session);
                checkpoint.asks.push(ask);
                checkpoint.asks.len() - 1
            }
        };
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        if checkpoint.asks[position].resolved {
            // A recorded answer is applied again after a crash; an ask
            // resolved without one leaves the decision to graph resume.
            if let Some(answer) = checkpoint.asks[position].answer.clone() {
                return apply(&node.id, rounds, &answer, checkpoint, journal);
            }
            if checkpoint.nodes[index]
                .loop_pause
                .as_ref()
                .is_none_or(|reason| !reason.contains("graph resume"))
            {
                checkpoint.nodes[index].loop_pause = Some(format!(
                    "loop {} exhausted after {rounds} rounds; ask closed; use graph resume --extend loop.{}.rounds=N or --extend loop.{}.decision=stop|accept",
                    node.id, node.id, node.id
                ));
                refresh_pause(checkpoint);
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
            return Ok(None);
        }
        match asks::poll(checkpoint, position, owner) {
            Ok(reply) => {
                checkpoint.nodes[index].loop_pause = Some(format!(
                    "loop {} exhausted after {rounds} rounds; stop, extend or accept as is",
                    node.id
                ));
                refresh_pause(checkpoint);
                let Some(reply) = reply else {
                    journal.save(checkpoint).map_err(GraphError::Journal)?;
                    return Ok(None);
                };
                let answer = asks::answered(reply);
                let ask = &mut checkpoint.asks[position];
                ask.answer = Some(answer.clone());
                ask.resolved = true;
                journal.save(checkpoint).map_err(GraphError::Journal)?;
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

/// Turn a recorded loop answer into its extension, or pause on a bad one.
fn apply<R: Error + 'static, J: GraphJournal>(
    node: &str,
    rounds: usize,
    answer: &asks::AskAnswer,
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
            for ask in &mut checkpoint.asks {
                if ask.node.as_deref() == Some(node)
                    && ask.rounds == Some(rounds)
                    && let Some(answer) = ask.answer.as_mut()
                {
                    answer
                        .rejected
                        .get_or_insert_with(|| bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
                }
            }
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
        progress.loop_pause = Some(format!(
            "loop {node} exhausted; decision cannot be applied: {}; use graph resume --extend loop.{node}.rounds=N or --extend loop.{node}.decision=stop|accept",
            bounded(reason, MAX_ADVANCE_ERROR_BYTES)
        ));
    }
    refresh_pause(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)
}

pub(super) fn prepare_decision_extension(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    extension: &str,
    limits: &crate::config::GraphLimits,
) -> Result<(), String> {
    validate_resume_extension(document, checkpoint, extension, limits)?;
    if extension
        .split_once('=')
        .is_some_and(|(key, _)| key.ends_with(".decision"))
    {
        return Ok(());
    }
    let mut extensions = checkpoint.extensions.clone();
    extensions.push(extension.to_owned());
    let expanded = plans::expand(document, &extensions, &checkpoint.expansions, limits)?;
    let (plan, _) = validate_flat_plan(&expanded, limits).map_err(str::to_owned)?;
    if let Some(reason) = structural_limit_reason(&expanded, &plan, limits) {
        return Err(reason);
    }
    if let Some(reason) = envelope::reason(document, &checkpoint.expansions, &extensions, &plan) {
        return Err(reason);
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
    if checkpoint
        .pause_reason
        .as_ref()
        .is_none_or(|r| r.starts_with("loops paused:") || r.starts_with("loop "))
    {
        checkpoint.pause_reason = (!reasons.is_empty()).then(|| {
            bounded(
                &format!("loops paused: {}", reasons.join("; ")),
                PAUSE_REASON_BYTES,
            )
        });
    }
}
