//! One forge request context. Every blocking forge operation a caller makes
//! for one observation (repository discovery, pull request metadata, each
//! check page and the checks read itself) shares one absolute deadline and one
//! cancellation, so a waiter that settles or expires interrupts the transport
//! instead of waiting out a fresh per-call duration.
use provenant_core::CancelToken;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Request {
    deadline: Instant,
    cancel: CancelToken,
}

impl Request {
    /// A request bounded by the caller's absolute `deadline` and interrupted by
    /// its `cancel` token.
    pub fn new(deadline: Instant, cancel: CancelToken) -> Self {
        Self { deadline, cancel }
    }

    /// A standalone request: `budget` from now, with its own cancellation.
    #[allow(clippy::disallowed_methods, reason = "forge request deadline boundary")]
    pub fn within(budget: Duration) -> Self {
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
    pub fn capped(&self, cap: Duration) -> Self {
        self.capped_at(Instant::now(), cap)
    }

    fn capped_at(&self, now: Instant, cap: Duration) -> Self {
        let capped = now.checked_add(cap).unwrap_or_else(far_future);
        Self::new(self.deadline.min(capped), self.cancel.clone())
    }

    /// Time left before the deadline; zero once cancelled. Each operation
    /// derives its own bound from this, never from a duration fixed earlier.
    #[allow(clippy::disallowed_methods, reason = "forge request deadline boundary")]
    pub fn remaining(&self) -> Duration {
        if self.cancel.is_cancelled() {
            Duration::ZERO
        } else {
            self.deadline.saturating_duration_since(Instant::now())
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn cancel(&self) -> &CancelToken {
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
        // Only the coordinate is opaque. Every cap uses this same injected
        // sample, so scheduling cannot change the expected deadlines.
        let now = Request::within(Duration::ZERO).deadline;
        let outer = Request::new(now + Duration::from_secs(5), CancelToken::new());
        let inner = outer.capped_at(now, Duration::from_secs(60));
        assert_eq!(inner.deadline, outer.deadline);
        let short = outer.capped_at(now, Duration::from_millis(10));
        assert_eq!(short.deadline, now + Duration::from_millis(10));
        outer.cancel().cancel();
        for request in [&outer, &inner, &short] {
            assert!(request.is_cancelled());
            assert_eq!(request.remaining(), Duration::ZERO);
        }
    }
}
