//! Targets to run-store records: one checkpoint read, then `attempt_by_key`.
use super::*;
use crate::target::{self, ResolveError, Resolved, Want};
use provenant_fabric_runtime::graph::FileJournal;

/// Resolve `target` by reading only its own graph's checkpoint.
pub fn resolve_target(edge: &Edge<'_>, target: &Target, want: Want) -> Result<Resolved, Fail> {
    let context = execution_context(edge)?;
    let names = FileJournal::reserved_graph_ids(&context.state_root, context.project_id)
        .unwrap_or_default();
    target::resolve(target, want, &names, |name| {
        FileJournal::read_status_checkpoint(&context.state_root, context.project_id, name)
    })
    .map_err(resolve_fail)
}

/// The envelope for a resolution fault: its code is the fault's own.
pub fn resolve_fail<E: std::fmt::Display>(error: ResolveError<E>) -> Fail {
    let kind = match &error {
        ResolveError::NotFound { .. } => ErrorKind::NotFound,
        ResolveError::Ambiguous { .. } => ErrorKind::AmbiguousTarget,
        ResolveError::Read(_) => ErrorKind::Io,
    };
    let status = if matches!(error, ResolveError::Read(_)) {
        Status::Error
    } else {
        Status::Invalid
    };
    // `ResolveError` renders its code for readers that have no typed envelope.
    // Here the code travels in `Fail.kind` and the human renderer prints it;
    // carrying it in the message too would print `NOT_FOUND: NOT_FOUND:`.
    Fail::new(
        status,
        kind,
        crate::target::fault_message(kind.code(), &error.to_string()),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_not_found_keeps_one_code_prefix() {
        let fail = super::resolve_fail::<std::convert::Infallible>(super::ResolveError::NotFound {
            target: "missing".into(),
            candidates: Vec::new(),
        });
        assert!(!fail.message.starts_with("NOT_FOUND:"));
        assert_eq!(fail.kind.code(), "NOT_FOUND");
    }
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
            format!("{} has no local attempt", resolved.target()),
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

/// Every local attempt under `target`: a graph, a node or one attempt.
pub(super) fn local_attempts(
    session: &Session,
    resolved: &Resolved,
    call: &run::Request<'_>,
) -> Result<Vec<AttemptId>, Fail> {
    let mut attempts = std::collections::BTreeSet::new();
    let mut query = lane_query(true, 100, 0);
    loop {
        let page = session.service().lanes(&query, call)?;
        attempts.extend(
            page.lanes
                .iter()
                .filter(|record| key_matches(&record.key, resolved))
                .map(|record| record.attempt_id),
        );
        match page.next {
            Some(after) => query.after = after,
            None => break,
        }
    }
    Ok(attempts.into_iter().collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec: docs/specs/cli.md#every-name-refusal-lists-close-matches. The
    /// envelope carries the code once; the message must not repeat it.
    #[test]
    fn resolve_fail_leaves_the_typed_code_to_the_envelope() {
        let missing: ResolveError<std::convert::Infallible> = ResolveError::NotFound {
            target: "peer-abc/task#1".into(),
            candidates: Vec::new(),
        };
        let fail = resolve_fail(missing);
        assert_eq!(fail.kind, ErrorKind::NotFound);
        assert_eq!(fail.message, "peer-abc/task#1");

        let ambiguous: ResolveError<std::convert::Infallible> = ResolveError::Ambiguous {
            target: "g".into(),
            candidates: vec!["one".into(), "two".into()],
        };
        let fail = resolve_fail(ambiguous);
        assert_eq!(fail.kind, ErrorKind::AmbiguousTarget);
        assert_eq!(fail.message, "g has several nodes; use one of one, two");
    }
}
