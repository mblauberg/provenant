//! Scheduling gate and pass driver.

use super::*;

/// Control flow for the extracted scheduling pass (#3542).
pub(super) enum ScheduleExit {
    /// The pass produced a final run for the caller to return.
    Done(Box<GraphRun>),
    /// Re-enter `advance_inner` for a decision extension or a cleared pause.
    Recurse {
        resume: bool,
        extensions: Vec<String>,
        observed_ci: std::collections::BTreeSet<String>,
    },
}

/// What the schedule does after a gate node was evaluated.
enum GateStep {
    /// Continue with the next node.
    Next,
    /// The checkpoint or plan was replaced; start the pass again.
    Reschedule,
}

/// Evaluate one gate node (repair-growth replan, CI poll or condition check).
/// Its own frame keeps the replan machinery off the scheduling loop (#3542).
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn graph_ancestors(
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> std::collections::BTreeSet<usize> {
    let mut seen = std::collections::BTreeSet::new();
    let mut pending: Vec<usize> = plan.nodes[node_index]
        .dependencies()
        .filter_map(|id| positions.get(id).copied())
        .collect();
    while let Some(index) = pending.pop() {
        if seen.insert(index) {
            pending.extend(
                plan.nodes[index]
                    .dependencies()
                    .filter_map(|id| positions.get(id).copied()),
            );
        }
    }
    seen
}

fn node_depends_on(
    node_index: usize,
    ancestor: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    graph_ancestors(node_index, plan, positions).contains(&ancestor)
}

pub(super) fn same_expanded_round(
    left: Option<&loops::LoopRound>,
    right: Option<&loops::LoopRound>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.loop_id == right.loop_id && left.round == right.round,
        (None, None) => true,
        _ => false,
    }
}

fn recovery_container_members(
    root: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> std::collections::BTreeSet<usize> {
    let mut members = std::collections::BTreeSet::new();
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        let node = &plan.nodes[parent];
        for member in &node.composite_members {
            if let Some(index) = positions.get(member).copied()
                && members.insert(index)
            {
                pending.push(index);
            }
        }
        if node.loop_body.is_some() {
            for (index, child) in plan.nodes.iter().enumerate() {
                if child
                    .round
                    .as_ref()
                    .is_some_and(|round| round.loop_id == node.id)
                    && members.insert(index)
                {
                    pending.push(index);
                }
            }
        }
    }
    members
}

fn prior_clear_conflict_gate(
    node_index: usize,
    published: &str,
    head: &str,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &GraphCheckpoint,
) -> Option<usize> {
    graph_ancestors(node_index, plan, positions)
        .into_iter()
        .rev()
        .find(|index| {
            same_expanded_round(
                plan.nodes[*index].round.as_ref(),
                plan.nodes[node_index].round.as_ref(),
            ) && matches!(
                &plan.nodes[*index].condition,
                Some(GateCondition::Conflict {
                    target: CiTarget::Node { node, .. },
                    ..
                }) if node == published
            ) && checkpoint.nodes[*index].state == NodeState::Succeeded
                && checkpoint.nodes[*index]
                    .gate
                    .as_ref()
                    .is_some_and(|gate| gate.passed)
                && checkpoint.nodes[*index]
                    .conflict
                    .as_ref()
                    .is_some_and(|evidence| {
                        evidence.state == ConflictState::Clear
                            && evidence.head_sha.as_deref() == Some(head)
                    })
        })
}

fn queue_conflict_recheck_already_observed(
    node_index: usize,
    condition: &GateCondition,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &GraphCheckpoint,
) -> bool {
    let GateCondition::Ci { target, .. } = condition else {
        return false;
    };
    let CiTarget::Node {
        node: published, ..
    } = target
    else {
        return false;
    };
    let Some(published_index) = positions.get(published).copied() else {
        return false;
    };
    if plan.nodes[published_index].kind != Some(NodeKind::Publish) {
        return false;
    }
    let Some(head) = ci::published_head(target, checkpoint, true) else {
        return false;
    };
    prior_clear_conflict_gate(node_index, published, &head, plan, positions, checkpoint)
        .and_then(|index| checkpoint.nodes[index].conflict.as_ref())
        .is_some_and(|evidence| evidence.ci_rechecks > 0)
}

fn reset_skipped_conflict_branch(
    index: usize,
    checkpoint: &mut GraphCheckpoint,
) -> Result<(), &'static str> {
    let progress = &mut checkpoint.nodes[index];
    if progress.state != NodeState::Skipped || progress.skip_reason.is_some() {
        return Ok(());
    }
    if !progress.attempts.is_empty()
        || !progress.previous_attempts.is_empty()
        || progress.writer_binding.is_some()
        || progress.outputs.is_some()
        || progress.gate.is_some()
        || progress.ci.is_some()
        || progress.conflict.is_some()
        || progress.delivery_park.is_some()
    {
        return Err("skipped conflict recovery branch has execution evidence");
    }
    progress.state = NodeState::Pending;
    progress.wait_reason = None;
    progress.skip_reason = None;
    Ok(())
}

pub(super) fn preparation_exhausted_message(message: Option<String>) -> String {
    message.unwrap_or_else(|| "Preparation retries exhausted".into())
}

fn review_retryable_leaf(node: &PlannedNode) -> bool {
    node.access == AccessMode::ReadOnly
        && node.composite.is_none()
        && (node.review_lens.is_some()
            || template::review_behaviour(node) == Some(template::ReviewBehaviour::Lens)
            || node
                .role
                .as_deref()
                .is_some_and(|role| role == "lens" || role.starts_with("lens.")))
}

fn review_retry_cause(progress: &NodeProgress) -> Option<ReviewRetryCause> {
    let attempt = progress.attempts.last()?;
    if attempt.state != AttemptState::Terminal
        || !matches!(progress.state, NodeState::Failed | NodeState::Blocked)
    {
        return None;
    }
    let code = progress.advance_code.as_deref();
    if review::malformed_delivery_evidence_error(code) || code == Some("review_invariant_missing") {
        return Some(ReviewRetryCause::Evidence);
    }
    if matches!(code, Some("blocked_environment" | "model_unavailable"))
        || matches!(
            attempt.stop,
            Some(
                provenant_records::StopCause::BlockedEnvironment
                    | provenant_records::StopCause::ModelUnavailable
            )
        )
        || attempt.outcome == Some(GraphAttemptOutcome::ModelUnavailable)
    {
        return Some(ReviewRetryCause::LensUnavailable);
    }
    None
}

pub(super) fn pinned_review_head(
    node: &PlannedNode,
    progress: &NodeProgress,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> Option<String> {
    let manifest = progress
        .attempts
        .last()
        .and_then(|attempt| attempt.input_manifest.as_ref())
        .and_then(|manifest| manifest.source_commit.clone());
    let source = match node.source.as_ref() {
        Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
        Some(SourceBinding::Integration) => checkpoint.integration_pin.clone(),
        Some(SourceBinding::Node { node } | SourceBinding::Head { node }) => positions
            .get(node)
            .and_then(|index| checkpoint.nodes.get(*index))
            .and_then(|producer| {
                producer
                    .outputs
                    .as_ref()
                    .and_then(|outputs| outputs.commit.clone())
                    .or_else(|| producer.source_commit.clone())
            }),
        None => None,
    };
    progress
        .source_commit
        .clone()
        .or(manifest)
        .or(source)
        .or_else(|| {
            checkpoint
                .subject
                .as_ref()
                .map(|subject| subject.head.to_string())
        })
        .filter(|head| head.parse::<provenant_core::GitOid>().is_ok())
}

/// Resolve a later review round's exact full/delta scope from the sealed
/// policy and the current review-chain ledger.
pub(super) fn review_round_scope(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    _positions: &std::collections::BTreeMap<String, usize>,
) -> Option<review::RoundScope> {
    let node = plan.nodes.get(node_index)?;
    let round = node.round.as_ref()?;
    if round.round == 0 {
        return None;
    }
    checkpoint
        .nodes
        .get(node_index)?
        .review_round_scope
        .as_ref()
        .map(|scope| match scope {
            ReviewRoundScope::Delta { base, head } => review::RoundScope::Delta {
                base: base.clone(),
                head: head.clone(),
            },
            ReviewRoundScope::Full { head } => review::RoundScope::Full { head: head.clone() },
        })
}

pub(super) fn effective_review_policy(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
) -> ReviewPolicy {
    node.effective_review_policy
        .clone()
        .unwrap_or_else(|| checkpoint.review_policy())
}

fn effective_route_review_policy(
    node: &PlannedNode,
    route: &GraphRoute,
    checkpoint: &GraphCheckpoint,
) -> ReviewPolicy {
    route
        .effective_review_policy
        .clone()
        .or_else(|| {
            node.route
                .as_ref()
                .filter(|candidate| *candidate == route)
                .and_then(|candidate| candidate.effective_review_policy.clone())
        })
        .or_else(|| {
            node.routes
                .iter()
                .find(|candidate| *candidate == route)
                .and_then(|candidate| candidate.effective_review_policy.clone())
        })
        .or_else(|| node.effective_review_policy.clone())
        .unwrap_or_else(|| checkpoint.review_policy())
}

fn effective_adjudicator_route(
    parent: &str,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> String {
    let fallback = positions
        .get(parent)
        .and_then(|index| plan.nodes.get(*index))
        .map(|node| {
            effective_review_policy(node, checkpoint)
                .adjudicator_route
                .value
        })
        .unwrap_or_else(|| checkpoint.review_policy().adjudicator_route.value);
    let Some(parent_row) = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == parent)
    else {
        return fallback;
    };
    let mut pending = parent_row
        .children
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        let Some(progress) = checkpoint.nodes.iter().find(|progress| progress.id == id) else {
            continue;
        };
        if let Some(node_index) = positions.get(&id).copied()
            && let Some(node) = plan.nodes.get(node_index)
            && review_retryable_leaf(node)
            && let Some(route) = progress.attempts.last().map(|attempt| &attempt.route)
        {
            let policy = node
                .routes
                .iter()
                .find(|candidate| *candidate == route)
                .map(|route| effective_route_review_policy(node, route, checkpoint))
                .unwrap_or_else(|| effective_review_policy(node, checkpoint));
            return policy.adjudicator_route.value;
        }
        pending.extend(progress.children.iter().rev().cloned());
    }
    fallback
}

/// Compute the structural delta candidate. The attempt owner still has to
/// prove ancestry before this is sealed for launch.
fn candidate_review_round_scope(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> Option<review::RoundScope> {
    let node = plan.nodes.get(node_index)?;
    let round = node.round.as_ref()?;
    if round.round == 0 {
        return None;
    }
    let head = pinned_review_head(node, &checkpoint.nodes[node_index], checkpoint, positions)?;
    let chain = review::chain_owner_id(node_index, plan);
    let ledger = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == chain)
        .and_then(|progress| progress.finding_ledger.as_ref())
        .cloned()
        .or_else(|| review::initial_chain_ledger(&round.loop_id, plan, checkpoint))
        .unwrap_or_default();
    let previous_base = ledger
        .last_reviewed_head
        .as_deref()
        .and_then(|last| ledger.heads.get(last))
        .and_then(|entry| entry.dispositions.get("base"))
        .map(String::as_str);
    let graph_base = checkpoint
        .bases
        .and_then(|bases| bases.review)
        .map(|base| base.to_string());
    let current_base = checkpoint.nodes[node_index]
        .review_base
        .as_deref()
        .or(node.review_base.as_deref())
        .or(graph_base.as_deref())
        .unwrap_or("");
    let mode =
        if effective_review_policy(node, checkpoint).later_rounds.value == ReviewRounds::Delta {
            review::RoundMode::Delta
        } else {
            review::RoundMode::Full
        };
    Some(review::round_scope(
        &ledger,
        &head,
        current_base,
        previous_base,
        mode,
    ))
}

/// Resolve and persist a later-round scope before any prompt or input manifest
/// is staged. A failed Git ancestry read widens the review to the full head.
pub(super) fn seal_review_round_scope<R: AttemptOwner>(
    owner: &mut R,
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    let Some(candidate) = candidate_review_round_scope(node_index, plan, checkpoint, positions)
    else {
        return false;
    };
    let head = match &candidate {
        review::RoundScope::Delta { head, .. } | review::RoundScope::Full { head } => head.clone(),
    };
    if let Some(scope) = checkpoint.nodes[node_index].review_round_scope.as_ref() {
        let already_sealed = match (scope, &candidate) {
            (ReviewRoundScope::Full { head: sealed }, _) => sealed == &head,
            (
                ReviewRoundScope::Delta {
                    base: sealed_base,
                    head: sealed_head,
                },
                review::RoundScope::Delta { base, head },
            ) => sealed_head == head && sealed_base == base,
            _ => false,
        };
        if already_sealed {
            return false;
        }
    }
    let resolved = match candidate {
        review::RoundScope::Delta { base, head }
            if owner.review_delta_attributable(&base, &head) =>
        {
            ReviewRoundScope::Delta { base, head }
        }
        review::RoundScope::Delta { head, .. } | review::RoundScope::Full { head } => {
            ReviewRoundScope::Full { head }
        }
    };
    if checkpoint.nodes[node_index].review_round_scope.as_ref() == Some(&resolved) {
        return false;
    }
    checkpoint.nodes[node_index].review_round_scope = Some(resolved);
    true
}

/// The launch and manifest base for one attempt. Later delta rounds use the
/// last reviewed head as their base without rewriting the node's authored base.
pub(super) fn review_base_for_attempt(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> Option<String> {
    let progress = checkpoint.nodes.get(node_index)?;
    let authored = progress.review_base.clone();
    match review_round_scope(node_index, plan, checkpoint, positions) {
        Some(review::RoundScope::Delta { base, .. }) => Some(base),
        _ => authored,
    }
}

fn apply_pending_review_retry<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    worktree_layout: &provenant_config::WorktreeLayoutPort,
    expanded: &mut GraphDocument,
    plan: &mut PlannedGraph,
    positions: &mut std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
) -> Result<bool, GraphError<R::Error, J::Error>> {
    let pending = checkpoint
        .nodes
        .iter()
        .enumerate()
        .find_map(|(index, progress)| {
            progress
                .review_retry
                .as_ref()
                .and_then(|state| state.pending_retry.as_ref())
                .map(|intent| (index, progress.id.clone(), intent.clone()))
        });
    let Some((index, id, intent)) = pending else {
        return Ok(false);
    };
    let Some(node_index) = positions.get(&id).copied() else {
        return Err(GraphError::BadCheckpoint {
            node: id,
            invariant: "review retry intent must identify a planned leaf",
        });
    };
    let progress = &checkpoint.nodes[index];
    let current_routes = &plan.nodes[node_index].routes;
    let already_reset = progress.state == NodeState::Pending
        && progress.attempts.is_empty()
        && current_routes.len() == 1
        && current_routes.first() == Some(&intent.route);
    let mut candidate = checkpoint.clone();
    if already_reset {
        candidate.nodes[index]
            .review_retry
            .as_mut()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: id.clone(),
                invariant: "pending review retry must retain its state",
            })?
            .pending_retry = None;
    } else {
        let attempt_is_terminal = progress
            .attempts
            .iter()
            .chain(&progress.previous_attempts)
            .any(|attempt| {
                attempt.key == intent.attempt && attempt.state == AttemptState::Terminal
            });
        if !attempt_is_terminal {
            return Err(GraphError::BadCheckpoint {
                node: id,
                invariant: "review retry intent must name a retained terminal attempt",
            });
        }
        let library = template::TemplateLibrary::bundled();
        let result = patch::retry_review_leaf(
            &mut patch::LiveGraph {
                document,
                checkpoint: &mut candidate,
            },
            &id,
            &intent.route,
            &intent.attempt,
            &patch::PatchContext {
                catalogue: None,
                library: &library,
                resolvers: &document.template_resolvers,
                journal: None,
                limits,
                worktree_layout: worktree_layout.clone(),
            },
        )
        .map_err(|error| GraphError::Refused(error.to_string()))?;
        let _ = result;
        let mut next_expanded = plans::expand(
            document,
            &candidate.extensions,
            &candidate.expansions,
            limits,
        )
        .map_err(|error| GraphError::Invalid(error.to_string()))?;
        candidate::resolve_review_policies(&mut next_expanded, &candidate.review_policy())
            .map_err(|error| GraphError::Invalid(error.to_string()))?;
        let (next_plan, next_positions) = validate_flat_plan(&next_expanded, limits)
            .map_err(|error| GraphError::Invalid(error.to_string()))?;
        validate_expanded_rows(&candidate, &next_plan)?;
        owner
            .validate_expansion(&next_expanded, &candidate)
            .map_err(GraphError::Run)?;
        let progress_index = candidate
            .nodes
            .iter()
            .position(|progress| progress.id == id)
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: id.clone(),
                invariant: "review retry reset must retain its progress row",
            })?;
        candidate.nodes[progress_index]
            .review_retry
            .as_mut()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: id.clone(),
                invariant: "review retry reset must retain its retry state",
            })?
            .pending_retry = None;
        journal.save(&candidate).map_err(GraphError::Journal)?;
        *checkpoint = candidate;
        *expanded = next_expanded;
        *plan = next_plan;
        *positions = next_positions;
        return Ok(true);
    }
    journal.save(&candidate).map_err(GraphError::Journal)?;
    *checkpoint = candidate;
    Ok(true)
}

fn schedule_review_retry<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    worktree_layout: &provenant_config::WorktreeLayoutPort,
    expanded: &mut GraphDocument,
    plan: &mut PlannedGraph,
    positions: &mut std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
) -> Result<bool, GraphError<R::Error, J::Error>> {
    if apply_pending_review_retry(
        document,
        owner,
        journal,
        limits,
        worktree_layout,
        expanded,
        plan,
        positions,
        checkpoint,
    )? {
        return Ok(true);
    }
    for (index, node) in plan.nodes.iter().enumerate() {
        if !review_retryable_leaf(node) {
            continue;
        }
        let policy = effective_review_policy(node, checkpoint);
        let route_lens_retry = |route: &GraphRoute| {
            effective_route_review_policy(node, route, checkpoint)
                .lens_retry
                .value
        };
        let progress = &checkpoint.nodes[index];
        let Some(cause) = review_retry_cause(progress) else {
            continue;
        };
        let Some(attempt) = progress.attempts.last().cloned() else {
            continue;
        };
        let Some(head) = pinned_review_head(node, progress, checkpoint, positions) else {
            continue;
        };
        let chain = review::chain_owner_id(index, plan);
        let current_route = attempt.route.clone();
        let eligible_routes = if node.routes.is_empty() {
            vec![current_route.clone()]
        } else {
            node.routes.clone()
        };
        let routes = progress
            .review_retry
            .as_ref()
            .filter(|state| state.matches(&chain, &head) && !state.eligible_routes.is_empty())
            .map(|state| state.eligible_routes.clone())
            .unwrap_or(eligible_routes);
        let max_lens_retry = routes
            .iter()
            .map(&route_lens_retry)
            .max()
            .unwrap_or(policy.lens_retry.value);
        if !routes.contains(&current_route) {
            continue;
        }
        let mut state = progress
            .review_retry
            .as_ref()
            .filter(|state| {
                state.valid_for(&chain, &head, policy.evidence_retries.value, max_lens_retry)
            })
            .cloned()
            .unwrap_or_else(|| ReviewRetryState::new(&chain, &head, &routes));
        let retry_route = match cause {
            ReviewRetryCause::Evidence
                if state.evidence_retries_for(&current_route) < policy.evidence_retries.value =>
            {
                if state.record_evidence_retry(&current_route).is_none() {
                    continue;
                }
                current_route
            }
            ReviewRetryCause::LensUnavailable
                if state.lens_retries < route_lens_retry(&current_route) =>
            {
                let Some(next_route) = state.next_route(&current_route).cloned() else {
                    continue;
                };
                if state.record_lens_retry().is_none() {
                    continue;
                }
                next_route
            }
            _ => continue,
        };
        state.pending_retry = Some(ReviewRetryIntent {
            route: retry_route,
            cause,
            attempt: attempt.key,
        });
        checkpoint.nodes[index].review_retry = Some(state);
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        return apply_pending_review_retry(
            document,
            owner,
            journal,
            limits,
            worktree_layout,
            expanded,
            plan,
            positions,
            checkpoint,
        );
    }
    Ok(false)
}

pub(super) fn queue_conflict_recheck(
    node_index: usize,
    condition: &GateCondition,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    wall_now: Option<provenant_core::WallTime>,
) -> Result<(usize, usize), &'static str> {
    let GateCondition::Ci { target, .. } = condition else {
        return Err("queue CI conflict must target a published node");
    };
    let CiTarget::Node {
        node: published, ..
    } = target
    else {
        return Err("queue CI conflict must target a published node");
    };
    let Some(published_index) = positions.get(published).copied() else {
        return Err("queue CI conflict target is missing");
    };
    if plan.nodes[published_index].kind != Some(NodeKind::Publish) {
        return Err("queue CI conflict target is not a publication node");
    }
    let Some(head) = ci::published_head(target, checkpoint, true) else {
        return Err("queue CI conflict lost its published head");
    };
    let conflict_index =
        prior_clear_conflict_gate(node_index, published, &head, plan, positions, checkpoint)
            .ok_or("queue CI conflict has no prior clear conflict gate")?;
    let rebase = plan.nodes[conflict_index]
        .on_fail
        .as_ref()
        .and_then(|target| positions.get(target))
        .copied()
        .ok_or("queue conflict gate has no merge-base writer")?;
    let recovery_container =
        plan.nodes[rebase].composite.is_some() || plan.nodes[rebase].loop_body.is_some();
    if (!plans::writer(&plan.nodes[rebase]) && !recovery_container)
        || !same_expanded_round(
            plan.nodes[rebase].round.as_ref(),
            plan.nodes[node_index].round.as_ref(),
        )
    {
        return Err("queue conflict recovery target is not a same-round writer or container");
    }

    reset_skipped_conflict_branch(rebase, checkpoint)?;
    let contained = if recovery_container {
        recovery_container_members(rebase, plan, positions)
    } else {
        std::collections::BTreeSet::new()
    };
    for candidate in 0..plan.nodes.len() {
        let contained_member = contained.contains(&candidate);
        let same_round_descendant = same_expanded_round(
            plan.nodes[candidate].round.as_ref(),
            plan.nodes[node_index].round.as_ref(),
        ) && node_depends_on(candidate, rebase, plan, positions);
        if candidate != rebase && (contained_member || same_round_descendant) {
            reset_skipped_conflict_branch(candidate, checkpoint)?;
        }
    }

    let conflict = &mut checkpoint.nodes[conflict_index];
    let prior_conflict = conflict.conflict.as_ref();
    let ci_rechecks = prior_conflict
        .filter(|evidence| evidence.head_sha.as_deref() == Some(head.as_str()))
        .map_or(0, |evidence| evidence.ci_rechecks)
        .saturating_add(1);
    let pending_observations = prior_conflict
        .map_or(0, |evidence| evidence.pending_observations)
        .saturating_add(1);
    conflict.state = NodeState::Running;
    conflict.wait_reason = None;
    conflict.gate = None;
    conflict.outputs = None;
    conflict.conflict = Some(ci::ConflictEvidence {
        state: ConflictState::Pending,
        head_sha: Some(head),
        summary: "rechecking mergeability after CI reported conflict".into(),
        wake_at: wall_now.or_else(|| provenant_core::WallTime::from_micros(0).ok()),
        pending_observations,
        ci_rechecks,
    });

    let ci = &mut checkpoint.nodes[node_index];
    ci.state = NodeState::Pending;
    ci.wait_reason = None;
    ci.gate = None;
    ci.outputs = None;
    ci.ci = None;
    Ok((conflict_index, rebase))
}

#[allow(
    clippy::too_many_arguments,
    reason = "gate advancement coordinates graph state and journal"
)]
fn advance_gate<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    worktree_layout: &provenant_config::WorktreeLayoutPort,
    effective_budget: &crate::config::GraphBudget,
    node_index: usize,
    condition: &GateCondition,
    checkpoint: &mut GraphCheckpoint,
    expanded: &mut GraphDocument,
    plan: &mut PlannedGraph,
    positions: &mut std::collections::BTreeMap<String, usize>,
    observed_ci: &mut std::collections::BTreeSet<String>,
) -> Result<GateStep, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
    let mut replan_error = None;
    // A split verdict on one head goes to one adjudication node. The patch adds
    // no round, so the gate waits for the adjudicator instead of failing into a
    // fresh repair cycle. Only a gate downstream of the split panel admits it.
    let split = repair::split_adjudication(node, plan, checkpoint);
    if let Err(error) = &split {
        replan_error = Some((*error).to_owned());
    }
    if let Ok(Some(repair::SplitAdjudication {
        item,
        parent,
        panels,
        head,
        revision,
        existing_node_id,
    })) = split
    {
        if existing_node_id.as_ref().is_some_and(|id| {
            checkpoint
                .nodes
                .iter()
                .find(|progress| progress.id == *id)
                .is_some_and(|progress| {
                    matches!(progress.state, NodeState::Pending | NodeState::Running)
                })
        }) {
            // Let the existing adjudicator's scheduled attempt settle first;
            // its old verdict cannot be consumed by this gate while split
            // evidence remains uncovered.
            return Ok(GateStep::Next);
        }
        let library = template::TemplateLibrary::bundled();
        let resolvers = Default::default();
        let adjudicator_route = effective_adjudicator_route(&parent, plan, checkpoint, positions);
        let result = (|| -> Result<_, String> {
            let mut prospective = checkpoint.clone();
            if let Some(node_id) = existing_node_id.as_deref() {
                patch::continue_adjudicator(
                    &mut patch::LiveGraph {
                        document,
                        checkpoint: &mut prospective,
                    },
                    &item,
                    node_id,
                    &panels,
                    head.as_deref(),
                    revision,
                    limits,
                    effective_budget,
                )
                .map_err(|error| error.message)?;
            } else {
                let change = repair::split_verdict_patch(
                    &item,
                    &parent,
                    &panels,
                    head.as_deref(),
                    revision,
                    &adjudicator_route,
                );
                patch::admit_patch(
                    &mut patch::LiveGraph {
                        document,
                        checkpoint: &mut prospective,
                    },
                    Some(&parent),
                    &change,
                    &patch::PatchContext {
                        catalogue: None,
                        library: &library,
                        resolvers: &resolvers,
                        journal: None,
                        limits,
                        worktree_layout: worktree_layout.clone(),
                    },
                )
                .map_err(|error| error.message)?;
            }
            let mut preview = plans::expand(
                document,
                &prospective.extensions,
                &prospective.expansions,
                limits,
            )
            .map_err(|error| error.to_string())?;
            candidate::resolve_review_policies(&mut preview, &prospective.review_policy())
                .map_err(|error| error.to_string())?;
            let (new_plan, new_positions) = validate_flat_plan(&preview, &plans::generous())
                .map_err(|error| error.to_string())?;
            owner
                .validate_expansion(&preview, &prospective)
                .map_err(|error| error.to_string())?;
            Ok((prospective, preview, new_plan, new_positions))
        })();
        match result {
            Ok((prospective, preview, new_plan, new_positions)) => {
                journal.save(&prospective).map_err(GraphError::Journal)?;
                *checkpoint = prospective;
                *expanded = preview;
                *plan = new_plan;
                *positions = new_positions;
                return Ok(GateStep::Reschedule);
            }
            Err(error) => {
                replan_error = Some(format!("split adjudication patch refused: {error}"));
            }
        }
    }
    let reported = repair::growth_in_round(node, &*plan, &*checkpoint, &*positions);
    let mut growth = reported;
    if repair::is_repair_gate(node, &*plan, &*positions) {
        if !reported && repair::risk_assessment_missing(node, &*plan, &*checkpoint) {
            replan_error = Some(
                "cannot assess repair growth: delta reviewer omitted a valid risk tier".into(),
            );
        }
        if let Some((before, after)) = repair::repair_commits(node, &*checkpoint)
            .map(|(before, after)| (before.to_owned(), after.to_owned()))
        {
            if let Some(value) =
                repair::cached_growth(&checkpoint.nodes[node_index], &before, &after)
            {
                growth |= value;
            } else {
                match owner.detect_repair_growth(&before, &after) {
                    Ok(value) => {
                        transitions::record_repair_growth(
                            &mut checkpoint.nodes[node_index],
                            &before,
                            &after,
                            value,
                        );
                        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                        growth |= value;
                    }
                    Err(error) if !reported => {
                        replan_error = Some(format!("cannot assess repair growth: {error}"))
                    }
                    Err(_) => {}
                }
            }
        } else if document.schema_version >= 2 && !reported {
            replan_error = Some("cannot assess repair growth: pinned commits unavailable".into());
        }
    }
    if growth {
        if let Some((parent, commit)) = repair::replan_source(node, &*checkpoint)
            .map(|(parent, commit)| (parent.to_owned(), commit.to_owned()))
        {
            if repair::panel_expansion(&*checkpoint, &parent).is_none() {
                let result = (|| -> Result<_, String> {
                    commit
                        .parse::<provenant_core::GitOid>()
                        .map_err(|e| e.to_string())?;
                    let panel = owner.resolve_repair_panel(document, &commit)?;
                    let single = panel.slots.len() == 1;
                    // Repair input needs checked dispositions even for
                    // a single lens. Reuse the existing synthesis step.
                    let verifiers = if single { 0 } else { panel.verifiers };
                    let stages = when::review_stages(panel.frame && !single, verifiers > 0, true);
                    let verifier_ids: Vec<String> = (1..=verifiers.max(1))
                        .map(|index| index.to_string())
                        .collect();
                    let slots = panel.slots;
                    let mut routes = Vec::with_capacity(slots.len());
                    let mut lenses = Vec::with_capacity(slots.len());
                    for slot in slots {
                        lenses.push(slot.lens.clone());
                        let mut fields = slot
                            .route
                            .as_object()
                            .cloned()
                            .ok_or("invalid panel route")?;
                        if document.schema_version >= 2
                            && let Some(group) = slot.family_group
                        {
                            fields.insert("family_group".into(), group.into());
                        }
                        fields.insert("distinct_from".into(), "dependencies".into());
                        fields.insert("distinct_strength".into(), "prefer".into());
                        fields.insert("require_findings".into(), true.into());
                        fields.insert("required_verdict".into(), "VERDICT: PASS".into());
                        routes.push(
                            serde_json::to_string(&fields).map_err(|error| error.to_string())?,
                        );
                    }
                    let mut resolvers = std::collections::BTreeMap::from([
                        ("review-panel".into(), lenses.clone()),
                        ("review-panel-routes".into(), routes),
                        ("review-verifiers".into(), verifier_ids),
                    ]);
                    resolvers.extend(stages);
                    for lens in lenses {
                        resolvers.insert(
                            format!("review-advice-{lens}"),
                            vec![panel.advice.get(&lens).cloned().unwrap_or_default()],
                        );
                    }
                    let library = template::TemplateLibrary::bundled();
                    let context = patch::PatchContext {
                        catalogue: None,
                        library: &library,
                        resolvers: &resolvers,
                        journal: None,
                        limits,
                        worktree_layout: worktree_layout.clone(),
                    };
                    let mut prospective = checkpoint.clone();
                    let change =
                        repair::panel_patch(&parent, &commit, patch::revision(&prospective));
                    patch::admit_patch(
                        &mut patch::LiveGraph {
                            document,
                            checkpoint: &mut prospective,
                        },
                        Some(&parent),
                        &change,
                        &context,
                    )
                    .map_err(|error| error.message)?;
                    let mut preview = plans::expand(
                        document,
                        &prospective.extensions,
                        &prospective.expansions,
                        limits,
                    )
                    .map_err(|error| error.to_string())?;
                    candidate::resolve_review_policies(&mut preview, &prospective.review_policy())
                        .map_err(|error| error.to_string())?;
                    let (new_plan, new_positions) =
                        validate_flat_plan(&preview, &plans::generous())
                            .map_err(|error| error.to_string())?;
                    owner
                        .validate_expansion(&preview, &prospective)
                        .map_err(|error| error.to_string())?;
                    Ok((prospective, preview, new_plan, new_positions))
                })();
                match result {
                    Ok((prospective, preview, new_plan, new_positions)) => {
                        journal.save(&prospective).map_err(GraphError::Journal)?;
                        *checkpoint = prospective;
                        (*expanded, *plan, *positions) = (preview, new_plan, new_positions);
                        return Ok(GateStep::Reschedule);
                    }
                    Err(error) => {
                        replan_error = Some(format!("full panel replan required: {error}"))
                    }
                }
            } else if let Some(panel) = repair::panel_expansion(&*checkpoint, &parent) {
                if !repair::panel_reviews_commit(panel, &commit) {
                    replan_error = Some("full panel reviewed a different repaired commit; retry the repair container".into());
                } else if repair::panel_pending(&*checkpoint, panel) {
                    return Ok(GateStep::Next);
                } else {
                    replan_error = repair::panel_failure(&*plan, &*checkpoint, panel);
                }
            }
        } else {
            replan_error = Some("full panel replan required: repaired commit unavailable".into());
        }
    }
    let gate_was_evaluated = !growth && replan_error.is_none();
    let reason = if growth || replan_error.is_some() {
        replan_error
    } else if matches!(
        condition,
        GateCondition::Ci { .. } | GateCondition::Conflict { .. }
    ) {
        if observed_ci.contains(&node.id) {
            return Ok(GateStep::Next);
        }
        let wall_now = owner.wall_now();
        let wake_at = if matches!(condition, GateCondition::Conflict { .. }) {
            checkpoint.nodes[node_index]
                .conflict
                .as_ref()
                .and_then(|conflict| conflict.wake_at)
        } else {
            checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .and_then(|ci| ci.wake_at)
        };
        if owner.respect_ci_wake()
            && wake_at.is_some_and(|wake| wall_now.is_none_or(|now| now < wake))
        {
            return Ok(GateStep::Next);
        }
        let backoff = owner.ci_poll_backoff(&node.id);
        observed_ci.insert(node.id.clone());
        let mut evaluation = if matches!(condition, GateCondition::Conflict { .. }) {
            ci::evaluate_conflict(
                plan.queue.is_some(),
                node_index,
                node,
                &mut *checkpoint,
                owner,
                wall_now,
                backoff,
            )
        } else {
            ci::evaluate(
                plan.queue.is_some(),
                node_index,
                node,
                &mut *checkpoint,
                owner,
                wall_now,
                backoff,
            )
        }
        .map_err(GraphError::Refused)?;
        if plan.queue.is_some()
            && matches!(condition, GateCondition::Ci { .. })
            && checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .is_some_and(|evidence| evidence.state == CiState::Conflict)
            && queue_conflict_recheck_already_observed(
                node_index, condition, plan, positions, checkpoint,
            )
        {
            let timeout_seconds = match condition {
                GateCondition::Ci {
                    timeout_seconds, ..
                } => *timeout_seconds,
                _ => unreachable!(),
            };
            let timed_out = checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .is_some_and(|evidence| evidence.elapsed_seconds >= timeout_seconds);
            let evidence = checkpoint.nodes[node_index].ci.as_mut().ok_or_else(|| {
                GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "CI conflict recheck must retain its observation",
                }
            })?;
            evidence.state = CiState::Pending;
            evidence.summary = "CI still reports conflict after the exact published head was observed clear; waiting for CI to reconcile".into();
            if timed_out {
                evidence.wake_at = None;
                evaluation = ci::Evaluation::ParkUnavailable;
            } else {
                let now = wall_now.ok_or_else(|| {
                    GraphError::Refused(format!(
                        "node {}: no wall clock to set its CI conflict-reconciliation wake deadline",
                        node.id
                    ))
                })?;
                let delay_micros = backoff
                    .delay_seconds(evidence.pending_observations)
                    .saturating_mul(1_000_000);
                let wake_at = now
                    .checked_add(provenant_core::DurationMicros::from_micros(delay_micros))
                    .map_err(|_| {
                        GraphError::Refused(format!(
                            "node {}: CI conflict-reconciliation wake deadline exceeds representable time",
                            node.id
                        ))
                    })?;
                evidence.wake_at = Some(wake_at);
                evidence.pending_observations = evidence.pending_observations.saturating_add(1);
                checkpoint.nodes[node_index].state = NodeState::Running;
                evaluation = ci::Evaluation::Pending;
            }
        }
        match evaluation {
            ci::Evaluation::Pending => {
                asks::refresh_wake(&mut *checkpoint);
                journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                return Ok(GateStep::Next);
            }
            ci::Evaluation::ParkUnavailable => {
                let ci = checkpoint.nodes[node_index]
                    .ci
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "timed-out queue CI must retain its observation",
                    })?;
                let writer = loops::nearest_writer_ancestor(node_index, plan)
                    .and_then(|index| checkpoint.nodes.get(index));
                let attempt = writer.and_then(|progress| {
                    progress
                        .attempts
                        .iter()
                        .rev()
                        .find(|attempt| attempt.state == AttemptState::Terminal)
                        .cloned()
                });
                let handoff = writer
                    .and_then(|progress| {
                        progress
                            .outputs
                            .as_ref()
                            .and_then(|outputs| outputs.handoff.as_ref())
                            .or(progress.handoff.as_ref())
                    })
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|_| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "CI timeout handoff must be serializable",
                    })?
                    .unwrap_or_else(|| {
                        "required CI did not settle before its bounded timeout".into()
                    });
                let mut evidence = vec![ci.summary.clone()];
                evidence.push(format!("elapsed seconds: {}", ci.elapsed_seconds));
                evidence.extend(
                    ci.check_names
                        .iter()
                        .map(|name| format!("observed check: {name}")),
                );
                park_delivery_transition(
                    document,
                    node_index,
                    provenant_records::graph::ParkReason::CiUnavailable,
                    attempt.as_ref().map(|attempt| &attempt.key),
                    attempt
                        .as_ref()
                        .and_then(|attempt| attempt.attempt_ref.as_deref()),
                    &handoff,
                    ci.head_sha.as_deref(),
                    &evidence,
                    owner,
                    journal,
                    checkpoint,
                )?;
                asks::refresh_wake(&mut *checkpoint);
                return Ok(GateStep::Next);
            }
            ci::Evaluation::Settled(reason) => reason,
        }
    } else {
        gate_failure(
            condition,
            node,
            &*plan,
            &*checkpoint,
            &*positions,
            effective_budget,
        )
    };
    if plan.queue.is_some()
        && matches!(condition, GateCondition::Ci { .. })
        && checkpoint.nodes[node_index]
            .ci
            .as_ref()
            .is_some_and(|evidence| evidence.state == CiState::Conflict)
    {
        queue_conflict_recheck(
            node_index,
            condition,
            plan,
            positions,
            checkpoint,
            owner.wall_now(),
        )
        .map_err(|_| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "queue CI conflict must re-enter its exact published conflict gate",
        })?;
        asks::refresh_wake(checkpoint);
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
        return Ok(GateStep::Reschedule);
    }
    let exact_queue_ci_red = gate_was_evaluated
        && plan.queue.is_some()
        && reason.is_some()
        && matches!(condition, GateCondition::Ci { .. })
        && match condition {
            GateCondition::Ci { target, .. } => checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .is_some_and(|evidence| {
                    evidence.state == CiState::Failed
                        && !evidence.failed_checks.is_empty()
                        && ci::published_head(target, checkpoint, true).as_deref()
                            == evidence.head_sha.as_deref()
                }),
            _ => false,
        };
    let failed_delivery_review = gate_was_evaluated
        && plan.queue.is_some()
        && reason.is_some()
        && delivery_review_gate_failed(condition, node, plan, checkpoint, positions);
    let delivery_ci_supersession = if gate_was_evaluated
        && plan.queue.is_some()
        && reason.is_some()
        && (node.on_fail.is_some() || node.round.as_ref().is_some_and(|round| round.retry_on))
    {
        queue_conflict_supersession(condition, node_index, positions, checkpoint)
    } else {
        None
    };
    let escalation_cause = if exact_queue_ci_red {
        Some(provenant_records::graph::EscalationCause::CiRed)
    } else if failed_delivery_review {
        Some(provenant_records::graph::EscalationCause::ReviewRepair)
    } else {
        None
    };
    let escalation = escalation_cause.and_then(|cause| {
        transitions::advance_delivery_escalation(node_index, plan, checkpoint, cause)
    });
    apply_gate_result(
        &mut checkpoint.nodes[node_index],
        reason,
        node.on_fail.clone(),
        matches!(condition, GateCondition::Ci { .. }),
        matches!(condition, GateCondition::Conflict { .. })
            && ci::conflict_details_requested(plan, &node.id),
        plan.queue.is_some(),
    );
    if let Some(conflict) = checkpoint.nodes[node_index].conflict.as_mut() {
        conflict.wake_at = None;
    }
    if let Some(supersession) = &delivery_ci_supersession {
        checkpoint
            .record_delivery_ci_supersession(&supersession.node_id, &supersession.head_sha)
            .map_err(|_| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "queue conflict recovery must record its exact superseded head",
            })?;
    }
    asks::refresh_wake(&mut *checkpoint);
    let conclusive_gate_lease = if gate_was_evaluated && plan.queue.is_some() {
        match condition {
            GateCondition::Ci {
                target:
                    target @ CiTarget::Node {
                        node: published, ..
                    },
                ..
            } => {
                let expected_head = ci::published_head(target, checkpoint, true);
                checkpoint.nodes[node_index]
                    .ci
                    .as_ref()
                    .filter(|evidence| {
                        matches!(evidence.state, CiState::Passed | CiState::Failed)
                            && evidence.head_sha.as_deref().is_some_and(|head| {
                                expected_head.as_deref() == Some(head)
                                    && head.parse::<provenant_core::GitOid>().is_ok()
                            })
                    })
                    .and_then(|_| positions.get(published))
                    .and_then(|index| checkpoint.nodes.get(*index))
                    .and_then(delivery_ci_lease)
            }
            _ => None,
        }
    } else {
        None
    };
    let ci_lease_to_release = conclusive_gate_lease.or_else(|| {
        delivery_ci_supersession.as_ref().and_then(|supersession| {
            positions
                .get(&supersession.node_id)
                .and_then(|index| checkpoint.nodes.get(*index))
                .and_then(delivery_ci_lease)
                .filter(|lease| lease.head_sha == supersession.head_sha)
        })
    });
    // Persist exact conclusive or supersession evidence before releasing the
    // matching durable class reservation.
    journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    if let Some(lease) = ci_lease_to_release {
        owner.release_delivery_ci(&lease).map_err(GraphError::Run)?;
    }
    if matches!(
        escalation,
        Some(transitions::EscalationTransition::Exhausted { .. })
    ) {
        let (escalation_owner, _) =
            loops::escalation_owner(node_index, plan).ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "exhausted CI escalation must retain its owner policy",
            })?;
        let attempt_key = checkpoint.nodes[escalation_owner]
            .escalation
            .as_ref()
            .map(|state| state.transition_attempt.clone())
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "exhausted CI escalation must retain its repair identity",
            })?;
        let repair_progress = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == attempt_key.node_id);
        let attempt_ref = repair_progress.and_then(|progress| {
            progress
                .attempts
                .iter()
                .find(|attempt| attempt.key == attempt_key)
                .and_then(|attempt| attempt.attempt_ref.clone())
        });
        let handoff = repair_progress
            .and_then(|progress| {
                progress
                    .outputs
                    .as_ref()
                    .and_then(|outputs| outputs.handoff.as_ref())
                    .or(progress.handoff.as_ref())
            })
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "delivery ladder handoff must be serializable",
            })?
            .unwrap_or_else(|| "delivery repair ladder exhausted".into());
        let cause = escalation_cause.ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "exhausted escalation must retain its cause",
        })?;
        let (head, mut evidence) = match cause {
            provenant_records::graph::EscalationCause::CiRed => {
                let head = match condition {
                    GateCondition::Ci { target, .. } => {
                        ci::published_head(target, checkpoint, true)
                    }
                    _ => None,
                }
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "exhausted CI escalation must retain its published head",
                })?;
                let mut evidence = checkpoint.nodes[node_index]
                    .ci
                    .as_ref()
                    .map(|ci| vec![ci.summary.clone()])
                    .unwrap_or_default();
                if let Some(ci) = &checkpoint.nodes[node_index].ci {
                    evidence.extend(ci.failed_checks.iter().map(|check| {
                        format!(
                            "job {}: {} {} {}",
                            check.job_id.as_deref().unwrap_or("unknown"),
                            check.name,
                            check.url.as_deref().unwrap_or(""),
                            check.excerpt
                        )
                    }));
                }
                (Some(head), evidence)
            }
            provenant_records::graph::EscalationCause::ReviewRepair => {
                let head = node.inputs.iter().find_map(|input| {
                    checkpoint.nodes[positions[&input.node]]
                        .source_commit
                        .clone()
                });
                let mut evidence = vec!["delivery review did not approve the repair".into()];
                for input in &node.inputs {
                    if let Some(outputs) = checkpoint.nodes[positions[&input.node]].outputs.as_ref()
                    {
                        if let Some(verdict) = outputs.verdict.as_deref() {
                            evidence.push(format!("{} verdict: {verdict}", input.node));
                        }
                        evidence.extend(outputs.findings.iter().map(|finding| {
                            format!(
                                "{} finding {}: {} ({})",
                                input.node, finding.id, finding.summary, finding.severity
                            )
                        }));
                    }
                }
                (head, evidence)
            }
            _ => {
                return Err(GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "unsupported escalation cause reached ladder exhaustion",
                });
            }
        };
        evidence.push(format!("escalation cause: {cause:?}"));
        park_delivery_transition(
            document,
            node_index,
            provenant_records::graph::ParkReason::LadderExhausted,
            Some(&attempt_key),
            attempt_ref.as_deref(),
            &handoff,
            head.as_deref(),
            &evidence,
            owner,
            journal,
            checkpoint,
        )?;
        return Ok(GateStep::Next);
    }
    Ok(GateStep::Next)
}

/// Effect driver for one scheduling pass. Kept separate from admission and
/// final settlement so a debug build does not pool this loop's temporaries into
/// a single large frame on the graph launch path (#3542).
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
pub(super) fn advance_schedule<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    worktree_layout: provenant_config::WorktreeLayoutPort,
    admission_policy: provenant_records::policy::Policy,
    catalogue: Option<&provenant_routing::Catalogue>,
    effective_budget: crate::config::GraphBudget,
    resume_in: bool,
    checkpoint_in: GraphCheckpoint,
    expanded_in: GraphDocument,
    plan_in: PlannedGraph,
    positions_in: std::collections::BTreeMap<String, usize>,
    observed_ci_in: std::collections::BTreeSet<String>,
) -> Result<ScheduleExit, GraphError<R::Error, J::Error>> {
    let mut resume = resume_in;
    let mut checkpoint = checkpoint_in;
    let mut expanded = expanded_in;
    let mut plan = plan_in;
    let mut positions = positions_in;
    let mut observed_ci = observed_ci_in;
    let mut stopped_children = std::collections::BTreeSet::new();
    'schedule: loop {
        for node_index in 0..checkpoint.nodes.len() {
            let Some(park) = checkpoint.nodes[node_index].delivery_park.clone() else {
                continue;
            };
            park_delivery_transition(
                document,
                node_index,
                park.reason,
                park.attempt_key.as_ref(),
                park.attempt_ref.as_deref(),
                &park.handoff,
                park.head.as_deref(),
                &park.evidence,
                owner,
                journal,
                &mut checkpoint,
            )?;
        }
        if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
            continue 'schedule;
        }
        if apply_pending_review_retry(
            document,
            owner,
            journal,
            limits,
            &worktree_layout,
            &mut expanded,
            &mut plan,
            &mut positions,
            &mut checkpoint,
        )? {
            resume = false;
            continue 'schedule;
        }
        // Save the running graph's pause before any recovery patch is planned
        // against today's tighter limits.
        let structural_pause =
            transitions::refresh_structure_pause(&mut checkpoint, &expanded, &plan, limits);
        if structural_pause.is_some() {
            // Persist the durable pause before observing attempts. A resume
            // against tighter limits still owes every launched attempt its
            // settlement: returning here would leave a finished node running
            // forever. A resume under the pause admits no new launch (see
            // the guard in the attempt loop) but never stops observing work the host already
            // accepted; it survives while any node is live and clears when
            // the graph fully settles or limits are raised (#3558).
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        if structural_pause.is_none() {
            if schedule_review_retry(
                document,
                owner,
                journal,
                limits,
                &worktree_layout,
                &mut expanded,
                &mut plan,
                &mut positions,
                &mut checkpoint,
            )? {
                resume = false;
                continue 'schedule;
            }
            if let Some((next, next_expanded, next_plan, next_positions)) =
                settle_stopped_writer::<R, J>(
                    document,
                    owner,
                    &plan,
                    &checkpoint,
                    limits,
                    &worktree_layout,
                )?
            {
                // A stop saved by an earlier host, or one the chair resumed.
                journal.save(&next).map_err(GraphError::Journal)?;
                checkpoint = next;
                (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                resume = false;
                continue 'schedule;
            }
        }
        plans::remaining(&mut checkpoint, &expanded, limits);
        if checkpoint.cancelled {
            relay::fence_detached_cancellation(document, &mut checkpoint)
                .map_err(GraphError::Refused)?;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        for node in &plan.nodes {
            if (!checkpoint.cancelled
                || resume
                || checkpoint
                    .nodes
                    .iter()
                    .any(|progress| progress.id == node.id && progress.detached_from_graph_cancel))
                && checkpoint.nodes.iter().any(|n| {
                    n.id == node.id
                        && (n.handoff.is_some()
                            || n.fragment.is_some()
                            || (n.state == NodeState::Failed
                                && node
                                    .plan
                                    .as_ref()
                                    .is_some_and(|spec| spec.fallback.is_some()))
                            || (n.state == NodeState::Succeeded
                                && n.expansion_pause.is_some()
                                && node.plan.is_some()))
                })
            {
                let before_settle = checkpoint.clone();
                let settled = plans::settle(
                    node,
                    document,
                    &expanded,
                    &mut checkpoint,
                    None,
                    limits,
                    &worktree_layout,
                    &admission_policy,
                    catalogue,
                );
                if let Some((next_expanded, next_plan, next_positions)) = settled {
                    validate_expanded_rows(&checkpoint, &next_plan)?;
                    owner
                        .validate_expansion(&next_expanded, &checkpoint)
                        .map_err(GraphError::Run)?;
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                    resume = false;
                    continue 'schedule;
                }
                // A rejected retry reactivates its pause; persist it before
                // returning or considering other work.
                if checkpoint != before_settle {
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
            }
        }
        let structural_pause =
            transitions::refresh_structure_pause(&mut checkpoint, &expanded, &plan, limits);
        if structural_pause.is_some()
            || (checkpoint.pause_cause() == Some(PauseCause::StructuralLimit)
                && checkpoint.nodes.iter().any(|node| {
                    matches!(
                        node.expansion_cause,
                        Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
                    )
                }))
        {
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }

        close_resolved_asks(&mut checkpoint, owner, journal)?;
        if checkpoint.cancelled {
            if !resume {
                settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                let detached_live = checkpoint
                    .nodes
                    .iter()
                    .any(|node| !node.state.settled() && node.detached_from_graph_cancel);
                if !detached_live {
                    return Ok(ScheduleExit::Done(Box::new(GraphRun {
                        status: GraphStatus::Cancelled,
                        outcome: None,
                        checkpoint,
                        advance_note: None,
                    })));
                }
            } else {
                transitions::clear_cancellation(&mut checkpoint);
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
            }
        }

        relay::settle_subtrees(
            document,
            &mut checkpoint,
            owner,
            journal,
            &mut stopped_children,
        )?;
        for node_index in 0..plan.nodes.len() {
            if checkpoint.nodes[node_index].delivery_park.is_some() {
                continue;
            }
            if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                continue 'schedule;
            }
            if checkpoint.cancelled && !checkpoint.nodes[node_index].detached_from_graph_cancel {
                continue;
            }
            if checkpoint.nodes[node_index].subtree_cancel.is_some() {
                continue;
            }
            // A parent earlier in this wave may just have settled.
            if checkpoint
                .nodes
                .iter()
                .any(|node| node.state.settled() && !node.children_cancelled)
            {
                relay::settle_subtrees(
                    document,
                    &mut checkpoint,
                    owner,
                    journal,
                    &mut stopped_children,
                )?;
                if checkpoint.nodes[node_index].subtree_cancel.is_some() {
                    continue;
                }
            }
            let node = plan.nodes[node_index].clone();
            if node.repair_lens.is_some()
                && !checkpoint.nodes[node_index].state.settled()
                && seal_review_round_scope(owner, node_index, &plan, &mut checkpoint, &positions)
            {
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
            }
            match transitions::node_action(node_index, &plan, &mut checkpoint, &positions) {
                transitions::NodeAction::Wait { changed } => {
                    if changed {
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    }
                    continue;
                }
                transitions::NodeAction::Composite => {
                    composites::settle::<R, J>(
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        journal,
                    )?;
                    if park_malformed_composite_delivery_review(
                        document,
                        node_index,
                        &plan,
                        &positions,
                        owner,
                        journal,
                        &mut checkpoint,
                    )? {
                        continue;
                    }
                    continue;
                }
                transitions::NodeAction::Loop(spec) => {
                    let terminal_decision = checkpoint.extensions.iter().any(|e| {
                        e == &format!("loop.{}.decision=stop", node.id)
                            || e == &format!("loop.{}.decision=accept", node.id)
                    });
                    let body_settled = checkpoint
                        .nodes
                        .iter()
                        .filter(|n| n.round.as_ref().is_some_and(|r| r.loop_id == node.id))
                        .all(|n| n.state.settled());
                    if (structural_pause.is_none() || terminal_decision || body_settled)
                        && let Some(extension) = loops::settle(
                            document,
                            node_index,
                            spec,
                            &plan,
                            &positions,
                            &mut checkpoint,
                            owner,
                            journal,
                        )?
                    {
                        if let Err(error) = loops::prepare_decision_extension(
                            document,
                            &checkpoint,
                            &extension,
                            limits,
                        ) {
                            match error {
                                loops::DecisionExtensionError::Structural(limit) => {
                                    // A structural pause owns the graph; an
                                    // active expansion pause with a different
                                    // cause must yield before it is saved.
                                    checkpoint.set_graph_pause_owner(
                                        PauseCause::StructuralLimit,
                                        &limit.to_string(),
                                    );
                                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                                }
                                loops::DecisionExtensionError::Invalid(reason) => {
                                    loops::pause_invalid_decision(
                                        &mut checkpoint,
                                        &node.id,
                                        &reason,
                                        journal,
                                    )?;
                                }
                            }
                        } else {
                            return Ok(ScheduleExit::Recurse {
                                resume: true,
                                extensions: vec![extension],
                                observed_ci,
                            });
                        }
                    }
                    continue;
                }
                transitions::NodeAction::Decide(spec) => {
                    asks::decide(
                        node_index,
                        &node,
                        spec,
                        &plan,
                        &mut checkpoint,
                        owner,
                        journal,
                    )?;
                    continue;
                }
                transitions::NodeAction::Land(spec) => {
                    let wall_now = owner.wall_now();
                    let waiting = owner.respect_ci_wake()
                        && checkpoint.nodes[node_index]
                            .land
                            .as_ref()
                            .and_then(|record| record.wake_at)
                            .is_some_and(|wake| wall_now.is_none_or(|now| now < wake));
                    if structural_pause.is_none() && checkpoint.pause.is_none() && !waiting {
                        land::advance(
                            node_index,
                            &node,
                            spec,
                            &plan,
                            &positions,
                            &mut checkpoint,
                            owner,
                            journal,
                        )?;
                    }
                    continue;
                }
                transitions::NodeAction::Gate(condition) => {
                    if structural_pause.is_some() || checkpoint.pause.is_some() {
                        continue;
                    }
                    match advance_gate(
                        document,
                        owner,
                        journal,
                        limits,
                        &worktree_layout,
                        &effective_budget,
                        node_index,
                        &condition.clone(),
                        &mut checkpoint,
                        &mut expanded,
                        &mut plan,
                        &mut positions,
                        &mut observed_ci,
                    )? {
                        GateStep::Next => continue,
                        GateStep::Reschedule => continue 'schedule,
                    }
                }
                transitions::NodeAction::Hygiene => {
                    advance_hygiene(
                        document,
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        owner,
                        journal,
                    )?;
                    continue;
                }
                transitions::NodeAction::Publish => {
                    advance_publish(
                        document,
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        owner,
                        journal,
                    )?;
                    continue;
                }
                transitions::NodeAction::Attempt => {}
            }
            if checkpoint.nodes[node_index].retry.is_none()
                && let Some((retries, origin)) = owner
                    .retry_allowance(&node.extra, &plan.extra)
                    .map_err(GraphError::Run)?
            {
                transitions::record_retry_allowance(
                    &mut checkpoint.nodes[node_index],
                    retries,
                    &origin,
                );
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
            }
            let requested_review_base = (node.access == AccessMode::ReadOnly)
                .then_some(node.review_base.as_deref())
                .flatten()
                .filter(|base| !base.is_empty());
            if let Some(reference) = requested_review_base {
                let progress = &checkpoint.nodes[node_index];
                if progress.review_base.is_none() && !progress.attempts.is_empty() {
                    return Err(GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "node review base must be bound before its first attempt",
                    });
                }
                if let Some(base) = &progress.review_base {
                    if base.parse::<provenant_core::GitOid>().is_err() {
                        return Err(GraphError::BadCheckpoint {
                            node: node.id.clone(),
                            invariant: "node review base must be a full object ID",
                        });
                    }
                } else {
                    let base = owner
                        .resolve_review_base(
                            reference,
                            source_worktree(&node, &plan, &checkpoint, &positions)
                                .or(node.worktree.as_deref()),
                        )
                        .map_err(GraphError::Run)?;
                    let Some(base) =
                        base.filter(|base| base.parse::<provenant_core::GitOid>().is_ok())
                    else {
                        return Err(GraphError::Refused(format!(
                            "node {}: review base could not be bound; correct the reference and rerun",
                            node.id
                        )));
                    };
                    transitions::apply_review_base(&mut checkpoint.nodes[node_index], base);
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
            }
            loop {
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    continue 'schedule;
                }
                let (route_index, settlement_retries) = match decide_next_attempt(
                    &node,
                    &checkpoint.nodes[node_index],
                    limits,
                    document
                        .queue
                        .as_ref()
                        .map(|queue| queue.prep_retries.value),
                    owner,
                    owner.wall_now(),
                )
                .map_err(GraphError::Run)?
                {
                    NextAttempt::WaitRetryWake => break,
                    NextAttempt::WaitFallbackEvidence(retry) => {
                        checkpoint.nodes[node_index].fallback_evidence_retry = Some(retry);
                        asks::refresh_wake(&mut checkpoint);
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    NextAttempt::LaunchRetryExhausted { reason } => {
                        transitions::retry_exhausted(&mut checkpoint.nodes[node_index], &reason);
                        asks::refresh_wake(&mut checkpoint);
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    NextAttempt::Fail { code, message } => {
                        if code == Some("prep_retries_exhausted") {
                            let attempt = checkpoint.nodes[node_index]
                                .attempts
                                .last()
                                .cloned()
                                .ok_or_else(|| GraphError::BadCheckpoint {
                                    node: node.id.clone(),
                                    invariant: "preparation retry exhaustion must retain its graph attempt key",
                                })?;
                            let message = preparation_exhausted_message(message);
                            let handoff = checkpoint.nodes[node_index]
                                .outputs
                                .as_ref()
                                .and_then(|outputs| outputs.handoff.as_ref())
                                .or(checkpoint.nodes[node_index].handoff.as_ref())
                                .map(serde_json::to_string)
                                .transpose()
                                .map_err(|_| GraphError::BadCheckpoint {
                                    node: node.id.clone(),
                                    invariant: "preparation park handoff must be serializable",
                                })?
                                .unwrap_or_else(|| message.clone());
                            let head = checkpoint.nodes[node_index]
                                .outputs
                                .as_ref()
                                .and_then(|outputs| outputs.commit.as_deref())
                                .or(checkpoint.nodes[node_index].source_commit.as_deref())
                                .map(str::to_owned);
                            park_delivery_transition(
                                document,
                                node_index,
                                provenant_records::graph::ParkReason::OperationError,
                                Some(&attempt.key),
                                attempt.attempt_ref.as_deref(),
                                &handoff,
                                head.as_deref(),
                                &[message],
                                owner,
                                journal,
                                &mut checkpoint,
                            )?;
                        } else {
                            fail_node(
                                &mut checkpoint.nodes[node_index],
                                code.map(Into::into),
                                message,
                            );
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        }
                        checkpoint.nodes[node_index].fallback_evidence_retry = None;
                        asks::refresh_wake(&mut checkpoint);
                        break;
                    }
                    NextAttempt::None => break,
                    NextAttempt::Candidate {
                        route_index,
                        settlement_retries,
                    } => (route_index, settlement_retries),
                };
                let (route, key_route_index) = if let Some((route, _rung)) =
                    loops::delivery_route(node_index, &plan, &checkpoint)
                {
                    (route, 0)
                } else {
                    (
                        node.routes
                            .get(route_index)
                            .ok_or_else(|| GraphError::BadCheckpoint {
                                node: node.id.clone(),
                                invariant: "attempt route index must name a configured route",
                            })?
                            .clone(),
                        route_index,
                    )
                };
                let key = transitions::attempt_key(&plan, node_index, &checkpoint, key_route_index)
                    .map_err(|error| GraphError::Invalid(error.into()))?;

                // A resume under tighter limits admits no new launch: a saved
                // attempt with no host reference would be started by the
                // observation below, so it waits unless the host has already
                // accepted it. An attempt with a reference is only observed,
                // so finished work still settles (#3558). An ordinary advance
                // keeps its pinned behaviour of re-submitting a saved attempt.
                if resume
                    && checkpoint.pause_cause() == Some(PauseCause::StructuralLimit)
                    && let Some(last) = checkpoint.nodes[node_index].attempts.last()
                    && last.state != AttemptState::Terminal
                    && last.attempt_ref.is_none()
                    && !owner.launch_accepted(last).map_err(GraphError::Run)?
                {
                    break;
                }

                {
                    let prior = checkpoint.nodes[node_index]
                        .launch_retry
                        .as_ref()
                        .and_then(|retry| retry.deadline);
                    if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)?
                    {
                        continue 'schedule;
                    }
                    if let Some(deadline) = owner
                        .begin_launch(node.access, prior)
                        .map_err(GraphError::Run)?
                        && prior.is_none()
                    {
                        checkpoint.nodes[node_index].launch_retry = Some(LaunchRetry {
                            extra: Default::default(),
                            policy: owner.launch_retry_policy(),
                            failures: 0,
                            deadline: Some(deadline),
                            wake_at: None,
                            reason: None,
                        });
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    }
                    // Relay provisioning seals the deadline before the host owner
                    // is available to resolve the configured retry policy.
                    if checkpoint.nodes[node_index].attempts.is_empty()
                        && let Some(retry) = checkpoint.nodes[node_index].launch_retry.as_mut()
                    {
                        retry.policy = owner.launch_retry_policy();
                    }
                }

                // Expanded writers acquire their binding as soon as they exist.
                match bind_expanded_writer(
                    document,
                    owner,
                    journal,
                    node_index,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &mut resume,
                )? {
                    LoopStep::Proceed => {}
                    LoopStep::Break => break,
                    LoopStep::Reschedule => continue 'schedule,
                }
                let needs_record = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .is_none_or(|last| last.state == AttemptState::Terminal);
                let prepared_inputs = if needs_record {
                    match stage_attempt(
                        owner,
                        journal,
                        limits,
                        &effective_budget,
                        &admission_policy,
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        &key,
                        &route,
                    )? {
                        Staged::Wait => break,
                        Staged::Ready(staged) => staged,
                    }
                } else {
                    None
                };
                let input = attempt_input(
                    owner,
                    journal,
                    limits,
                    node_index,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &key,
                    prepared_inputs,
                )?;
                // The writer is the nearest writing node along the source chain,
                // so a framing node between a lens and the writer is skipped.
                let separation = attempt_separation(
                    owner,
                    journal,
                    node_index,
                    route_index,
                    needs_record,
                    &plan,
                    &positions,
                    &mut checkpoint,
                )?;
                let continued = land::continued_branch(&node, &checkpoint, &positions);
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    continue 'schedule;
                }
                let Some(observation) = observe_attempt(
                    owner,
                    journal,
                    node_index,
                    settlement_retries,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &key,
                    &route,
                    &separation,
                    &input,
                    continued.as_deref(),
                )?
                else {
                    break;
                };
                // The owner can be launching a process while a stop arrives.
                // Persist its reference before any later owner query can fail;
                // leave the intent pending until the full observation saves.
                let (settled_result, should_continue, quota_redraw) = match record_observed_attempt(
                    owner,
                    journal,
                    limits,
                    node_index,
                    route_index,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &key,
                    &route,
                    observation,
                    &separation,
                )? {
                    Observed::NextAttempt => continue,
                    Observed::Applied {
                        settled_result,
                        should_continue,
                        quota_redraw,
                    } => (settled_result, should_continue, quota_redraw),
                };
                if schedule_review_retry(
                    document,
                    owner,
                    journal,
                    limits,
                    &worktree_layout,
                    &mut expanded,
                    &mut plan,
                    &mut positions,
                    &mut checkpoint,
                )? {
                    resume = false;
                    continue 'schedule;
                }
                composites::reconcile_checkpoint(&plan, &mut checkpoint);
                let progress = &checkpoint.nodes[node_index];
                let malformed_delivery_review = plan.queue.is_some()
                    && node.delivery_review.is_some()
                    && progress.attempts.last().is_some_and(|attempt| {
                        attempt.key == key && attempt.state == AttemptState::Terminal
                    })
                    && review::malformed_delivery_evidence_error(progress.advance_code.as_deref());
                if malformed_delivery_review {
                    let progress = &checkpoint.nodes[node_index];
                    let attempt_ref = progress
                        .attempts
                        .last()
                        .filter(|attempt| attempt.key == key)
                        .and_then(|attempt| attempt.attempt_ref.clone());
                    let handoff = progress
                        .outputs
                        .as_ref()
                        .and_then(|outputs| outputs.handoff.as_ref())
                        .or(progress.handoff.as_ref())
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|_| GraphError::BadCheckpoint {
                            node: node.id.clone(),
                            invariant: "review evidence handoff must be serializable",
                        })?
                        .unwrap_or_else(|| {
                            "delivery review returned malformed or incomplete evidence".into()
                        });
                    let head = progress.source_commit.clone();
                    let mut evidence = progress
                        .advance_code
                        .iter()
                        .chain(progress.advance_note.iter())
                        .cloned()
                        .collect::<Vec<_>>();
                    if let Some(outputs) = &progress.outputs {
                        evidence.extend(outputs.warnings.iter().cloned());
                    }
                    park_delivery_transition(
                        document,
                        node_index,
                        provenant_records::graph::ParkReason::ReviewEvidence,
                        Some(&key),
                        attempt_ref.as_deref(),
                        &handoff,
                        head.as_deref(),
                        &evidence,
                        owner,
                        journal,
                        &mut checkpoint,
                    )?;
                    continue 'schedule;
                }
                if quota_redraw {
                    transitions::redraw_quota_alias(
                        document,
                        &node,
                        &mut checkpoint,
                        node_index,
                        limits,
                        &worktree_layout,
                    )
                    .map_err(GraphError::Refused)?;
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    return Ok(ScheduleExit::Recurse {
                        resume: false,
                        extensions: Vec::new(),
                        observed_ci,
                    });
                }
                let appended = if settled_result.is_some() || node.plan.is_some() {
                    plans::settle(
                        &node,
                        document,
                        &expanded,
                        &mut checkpoint,
                        settled_result.as_deref(),
                        limits,
                        &worktree_layout,
                        &admission_policy,
                        catalogue,
                    )
                } else {
                    None
                };
                if let Some((next_expanded, next_plan, _)) = &appended {
                    validate_expanded_rows(&checkpoint, next_plan)?;
                    owner
                        .validate_expansion(next_expanded, &checkpoint)
                        .map_err(GraphError::Run)?;
                }
                // Save a writer's stop in the same transition as its restart
                // or chair pause: a stop saved alone reads as settled work.
                let stopped = if appended.is_none() {
                    settle_stopped_writer::<R, J>(
                        document,
                        owner,
                        &plan,
                        &checkpoint,
                        limits,
                        &worktree_layout,
                    )?
                } else {
                    None
                };
                if let Some((next, next_expanded, next_plan, next_positions)) = stopped {
                    journal.save(&next).map_err(GraphError::Journal)?;
                    checkpoint = next;
                    (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                    resume = false;
                    continue 'schedule;
                }
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    continue 'schedule;
                }
                if let Some((next_expanded, next_plan, next_positions)) = appended {
                    (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                    resume = false;
                    continue 'schedule;
                }
                if checkpoint.nodes[node_index].advance_code.as_deref()
                    == Some("review_verdict_invalid")
                {
                    resume = false;
                    continue 'schedule;
                }
                if !should_continue {
                    break;
                }
            }
            if (!checkpoint.cancelled
                || resume
                || checkpoint.nodes[node_index].detached_from_graph_cancel)
                && checkpoint.nodes.iter().any(|progress| {
                    progress.id == plan.nodes[node_index].id
                        && progress.state == NodeState::Failed
                        && plan.nodes[node_index]
                            .plan
                            .as_ref()
                            .is_some_and(|spec| spec.fallback.is_some())
                })
                && let Some((next_expanded, next_plan, next_positions)) = plans::settle(
                    &plan.nodes[node_index],
                    document,
                    &expanded,
                    &mut checkpoint,
                    None,
                    limits,
                    &worktree_layout,
                    &admission_policy,
                    catalogue,
                )
            {
                validate_expanded_rows(&checkpoint, &next_plan)?;
                owner
                    .validate_expansion(&next_expanded, &checkpoint)
                    .map_err(GraphError::Run)?;
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                resume = false;
                continue 'schedule;
            }
        }

        relay::settle_subtrees(
            document,
            &mut checkpoint,
            owner,
            journal,
            &mut stopped_children,
        )?;
        loops::refresh_pause(&mut checkpoint);
        if transitions::clear_settled_pause(&mut checkpoint) {
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        // Close asks only after an obsolete pause is cleared, so its ask closes too.
        close_resolved_asks(&mut checkpoint, owner, journal)?;
        if asks::pause(
            document,
            &plan,
            &mut checkpoint,
            owner,
            journal,
            &effective_budget,
        )? {
            return Ok(ScheduleExit::Recurse {
                resume: false,
                extensions: Vec::new(),
                observed_ci,
            });
        }
        if document.delivery == Some(provenant_records::run::DeliveryMode::Pr)
            && !checkpoint.delivered
            && checkpoint.successful()
            && !plan
                .nodes
                .iter()
                .any(|node| node.kind == Some(NodeKind::Publish))
        {
            let url = owner
                .deliver_graph(&expanded, &checkpoint)
                .map_err(GraphError::Run)?
                .ok_or_else(|| {
                    GraphError::Refused(
                        "PR delivery returned no identity; reconcile delivery before retrying"
                            .into(),
                    )
                })?;
            transitions::record_delivery(&mut checkpoint, url).map_err(GraphError::Refused)?;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        return Ok(ScheduleExit::Done(Box::new(transitions::graph_run(
            checkpoint,
        ))));
    }
}

fn queue_conflict_supersession(
    condition: &GateCondition,
    node_index: usize,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &GraphCheckpoint,
) -> Option<DeliveryCiSupersession> {
    let GateCondition::Conflict {
        target: target @ CiTarget::Node {
            node: published, ..
        },
        ..
    } = condition
    else {
        return None;
    };
    let head_sha = ci::published_head(target, checkpoint, true)?;
    let evidence = checkpoint.nodes.get(node_index)?.conflict.as_ref()?;
    if evidence.state != ConflictState::Conflicting
        || evidence.head_sha.as_deref() != Some(head_sha.as_str())
        || head_sha.parse::<provenant_core::GitOid>().is_err()
        || !positions.contains_key(published)
    {
        return None;
    }
    Some(DeliveryCiSupersession {
        node_id: published.clone(),
        head_sha,
    })
}

fn delivery_ci_lease(progress: &NodeProgress) -> Option<DeliveryCiLease> {
    progress
        .delivery_publication
        .as_ref()
        .and_then(|publication| publication.ci_admission.as_ref())
        .and_then(DeliveryCiAdmission::lease)
        .cloned()
        .or_else(|| {
            progress
                .delivery_adoption()
                .and_then(|adoption| adoption.ci_admission)
                .and_then(|admission| admission.lease().cloned())
        })
}
