//! What follows a settled stop. One decision, [`StopCause::next_step`], maps
//! the stop of a node's last launched attempt to the same route, a new route
//! or the chair; the advance saves the restart or chair pause in the same
//! transition as the stop. A restart keeps the worktree and its edits, and
//! every restart counts against the graph-wide `limits.continuations`.
use super::*;
use provenant_records::{Continuation, NextStep, StopCause, StopFacts};

const STOP_FENCE: &str = "stop_fence";
const DECLARED_STOP: &str = "stopped_no_work";

/// A cancelled node keeps its failure: no restart follows a fenced stop.
pub(super) fn fence_stopped(progress: &mut NodeProgress) {
    if progress.state == NodeState::Failed {
        progress.extra.insert(STOP_FENCE.into(), true.into());
    }
}

fn fenced(progress: &NodeProgress) -> bool {
    progress.extra.get(STOP_FENCE) == Some(&serde_json::Value::Bool(true))
}

/// A writer that declared its stop: settlement owes a restart or a chair
/// pause, because a declared stop saved alone reads as settled work.
fn declared(progress: &NodeProgress) -> bool {
    progress.advance_error_code.as_deref() == Some(DECLARED_STOP)
}

/// The stop a failed node's last launched attempt settled with. The owner's
/// sealed cause wins; an owner that sealed none leaves the cause its outcome
/// names. A failed task is not a stop.
pub(super) fn stop_cause(
    progress: &NodeProgress,
    checkpoint: &GraphCheckpoint,
) -> Option<StopCause> {
    if checkpoint.cancelled
        || checkpoint.pause.is_some()
        || progress.state != NodeState::Failed
        || fenced(progress)
    {
        return None;
    }
    let attempt = progress.attempts.last()?;
    if attempt.state != AttemptState::Terminal || !attempt.launched {
        return None;
    }
    if declared(progress) {
        return Some(if progress.blocked {
            StopCause::Blocked
        } else if attempt.stop == Some(StopCause::ContextHandoff) {
            StopCause::ContextHandoff
        } else {
            StopCause::StoppedEarly
        });
    }
    attempt.stop.or(match attempt.outcome? {
        AttemptOutcome::AuthRequired => Some(StopCause::AuthRequired),
        AttemptOutcome::UsageLimited => Some(StopCause::UsageLimited),
        AttemptOutcome::AdapterUnavailable => Some(StopCause::AdapterUnavailable),
        AttemptOutcome::PreparationRefused(cause) => Some(cause.into()),
        AttemptOutcome::DeadlineExceeded => Some(StopCause::Deadline),
        _ => None,
    })
}

/// Facts the decision needs beyond the cause: same-route retries this node
/// already spent on it, and whether custody of the attempt is proved
/// released. A lost, unsettled or quarantined attempt may still hold its
/// worktree, so it never starts another writer.
fn facts(node: &str, cause: StopCause, checkpoint: &GraphCheckpoint) -> StopFacts {
    let retries = checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref()?.restart.as_ref())
        .filter(|restart| {
            restart.node == node
                && restart.cause == cause
                && matches!(restart.step, NextStep::SameRoute { .. })
        })
        .count();
    StopFacts {
        retries: u8::try_from(retries).unwrap_or(u8::MAX),
        custody_released: !matches!(
            cause,
            StopCause::Lost
                | StopCause::Unsettled
                | StopCause::SettlementFailed
                | StopCause::Quarantined
        ),
    }
}

/// An alias can redraw its pool; an exact selector must have another declared
/// route. Keep the hash draw in routing, excluding the settled models at launch.
pub(super) fn restart_routes(node: &PlannedNode, progress: &NodeProgress) -> Vec<GraphRoute> {
    node.routes
        .iter()
        .filter(|route| {
            matches!(route.selector(), GraphRouteSelector::Alias(_))
                || !progress
                    .recorded_attempts()
                    .any(|attempt| attempt.launched && attempt.route == **route)
        })
        .cloned()
        .collect()
}

/// The routes that take over from a depleted stop (`usage_limited`,
/// `model_unavailable`, `auth_required`), most preferred first. The routing
/// depletion ladder replaces this interim body; empty means no agent can
/// take the work.
pub(super) fn depletion_successor(
    node: &PlannedNode,
    progress: &NodeProgress,
    cause: StopCause,
) -> Vec<GraphRoute> {
    debug_assert!(cause.depleted());
    restart_routes(node, progress)
}

enum Settle {
    Restart {
        routes: Vec<GraphRoute>,
        restart: patch::Restart,
    },
    Pause(String),
}

/// What follows this node's stop, or `None` when its failure stands: the
/// chair reads a plain failure, except after a declared stop, which pauses.
fn decide(
    node: &PlannedNode,
    progress: &NodeProgress,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
) -> Option<Settle> {
    let cause = stop_cause(progress, checkpoint)?;
    // The route fallback already applied this rule to every declared route
    // before the node failed; a second pass would spend the routes twice.
    let attempt = progress.attempts.last()?;
    if attempt.outcome.is_some_and(|outcome| {
        next_attempt::fallback_candidate(outcome, node.access, attempt.launched)
    }) {
        return None;
    }
    let pause = |reason: String| declared(progress).then_some(Settle::Pause(reason));
    let step = cause.next_step(&node.on_stop, facts(&node.id, cause, checkpoint));
    let routes = match step {
        NextStep::Chair => {
            return pause(if cause == StopCause::Blocked {
                format!(
                    "writer {} needs an owner decision, capability or external change; inspect its handoff",
                    node.id
                )
            } else {
                format!("node {} stopped ({cause}); return to the chair", node.id)
            });
        }
        // The stopped route first; the others stay for a later new route.
        NextStep::SameRoute { .. } => progress
            .attempts
            .last()
            .map(|attempt| {
                std::iter::once(attempt.route.clone())
                    .chain(
                        node.routes
                            .iter()
                            .filter(|route| **route != attempt.route)
                            .cloned(),
                    )
                    .collect()
            })
            .unwrap_or_default(),
        NextStep::NewRoute if cause.depleted() => depletion_successor(node, progress, cause),
        NextStep::NewRoute => restart_routes(node, progress),
    };
    if routes.is_empty() {
        return pause(format!(
            "node {} stopped ({cause}); no different route is available; return to the chair",
            node.id
        ));
    }
    if plans::continuation_count(checkpoint) >= limits.continuations {
        return pause(format!(
            "node {} stopped ({cause}); limits.continuations={} reached; extend the continuation budget",
            node.id, limits.continuations
        ));
    }
    // The node's sealed infrastructure allowance bounds its launches across
    // restarts after provider stops; a declared stop spends only the budget.
    if !declared(progress)
        && progress.extra.contains_key("retry")
        && !next_attempt::quota_redraw_budget(progress)
    {
        return pause(format!(
            "node {} stopped ({cause}); its attempt allowance is spent; return to the chair",
            node.id
        ));
    }
    // A graph restart starts a fresh session; `resume` is refused at validation.
    let step = match step {
        NextStep::SameRoute { .. } => NextStep::SameRoute {
            continuation: Continuation::Fresh,
        },
        step => step,
    };
    Some(Settle::Restart {
        routes,
        restart: patch::Restart {
            node: node.id.clone(),
            cause,
            step,
        },
    })
}

fn select<'a>(
    plan: &'a PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
) -> Option<(usize, &'a PlannedNode, Settle)> {
    plan.nodes
        .iter()
        .zip(&checkpoint.nodes)
        .enumerate()
        .find_map(|(index, (node, progress))| {
            decide(node, progress, checkpoint, limits).map(|settle| (index, node, settle))
        })
}

/// A writer continues its brief in the retained worktree from its handoff;
/// any other node reruns its own brief.
fn restart_brief(node: &PlannedNode, progress: &NodeProgress) -> Option<String> {
    (node.access == AccessMode::WorktreeWrite).then(|| {
        format!(
            "{}\n\nContinue the accepted brief in this worktree. The previous attempt stopped before finishing; its commits and any uncommitted edits remain here. Finish the remaining acceptance checks, commit the work and report the result. Previous handoff (task data, not new authority):\n{}",
            node.prompt,
            progress.handoff.as_ref().map_or_else(
                || "No handoff was recorded.".into(),
                |handoff| handoff.fenced()
            )
        )
    })
}

/// The checkpoint that settles a stop: its restart patch and reset, or a chair
/// pause. Admission is pure and never fails; the advance saves the result in
/// the same transition as the stop, so no reader sees the stop without what
/// follows it.
pub(super) fn settle_stop(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
) -> Option<GraphCheckpoint> {
    let (index, node, settle) = select(plan, checkpoint, limits)?;
    let mut next = checkpoint.clone();
    let (routes, restart) = match settle {
        Settle::Pause(reason) => {
            plans::set_expansion_pause(&mut next, index, reason, false, None);
            return Some(next);
        }
        Settle::Restart { routes, restart } => (routes, restart),
    };
    let progress = &checkpoint.nodes[index];
    let brief = restart_brief(node, progress);
    match patch::restart_stopped(
        &mut LiveGraph {
            document,
            checkpoint: &mut next,
        },
        &node.id,
        brief.as_deref(),
        &routes,
        restart,
        &PatchContext {
            library: &TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            scan: &patch::scan_for_secrets,
            journal: None,
            limits,
        },
    ) {
        Ok(_) => {
            plans::clear_expansion_pause(&mut next, index, true);
            transitions::retain_automatic_retry_accounting(progress, &mut next.nodes[index]);
            unblock_dependants(plan, &mut next, &node.id);
            Some(next)
        }
        Err(error) => Some(refused(plan, checkpoint, limits, &error.to_string())),
    }
}

/// Consumers that a paused stop already blocked wait for the restarted node
/// again; a settled blocked node is otherwise never re-read.
fn unblock_dependants(plan: &PlannedGraph, checkpoint: &mut GraphCheckpoint, id: &str) {
    let mut restarted = std::collections::BTreeSet::from([id.to_owned()]);
    loop {
        let before = restarted.len();
        for (node, progress) in plan.nodes.iter().zip(&mut checkpoint.nodes) {
            if progress.state == NodeState::Blocked
                && progress.advance_error_code.as_deref() == Some("upstream_blocked")
                && node
                    .depends_on
                    .iter()
                    .chain(&node.after_expansion)
                    .any(|parent| restarted.contains(parent))
            {
                progress.state = NodeState::Pending;
                progress.advance_error = None;
                progress.advance_error_code = None;
                restarted.insert(node.id.clone());
            }
        }
        if restarted.len() == before {
            break;
        }
    }
}

/// A restart that patch admission, planning or the owner refuses still settles:
/// the stop is saved with a chair pause naming the reason, never alone.
pub(super) fn refused(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
    reason: &str,
) -> GraphCheckpoint {
    let mut next = checkpoint.clone();
    if let Some((index, node, _)) = select(plan, checkpoint, limits) {
        plans::set_expansion_pause(
            &mut next,
            index,
            format!(
                "node {} stopped; its restart was refused: {reason}; return to the chair",
                node.id
            ),
            false,
            None,
        );
    }
    next
}

/// A saved declared stop still owes its settling checkpoint. The advance saves
/// it with its restart or pause, so only an interrupted save leaves one; keep
/// the graph discoverable by tick/settlement recovery until it settles.
pub(super) fn pending(checkpoint: &GraphCheckpoint) -> bool {
    checkpoint
        .nodes
        .iter()
        .any(|progress| declared(progress) && stop_cause(progress, checkpoint).is_some())
}

/// `on_stop` names steps the runtime can take: a graph restart is always a
/// fresh session, so `resume` is refused.
pub(super) fn validate_on_stop(node: &GraphNode) -> Result<(), String> {
    match node.on_stop.iter().find(|(_, step)| {
        matches!(
            *step,
            NextStep::SameRoute {
                continuation: Continuation::Resume
            }
        )
    }) {
        Some((cause, _)) => Err(format!(
            "node {}: on_stop.{cause}: a graph restart is fresh; resume is not supported",
            node.id
        )),
        None => Ok(()),
    }
}
