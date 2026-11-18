//! Resolve a graph steer to one verified live lane attempt.
use super::*;
use provenant_fabric_runtime::{
    coordination::facade::AttemptTarget,
    graph::{
        AttemptState as GraphAttemptState, FileJournal, GraphAttempt, GraphCheckpoint, GraphJournal,
    },
    hosts::{PeerAction, PeerResult},
};
use provenant_records::{WorkKey, run::AttemptState};
use std::collections::HashSet;

const MAX_SUPERSEDED: usize = 32;

pub(super) enum TargetProof {
    Verified(Box<VerifiedTarget>),
    Unavailable,
    Settled,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProofFailure {
    Unavailable,
    Conflict,
}

pub(super) struct VerifiedTarget {
    pub target: AttemptTarget,
    pub reference: String,
    pub work_key: WorkKey,
    pub launch_actor: provenant_core::ActorId,
    pub launch_operation: provenant_core::OperationId,
}

struct LaunchBinding<'a> {
    graph_attempt: &'a GraphAttempt,
    work_key: WorkKey,
    launch_actor: provenant_core::ActorId,
    launch_operation: provenant_core::OperationId,
    host: provenant_core::HostId,
}

pub(super) fn selected_is_settled(
    resolved: &provenant_cli::target::Resolved,
    checkpoint: &GraphCheckpoint,
) -> bool {
    checkpoint
        .nodes
        .iter()
        .find(|node| resolved.node.as_deref() == Some(node.id.as_str()))
        .and_then(|node| {
            node.recorded_attempts().find(|attempt| {
                resolved.number.map(|number| number.get()) == Some(attempt.key.attempt)
            })
        })
        .is_some_and(|attempt| attempt.state == GraphAttemptState::Terminal)
}

/// Resolve only the selected graph/node/attempt. Temporary owner/placement
/// gaps remain distinguishable from explicit identity or lineage conflicts.
pub(super) fn verified_target(
    factory: &Factory,
    resolved: &provenant_cli::target::Resolved,
    checkpoint: &GraphCheckpoint,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> TargetProof {
    let project = factory.context.identity.project;
    let Some(work_key) = resolved.work_key(project) else {
        return TargetProof::Conflict;
    };
    let Some(node) = checkpoint
        .nodes
        .iter()
        .find(|node| resolved.node.as_deref() == Some(node.id.as_str()))
    else {
        return TargetProof::Conflict;
    };
    let attempts: Vec<_> = node.recorded_attempts().collect();
    let Some((index, graph_attempt)) = attempts
        .iter()
        .enumerate()
        .find(|(_, attempt)| attempt.key.work_key(project).ok().as_ref() == Some(&work_key))
    else {
        return TargetProof::Conflict;
    };
    if graph_attempt.state == GraphAttemptState::Terminal {
        return TargetProof::Settled;
    }
    if index + 1 != attempts.len()
        || !graph_continuation_matches(&attempts, index)
        || resolved.number.map(|number| number.get()) != Some(graph_attempt.key.attempt)
    {
        return TargetProof::Conflict;
    }
    let launch_actor = match graph_attempt.launch_actor.parse() {
        Ok(actor) => actor,
        Err(_) => return TargetProof::Conflict,
    };
    let launch_operation = match super::graph_execution::graph_operation(
        "graph-attempt-operation",
        &graph_attempt.key,
    ) {
        Ok(operation) => operation,
        Err(_) => return TargetProof::Conflict,
    };
    let owner = BinaryRunOwner {
        factory,
        allow_remote: true,
        actor: Some(launch_actor),
        chair: None,
    };
    let local = factory.context.identity.host;
    let explicit_placement = match placement_host(factory, graph_attempt) {
        Ok(host) => host,
        Err(()) => return TargetProof::Conflict,
    };
    let reference_host = match graph_attempt.attempt_ref.as_deref() {
        Some(reference) => match parse_reference(reference).and_then(|target| target.host) {
            Some(host) => Some(host),
            None => return TargetProof::Conflict,
        },
        None => None,
    };
    if explicit_placement.is_some_and(|host| Some(host) != reference_host)
        && reference_host.is_some()
    {
        return TargetProof::Conflict;
    }
    let Some(host) = explicit_placement.or(reference_host) else {
        return TargetProof::Unavailable;
    };
    let binding = LaunchBinding {
        graph_attempt,
        work_key: work_key.clone(),
        launch_actor,
        launch_operation: launch_operation.clone(),
        host,
    };
    let initial = match accepted_target(factory, &owner, &binding, budget, cancel) {
        Ok(target) => target,
        Err(ProofFailure::Unavailable) => return TargetProof::Unavailable,
        Err(ProofFailure::Conflict) => return TargetProof::Conflict,
    };
    let checkpoint_target = match graph_attempt.attempt_ref.as_deref() {
        Some(reference) => match parse_reference(reference) {
            Some(target) => Some(target),
            None => return TargetProof::Conflict,
        },
        None => None,
    };
    let target_host = initial.host.unwrap_or(local);
    if host != target_host
        || (target_host != local
            && factory.context.peer_host(&target_host.to_string()) != Some(target_host))
    {
        return TargetProof::Conflict;
    }
    let target = match current_live_leaf(
        factory,
        &owner,
        initial,
        &binding,
        checkpoint_target.as_ref(),
        budget,
        cancel,
    ) {
        Ok(target) => target,
        Err(ProofFailure::Unavailable) => return TargetProof::Unavailable,
        Err(ProofFailure::Conflict) => return TargetProof::Conflict,
    };
    let reference = format!("{}/{}", target.host.unwrap_or(local), target.attempt);
    TargetProof::Verified(Box::new(VerifiedTarget {
        target,
        reference,
        work_key: binding.work_key,
        launch_actor: binding.launch_actor,
        launch_operation: binding.launch_operation,
    }))
}

/// Retry graph-scoped steers while the graph journal lock is held. A receipt
/// commits the inbox operation before its queue entry is removed, so a crash in
/// between replays the same operation under the same requester actor.
pub(super) fn dispatch_pending_steers(
    factory: &mut Factory,
    journal: &mut FileJournal,
    checkpoint: &mut GraphCheckpoint,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::PendingSteerRefusal;
    use provenant_fabric_runtime::identity::Address;
    use provenant_fabric_runtime::inbox::{InboxInput, InboxResult};
    use provenant_records::control::ControlState;
    use std::num::NonZeroU32;

    checkpoint
        .pending_steers
        .validate()
        .map_err(|_| FacadeError::StateCorrupt)?;
    let pending: Vec<_> = checkpoint
        .pending_steers
        .iter()
        .filter(|(_, value)| value.refusal.is_none())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut warnings = Vec::new();
    for (key, queued) in pending {
        if queued.work_key.project() != factory.context.identity.project
            || queued.work_key.graph() != checkpoint.graph_id.as_str()
            || queued.graph_context.origin_work_key != queued.work_key
        {
            checkpoint
                .pending_steers
                .refuse(&key, PendingSteerRefusal::TargetChanged);
            journal.save(checkpoint)?;
            warnings.push("a queued steer was refused because its graph identity changed".into());
            continue;
        }
        let number = NonZeroU32::new(queued.work_key.attempt()).ok_or(FacadeError::StateCorrupt)?;
        let resolved = provenant_cli::target::Resolved {
            graph: queued.work_key.graph().to_owned(),
            node: Some(queued.work_key.node().to_owned()),
            number: Some(number),
            attempt_ref: None,
        };
        let target = match verified_target(factory, &resolved, checkpoint, budget, cancel) {
            TargetProof::Unavailable => continue,
            TargetProof::Settled | TargetProof::Conflict => {
                checkpoint
                    .pending_steers
                    .refuse(&key, PendingSteerRefusal::TargetChanged);
                journal.save(checkpoint)?;
                warnings
                    .push("a queued steer was refused after its target identity changed".into());
                continue;
            }
            TargetProof::Verified(target) => target,
        };
        if target.work_key != queued.work_key
            || target.launch_actor != queued.graph_context.launch_actor
            || target.launch_operation != queued.graph_context.launch_operation
        {
            checkpoint
                .pending_steers
                .refuse(&key, PendingSteerRefusal::TargetChanged);
            journal.save(checkpoint)?;
            warnings.push("a queued steer was refused because its launch identity changed".into());
            continue;
        }
        let (host, attempt) = target
            .reference
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        let remote = target.target.host != Some(factory.context.identity.host);
        let input = InboxInput::Steer {
            extra: Default::default(),
            graph_context: remote.then(|| queued.graph_context.clone()),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            text: queued.text.clone(),
            operation_id: Some(queued.operation_id.clone()),
        };
        let address = Address::new(queued.requester_host, &queued.requester_agent)
            .map_err(|_| FacadeError::StateCorrupt)?;
        if queued.requester_host != factory.context.identity.host
            || address.actor != queued.requester_actor
        {
            checkpoint
                .pending_steers
                .refuse(&key, PendingSteerRefusal::RequesterChanged);
            journal.save(checkpoint)?;
            warnings.push(
                "a queued steer was refused because its original requester identity changed".into(),
            );
            continue;
        }
        let original_inbox = std::mem::replace(&mut factory.context.identity.inbox, address);
        let delivered = {
            let mut facade = BoundFacade { factory };
            facade.inbox_owner(input, Some(host.to_owned()), budget, cancel)
        };
        factory.context.identity.inbox = original_inbox;
        let receipt = match delivered {
            Ok(InboxResult::Control { receipt }) => receipt,
            Ok(_) => return Err(FacadeError::StateCorrupt.into()),
            Err(error) if steer_delivery_unavailable(&error) => continue,
            Err(_) => {
                checkpoint
                    .pending_steers
                    .refuse(&key, PendingSteerRefusal::OwnerRefused);
                journal.save(checkpoint)?;
                warnings.push("a queued steer was refused by its verified inbox owner".into());
                continue;
            }
        };
        if receipt.operation_id != queued.operation_id || receipt.target.as_str() != attempt {
            return Err(FacadeError::Conflict.into());
        }
        if receipt.state == ControlState::Unsupported {
            checkpoint
                .pending_steers
                .refuse(&key, PendingSteerRefusal::Unsupported);
            warnings.push("a queued steer was refused because this lane does not support steering; its text is retained".into());
        } else {
            checkpoint.pending_steers.remove(&key);
        }
        journal.save(checkpoint)?;
    }
    Ok(warnings)
}

pub(super) fn steer_delivery_unavailable(
    error: &provenant_fabric_runtime::relay::ServiceError,
) -> bool {
    // code() recursively projects local mutation/store failures as well as
    // remote owner replies, so temporary custody contention stays queued.
    if matches!(
        error.code(),
        "owner_unavailable" | "inbox_unavailable" | "ambiguous_outcome" | "busy" | "timeout"
    ) {
        return true;
    }

    use provenant_fabric_runtime::{inbox::InboxError, relay::ServiceError};
    match error {
        ServiceError::Mutation { source, .. } => steer_delivery_unavailable(source),
        ServiceError::Inbox(InboxError::Mutation { source, .. }) => {
            matches!(source.as_ref(), InboxError::OwnerUnavailable)
        }
        ServiceError::InboxOwner { code, .. } => matches!(
            code.as_str(),
            "owner_unavailable" | "inbox_unavailable" | "ambiguous_outcome"
        ),
        ServiceError::FabricOwnerUnavailable => true,
        ServiceError::Inbox(InboxError::OwnerUnavailable) => true,
        ServiceError::Facade(error) => matches!(
            error,
            FacadeError::OwnerUnavailable
                | FacadeError::NotFound
                | FacadeError::Busy
                | FacadeError::HomeUnavailable
                | FacadeError::LaunchUnknown { .. }
                | FacadeError::Timeout
                | FacadeError::Io { .. }
        ),
        _ => false,
    }
}

fn graph_continuation_matches(attempts: &[&GraphAttempt], index: usize) -> bool {
    let current = attempts[index];
    if attempts[..index]
        .iter()
        .any(|attempt| attempt.state != GraphAttemptState::Terminal)
    {
        return false;
    }
    let expected = attempts[..index]
        .iter()
        .rev()
        .find(|attempt| attempt.attempt_ref.is_some())
        .and_then(|attempt| attempt.attempt_ref.as_deref());
    current.key.previous_attempt_ref.as_deref() == expected
}

fn parse_reference(reference: &str) -> Option<AttemptTarget> {
    let (host, attempt) = reference.split_once('/')?;
    Some(AttemptTarget {
        extra: Default::default(),
        host: Some(host.parse().ok()?),
        attempt: attempt.parse().ok()?,
    })
}

fn placement_host(
    factory: &Factory,
    attempt: &GraphAttempt,
) -> Result<Option<provenant_core::HostId>, ()> {
    let local = factory.context.identity.host;
    let stable = match attempt.placement_host_id {
        Some(host) => Some(
            (host == local || factory.context.peer_host(&host.to_string()) == Some(host))
                .then_some(host)
                .ok_or(())?,
        ),
        None => None,
    };
    let alias = match attempt.placement_host.as_deref() {
        Some(name) => {
            let host = if matches!(name, "local" | "localhost")
                || name
                    .parse::<provenant_core::HostId>()
                    .is_ok_and(|host| host == local)
            {
                Some(local)
            } else {
                factory.context.peer_host(name)
            }
            .ok_or(())?;
            Some(host)
        }
        None => None,
    };
    if stable.is_some() && alias.is_some() && stable != alias {
        return Err(());
    }
    Ok(stable.or(alias))
}

fn accepted_target(
    factory: &Factory,
    owner: &BinaryRunOwner<'_>,
    binding: &LaunchBinding<'_>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<AttemptTarget, ProofFailure> {
    let local = factory.context.identity.host;
    if binding.host == local {
        let session = owner.read_session(cancel).map_err(facade_proof_failure)?;
        let call = owner.call(cancel);
        let operation = session
            .store
            .operation(&binding.launch_operation, &call)
            .map_err(run_proof_failure)?;
        let Some(operation) = operation else {
            return Err(ProofFailure::Unavailable);
        };
        match operation.state {
            provenant_run::OperationState::Accepted
            | provenant_run::OperationState::Terminal
            | provenant_run::OperationState::Blocked => {}
            provenant_run::OperationState::Prepared
            | provenant_run::OperationState::LaunchUnknown => {
                return Err(ProofFailure::Unavailable);
            }
            provenant_run::OperationState::Refused => return Err(ProofFailure::Conflict),
        }
        let admitted = session
            .store
            .operation_attempt(&binding.launch_operation, &call)
            .map_err(run_proof_failure)?;
        let key = binding
            .graph_attempt
            .key
            .work_key(factory.context.identity.project)
            .map_err(|_| ProofFailure::Conflict)?;
        let request = session
            .store
            .request(admitted, &call)
            .map_err(run_proof_failure)?;
        if !request.task.context.as_ref().is_some_and(|context| {
            context.graph_template_launch
                && context.graph_id.as_deref() == Some(key.graph())
                && context.node_id.as_deref() == Some(key.node())
                && context.attempt == Some(key.attempt())
        }) {
            return Err(ProofFailure::Conflict);
        }
        let record = session
            .store
            .record(admitted, &call)
            .map_err(run_proof_failure)?;
        if record.key != key
            || record.project_id != factory.context.identity.project
            || record.verified_owner != Some(binding.launch_actor)
        {
            return Err(ProofFailure::Conflict);
        }
        let receipt = owner
            .receipt(
                &session,
                binding.launch_operation.clone(),
                &record,
                &[],
                &call,
            )
            .map_err(facade_proof_failure)?;
        if receipt.operation_id != binding.launch_operation || receipt.targets.len() != 1 {
            return Err(ProofFailure::Conflict);
        }
        let target = receipt
            .targets
            .into_iter()
            .next()
            .ok_or(ProofFailure::Conflict)?;
        return (target.host == Some(local) && target.attempt == admitted)
            .then_some(target)
            .ok_or(ProofFailure::Conflict);
    }
    let expected_graph =
        super::peer::peer_graph_name(binding.launch_actor, &binding.launch_operation)
            .map_err(|_| ProofFailure::Conflict)?;
    let graph_context = Some(provenant_fabric_runtime::hosts::GraphReceiptContext {
        launch_actor: binding.launch_actor,
        origin_work_key: binding.work_key.clone(),
    });
    match owner.peer(
        binding.host,
        PeerAction::Receipt {
            extra: Default::default(),
            graph_context,
            operation_id: binding.launch_operation.clone(),
        },
        budget,
        cancel,
    ) {
        Ok(PeerResult::Submitted { data, .. }) => {
            if data.operation_id != binding.launch_operation || data.targets.len() != 1 {
                return Err(ProofFailure::Conflict);
            }
            let target = data
                .targets
                .into_iter()
                .next()
                .ok_or(ProofFailure::Conflict)?;
            if target.host != Some(binding.host)
                || !data.peer_graph.as_ref().is_some_and(|graph| {
                    graph.host == binding.host && graph.graph_id == expected_graph
                })
            {
                return Err(ProofFailure::Conflict);
            }
            Ok(target)
        }
        Err(error) => Err(facade_proof_failure(error)),
        _ => Err(ProofFailure::Unavailable),
    }
}

fn facade_proof_failure(error: FacadeError) -> ProofFailure {
    use FacadeError as E;
    match error {
        E::OwnerUnavailable
        | E::NotFound
        | E::Busy
        | E::HomeUnavailable
        | E::PeerVersion
        | E::PeerProtocolMismatch { .. }
        | E::LaunchUnknown { .. }
        | E::Timeout
        | E::Io { .. } => ProofFailure::Unavailable,
        _ => ProofFailure::Conflict,
    }
}

fn run_proof_failure(error: provenant_run::RunError) -> ProofFailure {
    use provenant_run::RunError as E;
    match error {
        E::NotFound | E::Contention | E::Cancelled | E::Io(_) | E::OperationIo { .. } => {
            ProofFailure::Unavailable
        }
        _ => ProofFailure::Conflict,
    }
}

fn current_live_leaf(
    factory: &Factory,
    owner: &BinaryRunOwner<'_>,
    mut target: AttemptTarget,
    binding: &LaunchBinding<'_>,
    checkpoint_target: Option<&AttemptTarget>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<AttemptTarget, ProofFailure> {
    let mut seen = HashSet::new();
    let mut predecessor: Option<provenant_records::run::AttemptRecord> = None;
    let mut receiver_project = None;
    let mut checkpoint_target_seen = checkpoint_target.is_none();
    for _ in 0..MAX_SUPERSEDED {
        if target.host.unwrap_or(factory.context.identity.host) != binding.host
            || !seen.insert(target.attempt)
        {
            return Err(ProofFailure::Conflict);
        }
        let record = owner
            .snapshot(target.clone(), budget, cancel)
            .map_err(facade_proof_failure)?;
        if !record_matches(
            factory,
            &record,
            &target,
            binding,
            receiver_project,
            predecessor.as_ref(),
        ) {
            return Err(ProofFailure::Conflict);
        }
        if checkpoint_target
            .is_some_and(|saved| saved.attempt == target.attempt && saved.host == target.host)
        {
            checkpoint_target_seen = true;
        }
        receiver_project.get_or_insert(record.project_id);
        if let Some(previous) = &predecessor
            && (record.retry_of != Some(previous.attempt_id)
                || previous.superseded_by != Some(record.attempt_id)
                || !successor_key_matches(&previous.key, &record.key)
                || record.project_id != previous.project_id)
        {
            return Err(ProofFailure::Conflict);
        }
        match record.superseded_by {
            Some(next) => {
                if !matches!(record.state, AttemptState::Terminal { .. }) {
                    return Err(ProofFailure::Conflict);
                }
                predecessor = Some(record);
                target.attempt = next;
            }
            None => {
                if matches!(record.state, AttemptState::Terminal { .. }) || !checkpoint_target_seen
                {
                    return Err(ProofFailure::Conflict);
                }
                return Ok(target);
            }
        }
    }
    Err(ProofFailure::Conflict)
}

fn record_matches(
    factory: &Factory,
    record: &provenant_records::run::AttemptRecord,
    target: &AttemptTarget,
    binding: &LaunchBinding<'_>,
    receiver_project: Option<provenant_core::ProjectId>,
    predecessor: Option<&provenant_records::run::AttemptRecord>,
) -> bool {
    if record.attempt_id != target.attempt
        || receiver_project.is_some_and(|project| project != record.project_id)
    {
        return false;
    }
    let local = factory.context.identity.host;
    if binding.host == local {
        return lineage_key_matches(&binding.work_key, predecessor, record)
            && record.verified_owner == Some(binding.launch_actor)
            && record.context.as_ref().is_some_and(|context| {
                context.graph_id.as_deref() == Some(record.key.graph())
                    && context.node_id.as_deref() == Some(record.key.node())
                    && context_attempt_matches(
                        context.attempt,
                        record.key.attempt(),
                        predecessor.is_some(),
                    )
            });
    }
    let receiver_actor = provenant_fabric_runtime::identity::Address::new(
        local,
        &format!("graph-{}", binding.launch_actor),
    )
    .ok()
    .map(|address| address.actor);
    if record.verified_owner != receiver_actor {
        return false;
    }
    let expected_graph =
        super::peer::peer_graph_name(binding.launch_actor, &binding.launch_operation).ok();
    peer_lineage_key_matches(&binding.work_key, predecessor, record)
        && record.key.node() == binding.work_key.node()
        && record.key.project() == record.project_id
        && expected_graph.as_deref() == Some(record.key.graph())
        && record.context.as_ref().is_some_and(|context| {
            context.graph_id.as_deref() == expected_graph.as_deref()
                && context.node_id.as_deref() == Some(record.key.node())
                && context_attempt_matches(
                    context.attempt,
                    record.key.attempt(),
                    predecessor.is_some(),
                )
        })
        && super::peer::peer_origin_matches(&record.peer, &binding.work_key)
        && record.peer.peer_graph.as_ref().is_some_and(|graph| {
            graph.host == binding.host && Some(graph.graph_id.as_str()) == expected_graph.as_deref()
        })
}

fn lineage_key_matches(
    origin: &WorkKey,
    predecessor: Option<&provenant_records::run::AttemptRecord>,
    record: &provenant_records::run::AttemptRecord,
) -> bool {
    match predecessor {
        None => record.project_id == origin.project() && record.key == *origin,
        Some(previous) => {
            record.project_id == previous.project_id
                && record.key.project() == previous.key.project()
                && record.key.graph() == previous.key.graph()
                && record.key.node() == previous.key.node()
                && previous.key.attempt().checked_add(1) == Some(record.key.attempt())
                && record.retry_of == Some(previous.attempt_id)
                && previous.superseded_by == Some(record.attempt_id)
        }
    }
}

fn peer_lineage_key_matches(
    origin: &WorkKey,
    predecessor: Option<&provenant_records::run::AttemptRecord>,
    record: &provenant_records::run::AttemptRecord,
) -> bool {
    match predecessor {
        None => record.key.node() == origin.node(),
        Some(previous) => {
            successor_key_matches(&previous.key, &record.key)
                && record.retry_of == Some(previous.attempt_id)
                && previous.superseded_by == Some(record.attempt_id)
        }
    }
}

pub(super) fn successor_key_matches(previous: &WorkKey, next: &WorkKey) -> bool {
    previous.project() == next.project()
        && previous.graph() == next.graph()
        && previous.node() == next.node()
        && previous.attempt().checked_add(1) == Some(next.attempt())
}

pub(super) fn context_attempt_matches(
    context: Option<u32>,
    key_attempt: u32,
    successor: bool,
) -> bool {
    match context {
        Some(attempt) => attempt == key_attempt,
        None => successor,
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "the custom worktree driver includes this module without libtest"
)]
mod tests {
    use super::*;

    #[test]
    fn run_continuations_preserve_identity_and_advance_one_ordinal() {
        let project = provenant_core::ProjectId::from_bytes([1; 16]);
        let previous = WorkKey::new(project, "graph", "writer", 2).unwrap();
        let next = WorkKey::new(project, "graph", "writer", 3).unwrap();
        assert!(successor_key_matches(&previous, &next));
        for invalid in [
            WorkKey::new(
                provenant_core::ProjectId::from_bytes([2; 16]),
                "graph",
                "writer",
                3,
            )
            .unwrap(),
            WorkKey::new(project, "other", "writer", 3).unwrap(),
            WorkKey::new(project, "graph", "other", 3).unwrap(),
            previous.clone(),
            WorkKey::new(project, "graph", "writer", 4).unwrap(),
        ] {
            assert!(!successor_key_matches(&previous, &invalid));
        }
        assert!(context_attempt_matches(Some(3), 3, false));
        assert!(!context_attempt_matches(None, 3, false));
        assert!(context_attempt_matches(None, 3, true));
        assert!(!context_attempt_matches(Some(2), 3, true));
    }
}

#[cfg(test)]
mod retry_classification_tests {
    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn transient_owner_replies_retry_but_identity_conflicts_do_not()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::relay::ServiceError;
        for (code, retry) in [
            ("busy", true),
            ("owner_unavailable", true),
            ("work_mismatch", false),
        ] {
            let error = ServiceError::Mutation {
                operation_id: provenant_core::OperationId::parse("queued-steer")?,
                source: Box::new(ServiceError::InboxOwner {
                    code: code.into(),
                    message: "fixture".into(),
                }),
            };
            assert_eq!(super::steer_delivery_unavailable(&error), retry);
        }
        Ok(())
    }
}
