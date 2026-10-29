//! Fabric identity, home-host coordination and bounded federation.
#![forbid(unsafe_code)]
pub mod chairs;
pub mod config;
pub mod coordination;
pub mod fetch;
pub mod git_transport;
pub mod graph;
pub mod handoff;
pub mod home;
pub mod hosts;
pub mod identity;
pub mod inbox;
pub mod peer_agent;
pub use provenant_records::result_capture;
#[cfg(test)]
mod forward_compatibility;
pub mod runs;
#[cfg(test)]
mod test_support;

/// Store failures shared with host composition edges.
pub use provenant_fabric_store::Error as StoreError;
