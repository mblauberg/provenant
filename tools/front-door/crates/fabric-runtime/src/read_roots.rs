//! Directories a graph node may read, confined to the project and the host's
//! `[domains.fabric.graph] allowed_read_roots` allow-list.
use std::path::{Path, PathBuf};

/// The project's primary checkout and registered worktrees, plus the host's
/// explicit allow-list. Roots that cannot be resolved are dropped.
#[derive(Debug, Default)]
pub struct ReadRoots {
    project: Vec<PathBuf>,
    allowed: Vec<PathBuf>,
}

impl ReadRoots {
    pub fn new(project: Vec<PathBuf>, allowed: &[String]) -> Self {
        let canonical = |path: &Path| provenant_os::fs::canonicalize(path).ok();
        Self {
            project: project.iter().filter_map(|root| canonical(root)).collect(),
            allowed: allowed
                .iter()
                .filter_map(|root| canonical(Path::new(root)))
                .collect(),
        }
    }

    /// Resolve symlinks and `..` before comparing, and return the canonical path.
    pub fn confine(&self, path: &Path) -> Result<PathBuf, String> {
        let canonical = provenant_os::fs::canonicalize(path)
            .map_err(|e| format!("read root {}: {e}", path.display()))?;
        if self
            .project
            .iter()
            .chain(&self.allowed)
            .any(|root| canonical.starts_with(root))
        {
            return Ok(canonical);
        }
        Err(format!(
            "read root {} is outside this project's registered worktrees; add it to [domains.fabric.graph] allowed_read_roots to permit it",
            canonical.display()
        ))
    }
}
