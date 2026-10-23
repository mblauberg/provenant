//! Catalogue, preferences, live probes and route decisions.

#![forbid(unsafe_code)]

#[path = "pending/casefold.rs"]
mod casefold;
mod render;

pub mod capability;
pub mod catalog;
pub mod cli;
pub mod discovery;
pub mod exec_routing;
pub mod live;
pub mod matching;
pub mod model_route;
pub mod pools;
pub mod preferences;
pub mod probe;

#[path = "pending/ordered_json.rs"]
pub mod ordered_json;
pub mod pending;

#[cfg(test)]
mod oracle;
