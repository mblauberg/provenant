use super::*;

fn delivery_text(value: &str, max_bytes: usize) -> String {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn delivery_operation(
    domain: &str,
    prefix: &str,
    value: &impl Serialize,
) -> Result<String, CanonicalError> {
    let digest = CanonicalV1::record_digest(domain, 1, value)?;
    Ok(format!(
        "{prefix}-{}",
        provenant_core::hex(digest.as_bytes())
    ))
}

/// Persist the parking identity before opening the inbox, then replay the same
/// immutable operation until a receipt is recorded.
#[allow(
    clippy::too_many_arguments,
    reason = "parking records the complete delivery transition"
)]
pub(super) fn park_delivery_transition<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    reason: provenant_records::graph::ParkReason,
    attempt_key: Option<&AttemptKey>,
    attempt_ref: Option<&str>,
    handoff: &str,
    head: Option<&str>,
    evidence: &[String],
    owner: &mut R,
    journal: &mut J,
    checkpoint: &mut GraphCheckpoint,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node_id = checkpoint
        .nodes
        .get(node_index)
        .map(|node| node.id.clone())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "delivery park node index must exist",
        })?;
    // TODO(#3972-queue): queue admission resolves tracker identities and seals
    // them in the graph context before launching item graphs.
    let item = document
        .context
        .as_ref()
        .and_then(|context| context.work_item.as_deref());
    let bounded_handoff = delivery_text(handoff, 1024);
    let bounded_evidence: Vec<String> = evidence
        .iter()
        .take(8)
        .map(|entry| delivery_text(entry, 512))
        .collect();
    let identity = serde_json::json!({
        "graph": document.graph_id,
        "item": item,
        "node": node_id,
        "reason": reason,
        "attempt_key": attempt_key,
        "attempt_ref": attempt_ref,
        "head": head,
        "handoff": bounded_handoff,
        "evidence": bounded_evidence,
    });
    let operation_id =
        delivery_operation("delivery-park", "park", &identity).map_err(GraphError::Canonical)?;
    let transition_id = operation_id.clone();

    if let Some(existing) = checkpoint.nodes[node_index].delivery_park.as_ref() {
        if existing.transition_id != transition_id {
            return Err(GraphError::BadCheckpoint {
                node: node_id,
                invariant: "one graph node cannot replace a persisted delivery park transition",
            });
        }
        if existing.request_id.is_some() {
            if checkpoint.nodes[node_index].state != NodeState::Blocked {
                checkpoint.nodes[node_index].state = NodeState::Blocked;
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
            return Ok(());
        }
    } else {
        checkpoint.nodes[node_index].delivery_park = Some(DeliveryParkProgress {
            transition_id: transition_id.clone(),
            operation_id: operation_id.clone(),
            item: item.map(str::to_owned),
            attempt_key: attempt_key.cloned(),
            attempt_ref: attempt_ref.map(str::to_owned),
            reason,
            handoff: bounded_handoff.clone(),
            head: head.map(str::to_owned),
            evidence: bounded_evidence.clone(),
            request_id: None,
        });
        checkpoint.nodes[node_index].state = NodeState::Running;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }

    let request_id = owner
        .park_delivery(DeliveryParkRequest {
            transition_id: &transition_id,
            operation_id: &operation_id,
            graph_id: &document.graph_id,
            item,
            node_id: &node_id,
            attempt_key,
            attempt_ref,
            repository: None,
            session: document.decision_session.as_deref(),
            reason,
            handoff: &bounded_handoff,
            head,
            evidence: &bounded_evidence,
        })
        .map_err(GraphError::Run)?;
    if let Some(request_id) = request_id {
        let park = checkpoint.nodes[node_index]
            .delivery_park
            .as_mut()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node_id.clone(),
                invariant: "delivery park intent must remain persisted until receipt",
            })?;
        park.request_id = Some(request_id);
        checkpoint.nodes[node_index].state = NodeState::Blocked;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(())
}

pub(super) fn advance_hygiene<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node = plan
        .nodes
        .get(node_index)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "hygiene node index must exist",
        })?;
    if let Some(park) = checkpoint.nodes[node_index].delivery_park.clone() {
        return park_delivery_transition(
            document,
            node_index,
            park.reason,
            park.attempt_key.as_ref(),
            park.attempt_ref.as_deref(),
            &park.handoff,
            park.head.as_deref(),
            &park.evidence,
            owner,
            journal,
            checkpoint,
        );
    }
    let source_id = match &node.source {
        Some(SourceBinding::Node { node }) => node,
        _ => {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene requires its validated direct writer source",
            });
        }
    };
    let source_index = *positions
        .get(source_id)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene writer source must exist",
        })?;
    let source_progress = &checkpoint.nodes[source_index];
    let source_attempt = source_progress
        .recorded_attempts()
        .rfind(|attempt| attempt.state == AttemptState::Terminal)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene requires a terminal writer attempt",
        })?;
    let input_head = source_attempt
        .input_manifest
        .as_ref()
        .and_then(|manifest| manifest.source_commit.as_deref())
        .or(source_progress.source_commit.as_deref())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene writer attempt must retain its input head",
        })?;
    let source_outputs =
        source_progress
            .outputs
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene writer must retain its outputs",
            })?;
    let expected_head = source_outputs
        .commit
        .as_deref()
        .or(source_progress.source_commit.as_deref())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene writer must retain its output head",
        })?;
    let writer_binding =
        source_progress
            .writer_binding
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene writer must retain its verified worktree binding",
            })?;
    let worktree = writer_binding.worktree.clone();
    let worktree_identity = writer_binding.source_directory_identity.clone();
    let source_key = source_attempt.key.clone();
    let source_attempt_ref =
        source_attempt
            .attempt_ref
            .clone()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene requires the accepted writer attempt reference",
            })?;
    let input_head = input_head.to_owned();
    let expected_head = expected_head.to_owned();
    let source_branch = source_outputs.branch.clone();
    let handoff = source_progress
        .handoff
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene handoff must be serializable",
        })?;

    let operation_id = delivery_operation(
        "delivery-hygiene",
        "hygiene",
        &serde_json::json!({
            "graph": document.graph_id,
            "node": node.id,
            "source_attempt": source_key,
            "input_head": input_head,
            "expected_head": expected_head,
        }),
    )
    .map_err(GraphError::Canonical)?;
    if checkpoint.nodes[node_index].delivery_hygiene.is_none() {
        checkpoint.nodes[node_index].delivery_hygiene = Some(DeliveryHygieneProgress {
            operation_id: operation_id.clone(),
            input_head: input_head.clone(),
            handoff: handoff.clone(),
            outcome: None,
        });
        checkpoint.nodes[node_index].state = NodeState::Running;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }

    let intent = checkpoint.nodes[node_index]
        .delivery_hygiene
        .as_ref()
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene intent must be saved before execution",
        })?;
    if intent.operation_id != operation_id
        || intent.input_head != input_head
        || intent.handoff != handoff
    {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene replay must retain its original operation, writer input and handoff",
        });
    }
    let intent_handoff = intent.handoff.clone();
    let intent_input_head = intent.input_head.clone();
    if let Some(outcome) = intent.outcome.clone() {
        if outcome.kind != provenant_records::graph::HygieneOutcomeKind::Error {
            return Ok(());
        }
        let evidence = checkpoint.nodes[node_index]
            .advance_error
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        return park_delivery_transition(
            document,
            node_index,
            provenant_records::graph::ParkReason::OperationError,
            Some(&source_key),
            Some(&source_attempt_ref),
            intent_handoff.as_deref().unwrap_or(""),
            outcome.after_head.as_deref(),
            &evidence,
            owner,
            journal,
            checkpoint,
        );
    }

    let mut execution = owner
        .run_hygiene(HygieneRequest {
            graph_id: &document.graph_id,
            node_id: &node.id,
            operation_id: &operation_id,
            source_node: source_id,
            source_key: &source_key,
            source_attempt: &source_attempt_ref,
            worktree: &worktree,
            worktree_identity: worktree_identity.as_ref(),
            input_head: &intent_input_head,
            expected_head: &expected_head,
            handoff: intent_handoff.as_deref(),
        })
        .map_err(GraphError::Run)?;
    execution.outcome.handoff = intent_handoff.clone();
    if execution.outcome.kind == provenant_records::graph::HygieneOutcomeKind::Clean {
        if execution.outcome.after_head.is_none() {
            execution.outcome.kind = provenant_records::graph::HygieneOutcomeKind::Error;
            execution.diagnostic = Some("hygiene completed without a verified output head".into());
        } else if document.queue.is_some()
            && execution
                .branch
                .as_ref()
                .or(source_branch.as_ref())
                .is_none_or(|branch| {
                    branch.trim().is_empty() || branch.chars().any(char::is_control)
                })
        {
            execution.outcome.kind = provenant_records::graph::HygieneOutcomeKind::Error;
            execution.diagnostic = Some("queue hygiene completed without a verified branch".into());
        }
    }
    let outcome = execution.outcome;
    let formatter_warnings = execution
        .formatter_source
        .iter()
        .map(|source| format!("delivery hygiene formatter source: {source}"))
        .collect();
    {
        let progress = &mut checkpoint.nodes[node_index];
        progress
            .delivery_hygiene
            .as_mut()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene intent must remain persisted during execution",
            })?
            .outcome = Some(outcome.clone());
        progress.outputs = Some(NodeOutputs {
            schema_version: 2,
            hygiene: Some(outcome.clone()),
            branch: execution.branch.or(source_branch),
            commit: outcome.after_head.clone(),
            warnings: formatter_warnings,
            ..Default::default()
        });
    }
    match outcome.kind {
        provenant_records::graph::HygieneOutcomeKind::Clean => {
            let progress = &mut checkpoint.nodes[node_index];
            progress.state = NodeState::Succeeded;
            progress.advance_error = None;
            progress.advance_error_code = None;
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        kind @ (provenant_records::graph::HygieneOutcomeKind::NoCommit
        | provenant_records::graph::HygieneOutcomeKind::UncommittedEdits) => {
            let cause = if kind == provenant_records::graph::HygieneOutcomeKind::NoCommit {
                provenant_records::graph::EscalationCause::NoCommit
            } else {
                provenant_records::graph::EscalationCause::UncommittedEdits
            };
            let (error_code, error_message) =
                if kind == provenant_records::graph::HygieneOutcomeKind::NoCommit {
                    (
                        "hygiene_no_commit",
                        "writer produced no commit beyond its input head",
                    )
                } else {
                    (
                        "hygiene_uncommitted_edits",
                        "writer worktree still contains tracked or untracked edits",
                    )
                };
            {
                let progress = &mut checkpoint.nodes[node_index];
                progress.state = NodeState::Failed;
                progress.advance_error_code = Some(error_code.into());
                progress.advance_error = Some(error_message.into());
            }
            let escalation =
                transitions::advance_delivery_escalation(node_index, plan, checkpoint, cause);
            if matches!(
                escalation,
                Some(transitions::EscalationTransition::Exhausted { .. })
            ) {
                let park_node = loops::escalation_owner(node_index, plan)
                    .map_or(node_index, |(owner_index, _)| owner_index);
                let evidence = vec![error_message.to_owned()];
                park_delivery_transition(
                    document,
                    park_node,
                    provenant_records::graph::ParkReason::LadderExhausted,
                    Some(&source_key),
                    Some(&source_attempt_ref),
                    handoff.as_deref().unwrap_or(""),
                    outcome.after_head.as_deref(),
                    &evidence,
                    owner,
                    journal,
                    checkpoint,
                )?;
            } else {
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
        }
        provenant_records::graph::HygieneOutcomeKind::Error => {
            let evidence = {
                let progress = &mut checkpoint.nodes[node_index];
                progress.state = NodeState::Running;
                progress.advance_error_code = Some("hygiene_operation_error".into());
                progress.advance_error = execution
                    .diagnostic
                    .as_deref()
                    .map(|diagnostic| delivery_text(diagnostic, 1024))
                    .or_else(|| Some("formatter or formatter commit failed".into()));
                progress.advance_error.iter().cloned().collect::<Vec<_>>()
            };
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            park_delivery_transition(
                document,
                node_index,
                provenant_records::graph::ParkReason::OperationError,
                Some(&source_key),
                Some(&source_attempt_ref),
                intent_handoff.as_deref().unwrap_or(""),
                outcome.after_head.as_deref(),
                &evidence,
                owner,
                journal,
                checkpoint,
            )?;
        }
    }
    Ok(())
}

const DELIVERY_CI_WAIT_MAX_SECONDS: u64 = 14_400;
const DELIVERY_CI_RETRY_MICROS: u64 = 1_000_000;

// A resumed publication must compare against the last receipt for this
// branch, including an authenticated adopted PR seed.
pub(super) fn prior_publication_head(
    node: &PlannedNode,
    progress: &NodeProgress,
    branch: &str,
) -> Option<String> {
    if node.kind != Some(NodeKind::Publish) || progress.state != NodeState::Succeeded {
        return None;
    }
    if let Some(intent) = node.delivery_adoption.as_ref() {
        if intent.branch != branch {
            return None;
        }
        let adoption = progress.delivery_adoption()?;
        if !adoption.matches_intent(intent) || adoption.ci_admission.is_none() {
            return None;
        }
        return adoption.receipt.map(|receipt| receipt.head);
    }
    let publication = progress.delivery_publication.as_ref()?;
    if publication.source_branch != branch {
        return None;
    }
    publication
        .receipt
        .as_ref()
        .map(|receipt| receipt.head.clone())
}

pub(super) fn prior_publication_adoption<'a>(
    plan: &'a PlannedGraph,
    checkpoint: &GraphCheckpoint,
    before: usize,
    branch: &str,
) -> Option<&'a DeliveryAdoptionIntent> {
    plan.nodes
        .iter()
        .zip(&checkpoint.nodes)
        .take(before)
        .rev()
        .find_map(|(node, progress)| {
            let intent = node.delivery_adoption.as_ref()?;
            let adoption = progress.delivery_adoption()?;
            (node.kind == Some(NodeKind::Publish)
                && intent.branch == branch
                && progress.state == NodeState::Succeeded
                && adoption.matches_intent(intent)
                && adoption.ci_admission.is_some()
                && adoption.receipt.is_some()
                && prior_publication_head(node, progress, branch).is_some())
            .then_some(intent)
        })
}

pub(super) fn delivery_ci_wait_policy(document: &GraphDocument) -> provenant_records::Sourced<u64> {
    document
        .queue_ci_wait()
        .unwrap_or_else(|| provenant_records::Sourced {
            value: crate::config::GraphConfig::default().launch_wait_seconds,
            source: provenant_records::Source {
                layer: provenant_records::Layer::Default,
                origin: None,
            },
        })
}

pub(super) fn start_delivery_ci_wait<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_id: &str,
    wall_now: Option<provenant_core::WallTime>,
) -> Result<DeliveryCiAdmissionWait, GraphError<R::Error, J::Error>> {
    let policy = delivery_ci_wait_policy(document);
    if !(1..=DELIVERY_CI_WAIT_MAX_SECONDS).contains(&policy.value) {
        return Err(GraphError::BadCheckpoint {
            node: node_id.into(),
            invariant: "queue CI admission wait must be bounded to 1..=14400 seconds",
        });
    }
    let started_at = wall_now.map_or_else(
        || {
            provenant_core::WallTime::from_micros(0).map_err(|_| GraphError::BadCheckpoint {
                node: node_id.into(),
                invariant: "Unix epoch must be representable",
            })
        },
        Ok,
    )?;
    let micros = policy.value * 1_000_000;
    let deadline = started_at
        .checked_add(DurationMicros::from_micros(micros))
        .map_err(|_| GraphError::BadCheckpoint {
            node: node_id.into(),
            invariant: "queue CI admission deadline exceeds representable time",
        })?;
    Ok(DeliveryCiAdmissionWait {
        started_at,
        deadline,
        wake_at: started_at,
        policy,
        clock_missing: wall_now.is_none(),
    })
}

fn delivery_ci_wait_elapsed_millis(
    wait: &DeliveryCiAdmissionWait,
    now: Option<provenant_core::WallTime>,
) -> u64 {
    let Some(now) = now else {
        return 0;
    };
    now.as_micros()
        .saturating_sub(wait.started_at.as_micros())
        .max(0) as u64
        / 1_000
}

pub(super) fn delivery_ci_wait_retry_at(
    wait: &DeliveryCiAdmissionWait,
    now: provenant_core::WallTime,
) -> provenant_core::WallTime {
    let next = now
        .checked_add(DurationMicros::from_micros(DELIVERY_CI_RETRY_MICROS))
        .unwrap_or(now);
    if now < wait.deadline && next > wait.deadline {
        wait.deadline
    } else {
        next
    }
}

pub(super) fn delivery_ci_wait_ready(
    wait: &DeliveryCiAdmissionWait,
    now: Option<provenant_core::WallTime>,
) -> Option<provenant_core::WallTime> {
    now.filter(|now| *now >= wait.wake_at)
}

pub(super) fn delivery_ci_wait_expired(
    wait: &DeliveryCiAdmissionWait,
    now: Option<provenant_core::WallTime>,
) -> bool {
    !wait.clock_missing && now.is_some_and(|now| now >= wait.deadline)
}

pub(super) fn advance_publish<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node = plan
        .nodes
        .get(node_index)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "publish node index must exist",
        })?;
    if node.delivery_adoption.is_some() {
        return advance_adopted_publish(document, node_index, plan, checkpoint, owner, journal);
    }
    if let Some(park) = checkpoint.nodes[node_index].delivery_park.clone() {
        return park_delivery_transition(
            document,
            node_index,
            park.reason,
            park.attempt_key.as_ref(),
            park.attempt_ref.as_deref(),
            &park.handoff,
            park.head.as_deref(),
            &park.evidence,
            owner,
            journal,
            checkpoint,
        );
    }
    let hygiene_id = match &node.source {
        Some(SourceBinding::Node { node }) => node.clone(),
        _ => {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish requires its validated direct hygiene source",
            });
        }
    };
    let hygiene_index = *positions
        .get(&hygiene_id)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish hygiene source must exist",
        })?;
    let hygiene_node = &plan.nodes[hygiene_index];
    if hygiene_node.kind != Some(NodeKind::Hygiene) {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish source must be a hygiene node",
        });
    }
    let hygiene_progress = &checkpoint.nodes[hygiene_index];
    let hygiene_outputs =
        hygiene_progress
            .outputs
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene source must retain outputs",
            })?;
    let hygiene_outcome =
        hygiene_outputs
            .hygiene
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene source must retain its typed outcome",
            })?;
    if hygiene_outcome.kind != provenant_records::graph::HygieneOutcomeKind::Clean {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish requires a clean hygiene outcome",
        });
    }
    let expected_head =
        hygiene_outcome
            .after_head
            .clone()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene outcome must retain after_head",
            })?;
    let writer_id = match &hygiene_node.source {
        Some(SourceBinding::Node { node }) => node.clone(),
        _ => {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene source must retain its direct writer binding",
            });
        }
    };
    let writer_index = *positions
        .get(&writer_id)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish writer source must exist",
        })?;
    let writer_progress = &checkpoint.nodes[writer_index];
    let writer_binding =
        writer_progress
            .writer_binding
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish writer must retain its verified worktree binding",
            })?;
    let writer_worktree = writer_binding.worktree.clone();
    let writer_identity = writer_binding.source_directory_identity.clone();
    let writer_attempt = writer_progress
        .recorded_attempts()
        .rfind(|attempt| attempt.state == AttemptState::Terminal)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish writer must retain a terminal attempt",
        })?;
    let writer_key = writer_attempt.key.clone();
    let writer_attempt_ref =
        writer_attempt
            .attempt_ref
            .clone()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish writer must retain the accepted attempt reference",
            })?;
    let writer_branch = hygiene_outputs
        .branch
        .clone()
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish hygiene output must retain its writer branch",
        })?;
    let source_branch = document
        .integration_branch
        .clone()
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish requires the sealed integration branch",
        })?;
    let integration_base = checkpoint
        .bases
        .and_then(|bases| bases.integration)
        .map(|base| base.to_string())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish requires the integration base pinned at admission",
        })?;
    let handoff = hygiene_outcome.handoff.clone();

    let mut expected_remote_head = None;
    for prior_index in 0..node_index {
        if plan.nodes[prior_index].kind != Some(NodeKind::Publish) {
            continue;
        }
        let prior_state = checkpoint.nodes[prior_index].state;
        if !prior_state.settled() {
            checkpoint.nodes[node_index].wait_reason = Some("after".into());
            checkpoint.nodes[node_index].state = NodeState::Pending;
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            return Ok(());
        }
        if prior_state == NodeState::Failed || prior_state == NodeState::Blocked {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "a later publication cannot bypass a failed or parked publication",
            });
        }
        let prior = &checkpoint.nodes[prior_index];
        let same_branch = plan.nodes[prior_index]
            .delivery_adoption
            .as_ref()
            .is_some_and(|intent| intent.branch == source_branch)
            || prior
                .delivery_publication
                .as_ref()
                .is_some_and(|intent| intent.source_branch == source_branch);
        if prior_state == NodeState::Succeeded && same_branch {
            if prior.delivery_park.is_some() {
                checkpoint.nodes[node_index].wait_reason = Some("after".into());
                checkpoint.nodes[node_index].state = NodeState::Pending;
                journal.save(checkpoint).map_err(GraphError::Journal)?;
                return Ok(());
            }
            expected_remote_head = Some(
                prior_publication_head(&plan.nodes[prior_index], prior, &source_branch)
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "prior graph publication must retain its verified receipt",
                    })?,
            );
        }
    }

    let adoption = prior_publication_adoption(plan, checkpoint, node_index, &source_branch);
    if adoption.is_some_and(|intent| {
        intent.branch != writer_branch
            || expected_remote_head.is_none()
            || intent.head != integration_base
    }) {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "adopted publication must retain its authenticated PR branch, base head and expected remote head",
        });
    }

    // Fresh publications use a graph-owned remote ref. An adopted publication
    // is separately bound below to its authenticated PR branch and URL.
    let publication_identity = document.graph_id.as_str();
    let intent_value = serde_json::json!({
        "graph": document.graph_id,
        "node": node.id,
        "publication_identity": publication_identity,
        "source_node": writer_id,
        "source_key": writer_key,
        "source_attempt": writer_attempt_ref,
        "writer_branch": writer_branch,
        "source_branch": source_branch,
        "adoption": adoption.map(|intent| serde_json::json!({
            "operation_id": intent.operation_id,
            "pull_request": intent.pull_request,
            "pull_request_url": intent.pull_request_url,
            "branch": intent.branch,
            "repository": intent.repository,
            "base_repository": intent.base_repository,
            "base_branch": intent.base_branch,
            "head": intent.head,
        })),
        "head": expected_head,
        "integration_base": integration_base,
        "expected_remote_head": expected_remote_head,
    });
    let operation_id = delivery_operation("delivery-publication", "publish", &intent_value)
        .map_err(GraphError::Canonical)?;
    if checkpoint.nodes[node_index].delivery_publication.is_none() {
        checkpoint.nodes[node_index].delivery_publication = Some(DeliveryPublicationProgress {
            operation_id: operation_id.clone(),
            source_node: writer_id.clone(),
            source_key: writer_key.clone(),
            source_attempt: writer_attempt_ref.clone(),
            writer_branch: writer_branch.clone(),
            input_head: expected_head.clone(),
            source_branch: source_branch.clone(),
            integration_base: integration_base.clone(),
            expected_remote_head: expected_remote_head.clone(),
            ci_admission: None,
            ci_admission_wait: None,
            receipt: None,
            error: None,
        });
        checkpoint.nodes[node_index].state = NodeState::Running;
        checkpoint.nodes[node_index].wait_reason = None;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let intent = checkpoint.nodes[node_index]
        .delivery_publication
        .as_ref()
        .cloned()
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publication intent must be saved before execution",
        })?;
    if intent.operation_id != operation_id
        || intent.source_node != writer_id
        || intent.source_key != writer_key
        || intent.source_attempt != writer_attempt_ref
        || intent.writer_branch != writer_branch
        || intent.input_head != expected_head
        || intent.source_branch != source_branch
        || intent.integration_base != integration_base
        || intent.expected_remote_head != expected_remote_head
    {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publication replay must retain its original source and remote lease",
        });
    }
    if let Some(queue) = plan.queue.as_ref()
        && checkpoint.nodes[node_index]
            .delivery_publication
            .as_ref()
            .is_some_and(|progress| progress.ci_admission.is_none())
    {
        let mut wait = checkpoint.nodes[node_index]
            .delivery_publication
            .as_ref()
            .and_then(|progress| progress.ci_admission_wait.clone());
        if wait.is_none() {
            wait = Some(start_delivery_ci_wait::<R, J>(
                document,
                &node.id,
                owner.wall_now(),
            )?);
            checkpoint.nodes[node_index]
                .delivery_publication
                .as_mut()
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "publication intent must remain persisted during CI admission",
                })?
                .ci_admission_wait = wait.clone();
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        let mut wait = wait.ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "CI admission wait must be persisted before its attempt",
        })?;
        let wall_now = owner.wall_now();
        if wait.clock_missing && wall_now.is_some() {
            wait = start_delivery_ci_wait::<R, J>(document, &node.id, wall_now)?;
            checkpoint.nodes[node_index]
                .delivery_publication
                .as_mut()
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "publication intent must remain persisted during CI admission",
                })?
                .ci_admission_wait = Some(wait.clone());
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        let Some(now) = delivery_ci_wait_ready(&wait, wall_now) else {
            return Ok(());
        };
        let allow_overflow = delivery_ci_wait_expired(&wait, wall_now);
        let waited_millis = delivery_ci_wait_elapsed_millis(&wait, wall_now);
        match owner
            .admit_delivery_ci(DeliveryCiRequest {
                graph_id: &document.graph_id,
                node_id: &node.id,
                head_sha: &expected_head,
                ci_slots: &queue.ci_slots,
                wait_policy: &wait.policy,
                allow_overflow,
                waited_millis,
            })
            .map_err(GraphError::Run)?
        {
            Some(admission) => {
                checkpoint.nodes[node_index]
                    .delivery_publication
                    .as_mut()
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "publication intent must remain persisted during CI admission",
                    })?
                    .ci_admission = Some(admission);
                asks::refresh_wake(checkpoint);
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
            None => {
                if wall_now.is_some() {
                    wait.wake_at = delivery_ci_wait_retry_at(&wait, now);
                }
                checkpoint.nodes[node_index]
                    .delivery_publication
                    .as_mut()
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "publication intent must remain persisted during CI admission",
                    })?
                    .ci_admission_wait = Some(wait);
                asks::refresh_wake(checkpoint);
                journal.save(checkpoint).map_err(GraphError::Journal)?;
                return Ok(());
            }
        }
    }
    if let Some(receipt) = intent.receipt.clone() {
        let outputs = checkpoint.nodes[node_index]
            .outputs
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "published node receipt must retain matching outputs",
            })?;
        if receipt.head != expected_head
            || outputs.publication.as_ref() != Some(&receipt)
            || outputs.branch.as_deref() != Some(&receipt.branch)
            || outputs.commit.as_deref() != Some(&receipt.head)
        {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publication receipt and outputs must match the sealed head",
            });
        }
        return Ok(());
    }
    let operation_id = intent.operation_id.clone();
    let expected_remote_head = intent.expected_remote_head.clone();
    let input_head = intent.input_head.clone();
    let source_branch = intent.source_branch.clone();
    let integration_base = intent.integration_base.clone();
    let writer_branch = intent.writer_branch.clone();
    let handoff = handoff.clone();
    let mut failure = None;
    let receipt = match owner.publish_delivery(PublicationRequest {
        graph_id: &document.graph_id,
        node_id: &node.id,
        operation_id: &operation_id,
        publication_identity,
        source_node: &writer_id,
        source_key: &writer_key,
        source_attempt: &writer_attempt_ref,
        worktree: &writer_worktree,
        worktree_identity: writer_identity.as_ref(),
        writer_branch: &writer_branch,
        source_branch: &source_branch,
        adoption,
        expected_head: &input_head,
        expected_remote_head: expected_remote_head.as_deref(),
        integration_base: &integration_base,
        handoff: handoff.as_deref(),
    }) {
        Ok(Some(receipt))
            if !receipt.branch.trim().is_empty()
                && receipt.branch.len() <= outputs::OUTPUT_FIELD_BYTES
                && adoption.map_or_else(
                    || {
                        receipt
                            .branch
                            .strip_prefix("provenant/graph/")
                            .is_some_and(|suffix| {
                                suffix.len() == 64
                                    && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                    },
                    |intent| {
                        receipt.branch == intent.branch
                            && receipt.pull_request == intent.pull_request_url
                    },
                )
                && !receipt.branch.chars().any(char::is_whitespace)
                && receipt.head == input_head
                && receipt.head.parse::<provenant_core::GitOid>().is_ok()
                && receipt.pull_request.starts_with("https://")
                && receipt.pull_request.len() <= outputs::OUTPUT_FIELD_BYTES
                && !receipt.pull_request.chars().any(char::is_whitespace)
                && !receipt.pull_request.chars().any(char::is_control)
                && receipt.pull_request.lines().count() == 1 =>
        {
            Some(receipt)
        }
        Ok(Some(_)) => {
            failure = Some("host publication receipt did not match the sealed head".to_owned());
            None
        }
        Ok(None) => {
            failure = Some("host does not provide delivery publication".to_owned());
            None
        }
        Err(error) => {
            failure = Some(delivery_text(&error.to_string(), 1024));
            None
        }
    };
    if let Some(receipt) = receipt {
        let progress = &mut checkpoint.nodes[node_index];
        let intent =
            progress
                .delivery_publication
                .as_mut()
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "publication intent must remain persisted during execution",
                })?;
        intent.receipt = Some(receipt.clone());
        intent.error = None;
        progress.outputs = Some(NodeOutputs {
            schema_version: 2,
            publication: Some(receipt.clone()),
            branch: Some(receipt.branch),
            commit: Some(receipt.head),
            ..Default::default()
        });
        progress.state = NodeState::Succeeded;
        progress.advance_error = None;
        progress.advance_error_code = None;
        progress.wait_reason = None;
        if checkpoint.delivered
            && checkpoint.pull_request.as_deref() != Some(receipt.pull_request.as_str())
        {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "queue publication must retain one stable graph PR identity",
            });
        }
        transitions::record_delivery(checkpoint, receipt.pull_request.clone())
            .map_err(GraphError::Refused)?;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        return Ok(());
    }

    let failure = failure.unwrap_or_else(|| "publication failed without a diagnostic".into());
    let evidence = vec![failure.clone()];
    if let Some(intent) = checkpoint.nodes[node_index].delivery_publication.as_mut() {
        intent.error = Some(failure.clone());
    }
    checkpoint.nodes[node_index].state = NodeState::Running;
    checkpoint.nodes[node_index].advance_error_code = Some("publication_operation_error".into());
    checkpoint.nodes[node_index].advance_error = Some(failure);
    journal.save(checkpoint).map_err(GraphError::Journal)?;
    park_delivery_transition(
        document,
        node_index,
        provenant_records::graph::ParkReason::OperationError,
        Some(&writer_key),
        Some(&writer_attempt_ref),
        handoff.as_deref().unwrap_or(""),
        Some(&input_head),
        &evidence,
        owner,
        journal,
        checkpoint,
    )
}

fn advance_adopted_publish<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node = plan
        .nodes
        .get(node_index)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "adopted publication node index must exist",
        })?;
    let intent =
        node.delivery_adoption
            .as_ref()
            .cloned()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "adopted publication must retain its untrusted intent",
            })?;
    if document.queue.is_none()
        || node.kind != Some(NodeKind::Publish)
        || node.source.is_some()
        || node.worktree.as_deref() != Some(intent.worktree.as_str())
        || !intent.valid()
    {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "adoption intent must be a valid queue-only publication seed",
        });
    }
    if let Some(park) = checkpoint.nodes[node_index].delivery_park.clone() {
        return park_delivery_transition(
            document,
            node_index,
            park.reason,
            park.attempt_key.as_ref(),
            park.attempt_ref.as_deref(),
            &park.handoff,
            park.head.as_deref(),
            &park.evidence,
            owner,
            journal,
            checkpoint,
        );
    }
    if checkpoint.nodes[node_index].delivery_adoption().is_none() {
        checkpoint.nodes[node_index].set_delivery_adoption(&DeliveryAdoptionProgress {
            operation_id: intent.operation_id.clone(),
            ci_admission: None,
            ci_admission_wait: None,
            receipt: None,
            error: None,
        });
        checkpoint.nodes[node_index].state = NodeState::Running;
        checkpoint.nodes[node_index].wait_reason = None;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let mut adoption = checkpoint.nodes[node_index]
        .delivery_adoption()
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "adoption intent must be saved before host verification",
        })?;
    if !adoption.matches_intent(&intent)
        || adoption.ci_admission.is_some() && adoption.receipt.is_none()
    {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "adoption replay must retain its original operation and receipt",
        });
    }
    let mut diagnostic = None;
    let receipt = match adoption.receipt.clone() {
        Some(receipt) => Some(receipt),
        None => match owner.seed_delivery_adoption(DeliveryAdoptionRequest {
            graph_id: &document.graph_id,
            node_id: &node.id,
            intent: &intent,
        }) {
            Ok(Some(receipt)) if intent.receipt_matches(&receipt) => Some(receipt),
            Ok(Some(_)) => {
                diagnostic =
                    Some("host adoption receipt did not match the requested PR head".into());
                None
            }
            Ok(None) => {
                diagnostic = Some("host does not provide queue pull-request adoption".into());
                None
            }
            Err(error) => {
                diagnostic = Some(delivery_text(&error.to_string(), 1024));
                None
            }
        },
    };
    let Some(receipt) = receipt else {
        let evidence =
            vec![diagnostic.unwrap_or_else(|| "queue pull-request adoption failed".into())];
        adoption.error = Some(evidence[0].clone());
        checkpoint.nodes[node_index].set_delivery_adoption(&adoption);
        checkpoint.nodes[node_index].state = NodeState::Running;
        checkpoint.nodes[node_index].advance_error_code =
            Some("publication_adoption_unavailable".into());
        checkpoint.nodes[node_index].advance_error = Some(evidence[0].clone());
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        return park_delivery_transition(
            document,
            node_index,
            provenant_records::graph::ParkReason::OperationError,
            None,
            None,
            "The host could not verify and adopt the existing pull request for this queue item.",
            Some(&intent.head),
            &evidence,
            owner,
            journal,
            checkpoint,
        );
    };
    adoption.receipt = Some(receipt.clone());
    adoption.error = None;
    if adoption.ci_admission.is_none() {
        let queue = plan
            .queue
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "adopted publication requires sealed queue CI admission",
            })?;
        let mut wait = adoption.ci_admission_wait.clone();
        if wait.is_none() {
            wait = Some(start_delivery_ci_wait::<R, J>(
                document,
                &node.id,
                owner.wall_now(),
            )?);
            adoption.ci_admission_wait = wait.clone();
            checkpoint.nodes[node_index].set_delivery_adoption(&adoption);
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        let mut wait = wait.ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "adoption CI wait must be persisted before its attempt",
        })?;
        let wall_now = owner.wall_now();
        if wait.clock_missing && wall_now.is_some() {
            wait = start_delivery_ci_wait::<R, J>(document, &node.id, wall_now)?;
            adoption.ci_admission_wait = Some(wait.clone());
            checkpoint.nodes[node_index].set_delivery_adoption(&adoption);
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        adoption.ci_admission_wait = Some(wait.clone());
        let Some(now) = delivery_ci_wait_ready(&wait, wall_now) else {
            return Ok(());
        };
        let allow_overflow = delivery_ci_wait_expired(&wait, wall_now);
        let waited_millis = delivery_ci_wait_elapsed_millis(&wait, wall_now);
        let admission = owner
            .admit_delivery_ci(DeliveryCiRequest {
                graph_id: &document.graph_id,
                node_id: &node.id,
                head_sha: &receipt.head,
                ci_slots: &queue.ci_slots,
                wait_policy: &wait.policy,
                allow_overflow,
                waited_millis,
            })
            .map_err(GraphError::Run)?;
        let Some(admission) = admission else {
            if wall_now.is_some() {
                wait.wake_at = delivery_ci_wait_retry_at(&wait, now);
            }
            adoption.ci_admission_wait = Some(wait);
            checkpoint.nodes[node_index].set_delivery_adoption(&adoption);
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            return Ok(());
        };
        adoption.ci_admission = Some(admission);
    }
    let progress = &mut checkpoint.nodes[node_index];
    progress.set_delivery_adoption(&adoption);
    progress.outputs = Some(NodeOutputs {
        schema_version: 2,
        publication: Some(receipt.clone()),
        branch: Some(receipt.branch.clone()),
        commit: Some(receipt.head.clone()),
        ..Default::default()
    });
    progress.state = NodeState::Succeeded;
    progress.advance_error_code = None;
    progress.advance_error = None;
    progress.wait_reason = None;
    transitions::record_delivery(checkpoint, receipt.pull_request.clone())
        .map_err(GraphError::Refused)?;
    journal.save(checkpoint).map_err(GraphError::Journal)
}
