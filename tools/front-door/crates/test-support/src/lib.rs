//! Shared helpers for bounded asynchronous tests.

use std::time::{Duration, Instant};

/// Poll `condition` until it succeeds or the real-time deadline expires.
// The helper's purpose is to bound tests by elapsed wall time.
#[allow(clippy::disallowed_methods)]
pub fn eventually(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::park_timeout(remaining.min(Duration::from_millis(5)));
    }
}
