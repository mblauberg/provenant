//! Read-only projection of the waits recorded in a graph checkpoint.
use super::{AttemptOutcome, CiState, GraphCheckpoint, NodeState};
use serde::Serialize;
use serde_json::{Value, json};

/// A node's current wait and the evidence the checkpoint retains about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphNodeWait {
    pub node: String,
    pub state: NodeState,
    pub wait: &'static str,
    pub disposition: &'static str,
    pub code: String,
    pub attempt: String,
    pub route: Value,
    pub failures: Value,
    pub limit: Value,
    pub limit_source: String,
    pub wake_at: String,
    pub next: String,
    pub cause: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub handoff: Option<provenant_records::handoff::Handoff>,
    pub output_path: Option<String>,
    pub spec_concerns: Vec<provenant_records::handoff::SpecConcern>,
    pub spec_concern_count: usize,
    pub rejected_boundaries: Vec<super::Finding>,
    pub rejected_boundary_count: usize,
    pub spec_defect_count: usize,
    pub spec_defects: Vec<super::Finding>,
}

/// Derive status without changing wake scheduling or checkpoint bytes.
pub fn graph_waits(checkpoint: &GraphCheckpoint) -> Vec<GraphNodeWait> {
    checkpoint
        .nodes
        .iter()
        .map(|node| {
            let attempt = node.attempts.last();
            let quota = node.state == NodeState::Failed
                && attempt.and_then(|attempt| attempt.outcome)
                    == Some(AttemptOutcome::UsageLimited);
            let approval = checkpoint
                .asks
                .iter()
                .find(|ask| ask.node.as_deref() == Some(node.id.as_str()));
            let retry = node
                .launch_retry
                .as_ref()
                .filter(|retry| retry.failures > 0)
                .or(node.last_launch_retry.as_ref());
            let (wait, wake) = if let Some(ask) = approval {
                ("approval", ask.wake_at)
            } else if let Some(retry) = node
                .launch_retry
                .as_ref()
                .filter(|retry| retry.failures > 0 && retry.wake_at.is_some())
            {
                ("launch_retry", retry.wake_at)
            } else if let Some(ci) = node.ci.as_ref().filter(|ci| ci.state == CiState::Pending) {
                ("ci", ci.wake_at)
            } else if node.state == NodeState::Running
                && let Some(land) = node.land.as_ref()
            {
                ("land", land.wake_at)
            } else {
                ("none", None)
            };
            let disposition = if node.state == NodeState::Blocked {
                "blocked"
            } else if quota && node.state != NodeState::Running {
                "terminal"
            } else if wait == "launch_retry" {
                "retry"
            } else if node.state == NodeState::Pending {
                "pending"
            } else if node.state != NodeState::Running {
                "terminal"
            } else {
                "running"
            };
            let code = node
                .advance_owner_code
                .clone()
                .or_else(|| node.advance_error_code.clone())
                .unwrap_or_else(|| {
                    if quota {
                        "usage_limited".into()
                    } else {
                        "unavailable".into()
                    }
                });
            let next = node
                .advance_error
                .as_deref()
                .and_then(|error| error.split_once("next: ").map(|(_, next)| next.to_owned()))
                .filter(|next| !next.is_empty())
                .unwrap_or_else(|| "unavailable".into());
            GraphNodeWait {
                node: node.id.clone(),
                state: node.state,
                wait,
                disposition,
                code,
                attempt: attempt
                    .and_then(|attempt| attempt.attempt_ref.clone())
                    .unwrap_or_else(|| "unavailable".into()),
                route: attempt
                    .map(|attempt| json!(attempt.route))
                    .unwrap_or_else(|| json!("unavailable")),
                failures: retry
                    .map(|retry| json!(retry.failures))
                    .unwrap_or_else(|| json!("unavailable")),
                limit: retry
                    .map(|retry| json!(retry.policy.limit))
                    .unwrap_or_else(|| json!("unavailable")),
                limit_source: retry
                    .map(|retry| retry.policy.limit_source.clone())
                    .unwrap_or_else(|| "unavailable".into()),
                wake_at: wake.map(|wake| wake.to_string()).unwrap_or_else(|| {
                    if wait == "none" {
                        "none".into()
                    } else {
                        "unavailable".into()
                    }
                }),
                next,
                cause: node
                    .advance_owner_code
                    .clone()
                    .or_else(|| node.advance_error_code.clone())
                    .or_else(|| node.loop_pause_code.clone())
                    .or_else(|| quota.then(|| "usage_limited".into())),
                branch: node
                    .outputs
                    .as_ref()
                    .and_then(|output| output.branch.clone())
                    .or_else(|| node.land.as_ref().map(|land| land.branch.clone())),
                head: node
                    .outputs
                    .as_ref()
                    .and_then(|output| output.commit.clone())
                    .or_else(|| node.land.as_ref().map(|land| land.head.clone()))
                    .or_else(|| {
                        node.writer_binding
                            .as_ref()
                            .map(|binding| binding.head.clone())
                    }),
                handoff: node.handoff.clone(),
                spec_concerns: node
                    .outputs
                    .as_ref()
                    .and_then(|output| output.extra.get("spec_concerns"))
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .unwrap_or_default(),
                spec_concern_count: node
                    .outputs
                    .as_ref()
                    .and_then(|output| output.extra.get("spec_concern_count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                rejected_boundaries: node
                    .outputs
                    .as_ref()
                    .map(|output| {
                        output
                            .findings
                            .iter()
                            .filter(|finding| finding.rejected_boundary())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
                rejected_boundary_count: node.outputs.as_ref().map_or(0, |output| {
                    output
                        .findings
                        .iter()
                        .filter(|finding| finding.rejected_boundary())
                        .count()
                }),
                spec_defects: node
                    .outputs
                    .as_ref()
                    .map(|output| {
                        output
                            .findings
                            .iter()
                            .filter(|finding| finding.spec_defect())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
                spec_defect_count: node
                    .outputs
                    .as_ref()
                    .and_then(|output| output.extra.get("spec_defect_count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                output_path: node
                    .outputs
                    .as_ref()
                    .and_then(|output| output.result.as_ref())
                    .and_then(|result| result.path.clone()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphRun, GraphStatus};

    #[test]
    fn wait_view_uses_only_recorded_facts_without_changing_checkpoint() {
        let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"g","plan_digest":"digest",
            "asks":[{"ask_id":"ask","kind":"decide","node":"approval","session":"chair","request":"hrq-ask"}],
            "nodes":[
                {"id":"approval","state":"running","worktree":null,"attempts":[]},
                {"id":"ci","state":"running","worktree":null,"attempts":[],"ci":{"state":"pending","summary":"waiting"}},
                {"id":"land","state":"running","worktree":null,"attempts":[],"land":{"tier":"routine","policy":"auto","policy_source":"default","branch":"work","head":"abc"}},
                {"id":"retry","state":"running","worktree":null,"launch_retry":{"policy":{"limit":3,"limit_source":"host","first_seconds":5,"max_seconds":60},"failures":2,"wake_at":"2026-01-01T00:00:00.000000Z"},"attempts":[{"launched":false,"key":{"graph_id":"g","node_id":"retry","route_index":0, "attempt": 1},"route":{"alias":"senior"},"state":"starting","attempt_ref":"host/attempt","outcome":null,"launch_actor":"actor"}]},
                {"id":"blocked","state":"blocked","worktree":null,"advance_error_code":"launch_retry_exhausted","last_launch_retry":{"policy":{"limit":1,"limit_source":"project","first_seconds":5,"max_seconds":60},"failures":2},"attempts":[]},
                {"id":"quota","state":"failed","worktree":null,"attempts":[{"launched":true,"key":{"graph_id":"g","node_id":"quota","route_index":0, "attempt": 1},"route":{"alias":"senior"},"state":"terminal","attempt_ref":"host/quota","outcome":"usage_limited","launch_actor":"actor"}]},
                {"id":"pending","state":"pending","worktree":null,"attempts":[]},
                {"id":"done","state":"succeeded","worktree":null,"handoff":{"schema_version":2,"status":"stopped_early","note":"done","remaining":[],"decisions":[],"verify":[]},"outputs":{"schema_version":1,"result":{"digest":"sha256:test","excerpt":"","truncated":false,"path":"result.txt"},"verdict":null,"findings":[],"findings_available":null,"branch":"lane","commit":"abc","warnings":[]},"attempts":[]}
            ]
        })).unwrap();
        let before = serde_json::to_vec(&checkpoint).unwrap();
        let waits = graph_waits(&checkpoint);
        let by_id = |id: &str| waits.iter().find(|wait| wait.node == id).unwrap();
        assert_eq!(waits.len(), checkpoint.nodes.len());
        assert_eq!(by_id("pending").disposition, "pending");
        assert_eq!(by_id("done").disposition, "terminal");
        assert_eq!(by_id("done").branch.as_deref(), Some("lane"));
        assert_eq!(by_id("done").head.as_deref(), Some("abc"));
        assert_eq!(
            by_id("done").handoff.as_ref().map(|h| h.note.as_str()),
            Some("done")
        );
        assert_eq!(by_id("done").output_path.as_deref(), Some("result.txt"));
        assert_eq!(
            by_id("blocked").cause.as_deref(),
            Some("launch_retry_exhausted")
        );
        assert_eq!(by_id("approval").wait, "approval");
        assert_eq!(by_id("ci").wait, "ci");
        assert_eq!(by_id("land").wait, "land");
        assert_eq!(by_id("retry").wait, "launch_retry");
        assert_eq!(by_id("retry").disposition, "retry");
        assert_eq!(by_id("retry").attempt, "host/attempt");
        assert_eq!(by_id("retry").failures, json!(2));
        assert_eq!(by_id("retry").limit, json!(3));
        assert_eq!(by_id("retry").limit_source, "host");
        assert_eq!(by_id("blocked").disposition, "blocked");
        assert_eq!(by_id("blocked").code, "launch_retry_exhausted");
        assert_eq!(by_id("quota").code, "usage_limited");
        assert_eq!(by_id("quota").cause.as_deref(), Some("usage_limited"));
        assert_eq!(by_id("quota").disposition, "terminal");
        assert_eq!(by_id("quota").wake_at, "none");
        assert_eq!(by_id("approval").next, "unavailable");
        assert_eq!(by_id("approval").wake_at, "unavailable");
        let run = GraphRun {
            status: GraphStatus::Pending,
            outcome: None,
            checkpoint,
            advance_error: None,
        };
        let wire = serde_json::to_value(&run).unwrap();
        assert_eq!(wire["waits"], serde_json::to_value(&waits).unwrap());
        assert_eq!(serde_json::to_vec(&run.checkpoint).unwrap(), before);
    }
}
