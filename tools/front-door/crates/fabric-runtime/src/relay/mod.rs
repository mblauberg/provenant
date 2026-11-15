//! Lane relay: the attempt-bound host endpoint through which a sandboxed lane
//! reaches coordination and its graph. Sender authority stays in the host.
mod call;
mod host;
#[cfg(test)]
mod lane_tests;

pub use call::{FacadeError, InvalidInput, ServiceError, ToolCall};
pub use host::{GRAPH_TOOL, GraphHandler, HostRelay, WORKERS, graph, host_budget};

/// The longest a relayed observation may wait, in seconds.
pub(crate) const MAX_WAIT_SECONDS: u64 = provenant_core::WaitTransport::Relay.maximum_seconds();
