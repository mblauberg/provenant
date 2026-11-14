//! Resolve a graph steer to one verified live lane attempt.
use super::*;
use provenant_fabric_runtime::{
    coordination::facade::AttemptTarget,
    graph::{AttemptState as GraphAttemptState, GraphAttempt, GraphCheckpoint},
    hosts::{PeerAction, PeerResult},
};
use provenant_records::{WorkKey, run::AttemptState};
use std::collections::HashSet;

const MAX_SUPERSEDED: usize = 32;

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

/// Resolve only the selected graph/node/attempt. A missing receipt, uncertain
/// owner, conflicting identity or terminal leaf is a refusal.
pub(super) fn verified_target(
    factory: &Factory,
    resolved: &provenant_cli::target::Resolved,
    checkpoint: &GraphCheckpoint,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<Option<VerifiedTarget>, FacadeError> {
    let project = factory.context.identity.project;
    let Some(work_key) = resolved.work_key(project) else {
        return Ok(None);
    };
    let Some(node) = checkpoint
        .nodes
        .iter()
        .find(|node| resolved.node.as_deref() == Some(node.id.as_str()))
    else {
        return Ok(None);
    };
    let attempts: Vec<_> = node.recorded_attempts().collect();
    let Some((index, graph_attempt)) = attempts
        .iter()
        .enumerate()
        .find(|(_, attempt)| attempt.key.work_key(project).ok().as_ref() == Some(&work_key))
    else {
        return Ok(None);
    };
    // A numbered target can name history. Controls only reach the newest leaf.
    if index + 1 != attempts.len()
        || graph_attempt.state == GraphAttemptState::Terminal
        || !graph_continuation_matches(&attempts, index)
    {
        return Ok(None);
    }
    if resolved.number.map(|number| number.get()) != Some(graph_attempt.key.attempt) {
        return Ok(None);
    }
    let launch_actor = match graph_attempt.launch_actor.parse() {
        Ok(actor) => actor,
        Err(_) => return Ok(None),
    };
    let launch_operation = match super::graph_execution::graph_operation(
        "graph-attempt-operation",
        &graph_attempt.key,
    ) {
        Ok(operation) => operation,
        Err(_) => return Ok(None),
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
        Err(()) => return Ok(None),
    };
    let reference_host = match graph_attempt.attempt_ref.as_deref() {
        Some(reference) => match parse_reference(reference).and_then(|target| target.host) {
            Some(host) => Some(host),
            None => return Ok(None),
        },
        None => None,
    };
    if explicit_placement.is_some_and(|host| Some(host) != reference_host)
        && reference_host.is_some()
    {
        return Ok(None);
    }
    let host = explicit_placement.or(reference_host).unwrap_or(local);
    let binding = LaunchBinding {
        graph_attempt,
        work_key: work_key.clone(),
        launch_actor,
        launch_operation: launch_operation.clone(),
        host,
    };
    let Some(initial) = accepted_target(factory, &owner, &binding, budget, cancel) else {
        return Ok(None);
    };
    let checkpoint_target = match graph_attempt.attempt_ref.as_deref() {
        Some(reference) => match parse_reference(reference) {
            Some(target) => Some(target),
            None => return Ok(None),
        },
        None => None,
    };
    let target_host = initial.host.unwrap_or(local);
    if host != target_host {
        return Ok(None);
    }
    if target_host != local
        && factory.context.peer_host(&target_host.to_string()) != Some(target_host)
    {
        return Ok(None);
    }
    let target = current_live_leaf(
        factory,
        &owner,
        initial,
        &binding,
        checkpoint_target.as_ref(),
        budget,
        cancel,
    );
    let Some(target) = target else {
        return Ok(None);
    };
    let reference = format!("{}/{}", target.host.unwrap_or(local), target.attempt);
    Ok(Some(VerifiedTarget {
        target,
        reference,
        work_key: binding.work_key,
        launch_actor: binding.launch_actor,
        launch_operation: binding.launch_operation,
    }))
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
    current
        .key
        .extra
        .get("previous_attempt_ref")
        .and_then(serde_json::Value::as_str)
        == expected
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
    let stable = match attempt.extra.get("placement_host_id") {
        Some(value) => {
            let host = value
                .as_str()
                .ok_or(())?
                .parse::<provenant_core::HostId>()
                .map_err(|_| ())?;
            Some(
                (host == local || factory.context.peer_host(&host.to_string()) == Some(host))
                    .then_some(host)
                    .ok_or(())?,
            )
        }
        None => None,
    };
    let alias = match attempt.extra.get("placement_host") {
        Some(value) => {
            let name = value.as_str().ok_or(())?;
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
) -> Option<AttemptTarget> {
    let local = factory.context.identity.host;
    if binding.host == local {
        let session = owner.read_session(cancel).ok()?;
        let call = owner.call(cancel);
        let operation = session
            .store
            .operation(&binding.launch_operation, &call)
            .ok()??;
        if !matches!(
            operation.state,
            provenant_run::OperationState::Accepted
                | provenant_run::OperationState::Terminal
                | provenant_run::OperationState::Blocked
        ) {
            return None;
        }
        let admitted = session
            .store
            .operation_attempt(&binding.launch_operation, &call)
            .ok()?;
        let key = binding
            .graph_attempt
            .key
            .work_key(factory.context.identity.project)
            .ok()?;
        let request = session.store.request(admitted, &call).ok()?;
        if !request.task.context.as_ref().is_some_and(|context| {
            context.graph_template_launch
                && context.graph_id.as_deref() == Some(key.graph())
                && context.node_id.as_deref() == Some(key.node())
                && context.attempt == Some(key.attempt())
        }) {
            return None;
        }
        let record = session.store.record(admitted, &call).ok()?;
        if record.key != key
            || record.project_id != factory.context.identity.project
            || record.verified_owner != Some(binding.launch_actor)
        {
            return None;
        }
        let receipt = owner
            .receipt(
                &session,
                binding.launch_operation.clone(),
                &record,
                &[],
                &call,
            )
            .ok()?;
        if receipt.operation_id != binding.launch_operation || receipt.targets.len() != 1 {
            return None;
        }
        let target = receipt.targets.into_iter().next()?;
        return (target.host == Some(local) && target.attempt == admitted).then_some(target);
    }
    let expected_graph =
        super::peer::peer_graph_name(binding.launch_actor, &binding.launch_operation).ok()?;
    let extra = [
        (
            "graph_launch_actor".into(),
            serde_json::json!(binding.launch_actor.to_string()),
        ),
        (
            "graph_origin_work_key".into(),
            serde_json::to_value(binding.work_key.clone()).ok()?,
        ),
    ]
    .into();
    match owner.peer(
        binding.host,
        PeerAction::Receipt {
            extra,
            operation_id: binding.launch_operation.clone(),
        },
        budget,
        cancel,
    ) {
        Ok(PeerResult::Submitted { data, .. })
            if data.operation_id == binding.launch_operation && data.targets.len() == 1 =>
        {
            let target = data.targets.into_iter().next()?;
            (target.host == Some(binding.host)
                && data.extra.get("peer_graph").is_some_and(|graph| {
                    graph["host"] == serde_json::json!(binding.host)
                        && graph["graph_id"] == serde_json::json!(expected_graph)
                }))
            .then_some(target)
        }
        _ => None,
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
) -> Option<AttemptTarget> {
    let mut seen = HashSet::new();
    let mut predecessor: Option<provenant_records::run::AttemptRecord> = None;
    let mut receiver_project = None;
    let mut checkpoint_target_seen = checkpoint_target.is_none();
    for _ in 0..MAX_SUPERSEDED {
        if target.host.unwrap_or(factory.context.identity.host) != binding.host
            || !seen.insert(target.attempt)
        {
            return None;
        }
        let record = owner.snapshot(target.clone(), budget, cancel).ok()?;
        if !record_matches(
            factory,
            &record,
            &target,
            binding,
            receiver_project,
            predecessor.as_ref(),
        ) {
            return None;
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
            return None;
        }
        match record.superseded_by {
            Some(next) => {
                if !matches!(record.state, AttemptState::Terminal { .. }) {
                    return None;
                }
                predecessor = Some(record);
                target.attempt = next;
            }
            None => {
                if matches!(record.state, AttemptState::Terminal { .. }) {
                    return None;
                }
                return checkpoint_target_seen.then_some(target);
            }
        }
    }
    None
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
        && super::peer::peer_origin_matches(&record.extra, &binding.work_key)
        && record.extra.get("peer_graph").is_some_and(|graph| {
            graph["host"] == serde_json::json!(binding.host)
                && graph["graph_id"].as_str() == expected_graph.as_deref()
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
