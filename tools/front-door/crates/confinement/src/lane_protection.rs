//! Typed lane gate requests, errors, and release records.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

mod unit_01;
pub use unit_01::{is_full_sha, validate_release, validate_revision};

pub const RELEASE_DIRECTORY: &str = "provenant-instruction-checks/releases";
pub const RELEASE_EXTENSION: &str = ".json";
pub const DEFAULT_BASE: &str = "HEAD";

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtectionError {
    #[error("protected_instructions_unverifiable: {0}")]
    Unverifiable(String),
    #[error("protected_instructions_changed: {0}")]
    Changed(String),
}

impl ProtectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unverifiable(_) => "protected_instructions_unverifiable",
            Self::Changed(_) => "protected_instructions_changed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckRequest {
    pub common: PathBuf,
    pub tip: String,
    pub base: String,
}

/// A release is an audit record. Only the owner's write boundary confers authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseRecord {
    pub tip: String,
    pub actor: String,
    pub reason: String,
    pub worktree: PathBuf,
    /// Supplied by the clock owner, preserving UTC ISO spelling and fractional precision.
    pub released_at: String,
}
