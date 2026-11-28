use serde::{Deserialize, Serialize};

/// A graph or queue item whose owner can drive bounded progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeSubjectRef {
    Graph {
        project: provenant_core::ProjectId,
        id: String,
    },
    QueueItem {
        project: provenant_core::ProjectId,
        queue: String,
        item: String,
    },
}
