//! The format-2 work identity: project, graph, node and attempt number.
//! A plain lane is a one-node graph, so every attempt has exactly one key.
use provenant_core::ProjectId;
use serde::{Deserialize, Serialize};

/// Longest graph identifier, as the graph document validates it.
pub const GRAPH_ID_BYTES: usize = 64;
/// Longest node identifier, as the graph document validates it.
pub const NODE_ID_BYTES: usize = 200;

/// One attempt of one node of one graph in one project. The key is the only
/// work identity: it replaces run, task and operation identifiers. Its text
/// form is the command target `GRAPH/NODE#N`; the project is implied by the
/// repository a command runs in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "WorkKeyWire")]
pub struct WorkKey {
    project: ProjectId,
    graph: String,
    node: String,
    attempt: u32,
}

/// Unknown fields from a newer writer are ignored, not kept: a key's identity
/// is its four fields.
#[derive(Deserialize)]
struct WorkKeyWire {
    project: ProjectId,
    graph: String,
    node: String,
    attempt: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkKeyError {
    #[error("graph identifier must be 1..=64 bytes of ASCII letters, digits, '-', '_' or '.'")]
    Graph,
    #[error("node identifier must be 1..=200 bytes of ASCII letters, digits, '-', '_' or '.'")]
    Node,
    #[error("attempt numbers start at 1")]
    Attempt,
}

fn identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// A node identifier as the graph document accepts it.
pub fn valid_node(node: &str) -> bool {
    identifier(node, NODE_ID_BYTES)
}

impl WorkKey {
    pub fn new(
        project: ProjectId,
        graph: impl Into<String>,
        node: impl Into<String>,
        attempt: u32,
    ) -> Result<Self, WorkKeyError> {
        let (graph, node) = (graph.into(), node.into());
        if !identifier(&graph, GRAPH_ID_BYTES) {
            return Err(WorkKeyError::Graph);
        }
        if !valid_node(&node) {
            return Err(WorkKeyError::Node);
        }
        if attempt == 0 {
            return Err(WorkKeyError::Attempt);
        }
        Ok(Self {
            project,
            graph,
            node,
            attempt,
        })
    }

    pub fn project(&self) -> ProjectId {
        self.project
    }

    pub fn graph(&self) -> &str {
        &self.graph
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    /// One-based attempt number within the node.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The key of the attempt that follows this one on the same node.
    pub fn next_attempt(&self) -> Option<Self> {
        Some(Self {
            attempt: self.attempt.checked_add(1)?,
            ..self.clone()
        })
    }
}

impl TryFrom<WorkKeyWire> for WorkKey {
    type Error = WorkKeyError;

    fn try_from(wire: WorkKeyWire) -> Result<Self, Self::Error> {
        Self::new(wire.project, wire.graph, wire.node, wire.attempt)
    }
}

impl std::fmt::Display for WorkKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}#{}", self.graph, self.node, self.attempt)
    }
}
