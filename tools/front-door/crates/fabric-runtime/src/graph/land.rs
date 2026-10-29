//! Writer continuation and graph `land` delivery.
//!
//! A node that `continues` an upstream writer reuses its worktree and delivery
//! branch: the host fast-forward pushes the new commits to that branch only,
//! so a repair updates the same pull request. A `land` node merges a writer's
//! pull request once a verdict gate and a CI gate downstream of the writer have
//! passed. The host's risk-tier policy decides whether it lands at once or
//! asks the chair, who lands by hand; the record keeps who or what authorised it.
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
    /// The tier asked in `ask_id`, and the chair merged the pull request at the
    /// pinned head with `provenant land`, which the node observed. An inbox
    /// answer never authorises a land: lanes and peers can write answers.
    Chair { ask_id: String },
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

/// One land, pinned to the head the reviews and CI saw.
#[derive(Clone, Copy, Debug)]
pub struct LandRequest<'a> {
    pub graph_id: &'a str,
    pub node_id: &'a str,
    pub branch: &'a str,
    pub head: &'a str,
    pub tier: RiskTier,
    /// `None` while the tier asks: the host only observes whether the chair
    /// merged the pull request at `head`, and never merges.
    pub authority: Option<&'a LandAuthority>,
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
        if let Some(SourceBinding::Head { node: target }) = &node.source {
            let upstream = ancestors(nodes, positions, &node.id);
            let valid = positions.get(target).is_some_and(|index| {
                let writer = &nodes[*index];
                upstream.contains(index)
                    && plans::writer(writer)
                    && matches!(writer.delivery, Some(DeliveryMode::Pr | DeliveryMode::Push))
                    && writer.continues.is_none()
            });
            if !valid {
                return Err("head source must name an upstream writer that delivers pr or push");
            }
        }
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

/// The head a reviewer judged, or `None` for a node that is not a reviewer.
/// The head is its pinned source commit or, without one, the
/// newest head of `writer`'s chain among its own upstream nodes. A review that
/// ran before a continuation therefore never judges the continuation's head.
/// A composite boundary has no subject of its own, so its verdict resolves to
/// the member that exported it: a council pinned to an earlier head never
/// vouches for a later one that merely precedes the boundary.
fn reviewed_head(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    reviewer: &str,
    writer: &str,
) -> Option<String> {
    let index = *positions.get(reviewer)?;
    let node = &plan.nodes[index];
    if let Some(spec) = &node.composite {
        let producer = spec
            .exports
            .iter()
            .find(|export| export.outputs.contains(&OutputKind::Verdict))?;
        return reviewed_head(plan, checkpoint, positions, &producer.node, writer);
    }
    // Only an agent attempt outside the writer's chain reviews. A gate, loop,
    // decision or land output, such as CI's PASS, is never review evidence.
    if node.condition.is_some()
        || node.loop_body.is_some()
        || node.decide.is_some()
        || node.land.is_some()
        || node.id == writer
        || node.continues.as_deref() == Some(writer)
        || checkpoint.nodes[index].attempts.is_empty()
    {
        return None;
    }
    if let Some(commit) = &checkpoint.nodes[index].source_commit {
        return Some(commit.clone());
    }
    chain_head(plan, checkpoint, positions, reviewer, writer)
}

/// The newest head of `writer`'s chain among `node`'s upstream nodes, which a
/// `head` source binding pins.
pub(super) fn chain_head(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    node: &str,
    writer: &str,
) -> Option<String> {
    let upstream = ancestors(&plan.nodes, positions, node);
    delivered_within(plan, checkpoint, writer, |index| upstream.contains(&index))
        .map(|(_, head)| head)
}

/// Attempt references of `writer` and of the writers that continue it among
/// `node`'s upstream nodes: every author of the head a `head` binding pins.
pub(super) fn chain_attempts(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    node: &str,
    writer: &str,
) -> Vec<String> {
    let upstream = ancestors(&plan.nodes, positions, node);
    let mut attempts: Vec<String> = plan
        .nodes
        .iter()
        .zip(&checkpoint.nodes)
        .enumerate()
        .filter(|(index, (chain, _))| {
            (chain.id == writer || chain.continues.as_deref() == Some(writer))
                && upstream.contains(index)
        })
        .flat_map(|(_, (_, progress))| &progress.attempts)
        .filter_map(|attempt| attempt.attempt_ref.clone())
        .collect();
    attempts.sort();
    attempts.dedup();
    attempts
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
) -> NodeAsk {
    let mut ask = asks::ask(
        format!("{}.ask-1", node.id),
        AskKind::Land,
        session,
        format!(
            "Graph land: {}",
            prefix(node.title.as_deref().unwrap_or(&node.id), 200)
        ),
        format!(
            "Graph {graph_id}, node {} is ready to land {} at {}.\n\nThe {} tier asks first ({}). To land, run `provenant land N --head {}` for the pull request N whose head is {}; the node observes the merge at this head and succeeds. Answer hold to stop. An answer cannot approve the land: any answer fails the node.\n\nThis decision does not replace review or CI.",
            node.id,
            record.branch,
            record.head,
            tier_name(record.tier),
            source_name(record.policy_source),
            record.head,
            record.branch,
        ),
    );
    ask.node = Some(node.id.clone());
    ask.options = vec![asks::option(
        HOLD,
        "Hold",
        "Do not merge; the land node fails.",
    )];
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
        Some(LandAuthority::Chair { ask_id }) => {
            format!("the chair landed it by hand after {ask_id}")
        }
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
        remaining: None,
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
    // A tier that asks only observes: the chair lands by hand.
    let asked = match record.authorised_by {
        Some(_) => None,
        None => match ask_chair(index, node, spec, plan, &record, checkpoint, owner, journal)? {
            Some(ask_id) => Some(ask_id),
            None => return Ok(()),
        },
    };
    let observation = owner
        .land(LandRequest {
            graph_id: &plan.graph_id,
            node_id: &node.id,
            branch: &record.branch,
            head: &record.head,
            tier: record.tier,
            authority: record.authorised_by.as_ref(),
        })
        .map_err(GraphError::Run)?;
    record.wake_at = None;
    match observation {
        LandObservation::Merged { pull_request, note } => {
            if let Some(ask_id) = asked {
                record.authorised_by = Some(LandAuthority::Chair { ask_id });
            }
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

/// Raise or poll the land ask, returning its ID while it stays open. An
/// answer never approves a land, because lanes and peers can write inbox
/// answers: any answer, `hold` or otherwise, fails the node closed.
#[allow(clippy::too_many_arguments)]
fn ask_chair<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    node: &PlannedNode,
    spec: &LandSpec,
    plan: &PlannedGraph,
    record: &LandRecord,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<Option<String>, GraphError<R::Error, J::Error>> {
    let mine = |ask: &NodeAsk| ask.node.as_deref() == Some(node.id.as_str());
    let position = match checkpoint.asks.iter().rposition(mine) {
        Some(position) => position,
        None => {
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
                return Ok(None);
            };
            asks::identity::<R, J::Error>(checkpoint, owner)?;
            checkpoint
                .asks
                .push(land_ask(&plan.graph_id, node, record, &session));
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            checkpoint.asks.len() - 1
        }
    };
    let ask_id = checkpoint.asks[position].ask_id.clone();
    if checkpoint.asks[position].resolved {
        return Ok(None);
    }
    let before = checkpoint.asks[position].inbox_error.clone();
    let reply = asks::poll(checkpoint, position, owner);
    let Ok(Some(reply)) = reply else {
        if checkpoint.asks[position].inbox_error != before {
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        return Ok(Some(ask_id));
    };
    let answer = asks::answered(reply);
    let actor = answer.actor.clone().unwrap_or_default();
    let reason = if answer.option.as_deref() == Some(HOLD) {
        format!("node {}: land held by {actor} in {ask_id}", node.id)
    } else {
        format!(
            "node {}: {actor} answered {ask_id}, but an answer cannot approve a land; land {} at {} by hand with provenant land",
            node.id, record.branch, record.head
        )
    };
    let progress = &mut checkpoint.nodes[index];
    progress.state = NodeState::Failed;
    progress.advance_error = Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
    // A failed land keeps no retry deadline from an earlier pending observation.
    if let Some(land) = progress.land.as_mut() {
        land.wake_at = None;
    }
    let ask = &mut checkpoint.asks[position];
    ask.answer = Some(answer);
    ask.resolved = true;
    asks::refresh_wake(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)?;
    Ok(None)
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
        Some(LandAuthority::Chair { ask_id }) => {
            progress
                .land
                .as_ref()
                .is_some_and(|record| record.policy == LandPolicy::Ask)
                && checkpoint.asks.iter().any(|ask| {
                    &ask.ask_id == ask_id
                        && ask.kind == AskKind::Land
                        && ask.node.as_deref() == Some(progress.id.as_str())
                        && ask.answer.is_none()
                })
        }
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
