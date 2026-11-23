//! One-shot migrations for non-SQLite state: files, ledgers and uninstallable
//! metadata that no store owns. SQLite stores migrate on writable open
//! (`provenant_sqlite::Migration` in `crates/sqlite/src/schema.rs`); this
//! runner never duplicates or wraps that mechanism.
use crate::warning::Warning;
use std::path::Path;

/// What one step changed, for the install report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    /// The step's stable id. It matches [`Migration::id`].
    pub id: &'static str,
    /// The state paths the step changed, for the install report.
    pub changed: Vec<String>,
    /// Warnings to surface after the step succeeds.
    pub warnings: Vec<Warning>,
}

impl MigrationReport {
    /// A report for `id` that changed nothing and warns about nothing.
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            changed: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// One idempotent step over non-SQLite state.
pub trait Migration {
    /// Stable id, recorded in the install receipt once applied.
    fn id(&self) -> &'static str;
    /// Must be idempotent: a rerun after a partial failure finishes the job.
    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError>;
}

/// A step could not finish. Its id is not recorded, so the next install
/// retries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationError {
    message: String,
}

impl MigrationError {
    /// A failure with a person-readable reason.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MigrationError {}

impl From<std::io::Error> for MigrationError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}
