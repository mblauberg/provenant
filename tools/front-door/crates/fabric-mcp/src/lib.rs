//! Fabric MCP stdio server, tool registration and result rendering.
#![forbid(unsafe_code)]
pub mod protocol;
pub mod render;
pub mod schema;
pub mod stdio;
pub mod tools;

pub(crate) const MAX_WAIT_SECONDS: u64 = provenant_core::WaitTransport::Mcp.maximum_seconds();

#[cfg(test)]
mod coordination_tests;
