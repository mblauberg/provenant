use super::*;

pub(in crate::graph) fn validate_checkpoint<R: Error + 'static, J: Error + 'static>(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
    digest: &str,
) -> Result<(), GraphError<R, J>> {
    if checkpoint.schema_version != 2
        || checkpoint.graph_id != plan.graph_id
        || checkpoint.delivery_required
            != (plan.delivery == Some(provenant_records::run::DeliveryMode::Pr))
    {
        return Err(GraphError::BadCheckpoint {
            node: plan.graph_id.clone(),
            invariant: "schema, graph identity and delivery policy must match plan",
        });
    }
    if checkpoint.plan_digest != digest {
        return Err(GraphError::PlanChanged);
    }
    if checkpoint.nodes.len() != plan.nodes.len()
        || plan
            .decision_session
            .as_ref()
            .is_some_and(|session| crate::inbox::session_id(session).is_err())
    {
        return Err(GraphError::BadCheckpoint {
            node: plan.graph_id.clone(),
            invariant: "node count and decision session must match plan",
        });
    }
    asks::validate(checkpoint, plan).map_err(|_| GraphError::BadCheckpoint {
        node: plan.graph_id.clone(),
        invariant: "asks must match checkpoint identities and decision sessions",
    })?;
    let positions: std::collections::BTreeMap<String, usize> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.clone(), i))
        .collect();
    let active_expansions = checkpoint
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.expansion_cause,
                Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
            )
        })
        .count();
    if active_expansions > 1
        || (active_expansions == 1
            && !checkpoint.nodes.iter().any(|node| {
                matches!(
                    node.expansion_cause,
                    Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
                ) && Some(plans::expansion_pause_cause(node)) == checkpoint.pause_cause()
            }))
        || (active_expansions == 0
            && checkpoint.pause_cause() == Some(PauseCause::InvalidPlanExpansion))
    {
        return Err(GraphError::BadCheckpoint {
            node: checkpoint.graph_id.clone(),
            invariant: "active expansion pause must own the graph pause",
        });
    }
    if let Some(cause) = checkpoint
        .pause_cause()
        .and_then(NodeDecisionCause::from_pause)
        && (!checkpoint
            .nodes
            .iter()
            .any(|node| node.loop_cause.as_ref() == Some(&cause)))
    {
        return Err(GraphError::BadCheckpoint {
            node: checkpoint.graph_id.clone(),
            invariant: "loop pause code must have a matching typed node cause",
        });
    }
    for (index, (progress, node)) in checkpoint.nodes.iter().zip(&plan.nodes).enumerate() {
        let mut historical;
        let checkpoint = if progress.state.settled() {
            if let Some(snapshots) = (!progress.settled_dependency_snapshots.is_empty())
                .then_some(&progress.settled_dependency_snapshots)
            {
                historical = checkpoint.clone();
                for (id, snapshot) in snapshots {
                    let position = *positions.get(id).ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "dependency snapshot must name a graph node",
                    })?;
                    let mut prior = snapshot.clone();
                    if prior.id != *id || !prior.state.settled() {
                        return Err(GraphError::BadCheckpoint {
                            node: node.id.clone(),
                            invariant: "dependency snapshot must retain settled identity",
                        });
                    }
                    prior.round = historical.nodes[position].round.clone();
                    historical.nodes[position] = prior;
                }
                &historical
            } else {
                checkpoint
            }
        } else {
            checkpoint
        };
        let bad = |invariant| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant,
        };
        let valid_loop_cause = match (
            &progress.loop_cause,
            progress.loop_pause_code.as_deref(),
            &progress.loop_pause,
        ) {
            (None, None, None) => true,
            (Some(NodeDecisionCause::LoopPaused), Some("loop_paused"), Some(_)) => {
                node.loop_body.is_some()
            }
            (Some(NodeDecisionCause::LoopAskClosed), Some("loop_ask_closed"), Some(_)) => {
                node.loop_body.is_some()
            }
            (
                Some(NodeDecisionCause::InvalidLoopDecision),
                Some("invalid_loop_decision"),
                Some(_),
            ) => node.loop_body.is_some(),
            _ => false,
        };
        let valid_expansion_cause = match (&progress.expansion_cause, &progress.expansion_pause) {
            (None, None) => true,
            (
                Some(NodeDecisionCause::PlanExpansionPaused {
                    active,
                    fallback,
                    fragment_error,
                    structural_limit,
                }),
                pause,
            ) => {
                plans::agent(node)
                    && (pause.is_some() || (*active && *fallback))
                    && structural_limit.is_none_or(|limit| limit.actual > limit.max)
                    && fragment_error.is_none_or(|fact| match fact {
                        PlanFailureFact::Structural(limit) => limit.actual > limit.max,
                        PlanFailureFact::Envelope(limit) => {
                            limit.bound > MAX_CHECKPOINT_BYTES as usize
                        }
                        _ => true,
                    })
            }
            _ => false,
        };
        let valid_truncation_owner = match &progress.truncation_cause {
            None => true,
            Some(NodeDecisionCause::CompositeFindingsTruncated { .. }) => node.composite.is_some(),
            _ => false,
        };
        for (invalid, invariant) in [
            (
                !valid_expansion_cause,
                "expansion pause cause must match its node",
            ),
            (
                !valid_loop_cause,
                "loop pause cause must match its code and loop boundary",
            ),
            (
                !valid_truncation_owner,
                "truncation cause requires a composite boundary",
            ),
            (
                !valid_compacted_findings(index, plan, checkpoint, &positions),
                "compacted findings must reconstruct from the node inputs",
            ),
            (
                progress.round != node.round,
                "round must match expanded plan",
            ),
            (
                progress.children != node.composite_members,
                "composite children must match expanded plan membership",
            ),
            (
                progress.min_successful.is_some()
                    && progress.min_successful
                        != node.composite.as_ref().and_then(|spec| spec.min_successful),
                "composite minimum must match expanded plan",
            ),
            (
                node.composite.is_some()
                    && (node.composite_members.is_empty()
                        || node
                            .composite_members
                            .iter()
                            .any(|id| !node.after_expansion.contains(id))),
                "composite completion edges must include every member",
            ),
            (
                document.schema_version >= 2 && progress.source_binding != node.source,
                "source selection must match the expanded plan",
            ),
            (
                progress.new_worktree_base.as_deref().is_some_and(|base| {
                    node.attempt
                        .as_ref()
                        .is_none_or(|spec| spec.new_worktree.is_none())
                        || base.parse::<provenant_core::GitOid>().is_err()
                }),
                "writer provisioning must retain a full commit base",
            ),
            (
                progress.source_commit.is_some()
                    && node.source.is_none()
                    && progress.writer_binding.is_none(),
                "resolved source requires a source binding",
            ),
            (
                progress
                    .source_commit
                    .as_ref()
                    .is_some_and(|commit| commit.parse::<provenant_core::GitOid>().is_err()),
                "resolved source must be a full commit ID",
            ),
            (
                progress.id != node.id,
                "row identity must match expanded plan order",
            ),
            (
                progress.worktree != node.worktree,
                "worktree must match expanded plan",
            ),
            (
                progress.attempts.len()
                    > node.routes.len().saturating_add(
                        plan.queue
                            .as_ref()
                            .map_or(0, |queue| queue.prep_retries.value),
                    ),
                "attempt count must not exceed configured routes and preparation retries",
            ),
            (
                progress
                    .outputs
                    .as_ref()
                    .is_some_and(|outputs| outputs.schema_version != 2),
                "output schema must be version 2",
            ),
        ] {
            if invalid {
                return Err(bad(invariant));
            }
        }
        let parked = if let Some(park) = &progress.delivery_park {
            let valid = (plan.queue.is_some() || node.kind == Some(NodeKind::Hygiene))
                && !park.operation_id.is_empty()
                && park.operation_id == park.transition_id
                && park.operation_id.len() <= 256
                && park.item.as_deref()
                    == document
                        .context
                        .as_ref()
                        .and_then(|context| context.work_item.as_deref())
                && park.handoff.len() <= 1024
                && park.evidence.len() <= 8
                && park.evidence.iter().all(|entry| entry.len() <= 512)
                && park
                    .attempt_key
                    .as_ref()
                    .is_none_or(|key| key.graph_id == plan.graph_id)
                && park
                    .head
                    .as_deref()
                    .is_none_or(|head| head.parse::<provenant_core::GitOid>().is_ok())
                && matches!(
                    (progress.state, park.request_id.is_some()),
                    (NodeState::Running, false) | (NodeState::Blocked, true)
                );
            if !valid {
                return Err(bad(
                    "delivery park intent and receipt must match the graph and state",
                ));
            }
            true
        } else {
            false
        };
        if let Some(condition) = &node.condition {
            let valid = match (&progress.gate, progress.state) {
                (None, NodeState::Pending | NodeState::Skipped) => {
                    progress.outputs.is_none()
                        && progress.ci.is_none()
                        && progress.conflict.is_none()
                }
                (None, NodeState::Blocked) => {
                    progress.outputs.is_none()
                        && ((progress.ci.is_none() && progress.conflict.is_none())
                            || (parked
                                && progress
                                    .ci
                                    .as_ref()
                                    .is_some_and(|ci| ci.state == CiState::Pending)))
                }
                (None, NodeState::Running) => {
                    ((matches!(node.condition, Some(GateCondition::Ci { .. }))
                        && progress.ci.as_ref().is_some_and(|ci| {
                            ci.state == CiState::Pending
                                && (ci.wake_at.is_some() || parked)
                                && ci.pending_observations > 0
                        }))
                        || (matches!(node.condition, Some(GateCondition::Conflict { .. }))
                            && progress.conflict.as_ref().is_some_and(|conflict| {
                                conflict.state == ConflictState::Pending
                                    && conflict.wake_at.is_some()
                                    && conflict.pending_observations > 0
                            })))
                        && progress.outputs.is_none()
                        && dependency_state(node, plan, checkpoint, &positions).is_none()
                }
                (
                    Some(result),
                    NodeState::Succeeded
                    | NodeState::Failed
                    | NodeState::Running
                    | NodeState::Blocked,
                ) if !matches!(progress.state, NodeState::Running | NodeState::Blocked)
                    || parked =>
                {
                    let mut expected = if matches!(condition, GateCondition::Ci { .. }) {
                        ci::outputs(result.passed, progress.ci.as_ref(), plan.queue.is_some())
                    } else {
                        gate_outputs(result.passed)
                    };
                    // Older checkpoints lack availability evidence; retain
                    // that unknown state rather than reconstructing it.
                    if progress
                        .outputs
                        .as_ref()
                        .is_some_and(|outputs| outputs.findings_available.is_none())
                    {
                        expected.findings_available = None;
                    }
                    (if parked {
                        !result.passed
                    } else {
                        result.passed == (progress.state == NodeState::Succeeded)
                    }) && result.passed == result.reason.is_none()
                        && result.on_fail == node.on_fail
                        && progress.outputs == Some(expected)
                        && dependency_state(node, plan, checkpoint, &positions).is_none()
                }
                _ => false,
            };
            let bound_head_valid = if plan.queue.is_some() && progress.gate.is_some() {
                let (target, observed) = match condition {
                    GateCondition::Ci { target, .. } => (
                        Some(target),
                        progress.ci.as_ref().and_then(|ci| ci.head_sha.as_ref()),
                    ),
                    GateCondition::Conflict { target, .. } => (
                        Some(target),
                        progress
                            .conflict
                            .as_ref()
                            .and_then(|conflict| conflict.head_sha.as_ref()),
                    ),
                    _ => (None, None),
                };
                target.is_none_or(|target| {
                    ci::published_head(target, checkpoint, true).as_ref() == observed
                        && observed.is_some()
                })
            } else {
                true
            };
            if !valid
                || !bound_head_valid
                || (progress.state != NodeState::Running
                    && progress.ci.as_ref().is_some_and(|ci| ci.wake_at.is_some()))
                || progress
                    .ci
                    .as_ref()
                    .is_some_and(|evidence| !ci::valid_evidence(evidence, condition))
                || !progress.attempts.is_empty()
                || progress.advance_error.is_some()
                || (progress.ci.is_some()
                    && !matches!(node.condition, Some(GateCondition::Ci { .. })))
                || (matches!(node.condition, Some(GateCondition::Ci { .. }))
                    && progress.gate.is_some()
                    && progress.ci.is_none())
                || (progress.conflict.is_some()
                    && !matches!(node.condition, Some(GateCondition::Conflict { .. })))
                || (matches!(node.condition, Some(GateCondition::Conflict { .. }))
                    && progress.gate.as_ref().is_some_and(|gate| {
                        progress.conflict.as_ref().is_none_or(|conflict| {
                            conflict.wake_at.is_some()
                                || conflict.head_sha.as_deref().is_none_or(|head| {
                                    head.parse::<provenant_core::GitOid>().is_err()
                                })
                                || if gate.passed {
                                    conflict.state != ConflictState::Clear
                                } else {
                                    conflict.state != ConflictState::Conflicting
                                }
                        })
                    }))
            {
                return Err(bad(
                    "gate evidence must match state, dependencies and condition",
                ));
            }
        } else if progress.gate.is_some() && !repair::is_repair(node)
            || progress.ci.is_some()
            || progress.conflict.is_some()
        {
            return Err(bad("only gate nodes may carry gate or CI evidence"));
        }
        if !when::holds(&node.when) {
            // A node its own condition skips is a leaf of the live graph: it
            // never runs, whatever its former parents do.
            if progress.state != NodeState::Skipped
                || !progress.attempts.is_empty()
                || progress.outputs.is_some()
            {
                return Err(bad(
                    "a node skipped by its when condition has no attempts or outputs",
                ));
            }
            continue;
        }
        if progress.state == NodeState::Skipped
            && (dependency_state(node, plan, checkpoint, &positions) != Some(NodeState::Skipped)
                && !loops::inactive_round(node, plan, checkpoint, &positions)
                && !repair::inactive_lens(node, plan, checkpoint, &positions)
                && !patch::skipped_by_patch(checkpoint, &node.id)
                || !progress.attempts.is_empty()
                || progress.outputs.is_some())
        {
            return Err(bad(
                "skipped node must have an inactive dependency and no attempts or outputs",
            ));
        }
        if node.loop_body.is_some() {
            loops::validate_progress(
                progress,
                node,
                plan,
                checkpoint,
                &positions,
                plan.queue.is_some(),
            )
            .map_err(|_| bad("loop progress must match round state"))?;
            continue;
        }
        if node.composite.is_some() {
            composites::validate_progress(progress, node, plan, checkpoint, &positions)
                .map_err(|_| bad("composite state and outputs must match member settlement"))?;
            continue;
        }
        if node.decide.is_some() {
            asks::validate_progress(progress, node, checkpoint)
                .map_err(|_| bad("decision progress must match its ask"))?;
            continue;
        }
        if node.land.is_some() {
            land::validate_progress(progress, checkpoint)
                .map_err(|_| bad("land progress must match its record"))?;
            continue;
        } else if progress.land.is_some() {
            return Err(bad("land record requires a land node"));
        }
        let requests_review_base =
            node.access == AccessMode::ReadOnly && node.review_base.is_some();
        if progress.review_base_pending
            && (!requests_review_base
                || progress.review_base.is_some()
                || !progress.attempts.is_empty())
        {
            return Err(bad(
                "pending review base must belong to an unlaunched reader",
            ));
        }
        if document.schema_version >= 2
            && requests_review_base
            && progress.review_base.is_none()
            && !progress.review_base_pending
        {
            return Err(bad("reader must retain its review base binding"));
        }
        if progress.writer_binding_pending
            && (!plans::writer(node)
                || progress.writer_binding.is_some()
                || !progress.attempts.is_empty())
        {
            return Err(bad(
                "pending writer binding must belong to an unlaunched writer",
            ));
        }
        if let Some(binding) = &progress.writer_binding {
            if !plans::writer(node)
                || !std::path::Path::new(&binding.worktree).is_absolute()
                || binding.worktree.len() > 4096
                || binding.head.parse::<provenant_core::GitOid>().is_err()
                || binding.shared_worktree.as_ref().is_some_and(|subject| {
                    subject
                        .worktree_identity()
                        .is_none_or(|identity| identity.path != binding.worktree)
                        || binding.source_directory_identity.is_none()
                })
                || (node.source.is_none()
                    && node.continues.is_none()
                    && progress.source_commit.as_ref() != Some(&binding.head))
            {
                return Err(bad(
                    "writer binding must retain its worktree and initial head",
                ));
            }
        } else if document.schema_version >= 2
            && plans::writer(node)
            && !peer_owned_writer(node.access, node.worktree.as_deref(), node.attempt.as_ref())
            && !progress.writer_binding_pending
        {
            return Err(bad("writer must retain its binding"));
        }
        if !progress.attempts.is_empty()
            && node.source.is_some()
            && progress.source_commit.is_none()
        {
            return Err(bad(
                "launched source-bound node must retain its resolved source commit",
            ));
        }
        if let Some(commit) = &progress.source_commit {
            let expected = match &node.source {
                Some(SourceBinding::Commit { commit }) => Some(commit),
                Some(SourceBinding::Node { node: source }) => positions
                    .get(source)
                    .and_then(|index| checkpoint.nodes[*index].outputs.as_ref())
                    .and_then(|outputs| outputs.commit.as_ref()),
                _ => None,
            };
            if matches!(
                node.source,
                Some(SourceBinding::Commit { .. } | SourceBinding::Node { .. })
            ) && expected != Some(commit)
            {
                return Err(bad("resolved source commit must match its binding"));
            }
        }
        if node.kind == Some(NodeKind::Publish) {
            let intent = progress.delivery_publication.as_ref();
            let receipt = intent.and_then(|intent| intent.receipt.as_ref());
            let valid_intent = intent.is_none_or(|intent| {
                !intent.operation_id.is_empty()
                    && intent.operation_id.len() <= 256
                    && match &node.source {
                        Some(SourceBinding::Node { node: hygiene }) => positions.get(hygiene)
                            .is_some_and(|hygiene_index| {
                                let hygiene_node = &plan.nodes[*hygiene_index];
                                let hygiene_outcome = checkpoint.nodes[*hygiene_index].outputs.as_ref()
                                    .and_then(|outputs| outputs.hygiene.as_ref());
                                matches!(&hygiene_node.source, Some(SourceBinding::Node { node }) if node == &intent.source_node)
                                    && hygiene_outcome.is_some_and(|outcome| outcome.kind == provenant_records::graph::HygieneOutcomeKind::Clean
                                        && outcome.after_head.as_deref() == Some(intent.input_head.as_str()))
                            }),
                        _ => false,
                    }
                    && intent.source_key.graph_id == plan.graph_id
                    && intent.source_key.node_id == intent.source_node
                    && !intent.source_attempt.is_empty()
                    && intent.input_head.parse::<provenant_core::GitOid>().is_ok()
                    && intent.integration_base.parse::<provenant_core::GitOid>().is_ok()
                    && checkpoint.bases.and_then(|bases| bases.integration).is_some_and(|base| base.to_string() == intent.integration_base)
                    && intent.expected_remote_head.as_deref().is_none_or(|head| head.parse::<provenant_core::GitOid>().is_ok())
                    && intent.expected_remote_head.as_ref() == plan.nodes[..index].iter().zip(&checkpoint.nodes[..index])
                        .filter(|(prior, _)| prior.kind == Some(NodeKind::Publish))
                        .filter_map(|(_, prior)| prior.delivery_publication.as_ref())
                        .filter(|prior| prior.source_branch == intent.source_branch)
                        .filter_map(|prior| prior.receipt.as_ref().map(|receipt| &receipt.head))
                        .next_back()
                    && !intent.writer_branch.is_empty()
                    && document.integration_branch.as_deref() == Some(intent.source_branch.as_str())
                    && intent.receipt.as_ref().is_none_or(|receipt| {
                        receipt.head == intent.input_head
                            && !receipt.branch.is_empty()
                            && !receipt.pull_request.is_empty()
                    })
            });
            let valid_output = match (receipt, progress.outputs.as_ref()) {
                (Some(receipt), Some(outputs)) => {
                    outputs.publication.as_ref() == Some(receipt)
                        && outputs.branch.as_ref() == Some(&receipt.branch)
                        && outputs.commit.as_ref() == Some(&receipt.head)
                }
                (None, None) => true,
                _ => false,
            };
            let valid_state = match progress.state {
                NodeState::Pending | NodeState::Skipped => intent.is_none(),
                NodeState::Running => intent.is_some() && receipt.is_none(),
                NodeState::Succeeded => receipt.is_some(),
                NodeState::Blocked => (intent.is_none() && progress.outputs.is_none()) || parked,
                NodeState::Failed => false,
            };
            if !progress.attempts.is_empty() || !valid_intent || !valid_output || !valid_state {
                return Err(bad(
                    "publication intent, exact-head receipt and park must match its state",
                ));
            }
            continue;
        }
        if node.kind == Some(NodeKind::Hygiene) {
            let intent = progress.delivery_hygiene.as_ref();
            let outcome = intent.and_then(|intent| intent.outcome.as_ref());
            let output = progress
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.hygiene.as_ref());
            let valid = progress.attempts.is_empty()
                && intent.is_none_or(|intent| {
                    !intent.operation_id.is_empty()
                        && intent.input_head.parse::<provenant_core::GitOid>().is_ok()
                })
                && output == outcome
                && match progress.state {
                    NodeState::Pending | NodeState::Skipped => {
                        intent.is_none() && progress.outputs.is_none()
                    }
                    NodeState::Running => intent.is_some(),
                    NodeState::Succeeded => outcome.is_some_and(|outcome| {
                        outcome.kind == provenant_records::graph::HygieneOutcomeKind::Clean
                    }),
                    NodeState::Failed => outcome.is_some_and(|outcome| {
                        matches!(
                            outcome.kind,
                            provenant_records::graph::HygieneOutcomeKind::NoCommit
                                | provenant_records::graph::HygieneOutcomeKind::UncommittedEdits
                        )
                    }),
                    NodeState::Blocked => {
                        (intent.is_none() && progress.outputs.is_none())
                            || (outcome.is_some_and(|outcome| {
                                outcome.kind == provenant_records::graph::HygieneOutcomeKind::Error
                            }) && progress
                                .delivery_park
                                .as_ref()
                                .is_some_and(|park| park.request_id.is_some()))
                    }
                };
            if !valid {
                return Err(bad(
                    "hygiene intent, outcome and park receipt must match its state",
                ));
            }
            continue;
        }
        if node.condition.is_some() {
            continue;
        }
        let mut previous_number = 0;
        for attempt in progress.recorded_attempts() {
            if attempt.key.graph_id != plan.graph_id
                || attempt.key.node_id != node.id
                || attempt.key.attempt <= previous_number
            {
                return Err(bad(
                    "attempts must retain distinct WorkKey identities for this node",
                ));
            }
            previous_number = attempt.key.attempt;
        }
        let escalation_policy = plans::writer(node)
            .then(|| loops::escalation_owner(index, plan).map(|(_, spec)| spec))
            .flatten();
        let preparation_refused = |attempt: &GraphAttempt| {
            plan.queue.is_some()
                && attempt.state == AttemptState::Terminal
                && !attempt.launched
                && attempt.host_refused_before_acceptance
                && attempt
                    .outcome
                    .is_some_and(|outcome| matches!(outcome, AttemptOutcome::PreparationRefused(_)))
        };
        if progress
            .attempts
            .iter()
            .filter(|attempt| preparation_refused(attempt))
            .count()
            > plan
                .queue
                .as_ref()
                .map_or(0, |queue| queue.prep_retries.value.saturating_add(1))
        {
            return Err(bad(
                "preparation refusals must respect the sealed retry bound",
            ));
        }
        for (index, attempt) in progress.attempts.iter().enumerate() {
            if document.schema_version >= 2 && attempt.input_manifest.is_none() {
                return Err(bad("attempt must retain its input manifest"));
            }
            if let Some(manifest) = &attempt.input_manifest {
                let count: usize = node.inputs.iter().map(|input| input.outputs.len()).sum();
                if manifest.schema_version != 1
                    || manifest.inputs.len() > MANIFEST_INPUT_LIMIT
                    || manifest.inputs.len().saturating_add(manifest.omitted) != count
                    || manifest.source_commit != progress.source_commit
                    || manifest.review_base != progress.review_base
                    || manifest
                        .inputs
                        .iter()
                        .zip(
                            node.inputs.iter().flat_map(|input| {
                                input.outputs.iter().map(move |kind| (input, kind))
                            }),
                        )
                        .any(|(evidence, (input, kind))| {
                            evidence.producer_node != input.node
                                || evidence.kind != *kind
                                || evidence.required != input.required_outputs.contains(kind)
                        })
                    || (attempt.state != AttemptState::Starting
                        && manifest
                            .inputs
                            .iter()
                            .any(|input| input.file_delivery == InputDelivery::Pending))
                {
                    return Err(bad(
                        "attempt input manifest must match its selected inputs and pins",
                    ));
                }
            }
            let expected_route_index = if index == 0 {
                0
            } else {
                let previous = &progress.attempts[index - 1];
                usize::from(previous.key.route_index) + usize::from(!preparation_refused(previous))
            };
            let route_matches = match (escalation_policy, attempt.escalation_rung) {
                (Some(policy), Some(rung)) => policy.ladder.get(rung).is_some_and(|alias| {
                    node.routes
                        .get(expected_route_index)
                        .is_some_and(|route| attempt.route == loops::tier_route(Some(route), alias))
                }),
                (None, None) => node.routes.get(expected_route_index) == Some(&attempt.route),
                _ => false,
            };
            if attempt.key.graph_id != plan.graph_id
                || attempt.key.node_id != node.id
                || usize::from(attempt.key.route_index) != expected_route_index
                || !route_matches
                || (attempt.review_substitution
                    && (index == 0
                        || !(review_substitution(node, &progress.attempts[index - 1])
                            || review_preparation_substitution(
                                node,
                                &progress.attempts[index - 1],
                            ))))
                || (index > 0
                    && review_substitution(node, &progress.attempts[index - 1])
                    && !attempt.review_substitution)
                || (attempt.state == AttemptState::Terminal) != attempt.outcome.is_some()
                || (index + 1 < progress.attempts.len()
                    && (attempt.state != AttemptState::Terminal
                        || !(preparation_refused(attempt)
                            || review_substitution(node, attempt)
                            || attempt.outcome.is_some_and(|outcome| {
                                fallback_candidate(outcome, node.access, attempt.launched)
                            }))))
            {
                return Err(bad(
                    "attempt key, route, terminal outcome and fallback order must match plan",
                ));
            }
        }
        let last = progress.attempts.last();
        let state_is_consistent = match progress.state {
            NodeState::Pending | NodeState::Skipped => progress.attempts.is_empty(),
            NodeState::Blocked => {
                progress.attempts.is_empty()
                    || (matches!(
                        progress.advance_error_code.as_deref(),
                        Some("launch_refused" | "launch_retry_exhausted")
                    ) && last.is_some_and(|attempt| {
                        attempt.state != AttemptState::Terminal && attempt.attempt_ref.is_none()
                    }))
                    || (progress.advance_error_code.as_deref()
                        == Some("steer_continuation_refused")
                        && progress.advance_error.is_some()
                        && last.is_some_and(|attempt| {
                            attempt.state == AttemptState::Terminal && attempt.attempt_ref.is_some()
                        }))
            }
            NodeState::Running => match last {
                Some(attempt) if attempt.state != AttemptState::Terminal => true,
                Some(attempt) => {
                    (preparation_refused(attempt)
                        && progress
                            .attempts
                            .iter()
                            .filter(|attempt| preparation_refused(attempt))
                            .count()
                            <= plan
                                .queue
                                .as_ref()
                                .map_or(0, |queue| queue.prep_retries.value))
                        || ((review_substitution(node, attempt)
                            || attempt.outcome.is_some_and(|outcome| {
                                fallback_candidate(outcome, node.access, attempt.launched)
                            }))
                            && usize::from(attempt.key.route_index) + 1 < node.routes.len())
                }
                // Writer binding and provisioning wait before the first
                // attempt exists. Only the bounded contention state emitted by
                // defer_launch_contention may replay as Running here.
                None => {
                    plans::writer(node)
                        && progress.writer_binding_pending
                        && progress.outputs.is_none()
                        && progress.advance_error.is_none()
                        && progress
                            .advance_owner_code
                            .as_deref()
                            .is_some_and(super::next_attempt::launch_contention_code)
                        && progress.launch_retry.as_ref().is_some_and(|retry| {
                            retry.failures > 0
                                && matches!((retry.wake_at, retry.deadline),
                                    (Some(wake), Some(deadline)) if wake <= deadline)
                                && retry.reason.as_ref().is_some_and(|reason| {
                                    !reason.is_empty() && reason.len() <= MAX_ADVANCE_ERROR_BYTES
                                })
                        })
                }
            },
            // A planner whose failure was settled by its static fallback is
            // succeeded though its last attempt failed (or never launched).
            NodeState::Succeeded => {
                last.is_some_and(|attempt| {
                    attempt.state == AttemptState::Terminal
                        && matches!(
                            attempt.outcome,
                            Some(AttemptOutcome::Succeeded | AttemptOutcome::ReviewRequired)
                        )
                }) || fallback_settled(node, progress, document, checkpoint)
            }
            NodeState::Failed if progress.advance_error.is_some() => last.is_none_or(|attempt| {
                attempt.state == AttemptState::Terminal
                    && attempt
                        .outcome
                        .is_some_and(|outcome| outcome != AttemptOutcome::Succeeded)
            }),
            NodeState::Failed => last.is_some_and(|attempt| {
                attempt.state == AttemptState::Terminal
                    && match attempt.outcome {
                        Some(
                            AttemptOutcome::TaskFailed
                            | AttemptOutcome::OtherFailure
                            | AttemptOutcome::ReviewRequired,
                        ) => true,
                        Some(outcome) if outcome != AttemptOutcome::Succeeded => {
                            !fallback_candidate(outcome, node.access, attempt.launched)
                                || usize::from(attempt.key.route_index) + 1 == node.routes.len()
                        }
                        _ => false,
                    }
            }),
        };
        if !state_is_consistent
            && !(parked && last.is_none_or(|attempt| attempt.state == AttemptState::Terminal))
        {
            return Err(bad(
                "node state must agree with terminal attempt outcome or validated planner fallback",
            ));
        }
    }
    Ok(())
}

/// A planner settled by its static fallback: it recorded the reason and
/// persisted exactly the stage its fallback spec derives from the document.
fn fallback_settled(
    node: &PlannedNode,
    progress: &NodeProgress,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
) -> bool {
    let Some(spec) = node.plan.as_ref().filter(|spec| spec.fallback.is_some()) else {
        return false;
    };
    progress
        .outputs
        .as_ref()
        .is_some_and(|outputs| outputs.plan.is_some())
        && plans::fallback_expansion_for_generation(
            &node.id,
            spec,
            document,
            node.require_skill_location,
            progress.retry_generation,
        )
        .is_ok_and(|derived| {
            checkpoint
                .expansions
                .iter()
                .any(|expansion| expansion.source == node.id && *expansion == derived)
        })
}
