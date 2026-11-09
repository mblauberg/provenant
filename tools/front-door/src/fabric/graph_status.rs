/// Refresh display time without querying CI or changing the durable checkpoint.
pub(super) fn graph_status_elapsed(
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    boot: &str,
    now: u64,
) {
    use provenant_fabric_runtime::graph::{CiState, NodeState};
    for node in &mut checkpoint.nodes {
        if node.state != NodeState::Running {
            continue;
        }
        let Some(ci) = &mut node.ci else {
            continue;
        };
        let Some(first) = &ci.first_observed_stamp else {
            continue;
        };
        if ci.state == CiState::Pending
            && first.boot_identity == boot
            && now >= first.host_monotonic_micros
        {
            ci.elapsed_seconds = (now - first.host_monotonic_micros) / 1_000_000;
        }
    }
}

/// Paths are retained in the journal for replay, but not in graph responses.
pub(super) fn redact_checkout_paths(
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
) {
    for node in &mut checkpoint.nodes {
        // Internal immutable dependency evidence may contain bound checkout paths.
        node.extra.remove("settled_dependency_snapshots");
        if let Some(binding) = &mut node.writer_binding {
            binding.worktree = "[redacted]".into();
            binding.extra.remove("source_directory_identity");
        }
    }
}

pub(super) fn graph_snapshot(
    checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
) -> provenant_fabric_runtime::graph::GraphRun {
    graph_snapshot_owing(checkpoint, false)
}

/// `restart_owed` marks a saved writer stop whose host restart is not yet
/// saved. That graph is still pending, so a waiter keeps polling rather than
/// returning the stopped attempt as the node's final one.
pub(super) fn graph_snapshot_owing(
    mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
    restart_owed: bool,
) -> provenant_fabric_runtime::graph::GraphRun {
    use provenant_fabric_runtime::graph::{GraphRun, GraphStatus, NodeState};
    redact_checkout_paths(&mut checkpoint);
    let unfinished = restart_owed
        || checkpoint
            .nodes
            .iter()
            .any(|node| node.expansion_pause.is_some())
        || (checkpoint.delivery_required && !checkpoint.delivered && checkpoint.successful())
        || checkpoint
            .nodes
            .iter()
            .any(|node| matches!(node.state, NodeState::Pending | NodeState::Running));
    let status = if checkpoint.cancelled {
        GraphStatus::Cancelled
    } else if !unfinished {
        // A crash can leave a pause behind after the final node was saved.
        checkpoint.pause_reason = None;
        GraphStatus::Complete
    } else if checkpoint.pause_reason.is_some() {
        GraphStatus::Paused
    } else {
        GraphStatus::Pending
    };
    let advance_error = checkpoint
        .nodes
        .iter()
        .find_map(|node| node.advance_error.clone());
    GraphRun {
        status,
        outcome: (status == GraphStatus::Complete).then(|| checkpoint.outcome()),
        checkpoint,
        advance_error,
    }
}
