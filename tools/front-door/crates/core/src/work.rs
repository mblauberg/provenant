use crate::{ProjectId, RunId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// An in-memory cooperative request, never a process signal or authority.
#[derive(Debug, Default, Clone)]
pub struct CancelToken(Arc<AtomicBool>);
impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Domain owners validate project membership before acting on a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "scope",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WorkScope {
    Project(ProjectId),
    Workspace {
        project: ProjectId,
        workspace: WorkspaceId,
    },
    Run {
        project: ProjectId,
        run: RunId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_reaches_clones_but_never_sibling_tokens() {
        let token = CancelToken::new();
        let clone = token.clone();
        let sibling = CancelToken::new();
        assert!(!token.is_cancelled());
        clone.cancel();
        assert!(token.is_cancelled());
        assert!(!sibling.is_cancelled());
    }
}
