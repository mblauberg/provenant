//! One host continuation for a writer that voluntarily stopped without a commit.
use super::*;

const STOP_FENCE: &str = "no_work_stop_fence";

pub(super) fn fence_stopped(progress: &mut NodeProgress) {
    if progress.state == NodeState::Failed
        && progress.advance_error_code.as_deref() == Some("stopped_no_work")
    {
        progress.extra.insert(STOP_FENCE.into(), true.into());
    }
}

fn unfenced_stop(progress: &NodeProgress) -> bool {
    progress.advance_error_code.as_deref() == Some("stopped_no_work")
        && progress.extra.get(STOP_FENCE) != Some(&serde_json::Value::Bool(true))
}

pub(super) fn eligible(
    node: &PlannedNode,
    progress: &NodeProgress,
    checkpoint: &GraphCheckpoint,
) -> bool {
    !checkpoint.cancelled
        && checkpoint.pause_reason.is_none()
        && node.access == AccessMode::WorktreeWrite
        && node.auto_continue_no_work.unwrap_or(true)
        && progress.state == NodeState::Failed
        && unfenced_stop(progress)
        && !checkpoint
            .expansions
            .iter()
            .filter_map(|expansion| expansion.patch.as_ref())
            .any(|record| {
                record.reason.as_deref() == Some(patch::no_work_reason(&node.id).as_str())
            })
}

/// Reconsider saved settlements after a crash. Admission is pure; the caller
/// saves the patch and reset together before launching the new attempt.
pub(super) fn continue_stopped(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
) -> Result<Option<GraphCheckpoint>, PatchError> {
    let Some((index, (node, progress))) = plan
        .nodes
        .iter()
        .zip(&checkpoint.nodes)
        .enumerate()
        .find(|(_, (node, progress))| eligible(node, progress, checkpoint))
    else {
        return Ok(None);
    };
    if plans::continuation_count(checkpoint) >= limits.continuations {
        let mut next = checkpoint.clone();
        plans::set_expansion_pause(
            &mut next,
            index,
            format!(
                "writer {} stopped with no work; limits.continuations={} reached; extend the continuation budget",
                node.id, limits.continuations
            ),
            false,
            None,
        );
        return Ok(Some(next));
    }
    let brief = format!(
        "{}\n\nContinue the accepted brief in this worktree. The previous attempt stopped with no work and no new commit. Finish the remaining acceptance checks, commit the work and report the result. This is the only automatic continuation.",
        node.prompt
    );
    let mut next = checkpoint.clone();
    patch::continue_no_work(
        &mut LiveGraph {
            document,
            checkpoint: &mut next,
        },
        &node.id,
        &brief,
        &PatchContext {
            library: &TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            scan: &patch::scan_for_secrets,
            journal: None,
            limits,
        },
    )?;
    plans::clear_expansion_pause(&mut next, index, true);
    transitions::retain_automatic_retry_accounting(progress, &mut next.nodes[index]);
    Ok(Some(next))
}

/// A stopped writer still owes a continuation checkpoint. Keep it discoverable
/// by ordinary tick/settlement recovery if a save fails or the host crashes.
pub(super) fn pending(document: &GraphDocument, checkpoint: &GraphCheckpoint) -> bool {
    if checkpoint.cancelled
        || checkpoint.pause_reason.is_some()
        || !checkpoint.nodes.iter().any(unfenced_stop)
    {
        return false;
    }
    let limits = plans::generous();
    let expanded = plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &limits,
    );
    expanded
        .and_then(|expanded| {
            validate_flat_plan(&expanded, &limits).map_err(|error| error.to_string())
        })
        .map(|(plan, _)| {
            plan.nodes
                .iter()
                .zip(&checkpoint.nodes)
                .any(|(node, row)| eligible(node, row, checkpoint))
        })
        .unwrap_or(true)
}
