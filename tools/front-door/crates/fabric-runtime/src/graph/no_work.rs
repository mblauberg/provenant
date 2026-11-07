//! One host continuation for a writer that voluntarily stopped without a commit.
use super::*;

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
        && progress.advance_error_code.as_deref() == Some("stopped_no_work")
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
    let Some((node, _)) = plan
        .nodes
        .iter()
        .zip(&checkpoint.nodes)
        .find(|(node, progress)| eligible(node, progress, checkpoint))
    else {
        return Ok(None);
    };
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
    Ok(Some(next))
}
