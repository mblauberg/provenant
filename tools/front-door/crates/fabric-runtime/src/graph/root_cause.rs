//! Select the failure that caused a graph to fail.
//!
//! This is the one cause walk shared by `status`, `wait`, `run` and
//! `upgrade --verify`. A node whose only advance evidence is `upstream_blocked`
//! is a dependant, not a cause, and is skipped even when it appears first.

use super::GraphCheckpoint;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The actionable failure behind a graph's blocked dependants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCause {
    pub node: String,
    pub attempt: Option<u32>,
    pub code: String,
    pub message: String,
}

/// The advance evidence the cause walk needs from a node.
#[derive(Clone, Copy)]
struct Cause<'a> {
    id: &'a str,
    note: Option<&'a str>,
    code: Option<&'a str>,
    owner_code: Option<&'a str>,
    attempt: Option<u32>,
}

/// Whether a node's advance fields only describe a dependant blocked by an
/// upstream failure, rather than an actionable cause. Callers that project a
/// single node (the launch-error renderer, for one) use this same rule.
pub fn is_upstream_blocked(code: Option<&str>, note: Option<&str>) -> bool {
    code == Some("upstream_blocked")
        || note.is_some_and(|note| note.trim_start().starts_with("blocked by"))
}

/// The first actionable failure. Owner codes are preferred because they
/// identify the original operation rather than the graph's wrapper disposition.
fn select<'a>(nodes: impl Iterator<Item = Cause<'a>>) -> Option<RootCause> {
    nodes
        .filter_map(|node| {
            let message = node.note?.trim();
            if message.is_empty() || is_upstream_blocked(node.code, node.note) {
                return None;
            }
            let code = node
                .owner_code
                .filter(|code| !code.is_empty())
                .or_else(|| node.code.filter(|code| !code.is_empty()))
                .unwrap_or("unknown")
                .to_owned();
            Some(RootCause {
                node: node.id.to_owned(),
                attempt: node.attempt,
                code,
                message: message.split_whitespace().collect::<Vec<_>>().join(" "),
            })
        })
        .next()
}

/// Select the first detailed failure, skipping nodes blocked by an upstream
/// failure. Owner codes are preferred because they identify the original
/// operation rather than the graph's wrapper disposition.
pub fn root_cause(checkpoint: &GraphCheckpoint) -> Option<RootCause> {
    select(checkpoint.nodes.iter().map(|node| Cause {
        id: &node.id,
        note: node.advance_note.as_deref(),
        code: node.advance_code.as_deref(),
        owner_code: node.advance_owner_code.as_deref(),
        attempt: node.attempts.last().map(|attempt| attempt.key.attempt),
    }))
}

/// The same walk over a checkpoint-shaped JSON value, for envelope readers
/// that hold a projected checkpoint rather than a decoded `GraphCheckpoint`.
/// Both entry points run the one selection above.
pub fn root_cause_from_value(checkpoint: &Value) -> Option<RootCause> {
    let nodes = checkpoint.get("nodes")?.as_array()?;
    select(nodes.iter().filter_map(|node| {
        Some(Cause {
            id: node.get("id")?.as_str()?,
            note: node.get("advance_note").and_then(Value::as_str),
            code: node.get("advance_code").and_then(Value::as_str),
            owner_code: node.get("advance_owner_code").and_then(Value::as_str),
            attempt: node
                .get("attempts")
                .and_then(Value::as_array)
                .and_then(|attempts| attempts.last())
                .and_then(|attempt| attempt.get("key"))
                .and_then(|key| key.get("attempt"))
                .and_then(Value::as_u64)
                .and_then(|attempt| u32::try_from(attempt).ok()),
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::{is_upstream_blocked, root_cause, root_cause_from_value};
    use crate::graph::GraphCheckpoint;

    fn checkpoint(nodes: serde_json::Value) -> GraphCheckpoint {
        serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "graph_id": "root-cause-fixture",
            "plan_digest": "digest",
            "nodes": nodes,
        }))
        .expect("fixture is a valid checkpoint")
    }

    fn blocked_dependant() -> serde_json::Value {
        serde_json::json!({
            "id": "task--review",
            "state": "blocked",
            "worktree": null,
            "advance_code": "upstream_blocked",
            "advance_note": "blocked by task--read (Blocked)",
            "attempts": []
        })
    }

    fn real_cause() -> serde_json::Value {
        serde_json::json!({
            "id": "task--read",
            "state": "failed",
            "worktree": null,
            "advance_code": "launch_refused",
            "advance_owner_code": "confinement_refused",
            "advance_note": "[confinement_refused]  peer_project_unprojectable   on /fixture/project/.git",
            "attempts": [{
                "launched": true,
                "key": {"graph_id": "root-cause-fixture", "node_id": "task--read", "route_index": 0, "attempt": 3},
                "route": {"alias": "junior"},
                "state": "terminal",
                "attempt_ref": "attempt-ref",
                "outcome": "task_failed",
                "launch_actor": "fixture"
            }]
        })
    }

    #[test]
    fn root_cause_skips_upstream_blocked() {
        // The dependant comes first in topological order and carries the
        // innermost `upstream_blocked` envelope. Only the node that actually
        // failed is actionable (#4102).
        let wire = serde_json::json!([blocked_dependant(), real_cause()]);
        let cause = root_cause(&checkpoint(wire.clone()))
            .expect("a blocked graph names the node that caused it");
        assert_eq!(cause.node, "task--read");
        assert_eq!(cause.attempt, Some(3));
        assert_eq!(cause.code, "confinement_refused");
        assert_eq!(
            cause.message,
            "[confinement_refused] peer_project_unprojectable on /fixture/project/.git"
        );
        assert!(!cause.message.contains("blocked by"));
        assert_ne!(cause.node, "task--review");

        // The projected-JSON entry point must pick the same node for the
        // envelope readers in `status`, `wait`, `run` and `upgrade --verify`.
        let projected = root_cause_from_value(&serde_json::json!({"nodes": wire}))
            .expect("the projected checkpoint has a cause");
        assert_eq!(projected, cause);
    }

    #[test]
    fn root_cause_prefers_owner_code_and_skips_bare_blocked_notes() {
        let wire = serde_json::json!([
            {
                "id": "note-only-dependant",
                "state": "blocked",
                "worktree": null,
                "advance_note": "   blocked by task--read (Blocked)  ",
                "attempts": []
            },
            real_cause()
        ]);
        let cause = root_cause(&checkpoint(wire)).expect("a cause follows the dependant");
        assert_eq!(cause.node, "task--read");
        assert_eq!(cause.code, "confinement_refused");
    }

    #[test]
    fn root_cause_is_none_when_every_node_is_a_dependant() {
        let cause = root_cause(&checkpoint(serde_json::json!([
            blocked_dependant(),
            {
                "id": "another-dependant",
                "state": "blocked",
                "worktree": null,
                "advance_code": "upstream_blocked",
                "advance_note": "blocked by task--read",
                "attempts": []
            }
        ])));
        assert_eq!(cause, None);
    }

    #[test]
    fn is_upstream_blocked_is_shared_by_the_single_node_projection() {
        assert!(is_upstream_blocked(
            Some("upstream_blocked"),
            Some("a failure")
        ));
        assert!(is_upstream_blocked(
            None,
            Some("  blocked by task--read (Blocked)")
        ));
        assert!(!is_upstream_blocked(
            Some("launch_refused"),
            Some("node failed")
        ));
    }
}
