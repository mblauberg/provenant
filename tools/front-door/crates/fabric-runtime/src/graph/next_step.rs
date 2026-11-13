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
    if let Some(cause) = attempt.stop {
        return Some(cause);
    }
    if declared(progress) {
        return Some(if progress.blocked {
            StopCause::Blocked
        } else {
            StopCause::StoppedEarly
        });
    }
    match attempt.outcome? {
        AttemptOutcome::AuthRequired => Some(StopCause::AuthRequired),
        AttemptOutcome::UsageLimited => Some(StopCause::UsageLimited),
        AttemptOutcome::AdapterUnavailable => Some(StopCause::AdapterUnavailable),
        AttemptOutcome::PreparationRefused(cause) => Some(cause.into()),
        AttemptOutcome::DeadlineExceeded => Some(StopCause::Deadline),
        _ => None,
    }
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

/// The stopped attempt the host reads routing facts for.
#[derive(Clone, Copy, Debug)]
pub struct DepletionRequest<'a> {
    pub key: &'a AttemptKey,
    /// The stopped attempt's reference.
    pub reference: &'a str,
    /// References of every launched attempt of the node, the stopped one included.
    pub tried: &'a [String],
    pub cause: StopCause,
    pub access: AccessMode,
}

/// What the routing depletion ladder reads, as the host observed it.
#[derive(Clone, Debug)]
pub struct DepletionFacts {
    pub catalogue: provenant_routing::Catalogue,
    pub config: provenant_routing::RoutingConfig,
    pub stopped: ladder::Stopped,
    pub readiness: Vec<ladder::ReadinessFact>,
    /// Routes the node's attempts already ran, as the host recorded them.
    pub tried: Vec<ladder::Target>,
    pub host_native: Vec<ladder::HostNativeRoute>,
    /// The standing, flag and graph exclusions the stopped attempt carried.
    pub exclusions: Vec<provenant_routing::ExclusionRule>,
    pub intent: provenant_routing::RouteIntent,
    pub confidential: bool,
    pub now: provenant_core::WallTime,
    /// `routing.depletion_wait_seconds`; `None` never parks.
    pub wait_bound_seconds: Option<u64>,
}

/// The host's routing facts for one depleted stop: `Ok(None)` when the host
/// keeps none, `Err` when it could not read them.
pub(super) type Ladder<'a> =
    dyn FnMut(DepletionRequest<'_>) -> Result<Option<DepletionFacts>, String> + 'a;

// The routing crate re-exports its depletion ladder at the root.
use provenant_routing as ladder;

/// The adapter id a host-native graph route names.
const HOST_NATIVE: &str = "host-native";

/// Where a depleted stop goes: routes to restart on, and the reset to wait
/// for before the first of them launches.
struct Successor {
    routes: Vec<GraphRoute>,
    wake: Option<provenant_core::WallTime>,
    /// What the ladder chose, when it handed the work off.
    choice: Option<patch::LadderChoice>,
}

/// The ladder key of a route the graph itself ran, when the route names one.
fn graph_target(route: &GraphRoute) -> Option<ladder::Target> {
    match (
        route.adapter.as_deref(),
        route.model.as_deref(),
        route.name.as_deref(),
    ) {
        (Some(HOST_NATIVE), _, Some(name)) => Some(ladder::Target::HostNative {
            name: name.to_owned(),
        }),
        (Some(adapter), Some(model), _) => Some(ladder::Target::Route {
            model: provenant_routing::ModelId::parse(model)
                .ok()
                .filter(|id| id.adapter() == adapter)
                .or_else(|| {
                    provenant_routing::ModelId::parse(&format!("{adapter}/{model}")).ok()
                })?,
            account: route.account.clone(),
        }),
        _ => None,
    }
}

/// The graph route a ladder target launches as.
fn target_route(target: &ladder::Target, effort: Option<String>) -> GraphRoute {
    match target {
        ladder::Target::Route { model, account } => GraphRoute {
            extra: Default::default(),
            adapter: Some(model.adapter().to_owned()),
            name: None,
            model: Some(model.model().to_owned()),
            alias: None,
            effort,
            account: account.clone(),
            tier: None,
        },
        ladder::Target::HostNative { name } => GraphRoute {
            extra: Default::default(),
            adapter: Some(HOST_NATIVE.to_owned()),
            name: Some(name.clone()),
            model: None,
            alias: None,
            effort: None,
            account: None,
            tier: None,
        },
    }
}

/// The routes that take over from a depleted stop (`usage_limited`,
/// `model_unavailable`, `auth_required`), from the routing depletion ladder.
/// A hand-off restarts on the agent it names; a wait restarts on the stopped
/// route once its quota resets, and only when `routing.depletion_wait_seconds`
/// allows it; the chair gets no route. Nothing the node already ran is offered
/// again. A host without routing facts keeps the node's declared routes.
fn depletion_successor(
    node: &PlannedNode,
    progress: &NodeProgress,
    cause: StopCause,
    ladder_facts: &mut Ladder<'_>,
) -> Successor {
    debug_assert!(cause.depleted());
    let declared = || Successor {
        routes: restart_routes(node, progress),
        wake: None,
        choice: None,
    };
    // The ladder reads readiness only for the adapters the stopped attempt
    // probed, so a chair answer may only mean it could not see another route:
    // the node's own untried routes go first, and the chair is the last resort.
    let chair = declared;
    // Without a recorded attempt the host has nothing to read facts for, so
    // the declared routes stand in, as for a host that keeps no facts.
    let (Some(attempt), Some(depletion)) = (
        progress.attempts.last(),
        ladder::DepletionCause::from_stop(cause),
    ) else {
        return declared();
    };
    let Some(reference) = attempt.attempt_ref.as_deref() else {
        return declared();
    };
    let launched: Vec<&GraphAttempt> = progress
        .recorded_attempts()
        .filter(|attempt| attempt.launched)
        .collect();
    let references: Vec<String> = launched
        .iter()
        .filter_map(|attempt| attempt.attempt_ref.clone())
        .collect();
    let mut facts = match ladder_facts(DepletionRequest {
        key: &attempt.key,
        reference,
        tried: &references,
        cause,
        access: node.access,
    }) {
        Ok(Some(facts)) => facts,
        Ok(None) => return declared(),
        Err(_) => return declared(),
    };
    if let Some(tier) = attempt.route.tier.as_ref() {
        facts.stopped.tier = Some(tier.clone());
    }
    let mut tried = facts.tried.clone();
    for target in launched
        .iter()
        .filter_map(|attempt| graph_target(&attempt.route))
    {
        if !tried.contains(&target) {
            tried.push(target);
        }
    }
    let pinned_account = node
        .attempt
        .as_ref()
        .and_then(|attempt| attempt.account.as_deref())
        .or_else(|| {
            node.routes
                .get(usize::from(attempt.key.route_index))
                .and_then(|route| route.account.as_deref())
        });
    let readiness: Vec<_> = facts
        .readiness
        .iter()
        .filter(|fact| pinned_account.is_none_or(|pin| fact.account.as_deref() == Some(pin)))
        .cloned()
        .collect();
    let host_native = if pinned_account.is_some() {
        &[][..]
    } else {
        facts.host_native.as_slice()
    };
    // A depletion does not silently spend a higher tier.
    facts
        .config
        .depletion
        .adjacent
        .retain(|step| *step != ladder::TierStep::Higher);
    let step = ladder::next_after_depletion(&ladder::DepletionInput {
        catalogue: &facts.catalogue,
        config: &facts.config,
        stopped: &facts.stopped,
        cause: depletion,
        facts: &readiness,
        tried: &tried,
        host_native,
        exclusions: &facts.exclusions,
        intent: facts.intent,
        confidential: facts.confidential,
        now: facts.now,
        wait_bound_seconds: facts.wait_bound_seconds,
    });
    match step {
        ladder::DepletionStep::Handoff(handoff) => {
            let choice = patch::LadderChoice {
                rung: handoff.rung,
                target: match &handoff.target {
                    ladder::Target::Route { model, account } => match account {
                        Some(account) => {
                            format!("{}/{} ({account})", model.adapter(), model.model())
                        }
                        None => format!("{}/{}", model.adapter(), model.model()),
                    },
                    ladder::Target::HostNative { name } => format!("{HOST_NATIVE}/{name}"),
                },
                tier: handoff.tier.clone(),
            };
            // The ladder route first; the node's untried routes stay behind it
            // as fallbacks, and a host-native route keeps a launchable one
            // after it, as the plan requires of every host-native route.
            let mut first = target_route(&handoff.target, handoff.effort);
            first.tier = handoff.tier;
            let mut routes = vec![first.clone()];
            routes.extend(
                restart_routes(node, progress)
                    .into_iter()
                    .filter(|route| *route != first)
                    .filter(|route| {
                        pinned_account.is_none_or(|pin| {
                            route
                                .account
                                .as_deref()
                                .is_none_or(|account| account == pin)
                        })
                    }),
            );
            let native = |route: &GraphRoute| route.adapter.as_deref() == Some(HOST_NATIVE);
            if routes
                .iter()
                .enumerate()
                .any(|(index, route)| native(route) && routes[index + 1..].iter().all(native))
            {
                let stopped = ladder::Target::Route {
                    model: facts.stopped.model.clone(),
                    account: facts.stopped.account.clone(),
                };
                routes.push(target_route(&stopped, facts.stopped.effort.clone()));
            }
            Successor {
                routes,
                wake: None,
                choice: Some(choice),
            }
        }
        ladder::DepletionStep::WaitUntil { unix_seconds } if facts.wait_bound_seconds.is_some() => {
            let stopped = ladder::Target::Route {
                model: facts.stopped.model.clone(),
                account: facts.stopped.account.clone(),
            };
            Successor {
                routes: vec![target_route(&stopped, facts.stopped.effort.clone())],
                wake: unix_seconds
                    .checked_mul(1_000_000)
                    .and_then(|micros| provenant_core::WallTime::from_micros(micros).ok()),
                choice: None,
            }
        }
        ladder::DepletionStep::WaitUntil { .. } | ladder::DepletionStep::Chair => chair(),
    }
}

enum Settle {
    Restart {
        routes: Vec<GraphRoute>,
        wake: Option<provenant_core::WallTime>,
        restart: patch::Restart,
    },
    Pause(String),
    Chair,
}

/// What follows this node's stop, or `None` when its failure stands: the
/// chair reads a plain failure, except after a declared stop, which pauses.
fn decide(
    node: &PlannedNode,
    progress: &NodeProgress,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
    ladder_facts: &mut Ladder<'_>,
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
    let mut wake = None;
    let mut ladder = None;
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
        NextStep::NewRoute
            if cause == StopCause::StoppedEarly
                && attempt
                    .route
                    .tier
                    .as_deref()
                    .or(attempt.route.alias.as_deref())
                    == Some("junior") =>
        {
            let mut route = attempt.route.clone();
            route.alias = Some("senior".into());
            route.tier = Some("senior".into());
            route.adapter = None;
            route.name = None;
            route.model = None;
            route.effort = None;
            vec![route]
        }
        NextStep::NewRoute if cause.depleted() => {
            let successor = depletion_successor(node, progress, cause, ladder_facts);
            wake = successor.wake;
            ladder = successor.choice;
            successor.routes
        }
        NextStep::NewRoute
            if matches!(cause, StopCause::LaneTokenLost | StopCause::ContextHandoff)
                && node.routes.len() == 1 =>
        {
            vec![attempt.route.clone()]
        }
        NextStep::NewRoute => restart_routes(node, progress),
    };
    if routes.is_empty() {
        return pause(format!(
            "node {} stopped ({cause}); no different route is available; return to the chair",
            node.id
        ))
        .or(Some(Settle::Chair));
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
        wake,
        restart: patch::Restart {
            node: node.id.clone(),
            cause,
            step,
            ladder,
        },
    })
}

fn select<'a>(
    plan: &'a PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
    ladder_facts: &mut Ladder<'_>,
) -> Option<(usize, &'a PlannedNode, Settle)> {
    plan.nodes
        .iter()
        .zip(&checkpoint.nodes)
        .enumerate()
        .find_map(|(index, (node, progress))| {
            decide(node, progress, checkpoint, limits, ladder_facts)
                .map(|settle| (index, node, settle))
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

/// The checkpoint that settles a stop, with the index of the stopped node: its
/// restart patch and reset, or a chair pause. Admission is pure and never
/// fails; the advance saves the result in the same transition as the stop, so
/// no reader sees the stop without what follows it. `ladder_facts` supplies the
/// host's routing facts when the stop is a depletion.
pub(super) fn settle_stop(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
    ladder_facts: &mut Ladder<'_>,
) -> Option<(usize, GraphCheckpoint)> {
    let (index, node, settle) = select(plan, checkpoint, limits, ladder_facts)?;
    let mut next = checkpoint.clone();
    let (routes, wake, restart) = match settle {
        Settle::Chair => {
            fence_stopped(&mut next.nodes[index]);
            return Some((index, next));
        }
        Settle::Pause(reason) => {
            plans::set_expansion_pause(&mut next, index, reason, false, None);
            return Some((index, next));
        }
        Settle::Restart {
            routes,
            wake,
            restart,
        } => (routes, wake, restart),
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
            if let Some(wake) = wake {
                // Waiting for the stopped route's reset holds the restart's
                // first launch, as a launch wait does.
                next.nodes[index].launch_retry = Some(LaunchRetry {
                    extra: Default::default(),
                    policy: LaunchRetryPolicy::default(),
                    failures: 0,
                    deadline: None,
                    wake_at: Some(wake),
                    reason: Some(format!(
                        "node {} waits for its quota to reset (routing.depletion_wait_seconds)",
                        node.id
                    )),
                });
                asks::refresh_wake(&mut next);
            }
            unblock_dependants(plan, &mut next, &node.id);
            Some((index, next))
        }
        Err(error) => Some((index, refused(plan, checkpoint, index, &error.to_string()))),
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
                && matches!(
                    progress.advance_error_code.as_deref(),
                    None | Some("upstream_blocked" | "required_input_unavailable")
                )
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
    index: usize,
    reason: &str,
) -> GraphCheckpoint {
    let mut next = checkpoint.clone();
    if let Some(node) = plan.nodes.get(index) {
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

/// Settlement fallback decision outcome.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SettlementFallback {
    Continue,
    Fail {
        code: Option<&'static str>,
        message: Option<String>,
    },
}

/// Settle an attempt outcome, evaluating whether fallback should fire or node should fail.
pub(super) struct FallbackEvidence<'a> {
    pub preparation: Option<PreparationFallbackEvidence>,
    pub unreadable: bool,
    pub review_limit_reached: bool,
    pub cause: Option<&'a provenant_records::RouteExclusionReason>,
    pub skipped_accounts: &'a [provenant_records::SkippedAccount],
}

pub(super) fn settlement_fallback_decision(
    node: &PlannedNode,
    route_index: usize,
    outcome: AttemptOutcome,
    launched: bool,
    evidence: FallbackEvidence<'_>,
) -> SettlementFallback {
    let FallbackEvidence {
        preparation: fallback_evidence,
        unreadable: evidence_unreadable,
        review_limit_reached,
        cause: fallback_cause,
        skipped_accounts,
    } = evidence;
    let route = node.routes.get(route_index);
    // Preparation evidence proves custody; the shared stop rule chooses the
    // step. A launched stop is handled by decide(), never by this path.
    let cause = match outcome {
        AttemptOutcome::AuthRequired => Some(StopCause::AuthRequired),
        AttemptOutcome::UsageLimited => Some(StopCause::UsageLimited),
        AttemptOutcome::AdapterUnavailable | AttemptOutcome::PreparationRefused(_) => {
            Some(StopCause::AdapterUnavailable)
        }
        _ => None,
    };
    let custody_released = live_fallback_allowed(outcome, launched, node.access, fallback_evidence);
    let step = cause.map(|cause| {
        cause.next_step(
            &node.on_stop,
            StopFacts {
                retries: 0,
                custody_released,
            },
        )
    });
    if custody_released && step != Some(NextStep::NewRoute) {
        return SettlementFallback::Fail {
            code: Some("on_stop_held"),
            message: Some("on_stop keeps this stop off the next route; return to the chair".into()),
        };
    }
    let allowed = step == Some(NextStep::NewRoute);
    if !allowed || route_index + 1 == node.routes.len() || review_limit_reached {
        let (mut advance_error_code, mut advance_error) = match outcome {
            AttemptOutcome::PreparationRefused(cause) => (
                Some("preparation_refused"),
                Some(bounded(cause.detail(), MAX_ADVANCE_ERROR_BYTES)),
            ),
            AttemptOutcome::AuthRequired => (
                Some("auth_required"),
                Some("The lane requires authentication.".into()),
            ),
            AttemptOutcome::AdapterUnavailable => (
                Some("adapter_unavailable"),
                Some("The lane's adapter is unavailable.".into()),
            ),
            _ => (None, None),
        };
        if outcome == AttemptOutcome::UsageLimited {
            advance_error_code = Some("usage_limited");
            let pool = route
                .and_then(|r| r.alias.as_deref())
                .map_or_else(String::new, |alias| format!(" for pool {alias}"));
            // A refusal before launch names each account it passed over: a
            // stale failure or a login is not a spent allowance.
            advance_error = Some(bounded(
                &if !launched && !skipped_accounts.is_empty() {
                    format!(
                        "No account could take the route{pool}: {}; repair or refresh an account (`provenant quota tick`) or choose an eligible route.",
                        provenant_records::SkippedAccount::describe(skipped_accounts)
                    )
                } else {
                    format!(
                        "Provider quota or rate limit reached{pool}; wait for the allowance to reset or choose an eligible route."
                    )
                },
                MAX_ADVANCE_ERROR_BYTES,
            ));
        }
        if route_index + 1 == node.routes.len()
            && launched
            && node.access == AccessMode::ReadOnly
            && route.is_some_and(|r| r.account.is_some() || r.model.is_some())
        {
            advance_error_code = Some("fallback_route_pinned");
            advance_error = Some("The selected route is pinned and has no fallback.".into());
        }
        if !allowed && route_index + 1 < node.routes.len() {
            let (code, message) = fallback_denial(
                launched,
                node.access,
                evidence_unreadable,
                outcome,
                fallback_cause,
            );
            advance_error_code = Some(code);
            advance_error = Some(message);
        }
        if review_limit_reached && route_index + 1 < node.routes.len() && allowed {
            advance_error_code = Some("review_substitution_limit");
            advance_error = Some("review substitution limit reached".into());
        }
        SettlementFallback::Fail {
            code: advance_error_code,
            message: advance_error,
        }
    } else {
        SettlementFallback::Continue
    }
}
