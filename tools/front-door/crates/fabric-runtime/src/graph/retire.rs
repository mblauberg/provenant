//! Retirement of a redone container's dynamic producer topology.
//!
//! A container redo resets its static members, but the expansions its
//! producers admitted belong to the old execution. Each is replaced in place by
//! a tombstone, so the revision (the expansion count) stays monotonic, while the
//! old nodes, their wiring and their progress rows leave the live plan. A node
//! the new execution recreates under a retired ID is rebound to a fresh retry
//! generation, so its attempt keys never collide with the retired ones.

use super::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Expansion field holding the retired expansion and its progress rows.
pub(super) const RETIRED: &str = "retired";

impl Expansion {
    pub(super) fn retired(&self) -> bool {
        self.extra.contains_key(RETIRED)
    }
}

/// Tombstone every live expansion produced inside `id`'s subtree and remove
/// patch edits aimed at its nodes. Returns the retired node IDs.
pub(super) fn dynamic_subtree(
    checkpoint: &mut GraphCheckpoint,
    id: &str,
    reason: &str,
) -> Result<BTreeSet<String>, String> {
    let prefix = format!("{id}--");
    let produced = |expansion: &Expansion| {
        !expansion.retired()
            && !expansion.nodes.is_empty()
            && (expansion.source == id || expansion.source.starts_with(&prefix))
    };
    let retired: BTreeSet<String> = checkpoint
        .expansions
        .iter()
        .filter(|expansion| produced(expansion))
        .flat_map(|expansion| expansion.nodes.iter().map(|node| node.id.clone()))
        .collect();
    if retired.is_empty() {
        return Ok(retired);
    }
    let mut rows: BTreeMap<String, NodeProgress> = checkpoint
        .nodes
        .iter()
        .filter(|row| retired.contains(&row.id))
        .map(|row| (row.id.clone(), row.clone()))
        .collect();
    for expansion in &mut checkpoint.expansions {
        if produced(expansion) {
            let progress: Vec<_> = expansion
                .nodes
                .iter()
                .filter_map(|node| rows.remove(&node.id))
                .collect();
            let record = serde_json::json!({
                "reason": reason,
                "expansion": serde_json::to_value(&*expansion).map_err(|e| e.to_string())?,
                "progress": serde_json::to_value(progress).map_err(|e| e.to_string())?,
            });
            *expansion = Expansion {
                review: None,
                extra: [(RETIRED.to_owned(), record)].into_iter().collect(),
                source: String::new(),
                continuation: false,
                nodes: Vec::new(),
                uses: Vec::new(),
                patch: None,
            };
        } else if let Some(record) = &mut expansion.patch {
            // A later generation may reuse a retired ID; an old brief or skip
            // must not bind to it.
            record.replaced.retain(|node| !retired.contains(&node.id));
            record.skipped.retain(|node| !retired.contains(node));
            expansion.uses.retain_mut(|used| {
                let before = used.nodes.len();
                used.nodes.retain(|node| !retired.contains(node));
                before == 0 || !used.nodes.is_empty()
            });
        }
    }
    checkpoint.nodes.retain(|row| !retired.contains(&row.id));
    Ok(retired)
}

/// Give a freshly created row that reuses a retired ID the retired history and
/// the next retry generation, so its attempt keys are new.
pub(super) fn rebind(expansions: &[Expansion], row: &mut NodeProgress) {
    let mut generation = None::<u64>;
    let mut history = Vec::new();
    for expansion in expansions.iter().filter(|expansion| expansion.retired()) {
        let rows = expansion
            .extra
            .get(RETIRED)
            .and_then(|record| record.get("progress"))
            .and_then(Value::as_array);
        for old in rows.into_iter().flatten() {
            if old.get("id").and_then(Value::as_str) != Some(row.id.as_str()) {
                continue;
            }
            let Ok(old) = serde_json::from_value::<NodeProgress>(old.clone()) else {
                continue;
            };
            let prior = old
                .extra
                .get("retry_generation")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            generation = Some(generation.map_or(prior, |current| current.max(prior)));
            history = old.previous_attempts;
            history.extend(old.attempts);
        }
    }
    if let Some(generation) = generation {
        row.extra
            .insert("retry_generation".into(), (generation + 1).into());
        row.extra.insert("container_retry".into(), true.into());
        row.previous_attempts = history;
    }
}
