//! Revisioned compositions for controls on durable delivery queue items.
use super::{BoundFacade, FacadeError, Factory};
use crate::fabric::owner::graph_journal_error;
use provenant_core::{CancelToken, Clock, HumanAnswerId, HumanRequestId, OperationId, WaitBudget};
pub(super) use provenant_fabric_runtime::graph::queue_control::{Action, Metadata};
use provenant_fabric_runtime::graph::queue_control::{Intent, Phase, QueueControlLedger as Ledger};
use provenant_fabric_runtime::{
    graph::{
        AttemptState, DeliveryReviewDisposition, FileJournal, GraphJournal, LiveGraph, NodeState,
        PendingGraphSteer, patch,
    },
    inbox::{GraphSteerContext, InboxFilter, InboxInput, InboxResult, QueueControlReceipt},
};
use provenant_records::{
    CanonicalV1,
    graph::ParkReason,
    inbox::{AnswerAction, InboxItem, RequestState},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptGateRetry {
    operation_id: OperationId,
    gate: String,
    baseline_generation: u64,
}

fn refusal(reason: impl std::fmt::Display) -> FacadeError {
    FacadeError::invalid(
        provenant_fabric_runtime::coordination::facade::InvalidInputDetail::graph_reason(reason),
    )
}

pub(super) fn decode_metadata(value: &Value) -> Result<Metadata, FacadeError> {
    serde_json::from_value(value.clone()).map_err(|error| {
        FacadeError::invalid(
            provenant_fabric_runtime::coordination::facade::InvalidInputDetail::graph_reason(
                format!("queue control metadata: {error}"),
            ),
        )
    })
}

fn ledger(checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint) -> Ledger {
    checkpoint
        .delivery_queue_controls
        .clone()
        .unwrap_or_default()
}

fn ledger_has_drop_fence(ledger: &Ledger) -> bool {
    ledger
        .intents
        .values()
        .any(|intent| intent.metadata.action == Action::Drop && intent.phase != Phase::Refused)
}

pub(super) fn drop_fence_active(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Result<bool, FacadeError> {
    Ok(ledger_has_drop_fence(&ledger(checkpoint)))
}

fn save_ledger(checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint, ledger: &Ledger) {
    checkpoint.delivery_queue_controls = Some(ledger.clone());
}

pub(super) fn control_digest(
    metadata: &Metadata,
    graph_id: &str,
    target_node: Option<&str>,
    request_id: Option<HumanRequestId>,
    payload: &Value,
) -> Result<String, FacadeError> {
    CanonicalV1::record_digest(
        "delivery-queue-control",
        1,
        &(metadata, graph_id, target_node, request_id, payload),
    )
    .map(|digest| digest.to_string())
    .map_err(|_| FacadeError::StateCorrupt)
}

fn competes_with(ledger: &Ledger, metadata: &Metadata) -> bool {
    ledger.intents.values().any(|saved| {
        saved.metadata.expected_revision == metadata.expected_revision || !saved.phase.terminal()
    })
}

fn reserve_checkpoint(
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    document: &provenant_fabric_runtime::graph::GraphDocument,
    graph_id: &str,
    target_node: Option<&str>,
    request_id: Option<HumanRequestId>,
    metadata: &Metadata,
    payload: &Value,
) -> Result<(Intent, bool), FacadeError> {
    if metadata
        .graph_id
        .as_deref()
        .is_some_and(|expected| expected != graph_id)
    {
        return Err(refusal(
            "queue control graph identity does not match its target",
        ));
    }
    if document.queue.is_none() {
        return Err(refusal(
            "queue control requires a persisted delivery queue graph",
        ));
    }
    let mut ledger = ledger(checkpoint);
    let key = metadata.operation_id.to_string();
    let digest = control_digest(metadata, graph_id, target_node, request_id, payload)?;
    if let Some(saved) = ledger.intents.get(&key) {
        if saved.digest != digest {
            return Err(FacadeError::OperationConflict);
        }
        return Ok((saved.clone(), true));
    }
    if ledger_has_drop_fence(&ledger) {
        return Err(refusal(
            "dropped queue items cannot receive further controls",
        ));
    }
    let current = patch::revision(checkpoint);
    if current != metadata.expected_revision {
        return Err(refusal(format!(
            "queue control expects graph revision {}, current revision is {current}",
            metadata.expected_revision
        )));
    }
    if metadata.action == Action::Fix
        && payload.get("op").and_then(Value::as_str) == Some("retry")
        && target_node.is_some_and(|target| {
            checkpoint
                .nodes
                .iter()
                .find(|progress| progress.id == target)
                .and_then(|progress| progress.recorded_attempts().last())
                .is_some_and(|attempt| attempt.state != AttemptState::Terminal)
        })
    {
        return Err(refusal(
            "a live queue fix must use the existing steer operation",
        ));
    }
    if competes_with(&ledger, metadata) {
        return Err(refusal(
            "another queue control already owns this graph revision or is pending",
        ));
    }
    let retry_generations = checkpoint
        .nodes
        .iter()
        .filter_map(|progress| {
            progress
                .retry_generation
                .map(|generation| (progress.id.clone(), generation))
        })
        .collect();
    let intent = Intent {
        digest,
        metadata: metadata.clone(),
        graph_id: graph_id.to_owned(),
        target_node: target_node.map(str::to_owned),
        retry_generations,
        gate_generations: BTreeMap::new(),
        request_id,
        payload: payload.clone(),
        phase: Phase::Pending,
        answer_id: None,
        applied_revision: None,
    };
    ledger.intents.insert(key, intent.clone());
    save_ledger(checkpoint, &ledger);
    Ok((intent, false))
}

pub(super) fn reserve_retry_control(
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    document: &provenant_fabric_runtime::graph::GraphDocument,
    graph_id: &str,
    node_id: &str,
    metadata: &Metadata,
    payload: &Value,
) -> Result<(bool, bool), FacadeError> {
    if !matches!(metadata.action, Action::Retry | Action::Fix) {
        return Err(refusal(
            "retry admission requires a retry or fix queue control",
        ));
    }
    let (mut intent, replayed) = reserve_checkpoint(
        checkpoint,
        document,
        graph_id,
        Some(node_id),
        None,
        metadata,
        payload,
    )?;
    if !intent.phase.terminal() {
        let operation_id = metadata.operation_id.to_string();
        let has_receipt = checkpoint
            .expansions
            .iter()
            .filter_map(|expansion| expansion.patch.as_ref())
            .any(|record| record.key.as_deref() == Some(operation_id.as_str()));
        if !has_receipt && patch::revision(checkpoint) != metadata.expected_revision {
            intent.phase = Phase::Refused;
            let mut ledger = ledger(checkpoint);
            ledger.intents.insert(operation_id, intent.clone());
            save_ledger(checkpoint, &ledger);
        }
    }
    Ok((intent.phase.terminal(), replayed))
}

fn drop_transition_admitted(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    target_node: Option<&str>,
) -> bool {
    match target_node {
        None => checkpoint.cancelled,
        Some(node) => checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == node)
            .is_some_and(|progress| {
                progress.subtree_cancel.as_deref() == Some(node)
                    || progress.cancelled_from.is_some()
                    || progress.skip_reason.as_deref().is_some_and(|reason| {
                        reason.starts_with(&format!("parent {node} cancelled:"))
                    })
            }),
    }
}

fn reserve_drop_control(
    factory: &Factory,
    graph_id: &str,
    target_node: Option<&str>,
    metadata: &Metadata,
    payload: &Value,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(Intent, bool), FacadeError> {
    if factory.context.in_lane || factory.context.inspection_only {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
    let project = factory.context.identity.project;
    let execution = FileJournal::execution(&factory.context.roots.state, project, graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let mut checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let (mut intent, replayed) = reserve_checkpoint(
        &mut checkpoint,
        &execution.document,
        graph_id,
        target_node,
        None,
        metadata,
        payload,
    )?;
    if intent.phase.terminal() {
        return Ok((intent, replayed));
    }

    if intent.phase != Phase::DropAdmitted
        && !drop_transition_admitted(&checkpoint, target_node)
        && replayed
        && patch::revision(&checkpoint) != metadata.expected_revision
    {
        intent.phase = Phase::Refused;
        let mut ledger = ledger(&checkpoint);
        ledger
            .intents
            .insert(metadata.operation_id.to_string(), intent.clone());
        save_ledger(&mut checkpoint, &ledger);
        journal.save(&checkpoint).map_err(graph_journal_error)?;
        return Ok((intent, replayed));
    }

    if intent.phase != Phase::DropAdmitted && !drop_transition_admitted(&checkpoint, target_node) {
        let reason = payload
            .get("reason")
            .and_then(Value::as_str)
            .map(|reason| provenant_fabric_runtime::hosts::plain_text(reason, 1024));
        let reason = reason.as_deref();
        if let Some(node) = target_node {
            provenant_fabric_runtime::graph::cancel_node_subtree(
                &execution.document,
                &mut checkpoint,
                node,
                reason,
            )
            .map_err(refusal)?;
        } else {
            checkpoint.cancelled = true;
            checkpoint.cancel_reason = reason.map(str::to_owned);
            provenant_fabric_runtime::graph::mark_cancelled_nodes_without_live_attempts(
                &mut checkpoint,
            );
        }
        intent.phase = Phase::DropAdmitted;
        let mut ledger = ledger(&checkpoint);
        ledger
            .intents
            .insert(metadata.operation_id.to_string(), intent.clone());
        save_ledger(&mut checkpoint, &ledger);
    } else if intent.phase != Phase::DropAdmitted {
        intent.phase = Phase::DropAdmitted;
        let mut ledger = ledger(&checkpoint);
        ledger
            .intents
            .insert(metadata.operation_id.to_string(), intent.clone());
        save_ledger(&mut checkpoint, &ledger);
    }
    journal.save(&checkpoint).map_err(graph_journal_error)?;
    let intent = ledger(&checkpoint)
        .intents
        .get(&metadata.operation_id.to_string())
        .cloned()
        .ok_or(FacadeError::StateCorrupt)?;
    Ok((intent, replayed))
}

fn find_intent(
    factory: &Factory,
    graph_id: &str,
    target_node: Option<&str>,
    metadata: &Metadata,
    payload: &Value,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Option<Intent>, FacadeError> {
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let key = metadata.operation_id.to_string();
    let mut saved_ledger = ledger(&checkpoint);
    let Some(intent) = saved_ledger.intents.remove(&key) else {
        return Ok(None);
    };
    let digest = control_digest(metadata, graph_id, target_node, None, payload)?;
    if intent.digest != digest {
        return Err(FacadeError::OperationConflict);
    }
    Ok(Some(intent))
}

fn request_id_for_drop(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    target_node: Option<&str>,
) -> Result<Option<HumanRequestId>, FacadeError> {
    let mut requests = checkpoint
        .nodes
        .iter()
        .filter(|progress| target_node.is_none_or(|node| progress.id == node))
        .filter_map(|progress| progress.delivery_park.as_ref()?.request_id);
    let request_id = requests.next();
    if requests.next().is_some() {
        return Err(refusal("drop requires one unambiguous owned park request"));
    }
    Ok(request_id)
}

fn begin(
    factory: &Factory,
    graph_id: &str,
    (target_node, request_id): (Option<&str>, Option<HumanRequestId>),
    metadata: &Metadata,
    payload: &Value,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(Intent, bool), FacadeError> {
    if factory.context.in_lane || factory.context.inspection_only {
        return Err(FacadeError::ConfinementRefused { resource: None });
    }
    let project = factory.context.identity.project;
    if metadata
        .graph_id
        .as_deref()
        .is_some_and(|expected| expected != graph_id)
    {
        return Err(refusal(
            "queue control graph identity does not match its target",
        ));
    }
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let execution = FileJournal::execution(&factory.context.roots.state, project, graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    if execution.document.queue.is_none() {
        return Err(refusal(
            "queue control requires a persisted delivery queue graph",
        ));
    }
    let mut checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let mut ledger = ledger(&checkpoint);
    let key = metadata.operation_id.to_string();
    let digest = control_digest(metadata, graph_id, target_node, request_id, payload)?;
    if let Some(saved) = ledger.intents.get(&key) {
        if saved.digest != digest {
            return Err(FacadeError::OperationConflict);
        }
        return Ok((saved.clone(), true));
    }
    if ledger_has_drop_fence(&ledger) {
        return Err(refusal(
            "dropped queue items cannot receive further controls",
        ));
    }
    let current = patch::revision(&checkpoint);
    if current != metadata.expected_revision {
        return Err(refusal(format!(
            "queue control expects graph revision {}, current revision is {current}",
            metadata.expected_revision
        )));
    }
    if metadata.action == Action::Fix
        && payload.get("op").and_then(Value::as_str) == Some("retry")
        && target_node.is_some_and(|target| {
            checkpoint
                .nodes
                .iter()
                .find(|progress| progress.id == target)
                .and_then(|progress| progress.recorded_attempts().last())
                .is_some_and(|attempt| attempt.state != AttemptState::Terminal)
        })
    {
        return Err(refusal(
            "a live queue fix must use the existing steer operation",
        ));
    }
    if metadata.action == Action::Accept {
        let request_id = request_id.ok_or_else(|| refusal("accept requires a park request ID"))?;
        let node_id = metadata
            .review_node
            .as_deref()
            .ok_or_else(|| refusal("accept requires an exact review node"))?;
        let head = metadata
            .review_head
            .ok_or_else(|| refusal("accept requires an exact reviewed head"))?;
        let Some(park) = checkpoint.nodes.iter().find_map(|progress| {
            progress
                .delivery_park
                .as_ref()
                .filter(|park| park.request_id == Some(request_id))
        }) else {
            return Err(refusal("accept request is not owned by this graph"));
        };
        let expected_head = head.to_string();
        if park.reason != ParkReason::LadderExhausted
            || park.head.as_deref() != Some(expected_head.as_str())
        {
            return Err(refusal(
                "accept request does not match the current exhausted review head",
            ));
        }
        // Validate the disposition before answering the inbox request. The
        // placeholder answer ID is never saved; it proves only the exact-head,
        // findings and gate predicates under the same journal lock.
        let probe_disposition = DeliveryReviewDisposition {
            operation_id: metadata.operation_id.clone(),
            request_id,
            answer_id: HumanAnswerId::from_bytes([0; 16]),
            head,
            reason: payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        let mut probe = checkpoint.clone();
        patch::admit_delivery_review_disposition(
            &mut LiveGraph {
                document: &execution.document,
                checkpoint: &mut probe,
            },
            metadata.expected_revision,
            node_id,
            &probe_disposition,
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
            execution.requester_path.as_deref(),
        )
        .map_err(|error| refusal(error.to_string()))?;
    }
    if competes_with(&ledger, metadata) {
        return Err(refusal(
            "another queue control already owns this graph revision or is pending",
        ));
    }
    let retry_generations = checkpoint
        .nodes
        .iter()
        .filter_map(|progress| {
            progress
                .retry_generation
                .map(|generation| (progress.id.clone(), generation))
        })
        .collect();
    let request_id = if metadata.action == Action::Drop {
        request_id.or(request_id_for_drop(&checkpoint, target_node)?)
    } else {
        request_id
    };
    let intent = Intent {
        digest,
        metadata: metadata.clone(),
        graph_id: graph_id.to_owned(),
        target_node: target_node.map(str::to_owned),
        retry_generations,
        gate_generations: BTreeMap::new(),
        request_id,
        payload: payload.clone(),
        phase: Phase::Pending,
        answer_id: None,
        applied_revision: None,
    };
    ledger.intents.insert(key, intent.clone());
    save_ledger(&mut checkpoint, &ledger);
    journal.save(&checkpoint).map_err(graph_journal_error)?;
    Ok((intent, false))
}

/// Compose `steer` into the durable pending-steer queue for a live queue item.
/// The control intent and the graph-owned steer receipt are saved together;
/// the graph tick revalidates the exact work key before delivery.
pub(super) fn queue_live_fix(
    factory: &Factory,
    graph_id: &str,
    node_id: &str,
    metadata: &Metadata,
    (text, work_key): (&str, &provenant_records::WorkKey),
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Value, FacadeError> {
    if metadata.action != Action::Fix {
        return Err(refusal("live queue steering requires a fix control"));
    }
    let text = provenant_fabric_runtime::graph::normalise_prompt_text(text).map_err(refusal)?;
    patch::scan_for_secrets(&text).map_err(refusal)?;
    let payload = json!({"kind":"steer", "text":text, "work_key":work_key});
    let project = factory.context.identity.project;
    let execution = FileJournal::execution(&factory.context.roots.state, project, graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let mut checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let (intent, replayed) = reserve_checkpoint(
        &mut checkpoint,
        &execution.document,
        graph_id,
        Some(node_id),
        None,
        metadata,
        &payload,
    )?;
    if intent.phase.terminal() {
        return Ok(control_receipt(&intent, replayed));
    }
    let reason = if patch::revision(&checkpoint) != metadata.expected_revision {
        Some("queue fix graph revision changed before the steer was queued")
    } else if execution.document.queue.is_none() {
        Some("queue fix requires a persisted delivery queue graph")
    } else {
        None
    };
    let Some(progress) = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == node_id)
    else {
        return Err(refusal("queue fix target node no longer exists"));
    };
    let attempts: Vec<_> = progress.recorded_attempts().collect();
    let Some((index, attempt)) = attempts
        .iter()
        .enumerate()
        .find(|(_, attempt)| attempt.key.work_key(project).ok().as_ref() == Some(work_key))
    else {
        return Err(refusal("queue fix target attempt is no longer recorded"));
    };
    let reason = reason.or_else(|| {
        (attempt.state == AttemptState::Terminal || index + 1 != attempts.len())
            .then_some("queue fix target attempt is no longer the current live attempt")
    });
    if reason.is_some() {
        let mut ledger = ledger(&checkpoint);
        let saved = ledger
            .intents
            .get_mut(&metadata.operation_id.to_string())
            .ok_or(FacadeError::StateCorrupt)?;
        saved.phase = Phase::Refused;
        let receipt = saved.clone();
        save_ledger(&mut checkpoint, &ledger);
        journal.save(&checkpoint).map_err(graph_journal_error)?;
        return Ok(control_receipt(&receipt, replayed));
    }
    let launch_actor = attempt
        .launch_actor
        .parse()
        .map_err(|_| FacadeError::StateCorrupt)?;
    let launch_operation =
        super::graph_execution::graph_operation("graph-attempt-operation", &attempt.key)
            .map_err(|_| FacadeError::StateCorrupt)?;
    let pending = PendingGraphSteer {
        operation_id: metadata.operation_id.clone(),
        caller_operation_id: Some(metadata.operation_id.clone()),
        work_key: work_key.clone(),
        graph_context: GraphSteerContext {
            launch_actor,
            launch_operation,
            origin_work_key: work_key.clone(),
        },
        requester_host: factory.context.identity.inbox.host,
        requester_agent: factory.context.identity.inbox.agent.clone(),
        requester_actor: factory.context.identity.inbox.actor,
        text,
        refusal: None,
    };
    if checkpoint
        .pending_steers
        .enqueue(metadata.operation_id.to_string(), pending)
        .is_err()
    {
        let mut ledger = ledger(&checkpoint);
        let saved = ledger
            .intents
            .get_mut(&metadata.operation_id.to_string())
            .ok_or(FacadeError::StateCorrupt)?;
        saved.phase = Phase::Refused;
        let receipt = saved.clone();
        save_ledger(&mut checkpoint, &ledger);
        journal.save(&checkpoint).map_err(graph_journal_error)?;
        return Ok(control_receipt(&receipt, replayed));
    }
    let mut ledger = ledger(&checkpoint);
    let saved = ledger
        .intents
        .get_mut(&metadata.operation_id.to_string())
        .ok_or(FacadeError::StateCorrupt)?;
    saved.phase = Phase::Applied;
    saved.applied_revision = Some(patch::revision(&checkpoint));
    let receipt = saved.clone();
    save_ledger(&mut checkpoint, &ledger);
    let receipt = control_receipt(&receipt, replayed);
    journal.save(&checkpoint).map_err(graph_journal_error)?;
    Ok(receipt)
}

fn update_intent(
    factory: &Factory,
    graph_id: &str,
    operation_id: &OperationId,
    budget: WaitBudget,
    cancel: &CancelToken,
    update: impl FnOnce(&mut Intent),
) -> Result<Intent, FacadeError> {
    let project = factory.context.identity.project;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let mut checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let mut ledger = ledger(&checkpoint);
    let key = operation_id.to_string();
    let intent = ledger
        .intents
        .get_mut(&key)
        .ok_or(FacadeError::StateCorrupt)?;
    update(intent);
    let result = intent.clone();
    save_ledger(&mut checkpoint, &ledger);
    journal.save(&checkpoint).map_err(graph_journal_error)?;
    Ok(result)
}

fn refuse_intent_locked(
    journal: &mut FileJournal,
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    operation_id: &OperationId,
) -> Result<(), FacadeError> {
    let mut ledger = ledger(checkpoint);
    let Some(intent) = ledger.intents.get_mut(&operation_id.to_string()) else {
        return Err(FacadeError::StateCorrupt);
    };
    intent.phase = Phase::Refused;
    save_ledger(checkpoint, &ledger);
    journal.save(checkpoint).map_err(graph_journal_error)
}

fn complete_retry(
    factory: &Factory,
    graph_id: &str,
    operation_id: &OperationId,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Intent, FacadeError> {
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let mut checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let key = operation_id.to_string();
    let mut ledger = ledger(&checkpoint);
    let saved = ledger
        .intents
        .get_mut(&key)
        .ok_or(FacadeError::StateCorrupt)?;
    let target = saved
        .target_node
        .as_deref()
        .ok_or(FacadeError::StateCorrupt)?;
    let reason = format!(
        "queue control {key} retry node {target} intent {}",
        saved.digest
    );
    if !checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .any(|record| {
            record.key.as_deref() == Some(key.as_str())
                && record.reason.as_deref() == Some(reason.as_str())
        })
    {
        return Err(FacadeError::StateCorrupt);
    }
    saved.phase = Phase::Applied;
    saved.applied_revision = Some(patch::revision(&checkpoint));
    let result = saved.clone();
    save_ledger(&mut checkpoint, &ledger);
    journal.save(&checkpoint).map_err(graph_journal_error)?;
    Ok(result)
}

fn complete_drop(
    factory: &Factory,
    graph_id: &str,
    operation_id: &OperationId,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Intent, FacadeError> {
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let mut checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let key = operation_id.to_string();
    let mut ledger = ledger(&checkpoint);
    let saved = ledger
        .intents
        .get_mut(&key)
        .ok_or(FacadeError::StateCorrupt)?;
    if saved.phase != Phase::DropAdmitted
        && !drop_transition_admitted(&checkpoint, saved.target_node.as_deref())
    {
        return Err(FacadeError::StateCorrupt);
    }
    saved.phase = Phase::Applied;
    saved.applied_revision = Some(patch::revision(&checkpoint));
    let result = saved.clone();
    save_ledger(&mut checkpoint, &ledger);
    journal.save(&checkpoint).map_err(graph_journal_error)?;
    Ok(result)
}

fn refuse_unadmitted_retry(
    factory: &Factory,
    graph_id: &str,
    operation_id: &OperationId,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    let project = factory.context.identity.project;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let mut checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let mut ledger = ledger(&checkpoint);
    let key = operation_id.to_string();
    let Some(intent) = ledger.intents.get_mut(&key) else {
        return Ok(());
    };
    if intent.phase.terminal() {
        return Ok(());
    }
    let reason = format!(
        "queue control {key} retry node {} intent {}",
        intent
            .target_node
            .as_deref()
            .ok_or(FacadeError::StateCorrupt)?,
        intent.digest
    );
    let has_retry_receipt = checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .any(|record| {
            record.key.as_deref() == Some(key.as_str())
                && record.reason.as_deref() == Some(reason.as_str())
        });
    if !has_retry_receipt {
        intent.phase = Phase::Refused;
        save_ledger(&mut checkpoint, &ledger);
        journal.save(&checkpoint).map_err(graph_journal_error)?;
    }
    Ok(())
}

fn typed_control_receipt(intent: &Intent, replayed: bool) -> QueueControlReceipt {
    QueueControlReceipt {
        action: intent.metadata.action,
        operation_id: intent.metadata.operation_id.clone(),
        graph_id: intent.graph_id.clone(),
        target_node: intent.target_node.clone(),
        expected_revision: intent.metadata.expected_revision,
        applied_revision: intent.applied_revision,
        state: intent.phase,
        replayed,
        request_id: intent.request_id,
        answer_id: intent.answer_id,
    }
}

fn control_receipt(intent: &Intent, replayed: bool) -> Value {
    json!(typed_control_receipt(intent, replayed))
}

fn intent_requires_parallel_slot(intent: &Intent) -> bool {
    !intent.phase.terminal() && intent.metadata.action != Action::Drop
}

pub(super) fn checkpoint_has_pending_parallel_control(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Result<bool, FacadeError> {
    Ok(ledger(checkpoint)
        .intents
        .values()
        .any(intent_requires_parallel_slot))
}

pub(super) fn checkpoint_has_pending_control(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Result<bool, FacadeError> {
    Ok(ledger(checkpoint)
        .intents
        .values()
        .any(|intent| !intent.phase.terminal()))
}

/// Validate the internal retry marker emitted only by the durable Accept
/// workflow. The Accept ledger entry and matching exact-head disposition are
/// the authority; the marker itself carries no authority.
pub(super) fn authorise_accept_gate_retry(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    graph_id: &str,
    node_id: &str,
    value: &Value,
) -> Result<(String, String), FacadeError> {
    let marker: AcceptGateRetry = serde_json::from_value(value.clone())
        .map_err(|_| refusal("accept gate retry marker is malformed"))?;
    if marker.gate != node_id || marker.gate.is_empty() {
        return Err(refusal(
            "accept gate retry target does not match its marker",
        ));
    }
    let ledger = ledger(checkpoint);
    let operation_key = marker.operation_id.to_string();
    if ledger_has_drop_fence(&ledger) {
        return Err(refusal("dropped queue items cannot be retried"));
    }
    let intent = ledger
        .intents
        .get(&operation_key)
        .ok_or_else(|| refusal("accept gate retry has no saved Accept operation"))?;
    if intent.metadata.action != Action::Accept
        || !matches!(intent.phase, Phase::DispositionAdmitted | Phase::Applied)
        || intent.graph_id != graph_id
        || intent.metadata.graph_id.as_deref() != Some(graph_id)
        || intent.target_node.as_deref() != intent.metadata.review_node.as_deref()
        || intent.request_id.is_none()
        || intent.answer_id.is_none()
        || intent.metadata.review_head.is_none()
        || intent.applied_revision.is_none()
        || intent.gate_generations.get(node_id).copied() != Some(marker.baseline_generation)
        || ledger
            .intents
            .iter()
            .any(|(key, other)| key != &operation_key && !other.phase.terminal())
    {
        return Err(refusal(
            "accept gate retry does not match its saved operation",
        ));
    }
    let request_id = intent.request_id.ok_or(FacadeError::StateCorrupt)?;
    let answer_id = intent.answer_id.ok_or(FacadeError::StateCorrupt)?;
    let head = intent
        .metadata
        .review_head
        .ok_or(FacadeError::StateCorrupt)?;
    let review_node = intent
        .metadata
        .review_node
        .as_deref()
        .ok_or(FacadeError::StateCorrupt)?;
    let answer: InboxInput =
        serde_json::from_value(intent.payload.clone()).map_err(|_| FacadeError::StateCorrupt)?;
    let InboxInput::Answer {
        id,
        action: AnswerAction::Reply,
        option_id: None,
        text: Some(reason),
        operation_id: Some(answer_operation),
        ..
    } = answer
    else {
        return Err(FacadeError::StateCorrupt);
    };
    if id != request_id || answer_operation != marker.operation_id {
        return Err(FacadeError::StateCorrupt);
    }
    let disposition = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == review_node)
        .and_then(|progress| progress.delivery_review_disposition())
        .ok_or_else(|| refusal("accept gate retry has no saved review disposition"))?;
    if disposition.operation_id != marker.operation_id
        || disposition.request_id != request_id
        || disposition.answer_id != answer_id
        || disposition.head != head
        || disposition.reason != reason
    {
        return Err(refusal(
            "accept gate retry disposition does not match its answer",
        ));
    }
    let digest = CanonicalV1::record_digest(
        "delivery-queue-accept-gate-retry",
        1,
        &(
            graph_id,
            &intent.digest,
            &disposition,
            node_id,
            marker.baseline_generation,
        ),
    )
    .map_err(|_| FacadeError::StateCorrupt)?
    .to_string();
    let digest_suffix = digest
        .strip_prefix("sha256:")
        .ok_or(FacadeError::StateCorrupt)?;
    let retry_key = format!("accept-gate-{digest_suffix}");
    let receipt_reason = format!("queue control {retry_key} retry node {node_id} intent {digest}");
    let has_retry_receipt = checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .any(|record| {
            record.key.as_deref() == Some(retry_key.as_str())
                && record.reason.as_deref() == Some(receipt_reason.as_str())
        });
    let progress = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == node_id)
        .ok_or_else(|| refusal("accept gate retry target is absent"))?;
    let current_generation = progress.retry_generation.unwrap_or_default();
    if !has_retry_receipt
        && (current_generation != marker.baseline_generation
            || !matches!(progress.state, NodeState::Failed | NodeState::Blocked))
    {
        return Err(refusal(
            "accept gate retry no longer matches the failed gate",
        ));
    }
    Ok((retry_key, digest))
}

pub(super) fn reserve_control_capacity(
    factory: &Factory,
    graph_id: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<bool, FacadeError> {
    let project = factory.context.identity.project;
    let execution = FileJournal::execution(&factory.context.roots.state, project, graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    if execution.document.queue.is_none() {
        return Err(refusal("queue control requires a persisted queue owner"));
    }
    let registry =
        super::queue_registry::QueueRegistry::open(factory.context.roots.state.as_path())
            .map_err(|_| FacadeError::OwnerUnavailable)?;
    let queues = registry
        .queues(budget, cancel, &factory.clock)
        .map_err(|_| FacadeError::OwnerUnavailable)?;
    let project = project.to_string();
    let owners: Vec<_> = queues
        .into_iter()
        .filter(|queue| queue.project == project)
        .flat_map(|queue| {
            let queue_id = queue.queue_id;
            queue
                .items
                .into_iter()
                .filter(|item| item.graph_id == graph_id)
                .map(move |item| (queue_id.clone(), item))
        })
        .collect();
    let [(queue_id, item)] = owners.as_slice() else {
        return Err(refusal(
            "queue control graph does not have exactly one durable queue owner",
        ));
    };
    if !super::queue_scheduler::matches_item_execution(queue_id, item, &execution)
        .map_err(|_| FacadeError::StateCorrupt)?
    {
        return Err(refusal(
            "queue control graph does not match its durable queue identity",
        ));
    }
    let now = factory
        .clock
        .wall_now()
        .map_err(|_| FacadeError::OwnerUnavailable)?;
    registry
        .reserve_parked_for_control(
            (queue_id, &item.identity),
            graph_id,
            now,
            budget,
            cancel,
            &factory.clock,
        )
        .map_err(refusal)
}

fn attach_receipt(mut result: Value, intent: &Intent, replayed: bool) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("queue_control".into(), control_receipt(intent, replayed));
    }
    result
}

pub(super) fn handle_graph_operation(
    factory: &Factory,
    input: Value,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Value, FacadeError> {
    let mut metadata = decode_metadata(
        input
            .get("queue_control")
            .ok_or_else(|| refusal("queue control metadata is required"))?,
    )?;
    let operation = input
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| refusal("graph operation is required"))?;
    let valid = matches!(
        (metadata.action, operation),
        (Action::Drop, "stop") | (Action::Retry, "retry") | (Action::Fix, "retry")
    );
    if !valid {
        return Err(refusal(
            "queue control action does not match the existing graph operation",
        ));
    }
    let selector = input
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| refusal("graph target is required"))?;
    let target = factory
        .graph_target(selector)?
        .ok_or(FacadeError::NotFound)?;
    if target.attempt.is_some() {
        return Err(refusal(
            "queue controls require a graph or exact node target",
        ));
    }
    if metadata.action == Action::Drop && target.node.is_some() {
        return Err(refusal("dropping a queue item requires its graph target"));
    }
    if metadata
        .graph_id
        .as_deref()
        .is_some_and(|graph_id| graph_id != target.graph)
    {
        return Err(refusal(
            "queue control graph identity does not match its target",
        ));
    }
    metadata.graph_id = Some(target.graph.clone());
    let mut payload = without_queue_control(&input)?;
    if metadata.action == Action::Fix {
        let raw = payload
            .get("brief")
            .and_then(Value::as_str)
            .filter(|brief| !brief.is_empty())
            .ok_or_else(|| refusal("fix requires a replacement brief"))?;
        let brief = provenant_fabric_runtime::graph::normalise_prompt_text(raw).map_err(refusal)?;
        patch::scan_for_secrets(&brief).map_err(refusal)?;
        payload["brief"] = Value::String(brief);
    } else if metadata.action == Action::Retry
        && payload.get("brief").is_some_and(|brief| !brief.is_null())
    {
        return Err(refusal(
            "retry does not accept replacement text; use the fix control",
        ));
    }
    if let Some(intent) = find_intent(
        factory,
        &target.graph,
        target.node.as_deref(),
        &metadata,
        &payload,
        budget,
        cancel,
    )? && intent.phase.terminal()
    {
        let status =
            factory.graph_document(json!({"op":"status", "id":selector}), budget, cancel)?;
        return Ok(attach_receipt(status, &intent, true));
    }

    match metadata.action {
        Action::Drop => {
            let (intent, replayed) = reserve_drop_control(
                factory,
                &target.graph,
                target.node.as_deref(),
                &metadata,
                &payload,
                budget,
                cancel,
            )?;
            if intent.phase.terminal() {
                let status = factory.graph_document(
                    json!({"op":"status", "id":selector}),
                    budget,
                    cancel,
                )?;
                return Ok(attach_receipt(status, &intent, replayed));
            }
            let operation_result = factory.graph_document(payload, budget, cancel)?;
            close_owned_park(factory, &intent, budget, cancel)?;
            let intent = complete_drop(
                factory,
                &target.graph,
                &metadata.operation_id,
                budget,
                cancel,
            )?;
            Ok(attach_receipt(operation_result, &intent, replayed))
        }
        Action::Retry | Action::Fix => {
            if target.node.is_none() {
                return Err(refusal("queue retry requires one exact node target"));
            }
            let (intent, _) = begin(
                factory,
                &target.graph,
                (target.node.as_deref(), None),
                &metadata,
                &payload,
                budget,
                cancel,
            )?;
            if intent.phase.terminal() {
                return Err(refusal("this queue retry operation was already refused"));
            }
            if !reserve_control_capacity(factory, &target.graph, budget, cancel)? {
                return Err(refusal(
                    "parked queue item is waiting for max_parallel capacity",
                ));
            }
            payload["queue_control"] =
                serde_json::to_value(&metadata).map_err(|_| FacadeError::StateCorrupt)?;
            let operation_result = match factory.retry_document(&payload, budget, cancel) {
                Ok(result) => result,
                Err(error) => {
                    if super::queue_dispatch::deterministic_launch_refusal(&error).is_some() {
                        refuse_unadmitted_retry(
                            factory,
                            &target.graph,
                            &metadata.operation_id,
                            budget,
                            cancel,
                        )?;
                    }
                    return Err(error);
                }
            };
            let intent = complete_retry(
                factory,
                &target.graph,
                &metadata.operation_id,
                budget,
                cancel,
            )?;
            Ok(attach_receipt(operation_result, &intent, false))
        }
        Action::Accept => Err(refusal(
            "accept must use the existing inbox answer operation",
        )),
    }
}

pub(super) fn without_queue_control(input: &Value) -> Result<Value, FacadeError> {
    let mut payload = input.clone();
    payload
        .as_object_mut()
        .ok_or_else(|| refusal("graph operation must be an object"))?
        .remove("queue_control");
    Ok(payload)
}

fn inbox_item(
    factory: &Factory,
    id: HumanRequestId,
    host: Option<String>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<InboxItem, FacadeError> {
    let result = BoundFacade { factory }
        .inbox_owner(
            InboxInput::List {
                extra: Default::default(),
                id: Some(id),
                session: None,
                state: InboxFilter::All,
                limit: 1,
                cursor: None,
            },
            host,
            budget,
            cancel,
        )
        .map_err(|_| FacadeError::OwnerUnavailable)?;
    match result {
        InboxResult::Item { item, .. } => Ok(*item),
        _ => Err(FacadeError::StateCorrupt),
    }
}

fn accept_request(
    factory: &Factory,
    input: InboxInput,
    host: Option<String>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<InboxResult, FacadeError> {
    let InboxInput::Answer {
        extra,
        queue_control,
        id,
        action,
        option_id,
        mut text,
        expect_revision,
        channel,
        operation_id,
    } = input
    else {
        return Err(refusal("queue accept requires an inbox answer"));
    };
    let mut metadata =
        queue_control.ok_or_else(|| refusal("queue control metadata is required"))?;
    if metadata.action != Action::Accept
        || operation_id.as_ref() != Some(&metadata.operation_id)
        || action != AnswerAction::Reply
        || option_id.is_some()
    {
        return Err(refusal(
            "accept requires a reply answer using the control operation ID",
        ));
    }
    let Some(raw_reason) = text.take() else {
        return Err(refusal("accept requires a reason in the answer text"));
    };
    let reason =
        provenant_fabric_runtime::graph::normalise_prompt_text(&raw_reason).map_err(refusal)?;
    patch::scan_for_secrets(&reason).map_err(refusal)?;
    let answer = InboxInput::Answer {
        extra,
        queue_control: None,
        id,
        action,
        option_id,
        text: Some(reason.clone()),
        expect_revision,
        channel,
        operation_id: operation_id.clone(),
    };
    answer
        .validate()
        .map_err(|error| refusal(error.to_string()))?;
    let item = inbox_item(factory, id, host.clone(), budget, cancel)?;
    if item.request.owner_host != factory.context.identity.host {
        return Err(refusal(
            "queue accept must run on the host that owns the graph",
        ));
    }
    let request_graph = item
        .request
        .request
        .work
        .as_ref()
        .map(|work| work.graph.as_str());
    let answer_replay = item.answer.as_ref().is_some_and(|saved| {
        saved.operation_id == metadata.operation_id
            && saved.request_revision == expect_revision
            && item.request.answer_id == Some(saved.id)
    });
    if (!answer_replay
        && (item.request.revision != expect_revision || item.request.state != RequestState::Open))
        || metadata
            .graph_id
            .as_deref()
            .is_some_and(|expected| request_graph != Some(expected))
    {
        return Err(refusal(
            "accept request is stale or belongs to another control",
        ));
    }
    let graph_id = item
        .request
        .request
        .work
        .as_ref()
        .map(|work| work.graph.clone())
        .ok_or_else(|| refusal("accept inbox request has no graph identity"))?;
    metadata.graph_id = Some(graph_id.clone());
    let project = factory.context.identity.project;
    let execution = FileJournal::execution(&factory.context.roots.state, project, &graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let payload = serde_json::to_value(&answer).map_err(|_| FacadeError::StateCorrupt)?;
    let (mut intent, replayed) = begin(
        factory,
        &graph_id,
        (metadata.review_node.as_deref(), Some(id)),
        &metadata,
        &payload,
        budget,
        cancel,
    )?;
    if intent_requires_parallel_slot(&intent)
        && !reserve_control_capacity(factory, &graph_id, budget, cancel)?
    {
        return Err(refusal(
            "parked queue item is waiting for max_parallel capacity",
        ));
    }
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        project,
        &graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let mut checkpoint = journal
        .load(&graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let saved = ledger(&checkpoint)
        .intents
        .get(&metadata.operation_id.to_string())
        .cloned()
        .ok_or(FacadeError::StateCorrupt)?;
    if saved.phase == Phase::Refused {
        return Err(refusal("this queue accept operation was already refused"));
    }
    if !matches!(saved.phase, Phase::DispositionAdmitted | Phase::Applied)
        && patch::revision(&checkpoint) != metadata.expected_revision
    {
        refuse_intent_locked(&mut journal, &mut checkpoint, &metadata.operation_id)?;
        return Err(refusal(
            "queue accept graph revision changed before its answer was admitted",
        ));
    }
    if !matches!(saved.phase, Phase::DispositionAdmitted | Phase::Applied) {
        let probe = DeliveryReviewDisposition {
            operation_id: metadata.operation_id.clone(),
            request_id: id,
            answer_id: HumanAnswerId::from_bytes([0; 16]),
            head: metadata.review_head.ok_or(FacadeError::StateCorrupt)?,
            reason: reason.clone(),
        };
        let mut probe_checkpoint = checkpoint.clone();
        let probe_result = patch::admit_delivery_review_disposition(
            &mut LiveGraph {
                document: &execution.document,
                checkpoint: &mut probe_checkpoint,
            },
            metadata.expected_revision,
            metadata
                .review_node
                .as_deref()
                .ok_or(FacadeError::StateCorrupt)?,
            &probe,
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
            execution.requester_path.as_deref(),
        );
        if let Err(error) = probe_result {
            refuse_intent_locked(&mut journal, &mut checkpoint, &metadata.operation_id)?;
            return Err(refusal(error.to_string()));
        }
    }
    // A prior Inbox write with this same operation ID is replayed by the inbox
    // store, closing the crash window between the answer receipt and graph edit.
    let local_host = factory.context.identity.host.to_string();
    let result = BoundFacade { factory }
        .inbox_owner(answer, Some(local_host), budget, cancel)
        .map_err(|_| FacadeError::OwnerUnavailable)?;
    let InboxResult::Saved { mut receipt } = result else {
        return Err(FacadeError::StateCorrupt);
    };
    let answer_id = receipt.answer_id.ok_or(FacadeError::StateCorrupt)?;
    if !matches!(intent.phase, Phase::DispositionAdmitted | Phase::Applied) {
        let mut control_ledger = ledger(&checkpoint);
        let saved = control_ledger
            .intents
            .get_mut(&metadata.operation_id.to_string())
            .ok_or(FacadeError::StateCorrupt)?;
        saved.phase = Phase::Answered;
        saved.answer_id = Some(answer_id);
        save_ledger(&mut checkpoint, &control_ledger);
        journal.save(&checkpoint).map_err(graph_journal_error)?;
    }
    let (admitted_intent, gates) = apply_accept_disposition_locked(
        factory,
        &execution.document,
        execution.requester_path.as_deref(),
        &mut journal,
        &mut checkpoint,
        &intent,
        (answer_id, &reason),
    )?;
    drop(journal);
    retry_failed_gates(
        factory,
        &graph_id,
        &metadata.operation_id.to_string(),
        &gates,
        budget,
        cancel,
    )?;
    intent = update_intent(
        factory,
        &graph_id,
        &metadata.operation_id,
        budget,
        cancel,
        |saved| {
            saved.phase = Phase::Applied;
            saved.applied_revision = admitted_intent.applied_revision;
        },
    )?;
    receipt.queue_control = Some(typed_control_receipt(&intent, replayed));
    Ok(InboxResult::Saved { receipt })
}

fn apply_accept_disposition_locked(
    factory: &Factory,
    document: &provenant_fabric_runtime::graph::GraphDocument,
    requester_path: Option<&[u8]>,
    journal: &mut FileJournal,
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    intent: &Intent,
    (answer_id, reason): (HumanAnswerId, &str),
) -> Result<(Intent, Vec<String>), FacadeError> {
    let disposition = DeliveryReviewDisposition {
        operation_id: intent.metadata.operation_id.clone(),
        request_id: intent.request_id.ok_or(FacadeError::StateCorrupt)?,
        answer_id,
        head: intent
            .metadata
            .review_head
            .ok_or(FacadeError::StateCorrupt)?,
        reason: reason.to_owned(),
    };
    let node_id = intent
        .metadata
        .review_node
        .as_deref()
        .ok_or(FacadeError::StateCorrupt)?;
    let admitted = patch::admit_delivery_review_disposition(
        &mut LiveGraph {
            document,
            checkpoint,
        },
        intent.metadata.expected_revision,
        node_id,
        &disposition,
        &factory.context.config.graph.limits,
        &factory.context.config.graph.budget,
        requester_path,
    )
    .map_err(|error| refusal(error.to_string()))?;
    let gates = patch::failed_delivery_review_gates(
        document,
        checkpoint,
        node_id,
        &factory.context.config.graph.limits,
    )
    .map_err(|error| refusal(error.to_string()))?;
    let mut ledger = ledger(checkpoint);
    let key = intent.metadata.operation_id.to_string();
    let disposition_was_admitted = ledger.intents.get(&key).is_some_and(|saved| {
        saved.phase == Phase::DispositionAdmitted || saved.phase == Phase::Applied
    });
    let saved = ledger
        .intents
        .get_mut(&key)
        .ok_or(FacadeError::StateCorrupt)?;
    saved.phase = Phase::DispositionAdmitted;
    saved.answer_id = Some(answer_id);
    saved.applied_revision = Some(admitted.revision);
    if !disposition_was_admitted {
        saved.gate_generations = gates
            .iter()
            .filter_map(|gate| {
                checkpoint
                    .nodes
                    .iter()
                    .find(|progress| progress.id == *gate)
                    .map(|progress| (gate.clone(), progress.retry_generation.unwrap_or_default()))
            })
            .collect();
    }
    let saved = saved.clone();
    save_ledger(checkpoint, &ledger);
    journal.save(checkpoint).map_err(graph_journal_error)?;
    Ok((saved, gates))
}

fn retry_failed_gates(
    factory: &Factory,
    graph_id: &str,
    operation_key: &str,
    gates: &[String],
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    for gate in gates {
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            graph_id,
            budget,
            cancel,
            &factory.clock,
        )
        .map_err(graph_journal_error)?;
        let checkpoint = journal
            .load(graph_id)
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
        let ledger = ledger(&checkpoint);
        let intent = ledger
            .intents
            .get(operation_key)
            .ok_or(FacadeError::StateCorrupt)?;
        let Some(baseline) = intent.gate_generations.get(gate).copied() else {
            continue;
        };
        let accept_operation_id = intent.metadata.operation_id.to_string();
        let progress = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == *gate)
            .ok_or(FacadeError::StateCorrupt)?;
        if progress.retry_generation.unwrap_or_default() != baseline
            || !matches!(progress.state, NodeState::Failed | NodeState::Blocked)
        {
            continue;
        }
        drop(journal);
        factory.graph_document(
            json!({
                "op":"retry",
                "id":format!("{graph_id}/{gate}"),
                "queue_accept_retry": {
                    "operation_id": accept_operation_id,
                    "gate": gate,
                    "baseline_generation": baseline,
                }
            }),
            budget,
            cancel,
        )?;
    }
    Ok(())
}

pub(super) fn handle_inbox_control(
    factory: &Factory,
    input: InboxInput,
    host: Option<String>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Option<InboxResult>, FacadeError> {
    let marked_answer = matches!(
        &input,
        InboxInput::Answer {
            queue_control: Some(_),
            ..
        }
    );
    if !marked_answer {
        return Ok(None);
    }
    accept_request(factory, input, host, budget, cancel).map(Some)
}

pub(super) fn reconcile_pending(
    factory: &Factory,
    graph_id: &str,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph_id,
        budget,
        cancel,
        &factory.clock,
    )
    .map_err(graph_journal_error)?;
    let checkpoint = journal
        .load(graph_id)
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
    let mut intents: Vec<_> = ledger(&checkpoint)
        .intents
        .into_values()
        .filter(|intent| !intent.phase.terminal())
        .collect();
    drop(journal);
    intents.sort_by_key(|intent| intent.metadata.action != Action::Drop);
    if intents.iter().any(intent_requires_parallel_slot)
        && !reserve_control_capacity(factory, graph_id, budget, cancel)?
    {
        // Keep the durable control intent for the scheduler to replay after it
        // can atomically reacquire this item's parallel slot.
        for intent in intents
            .iter()
            .filter(|intent| intent.metadata.action == Action::Drop)
        {
            let mut input = intent.payload.clone();
            input["queue_control"] =
                serde_json::to_value(&intent.metadata).map_err(|_| FacadeError::StateCorrupt)?;
            handle_graph_operation(factory, input, budget, cancel)?;
        }
        return Ok(());
    }
    for intent in intents {
        match intent.metadata.action {
            Action::Accept => {
                let mut payload = intent.payload.clone();
                payload["queue_control"] = serde_json::to_value(&intent.metadata)
                    .map_err(|_| FacadeError::StateCorrupt)?;
                let input: InboxInput =
                    serde_json::from_value(payload).map_err(|_| FacadeError::StateCorrupt)?;
                accept_request(factory, input, None, budget, cancel)?;
            }
            Action::Fix if intent.payload.get("kind").and_then(Value::as_str) == Some("steer") => {
                let work_key: provenant_records::WorkKey = serde_json::from_value(
                    intent
                        .payload
                        .get("work_key")
                        .cloned()
                        .ok_or(FacadeError::StateCorrupt)?,
                )
                .map_err(|_| FacadeError::StateCorrupt)?;
                let text = intent
                    .payload
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(FacadeError::StateCorrupt)?;
                let node = intent
                    .target_node
                    .as_deref()
                    .ok_or(FacadeError::StateCorrupt)?;
                queue_live_fix(
                    factory,
                    graph_id,
                    node,
                    &intent.metadata,
                    (text, &work_key),
                    budget,
                    cancel,
                )?;
            }
            Action::Drop | Action::Retry | Action::Fix => {
                let mut input = intent.payload.clone();
                input["queue_control"] = serde_json::to_value(&intent.metadata)
                    .map_err(|_| FacadeError::StateCorrupt)?;
                handle_graph_operation(factory, input, budget, cancel)?;
            }
        }
    }
    Ok(())
}

fn close_owned_park(
    factory: &Factory,
    intent: &Intent,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    let Some(request_id) = intent.request_id else {
        return Ok(());
    };
    let item = inbox_item(factory, request_id, None, budget, cancel)?;
    if item.request.state != RequestState::Open {
        return Ok(());
    }
    let result = BoundFacade { factory }
        .inbox_owner(
            InboxInput::Close {
                extra: Default::default(),
                id: request_id,
                state: RequestState::Withdrawn,
                reason: "delivery queue item dropped".into(),
                expect_revision: item.request.revision,
                operation_id: Some(intent.metadata.operation_id.clone()),
            },
            Some(item.request.owner_host.to_string()),
            budget,
            cancel,
        )
        .map_err(super::interface::relay_error)?;
    if matches!(result, InboxResult::Saved { .. }) {
        Ok(())
    } else {
        Err(FacadeError::StateCorrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn operation(value: u8) -> OperationId {
        OperationId::parse(&format!("{value:02x}").repeat(16)).unwrap()
    }

    #[test]
    fn operation_fixture_preserves_all_bytes_and_distinguishes_operations() {
        assert_eq!(operation(1).to_string(), "01".repeat(16));
        assert_ne!(operation(1), operation(2));
    }

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn metadata(revision: u64, operation_id: OperationId) -> Metadata {
        Metadata {
            action: Action::Retry,
            expected_revision: revision,
            operation_id,
            review_node: None,
            review_head: None,
            graph_id: Some("queue-1".into()),
        }
    }

    #[test]
    fn pending_control_survives_checkpoint_restart_and_blocks_competitors() {
        let control = metadata(4, operation(1));
        let intent = Intent {
            digest: "digest".into(),
            metadata: control.clone(),
            graph_id: "queue-1".into(),
            target_node: Some("node".into()),
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::new(),
            request_id: None,
            payload: json!({"op":"retry"}),
            phase: Phase::Pending,
            answer_id: None,
            applied_revision: None,
        };
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": []
            }))
            .unwrap();
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(control.operation_id.to_string(), intent.clone());
        save_ledger(&mut checkpoint, &saved);

        // Model a process restart by round-tripping the journal checkpoint.
        let persisted = serde_json::to_value(&checkpoint).unwrap();
        let restored: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(persisted).unwrap();
        let restored_ledger = ledger(&restored);
        assert_eq!(
            restored_ledger
                .intents
                .get(&control.operation_id.to_string()),
            Some(&intent)
        );
        assert!(competes_with(&restored_ledger, &metadata(5, operation(2))));

        let mut completed = restored_ledger.clone();
        completed
            .intents
            .get_mut(&control.operation_id.to_string())
            .unwrap()
            .phase = Phase::Applied;
        assert!(competes_with(&completed, &metadata(4, operation(2))));
        assert!(!competes_with(&completed, &metadata(5, operation(2))));

        let mut dropping = restored_ledger;
        dropping
            .intents
            .get_mut(&control.operation_id.to_string())
            .unwrap()
            .phase = Phase::DropAdmitted;
        let persisted = serde_json::to_value(&dropping).unwrap();
        let restored: Ledger = serde_json::from_value(persisted).unwrap();
        assert!(!Phase::DropAdmitted.terminal());
        assert!(competes_with(&restored, &metadata(5, operation(2))));
    }

    #[test]
    fn malformed_control_ledger_is_not_treated_as_absent() {
        let checkpoint = json!({
            "schema_version": 2,
            "graph_id": "queue-1",
            "plan_digest": "digest",
            "nodes": [],
            "delivery_queue_controls": {"intents": "invalid"}
        });
        assert!(
            serde_json::from_value::<provenant_fabric_runtime::graph::GraphCheckpoint>(checkpoint)
                .is_err()
        );
    }

    #[test]
    fn operation_replay_digest_binds_target_and_payload() {
        let control = metadata(4, operation(1));
        let payload = json!({"op":"retry"});
        let first = control_digest(&control, "queue-1", Some("node"), None, &payload).unwrap();
        let replay = control_digest(&control, "queue-1", Some("node"), None, &payload).unwrap();
        let changed =
            control_digest(&control, "queue-1", Some("other-node"), None, &payload).unwrap();
        assert_eq!(first, replay);
        assert_ne!(first, changed);
    }

    #[test]
    fn accept_gate_retry_is_bound_to_saved_answer_and_failed_gate_generation() {
        use provenant_core::GitOid;
        let operation_id = operation(8);
        let request_id = HumanRequestId::from_bytes([1; 16]);
        let answer_id = HumanAnswerId::from_bytes([2; 16]);
        let head = GitOid::parse(&"a".repeat(40)).unwrap();
        let reason = "accept this exact reviewed head";
        let control = Metadata {
            action: Action::Accept,
            expected_revision: 4,
            operation_id: operation_id.clone(),
            review_node: Some("review".into()),
            review_head: Some(head),
            graph_id: Some("queue-1".into()),
        };
        let payload = serde_json::to_value(InboxInput::Answer {
            queue_control: None,
            extra: Default::default(),
            id: request_id,
            action: AnswerAction::Reply,
            option_id: None,
            text: Some(reason.into()),
            expect_revision: 4,
            channel: provenant_records::inbox::InteractionChannel::Cli,
            operation_id: Some(operation_id.clone()),
        })
        .unwrap();
        let intent = Intent {
            digest: "saved-accept-intent".into(),
            metadata: control,
            graph_id: "queue-1".into(),
            target_node: Some("review".into()),
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::from([("gate".into(), 2)]),
            request_id: Some(request_id),
            payload,
            phase: Phase::DispositionAdmitted,
            answer_id: Some(answer_id),
            applied_revision: Some(5),
        };
        let disposition = DeliveryReviewDisposition {
            operation_id: operation_id.clone(),
            request_id,
            answer_id,
            head,
            reason: reason.into(),
        };
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": [
                    {
                        "id":"review", "state":"failed", "worktree":null, "attempts":[],
                        "delivery_review_disposition": disposition
                    },
                    {
                        "id":"gate", "state":"failed", "worktree":null, "attempts":[],
                        "retry_generation":2
                    }
                ]
            }))
            .unwrap();
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(operation_id.to_string(), intent.clone());
        save_ledger(&mut checkpoint, &saved);

        let marker = json!({
            "operation_id":operation_id,
            "gate":"gate",
            "baseline_generation":2
        });
        let (key, digest) =
            authorise_accept_gate_retry(&checkpoint, "queue-1", "gate", &marker).unwrap();
        assert!(key.starts_with("accept-gate-"));
        assert!(digest.starts_with("sha256:"));
        assert!(authorise_accept_gate_retry(&checkpoint, "queue-1", "other", &marker).is_err());
        assert!(authorise_accept_gate_retry(&checkpoint, "other-graph", "gate", &marker).is_err());

        let mut stale = marker.clone();
        stale["baseline_generation"] = json!(3);
        assert!(authorise_accept_gate_retry(&checkpoint, "queue-1", "gate", &stale).is_err());

        let mut dropping = saved;
        let mut drop_intent = intent;
        drop_intent.metadata.action = Action::Drop;
        drop_intent.phase = Phase::DropAdmitted;
        dropping.intents.insert("drop-op".into(), drop_intent);
        let mut fenced = checkpoint;
        save_ledger(&mut fenced, &dropping);
        assert!(authorise_accept_gate_retry(&fenced, "queue-1", "gate", &marker).is_err());
    }

    #[test]
    fn only_pending_controls_that_can_wake_a_writer_need_a_parallel_slot() {
        let retry = metadata(4, operation(4));
        let retry = Intent {
            digest: "retry".into(),
            metadata: retry,
            graph_id: "queue-1".into(),
            target_node: Some("node".into()),
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::new(),
            request_id: None,
            payload: json!({"op":"retry"}),
            phase: Phase::Pending,
            answer_id: None,
            applied_revision: None,
        };
        let mut drop_intent = retry.clone();
        drop_intent.metadata.action = Action::Drop;
        drop_intent.phase = Phase::DropAdmitted;
        let mut accept = retry.clone();
        accept.metadata.action = Action::Accept;
        accept.phase = Phase::Answered;
        let mut applied = retry.clone();
        applied.phase = Phase::Applied;

        assert!(intent_requires_parallel_slot(&retry));
        assert!(intent_requires_parallel_slot(&accept));
        assert!(!intent_requires_parallel_slot(&drop_intent));
        assert!(!intent_requires_parallel_slot(&applied));

        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": []
            }))
            .unwrap();
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(retry.metadata.operation_id.to_string(), drop_intent);
        save_ledger(&mut checkpoint, &saved);
        assert!(checkpoint_has_pending_control(&checkpoint).unwrap());
        assert!(!checkpoint_has_pending_parallel_control(&checkpoint).unwrap());
    }

    #[test]
    fn admitted_drop_fences_retry_and_edit_after_restart() {
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": []
            }))
            .unwrap();
        let mut drop_metadata = metadata(4, operation(3));
        drop_metadata.action = Action::Drop;
        let drop_intent = Intent {
            digest: "drop-digest".into(),
            metadata: drop_metadata.clone(),
            graph_id: "queue-1".into(),
            target_node: None,
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::new(),
            request_id: None,
            payload: json!({"op":"stop","id":"queue-1"}),
            phase: Phase::DropAdmitted,
            answer_id: None,
            applied_revision: None,
        };
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(drop_metadata.operation_id.to_string(), drop_intent);
        save_ledger(&mut checkpoint, &saved);

        let persisted = serde_json::to_value(&checkpoint).unwrap();
        let restored: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(persisted).unwrap();
        assert!(drop_fence_active(&restored).unwrap());

        let mut refused = ledger(&restored);
        refused
            .intents
            .get_mut(&drop_metadata.operation_id.to_string())
            .unwrap()
            .phase = Phase::Refused;
        save_ledger(&mut checkpoint, &refused);
        assert!(!drop_fence_active(&checkpoint).unwrap());
    }
}
