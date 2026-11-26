use super::*;

pub(in crate::graph) fn validate_progress(
    progress: &NodeProgress,
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    queue: bool,
) -> Result<(), ()> {
    if checkpoint.ask_actor.is_none()
        || !progress.attempts.is_empty()
        || progress.gate.is_some() && !repair::is_repair(node)
        || progress.advance_note.is_some()
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
pub(in crate::graph) fn settle<R: AttemptOwner, J: GraphJournal>(
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
            .filter_map(|progress| progress.advance_note.clone())
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
                        .outputs
                        .as_ref()
                        .and_then(|outputs| outputs.handoff.as_ref())
                        .or(progress.handoff.as_ref())
                })
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "loop exhaustion handoff must be serializable",
                })?
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
pub(in crate::graph) fn pause_invalid_decision<R: Error + 'static, J: GraphJournal>(
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
            bounded(reason, MAX_ADVANCE_NOTE_BYTES)
        ));
    }
    refresh_pause(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)
}

pub(in crate::graph) enum DecisionExtensionError {
    Invalid(String),
    Structural(GraphStructuralLimit),
}

pub(in crate::graph) fn prepare_decision_extension(
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

pub(in crate::graph) fn reopen_after_delivery(
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

pub(in crate::graph) fn refresh_pause(checkpoint: &mut GraphCheckpoint) {
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
