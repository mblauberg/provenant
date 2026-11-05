//! Read-only node wait projection from recorded graph owner state.
use super::*;
use serde_json::{Value, json};

/// Facts shown consistently by graph status clients.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeWaitView {
    pub node: String,
    pub wait: &'static str,
    pub disposition: &'static str,
    pub code: Value,
    pub reason: Value,
    pub attempt: Value,
    pub route: Value,
    pub failures: Value,
    pub limit: Value,
    pub limit_source: Value,
    pub wake_at: Value,
    pub next: Value,
}

impl GraphCheckpoint {
    /// Derive status without observing providers or changing the checkpoint.
    pub fn wait_views(&self) -> Vec<NodeWaitView> {
        self.nodes
            .iter()
            .map(|node| {
                let unavailable = || json!("unavailable");
                let ask = self.asks.iter().find(|ask| {
                    !ask.resolved && !ask.closed && ask.node.as_deref() == Some(&node.id)
                });
                let retry = node
                    .launch_retry
                    .as_ref()
                    .filter(|retry| retry.failures > 0 && node.state == NodeState::Running);
                let block = node.launch_block.as_ref().filter(|_| node.state == NodeState::Blocked);
                let ci = node
                    .ci
                    .as_ref()
                    .filter(|ci| node.state == NodeState::Running && ci.state == CiState::Pending);
                let land = node
                    .land
                    .as_ref()
                    .filter(|_| node.state == NodeState::Running);
                let (wait, wake) = if matches!(node.state, NodeState::Succeeded | NodeState::Failed | NodeState::Skipped) {
                    ("none", None)
                } else if let Some(ask) = ask {
                    ("approval", ask.wake_at)
                } else if let Some(retry) = retry {
                    ("launch_retry", retry.wake_at)
                } else if block.is_some()
                    && node.advance_error_code.as_deref() == Some("launch_retry_exhausted")
                {
                    ("launch_retry", None)
                } else if let Some(ci) = ci {
                    ("ci", ci.wake_at)
                } else if let Some(land) = land {
                    ("land", land.wake_at)
                } else {
                    ("none", None)
                };
                let disposition = match node.state {
                    NodeState::Blocked => "blocked",
                    NodeState::Succeeded | NodeState::Failed | NodeState::Skipped => "terminal",
                    _ if matches!(wait, "ci" | "land" | "launch_retry") => "retry",
                    _ => "running",
                };
                let attempt = node.attempts.last();
                let code = node
                    .advance_error_code
                    .as_ref()
                    .or_else(|| retry.and_then(|r| r.code.as_ref()));
                let reason = node
                    .advance_error
                    .as_ref()
                    .or_else(|| retry.and_then(|r| r.reason.as_ref()))
                    .or_else(|| ci.map(|c| &c.summary))
                    .or_else(|| land.and_then(|l| l.note.as_ref()))
                    .or_else(|| ask.map(|a| &a.title));
                let failures = retry
                    .map(|r| r.failures)
                    .or_else(|| block.and_then(|b| b.failures));
                let limit = retry
                    .and_then(|r| r.limit)
                    .or_else(|| block.and_then(|b| b.limit));
                let source = retry
                    .and_then(|r| r.limit_source)
                    .or_else(|| block.and_then(|b| b.limit_source));
                let next = if wait == "launch_retry" || node.state == NodeState::Blocked {
                    retry.and_then(|r| r.next.as_ref()).or_else(|| block.and_then(|b| b.next.as_ref()))
                } else { None };
                NodeWaitView {
                    node: node.id.clone(),
                    wait,
                    disposition,
                    code: code.map_or_else(unavailable, |v| json!(v)),
                    reason: reason.map_or_else(unavailable, |v| json!(v)),
                    attempt: attempt.map_or_else(
                        unavailable,
                        |a| json!({"key":a.key,"reference":a.attempt_ref.as_deref().unwrap_or("unavailable")}),
                    ),
                    route: attempt.map_or_else(unavailable, |a| json!(a.route)),
                    failures: failures.map_or_else(unavailable, |v| json!(v)),
                    limit: limit.map_or_else(unavailable, |v| json!(v)),
                    limit_source: source.map_or_else(unavailable, |v| json!(v)),
                    wake_at: wake.map_or_else(|| json!("none"), |time| json!(time)),
                    next: if let Some(next) = next {
                        json!(next)
                    } else if let Some(ask) = ask.filter(|_| wait == "approval") {
                        json!(format!("answer the recorded ask {}", ask.ask_id))
                    } else if wait == "ci" {
                        json!(if wake.is_some() { "wait for the scheduled CI observation" } else { "inspect CI and resume the graph" })
                    } else if wait == "land" {
                        json!(if wake.is_some() { "wait for the scheduled land observation" } else { "inspect the pull request and resume the graph" })
                    } else if node.advance_error_code.as_deref() == Some("usage_limited") {
                        json!("inspect quota before submitting new work")
                    } else {
                        unavailable()
                    },
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_projection_uses_only_recorded_facts_and_never_changes_checkpoint() {
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version": 1, "graph_id": "waits", "plan_digest": "digest",
            "nodes": [
                {"id":"approval", "state":"running", "worktree":null, "attempts":[]},
                {"id":"ci", "state":"running", "worktree":null, "attempts":[], "ci":{"state":"pending", "summary":"checks pending"}},
                {"id":"land", "state":"running", "worktree":null, "attempts":[], "land":{"tier":"routine", "policy":"auto", "policy_source":"default", "branch":"feature", "head":"head", "note":"waiting"}},
                {"id":"retry", "state":"running", "worktree":null, "attempts":[{"launched":true,"key":{"graph_id":"waits","node_id":"retry","route_index":0},"route":{"alias":"senior"},"state":"starting","attempt_ref":"host/1","outcome":null,"launch_actor":"actor"}], "launch_retry":{"failures":1, "limit":3, "limit_source":"override", "code":"writer_lock_busy", "next":"wait for the lock", "reason":"lock busy"}},
                {"id":"exhausted", "state":"blocked", "worktree":null, "attempts":[], "advance_error_code":"launch_retry_exhausted", "launch_block":{"failures":2,"limit":1,"limit_source":"override","next":"inspect custody"}},
                {"id":"quota", "state":"failed", "worktree":null, "attempts":[], "advance_error_code":"usage_limited"}
            ],
            "asks":[{"ask_id":"a", "kind":"decide", "node":"approval", "session":"chair", "title":"Decision", "body":"Choose", "options":[], "resolved":false, "closed":false}]
        })).unwrap();
        checkpoint.nodes[3].launch_retry.as_mut().unwrap().wake_at =
            Some(provenant_core::WallTime::from_micros(1_800_000_000_000_000).unwrap());
        let before = serde_json::to_vec(&checkpoint).unwrap();
        let views = checkpoint.wait_views();
        assert_eq!(
            views.iter().map(|view| view.wait).collect::<Vec<_>>(),
            [
                "approval",
                "ci",
                "land",
                "launch_retry",
                "launch_retry",
                "none"
            ]
        );
        assert_eq!(views[3].code, "writer_lock_busy");
        assert_eq!(views[3].attempt["reference"], "host/1");
        assert_eq!(views[3].attempt["key"]["route_index"], 0);
        assert_eq!(views[3].route["alias"], "senior");
        assert_eq!(views[3].failures, 1);
        assert_eq!(views[3].limit, 3);
        assert_eq!(views[3].limit_source, "override");
        assert_eq!(views[3].next, "wait for the lock");
        assert_ne!(views[3].wake_at, "none");
        assert_eq!(views[4].disposition, "blocked");
        assert_eq!(views[4].code, "launch_retry_exhausted");
        assert_eq!(views[4].failures, 2);
        assert_eq!(views[5].disposition, "terminal");
        assert_eq!(views[5].code, "usage_limited");
        assert_eq!(views[5].wake_at, "none");
        assert_eq!(views[0].limit, "unavailable");
        let run = GraphRun {
            status: GraphStatus::Pending,
            outcome: None,
            checkpoint: checkpoint.clone(),
            advance_error: None,
        };
        let rendered = serde_json::to_value(&run).unwrap();
        assert_eq!(rendered["waits"], serde_json::to_value(&views).unwrap());
        let reopened: GraphCheckpoint = serde_json::from_slice(&before).unwrap();
        assert_eq!(reopened.wait_views(), views);
        assert_eq!(serde_json::to_vec(&checkpoint).unwrap(), before);
    }
}
