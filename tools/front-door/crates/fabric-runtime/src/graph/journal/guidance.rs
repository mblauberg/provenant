use super::JournalError;
use thiserror::Error;

/// Guidance for an unreadable checkpoint, with any recovery its error allows.
pub fn unreadable_graph_explanation(graph_id: &str, _error: &JournalError) -> String {
    unreadable_graph_guidance(graph_id)
}

/// A tick notice already recorded in the dedicated unreadable marker. The
/// reason is the typed `JournalError` code that made the checkpoint unreadable.
#[derive(Debug, Error)]
#[error("{}", unreadable_graph_guidance_with_reason(.0, .1))]
pub struct UnreadableCheckpointNotice(pub String, pub &'static str);

/// Guidance for a graph whose checkpoint cannot be interpreted.
pub fn unreadable_graph_guidance(graph_id: &str) -> String {
    format!(
        "Graph {graph_id} state is unreadable. From the project and state directory that own it, run `provenant stop -- {graph_id}` to remove its record from the listing. There is no safe automatic repair. Other graphs and separately listed attempts remain available; inspect active attempts and writer fences separately."
    )
}

/// Guidance carrying the typed reason recorded in the quarantine marker.
fn unreadable_graph_guidance_with_reason(graph_id: &str, reason: &str) -> String {
    format!(
        "Graph {graph_id} state is unreadable ({reason}); it is quarantined and will not be retried until its bytes change. From the project and state directory that own it, run `provenant stop -- {graph_id}` to retire its record. There is no safe automatic repair. Other graphs and separately listed attempts remain available; inspect active attempts and writer fences separately."
    )
}
