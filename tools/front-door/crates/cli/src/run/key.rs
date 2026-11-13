//! Targets to run-store records: one checkpoint read, then `attempt_by_key`.
use super::*;
use crate::target::{self, ResolveError, Resolved, Want};
use provenant_fabric_runtime::graph::FileJournal;

/// Resolve `target` by reading only its own graph's checkpoint.
pub fn resolve_target(edge: &Edge<'_>, target: &Target, want: Want) -> Result<Resolved, Fail> {
    let context = execution_context(edge)?;
    target::resolve(target, want, |name| {
        FileJournal::read_status_checkpoint(&context.state_root, context.project_id, name)
    })
    .map_err(resolve_fail)
}

/// The envelope for a resolution fault: its code is the fault's own.
pub fn resolve_fail<E: std::fmt::Display>(error: ResolveError<E>) -> Fail {
    let kind = match &error {
        ResolveError::NotFound(_) => ErrorKind::NotFound,
        ResolveError::Ambiguous { .. } => ErrorKind::AmbiguousTarget,
        ResolveError::Read(_) => ErrorKind::Io,
    };
    let status = if matches!(error, ResolveError::Read(_)) {
        Status::Error
    } else {
        Status::Invalid
    };
    Fail::new(status, kind, error.to_string())
}

/// The local attempt id a resolved attempt names, or `NOT_FOUND` when it ran
/// on another host or has not launched.
pub(super) fn local_attempt(
    session: &Session,
    resolved: &Resolved,
    call: &run::Request<'_>,
) -> Result<AttemptId, Fail> {
    let missing = || {
        Fail::new(
            Status::Invalid,
            ErrorKind::NotFound,
            format!("NOT_FOUND: {} has no local attempt", resolved.target()),
        )
    };
    let key = resolved
        .work_key(session.context.project_id)
        .ok_or_else(missing)?;
    match session.store.attempt_by_key(&key, call) {
        Ok(record) => Ok(record.attempt_id),
        Err(run::RunError::NotFound) => Err(missing()),
        Err(error) => Err(error.into()),
    }
}

/// Open the store writable and select the one attempt `target` names.
pub(super) fn open_attempt(
    edge: &Edge<'_>,
    target: &Target,
    want: Want,
    call: &run::Request<'_>,
) -> Result<(Session, AttemptId), Fail> {
    let resolved = resolve_target(edge, target, want)?;
    let session = open(edge, call)?;
    let id = local_attempt(&session, &resolved, call)?;
    Ok((session, id))
}

/// The run ids of every local attempt under `target`: a graph, a node or one
/// attempt. Reconcile and trace act on runs.
pub(super) fn local_runs(
    session: &Session,
    resolved: &Resolved,
    call: &run::Request<'_>,
) -> Result<Vec<RunId>, Fail> {
    let mut runs = std::collections::BTreeSet::new();
    let mut query = lane_query(true, 100, 0);
    loop {
        let page = session.service().lanes(&query, call)?;
        runs.extend(
            page.lanes
                .iter()
                .filter(|record| {
                    record
                        .key
                        .as_ref()
                        .is_some_and(|key| key_matches(key, resolved))
                })
                .map(|record| record.run_id),
        );
        match page.next {
            Some(after) => query.after = after,
            None => break,
        }
    }
    Ok(runs.into_iter().collect())
}

/// Whether `key` lies under the resolved target.
pub(super) fn key_matches(key: &provenant_records::WorkKey, resolved: &Resolved) -> bool {
    key.graph() == resolved.graph
        && resolved
            .node
            .as_deref()
            .is_none_or(|node| key.node() == node)
        && resolved
            .number
            .is_none_or(|number| key.attempt() == number.get())
}

/// Whether a settled attempt's recorded graph context lies under the target.
pub(super) fn context_matches(
    context: Option<&provenant_records::run::WorkContext>,
    resolved: &Resolved,
) -> bool {
    context.is_some_and(|context| {
        context.graph_id.as_deref() == Some(resolved.graph.as_str())
            && resolved
                .node
                .as_deref()
                .is_none_or(|node| context.node_id.as_deref() == Some(node))
            && resolved
                .number
                .is_none_or(|number| context.attempt == Some(number.get()))
    })
}
