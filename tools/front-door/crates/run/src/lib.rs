//! Dispatch, attempts, admission, batch, retry and publication orchestration.
#![forbid(unsafe_code)]

pub mod detached;
pub mod lease;
pub mod lineage;
mod pending;

#[cfg(test)]
mod oracle;
