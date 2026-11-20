//! Snapshot transfer seam; S1 supplies Git snapshot creation.

use super::{BuildError, GitBinding, SnapshotSource};
use std::path::Path;

/// A commit pinned in the project store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRef {
    pub commit: String,
    pub ref_name: String,
    pub base: Option<String>,
}

/// Materialise a source into the persistent store. The S0 implementation
/// deliberately reports that snapshot support is not available yet.
pub fn create(
    _source: &SnapshotSource,
    _binding: Option<&GitBinding>,
    _store: &Path,
    _owner: &str,
) -> Result<SnapshotRef, BuildError> {
    Err(BuildError::Unsupported(
        "build snapshots are not implemented in S0".into(),
    ))
}
