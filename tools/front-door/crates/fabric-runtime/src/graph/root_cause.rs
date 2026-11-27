//! Select the failure that caused a graph to fail.

use super::GraphCheckpoint;
use serde::{Deserialize, Serialize};

/// The actionable failure behind a graph's blocked dependants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCause {
    pub node: String,
    pub attempt: Option<u32>,
    pub code: String,
    pub message: String,
}

/// Select the first detailed failure, skipping nodes blocked by an upstream
/// failure. Owner codes are preferred because they identify the original
/// operation rather than the graph's wrapper disposition.
pub fn root_cause(checkpoint: &GraphCheckpoint) -> Option<RootCause> {
    checkpoint.nodes.iter().find_map(|node| {
        let message = node.advance_note.as_deref()?.trim();
        if message.is_empty()
            || node.advance_code.as_deref() == Some("upstream_blocked")
            || message.starts_with("blocked by")
        {
            return None;
        }

        let code = node
            .advance_owner_code
            .as_deref()
            .filter(|code| !code.is_empty())
            .or_else(|| node.advance_code.as_deref().filter(|code| !code.is_empty()))
            .unwrap_or("unknown")
            .to_owned();

        Some(RootCause {
            node: node.id.clone(),
            attempt: node.attempts.last().map(|attempt| attempt.key.attempt),
            code,
            message: message.split_whitespace().collect::<Vec<_>>().join(" "),
        })
    })
}
