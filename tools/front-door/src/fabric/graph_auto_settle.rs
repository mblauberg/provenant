//! Settle a finished graph whose worktree was removed after its nodes settled.
//!
//! A tick that finds the execution worktree gone normally warns and leaves the
//! graph pending forever. When every node has settled, there is no work left to
//! resume: settle the graph from its durable checkpoint instead. A live writer
//! (presence or custody lease) still fences this path, so a detached lane is
//! never settled underneath itself.

/// Recorded when a finished graph is settled because its worktree is gone.
pub(super) const AUTO_SETTLE_REASON: &str = "worktree removed after settlement";

/// Settle `checkpoint` in place when it is finished and has no live holder.
/// Returns whether the graph was settled. The caller holds the graph journal.
pub(super) fn settle_removed_worktree(
    state: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    id: &str,
    checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
    journal: &mut provenant_fabric_runtime::graph::FileJournal,
    clock: &dyn provenant_core::Clock,
    cancel: &provenant_core::CancelToken,
) -> Result<bool, Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{GraphJournal, GraphOutcome};

    if checkpoint.nodes.iter().any(|node| !node.state.settled()) {
        return Ok(false);
    }
    if live_holder(state, project, id, checkpoint, clock, cancel)? {
        return Ok(false);
    }

    let succeeded = checkpoint.outcome() == GraphOutcome::Succeeded;
    checkpoint.pause = None;
    checkpoint.wake_at = None;
    // Open asks and queued steers can keep the pending marker alive after the
    // work itself is done; neither can be answered without the worktree.
    checkpoint.asks.clear();
    checkpoint.pending_steers = Default::default();
    if succeeded {
        checkpoint.cancelled = false;
        checkpoint.cancel_reason = None;
    } else {
        // There is no `Stopped` graph status: a stopped graph is a cancelled
        // checkpoint whose reason records why it ended.
        checkpoint.cancelled = true;
        checkpoint.cancel_reason = Some(AUTO_SETTLE_REASON.to_owned());
    }
    // Saving a settled checkpoint also retires the pending marker, so later
    // ticks stop revisiting a worktree that no longer exists.
    journal.save(checkpoint)?;
    provenant_fabric_runtime::graph::FileJournal::set_advance_error(state, project, id, None)?;
    Ok(true)
}

/// A graph is fenced while it holds a lease or an observable live writer.
/// Unknown process evidence alone does not claim liveness: the worktree is
/// already gone, so an unobservable writer has nothing left to advance.
fn live_holder(
    state: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    id: &str,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    clock: &dyn provenant_core::Clock,
    cancel: &provenant_core::CancelToken,
) -> Result<bool, Box<dyn std::error::Error>> {
    use provenant_core::AttemptId;
    use provenant_fabric_store::{HostStore, Request as StoreRequest};
    use provenant_records::custody::{CustodyState, Subject, SubjectKind};

    let store_call = StoreRequest { clock, cancel };
    #[allow(clippy::disallowed_methods)] // Native read-only store open deadline.
    let ledger = HostStore::open_read_only(
        state,
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    )?;
    if let Some(ledger) = ledger {
        let subject = Subject::new(SubjectKind::Graph, id);
        if let Some(item) = ledger.custody_item(project, &subject, &store_call)?
            && item.state == CustodyState::Active
        {
            return Ok(true);
        }
    }

    let run_call = provenant_run::Request { clock, cancel };
    let context = provenant_run::ExecutionContext {
        state_root: state.clone(),
        project_id: project,
        actor_id: provenant_core::ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let Some(store) = provenant_run::RunStore::open_read_only(&context, &run_call)? else {
        return Ok(false);
    };
    for attempt in checkpoint.nodes.iter().flat_map(|node| {
        node.attempts
            .iter()
            .chain(node.previous_attempts.iter())
            .filter_map(|attempt| attempt.attempt_ref.as_deref())
    }) {
        let Ok(attempt) = AttemptId::parse(attempt) else {
            continue;
        };
        let Ok(record) = store.record(attempt, &run_call) else {
            continue;
        };
        if matches!(
            provenant_run::attempt_presence(state.as_path(), &record),
            Some(provenant_run::AttemptPresence::Live)
        ) || matches!(store.custody_released(&record, &run_call), Ok(false))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
