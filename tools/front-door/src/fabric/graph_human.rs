#[cfg(test)]
#[allow(dead_code, reason = "the custom worktree driver omits libtest functions")]
mod test_presentation {
    use super::*;
pub(crate) fn graph_human(run: &provenant_fabric_runtime::graph::GraphRun) -> String {
    graph_human_view(run, false)
}

pub(crate) fn graph_human_full(run: &provenant_fabric_runtime::graph::GraphRun) -> String {
    graph_human_view(run, true)
}

fn graph_human_view(run: &provenant_fabric_runtime::graph::GraphRun, full: bool) -> String {
    let waits = provenant_fabric_runtime::graph::graph_waits(&run.checkpoint);
    let members: std::collections::BTreeSet<_> = run
        .checkpoint
        .nodes
        .iter()
        .flat_map(|node| node.children.iter())
        .collect();
    let mut human = format!("Graph {}: {:?}\n", run.checkpoint.graph_id, run.status);
    if full
        && (run.checkpoint.bound_review_base.is_some()
            || run.checkpoint.bound_integration_base.is_some())
    {
        human.push_str(&format!(
            "Graph bases: review={} integration={}\n",
            run.checkpoint
                .bound_review_base
                .as_deref()
                .unwrap_or("unavailable"),
            run.checkpoint
                .bound_integration_base
                .as_deref()
                .unwrap_or("unavailable")
        ));
    }
    if let Some(outcome) = run.outcome {
        human.push_str(&format!("Outcome: {outcome:?}\n"));
    }
    if run.status == provenant_fabric_runtime::graph::GraphStatus::Paused
        && let Some(reason) = &run.checkpoint.pause_reason
    {
        human.push_str(&format!("Pause reason: {}\n", bounded_reason(reason)));
    }
    if run.status == provenant_fabric_runtime::graph::GraphStatus::Cancelled
        && let Some(reason) = &run.checkpoint.cancel_reason
    {
        human.push_str(&format!(
            "Cancellation reason: {}\n",
            bounded_reason(reason)
        ));
    }
    for node in &run.checkpoint.nodes {
        let substituted = node
            .attempts
            .iter()
            .any(|attempt| attempt.review_substitution);
        let waiting_ci = node.ci.as_ref().filter(|ci| {
            node.state == provenant_fabric_runtime::graph::NodeState::Running
                && ci.state == provenant_fabric_runtime::graph::CiState::Pending
        });
        if !full
            && (members.contains(&node.id) || node.round.is_some())
            && waiting_ci.is_none()
            && !substituted
            && !matches!(
                node.state,
                provenant_fabric_runtime::graph::NodeState::Running
                    | provenant_fabric_runtime::graph::NodeState::Blocked
                    | provenant_fabric_runtime::graph::NodeState::Failed
            )
        {
            continue;
        }
        human.push_str(&format!("  {}: {:?}", node.id, node.state));
        if substituted {
            human.push_str(" (review substitute)");
        }
        if let Some(reason) = &node.skip_reason {
            human.push_str(&format!(" ({})", bounded_reason(reason)));
        }
        if !full
            && node.state == provenant_fabric_runtime::graph::NodeState::Blocked
            && let Some(error) = &node.advance_error
        {
            human.push_str(&format!(" ({})", bounded_reason(error)));
        }
        if full {
            if let Some(retry) = node.extra.get("retry") {
                human.push_str(&format!(
                    " settlement_retries={} origin={}",
                    retry["retries"],
                    retry["origin"].as_str().unwrap_or("unknown")
                ));
            }
            if let Some(attempt) = node.attempts.last() {
                let route = &attempt.route;
                for (name, value) in [
                    ("adapter", &route.adapter),
                    ("name", &route.name),
                    ("model", &route.model),
                    ("alias", &route.alias),
                    ("effort", &route.effort),
                    ("account", &route.account),
                ] {
                    if let Some(value) = value {
                        human.push_str(&format!(" {name}={value}"));
                    }
                }
                human.push_str(&format!(
                    " attempt={}",
                    attempt.attempt_ref.as_deref().unwrap_or("none")
                ));
                if let Some(outcome) = attempt.outcome {
                    human.push_str(&format!(" outcome={outcome:?}"));
                }
            } else {
                human.push_str(" route=none attempt=none");
            }
            if let Some(error) = &node.advance_error {
                human.push_str(&format!(" error={error}"));
            }
        }
        if !full
            && node.state == provenant_fabric_runtime::graph::NodeState::Failed
            && let Some(outcome) = node.attempts.last().and_then(|attempt| attempt.outcome)
        {
            human.push_str(&format!(" outcome={outcome:?}"));
        }
        if let Some(ci) = waiting_ci {
            human.push_str(&format!(
                " {}",
                graph_ci_waiting(
                    ci.elapsed_seconds,
                    ci.check_names.iter().map(String::as_str),
                    ci.head_sha.as_deref(),
                )
            ));
        }
        if !node.children.is_empty() {
            human.push_str(&format!(" ({} members)", node.children.len()));
        }
        if let Some(head) = &node.source_commit {
            human.push_str(&format!(" source={head}"));
        } else if node.source_binding.is_some() {
            human.push_str(" source=pending");
        }
        if let Some(base) = &node.review_base {
            human.push_str(&format!(" review_base={base}"));
        }
        if let Some(wait) = waits.iter().find(|wait| wait.node == node.id) {
            human.push_str(&format!(
                " wait={} disposition={} code={} current_attempt={} route={} failures={} limit={} limit_source={} wake_at={} next={}",
                wait.wait, wait.disposition, bounded_reason(&wait.code), bounded_reason(&wait.attempt),
                wait.route, wait.failures, wait.limit, bounded_reason(&wait.limit_source),
                bounded_reason(&wait.wake_at), bounded_reason(&wait.next)
            ));
        }
        human.push('\n');
        if full {
            for attempt in &node.attempts {
                if let Some(manifest) = &attempt.input_manifest {
                    let route = manifest.applied_route.as_ref();
                    human.push_str(&format!(
                        "    attempt route_index={} state={:?} outcome={:?} applied_adapter={} applied_model={} applied_effort={} applied_account={} source={} review_base={} graph_review_base={} integration_base={} integration_source={} input_limit={} input_limit_source={} omitted={}\n",
                        attempt.key.route_index, attempt.state, attempt.outcome,
                        route.and_then(|route| route.adapter.as_deref()).unwrap_or("unavailable"),
                        route.and_then(|route| route.model.as_deref()).unwrap_or("unavailable"),
                        route.and_then(|route| route.effort.as_deref()).unwrap_or("unavailable"),
                        route.and_then(|route| route.account.as_deref()).unwrap_or("unavailable"),
                        manifest.source_commit.as_deref().unwrap_or("unavailable"),
                        manifest.review_base.as_deref().unwrap_or("unavailable"),
                        manifest.graph_review_base.as_deref().unwrap_or("unavailable"),
                        manifest.integration_base.as_deref().unwrap_or("unavailable"),
                        manifest.integration_source.as_deref().unwrap_or("unavailable"),
                        attempt.input_cap.map_or_else(|| "unavailable".into(), |cap| cap.to_string()),
                        attempt.input_cap_source.map_or("unavailable", |source| match source {
                            provenant_fabric_runtime::graph::InputCapSource::EffectiveGraphLimits => "effective_graph_limits",
                            provenant_fabric_runtime::graph::InputCapSource::Default => "default",
                            provenant_fabric_runtime::graph::InputCapSource::Configured => "configured",
                        }),
                        manifest.omitted,
                    ));
                    if let Some(producer) = &manifest.source_producer {
                        human.push_str(&format!(
                            "      source_producer graph={} node={} attempt={}\n",
                            producer.graph_id, producer.node_id, producer.route_index
                        ));
                    }
                    if let Some(head) = &manifest.writer_head {
                        human.push_str(&format!("      writer_initial_head={head}\n"));
                    }
                    match &manifest.dependencies {
                        Some(dependencies) => human.push_str(&format!(
                            "      dependencies={} omitted={}\n",
                            dependencies.join(","),
                            manifest.dependencies_omitted
                        )),
                        None => human.push_str("      dependencies=unavailable\n"),
                    }
                    for input in &manifest.inputs {
                        let producer = input.producer_attempt.as_ref();
                        human.push_str(&format!(
                            "      selected_input node={} graph={} attempt={} kind={:?} required={} digest={} commit={} delivery={:?} file={:?}\n",
                            input.producer_node,
                            producer.map_or("unavailable", |key| key.graph_id.as_str()),
                            producer.map_or_else(|| "unavailable".into(), |key| key.route_index.to_string()),
                            input.kind, input.required,
                            input.producer_result_digest.as_deref().unwrap_or("unavailable"),
                            input.commit.as_deref().unwrap_or("unavailable"),
                            input.delivery, input.file_delivery,
                        ));
                    }
                }
            }
            if let Some(outputs) = &node.outputs {
                human.push_str(&format!(
                    "    outputs digest={} commit={} verdict={}\n",
                    outputs
                        .result
                        .as_ref()
                        .map_or("unavailable", |result| result.digest.as_str()),
                    outputs.commit.as_deref().unwrap_or("unavailable"),
                    outputs.verdict.as_deref().unwrap_or("unavailable")
                ));
                for warning in &outputs.warnings {
                    human.push_str(&format!("    warning: {}\n", bounded_reason(warning)));
                }
            }
        }
    }
    if let Some(error) = &run.advance_error {
        human.push_str(&format!("Advance error: {error}\n"));
    }
    human
}


}
#[cfg(test)]
#[allow(unused_imports, reason = "the custom worktree driver omits libtest functions")]
pub(crate) use test_presentation::{graph_human, graph_human_full};

pub(super) fn graph_ci_waiting<'a>(
    elapsed_seconds: u64,
    check_names: impl Iterator<Item = &'a str>,
    head_sha: Option<&str>,
) -> String {
    let names = check_names.collect::<Vec<_>>().join(", ");
    let mut detail = format!(
        "CI waiting elapsed={elapsed_seconds}s checks={}",
        if names.is_empty() {
            "awaiting registration"
        } else {
            &names
        },
    );
    if let Some(head) = head_sha {
        detail.push_str(&format!(" head={head}"));
    }
    detail
}

/// Counts of nodes by state, then the nodes that need attention (failed,
/// paused, running), then the command that shows the rest.
pub(super) fn graph_summary(run: &serde_json::Value) -> provenant_cli::Summary {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let checkpoint = run.get("checkpoint").unwrap_or(&serde_json::Value::Null);
    let id = text(checkpoint, "graph_id").unwrap_or_else(|| "unknown".into());
    let nodes: Vec<(String, String, Option<String>)> = checkpoint
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .map(|node| {
                    (
                        text(node, "id").unwrap_or_default(),
                        text(node, "state").unwrap_or_else(|| "unknown".into()),
                        text(node, "advance_error").or_else(|| {
                            if text(node, "state").as_deref() == Some("failed")
                                && let Some(outcome) = node
                                    .get("attempts")
                                    .and_then(serde_json::Value::as_array)
                                    .and_then(|attempts| attempts.last())
                                    .and_then(|attempt| text(attempt, "outcome"))
                            {
                                return Some(outcome);
                            }
                            let ci = node.get("ci")?;
                            if text(node, "state").as_deref() != Some("running")
                                || text(ci, "state").as_deref() != Some("pending")
                            {
                                return None;
                            }
                            let names = ci.get("check_names").and_then(serde_json::Value::as_array);
                            Some(graph_ci_waiting(
                                ci.get("elapsed_seconds")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0),
                                names
                                    .into_iter()
                                    .flatten()
                                    .filter_map(serde_json::Value::as_str),
                                ci.get("head_sha").and_then(serde_json::Value::as_str),
                            ))
                        }),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let rank = |state: &str| match state {
        "failed" | "refused" | "cancelled" => 0,
        "paused" | "blocked" | "input_required" => 1,
        "running" | "launched" | "pending" => 2,
        _ => 3,
    };
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (_, state, _) in &nodes {
        match counts.iter_mut().find(|(name, _)| name == state) {
            Some((_, n)) => *n += 1,
            None => counts.push((state.clone(), 1)),
        }
    }
    counts.sort_by_key(|(state, _)| rank(state));
    let tally = provenant_cli::Summary::tally(
        &counts
            .iter()
            .map(|(state, n)| (state.as_str(), *n))
            .collect::<Vec<_>>(),
    );
    let status = text(run, "status").unwrap_or_else(|| "unknown".into());
    let mut action: Vec<&(String, String, Option<String>)> = nodes
        .iter()
        .filter(|(_, state, _)| rank(state) < 3)
        .collect();
    action.sort_by_key(|(_, state, _)| rank(state));
    let mut rows: Vec<String> = Vec::new();
    if let Some(reason) = text(checkpoint, "pause_reason") {
        rows.push(format!("paused: {}", bounded_reason(&reason)));
    }
    if let Some(error) = text(run, "advance_error") {
        rows.push(format!("advance error: {error}"));
    }
    rows.extend(action.iter().map(|(node, state, error)| match error {
        Some(error) => format!("{node} {state}: {error}"),
        None => format!("{node} {state}"),
    }));
    let next = match status.as_str() {
        "complete" | "cancelled" => None,
        "paused" => Some("provenant fabric graph resume --document DOCUMENT".to_owned()),
        _ => Some(format!("provenant fabric graph status {id}")),
    };
    provenant_cli::Summary::new(
        format!("graph {id} {status}: {} nodes ({tally})", nodes.len()),
        rows,
        next,
    )
}

/// Sanitise and bound even pause reasons from older checkpoints before printing.
fn bounded_reason(reason: &str) -> String {
    provenant_fabric_runtime::hosts::plain_text(reason, 1024)
}
