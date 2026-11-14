use super::{GraphCheckpoint, GraphDocument, GraphJournal, JournalError};
use crate::graph::journal::{CancellationTarget, FileJournal, graph_directory};
use provenant_core::{CancelToken, Clock, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, io::ErrorKind};

const MAX_REQUEST_BYTES: u64 = super::MAX_CHECKPOINT_BYTES;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestSet {
    schema_version: u8,
    requests: Vec<CancellationRequest>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancellationRequest {
    id: String,
    graph_id: String,
    plan_digest: String,
    instance_id: Option<String>,
    node: Option<String>,
    reason: Option<String>,
}

impl Default for RequestSet {
    fn default() -> Self {
        Self {
            schema_version: 1,
            requests: Vec::new(),
        }
    }
}

fn lock_name(graph_id: &str) -> String {
    format!("graph-{graph_id}.cancel-control.lock")
}

fn requests_name(graph_id: &str) -> String {
    format!("graph-{graph_id}.cancel-requests.json")
}

fn read(directory: &Directory, graph_id: &str) -> Result<RequestSet, JournalError> {
    let name = requests_name(graph_id);
    let bytes = match directory.read_stable(OsStr::new(&name), MAX_REQUEST_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(RequestSet::default()),
        Err(error) => return Err(error.into()),
    };
    let set: RequestSet = serde_json::from_slice(&bytes)
        .map_err(|error| JournalError::Io(std::io::Error::new(ErrorKind::InvalidData, error)))?;
    if set.schema_version != 1 {
        return Err(JournalError::TooLarge);
    }
    Ok(set)
}

fn write(directory: &Directory, graph_id: &str, set: &RequestSet) -> Result<(), JournalError> {
    let name = requests_name(graph_id);
    if set.requests.is_empty() {
        match directory.unlink(OsStr::new(&name)) {
            Ok(()) => directory
                .sync()
                .map_err(JournalError::DurabilityUnconfirmed)?,
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    let bytes = super::CanonicalV1::bytes(set)?;
    if bytes.as_bytes().len() as u64 > MAX_REQUEST_BYTES {
        return Err(JournalError::TooLarge);
    }
    directory
        .atomic_write(
            OsStr::new(&name),
            ".cancel-requests.",
            bytes.as_bytes(),
            PublishMode::Replace,
        )?
        .require_durable()
        .map_err(JournalError::DurabilityUnconfirmed)
}

/// Persist a request without acquiring the graph's journal lock. The independent
/// control lock is held only while the bounded sidecar is read and replaced.
pub(super) fn record(
    root: &StateRoot,
    project: ProjectId,
    target: CancellationTarget<'_>,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<String, JournalError> {
    let CancellationTarget {
        graph_id,
        node,
        reason,
    } = target;
    let checkpoint = FileJournal::read_checkpoint(root, project, graph_id)?
        .ok_or(JournalError::GraphMismatch)?;
    if let Some(node) = node
        && !checkpoint
            .nodes
            .iter()
            .any(|candidate| candidate.id == node)
    {
        return Err(JournalError::GraphMismatch);
    }
    let directory = graph_directory(root, project)?;
    let _control = directory.lock(
        OsStr::new(&lock_name(graph_id)),
        LockMode::Exclusive,
        budget,
        cancel,
        clock,
    )?;
    let mut set = read(&directory, graph_id)?;
    set.requests.retain(|request| {
        request.graph_id == graph_id
            && request.plan_digest == checkpoint.plan_digest
            && request.instance_id == checkpoint.instance_id
            && request
                .node
                .as_ref()
                .is_none_or(|node| checkpoint.nodes.iter().any(|entry| &entry.id == node))
    });
    if let Some(existing) = set.requests.iter_mut().find(|request| {
        request.node.as_deref() == node
            && request.plan_digest == checkpoint.plan_digest
            && request.instance_id == checkpoint.instance_id
    }) {
        if let Some(reason) = reason {
            existing.reason = Some(crate::hosts::plain_text(reason, 1024));
        }
        let id = existing.id.clone();
        write(&directory, graph_id, &set)?;
        return Ok(id);
    }
    if set.requests.len() >= checkpoint.nodes.len().saturating_add(1) {
        return Err(JournalError::TooLarge);
    }
    let id = provenant_core::hex(&provenant_os::fs::entropy_128()?);
    set.requests.push(CancellationRequest {
        id: id.clone(),
        graph_id: graph_id.to_owned(),
        plan_digest: checkpoint.plan_digest,
        instance_id: checkpoint.instance_id,
        node: node.map(str::to_owned),
        reason: reason.map(|reason| crate::hosts::plain_text(reason, 1024)),
    });
    write(&directory, graph_id, &set)?;
    Ok(id)
}

pub(super) fn pending(
    root: &StateRoot,
    project: ProjectId,
    graph_id: &str,
    request_id: &str,
) -> Result<bool, JournalError> {
    let directory = graph_directory(root, project)?;
    let _control = match directory.try_lock(OsStr::new(&lock_name(graph_id)), LockMode::Exclusive) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => {
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(read(&directory, graph_id)?
        .requests
        .iter()
        .any(|request| request.id == request_id))
}

/// Apply matching requests and remove them only after the checkpoint save is
/// durable. The caller already holds the graph lock; this control lock is never
/// held while a provider or other external owner is called.
pub(super) fn acknowledge(
    journal: &mut FileJournal,
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
) -> Result<bool, JournalError> {
    let _control = match journal.directory.try_lock(
        OsStr::new(&lock_name(&journal.graph_id)),
        LockMode::Exclusive,
    ) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => {
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    let mut set = read(&journal.directory, &journal.graph_id)?;
    if set.requests.is_empty() {
        return Ok(false);
    }
    let mut candidate = checkpoint.clone();
    let mut acknowledged = Vec::new();
    let mut stale = Vec::new();
    for request in &set.requests {
        if request.graph_id != journal.graph_id
            || request.plan_digest != checkpoint.plan_digest
            || request.instance_id != checkpoint.instance_id
        {
            stale.push(request.id.clone());
            continue;
        }
        if let Some(node) = &request.node {
            if !candidate.nodes.iter().any(|progress| progress.id == *node) {
                stale.push(request.id.clone());
                continue;
            }
            // A missing dynamic parent means the target was retired before this
            // request reached the graph lock. Do not apply it to another node.
            if super::cancel_node_subtree(document, &mut candidate, node, request.reason.as_deref())
                .is_err()
            {
                stale.push(request.id.clone());
                continue;
            }
        } else {
            if !candidate.cancelled || request.reason.is_some() {
                candidate.cancel_reason = request.reason.clone();
            }
            candidate.cancelled = true;
        }
        acknowledged.push(request.id.clone());
    }

    if candidate != *checkpoint {
        journal.save(&candidate)?;
        *checkpoint = candidate;
    }
    if !acknowledged.is_empty() || !stale.is_empty() {
        let consumed: std::collections::BTreeSet<_> =
            acknowledged.iter().chain(&stale).cloned().collect();
        set.requests
            .retain(|request| !consumed.contains(&request.id));
        if set.requests.len() > checkpoint.nodes.len().saturating_add(1) {
            return Err(JournalError::TooLarge);
        }
        write(&journal.directory, &journal.graph_id, &set)?;
    }
    Ok(!acknowledged.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_intent_survives_graph_lock_and_is_acknowledged_after_save() {
        use crate::graph::GraphExecution;
        use provenant_core::ProjectId;
        use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
        use std::os::unix::fs::PermissionsExt;

        let temp = crate::test_support::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/graph-checkpoint.json")).unwrap();
        let execution: GraphExecution =
            serde_json::from_value(fixture["execution"].clone()).unwrap();
        let checkpoint: GraphCheckpoint =
            serde_json::from_value(fixture["checkpoint"].clone()).unwrap();
        let mut journal =
            FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
        journal.bind(execution).unwrap();
        journal.save(&checkpoint).unwrap();

        let request = record(
            &root,
            project,
            CancellationTarget {
                graph_id: "g-test",
                node: None,
                reason: Some("stop requested"),
            },
            budget,
            &cancel,
            &clock,
        )
        .unwrap();
        assert!(!request.is_empty());
        let execution = FileJournal::execution(&root, project, "g-test")
            .unwrap()
            .unwrap();
        let mut checkpoint = journal.load("g-test").unwrap().unwrap();
        assert!(acknowledge(&mut journal, &execution.document, &mut checkpoint).unwrap());
        assert!(checkpoint.cancelled);
        assert_eq!(checkpoint.cancel_reason.as_deref(), Some("stop requested"));
        assert!(!acknowledge(&mut journal, &execution.document, &mut checkpoint).unwrap());
        assert!(
            !FileJournal::cancellation_request_pending(&root, project, "g-test", &request).unwrap()
        );
        assert_eq!(
            FileJournal::read_checkpoint(&root, project, "g-test").unwrap(),
            Some(checkpoint)
        );
    }
}
