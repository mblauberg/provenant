//! Retry timing and transient failure classification, without clocks or sleeping.
use std::time::Duration;

/// A capped exponential delay. The caller owns the attempt count and entropy.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    first: Duration,
    factor: u32,
    cap: Duration,
}

impl Backoff {
    /// A factor of one gives a fixed delay. Attempts are zero based.
    pub const fn new(first: Duration, factor: u32, cap: Duration) -> Self {
        Self { first, factor, cap }
    }

    pub fn delay(self, attempt: u64) -> Duration {
        let mut delay = self.first.min(self.cap);
        for _ in 0..attempt {
            let next = delay.saturating_mul(self.factor).min(self.cap);
            if next == delay {
                break;
            }
            delay = next;
        }
        delay
    }

    /// Optionally add caller-supplied jitter, still bounded by the cap.
    /// Supplying the jitter makes tests deterministic and needs no RNG here.
    pub fn delay_with_jitter(self, attempt: u64, jitter: Duration) -> Duration {
        self.delay(attempt).saturating_add(jitter).min(self.cap)
    }
}

/// Error adapters retain typed domain knowledge; this owner decides whether a
/// cause is transient. Deadlines and mutation replay safety stay with callers.
pub enum RetryCause<'a> {
    Contention,
    Timeout,
    Cancelled,
    Transport,
    Permanent,
    CommandOutput { stderr: &'a str, stdout: &'a str },
}

pub fn is_transient(cause: RetryCause<'_>) -> bool {
    match cause {
        RetryCause::Contention
        | RetryCause::Timeout
        | RetryCause::Cancelled
        | RetryCause::Transport => true,
        RetryCause::Permanent => false,
        RetryCause::CommandOutput { stderr, stdout } => {
            let text = format!("{stderr}\n{stdout}").to_lowercase();
            [
                "http 5",
                "http 429",
                "rate limit",
                "abuse detection",
                "timed out",
                "timeout",
                "connection reset",
                "connection refused",
                "unexpected eof",
                "no such host",
                "could not resolve host",
                "temporary failure",
                "tls handshake",
                "network is unreachable",
                "bad gateway",
                "service unavailable",
            ]
            .iter()
            .any(|marker| text.contains(marker))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_delays_cap_and_saturate() {
        let policy = Backoff::new(Duration::from_millis(100), 2, Duration::from_secs(2));
        let millis: Vec<_> = (0..7).map(|n| policy.delay(n).as_millis()).collect();
        assert_eq!(millis, [100, 200, 400, 800, 1600, 2000, 2000]);
        assert_eq!(policy.delay(u64::MAX), Duration::from_secs(2));
        assert_eq!(
            Backoff::new(Duration::MAX, 2, Duration::MAX).delay(1),
            Duration::MAX
        );
        assert_eq!(
            Backoff::new(Duration::from_secs(5), 1, Duration::from_secs(5)).delay(u64::MAX),
            Duration::from_secs(5)
        );
        assert_eq!(
            Backoff::new(Duration::ZERO, 2, Duration::MAX).delay(u64::MAX),
            Duration::ZERO
        );
        assert_eq!(
            Backoff::new(Duration::from_secs(5), 0, Duration::MAX).delay(u64::MAX),
            Duration::ZERO
        );
    }

    #[test]
    fn settlement_schedule_and_optional_jitter_are_deterministic() {
        let policy = Backoff::new(Duration::from_secs(5), 6, Duration::from_secs(120));
        assert_eq!(
            (0..4)
                .map(|n| policy.delay(n).as_secs())
                .collect::<Vec<_>>(),
            [5, 30, 120, 120]
        );
        assert_eq!(policy.delay_with_jitter(0, Duration::ZERO), policy.delay(0));
        assert_eq!(
            policy.delay_with_jitter(0, Duration::from_secs(2)),
            Duration::from_secs(7)
        );
        assert_eq!(
            policy.delay_with_jitter(1, Duration::MAX),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn classifies_typed_causes_and_all_command_markers() {
        for cause in [
            RetryCause::Contention,
            RetryCause::Timeout,
            RetryCause::Cancelled,
            RetryCause::Transport,
        ] {
            assert!(is_transient(cause));
        }
        assert!(!is_transient(RetryCause::Permanent));
        for text in [
            "HTTP 500",
            "HTTP 429",
            "RATE LIMIT",
            "abuse detection",
            "timed out",
            "timeout",
            "connection reset",
            "connection refused",
            "unexpected EOF",
            "no such host",
            "could not resolve host",
            "temporary failure",
            "TLS handshake",
            "network is unreachable",
            "bad gateway",
            "service unavailable",
        ] {
            assert!(
                is_transient(RetryCause::CommandOutput {
                    stderr: text,
                    stdout: ""
                }),
                "{text}"
            );
            assert!(
                is_transient(RetryCause::CommandOutput {
                    stderr: "",
                    stdout: text
                }),
                "{text}"
            );
        }
        for text in [
            "",
            "HTTP 401: Bad credentials",
            "HTTP 403: Resource not accessible",
            "HTTP 404: Not Found",
            "no pull requests found",
        ] {
            assert!(
                !is_transient(RetryCause::CommandOutput {
                    stderr: text,
                    stdout: ""
                }),
                "{text}"
            );
        }
    }
}
