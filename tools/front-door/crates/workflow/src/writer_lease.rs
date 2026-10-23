//! Writer retry and branch-name policy. Lease effects remain with the safe OS owner.
use crate::{PolicyError, policy::hashable};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const WORKTREE_WRITER_LOCK: &str = "provenant-dispatch-writer.lock";
pub const GRAPH_RESERVATION_FILE: &str = "provenant-graph-reservation.json";
pub const WORKTREE_WRITE_ADAPTERS: &[&str] = &[
    "claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot",
];
pub const STARTUP_WALK_SKIP: &[&str] = &[
    ".git",
    ".agent-run",
    "node_modules",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    "dist",
    "build",
];
pub const STARTUP_WALK_MAX_ENTRIES: u64 = 200_000;
pub const STARTUP_WALK_MAX_SECONDS: f64 = 5.0;
const RETRY_FIX: &str = "inspect the worktree: it changed or was dirty when the writer stopped; then dispatch again on another model or adapter";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSnapshot {
    pub head: String,
    pub porcelain: Vec<u8>,
}

/// Unknown from a missing or dirty start, matching the source's tri-state result.
pub fn worktree_unchanged(
    start: Option<&WorktreeSnapshot>,
    current: Option<&WorktreeSnapshot>,
) -> Option<bool> {
    let start = start.filter(|s| s.porcelain.is_empty())?;
    current.map(|current| current == start)
}

/// `modified_since` is the bounded filesystem observation for launched - 1.0
/// seconds, including ignored entries and fail-closed unreadable/cap results.
/// A caller supplies both observations; this policy does not acquire a lease.
pub fn guard_writer_retry(
    record: &mut Map<String, Value>,
    start: Option<&WorktreeSnapshot>,
    current: Option<&WorktreeSnapshot>,
    modified_since: bool,
) -> Result<(), PolicyError> {
    let status = record.get("status").unwrap_or(&Value::Null);
    hashable(status, "status")?;
    if !status
        .as_str()
        .is_some_and(|s| ["startup_timeout", "rate_limited"].contains(&s))
    {
        return Ok(());
    }
    if start.is_some_and(|state| {
        state.porcelain.is_empty() && current == Some(state) && !modified_since
    }) {
        return Ok(());
    }
    record.insert("retryable".into(), Value::Bool(false));
    record.insert("fix".into(), Value::String(RETRY_FIX.into()));
    Ok(())
}

pub fn branch_warning(branch: &str) -> Result<Option<String>, regex::Error> {
    let pattern = Regex::new(
        r"\A(?:(?:feat|fix|refactor|perf|test|docs|chore|ci|build|proto)/[a-z][a-z0-9]*-[a-z0-9]+(?:-[a-z0-9]+){1,4}|land/[0-9]{8}-[a-z0-9]+(?:-[a-z0-9]+)*)\z",
    )?;
    if branch.chars().count() <= 48 && pattern.is_match(branch) {
        Ok(None)
    } else {
        Ok(Some(format!(
            "branch {branch} is outside <type>/<area>-<slug> (or land/, proto/); rename for repository convention"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn retry_requires_a_clean_equal_snapshot_and_no_ignored_file_changes() {
        let clean = WorktreeSnapshot {
            head: "h".into(),
            porcelain: vec![],
        };
        let dirty = WorktreeSnapshot {
            head: "h".into(),
            porcelain: b"M file".to_vec(),
        };
        for status in ["startup_timeout", "rate_limited", "ok"] {
            for start in [None, Some(&clean), Some(&dirty)] {
                for current in [None, Some(&clean), Some(&dirty)] {
                    for modified in [false, true] {
                        let mut record = json!({"status":status,"retryable":true})
                            .as_object()
                            .unwrap()
                            .clone();
                        guard_writer_retry(&mut record, start, current, modified).unwrap();
                        assert_eq!(
                            record["retryable"],
                            status == "ok"
                                || (start == Some(&clean) && current == Some(&clean) && !modified)
                        );
                    }
                }
            }
        }
        assert_eq!(worktree_unchanged(Some(&dirty), Some(&clean)), None);
        assert_eq!(worktree_unchanged(Some(&clean), None), None);
        assert_eq!(worktree_unchanged(Some(&clean), Some(&dirty)), Some(false));
    }
    #[test]
    fn branch_name_contract_matches_python_without_running_git() {
        let branches = json!([
            "feat/workflow-port-batch",
            "fix/a-b-c",
            "land/20261023-port-workflow",
            "proto/workflow-test-one",
            "port/workflow-2",
            "(detached HEAD)",
            "feat/a-b",
            "feat/a-b-c-d-e-f-g-h",
            "feat/area-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnop",
            "feat/a-b-c\n"
        ]);
        let actual: Vec<_> = branches
            .as_array()
            .unwrap()
            .iter()
            .map(|b| json!(branch_warning(b.as_str().unwrap()).unwrap()))
            .collect();
        if let Some(expected) = crate::test_support::oracle("writer_branch", &branches) {
            assert_eq!(json!(actual), expected);
        }
    }
}
