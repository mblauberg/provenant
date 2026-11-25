use super::*;

pub(in crate::graph) fn land_ask(
    graph_id: &str,
    node: &PlannedNode,
    record: &LandRecord,
    session: &str,
    sequence: u64,
    generation: u64,
) -> AskRequest {
    let mut ask = asks::ask(
        ask_id(&node.id, sequence, generation),
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
    code: &'static str,
    reason: String,
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let progress = &mut checkpoint.nodes[index];
    progress.state = NodeState::Failed;
    progress.outputs = None;
    progress.advance_error = Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
    progress.advance_error_code = Some(code.into());
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
        repair_design: None,
        partial: false,
        risk_tier: None,
        repair_items: None,
        spec_concerns: None,
        spec_concern_count: None,
        spec_defect_count: None,
        artifact_findings: None,
        full_artifact_findings: None,
        compact_disposition_findings: None,
        full_disposition_findings: None,
        verdict_error: None,
        hygiene: None,
        publication: None,
        handoff: None,
        schema_version: 2,
        decide_outcome: None,
        result: Some(ResultOutput {
            extra: Default::default(),
            digest: CanonicalV1::file_digest(excerpt.as_bytes()).to_string(),
            excerpt,
            truncated: false,
            path: None,
        }),
        verdict: Some(LANDED.into()),
        findings: Vec::new(),
        findings_available: None,
        branch: Some(record.branch.clone()),
        commit: Some(record.head.clone()),
        warnings: Vec::new(),
        plan: None,
        remaining: None,
    }
}

/// Pin, authorise and land. Each step is saved before the next, so a replay
/// never asks twice or merges a different head.
#[allow(clippy::too_many_arguments)]
pub(in crate::graph) fn advance<R: AttemptOwner, J: GraphJournal>(
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
    let target_index = positions.get(&spec.node).copied();
    let publish_target = target_index.is_some_and(|target| {
        plan.nodes[target].kind == Some(NodeKind::Publish)
            || plan.nodes[target]
                .loop_body
                .as_ref()
                .is_some_and(|body| body.selected_publication.is_some())
    });
    let publication_node = target_index.and_then(|target| {
        if plan.nodes[target].kind == Some(NodeKind::Publish) {
            Some(spec.node.clone())
        } else {
            selected_loop_publication_node(plan, checkpoint, positions, &spec.node)
        }
    });
    let published_subject = if publish_target {
        land_subject(plan, checkpoint, positions, &spec.node)
    } else {
        None
    };
    if publish_target && published_subject.is_none() {
        return fail::<R, J>(
            index,
            "land_publication_missing",
            format!(
                "node {}: publication {} has no consistent exact-head receipt to land",
                node.id, spec.node
            ),
            checkpoint,
            journal,
        );
    }
    if let Some((branch, head)) = published_subject.as_ref() {
        if checkpoint.nodes[index]
            .land
            .as_ref()
            .is_some_and(|record| record.branch != branch.as_str() || record.head != head.as_str())
        {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "land record must match its queue publication receipt",
            });
        }
        let publication_node =
            publication_node
                .as_deref()
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "selected loop publication must resolve for landing",
                })?;
        if let Some(reason) = queue_landing_gate_error(
            plan,
            checkpoint,
            positions,
            &node.id,
            publication_node,
            head,
        ) {
            return fail::<R, J>(index, "land_gate_stale", reason, checkpoint, journal);
        }
        if let Some(reason) = acceptance_error(
            plan,
            checkpoint,
            positions,
            owner,
            &node.id,
            publication_node,
            (branch.as_str(), head.as_str()),
        )
        .map_err(GraphError::Run)?
        {
            return fail::<R, J>(index, "land_review_required", reason, checkpoint, journal);
        }
    }
    if checkpoint.nodes[index].land.is_none() {
        let Some((branch, head)) = land_subject(plan, checkpoint, positions, &spec.node) else {
            return fail::<R, J>(
                index,
                "land_delivery_missing",
                format!(
                    "node {}: target {} delivered no branch and commit to land",
                    node.id, spec.node
                ),
                checkpoint,
                journal,
            );
        };
        if !publish_target
            && let Some(reason) = acceptance_error(
                plan,
                checkpoint,
                positions,
                owner,
                &node.id,
                &spec.node,
                (branch.as_str(), head.as_str()),
            )
            .map_err(GraphError::Run)?
        {
            return fail::<R, J>(index, "land_review_required", reason, checkpoint, journal);
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
    let mut record =
        checkpoint.nodes[index]
            .land
            .clone()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "running land node must have a land record",
            })?;
    // A tier that asks only observes: the chair lands by hand.
    let asked = match record.authorised_by {
        Some(_) => None,
        None => match ask_chair(index, node, spec, plan, &record, checkpoint, owner, journal)? {
            Some(ask_id) => Some(ask_id),
            None => return Ok(()),
        },
    };
    let observation = loop {
        let observation = owner
            .land(LandRequest {
                graph_id: &plan.graph_id,
                node_id: &node.id,
                branch: &record.branch,
                head: &record.head,
                pull_request: record.pull_request,
                tier: record.tier,
                authority: record.authorised_by.as_ref(),
            })
            .map_err(GraphError::Run)?;
        match observation {
            LandObservation::Identified { pull_request } => {
                if pull_request == 0 || record.pull_request.is_some() {
                    return Err(GraphError::Refused(format!(
                        "node {}: land owner returned an invalid or repeated PR identity",
                        node.id
                    )));
                }
                record.pull_request = Some(pull_request);
                checkpoint.nodes[index].land = Some(record.clone());
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
            other => break other,
        }
    };
    record.wake_at = None;
    match observation {
        LandObservation::Identified { .. } => {
            unreachable!("identity is saved in the observation loop")
        }
        LandObservation::Merged { pull_request, note } => {
            if record.pull_request.is_none() || record.pull_request != pull_request {
                return Err(GraphError::Refused(format!(
                    "node {}: land observation did not match the saved PR identity",
                    node.id
                )));
            }
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
                "land_refused",
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
    let mine = |ask: &AskLink| ask.node.as_deref() == Some(node.id.as_str());
    let position = match checkpoint.asks.iter().rposition(mine) {
        Some(position) => position,
        None => {
            let Some(session) = session(spec, plan).map(str::to_owned) else {
                fail::<R, J>(
                    index,
                    "land_session_missing",
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
            let generation = asks::retry_generation(&checkpoint.nodes[index]);
            let mut filed = None;
            for sequence in 1..=MAX_LAND_SEQUENCE {
                let request =
                    land_ask(&plan.graph_id, node, record, &session, sequence, generation);
                let (link, closed) = asks::file(checkpoint, owner, journal, &request)?;
                // A request an earlier cancel closed keeps its ask ID, so a
                // raise after a resume files under the next one.
                if !closed {
                    filed = Some(link);
                    break;
                }
            }
            let Some(link) = filed else {
                return Err(GraphError::Refused(format!(
                    "node {}: {MAX_LAND_SEQUENCE} land requests are already closed; retry the node",
                    node.id
                )));
            };
            checkpoint.asks.push(link);
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            checkpoint.asks.len() - 1
        }
    };
    let ask_id = checkpoint.asks[position].ask_id.clone();
    let Ok(Some(reply)) = asks::poll(checkpoint, position, owner) else {
        return Ok(Some(ask_id));
    };
    let answer = asks::answered(reply);
    let actor = answer.actor.clone().unwrap_or_default();
    let code = if answer.option.as_deref() == Some(HOLD) {
        "land_held"
    } else {
        "land_answer_cannot_approve"
    };
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
    progress.advance_error_code = Some(code.into());
    // A failed land keeps no retry deadline from an earlier pending observation.
    if let Some(land) = progress.land.as_mut() {
        land.wake_at = None;
    }
    checkpoint.asks.remove(position);
    asks::refresh_wake(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)?;
    Ok(None)
}

pub(in crate::graph) fn validate_progress(
    progress: &NodeProgress,
    _checkpoint: &GraphCheckpoint,
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
                // The ask itself is the inbox's; the chair landed by hand on its
                // authority, so the node names the one ask it raises.
                && parse(ask_id, &progress.id)
                    .is_some_and(|(_, generation)| generation == asks::retry_generation(progress))
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

#[cfg(test)]
mod recovery_members_tests {
    use super::*;

    #[test]
    fn recovery_members_excludes_after_expansion_prerequisites() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":2,"graph_id":"recovery-members",
            "nodes":[
                {"id":"recovery","access":"worktree_write","worktree":"/wt/retained",
                 "prompt":"rebase","write_scope":["src"],"route":{"alias":"senior"}},
                {"id":"prerequisite","access":"worktree_write","worktree":"/wt/other",
                 "prompt":"unrelated prerequisite","write_scope":["docs"],
                 "route":{"alias":"senior"}}
            ]
        }))
        .unwrap();
        let (plan, positions) =
            validate_flat_plan(&document, &crate::config::GraphLimits::default()).unwrap();
        let recovery = positions["recovery"];
        let prerequisite = positions["prerequisite"];
        let mut nodes = plan.nodes;
        nodes[recovery].after_expansion.push("prerequisite".into());

        let members = recovery_members(recovery, &nodes, &positions);
        assert!(members.contains(&recovery));
        assert!(!members.contains(&prerequisite));
    }
}
