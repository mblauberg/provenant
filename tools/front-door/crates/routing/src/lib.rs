//! Catalogue, preferences, live probes and route decisions.

#![forbid(unsafe_code)]

mod casefold;
mod render;

pub mod catalog;
pub mod capability;
pub mod cli;
pub mod exec_routing;
pub mod live;
pub mod matching;
pub mod model_route;
pub mod pools;
pub mod preferences;
pub mod probe;

pub mod ordered_json;
pub mod pending;

#[cfg(test)]
mod oracle;
