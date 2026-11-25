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
    /// A live attempt silent past its configured silence threshold.
    pub stalled: bool,
    /// The last output or provider start time, rendered beside `stalled`.
    pub last_event_at: Option<String>,
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

/// A sanitised field kept whole. The digest builder owns the facts; the compact
/// renderer owns the `SUMMARY_ROW_BYTES` bound, so no fact is cut here.
fn whole_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(|s| provenant_fabric_runtime::hosts::plain_text(s, usize::MAX))
}

/// The one state vocabulary shared by the status listing and the detail view.
/// A live attempt is running; only work before admission or launch is pending.
/// An observed but non-live writer keeps its own presence label, so neither
/// view claims a live lane it cannot prove.
pub(crate) fn lane_state<'a>(
    state: &'a str,
    attempt_state: Option<&'a str>,
    presence: Option<&'a str>,
) -> &'a str {
    match presence {
        Some("live") => "running",
        Some(presence @ ("unattended" | "settling" | "unknown")) => presence,
        Some(_) => state,
        None => match attempt_state {
            // `starting` precedes admission and the provider; it has not
            // launched, so it is pending rather than running.
            Some("queued" | "starting") => "pending",
            Some(attempt_state) => attempt_state,
            None => match state {
                // A node held before admission reports `waiting` instead of
                // `pending`; both mean it has not been launched yet.
                "waiting" | "pending" => "pending",
                state => state,
            },
        },
    }
}

impl NodeDigest {
    /// The label this node carries in both the compact summary and the detail
    /// line, derived from the same node and attempt facts as its fields.
    pub(crate) fn display_state(&self) -> &str {
        lane_state(
            &self.state,
            self.attempt_state.as_deref(),
            self.attempt_presence.as_deref(),
        )
    }
}

pub(super) fn path_text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| {
        let path: provenant_records::StructuredPathWire =
            serde_json::from_value(value.clone()).ok()?;
        Some(path.to_path_buf().ok()?.to_string_lossy().into_owned())
    })
}

/// Select the attempt that represents a node in status. A live attempt always
/// owns the display. Once the node is settled, an attempt that never launched
/// cannot carry a result, so it must not hide an earlier successful attempt and
/// its result: a preparation refusal is an operation outcome, not the node's
/// work (#3987). A node with no result-bearing attempt keeps its latest one.
fn selected_attempt<'a>(node: &Value, rows: &'a [Value]) -> Option<&'a Value> {
    let attempts: Vec<_> = rows
        .iter()
        .filter(|row| row["kind"] == "attempt" && row["work_key"] == node["work_key"])
        .collect();
    attempts
        .iter()
        .rfind(|row| {
            matches!(
                row["state"].as_str(),
                Some("running" | "starting" | "queued")
            ) || row["presence"].as_str() == Some("live")
        })
        .or_else(|| {
            attempts.iter().rfind(|row| {
                row["outcome"].as_str() == Some("succeeded")
                    || !row["result_path"].is_null()
                    || row["result_summary"]
                        .as_str()
                        .is_some_and(|summary| !summary.is_empty())
            })
        })
        .or_else(|| attempts.last())
        .copied()
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
                let attempt = selected_attempt(node, rows);
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
            let stalled = attempt["stalled"] == true;
            let last_event_at = text(&attempt["last_event_at"], 64);
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
            let current_state = lane_state(
                &state,
                attempt_state.as_deref(),
                attempt_presence.as_deref(),
            );
            let mut line = if attempt_state.is_some() {
                format!(
                    "{key} {current_state} node_state={state} attempt_state={} outcome={outcome}",
                    attempt_state.as_deref().unwrap_or("unknown")
                )
            } else {
                format!("{key} {current_state} outcome={outcome}")
            };
            // The reason leads the evidence fields. The compact renderer bounds
            // a row from its front, so a failure's reason (which names a path on
            // a `worktree_missing` refusal) must come before the longer evidence
            // fields or its leading clause would be clipped away (#3835).
            let result =
                whole_text(&attempt["result_summary"]).filter(|summary| !summary.is_empty());
            let note = if result.is_some() {
                None
            } else {
                whole_text(&node["reason"]).or_else(|| whole_text(&node["waiting_for"]))
            };
            if let Some(reason) = note.as_deref() {
                line.push_str(&format!(" note={reason}"));
            }
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
            // The checkpoint head is an observation, not a fresh forge read.
            if let Some(head) = text(&attempt["head"], 128).or_else(|| text(&node["head"], 128)) {
                line.push_str(&format!(" head={head}/recorded"));
            }
            if let Some(presence) = &attempt_presence {
                line.push_str(&format!(" attempt_presence={presence}"));
            }
            if stalled {
                line.push_str(&format!(
                    " stalled last_event={}",
                    last_event_at.as_deref().unwrap_or("unknown")
                ));
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
            if let Some(summary) = result {
                line.push_str(&format!(" result={summary}"));
            }
            // Identity, outcome and all evidence fields stay in the structured
            // digest; the whole human line is kept and bounded only by the
            // compact renderer, never before the render mode is known.
            line = provenant_fabric_runtime::hosts::plain_text(&line, usize::MAX);
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
                stalled,
                last_event_at,
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

pub(super) fn graph_snapshot(
    mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint,
) -> provenant_fabric_runtime::graph::GraphRun {
    use provenant_fabric_runtime::graph::{GraphRun, GraphStatus, NodeState};
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
    fn lane_state_uses_one_vocabulary_for_live_attempts_and_prelaunch_work() {
        // Spec: docs/specs/cli.md#status-selection
        use super::lane_state;

        assert_eq!(
            lane_state("running", Some("running"), Some("live")),
            "running"
        );
        for presence in ["unattended", "settling", "unknown"] {
            assert_eq!(
                lane_state("running", Some("running"), Some(presence)),
                presence
            );
        }
        assert_eq!(lane_state("running", Some("running"), None), "running");
        for attempt_state in ["queued", "starting"] {
            assert_eq!(lane_state("waiting", Some(attempt_state), None), "pending");
        }
        assert_eq!(lane_state("waiting", None, None), "pending");
        assert_eq!(lane_state("pending", None, None), "pending");
        assert_eq!(lane_state("failed", None, None), "failed");
    }

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
        assert_eq!(digest.display_state(), "cancelled");
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
            let expected_state = if presence == "live" {
                "running"
            } else {
                presence
            };

            assert_eq!(digest.state, "running");
            assert_eq!(digest.attempt_state.as_deref(), Some("running"));
            assert_eq!(digest.attempt_presence.as_deref(), Some(presence));
            assert_eq!(digest.display_state(), expected_state);
            assert!(digest.line.contains(&format!(
                "{expected_state} node_state=running attempt_state=running"
            )));
            assert!(
                digest
                    .line
                    .contains(&format!("attempt_presence={presence}"))
            );
            assert!(!digest.settled);
        }
    }

    #[test]
    // Spec: docs/specs/supervisor.md#attempt-silence-is-reported-action-is-opt-in
    fn digest_reports_a_stall_with_its_last_event_time() {
        use super::node_digests;
        use provenant_core::ProjectId;
        use provenant_records::WorkKey;
        use serde_json::json;

        let key = json!(WorkKey::new(ProjectId::from_bytes([8; 16]), "g", "n", 1).unwrap());
        let rows = vec![
            json!({"kind":"node","work_key":key,"state":"running"}),
            json!({
                "kind":"attempt","work_key":key,"state":"running","presence":"live",
                "stalled":true,"last_event_at":"2026-11-26T04:58:00Z"
            }),
        ];
        let digest = node_digests(&rows).remove(0);
        assert!(digest.stalled);
        assert_eq!(
            digest.last_event_at.as_deref(),
            Some("2026-11-26T04:58:00Z")
        );
        assert!(
            digest
                .line
                .contains("stalled last_event=2026-11-26T04:58:00Z")
        );
        assert!(!digest.settled, "a stall is not a settlement");
    }

    #[test]
    fn digest_labels_the_recorded_head_as_an_observation() {
        use provenant_core::ProjectId;
        use provenant_records::WorkKey;
        use serde_json::json;

    }

    #[test]
    fn digest_line_keeps_a_long_refusal_reason_whole() {
        // A worktree_missing refusal names its path; the renderer, not the
        // digest builder, decides how much of it a bounded view shows.
        use super::node_digests;
        use provenant_core::ProjectId;
        use provenant_records::WorkKey;
        use serde_json::{Value, json};

        let key = json!(WorkKey::new(ProjectId::from_bytes([9; 16]), "g", "node", 1).unwrap());
        let path = format!("/host/{}", "long/path/segment/".repeat(40));
        let reason = format!("worktree_missing: checkout at {path} was removed");
        let rows: Vec<Value> = vec![json!({
            "kind":"node","work_key":key,"state":"failed","reason":reason
        })];

        let digest = node_digests(&rows).remove(0);
        assert!(
            digest.line.contains(&reason),
            "digest clipped the reason: {}",
            digest.line
        );
    }

    #[test]
    fn a_never_launched_refusal_does_not_hide_the_successful_attempt() {
        // Spec: docs/specs/cli.md#status-selection
        use super::node_digests;
        use provenant_core::ProjectId;
        use provenant_records::WorkKey;
        use serde_json::{Value, json};

        let key = json!(WorkKey::new(ProjectId::from_bytes([11; 16]), "g", "task", 1).unwrap());
        let rows: Vec<Value> = vec![
            json!({"kind":"node","work_key":key,"state":"succeeded"}),
            json!({
                "kind":"attempt","work_key":key,"state":"terminal",
                "outcome":"succeeded","result_path":"attempts/1/result.md",
                "result_summary":"task 1 finished and pushed its commits"
            }),
            json!({
                "kind":"attempt","work_key":key,"state":"terminal",
                "outcome":"rejected","terminal_cause":"handoff_refused",
                "cause_code":"handoff_refused","result_path":null,
                "reason":"writer lineage: invalid run record: graph predecessor writer or source"
            }),
        ];

        let digest = node_digests(&rows).remove(0);
        assert_eq!(digest.outcome, "succeeded");
        assert_eq!(
            digest.result_path.as_deref(),
            Some("attempts/1/result.md"),
            "a refusal that never launched hid the successful result: {}",
            digest.line
        );
        assert!(
            digest
                .line
                .contains("result=task 1 finished and pushed its commits"),
            "{}",
            digest.line
        );
        assert!(
            !digest.line.contains("cause=handoff_refused"),
            "the refused attempt replaced the successful one: {}",
            digest.line
        );
    }
}
