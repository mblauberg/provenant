//! One forge request context. Every blocking forge operation a caller makes
//! for one observation (repository discovery, pull request metadata, each
//! check page and the checks read itself) shares one absolute deadline and one
//! cancellation, so a waiter that settles or expires interrupts the transport
//! instead of waiting out a fresh per-call duration.
use provenant_core::CancelToken;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub(crate) struct Request {
    deadline: Instant,
    cancel: CancelToken,
}

impl Request {
    /// A request bounded by the caller's absolute `deadline` and interrupted by
    /// its `cancel` token.
    pub(crate) fn new(deadline: Instant, cancel: CancelToken) -> Self {
        Self { deadline, cancel }
    }

    /// A standalone request: `budget` from now, with its own cancellation.
    #[allow(clippy::disallowed_methods, reason = "forge request deadline boundary")]
    pub(crate) fn within(budget: Duration) -> Self {
        Self::new(
            Instant::now()
                .checked_add(budget)
                .unwrap_or_else(far_future),
            CancelToken::new(),
        )
    }

    /// This request, its deadline shortened to at most `cap` from now. The
    /// cancellation stays shared with the original.
    #[allow(clippy::disallowed_methods, reason = "forge request deadline boundary")]
    pub(crate) fn capped(&self, cap: Duration) -> Self {
        let capped = Instant::now().checked_add(cap).unwrap_or_else(far_future);
        Self::new(self.deadline.min(capped), self.cancel.clone())
    }

    /// Time left before the deadline; zero once cancelled. Each operation
    /// derives its own bound from this, never from a duration fixed earlier.
    #[allow(clippy::disallowed_methods, reason = "forge request deadline boundary")]
    pub(crate) fn remaining(&self) -> Duration {
        if self.cancel.is_cancelled() {
            Duration::ZERO
        } else {
            self.deadline.saturating_duration_since(Instant::now())
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) fn cancel(&self) -> &CancelToken {
        &self.cancel
    }
}

#[allow(clippy::disallowed_methods, reason = "forge request deadline boundary")]
fn far_future() -> Instant {
    // An unrepresentable bound is the longest wait any caller may make.
    Instant::now() + Duration::from_secs(provenant_core::MAX_LANE_RUNTIME_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capped_request_shares_cancellation_and_never_extends_the_deadline() {
        let outer = Request::within(Duration::from_secs(5));
        let inner = outer.capped(Duration::from_secs(60));
        assert!(inner.remaining() <= outer.remaining() + Duration::from_millis(50));
        let short = outer.capped(Duration::from_millis(10));
        assert!(short.remaining() <= Duration::from_millis(10));
        outer.cancel().cancel();
        for request in [&outer, &inner, &short] {
            assert!(request.is_cancelled());
            assert_eq!(request.remaining(), Duration::ZERO);
        }
    }
}
