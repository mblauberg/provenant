//! Build store layout seam; S1 supplies clone, fetch and garbage collection.

use std::path::{Path, PathBuf};

/// Store path for a project key.
pub fn path(state: &Path, project_key: &str) -> PathBuf {
    super::paths::store_path(state, project_key)
}

/// Ensure the bare store exists. S0 leaves creation to the slot fallback.
pub fn ensure(_state: &Path, _project_key: &str) -> Result<PathBuf, super::BuildError> {
    Err(super::BuildError::Unsupported(
        "build store is not implemented in S0".into(),
    ))
}
