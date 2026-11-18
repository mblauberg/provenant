use provenant_records::WorkKey;
use serde::Serialize;
use serde_json::{Value, json};

/// The one node projection consumed by both status and wait. External facts
/// are retained observations, never silently promoted to a current forge read.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct NodeDigest {
    pub key: WorkKey,
    pub outcome: String,
    pub cause: Option<String>,
    pub branch: Option<String>,
    pub pr: Value,
    pub ci: Value,
    pub result_path: Option<String>,
    pub provider_error: Option<String>,
    pub review_verdict: Option<String>,
    pub findings_count: Option<usize>,
    pub findings_available: Option<bool>,
    pub review_partial: Option<bool>,
    /// The selected Run attempt's stored state, separate from graph node state.
    pub attempt_state: Option<String>,
    /// This host's current liveness projection for the selected attempt.
    pub attempt_presence: Option<String>,
    pub line: String,
    pub state: String,
    pub settled: bool,
    pub handoff_status: Option<String>,
    pub open_asks: Vec<Value>,
    pub evidence_error: Option<String>,
}

fn text(value: &Value, limit: usize) -> Option<String> {
    value
        .as_str()
        .map(|s| provenant_fabric_runtime::hosts::plain_text(s, limit))
}

pub(super) fn path_text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| {
        let path: provenant_records::StructuredPathWire =
            serde_json::from_value(value.clone()).ok()?;
        Some(path.to_path_buf().ok()?.to_string_lossy().into_owned())
    })
}

/// Select each node's current attempt once, or the explicitly selected older
/// attempt. Node state owns settlement; a previous attempt cannot settle it.
pub(crate) fn node_digests(rows: &[Value]) -> Vec<NodeDigest> {
    let nodes: Vec<_> = rows.iter().filter(|row| row["kind"] == "node").collect();
    let selected: Vec<_> = if nodes.is_empty() {
        rows.iter()
            .filter(|row| row["kind"] == "attempt")
            .map(|row| (row, row))
            .collect()
    } else {
        nodes
            .into_iter()
            .map(|node| {
                let attempt = rows
                    .iter()
                    .rfind(|row| row["kind"] == "attempt" && row["work_key"] == node["work_key"]);
                (node, attempt.unwrap_or(node))
            })
            .collect()
    };
    let mut digests: Vec<_> = selected
        .into_iter()
        .filter_map(|(node, attempt)| {
            let key: WorkKey = serde_json::from_value(node["work_key"].clone()).ok()?;
            let state = text(&node["state"], 64).unwrap_or_else(|| "unknown".into());
            let attempt_state = (attempt["kind"] == "attempt")
                .then(|| text(&attempt["state"], 64))
                .flatten();
            let attempt_presence = text(&attempt["presence"], 64).or_else(|| {
                matches!(attempt_state.as_deref(), Some("running" | "starting"))
                    .then(|| "unknown".to_owned())
            });
            let settled = if node["kind"] == "node" {
                matches!(
                    state.as_str(),
                    "succeeded" | "failed" | "blocked" | "skipped" | "unreadable"
                )
            } else {
                state == "terminal"
                    || attempt["graph_state"] == "terminal"
                    || !attempt["finished_at"].is_null()
            };
            let recorded_outcome = text(&attempt["outcome"], 64);
            let outcome = if node["kind"] == "node" && state != "failed" {
                if state == "running"
                    && attempt_state.as_deref().is_some_and(|attempt_state| {
                        !matches!(attempt_state, "running" | "starting" | "queued")
                    })
                {
                    attempt_state.clone().unwrap_or_else(|| state.clone())
                } else {
                    state.clone()
                }
            } else {
                recorded_outcome
                    .filter(|outcome| state != "failed" || outcome.as_str() != "succeeded")
                    .unwrap_or_else(|| state.clone())
            };
            let cause = text(&node["cause_code"], 128)
                .or_else(|| text(&attempt["cause_code"], 128))
                .or_else(|| text(&attempt["cause"], 128));
            let branch =
                text(&attempt["branch"], 256).or_else(|| text(&attempt["delivery"]["branch"], 256));
            let pr = if attempt["pr"].is_null() {
                json!({"state":"unknown","reference":null})
            } else {
                json!({"state":"stale","reference":attempt["pr"]})
            };
            let ci = if attempt["ci"].is_null() {
                json!({"state":"unknown","observation":null})
            } else {
                json!({"state":"stale","observation":attempt["ci"]})
            };
            let result_path = path_text(&attempt["result_path"]);
            let provider_error = text(&attempt["provider_error"], 1024);
            let review_verdict = text(&node["review_verdict"], 128);
            let findings_count = node["findings_count"]
                .as_u64()
                .and_then(|count| usize::try_from(count).ok());
            let findings_available = node["findings_available"].as_bool();
            let review_partial = node["review_partial"].as_bool();
            let handoff_error = attempt["handoff"].get("Err").and_then(|value| {
                serde_json::from_value::<provenant_records::handoff::HandoffError>(value.clone())
                    .ok()
            });
            let handoff_status = if handoff_error.is_some() {
                Some("malformed".to_owned())
            } else {
                text(&attempt["handoff"]["Ok"]["status"], 64)
            };
            let open_asks = node["open_asks"].as_array().cloned().unwrap_or_default();
            let evidence_error = text(&attempt["evidence_error"], 200);
            let current_state = attempt_presence
                .as_deref()
                .or(attempt_state.as_deref())
                .unwrap_or(&state);
            let mut line = if attempt_state.is_some() {
                format!(
                    "{key} {current_state} node_state={state} attempt_state={} outcome={outcome}",
                    attempt_state.as_deref().unwrap_or("unknown")
                )
            } else {
                format!("{key} {state} outcome={outcome}")
            };
            line.push_str(&format!(
                " cause={} branch={} pr={} ci={} result_path={}",
                cause.as_deref().unwrap_or("-"),
                branch.as_deref().unwrap_or("-"),
                if pr["reference"].is_null() {
                    "unknown".to_owned()
                } else {
                    format!(
                        "{}/stale",
                        pr["reference"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| pr["reference"].to_string())
                    )
                },
                if ci["observation"].is_null() {
                    "unknown".to_owned()
                } else {
                    format!(
                        "{}/stale",
                        ci["observation"]["state"].as_str().unwrap_or("unknown")
                    )
                },
                result_path.as_deref().unwrap_or("-")
            ));
            if let Some(presence) = &attempt_presence {
                line.push_str(&format!(" attempt_presence={presence}"));
            }
            if let Some(verdict) = &review_verdict {
                line.push_str(&format!(" verdict={verdict}"));
            }
            if let Some(count) = findings_count {
                let availability = match findings_available {
                    Some(true) => "available",
                    Some(false) => "incomplete",
                    None => "availability_unknown",
                };
                line.push_str(&format!(" findings={count} ({availability})"));
            }
            if review_partial == Some(true) {
                line.push_str(" review_partial=true");
            }
            if let Some(status) = &handoff_status {
                line.push_str(&format!(" handoff_status={status}"));
            }
            if let Some(error) = &handoff_error {
                line.push_str(&format!(
                    " {}",
                    provenant_fabric_runtime::hosts::plain_text(&error.to_string(), 1024)
                ));
            }
            if !open_asks.is_empty() {
                line.push_str(&format!(
                    " open_asks={}",
                    open_asks
                        .iter()
                        .filter_map(|ask| ask["ask_id"].as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
            if let Some(error) = &provider_error {
                line.push_str(&format!(" provider_error={error}"));
            }
            if let Some(error) = &evidence_error {
                line.push_str(&format!(" evidence_unavailable={error}"));
            }
            if let Some(summary) =
                text(&attempt["result_summary"], 200).filter(|summary| !summary.is_empty())
            {
                line.push_str(&format!(" result={summary}"));
            } else if let Some(reason) =
                text(&node["reason"], 200).or_else(|| text(&node["waiting_for"], 200))
            {
                line.push_str(&format!(" note={reason}"));
            }
            // Identity, outcome and all evidence fields stay in the structured
            // digest even when a long human line is abbreviated.
            line = provenant_fabric_runtime::hosts::plain_text(&line, 4096);
            Some(NodeDigest {
                key,
                outcome,
                cause,
                branch,
                pr,
                ci,
                result_path,
                provider_error,
                review_verdict,
                findings_count,
                findings_available,
                review_partial,
                attempt_state,
                attempt_presence,
                line,
                state,
                settled,
                handoff_status,
                open_asks,
                evidence_error,
            })
        })
        .collect();
    digests.sort_by_key(|digest| match digest.state.as_str() {
        "failed" | "blocked" | "paused" | "unresolved" | "unreadable" => 0,
        _ if digest.settled && digest.outcome != "succeeded" => 1,
        "pending" | "running" | "starting" => 2,
        _ => 3,
    });
    digests
}

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
        node.settled_dependency_snapshots.clear();
        if let Some(binding) = &mut node.writer_binding {
            binding.worktree = "[redacted]".into();
            binding.source_directory_identity = None;
        }
    }
}

pub(super) fn graph_snapshot(
    mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
) -> provenant_fabric_runtime::graph::GraphRun {
    use provenant_fabric_runtime::graph::{GraphRun, GraphStatus, NodeState};
    redact_checkout_paths(&mut checkpoint);
    let unfinished = checkpoint
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
        checkpoint.pause = None;
        GraphStatus::Complete
    } else if checkpoint.pause.is_some() {
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

#[cfg(test)]
mod tests {
    #[test]
    fn selected_attempt_digest_preserves_graph_state_and_writer_presence() {
        // Spec: docs/specs/cli.md#status-selection
        use super::node_digests;
        use provenant_core::ProjectId;
        use provenant_records::WorkKey;
        use serde_json::{Value, json};

        fn rows(attempt_state: &str, presence: Option<&str>) -> Vec<Value> {
            let key = WorkKey::new(ProjectId::from_bytes([7; 16]), "graph", "task", 1).unwrap();
            let key = json!(key);
            let mut attempt = json!({
                "kind":"attempt",
                "work_key":key,
                "state":attempt_state,
                "graph_state":"running",
                "cause_code":"steered"
            });
            if let Some(presence) = presence {
                attempt["presence"] = json!(presence);
            }
            vec![
                json!({"kind":"node","work_key":key,"state":"running"}),
                attempt,
            ]
        }

        let digest = node_digests(&rows("cancelled", None)).remove(0);

        assert_eq!(
            digest.state, "running",
            "durable graph node state remains intact"
        );
        assert_eq!(digest.attempt_state.as_deref(), Some("cancelled"));
        assert_eq!(digest.outcome, "cancelled");
        assert_eq!(digest.attempt_presence, None);
        assert!(!digest.settled, "the graph still requires settlement");
        assert!(digest.line.contains("cancelled node_state=running"));
        assert!(!digest.line.contains("running outcome=running"));

        for (recorded_presence, presence) in [
            (Some("live"), "live"),
            (Some("unattended"), "unattended"),
            (Some("settling"), "settling"),
            (Some("unknown"), "unknown"),
            (None, "unknown"),
        ] {
            let digest = node_digests(&rows("running", recorded_presence)).remove(0);

            assert_eq!(digest.state, "running");
            assert_eq!(digest.attempt_state.as_deref(), Some("running"));
            assert_eq!(digest.attempt_presence.as_deref(), Some(presence));
            assert!(digest.line.contains(&format!(
                "{presence} node_state=running attempt_state=running"
            )));
            assert!(
                digest
                    .line
                    .contains(&format!("attempt_presence={presence}"))
            );
            assert!(!digest.settled);
        }
    }
}
