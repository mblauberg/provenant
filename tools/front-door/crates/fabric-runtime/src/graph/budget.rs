use super::*;

pub(super) fn document_limit_notes(document: &GraphDocument) -> Vec<String> {
    if document.limits.is_some() {
        vec!["graph document limits are ignored; host graph limits apply".to_owned()]
    } else {
        Vec::new()
    }
}

pub(super) fn structural_limit_reason(
    document: &GraphDocument,
    plan: &PlannedGraph,
    limits: &crate::config::GraphLimits,
) -> Option<String> {
    let check = |key: &str, actual: usize, max: usize| limits.bound(key, max).refusal(actual);
    check("nodes", document.nodes.len(), limits.nodes)
        .or_else(|| {
            check(
                "edges",
                document
                    .nodes
                    .iter()
                    .map(|node| node.dependencies().count())
                    .sum(),
                limits.edges,
            )
        })
        .or_else(|| {
            document.nodes.iter().find_map(|node| {
                check(
                    "routes_per_node",
                    node.routes
                        .as_ref()
                        .map_or(usize::from(node.route.is_some()), Vec::len),
                    limits.routes_per_node,
                )
            })
        })
        .or_else(|| {
            document.nodes.iter().find_map(|node| {
                check(
                    "node_prompt_bytes",
                    node.content_bytes(),
                    limits.node_prompt_bytes,
                )
            })
        })
        .or_else(|| {
            check(
                "graph_prompt_bytes",
                document.nodes.iter().map(GraphNode::content_bytes).sum(),
                limits.graph_prompt_bytes,
            )
        })
        .or_else(|| {
            let mut children = std::collections::BTreeMap::<&str, usize>::new();
            for node in &plan.nodes {
                for parent in node.dependencies() {
                    *children.entry(parent).or_default() += 1;
                }
            }
            children
                .values()
                .find_map(|width| check("fan_out_width", *width, limits.fan_out_width))
        })
        .or_else(|| {
            let mut depths = std::collections::BTreeMap::<&str, usize>::new();
            for node in &plan.nodes {
                let depth = node
                    .dependencies()
                    .map(|parent| depths[parent.as_str()] + 1)
                    .max()
                    .unwrap_or(0);
                if let Some(reason) = check("expansion_depth", depth, limits.expansion_depth) {
                    return Some(reason);
                }
                depths.insert(&node.id, depth);
            }
            None
        })
}

#[derive(Debug, Error)]
#[error("{message}")]
pub(super) struct BudgetExtensionError {
    code: &'static str,
    message: String,
}

impl BudgetExtensionError {
    pub(super) fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }

    pub(super) fn code(&self) -> &'static str {
        self.code
    }
}

pub(super) fn apply_extension(
    budget: &mut crate::config::GraphBudget,
    extension: &str,
    replay: bool,
) -> Result<(), BudgetExtensionError> {
    let (key, value) = extension.split_once('=').ok_or_else(|| {
        BudgetExtensionError::new(
            "budget_extension_syntax",
            format!("{extension}: expected KEY=VALUE"),
        )
    })?;
    let key = key.strip_prefix("budget.").unwrap_or(key);
    let (scope, field) = key.split_once('.').ok_or_else(|| {
        BudgetExtensionError::new(
            "budget_extension_key",
            format!("{extension}: unknown budget key"),
        )
    })?;
    let target = match scope {
        "per_node" => &mut budget.per_node,
        "per_graph" => &mut budget.per_graph,
        _ => {
            return Err(BudgetExtensionError::new(
                "budget_extension_key",
                format!("{extension}: unknown budget key"),
            ));
        }
    };
    match field {
        "tokens" | "wall_time_seconds" => {
            let number: u64 = value.parse().map_err(|_| {
                BudgetExtensionError::new(
                    "budget_extension_value",
                    format!("{extension}: invalid budget value"),
                )
            })?;
            let slot = if field == "tokens" {
                &mut target.tokens
            } else {
                &mut target.wall_time_seconds
            };
            if number == 0 {
                return Err(BudgetExtensionError::new(
                    "budget_extension_value",
                    format!("{extension}: budget must be positive"),
                ));
            }
            if let Some(old) = slot.as_mut() {
                *old = (*old).max(number);
            } else if !replay {
                return Err(BudgetExtensionError::new(
                    "budget_extension_unconfigured",
                    format!("{key} is not configured"),
                ));
            }
        }
        "cost_usd" => {
            let number: f64 = value.parse().map_err(|_| {
                BudgetExtensionError::new(
                    "budget_extension_value",
                    format!("{extension}: invalid budget value"),
                )
            })?;
            if !number.is_finite() || number <= 0.0 {
                return Err(BudgetExtensionError::new(
                    "budget_extension_value",
                    format!("{extension}: budget must be finite and positive"),
                ));
            }
            if let Some(old) = target.cost_usd.as_mut() {
                *old = old.max(number);
            } else if !replay {
                return Err(BudgetExtensionError::new(
                    "budget_extension_unconfigured",
                    format!("{key} is not configured"),
                ));
            }
        }
        _ => {
            return Err(BudgetExtensionError::new(
                "budget_extension_key",
                format!("{extension}: unknown budget key"),
            ));
        }
    }
    Ok(())
}

pub(super) fn budget_reason(
    checkpoint: &GraphCheckpoint,
    budget: &crate::config::GraphBudget,
) -> Option<String> {
    let mut graph_tokens = 0u64;
    let mut graph_cost = 0u64;
    let mut graph_seconds = 0u64;
    let mut graph_unknown = [false; 3];
    for node in &checkpoint.nodes {
        let mut tokens = 0u64;
        let mut cost = 0u64;
        let mut seconds = 0u64;
        let mut unknown = [false; 3];
        for attempt in node
            .attempts
            .iter()
            .filter(|attempt| attempt.state == AttemptState::Terminal && attempt.launched)
        {
            let usage = attempt.usage.as_ref();
            let values = [
                usage.and_then(|value| value.tokens),
                usage.and_then(|value| value.cost_micro_usd),
                usage.and_then(|value| value.wall_time_seconds),
            ];
            for (missing, value) in unknown.iter_mut().zip(values) {
                *missing |= value.is_none();
            }
            unknown[0] |= usage.is_some_and(|value| value.tokens_partial);
            unknown[1] |= usage.is_some_and(|value| value.cost_partial);
            tokens = tokens.saturating_add(values[0].unwrap_or(0));
            cost = cost.saturating_add(values[1].unwrap_or(0));
            seconds = seconds.saturating_add(values[2].unwrap_or(0));
        }
        graph_tokens = graph_tokens.saturating_add(tokens);
        graph_cost = graph_cost.saturating_add(cost);
        graph_seconds = graph_seconds.saturating_add(seconds);
        for (total, missing) in graph_unknown.iter_mut().zip(unknown) {
            *total |= missing;
        }
        if let Some(reason) =
            exceeded("per_node", &budget.per_node, tokens, cost, seconds).or_else(|| {
                unknown_budget_reason("per_node", &budget.per_node, unknown, budget.unknown_usage)
            })
        {
            return Some(format!("node {}: {reason}", node.id));
        }
    }
    exceeded(
        "per_graph",
        &budget.per_graph,
        graph_tokens,
        graph_cost,
        graph_seconds,
    )
    .or_else(|| {
        unknown_budget_reason(
            "per_graph",
            &budget.per_graph,
            graph_unknown,
            budget.unknown_usage,
        )
    })
}

pub(super) const USAGE_METRICS: [&str; 3] = ["tokens", "cost_usd", "wall_time_seconds"];

pub(super) fn unknown_metrics(attempt: &GraphAttempt) -> impl Iterator<Item = &'static str> + '_ {
    let usage = attempt.usage.as_ref();
    let counted = attempt.state == AttemptState::Terminal && attempt.launched;
    USAGE_METRICS.into_iter().filter(move |name| {
        counted
            && !usage.is_some_and(|value| match *name {
                "tokens" => value.tokens.is_some() && !value.tokens_partial,
                "cost_usd" => value.cost_micro_usd.is_some() && !value.cost_partial,
                _ => value.wall_time_seconds.is_some(),
            })
    })
}

/// Give a node's newly settled attempt the next settlement order.
pub(super) fn stamp_settlement(checkpoint: &mut GraphCheckpoint, node_index: usize) {
    let next = checkpoint.settlements.saturating_add(1);
    if let Some(attempt) = checkpoint.nodes[node_index].attempts.last_mut()
        && attempt.settled == 0
    {
        attempt.settled = next;
        checkpoint.settlements = next;
    }
}

/// Rebuild the unknown-usage lines in settlement order, keeping the newest
/// `MAX_UNKNOWN_USAGE`.
pub(super) fn update_usage_warnings(checkpoint: &mut GraphCheckpoint) {
    let mut settled: Vec<_> = checkpoint
        .nodes
        .iter()
        .flat_map(|node| node.attempts.iter().map(move |attempt| (node, attempt)))
        .collect();
    settled.sort_by_key(|(_, attempt)| attempt.settled);
    let mut warnings = Vec::new();
    for (node, attempt) in settled {
        for name in unknown_metrics(attempt) {
            warnings.push(format!(
                "node {} route {}: {name} usage unknown",
                node.id, attempt.key.route_index
            ));
        }
    }
    let omitted = warnings.len().saturating_sub(MAX_UNKNOWN_USAGE);
    warnings.drain(..omitted);
    checkpoint.unknown_usage = warnings;
    checkpoint.unknown_usage_omitted = omitted;
}

/// Record one usage event once, dropping the oldest beyond `MAX_USAGE_EVENTS`.
pub(super) fn push_usage_event(checkpoint: &mut GraphCheckpoint, event: UnknownUsageEvent) {
    if checkpoint.usage_events.contains(&event) {
        return;
    }
    checkpoint.usage_events.push(event);
    let omitted = checkpoint
        .usage_events
        .len()
        .saturating_sub(MAX_USAGE_EVENTS);
    checkpoint.usage_events.drain(..omitted);
    checkpoint.usage_events_omitted = checkpoint
        .usage_events_omitted
        .saturating_add(omitted as u64);
}

/// Events for an attempt that just settled: host telemetry and cost failures,
/// then each metric the settled usage leaves unknown.
pub(super) fn record_usage_events(
    checkpoint: &mut GraphCheckpoint,
    node_index: usize,
    usage_error: Option<String>,
    cost_error: Option<String>,
) {
    let Some(attempt) = checkpoint.nodes[node_index].attempts.last() else {
        return;
    };
    let node_id = checkpoint.nodes[node_index].id.clone();
    let route_index = attempt.key.route_index;
    let event = |metric: &str, reason: Option<String>| UnknownUsageEvent {
        extra: Default::default(),
        node_id: node_id.clone(),
        route_index,
        metric: metric.to_owned(),
        reason,
    };
    let mut events = Vec::new();
    if let Some(reason) = usage_error {
        events.push(event("telemetry", Some(reason)));
    }
    if let Some(reason) = cost_error {
        events.push(event("cost_usd", Some(reason)));
    }
    for name in unknown_metrics(attempt) {
        let reason = (name == "cost_usd")
            .then(|| {
                attempt
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.cost_unknown_reason.clone())
            })
            .flatten();
        events.push(event(name, reason));
    }
    for event in events {
        push_usage_event(checkpoint, event);
    }
}

pub(super) fn unknown_budget_reason(
    scope: &str,
    limit: &crate::config::BudgetLimit,
    unknown: [bool; 3],
    policy: crate::config::UnknownUsage,
) -> Option<String> {
    if policy != crate::config::UnknownUsage::Pause {
        return None;
    }
    [
        ("tokens", limit.tokens.is_some()),
        ("cost_usd", limit.cost_usd.is_some()),
        ("wall_time_seconds", limit.wall_time_seconds.is_some()),
    ]
    .into_iter()
    .zip(unknown)
    .find_map(|((name, configured), missing)| {
        (configured && missing).then(|| format!("{scope}.{name} usage unknown"))
    })
}

pub(super) fn exceeded(
    scope: &str,
    limit: &crate::config::BudgetLimit,
    tokens: u64,
    cost: u64,
    seconds: u64,
) -> Option<String> {
    if let Some(max) = limit.tokens.filter(|max| tokens >= *max) {
        return Some(format!("{scope}.tokens budget {max} reached ({tokens})"));
    }
    if let Some(max) = limit
        .cost_usd
        .filter(|max| (cost as f64) / 1_000_000.0 >= *max)
    {
        return Some(format!("{scope}.cost_usd budget {max} reached"));
    }
    if let Some(max) = limit.wall_time_seconds.filter(|max| seconds >= *max) {
        return Some(format!(
            "{scope}.wall_time_seconds budget {max} reached ({seconds})"
        ));
    }
    None
}
