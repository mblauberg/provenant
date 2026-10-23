//! Inert Fabric runtime contracts and policy. No command is switched to this crate.
#![forbid(unsafe_code)]

pub mod batch_execution;
pub mod catalogue;
pub mod execution;
pub mod execution_input;
pub mod fabric_policy;
pub mod field;
pub mod gates;
pub mod git_transport;
pub mod graph_document;
pub mod host_protocol;
pub mod hosts;
pub mod identity;
pub mod json_subprocess;
pub mod lane_federation;
pub mod lane_watch;
pub mod pools;
pub mod ps;
pub mod run_registry;
pub mod run_root;
pub mod success_status;

mod pending;
mod policy_json;

#[cfg(test)]
mod test_support;
