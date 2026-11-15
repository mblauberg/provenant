//! Retirement of a redone container's dynamic producer topology.
//!
//! A container redo resets its static members, but the expansions its
//! producers admitted belong to the old execution. Each is replaced in place by
//! a tombstone, so the revision (the expansion count) stays monotonic, while the
//! old nodes, their wiring and their progress rows leave the live plan. A node
//! the new execution recreates under a retired ID is rebound to a fresh retry
//! generation, so its attempt keys never collide with the retired ones.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

impl Expansion {
    pub(super) fn retired(&self) -> bool {
        self.retired.is_some()
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
            let record = RetiredExpansion {
                reason: reason.to_owned(),
                expansion: expansion.clone(),
                progress,
            };
            *expansion = Expansion {
                extra: Default::default(),
                retired: Some(Box::new(record)),
                retry_generation: None,
                final_result: false,
                final_fragment: false,
                floor_warnings: Vec::new(),
                source: String::new(),
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
    // A settled consumer keeps the container generation it observed; retired
    // members are no longer graph nodes it can name.
    for row in &mut checkpoint.nodes {
        row.settled_dependency_snapshots
            .retain(|node, _| !retired.contains(node));
    }
    Ok(retired)
}

/// Give a freshly created row that reuses a retired ID the retired history and
/// the next retry generation, so its attempt keys are new.
pub(super) fn rebind(expansions: &[Expansion], row: &mut NodeProgress) {
    let mut generation = None::<u64>;
    let mut history = Vec::new();
    for retired in expansions
        .iter()
        .filter_map(|expansion| expansion.retired.as_deref())
    {
        for old in &retired.progress {
            if old.id != row.id {
                continue;
            }
            let prior = old.retry_generation.unwrap_or_default();
            generation = Some(generation.map_or(prior, |current| current.max(prior)));
            history = old.previous_attempts.clone();
            history.extend(old.attempts.clone());
        }
    }
    if let Some(generation) = generation {
        row.retry_generation = Some(generation + 1);
        row.container_retry = true;
        row.previous_attempts = history;
    }
}
