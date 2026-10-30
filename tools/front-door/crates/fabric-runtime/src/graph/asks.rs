//! Durable node asks (primitive P4) and their deadlines (P5).
//!
//! A loop at exhaustion, a decide node and a non-loop pause each raise one
//! inbox decision, keyed by an ask ID that is unique in the checkpoint, so
//! several can be open at once. The checkpoint stores the rendered ask, so a
//! replay sends the identical request, and it records the answer and who gave
//! it, or the default taken at the deadline. `wake_at` on the checkpoint is
//! the earliest open deadline, for a host tick; running CI gates and nodes
//! waiting on a bounded launch retry also owe the tick a wake. Until one
//! exists, every advance checks the deadline itself.
use super::*;
use provenant_core::{DurationMicros, WallTime};

/// Closed pause asks kept as history. Older ones are dropped.
pub(super) const MAX_PAUSE_ASKS: usize = 8;
/// Canonical bytes of stored reply text. A longer reply is recorded as rejected.
pub(super) const ANSWER_BYTES: usize = 1_024;
/// Canonical bytes of a stored option ID; inbox option IDs are shorter.
pub(super) const OPTION_BYTES: usize = 128;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    Answered,
    Default,
}

/// The decision an ask settled on, kept as graph history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskAnswer {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub source: AnswerSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Who answered. A default has no actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<WallTime>,
    /// Why the answer could not be applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected: Option<String>,
    /// Stable reason code when an answer could not be applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAsk {
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
    pub title: String,
    pub body: String,
    pub options: Vec<AskOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<WallTime>,
    pub resolved: bool,
    pub closed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<AskAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_error: Option<String>,
}

/// One ask as the host's inbox sees it. `actor` is the checkpoint's ask actor,
/// recorded before the first ask, so a resume under another client replays it.
#[derive(Clone, Copy, Debug)]
pub struct AskScope<'a> {
    pub graph_id: &'a str,
    pub instance_id: &'a str,
    pub actor: &'a str,
    pub ask: &'a NodeAsk,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecideSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub question: String,
    pub options: Vec<AskOption>,
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
) -> NodeAsk {
    NodeAsk {
        extra: Default::default(),
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
        resolved: false,
        closed: false,
        answer: None,
        inbox_error: None,
    }
}

pub(super) const LOOP_ANSWER_HELP: &str = "loop answer must choose stop, extend (one round) or accept; to extend by K positive rounds, reply with {\"decision\":\"extend\",\"rounds\":K}";

pub fn loop_ask(graph_id: &str, node: &str, rounds: usize, session: &str) -> NodeAsk {
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

/// The rendered decide request body. A rejected previous answer is noted at
/// the end, shortened to keep the body within the inbox limit.
fn decide_body(graph_id: &str, node_id: &str, spec: &DecideSpec, previous: Option<&str>) -> String {
    let fallback = match (&spec.default, spec.timeout_seconds) {
        (Some(default), Some(seconds)) => {
            format!("\n\nWith no answer within {seconds} seconds, the graph takes {default}.")
        }
        _ => String::new(),
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

/// The ID sequence of a decide ask, so a replacement never reuses an ID.
fn decide_sequence(ask: &NodeAsk, node_id: &str) -> Option<u64> {
    ask.ask_id
        .strip_prefix(node_id)?
        .strip_prefix(".ask-")?
        .parse()
        .ok()
        .filter(|sequence| *sequence > 0)
}

pub(super) fn decide_ask(
    graph_id: &str,
    node: &PlannedNode,
    spec: &DecideSpec,
    session: &str,
    sequence: u64,
    wake_at: Option<WallTime>,
    previous: Option<&str>,
) -> NodeAsk {
    let mut ask = ask(
        format!("{}.ask-{sequence}", node.id),
        AskKind::Decide,
        session,
        format!(
            "Graph decision: {}",
            prefix(node.title.as_deref().unwrap_or(&node.id), 200)
        ),
        decide_body(graph_id, &node.id, spec, previous),
    );
    ask.node = Some(node.id.clone());
    ask.options = spec.options.clone();
    ask.default = spec.default.clone();
    ask.wake_at = wake_at;
    ask
}

pub(super) fn pause_ask(
    graph_id: &str,
    sequence: u64,
    session: &str,
    reason: &str,
    previous: Option<&str>,
) -> NodeAsk {
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
    if spec
        .default
        .as_ref()
        .is_some_and(|default| !ids.contains(default.as_str()))
        || spec.timeout_seconds.is_some() && spec.default.is_none()
    {
        return Err("decide default must name an option, and a timeout needs a default");
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
    if decide_body(graph_id, &node.id, spec, None).len()
        > provenant_records::inbox::REQUEST_BODY_BYTES
    {
        return Err("decide question makes the request body longer than the inbox's 8192 bytes");
    }
    if decide_session(spec, decision_session).is_none_or(|s| crate::inbox::session_id(s).is_err()) {
        return Err("decide needs a valid session or graph decision_session");
    }
    Ok(())
}

/// Assign the ask identity once, before the first ask is saved.
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
        .filter(|ask| !ask.resolved)
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

/// Read an answer. An inbox failure is kept on the ask and returned.
pub(super) fn poll<R: AttemptOwner>(
    checkpoint: &mut GraphCheckpoint,
    position: usize,
    owner: &mut R,
) -> Result<Option<AskReply>, String> {
    let result = match (&checkpoint.instance_id, &checkpoint.ask_actor) {
        (Some(instance), Some(actor)) => owner
            .node_ask(AskScope {
                graph_id: &checkpoint.graph_id,
                instance_id: instance,
                actor,
                ask: &checkpoint.asks[position],
            })
            .map_err(|error| error.to_string()),
        _ => Err("graph has no ask identity".into()),
    };
    checkpoint.asks[position].inbox_error = result.as_ref().err().map(|error| {
        bounded(
            &format!("inbox unavailable: {error}"),
            MAX_ADVANCE_ERROR_BYTES,
        )
    });
    result
}

pub(super) fn answered(reply: AskReply) -> AskAnswer {
    let rejected = reply
        .text
        .as_ref()
        .filter(|text| encoded_len(text) > ANSWER_BYTES + 2)
        .map(|_| format!("reply longer than {ANSWER_BYTES} bytes"));
    AskAnswer {
        extra: Default::default(),
        source: AnswerSource::Answered,
        option: reply.option.map(|option| bounded(&option, OPTION_BYTES)),
        text: reply.text.filter(|_| rejected.is_none()),
        actor: Some(bounded(&reply.actor, HOST_ID_BYTES)),
        at: reply.at,
        rejection_code: rejected.as_ref().map(|_| "answer_too_large".into()),
        rejected,
    }
}

fn due(ask: &NodeAsk, now: Option<WallTime>) -> bool {
    ask.wake_at
        .zip(now)
        .is_some_and(|(wake_at, now)| now >= wake_at)
}

/// Raise, poll and settle a decide node. The ask, its answer and the node's
/// outputs are saved together, so no crash leaves a decision half applied.
pub(super) fn decide<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    node: &PlannedNode,
    spec: &DecideSpec,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let mine = |ask: &NodeAsk| ask.node.as_deref() == Some(node.id.as_str());
    let latest = checkpoint.asks.iter().rposition(mine);
    let position = match latest {
        Some(position) if !checkpoint.asks[position].resolved => position,
        // A withdrawn ask is replaced once its inbox request is closed.
        Some(position) if !checkpoint.asks[position].closed => return Ok(()),
        _ => {
            if checkpoint.pause_reason.is_some() {
                return Ok(());
            }
            let session = decide_session(spec, plan.decision_session.as_deref())
                .ok_or(GraphError::BadCheckpoint)?
                .to_owned();
            // A deadline is fixed before the ask exists. Without a clock the
            // advance is refused and retried, so the timeout is never lost.
            let wake_at = match spec.timeout_seconds {
                None => None,
                Some(seconds) => {
                    let now = owner.wall_now().ok_or_else(|| {
                        GraphError::Refused(format!(
                            "node {}: no wall clock to set its decide deadline; retry the advance",
                            node.id
                        ))
                    })?;
                    let deadline = DurationMicros::from_seconds(seconds)
                        .ok()
                        .and_then(|duration| now.checked_add(duration).ok())
                        .ok_or_else(|| {
                            GraphError::Refused(format!(
                                "node {}: decide deadline passes the last representable wall time; lower timeout_seconds",
                                node.id
                            ))
                        })?;
                    Some(deadline)
                }
            };
            identity::<R, J::Error>(checkpoint, owner)?;
            let previous = latest.map(|position| &checkpoint.asks[position]);
            let sequence = previous
                .and_then(|ask| decide_sequence(ask, &node.id))
                .unwrap_or(0)
                .saturating_add(1);
            let rejected = previous
                .and_then(|ask| ask.answer.as_ref())
                .and_then(|answer| answer.rejected.as_deref());
            let ask = decide_ask(
                &plan.graph_id,
                node,
                spec,
                &session,
                sequence,
                wake_at,
                rejected,
            );
            checkpoint.asks.retain(|ask| !mine(ask));
            checkpoint.asks.push(ask);
            checkpoint.nodes[index].state = NodeState::Running;
            refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            checkpoint.asks.len() - 1
        }
    };
    let before = checkpoint.asks[position].inbox_error.clone();
    let reply = poll(checkpoint, position, owner);
    let now = owner.wall_now();
    let ask = &checkpoint.asks[position];
    let answer = match reply {
        Ok(Some(reply)) => Some(answered(reply)),
        _ if due(ask, now) => Some(AskAnswer {
            extra: Default::default(),
            source: AnswerSource::Default,
            option: ask.default.clone(),
            text: None,
            actor: None,
            at: now,
            rejected: None,
            rejection_code: None,
        }),
        _ => None,
    };
    let Some(mut answer) = answer else {
        if checkpoint.asks[position].inbox_error != before {
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        return Ok(());
    };
    let chosen = answer
        .option
        .clone()
        .or_else(|| answer.text.as_deref().map(|text| text.trim().to_owned()))
        .filter(|id| ask.options.iter().any(|option| &option.id == id));
    let ask_id = ask.ask_id.clone();
    if answer.rejected.is_none() && chosen.is_none() {
        answer.rejection_code = Some("invalid_decide_option".into());
        let ids: Vec<_> = ask
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        answer.rejected = Some(bounded(
            &format!("answer must choose one of {}", ids.join(", ")),
            MAX_ADVANCE_ERROR_BYTES,
        ));
    }
    // A rejected answer keeps the node open: once its request is closed, the
    // next advance asks again under a new ask ID and says why.
    if let (None, Some(chosen)) = (&answer.rejected, chosen) {
        let progress = &mut checkpoint.nodes[index];
        progress.state = NodeState::Succeeded;
        progress.outputs = Some(decision_outputs(&ask_id, &chosen, &answer));
        answer.option = Some(chosen);
    }
    let ask = &mut checkpoint.asks[position];
    ask.answer = Some(answer);
    ask.resolved = true;
    refresh_wake(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)
}

fn decision_outputs(ask_id: &str, option: &str, answer: &AskAnswer) -> NodeOutputs {
    let excerpt = bounded(
        &match (answer.source, &answer.actor) {
            (AnswerSource::Answered, Some(actor)) => {
                format!("decision {ask_id}: {option}, answered by {actor}")
            }
            _ => format!("decision {ask_id}: {option}, the default at the deadline"),
        },
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
        verdict: Some(option.to_owned()),
        findings: Vec::new(),
        branch: None,
        commit: None,
        warnings: Vec::new(),
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
    answer: &AskAnswer,
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
    if let Some(reason) = envelope::reason(document, &checkpoint.expansions, &all, plan) {
        return Err(("checkpoint_too_large", reason));
    }
    Ok(PauseAction::Resume(extensions))
}

/// Raise or poll the ask for a non-loop pause. Returns true when an answer
/// was applied and the caller should advance again.
pub(super) fn pause<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
    budget: &crate::config::GraphBudget,
) -> Result<bool, GraphError<R::Error, J::Error>> {
    let (Some(session), Some(reason)) = (&plan.decision_session, checkpoint.pause_reason.clone())
    else {
        return Ok(false);
    };
    if checkpoint.cancelled || loop_pause(&reason) {
        return Ok(false);
    }
    let pauses = |ask: &NodeAsk| ask.kind == AskKind::Pause;
    let position = match checkpoint.asks.iter().rposition(pauses) {
        Some(position) if !checkpoint.asks[position].resolved => position,
        Some(position) if !checkpoint.asks[position].closed => return Ok(false),
        latest => {
            identity::<R, J::Error>(checkpoint, owner)?;
            let previous = latest
                .and_then(|position| checkpoint.asks[position].answer.as_ref())
                .and_then(|answer| answer.rejected.clone());
            checkpoint.pause_asks += 1;
            let ask = pause_ask(
                &checkpoint.graph_id,
                checkpoint.pause_asks,
                session,
                &reason,
                previous.as_deref(),
            );
            checkpoint.asks.push(ask);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            checkpoint.asks.len() - 1
        }
    };
    let before = checkpoint.asks[position].inbox_error.clone();
    let reply = match poll(checkpoint, position, owner) {
        Ok(Some(reply)) => reply,
        _ => {
            if checkpoint.asks[position].inbox_error != before {
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
            return Ok(false);
        }
    };
    let mut answer = answered(reply);
    let action = match &answer.rejected {
        Some(reason) => Err(("answer_too_large", reason.clone())),
        None => pause_action(&answer, document, checkpoint, plan, budget),
    };
    let applied = match action {
        Ok(PauseAction::Cancel) => {
            checkpoint.cancelled = true;
            true
        }
        Ok(PauseAction::Resume(extensions)) => {
            for extension in extensions {
                if !checkpoint.extensions.contains(&extension) {
                    checkpoint.extensions.push(extension);
                }
            }
            checkpoint.pause_reason = None;
            checkpoint.pause_code = None;
            true
        }
        Err((code, reason)) => {
            answer.rejection_code = Some(code.into());
            answer.rejected = Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
            false
        }
    };
    let ask = &mut checkpoint.asks[position];
    ask.answer = Some(answer);
    ask.resolved = true;
    journal.save(checkpoint).map_err(GraphError::Journal)?;
    Ok(applied)
}

pub(super) fn loop_pause(reason: &str) -> bool {
    reason.starts_with("loops paused:") || reason.starts_with("loop ")
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

/// Resolve asks whose question has gone away, then close their inbox
/// requests. A close failure stays on its ask and is retried next advance.
pub fn close_resolved_asks<R: AttemptOwner, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let mut changed = false;
    for index in 0..checkpoint.asks.len() {
        let ask = &checkpoint.asks[index];
        if ask.resolved {
            continue;
        }
        let settled = ask.node.as_ref().is_none_or(|id| {
            checkpoint
                .nodes
                .iter()
                .find(|n| &n.id == id)
                .is_none_or(|n| n.state.settled())
        });
        let gone = checkpoint.cancelled
            || match ask.kind {
                AskKind::Loop => {
                    settled
                        || ask.rounds.unwrap_or(0)
                            < loop_rounds(checkpoint, ask.node.as_deref().unwrap_or_default())
                }
                AskKind::Decide | AskKind::Land => settled,
                AskKind::Pause => checkpoint.pause_reason.as_deref().is_none_or(loop_pause),
            };
        if gone {
            checkpoint.asks[index].resolved = true;
            changed = true;
        }
    }
    let closed_pauses = checkpoint
        .asks
        .iter()
        .filter(|ask| ask.kind == AskKind::Pause && ask.closed)
        .count();
    if closed_pauses > MAX_PAUSE_ASKS {
        let mut drop = closed_pauses - MAX_PAUSE_ASKS;
        checkpoint.asks.retain(|ask| {
            let old = drop > 0 && ask.kind == AskKind::Pause && ask.closed;
            drop -= usize::from(old);
            !old
        });
        changed = true;
    }
    if changed {
        refresh_wake(checkpoint);
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let (Some(instance), Some(actor)) =
        (checkpoint.instance_id.clone(), checkpoint.ask_actor.clone())
    else {
        return Ok(());
    };
    for index in 0..checkpoint.asks.len() {
        if !checkpoint.asks[index].resolved || checkpoint.asks[index].closed {
            continue;
        }
        let result = owner.close_node_ask(AskScope {
            graph_id: &checkpoint.graph_id,
            instance_id: &instance,
            actor: &actor,
            ask: &checkpoint.asks[index],
        });
        let ask = &mut checkpoint.asks[index];
        match result {
            Ok(()) => {
                ask.closed = true;
                ask.inbox_error = None;
            }
            Err(error) => {
                ask.inbox_error = Some(bounded(
                    &format!("close ask: {error}"),
                    MAX_ADVANCE_ERROR_BYTES,
                ));
            }
        }
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(())
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
            && (ask.resolved || !ask.closed)
            && (ask.resolved || ask.answer.is_none())
            && ask
                .default
                .as_ref()
                .is_none_or(|default| ask.options.iter().any(|o| &o.id == default))
            && match (ask.kind, node) {
                (AskKind::Loop, Some(node)) => {
                    ask.rounds.is_some_and(|rounds| rounds > 0)
                        && node
                            .loop_body
                            .as_ref()
                            .and_then(|spec| spec.decision_session.as_deref())
                            == session
                }
                (AskKind::Decide, Some(node)) => {
                    decide_sequence(ask, &node.id).is_some()
                        && node.decide.as_ref().is_some_and(|spec| {
                            decide_session(spec, plan.decision_session.as_deref()) == session
                        })
                }
                (AskKind::Pause, None) => plan.decision_session.as_deref() == session,
                (AskKind::Land, Some(node)) => {
                    land::sequence(ask, &node.id).is_some()
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
            NodeState::Pending | NodeState::Blocked | NodeState::Skipped => {
                progress.outputs.is_none()
            }
            NodeState::Running => progress.outputs.is_none() && latest.is_some(),
            NodeState::Succeeded => {
                latest
                    .and_then(|ask| ask.answer.as_ref())
                    .is_some_and(|answer| {
                        answer.rejected.is_none()
                            && progress.outputs.as_ref().and_then(|o| o.verdict.as_ref())
                                == answer.option.as_ref()
                    })
            }
            NodeState::Failed => progress.outputs.is_none(),
        };
    if valid { Ok(()) } else { Err(()) }
}
