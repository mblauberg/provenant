//! Fabric MCP stdio server, tool registration and result rendering.
#![forbid(unsafe_code)]
pub mod protocol;
pub mod render;
pub mod schema;
pub mod stdio;
pub mod tools;

#[cfg(test)]
mod coordination_tests;
