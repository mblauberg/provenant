//! Custody-specific native process operations.
pub(crate) mod census;
pub(crate) mod handoff;
#[cfg(target_os = "macos")]
pub(crate) mod provider;
#[cfg(not(target_os = "macos"))]
#[path = "process_os/provider_unsupported.rs"]
pub(crate) mod provider;
// Exercise the unsupported backend on the dispatch platform too.
#[cfg(all(test, target_os = "macos"))]
#[allow(dead_code)]
#[path = "process_os/provider_unsupported.rs"]
mod provider_unsupported_tests;
