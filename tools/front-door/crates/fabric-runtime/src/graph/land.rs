//! Writer continuation and graph `land` delivery.
//!
//! A node that `continues` an upstream writer reuses its worktree and delivery
//! branch: the host fast-forward pushes the new commits to that branch only,
//! so a repair updates the same pull request. A `land` node merges a writer's
//! pull request once a verdict gate and a CI gate downstream of the writer have
//! passed. The host's risk-tier policy decides whether it lands at once or
//! raises a node ask first; the record keeps who or what authorised it.
use super::*;
use crate::config::{LandChoice, LandPolicy, LandSource};
use provenant_records::run::DeliveryMode;
use provenant_records::workflow::RiskTier;
use std::collections::{BTreeMap, BTreeSet};

/// Land the pull request of `node`, a writer that delivers `pr`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub node: String,
    pub tier: RiskTier,
    /// Inbox session for a tier whose policy asks; falls back to the graph's
    /// `decision_session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// What authorised a land.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LandAuthority {
    /// The tier policy was `auto`.
    Policy,
    /// An answer of `land` to this node ask.
    Ask { ask_id: String, actor: String },
}

/// A land node's pinned subject and decision, kept as the receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandRecord {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub tier: RiskTier,
    pub policy: LandPolicy,
    pub policy_source: LandSource,
    pub branch: String,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorised_by: Option<LandAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u64>,
    /// The host's latest pending or merge note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Next retry of a pending land, which tick wake discovery includes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<provenant_core::WallTime>,
    /// Pending land observations already made, which selects the next backoff.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub pending_observations: u64,
}

/// One authorised land, pinned to the head the reviews and CI saw.
#[derive(Clone, Copy, Debug)]
pub struct LandRequest<'a> {
    pub graph_id: &'a str,
    pub node_id: &'a str,
    pub branch: &'a str,
    pub head: &'a str,
    pub tier: RiskTier,
    pub authority: &'a LandAuthority,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandObservation {
    Merged {
        pull_request: Option<u64>,
        note: Option<String>,
    },
    /// Not merged yet, such as checks still running. Observe again later.
    Pending { reason: String },
    /// The land cannot proceed; the node fails with this reason.
    Refused { reason: String },
}

pub(super) const LAND: &str = "land";
pub(super) const HOLD: &str = "hold";
pub(super) const LANDED: &str = "LANDED";
const PUSH: DeliveryMode = DeliveryMode::Push;

/// The delivery a node launches with: a continuing writer pushes.
pub(super) fn delivery(node: &PlannedNode) -> Option<&DeliveryMode> {
    if node.continues.is_some() {
        Some(&PUSH)
    } else {
        node.delivery.as_ref()
    }
}

/// Static checks on one declared node.
pub(super) fn validate_node(node: &GraphNode) -> Result<(), &'static str> {
    if let Some(target) = &node.continues
        && (node.access != AccessMode::WorktreeWrite
            || node.kind.is_some_and(|kind| kind != NodeKind::Task)
            || node.delivery.is_some()
            || node.worktree.is_none()
            || target == &node.id)
    {
        return Err(
            "continues requires a writer that names the target's worktree and no delivery of its own",
        );
    }
    if let Some(spec) = &node.land
        && (node.route.is_some()
            || node.routes.is_some()
            || node.access != AccessMode::ReadOnly
            || node.delivery.is_some()
            || node.worktree.is_some()
            || !node.prompt.is_empty()
            || node.required_verdict.is_some()
            || !node.inputs.is_empty()
            || node.on_fail.is_some()
            || node.continues.is_some()
            || !valid_identifier(&spec.node)
            || spec
                .session
                .as_ref()
                .is_some_and(|session| crate::inbox::session_id(session).is_err()))
    {
        return Err(
            "land takes a writer node, a tier and an optional session, and no agent configuration",
        );
    }
    Ok(())
}

fn ancestors(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    id: &str,
) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut pending: Vec<usize> = positions.get(id).into_iter().copied().collect();
    while let Some(index) = pending.pop() {
        for parent in nodes[index].dependencies() {
            if let Some(parent) = positions.get(parent)
                && seen.insert(*parent)
            {
                pending.push(*parent);
            }
        }
    }
    seen
}

/// Checks that need the whole acyclic plan.
pub(super) fn validate_relations(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> Result<(), &'static str> {
    for node in nodes {
        if let Some(target) = &node.continues {
            let upstream = ancestors(nodes, positions, &node.id);
            let valid = positions.get(target).is_some_and(|index| {
                let writer = &nodes[*index];
                upstream.contains(index)
                    && plans::writer(writer)
                    && matches!(writer.delivery, Some(DeliveryMode::Pr | DeliveryMode::Push))
                    && writer.continues.is_none()
                    && writer.worktree == node.worktree
            });
            if !valid {
                return Err(
                    "continues must name an upstream writer that delivers pr or push from the same worktree",
                );
            }
        }
        if let Some(spec) = &node.land {
            let upstream = ancestors(nodes, positions, &node.id);
            let Some(writer) = positions.get(&spec.node).copied().filter(|index| {
                let writer = &nodes[*index];
                upstream.contains(index)
                    && plans::writer(writer)
                    && writer.delivery == Some(DeliveryMode::Pr)
                    && writer.continues.is_none()
            }) else {
                return Err("land must follow a writer that delivers pr");
            };
            let gate_after_writer = |ci: bool| {
                upstream.iter().any(|index| {
                    let gate = &nodes[*index];
                    let kind = match &gate.condition {
                        Some(GateCondition::Ci { .. }) => true,
                        Some(GateCondition::Verdict { .. }) => false,
                        _ => return false,
                    };
                    kind == ci && ancestors(nodes, positions, &gate.id).contains(&writer)
                })
            };
            if !gate_after_writer(true) || !gate_after_writer(false) {
                return Err("land must follow a verdict gate and a CI gate after its writer");
            }
        }
    }
    Ok(())
}

/// The bound branch of a continuing writer, from the writer it continues.
pub(super) fn continued_branch(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Option<String> {
    let target = positions.get(node.continues.as_ref()?)?;
    checkpoint.nodes[*target].outputs.as_ref()?.branch.clone()
}

/// The branch and newest commit delivered by `writer` and the writers that
/// continue it, ordered by settlement.
pub(super) fn delivered_head(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    writer: &str,
) -> Option<(String, String)> {
    delivered_within(plan, checkpoint, writer, |_| true)
}

/// As [`delivered_head`], counting only the chain nodes `include` accepts.
fn delivered_within(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    writer: &str,
    include: impl Fn(usize) -> bool,
) -> Option<(String, String)> {
    plan.nodes
        .iter()
        .zip(&checkpoint.nodes)
        .enumerate()
        .filter(|(index, (node, progress))| {
            (node.id == writer || node.continues.as_deref() == Some(writer))
                && progress.state == NodeState::Succeeded
                && include(*index)
        })
        .filter_map(|(_, (_, progress))| {
            let outputs = progress.outputs.as_ref()?;
            let settled = progress
                .attempts
                .last()
                .map_or(0, |attempt| attempt.settled);
            Some((settled, outputs.branch.clone()?, outputs.commit.clone()?))
        })
        .max_by_key(|(settled, _, _)| *settled)
        .map(|(_, branch, commit)| (branch, commit))
}

/// The head a reviewer judged: its pinned source commit or, without one, the
/// newest head of `writer`'s chain among its own upstream nodes. A review that
/// ran before a continuation therefore never judges the continuation's head.
fn reviewed_head(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    reviewer: &str,
    writer: &str,
) -> Option<String> {
    let index = *positions.get(reviewer)?;
    if let Some(commit) = &checkpoint.nodes[index].source_commit {
        return Some(commit.clone());
    }
    let upstream = ancestors(&plan.nodes, positions, reviewer);
    delivered_within(plan, checkpoint, writer, |index| upstream.contains(&index))
        .map(|(_, head)| head)
}

/// Why no passing verdict gate after `writer` accepted `head`, if none did.
/// Landing needs a gate that passed and whose every input reviewed `head`; a
/// failed gate whose `on_fail` reaches the land is not acceptance.
fn acceptance_error(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    land: &str,
    writer: &str,
    head: &str,
) -> Option<String> {
    let writer_index = positions.get(writer).copied();
    let mut reviewed = Vec::new();
    for index in ancestors(&plan.nodes, positions, land) {
        let gate = &plan.nodes[index];
        let progress = &checkpoint.nodes[index];
        if !matches!(gate.condition, Some(GateCondition::Verdict { .. }))
            || !writer_index
                .is_some_and(|writer| ancestors(&plan.nodes, positions, &gate.id).contains(&writer))
            || progress.state != NodeState::Succeeded
            || !progress.gate.as_ref().is_some_and(|gate| gate.passed)
        {
            continue;
        }
        let subjects: Vec<Option<String>> = gate
            .inputs
            .iter()
            .map(|input| reviewed_head(plan, checkpoint, positions, &input.node, writer))
            .collect();
        if !subjects.is_empty()
            && subjects
                .iter()
                .all(|subject| subject.as_deref() == Some(head))
        {
            return None;
        }
        let subjects: Vec<&str> = subjects
            .iter()
            .map(|subject| subject.as_deref().unwrap_or("no head"))
            .collect();
        reviewed.push(format!("{} passed {}", gate.id, subjects.join(", ")));
    }
    let evidence = if reviewed.is_empty() {
        "no verdict gate after the writer passed".to_owned()
    } else {
        reviewed.join("; ")
    };
    Some(format!(
        "node {land}: no passing verdict gate after {writer} reviewed the land head {head} ({evidence}); route the newest head through review before land"
    ))
}

pub(super) fn session<'a>(spec: &'a LandSpec, plan: &'a PlannedGraph) -> Option<&'a str> {
    spec.session.as_deref().or(plan.decision_session.as_deref())
}

pub(super) fn sequence(ask: &NodeAsk, node_id: &str) -> Option<u64> {
    ask.ask_id
        .strip_prefix(node_id)?
        .strip_prefix(".ask-")?
        .parse()
        .ok()
        .filter(|sequence| *sequence > 0)
}

fn source_name(source: LandSource) -> &'static str {
    match source {
        LandSource::Default => "default",
        LandSource::Config => "host config",
        LandSource::Project => "project config",
    }
}

fn tier_name(tier: RiskTier) -> &'static str {
    match tier {
        RiskTier::Routine => "routine",
        RiskTier::Substantial => "substantial",
        RiskTier::Crucial => "crucial",
        RiskTier::Terminal => "terminal",
    }
}

pub(super) fn land_ask(
    graph_id: &str,
    node: &PlannedNode,
    record: &LandRecord,
    session: &str,
    sequence: u64,
    previous: Option<&str>,
) -> NodeAsk {
    let previous = previous
        .map(|reason| format!("\n\nThe previous answer was not applied: {reason}"))
        .unwrap_or_default();
    let mut ask = asks::ask(
        format!("{}.ask-{sequence}", node.id),
        AskKind::Land,
        session,
        format!(
            "Graph land: {}",
            prefix(node.title.as_deref().unwrap_or(&node.id), 200)
        ),
        format!(
            "Graph {graph_id}, node {} is ready to land {} at {}.\n\nThe {} tier asks first ({}). Choose land to merge through the repository's land workflow, pinned to this head, or hold to stop.{previous}\n\nThis decision does not replace review or CI.",
            node.id,
            record.branch,
            record.head,
            tier_name(record.tier),
            source_name(record.policy_source),
        ),
    );
    ask.node = Some(node.id.clone());
    ask.options = vec![
        asks::option(LAND, "Land", "Merge the pull request at the pinned head."),
        asks::option(HOLD, "Hold", "Do not merge; the land node fails."),
    ];
    ask
}

fn fail<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    reason: String,
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let progress = &mut checkpoint.nodes[index];
    progress.state = NodeState::Failed;
    progress.outputs = None;
    progress.advance_error = Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
    journal.save(checkpoint).map_err(GraphError::Journal)
}

fn outputs(record: &LandRecord) -> NodeOutputs {
    let authority = match &record.authorised_by {
        Some(LandAuthority::Ask { ask_id, actor }) => format!("{actor} answered land to {ask_id}"),
        _ => format!(
            "the {} tier policy is auto ({})",
            tier_name(record.tier),
            source_name(record.policy_source)
        ),
    };
    let subject = record
        .pull_request
        .map_or_else(String::new, |number| format!("#{number} "));
    let excerpt = bounded(
        &format!(
            "landed {subject}{} at {}; authorised because {authority}",
            record.branch, record.head
        ),
        OUTPUT_FIELD_BYTES,
    );
    NodeOutputs {
        extra: Default::default(),
        verdict_error: None,
        schema_version: 2,
        result: Some(ResultOutput {
            extra: Default::default(),
            digest: CanonicalV1::file_digest(excerpt.as_bytes()).to_string(),
            excerpt,
            truncated: false,
            path: None,
        }),
        verdict: Some(LANDED.into()),
        findings: Vec::new(),
        branch: Some(record.branch.clone()),
        commit: Some(record.head.clone()),
        warnings: Vec::new(),
    }
}

/// Pin, authorise and land. Each step is saved before the next, so a replay
/// never asks twice or merges a different head.
#[allow(clippy::too_many_arguments)]
pub(super) fn advance<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    node: &PlannedNode,
    spec: &LandSpec,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    if checkpoint.nodes[index].state.settled() {
        return Ok(());
    }
    if checkpoint.nodes[index].land.is_none() {
        let Some((branch, head)) = delivered_head(plan, checkpoint, &spec.node) else {
            return fail::<R, J>(
                index,
                format!(
                    "node {}: writer {} delivered no branch and commit to land",
                    node.id, spec.node
                ),
                checkpoint,
                journal,
            );
        };
        if let Some(reason) =
            acceptance_error(plan, checkpoint, positions, &node.id, &spec.node, &head)
        {
            return fail::<R, J>(index, reason, checkpoint, journal);
        }
        let LandChoice { policy, source } = owner.land_policy(spec.tier);
        let progress = &mut checkpoint.nodes[index];
        progress.land = Some(LandRecord {
            extra: Default::default(),
            tier: spec.tier,
            policy,
            policy_source: source,
            branch: bounded(&branch, OUTPUT_FIELD_BYTES),
            head: bounded(&head, OUTPUT_FIELD_BYTES),
            authorised_by: (policy == LandPolicy::Auto).then_some(LandAuthority::Policy),
            pull_request: None,
            note: None,
            wake_at: None,
            pending_observations: 0,
        });
        progress.state = NodeState::Running;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let mut record = checkpoint.nodes[index]
        .land
        .clone()
        .ok_or(GraphError::BadCheckpoint)?;
    if record.authorised_by.is_none() {
        if !authorise(index, node, spec, plan, &record, checkpoint, owner, journal)? {
            return Ok(());
        }
        record = checkpoint.nodes[index]
            .land
            .clone()
            .ok_or(GraphError::BadCheckpoint)?;
    }
    let authority = record
        .authorised_by
        .clone()
        .ok_or(GraphError::BadCheckpoint)?;
    let observation = owner
        .land(LandRequest {
            graph_id: &plan.graph_id,
            node_id: &node.id,
            branch: &record.branch,
            head: &record.head,
            tier: record.tier,
            authority: &authority,
        })
        .map_err(GraphError::Run)?;
    record.wake_at = None;
    match observation {
        LandObservation::Merged { pull_request, note } => {
            record.pull_request = pull_request;
            record.note = note.map(|note| bounded(&note, MAX_ADVANCE_ERROR_BYTES));
            let progress = &mut checkpoint.nodes[index];
            progress.outputs = Some(outputs(&record));
            progress.land = Some(record);
            progress.state = NodeState::Succeeded;
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)
        }
        LandObservation::Pending { reason } => {
            // Persist the retry so a scheduled tick wakes the graph again.
            let id = &node.id;
            let now = owner.wall_now().ok_or_else(|| {
                GraphError::Refused(format!(
                    "node {id}: no wall clock to set its land retry deadline; retry the advance"
                ))
            })?;
            let delay = owner
                .ci_poll_backoff(id)
                .delay_seconds(record.pending_observations)
                .saturating_mul(1_000_000);
            let wake_at = now
                .checked_add(provenant_core::DurationMicros::from_micros(delay))
                .map_err(|_| {
                    GraphError::Refused(format!(
                        "node {id}: land retry deadline passes the last representable wall time; lower ci_poll_backoff"
                    ))
                })?;
            record.note = Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
            record.wake_at = Some(wake_at);
            record.pending_observations = record.pending_observations.saturating_add(1);
            checkpoint.nodes[index].land = Some(record);
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)
        }
        LandObservation::Refused { reason } => {
            checkpoint.nodes[index].land = Some(record);
            asks::refresh_wake(checkpoint);
            fail::<R, J>(
                index,
                format!("node {}: land refused: {reason}", node.id),
                checkpoint,
                journal,
            )
        }
    }
}

/// Raise or poll the land ask. True once an answer of `land` is saved.
#[allow(clippy::too_many_arguments)]
fn authorise<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    node: &PlannedNode,
    spec: &LandSpec,
    plan: &PlannedGraph,
    record: &LandRecord,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<bool, GraphError<R::Error, J::Error>> {
    let mine = |ask: &NodeAsk| ask.node.as_deref() == Some(node.id.as_str());
    let latest = checkpoint.asks.iter().rposition(mine);
    let position = match latest {
        Some(position) if !checkpoint.asks[position].resolved => position,
        // A rejected answer is replaced once its inbox request is closed.
        Some(position) if !checkpoint.asks[position].closed => return Ok(false),
        _ => {
            let Some(session) = session(spec, plan).map(str::to_owned) else {
                fail::<R, J>(
                    index,
                    format!(
                        "node {}: the {} tier asks before landing, but neither the land node nor the graph names a decision session; set land.session or decision_session, or land {} at {} by hand with provenant land",
                        node.id,
                        tier_name(record.tier),
                        record.branch,
                        record.head
                    ),
                    checkpoint,
                    journal,
                )?;
                return Ok(false);
            };
            asks::identity::<R, J::Error>(checkpoint, owner)?;
            let previous = latest.map(|position| &checkpoint.asks[position]);
            let next = previous
                .and_then(|ask| sequence(ask, &node.id))
                .unwrap_or(0)
                .saturating_add(1);
            let rejected = previous
                .and_then(|ask| ask.answer.as_ref())
                .and_then(|answer| answer.rejected.as_deref());
            let ask = land_ask(&plan.graph_id, node, record, &session, next, rejected);
            checkpoint.asks.retain(|ask| !mine(ask));
            checkpoint.asks.push(ask);
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            checkpoint.asks.len() - 1
        }
    };
    let before = checkpoint.asks[position].inbox_error.clone();
    let reply = asks::poll(checkpoint, position, owner);
    let Ok(Some(reply)) = reply else {
        if checkpoint.asks[position].inbox_error != before {
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        return Ok(false);
    };
    let mut answer = asks::answered(reply);
    let chosen = answer
        .option
        .clone()
        .or_else(|| answer.text.as_deref().map(|text| text.trim().to_owned()))
        .filter(|id| id == LAND || id == HOLD);
    if answer.rejected.is_none() && chosen.is_none() {
        answer.rejected = Some("answer must choose land or hold".into());
    }
    let ask_id = checkpoint.asks[position].ask_id.clone();
    let actor = answer.actor.clone().unwrap_or_default();
    let mut authorised = false;
    if answer.rejected.is_none() {
        answer.option = chosen.clone();
        if chosen.as_deref() == Some(LAND) {
            if let Some(land) = checkpoint.nodes[index].land.as_mut() {
                land.authorised_by = Some(LandAuthority::Ask {
                    ask_id: ask_id.clone(),
                    actor,
                });
            }
            authorised = true;
        } else {
            let progress = &mut checkpoint.nodes[index];
            progress.state = NodeState::Failed;
            progress.advance_error = Some(bounded(
                &format!("node {}: land held by {actor} in {ask_id}", node.id),
                MAX_ADVANCE_ERROR_BYTES,
            ));
        }
    }
    let ask = &mut checkpoint.asks[position];
    ask.answer = Some(answer);
    ask.resolved = true;
    asks::refresh_wake(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)?;
    Ok(authorised)
}

pub(super) fn validate_progress(
    progress: &NodeProgress,
    checkpoint: &GraphCheckpoint,
) -> Result<(), ()> {
    let authority = progress
        .land
        .as_ref()
        .and_then(|record| record.authorised_by.as_ref());
    let authority_valid = match authority {
        Some(LandAuthority::Ask { ask_id, .. }) => checkpoint.asks.iter().any(|ask| {
            &ask.ask_id == ask_id
                && ask.kind == AskKind::Land
                && ask.node.as_deref() == Some(progress.id.as_str())
                && ask
                    .answer
                    .as_ref()
                    .is_some_and(|answer| answer.option.as_deref() == Some(LAND))
        }),
        Some(LandAuthority::Policy) => progress
            .land
            .as_ref()
            .is_some_and(|record| record.policy == LandPolicy::Auto),
        None => true,
    };
    let valid = authority_valid
        && (progress.state == NodeState::Running
            || progress
                .land
                .as_ref()
                .is_none_or(|record| record.wake_at.is_none()))
        && progress.attempts.is_empty()
        && progress.gate.is_none()
        && progress.ci.is_none()
        && (progress.advance_error.is_none() || progress.state == NodeState::Failed)
        && match progress.state {
            NodeState::Pending | NodeState::Blocked | NodeState::Skipped => {
                progress.land.is_none() && progress.outputs.is_none()
            }
            NodeState::Running => progress.land.is_some() && progress.outputs.is_none(),
            NodeState::Succeeded => {
                authority.is_some()
                    && progress
                        .outputs
                        .as_ref()
                        .is_some_and(|outputs| outputs.verdict.as_deref() == Some(LANDED))
            }
            NodeState::Failed => progress.outputs.is_none(),
        };
    if valid { Ok(()) } else { Err(()) }
}
