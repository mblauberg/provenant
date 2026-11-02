pub(crate) fn graph_human(run: &provenant_fabric_runtime::graph::GraphRun) -> String {
    let members: std::collections::BTreeSet<_> = run
        .checkpoint
        .nodes
        .iter()
        .flat_map(|node| node.children.iter())
        .collect();
    let mut human = format!("Graph {}: {:?}\n", run.checkpoint.graph_id, run.status);
    for node in &run.checkpoint.nodes {
        let waiting_ci = node.ci.as_ref().filter(|ci| {
            node.state == provenant_fabric_runtime::graph::NodeState::Running
                && ci.state == provenant_fabric_runtime::graph::CiState::Pending
        });
        if (members.contains(&node.id) || node.round.is_some()) && waiting_ci.is_none() {
            continue;
        }
        human.push_str(&format!("  {}: {:?}", node.id, node.state));
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
        }
        human.push('\n');
    }
    if let Some(error) = &run.advance_error {
        human.push_str(&format!("Advance error: {error}\n"));
    }
    human
}

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
        rows.push(format!("paused: {reason}"));
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
