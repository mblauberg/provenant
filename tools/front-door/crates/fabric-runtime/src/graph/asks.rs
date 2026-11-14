//! Durable node asks (primitive P4) and their deadlines (P5).
//!
//! A loop at exhaustion, a decide node and a non-loop pause each raise one
//! inbox decision, keyed by an ask ID that is unique in the checkpoint, so
//! several can be open at once. The inbox is the one store for the question and
//! its answer. The checkpoint keeps only a link from the node to the request:
//! the ask ID, the session, the request ID the inbox returned and the deadline
//! the request carries. Filing is idempotent by the graph instance and ask ID,
//! and the link is saved straight after, so a crash between the two replays the
//! same request instead of filing a second. `wake_at` on the checkpoint is the
//! earliest open deadline, for a host tick; running CI gates and nodes waiting
//! on a bounded launch retry also owe the tick a wake. Until one exists, every
//! advance checks the deadline itself.
use super::*;
use provenant_core::{DurationMicros, WallTime};

/// Canonical bytes of stored reply text. A longer reply is recorded as rejected.
pub(super) const ANSWER_BYTES: usize = 1_024;
/// Canonical bytes of a stored option ID; inbox option IDs are shorter.
pub(super) const OPTION_BYTES: usize = 128;
/// Canonical bytes of an inbox request ID a link names.
pub(super) const REQUEST_ID_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    Loop,
    Decide,
    Pause,
    Land,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskOption {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnswerSource {
    Answered,
    Default,
}

/// The decision an ask settled on, while the graph applies it. The inbox keeps
/// the answer; the checkpoint does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Answer {
    pub source: AnswerSource,
    pub option: Option<String>,
    pub text: Option<String>,
    /// Who answered. A default has no actor.
    pub actor: Option<String>,
    /// Why the answer could not be applied.
    pub rejected: Option<String>,
}

/// One question as the inbox will show it. Rendered when the ask is raised and
/// filed once; the checkpoint keeps no copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskRequest {
    pub ask_id: String,
    pub kind: AskKind,
    pub node: Option<String>,
    /// The exhausted round count a loop ask decides on.
    pub rounds: Option<usize>,
    pub session: String,
    pub title: String,
    pub body: String,
    pub options: Vec<AskOption>,
    pub default: Option<String>,
    pub wake_at: Option<WallTime>,
    pub timeout: Option<DecideTimeout>,
}

/// The link from a node to its open inbox request, which is all the graph
/// checkpoint keeps of an ask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskLink {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub ask_id: String,
    pub kind: AskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The exhausted round count a loop ask decides on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds: Option<usize>,
    pub session: String,
    /// The deadline the request carries, when the question has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<WallTime>,
    /// Effective decide deadline and where its seconds came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<DecideTimeout>,
    /// The inbox request ID the host returned when it filed the question.
    pub request: String,
}

/// An ask to file. `actor` is the checkpoint's ask actor, recorded before the
/// first ask, so a resume under another client files under the same actor.
#[derive(Clone, Copy, Debug)]
pub struct AskFiling<'a> {
    pub graph_id: &'a str,
    pub instance_id: &'a str,
    pub actor: &'a str,
    pub ask: &'a AskRequest,
}

/// What the host returns for a filed ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskFiled {
    pub request: String,
    /// The deadline the request carries; a replay keeps the first one.
    pub deadline: Option<WallTime>,
    /// The request was closed with no answer before this filing found it.
    pub closed: bool,
}

/// A link as the host reads and closes its request.
#[derive(Clone, Copy, Debug)]
pub struct AskScope<'a> {
    pub actor: &'a str,
    pub ask: &'a AskLink,
}

/// An answer read from the inbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskReply {
    pub option: Option<String>,
    pub text: Option<String>,
    pub actor: String,
    pub at: Option<WallTime>,
}

/// A human decision point. The chosen option ID becomes the node's verdict,
/// so downstream verdict gates route on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnansweredDisposition {
    BlockDependants,
    SkipDependants,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecideTimeoutSource {
    Graph,
    Configured,
    DomainDefault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideTimeout {
    pub seconds: u64,
    pub source: DecideTimeoutSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecideSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub question: String,
    pub options: Vec<AskOption>,
    pub on_unanswered: UnansweredDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// Inbox session; falls back to the graph's `decision_session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

pub(super) fn option(id: &str, label: &str, description: &str) -> AskOption {
    AskOption {
        extra: Default::default(),
        id: id.into(),
        label: label.into(),
        description: description.into(),
    }
}

pub(super) fn ask(
    ask_id: String,
    kind: AskKind,
    session: &str,
    title: String,
    body: String,
) -> AskRequest {
    AskRequest {
        ask_id,
        kind,
        node: None,
        rounds: None,
        session: session.into(),
        title,
        body,
        options: Vec::new(),
        default: None,
        wake_at: None,
        timeout: None,
    }
}

pub(super) const LOOP_ANSWER_HELP: &str = "loop answer must choose stop, extend (one round) or accept; to extend by K positive rounds, reply with {\"decision\":\"extend\",\"rounds\":K}";

pub fn loop_ask(graph_id: &str, node: &str, rounds: usize, session: &str) -> AskRequest {
    let mut ask = ask(
        format!("{node}.rounds-{rounds}"),
        AskKind::Loop,
        session,
        format!("Graph loop {node} exhausted"),
        format!(
            "Graph {graph_id}, loop {node} exhausted after {rounds} rounds. Choose stop to stop the loop, extend to add one round, or accept to use the current outputs as is. To add K rounds, reply with JSON {{\"decision\":\"extend\",\"rounds\":K}} where K is positive and within max_extension_rounds (default 5) and the host limits. This decision does not replace review or grant new authority."
        ),
    );
    ask.node = Some(node.into());
    ask.rounds = Some(rounds);
    ask.options = vec![
        option(
            "stop",
            "Stop",
            "Stop the loop without accepting its outputs.",
        ),
        option(
            "extend",
            "Extend by 1 round",
            "Continue on the same branch and worktree for one more round; reply with JSON to add K rounds.",
        ),
        option(
            "accept",
            "Accept as is",
            "Accept the current outputs and continue the graph.",
        ),
    ];
    ask
}

const PREVIOUS_ANSWER: &str = "\n\nThe previous answer was not applied: ";

fn decide_default(spec: &DecideSpec) -> Option<String> {
    spec.default.clone()
}

fn unanswered_disposition_text(disposition: UnansweredDisposition) -> &'static str {
    match disposition {
        UnansweredDisposition::BlockDependants => "block_dependants",
        UnansweredDisposition::SkipDependants => "skip_dependants",
    }
}

fn timeout_source_text(source: DecideTimeoutSource) -> &'static str {
    match source {
        DecideTimeoutSource::Graph => "the decide declaration",
        DecideTimeoutSource::Configured => "domains.fabric.graph.decide_timeout_seconds",
        DecideTimeoutSource::DomainDefault => "the Fabric graph domain default",
    }
}

/// The rendered decide request body. A rejected previous answer is noted at
/// the end, shortened to keep the body within the inbox limit.
fn decide_body(
    graph_id: &str,
    node_id: &str,
    spec: &DecideSpec,
    timeout: DecideTimeout,
    previous: Option<&str>,
) -> String {
    let fallback = match decide_default(spec) {
        Some(default) => format!(
            "\n\nWith no answer within {} seconds (from {}), the graph takes {default}.",
            timeout.seconds,
            timeout_source_text(timeout.source),
        ),
        None => format!(
            "\n\nWith no answer within {} seconds (from {}), the decision is unanswered and follows {:?}.",
            timeout.seconds,
            timeout_source_text(timeout.source),
            unanswered_disposition_text(spec.on_unanswered),
        ),
    };
    let mut body = format!(
        "Graph {graph_id}, node {node_id}.\n\n{}{fallback}\n\nThe chosen option becomes this node's verdict. This decision does not replace review or grant new authority.",
        spec.question
    );
    let room = provenant_records::inbox::REQUEST_BODY_BYTES
        .saturating_sub(body.len() + PREVIOUS_ANSWER.len());
    if let Some(reason) = previous.filter(|_| room > 0) {
        body.push_str(PREVIOUS_ANSWER);
        body.push_str(prefix(reason, room));
    }
    body
}

/// The part of an ask ID that names the node's retry generation, empty for a
/// node that was never retried. A retry resets the node, so its first ask would
/// otherwise repeat an ID the inbox already holds, answered, and the old answer
/// would be consumed again.
pub(super) fn retry_suffix(progress: &NodeProgress) -> String {
    match retry_generation(progress) {
        0 => String::new(),
        generation => format!(".retry-{generation}"),
    }
}

pub(super) fn retry_generation(progress: &NodeProgress) -> u64 {
    progress
        .extra
        .get("retry_generation")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// The ID sequence of a decide ask, so a replacement never reuses an ID.
fn decide_sequence(ask_id: &str, node_id: &str) -> Option<u64> {
    let identity = ask_id.strip_prefix(node_id)?.strip_prefix(".ask-")?;
    let sequence = if let Some((sequence, generation)) = identity.split_once(".retry-") {
        generation
            .parse::<u64>()
            .ok()
            .filter(|generation| *generation > 0)?;
        sequence
    } else {
        identity
    };
    sequence.parse().ok().filter(|sequence| *sequence > 0)
}

pub(super) fn decide_ask(
    graph_id: &str,
    node: &PlannedNode,
    spec: &DecideSpec,
    session: &str,
    sequence: u64,
    wake_at: Option<WallTime>,
    timeout: DecideTimeout,
    previous: Option<&str>,
) -> AskRequest {
    let mut ask = ask(
        format!("{}.ask-{sequence}", node.id),
        AskKind::Decide,
        session,
        format!(
            "Graph decision: {}",
            prefix(node.title.as_deref().unwrap_or(&node.id), 200)
        ),
        decide_body(graph_id, &node.id, spec, timeout, previous),
    );
    ask.node = Some(node.id.clone());
    ask.options = spec.options.clone();
    ask.default = decide_default(spec);
    ask.wake_at = wake_at;
    ask.timeout = Some(timeout);
    ask
}

pub(super) fn pause_ask(
    graph_id: &str,
    sequence: u64,
    session: &str,
    reason: &str,
    previous: Option<&str>,
) -> AskRequest {
    let previous = previous
        .map(|reason| format!("\n\nThe previous answer was not applied: {reason}"))
        .unwrap_or_default();
    let mut ask = ask(
        format!("pause-{sequence}"),
        AskKind::Pause,
        session,
        format!("Graph {graph_id} paused"),
        format!(
            "Graph {graph_id} paused: {reason}{previous}\n\nChoose resume to continue or cancel to stop admitting work. To raise a budget first, reply with JSON {{\"decision\":\"extend\",\"extensions\":[\"budget.per_graph.tokens=N\"]}}. This decision does not replace review or grant new authority."
        ),
    );
    ask.options = vec![
        option("resume", "Resume", "Continue the graph as it stands."),
        option("cancel", "Cancel", "Stop admitting new work."),
    ];
    ask
}

pub(super) fn decide_session<'a>(
    spec: &'a DecideSpec,
    plan_session: Option<&'a str>,
) -> Option<&'a str> {
    spec.session.as_deref().or(plan_session)
}

/// Static checks shared by every place a decide node can be declared.
pub(super) fn validate_decide(
    graph_id: &str,
    node: &GraphNode,
    decision_session: Option<&str>,
) -> Result<(), &'static str> {
    let Some(spec) = &node.decide else {
        return Ok(());
    };
    if node.route.is_some()
        || node.routes.is_some()
        || node.access != AccessMode::ReadOnly
        || node.delivery.is_some()
        || node.worktree.is_some()
        || !node.prompt.is_empty()
        || node.required_verdict.is_some()
        || !node.inputs.is_empty()
        || node.on_fail.is_some()
    {
        return Err("decide takes a question and options and no agent configuration");
    }
    let mut ids = std::collections::BTreeSet::new();
    if spec.question.trim().is_empty()
        || spec.options.len() < 2
        || spec.options.len() > provenant_records::inbox::MAX_OPTIONS
        || spec.options.iter().any(|option| {
            provenant_core::OperationId::parse(&option.id).is_err()
                || !ids.insert(option.id.as_str())
                || option.label.trim().is_empty()
                || option.label.len() > 256
                || option.description.len() > 1024
        })
    {
        return Err("decide needs a question and 2 to 16 options with inbox-safe IDs and labels");
    }
    if repair_scope_gate(node.extra.get("repair_scope_gate"))
        && spec.default.as_deref() == Some("approve")
    {
        return Err("scope approval requires an owner answer, never a timeout default");
    }
    if spec
        .default
        .as_ref()
        .is_some_and(|default| !ids.contains(default.as_str()))
    {
        return Err("decide default must name an option");
    }
    // A timeout must be a representable duration that fits the wall-time
    // range at all; the deadline itself is checked when the ask is raised.
    if spec.timeout_seconds.is_some_and(|seconds| {
        seconds == 0
            || DurationMicros::from_seconds(seconds)
                .ok()
                .and_then(|duration| WallTime::from_micros(0).ok()?.checked_add(duration).ok())
                .is_none()
    }) {
        return Err("decide timeout_seconds must be positive and end within the wall-time range");
    }
    let timeout = spec.timeout_seconds.map_or(
        DecideTimeout {
            seconds: crate::config::MAX_DECIDE_TIMEOUT_SECONDS,
            source: DecideTimeoutSource::Configured,
        },
        |seconds| DecideTimeout {
            seconds,
            source: DecideTimeoutSource::Graph,
        },
    );
    if decide_body(graph_id, &node.id, spec, timeout, None).len()
        > provenant_records::inbox::REQUEST_BODY_BYTES
    {
        return Err("decide question makes the request body longer than the inbox's 8192 bytes");
    }
    if decide_session(spec, decision_session)
        .or_else(|| repair_scope_gate(node.extra.get("repair_scope_gate")).then_some(graph_id))
        .is_none_or(|s| crate::inbox::session_id(s).is_err())
    {
        return Err("decide needs a valid session or graph decision_session");
    }
    Ok(())
}

/// Assign the ask identity once, before the first ask is filed.
pub(super) fn identity<R: AttemptOwner, J: Error + 'static>(
    checkpoint: &mut GraphCheckpoint,
    owner: &R,
) -> Result<(), GraphError<R::Error, J>> {
    if checkpoint.instance_id.is_none() {
        checkpoint.instance_id = Some(provenant_core::hex(
            &provenant_os::fs::entropy_128().map_err(GraphError::Instance)?,
        ));
    }
    if checkpoint.ask_actor.is_none() {
        checkpoint.ask_actor = Some(owner.actor());
    }
    Ok(())
}

pub(super) fn refresh_wake(checkpoint: &mut GraphCheckpoint) {
    checkpoint.wake_at = next_wake(checkpoint);
}

fn next_wake(checkpoint: &GraphCheckpoint) -> Option<WallTime> {
    checkpoint
        .asks
        .iter()
        .filter_map(|ask| ask.wake_at)
        .chain(
            checkpoint
                .nodes
                .iter()
                .filter(|node| node.state == NodeState::Running)
                .filter_map(|node| {
                    let ci = node.ci.as_ref().and_then(|ci| ci.wake_at);
                    ci.or_else(|| node.land.as_ref().and_then(|land| land.wake_at))
                }),
        )
        // A node waiting on its own bounded launch retry also owes the tick a
        // wake.
        .chain(
            checkpoint
                .nodes
                .iter()
                .filter(|node| node.state == NodeState::Running)
                .filter_map(|node| node.launch_retry.as_ref().and_then(|retry| retry.wake_at)),
        )
        .min()
}

/// File a question and return the link to keep. The identity is saved before
/// the request exists, so a replay files under the same instance and ask ID.
/// The host files idempotently by that key: a crash before the caller saves the
/// link finds the same request again, never a second. The flag says the request
/// was already closed without an answer.
pub(super) fn file<R: AttemptOwner, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
    request: &AskRequest,
) -> Result<(AskLink, bool), GraphError<R::Error, J::Error>> {
    if checkpoint.instance_id.is_none() || checkpoint.ask_actor.is_none() {
        identity::<R, J::Error>(checkpoint, owner)?;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let (Some(instance_id), Some(actor)) = (&checkpoint.instance_id, &checkpoint.ask_actor) else {
        return Err(GraphError::BadCheckpoint {
            node: request.node.clone().unwrap_or_default(),
            invariant: "an ask needs the saved graph identity",
        });
    };
    let filed = owner
        .file_ask(AskFiling {
            graph_id: &checkpoint.graph_id,
            instance_id,
            actor,
            ask: request,
        })
        .map_err(GraphError::Run)?;
    let link = AskLink {
        extra: Default::default(),
        ask_id: request.ask_id.clone(),
        kind: request.kind,
        node: request.node.clone(),
        rounds: request.rounds,
        session: request.session.clone(),
        wake_at: filed.deadline,
        timeout: request.timeout,
        request: bounded(&filed.request, REQUEST_ID_BYTES),
    };
    Ok((link, filed.closed))
}

/// Read an answer. An inbox failure is returned and the ask stays open, so the
/// next advance reads it again.
pub(super) fn poll<R: AttemptOwner>(
    checkpoint: &GraphCheckpoint,
    position: usize,
    owner: &mut R,
) -> Result<Option<AskReply>, String> {
    let Some(actor) = &checkpoint.ask_actor else {
        return Err("graph has no ask identity".into());
    };
    owner
        .node_ask(AskScope {
            actor,
            ask: &checkpoint.asks[position],
        })
        .map_err(|error| error.to_string())
}

pub(super) fn answered(reply: AskReply) -> Answer {
    let rejected = reply
        .text
        .as_ref()
        .filter(|text| encoded_len(text) > ANSWER_BYTES + 2)
        .map(|_| format!("reply longer than {ANSWER_BYTES} bytes"));
    Answer {
        source: AnswerSource::Answered,
        option: reply.option.map(|option| bounded(&option, OPTION_BYTES)),
        text: reply.text.filter(|_| rejected.is_none()),
        actor: Some(bounded(&reply.actor, HOST_ID_BYTES)),
        rejected,
    }
}

fn due(ask: &AskLink, now: Option<WallTime>) -> bool {
    ask.wake_at
        .zip(now)
        .is_some_and(|(wake_at, now)| now >= wake_at)
}

fn effective_decide_timeout<R: AttemptOwner, J: Error + 'static>(
    spec: &DecideSpec,
    owner: &R,
) -> Result<DecideTimeout, GraphError<R::Error, J>> {
    let timeout = match spec.timeout_seconds {
        Some(seconds) => DecideTimeout {
            seconds,
            source: DecideTimeoutSource::Graph,
        },
        None => owner.decide_timeout().map_err(GraphError::Run)?,
    };
    let representable = DurationMicros::from_seconds(timeout.seconds)
        .ok()
        .and_then(|duration| WallTime::from_micros(0).ok()?.checked_add(duration).ok())
        .is_some();
    if timeout.seconds == 0 || !representable {
        return Err(GraphError::Refused(
            "effective decide timeout is not a positive representable duration".into(),
        ));
    }
    Ok(timeout)
}

/// The deadline of a decide ask, fixed before the ask exists. Without a clock
/// the advance is refused and retried, so the timeout is never lost.
fn decide_deadline<R: AttemptOwner, J: Error + 'static>(
    node: &PlannedNode,
    timeout: DecideTimeout,
    owner: &R,
) -> Result<Option<WallTime>, GraphError<R::Error, J>> {
    let seconds = timeout.seconds;
    let now = owner.wall_now().ok_or_else(|| {
        GraphError::Refused(format!(
            "node {}: no wall clock to set its decide deadline; retry the advance",
            node.id
        ))
    })?;
    DurationMicros::from_seconds(seconds)
        .ok()
        .and_then(|duration| now.checked_add(duration).ok())
        .map(Some)
        .ok_or_else(|| {
            GraphError::Refused(format!(
                "node {}: decide deadline passes the last representable wall time; lower timeout_seconds",
                node.id
            ))
        })
}

/// The decide request for one sequence number. A replacement for a rejected
/// answer names the reason in its body.
#[allow(clippy::too_many_arguments)]
fn decide_request(
    index: usize,
    node: &PlannedNode,
    spec: &DecideSpec,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    session: &str,
    sequence: u64,
    wake_at: Option<WallTime>,
    timeout: DecideTimeout,
    previous: Option<&str>,
) -> AskRequest {
    let mut ask = decide_ask(
        &plan.graph_id,
        node,
        spec,
        session,
        sequence,
        wake_at,
        timeout,
        previous,
    );
    ask.ask_id.push_str(&retry_suffix(&checkpoint.nodes[index]));
    if repair_scope_gate(node.extra.get("repair_scope_gate"))
        && let Some(report) = node.depends_on.iter().find_map(|id| {
            checkpoint
                .nodes
                .iter()
                .find(|progress| &progress.id == id)
                .and_then(|progress| progress.outputs.as_ref())
                .and_then(|outputs| outputs.result.as_ref())
        })
    {
        ask.body = bounded(
            &format!("{}\n\nPrincipal report:\n{}", ask.body, report.excerpt),
            provenant_records::inbox::REQUEST_BODY_BYTES,
        );
    }
    ask
}

/// The most closed requests one decide node may leave behind before a new raise
/// is refused.
const MAX_DECIDE_SEQUENCE: u64 = 1_000;

/// File a decide request under `sequence`, or the next free one. A request that
/// an earlier cancel closed keeps its ask ID, so a raise after a resume files
/// under a new one rather than waiting on a request nobody can answer.
#[allow(clippy::too_many_arguments)]
fn file_decide<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    node: &PlannedNode,
    spec: &DecideSpec,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
    session: &str,
    first: u64,
    timeout: DecideTimeout,
    previous: Option<&str>,
) -> Result<AskLink, GraphError<R::Error, J::Error>> {
    let wake_at = decide_deadline::<R, J::Error>(node, timeout, owner)?;
    for sequence in first..first.saturating_add(MAX_DECIDE_SEQUENCE) {
        let request = decide_request(
            index, node, spec, plan, checkpoint, session, sequence, wake_at, timeout, previous,
        );
        let (link, closed) = file(checkpoint, owner, journal, &request)?;
        if !closed {
            return Ok(link);
        }
    }
    Err(GraphError::Refused(format!(
        "node {}: {MAX_DECIDE_SEQUENCE} decide requests are already closed; retry the node",
        node.id
    )))
}

/// Raise, poll and settle a decide node. The link and the node's running state
/// are saved together, and so are the node's outputs and the link's removal, so
/// no crash leaves a decision half applied. A rejected answer is replaced by a
/// new request that says why.
pub(super) fn decide<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    node: &PlannedNode,
    spec: &DecideSpec,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    if repair_scope_gate(node.extra.get("repair_scope_gate"))
        && repair_scope(index, node, spec, plan, checkpoint, owner, journal)?
    {
        return Ok(());
    }
    let mine = |ask: &AskLink| ask.node.as_deref() == Some(node.id.as_str());
    let position = match checkpoint.asks.iter().rposition(mine) {
        Some(position) => position,
        None => {
            if checkpoint.pause.is_some() {
                return Ok(());
            }
            let session = decide_session(spec, plan.decision_session.as_deref())
                .or_else(|| {
                    repair_scope_gate(node.extra.get("repair_scope_gate"))
                        .then_some(plan.graph_id.as_str())
                })
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "decision must name an inbox session",
                })?
                .to_owned();
            let timeout = effective_decide_timeout::<R, J::Error>(spec, owner)?;
            let link = file_decide(
                index, node, spec, plan, checkpoint, owner, journal, &session, 1, timeout, None,
            )?;
            checkpoint.asks.push(link);
            checkpoint.nodes[index].state = NodeState::Running;
            refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            checkpoint.asks.len() - 1
        }
    };
    let timeout = checkpoint.asks[position]
        .timeout
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "a decide ask must retain its effective timeout",
        })?;
    let reply = poll(checkpoint, position, owner);
    let now = owner.wall_now();
    let link = &checkpoint.asks[position];
    let answer = match reply {
        Ok(Some(reply)) => Some(answered(reply)),
        Ok(None) if due(link, now) => Some(Answer {
            source: AnswerSource::Default,
            option: decide_default(spec),
            text: None,
            actor: None,
            rejected: None,
        }),
        _ => None,
    };
    let Some(mut answer) = answer else {
        return Ok(());
    };
    // Silence cannot grant a change of scope or a one-way-door authority.
    // An unanswered decide follows its declared disposition for every node.
    if answer.source == AnswerSource::Default && answer.option.is_none() {
        let state = match spec.on_unanswered {
            UnansweredDisposition::BlockDependants => NodeState::Failed,
            UnansweredDisposition::SkipDependants => NodeState::Skipped,
        };
        let progress = &mut checkpoint.nodes[index];
        progress.state = state;
        progress.outputs = Some(decision_outputs(
            &link.ask_id,
            DecideOutcome::Unanswered,
        ));
        checkpoint.asks.remove(position);
        refresh_wake(checkpoint);
        return journal.save(checkpoint).map_err(GraphError::Journal);
    }
    let chosen = answer
        .option
        .clone()
        .or_else(|| answer.text.as_deref().map(|text| text.trim().to_owned()))
        .filter(|id| spec.options.iter().any(|option| &option.id == id));
    let ask_id = link.ask_id.clone();
    if answer.rejected.is_none() && chosen.is_none() {
        let ids: Vec<_> = spec
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        answer.rejected = Some(bounded(
            &format!("answer must choose one of {}", ids.join(", ")),
            MAX_ADVANCE_ERROR_BYTES,
        ));
    }
    if let (None, Some(chosen)) = (&answer.rejected, chosen) {
        let progress = &mut checkpoint.nodes[index];
        progress.state = NodeState::Succeeded;
        let outcome = match answer.source {
            AnswerSource::Default => DecideOutcome::AnsweredByDefault {
                option: chosen.clone(),
            },
            AnswerSource::Answered => DecideOutcome::Answered {
                option: chosen.clone(),
                actor: answer.actor.clone().ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "an owner decide answer must retain its actor",
                })?,
            },
        };
        progress.outputs = Some(decision_outputs(&ask_id, outcome));
        checkpoint.asks.remove(position);
        refresh_wake(checkpoint);
        return journal.save(checkpoint).map_err(GraphError::Journal);
    }
    // A rejected answer keeps the node open: the request is replaced under the
    // next ask ID, which says why.
    if checkpoint.pause.is_some() {
        return Ok(());
    }
    let previous = &checkpoint.asks[position];
    let session = previous.session.clone();
    let sequence = decide_sequence(&previous.ask_id, &node.id)
        .unwrap_or(0)
        .saturating_add(1);
    let link = file_decide(
        index,
        node,
        spec,
        plan,
        checkpoint,
        owner,
        journal,
        &session,
        sequence,
        timeout,
        answer.rejected.as_deref(),
    )?;
    checkpoint.asks[position] = link;
    refresh_wake(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)
}

const APPROVED_SCOPE_RULE: &str = "approved_repair_scope";

fn decision_outputs(ask_id: &str, outcome: DecideOutcome) -> NodeOutputs {
    let excerpt = bounded(
        &match &outcome {
            DecideOutcome::Answered { option, actor } => {
                format!("decision {ask_id}: {option}, answered by {actor}")
            }
            DecideOutcome::AnsweredByDefault { option } => {
                format!("decision {ask_id}: {option}, the declared default at the deadline")
            }
            DecideOutcome::AnsweredByPolicy { option, rule } => {
                format!("decision {ask_id}: {option}, applied policy {rule}")
            }
            DecideOutcome::Unanswered => format!("decision {ask_id}: unanswered at the deadline"),
        },
        OUTPUT_FIELD_BYTES,
    );
    let verdict = outcome.option().map(str::to_owned);
    NodeOutputs {
        extra: Default::default(),
        verdict_error: None,
        schema_version: 2,
        decide_outcome: Some(outcome.clone()),
        result: Some(ResultOutput {
            extra: Default::default(),
            digest: CanonicalV1::file_digest(excerpt.as_bytes()).to_string(),
            excerpt,
            truncated: false,
            path: None,
        }),
        verdict,
        findings: Vec::new(),
        findings_available: None,
        branch: None,
        commit: None,
        warnings: match outcome {
            DecideOutcome::Answered { .. } | DecideOutcome::AnsweredByPolicy { .. } => Vec::new(),
            DecideOutcome::AnsweredByDefault { option } => vec![format!(
                "decide timeout: applied declared default {option}; no answer arrived before the deadline"
            )],
            DecideOutcome::Unanswered => vec![
                "decide timeout: no answer or declared default; recorded unanswered".into(),
            ],
        },
        plan: None,
        remaining: None,
    }
}

enum PauseAction {
    Resume(Vec<String>),
    Cancel,
}

#[derive(Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
enum PauseReply {
    Resume {},
    Cancel {},
    Extend { extensions: Vec<String> },
}

fn pause_action(
    answer: &Answer,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
    budget: &crate::config::GraphBudget,
) -> Result<PauseAction, (&'static str, String)> {
    const HELP: &str = "pause answer must choose resume or cancel, or reply with {\"decision\":\"extend\",\"extensions\":[\"budget.KEY=VALUE\"]}";
    let reply = match (answer.option.as_deref(), answer.text.as_deref()) {
        (Some("resume"), None) => PauseReply::Resume {},
        (Some("cancel"), None) => PauseReply::Cancel {},
        (None, Some(text)) => {
            serde_json::from_str(text).map_err(|_| ("invalid_pause_answer", HELP.to_owned()))?
        }
        _ => return Err(("invalid_pause_answer", HELP.into())),
    };
    let extensions = match reply {
        PauseReply::Cancel {} => return Ok(PauseAction::Cancel),
        PauseReply::Resume {} => Vec::new(),
        PauseReply::Extend { extensions } => extensions,
    };
    let mut trial = budget.clone();
    for extension in &extensions {
        if !extension.starts_with("budget.") {
            return Err((
                "invalid_pause_answer",
                format!(
                    "{}: a pause answer extends budgets only; use graph resume for other extensions",
                    bounded(extension, 128)
                ),
            ));
        }
        apply_extension(&mut trial, extension, false)
            .map_err(|error| (error.code(), error.to_string()))?;
    }
    let mut all = checkpoint.extensions.clone();
    all.extend(extensions.iter().cloned());
    if let Some(reason) = graph_envelope_reason(document, &checkpoint.expansions, &all, plan) {
        return Err(("checkpoint_too_large", reason));
    }
    Ok(PauseAction::Resume(extensions))
}

/// Raise or poll the ask for a non-loop pause. Returns true when an answer
/// was applied and the caller should advance again. An answer that cannot be
/// applied is replaced by a new request that says why.
pub(super) fn pause<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
    budget: &crate::config::GraphBudget,
) -> Result<bool, GraphError<R::Error, J::Error>> {
    let (Some(session), Some(reason)) = (
        &plan.decision_session,
        checkpoint.pause_reason().map(str::to_owned),
    ) else {
        return Ok(false);
    };
    if checkpoint.cancelled || loop_pause(checkpoint) {
        return Ok(false);
    }
    let pauses = |ask: &AskLink| ask.kind == AskKind::Pause;
    let position = match checkpoint.asks.iter().rposition(pauses) {
        Some(position) => position,
        None => {
            let request = pause_ask(
                &checkpoint.graph_id,
                checkpoint.pause_asks + 1,
                session,
                &reason,
                None,
            );
            let (link, _) = file(checkpoint, owner, journal, &request)?;
            checkpoint.pause_asks += 1;
            checkpoint.asks.push(link);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            checkpoint.asks.len() - 1
        }
    };
    let reply = match poll(checkpoint, position, owner) {
        Ok(Some(reply)) => reply,
        _ => return Ok(false),
    };
    let answer = answered(reply);
    let action = match &answer.rejected {
        Some(reason) => Err(("answer_too_large", reason.clone())),
        None => pause_action(&answer, document, checkpoint, plan, budget),
    };
    match action {
        Ok(PauseAction::Cancel) => {
            checkpoint.cancelled = true;
            checkpoint.asks.remove(position);
        }
        Ok(PauseAction::Resume(extensions)) => {
            for extension in extensions {
                if !checkpoint.extensions.contains(&extension) {
                    checkpoint.extensions.push(extension);
                }
            }
            checkpoint.deactivate_expansion_pauses();
            checkpoint.pause = None;
            checkpoint.asks.remove(position);
        }
        Err((_, note)) => {
            let note = bounded(&note, MAX_ADVANCE_ERROR_BYTES);
            let request = pause_ask(
                &checkpoint.graph_id,
                checkpoint.pause_asks + 1,
                session,
                &reason,
                Some(&note),
            );
            let (link, _) = file(checkpoint, owner, journal, &request)?;
            checkpoint.pause_asks += 1;
            checkpoint.asks[position] = link;
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            return Ok(false);
        }
    }
    journal.save(checkpoint).map_err(GraphError::Journal)?;
    Ok(true)
}

pub(super) fn loop_pause(checkpoint: &GraphCheckpoint) -> bool {
    checkpoint
        .pause_cause()
        .and_then(NodeDecisionCause::from_pause)
        .is_some()
}

fn loop_rounds(checkpoint: &GraphCheckpoint, loop_id: &str) -> usize {
    checkpoint
        .nodes
        .iter()
        .filter_map(|n| {
            n.round
                .as_ref()
                .filter(|r| r.loop_id == loop_id)
                .map(|r| r.round)
        })
        .max()
        .unwrap_or(0)
}

/// Whether cancelling the graph withdraws this node's asks. A node detached
/// from graph cancellation keeps its own.
pub(super) fn withdrawn(checkpoint: &GraphCheckpoint, node_id: &str) -> bool {
    checkpoint.cancelled
        && !checkpoint.nodes.iter().any(|node| {
            node.id == node_id
                && node.extra.get(super::relay::DETACHED_FROM_GRAPH_CANCEL)
                    == Some(&serde_json::Value::Bool(true))
        })
}

/// Whether an ask's question has gone away: its node settled, the graph was
/// cancelled, its loop moved on or its pause ended.
fn gone(checkpoint: &GraphCheckpoint, ask: &AskLink) -> bool {
    let settled = ask.node.as_ref().is_none_or(|id| {
        checkpoint
            .nodes
            .iter()
            .find(|n| &n.id == id)
            .is_none_or(|n| n.state.settled())
    });
    let withdrawn = match &ask.node {
        Some(id) => withdrawn(checkpoint, id),
        None => checkpoint.cancelled,
    };
    withdrawn
        || match ask.kind {
            AskKind::Loop => {
                settled
                    || ask.rounds.unwrap_or(0)
                        < loop_rounds(checkpoint, ask.node.as_deref().unwrap_or_default())
            }
            AskKind::Decide | AskKind::Land => settled,
            AskKind::Pause => checkpoint.pause.is_none() || loop_pause(checkpoint),
        }
}

/// Close the inbox requests of asks whose question has gone away, then drop
/// their links. The request closes first, so a crash between the two leaves a
/// link whose close is a no-op on the next advance. A close failure keeps the
/// link, and the next advance retries it. An answered request stays answered.
pub fn close_resolved_asks<R: AttemptOwner, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let Some(actor) = checkpoint.ask_actor.clone() else {
        return Ok(());
    };
    let mut closed = Vec::new();
    for ask in checkpoint.asks.iter().filter(|ask| gone(checkpoint, ask)) {
        if owner
            .close_node_ask(AskScope { actor: &actor, ask })
            .is_ok()
        {
            closed.push(ask.ask_id.clone());
        }
    }
    if closed.is_empty() {
        return Ok(());
    }
    checkpoint.asks.retain(|ask| !closed.contains(&ask.ask_id));
    refresh_wake(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)
}

/// Checkpoint-level ask invariants. Node-level state is checked beside each kind.
pub(super) fn validate(checkpoint: &GraphCheckpoint, plan: &PlannedGraph) -> Result<(), ()> {
    if checkpoint.wake_at != next_wake(checkpoint) {
        return Err(());
    }
    if checkpoint.asks.is_empty() {
        return Ok(());
    }
    if checkpoint.instance_id.is_none() || checkpoint.ask_actor.is_none() {
        return Err(());
    }
    let mut ids = std::collections::BTreeSet::new();
    for ask in &checkpoint.asks {
        let node = match &ask.node {
            Some(id) => Some(plan.nodes.iter().find(|n| &n.id == id).ok_or(())?),
            None => None,
        };
        let session = Some(ask.session.as_str());
        let valid = ids.insert(ask.ask_id.as_str())
            && !ask.request.is_empty()
            && match (ask.kind, node) {
                (AskKind::Loop, Some(node)) => {
                    ask.timeout.is_none()
                        && ask.rounds.is_some_and(|rounds| rounds > 0)
                        && node
                            .loop_body
                            .as_ref()
                            .and_then(|spec| spec.decision_session.as_deref())
                            == session
                }
                (AskKind::Decide, Some(node)) => {
                    decide_sequence(&ask.ask_id, &node.id).is_some()
                        && ask.wake_at.is_some()
                        && ask.timeout.is_some_and(|timeout| timeout.seconds > 0)
                        && node.decide.as_ref().is_some_and(|spec| {
                            decide_session(spec, plan.decision_session.as_deref()).or_else(|| {
                                repair_scope_gate(node.extra.get("repair_scope_gate"))
                                    .then_some(plan.graph_id.as_str())
                            }) == session
                        })
                }
                (AskKind::Pause, None) => {
                    ask.timeout.is_none() && plan.decision_session.as_deref() == session
                },
                (AskKind::Land, Some(node)) => {
                    ask.timeout.is_none()
                        && land::parse(&ask.ask_id, &node.id).is_some()
                        && node
                            .land
                            .as_ref()
                            .is_some_and(|spec| land::session(spec, plan) == session)
                }
                _ => false,
            };
        if !valid {
            return Err(());
        }
    }
    Ok(())
}

pub(super) fn validate_progress(
    progress: &NodeProgress,
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
) -> Result<(), ()> {
    let latest = checkpoint
        .asks
        .iter()
        .rfind(|ask| ask.node.as_deref() == Some(node.id.as_str()));
    let valid = progress.attempts.is_empty()
        && progress.gate.is_none()
        && progress.ci.is_none()
        && (progress.advance_error.is_none() || progress.state == NodeState::Failed)
        && match progress.state {
            NodeState::Pending | NodeState::Blocked => progress.outputs.is_none(),
            NodeState::Skipped => {
                progress.outputs.is_none() || unanswered_progress(progress, node)
            }
            // A running decision without a link raises its ask again.
            NodeState::Running => progress.outputs.is_none(),
            NodeState::Succeeded => {
                (repair_scope_gate(node.extra.get("repair_scope_gate"))
                    && approved_repair_scope(node, checkpoint)
                    && progress.extra.get("owner_notified") == Some(&serde_json::Value::Bool(true))
                    && progress.outputs.as_ref().is_some_and(|outputs| {
                        outputs.verdict.as_deref() == Some("approve")
                            && matches!(
                                outputs.decide_outcome,
                                Some(DecideOutcome::AnsweredByPolicy { ref option, ref rule })
                                    if option == "approve" && rule == APPROVED_SCOPE_RULE
                            )
                    })
                    && latest.is_none())
                    // The answer is the inbox's. The checkpoint keeps the
                    // verdict it produced, which must be one of the options.
                    || progress.outputs.as_ref().is_some_and(|outputs| {
                        let declared_option = outputs
                            .decide_outcome
                            .as_ref()
                            .filter(|outcome| matches!(outcome,
                                DecideOutcome::Answered { .. } | DecideOutcome::AnsweredByDefault { .. }))
                            .and_then(DecideOutcome::option);
                        declared_option
                            .zip(outputs.verdict.as_deref())
                            .zip(node.decide.as_ref())
                            .is_some_and(|((option, verdict), spec)| {
                                option == verdict
                                    && spec.options.iter().any(|candidate| candidate.id == option)
                            })

                    })
            }
            NodeState::Failed => {
                progress.outputs.is_none() || unanswered_progress(progress, node)
            }
        };
    if valid { Ok(()) } else { Err(()) }
}

fn unanswered_progress(progress: &NodeProgress, node: &PlannedNode) -> bool {
    node.decide.as_ref().is_some_and(|spec| {
        let expected = match spec.on_unanswered {
            UnansweredDisposition::BlockDependants => NodeState::Failed,
            UnansweredDisposition::SkipDependants => NodeState::Skipped,
        };
        progress.state == expected
            && progress.outputs.as_ref().is_some_and(|outputs| {
                outputs.decide_outcome == Some(DecideOutcome::Unanswered)
                    && outputs.verdict.is_none()
            })
    })
}

fn repair_scope_gate(value: Option<&serde_json::Value>) -> bool {
    value == Some(&serde_json::Value::Bool(true))
}

fn approved_repair_scope(node: &PlannedNode, checkpoint: &GraphCheckpoint) -> bool {
    node.depends_on
        .iter()
        .find_map(|id| {
            checkpoint
                .nodes
                .iter()
                .find(|progress| &progress.id == id)
                .and_then(|progress| progress.outputs.as_ref())
        })
        .and_then(|outputs| outputs.extra.get("repair_design"))
        .is_some_and(|design| {
            design
                .get("within_scope")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
                && design
                    .get("one_way_door")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
        })
}

/// The host owns notification and the scope barrier. Principal prose and a
/// continuation planner cannot silently bypass either action.
fn repair_scope<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    node: &PlannedNode,
    spec: &DecideSpec,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<bool, GraphError<R::Error, J::Error>> {
    let design = node.depends_on.iter().find_map(|id| {
        checkpoint
            .nodes
            .iter()
            .find(|progress| &progress.id == id)
            .and_then(|progress| progress.outputs.as_ref())
    });
    let approved_scope = approved_repair_scope(node, checkpoint);
    if checkpoint.nodes[index].extra.get("owner_notified") != Some(&serde_json::Value::Bool(true)) {
        let report = design
            .and_then(|outputs| outputs.result.as_ref())
            .map(|result| result.excerpt.as_str())
            .unwrap_or("Principal report unavailable");
        let generation = checkpoint.nodes[index]
            .extra
            .get("retry_generation")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let notice_id = if generation == 0 {
            format!("{}.owner-notification", node.id)
        } else {
            format!("{}.owner-notification.retry-{generation}", node.id)
        };
        let mut notice = ask(
            notice_id,
            AskKind::Decide,
            decide_session(spec, plan.decision_session.as_deref()).unwrap_or(&plan.graph_id),
            "Review repair: principal redesign".into(),
            bounded(
                &format!(
                    "Graph {}, principal redesign. {}\n\n{}",
                    plan.graph_id,
                    if approved_scope {
                        "Delivery continues within approved scope."
                    } else {
                        "Delivery waits for a scope or one-way-door decision."
                    },
                    report
                ),
                provenant_records::inbox::REQUEST_BODY_BYTES,
            ),
        );
        notice.node = Some(node.id.clone());
        identity::<R, J::Error>(checkpoint, owner)?;
        // Save the identity before the external effect; replay uses the same
        // inbox operation if acknowledgement cannot be checkpointed.
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        let (Some(instance), Some(actor)) = (
            checkpoint.instance_id.as_deref(),
            checkpoint.ask_actor.as_deref(),
        ) else {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "owner notification requires saved identity",
            });
        };
        let notified = owner
            .notify_owner(AskFiling {
                graph_id: &plan.graph_id,
                instance_id: instance,
                actor,
                ask: &notice,
            })
            .map_err(GraphError::Run)?;
        if !notified {
            return Ok(true);
        }
        checkpoint.nodes[index]
            .extra
            .insert("owner_notified".into(), true.into());
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    if approved_scope {
        checkpoint.nodes[index].state = NodeState::Succeeded;
        checkpoint.nodes[index].outputs = Some(decision_outputs(
            &format!("{}.approved-scope", node.id),
            DecideOutcome::AnsweredByPolicy {
                option: "approve".into(),
                rule: APPROVED_SCOPE_RULE.into(),
            },
        ));
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(approved_scope)
}
