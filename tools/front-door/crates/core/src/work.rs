use crate::{DurationMicros, ProjectId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// An in-memory cooperative request, never a process signal or authority.
#[derive(Default, Clone)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    probe: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.cancelled.load(Ordering::Acquire))
            .field("linked", &self.probe.is_some())
            .finish()
    }
}
impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    /// Link an execution token to durable owner intent without a watcher thread.
    /// Cancelling the linked token does not cancel its parent.
    pub fn linked(&self, probe: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        let parent = self.clone();
        Self {
            cancelled: Arc::default(),
            probe: Some(Arc::new(move || parent.is_cancelled() || probe())),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.probe.as_ref().is_some_and(|probe| probe())
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
    #[test]
    fn durable_owner_intent_reaches_linked_execution_without_cancelling_observers() {
        let observer = CancelToken::new();
        let scheduler = CancelToken::new();
        let intent = Arc::new(AtomicBool::new(false));
        let probe = intent.clone();
        let execution = scheduler.linked(move || probe.load(Ordering::Acquire));
        observer.cancel();
        assert!(!execution.is_cancelled());
        intent.store(true, Ordering::Release);
        assert!(execution.is_cancelled());
        assert!(!scheduler.is_cancelled());
    }
}
