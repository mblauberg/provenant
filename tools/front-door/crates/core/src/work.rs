use crate::{DurationMicros, ProjectId, RunId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
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

/// A cancellable pause between observations of a genuine wait. Injected so
/// simulations can advance their own clock instead of sleeping.
pub trait Pause {
    fn wait(&self, delay: DurationMicros, cancel: &CancelToken);
}

/// Sleeps on the thread, waking every 10 ms to observe cancellation.
pub struct ThreadPause;

impl Pause for ThreadPause {
    // A pause must observe cancellation against real elapsed time.
    #[allow(clippy::disallowed_methods)]
    fn wait(&self, delay: DurationMicros, cancel: &CancelToken) {
        let started = Instant::now();
        let delay = Duration::from_micros(delay.as_micros().min(250_000));
        while !cancel.is_cancelled() {
            let remaining = delay.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            std::thread::park_timeout(remaining.min(Duration::from_millis(10)));
        }
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
