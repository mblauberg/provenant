//! Bounded CI polling for a pinned PR head. No progress goes to JSON stdout.
use super::{Result, execute_for, observer::ObserveError};
use provenant_core::retry::{Backoff, RetryCause, is_transient};
use provenant_os::exec::Termination;
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(super) struct Failure {
    pub code: &'static str,
    pub message: String,
}
pub(super) fn failure(
    code: &'static str,
    message: impl Into<String>,
) -> Box<dyn std::error::Error> {
    Box::new(Failure {
        code,
        message: message.into(),
    })
}
/// First and longest pause between retries of a transient GitHub read.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LAST: Duration = Duration::from_secs(15);

/// Reads GitHub within one deadline, retrying transient failures with backoff.
/// Persistent or non-transient failures return at once for the caller to refuse.
pub(super) struct Reader<'a> {
    pub project: &'a Path,
    pub deadline: Instant,
    pub retry_command: String,
}
impl Reader<'_> {
    #[allow(clippy::disallowed_methods)]
    pub fn read(&self, argv: &[&str]) -> Result<provenant_os::exec::ExecOutcome> {
        self.request(argv, false)
    }

    #[allow(clippy::disallowed_methods)]
    pub fn request(
        &self,
        argv: &[&str],
        mutation: bool,
    ) -> Result<provenant_os::exec::ExecOutcome> {
        let backoff = Backoff::new(RETRY_FIRST, 2, RETRY_LAST);
        let mut retries = 0_u64;
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(failure("CI_TIMEOUT", "GitHub wait budget expired"));
            }
            let mut args = argv.to_vec();
            if args.starts_with(&["gh", "api"]) {
                args.insert(2, "--include");
            }
            let mut output = match execute_for(
                self.project,
                &args,
                left.min(Duration::from_secs(60)),
                false,
            ) {
                Ok(output) => output,
                Err(error) if !mutation && error.is::<super::TimedOut>() => {
                    let left = self.deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(backoff.delay(retries).min(left));
                    retries = retries.saturating_add(1);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let (headers, body) = strip_headers(&output.stdout.bytes);
            output.stdout.bytes = body;
            if output.termination == Termination::Exit(0) {
                return Ok(output);
            }
            if let Some(status) = http_status(&headers) {
                output
                    .stderr
                    .bytes
                    .extend_from_slice(format!("\nHTTP {status}\n").as_bytes());
            }
            let stderr = String::from_utf8_lossy(&output.stderr.bytes);
            let stdout = String::from_utf8_lossy(&output.stdout.bytes);
            let limited = is_rate_limit(&headers, &stderr, &stdout);
            if !limited
                && (mutation
                    || !is_transient(RetryCause::CommandOutput {
                        stderr: &stderr,
                        stdout: &stdout,
                    }))
            {
                return Ok(output);
            }
            if limited {
                // A rate limit skips this read rather than sleeping to the
                // reset: the caller resubmits once the quota returns.
                let (_, reset) = rate_delay(&headers, Duration::from_secs(60));
                eprintln!("warning: GitHub rate limit; skipped this read; reset {reset}");
                return Err(failure(
                    "RATE_LIMITED",
                    format!(
                        "GitHub rate limit; reset {reset}; next: {}",
                        self.retry_command
                    ),
                ));
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(backoff.delay(retries).max(Duration::from_secs(1)).min(left));
            retries = retries.saturating_add(1);
        }
    }
}

// `gh api --include --paginate --slurp` interleaves headers with the
// slurped JSON array. Remove each response header while retaining the pages.
fn strip_headers(bytes: &[u8]) -> (String, Vec<u8>) {
    let mut headers = String::new();
    let mut body = Vec::new();
    let mut position = 0;
    let mut quoted = false;
    let mut escaped = false;
    while position < bytes.len() {
        if !quoted
            && bytes[position..].starts_with(b"HTTP/")
            && (position == 0 || matches!(bytes[position - 1], b'\n' | b'[' | b','))
        {
            let rest = &bytes[position..];
            let end = rest
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .map(|i| (i, 4))
                .or_else(|| {
                    rest.windows(2)
                        .position(|part| part == b"\n\n")
                        .map(|i| (i, 2))
                });
            if let Some((end, separator)) = end {
                headers.push_str(&String::from_utf8_lossy(&rest[..end]));
                headers.push('\n');
                position += end + separator;
                continue;
            }
        }
        let byte = bytes[position];
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        }
        body.push(byte);
        position += 1;
    }
    (headers, body)
}
fn http_status(headers: &str) -> Option<u16> {
    headers
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            parts
                .next()?
                .starts_with("HTTP/")
                .then(|| parts.next()?.parse().ok())
                .flatten()
        })
        .next_back()
}
fn is_rate_limit(headers: &str, stderr: &str, stdout: &str) -> bool {
    let text = format!("{headers} {stderr} {stdout}").to_ascii_lowercase();
    text.lines()
        .any(|line| line.starts_with("http/") && line.split_whitespace().nth(1) == Some("429"))
        || text.contains("http 429")
        || text.contains("rate limit")
        || text.contains("abuse detection")
        || (http_status(headers) == Some(403)
            && (text.contains("x-ratelimit-remaining: 0") || text.contains("retry-after:")))
}
#[allow(clippy::disallowed_methods)]
fn rate_delay(headers: &str, fallback: Duration) -> (Duration, String) {
    rate_delay_at(headers, fallback, jiff::Timestamp::now().as_second())
}

/// The retry delay and the time it ends, for a response received at `now`
/// (Unix seconds).
fn rate_delay_at(headers: &str, fallback: Duration, now: i64) -> (Duration, String) {
    let mut retry = None;
    let mut reset = None;
    let mut remaining = None;
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("retry-after") {
            retry = value.trim().parse::<u64>().ok().or_else(|| {
                let date = value.trim().strip_suffix(" GMT")?;
                let timestamp =
                    jiff::Timestamp::strptime("%a, %d %b %Y %H:%M:%S %z", format!("{date} +0000"))
                        .ok()?;
                Some(timestamp.as_second().saturating_sub(now).max(0) as u64)
            });
        } else if name.eq_ignore_ascii_case("x-ratelimit-reset") {
            reset = value.trim().parse::<i64>().ok();
        } else if name.eq_ignore_ascii_case("x-ratelimit-remaining") {
            remaining = value.trim().parse::<u64>().ok();
        }
    }
    // A secondary limit can include the unrelated primary window's headers.
    // Only an exhausted primary window (remaining exactly 0) makes its reset
    // the retry time; otherwise retry after a minute rather than waiting for
    // that window to reset.
    let reset = reset.filter(|_| remaining == Some(0));
    let fallback = fallback.max(Duration::from_secs(60));
    let delay = retry
        .map(Duration::from_secs)
        .or_else(|| reset.map(|at| Duration::from_secs(at.saturating_sub(now).max(0) as u64)))
        .unwrap_or(fallback);
    // Report the time the chosen delay ends, whichever header chose it.
    let at = now.saturating_add(i64::try_from(delay.as_secs()).unwrap_or(i64::MAX));
    let label = if reset.is_none() && retry.is_none() {
        "unknown".to_owned()
    } else {
        jiff::Timestamp::from_second(at).map_or_else(|_| at.to_string(), |time| time.to_string())
    };
    (delay, label)
}
/// The failure a land wait reports for an observer error.
pub(super) fn ci_failure(error: ObserveError, retry_command: &str) -> Box<dyn std::error::Error> {
    match error {
        ObserveError::RateLimited { reset } => failure(
            "RATE_LIMITED",
            format!("GitHub rate limit; reset {reset}; next: {retry_command}"),
        ),
        ObserveError::Expired => failure("CI_TIMEOUT", "GitHub wait budget expired"),
        ObserveError::Unavailable(message) => failure("CI_UNAVAILABLE", message),
    }
}

#[cfg(test)]
#[allow(dead_code, unused_imports)] // Also compiled, unrun, by the harness-less worktrees driver.
mod tests {
    use super::super::observer::Check;
    use super::super::status::{CiCheck, CiCompletion, Required, ci_completion};
    use super::*;
    use provenant_core::GitOid;
    use serde_json::json;

    fn state_for(head: &str, checks: &[Check], required: &[Required]) -> CiCompletion {
        let normalized: Vec<_> = checks
            .iter()
            .map(|check| CiCheck {
                head: check.head.clone(),
                name: check.name.clone(),
                bucket: check.bucket.clone(),
                app: check.app,
            })
            .collect();
        ci_completion(head, &normalized, required)
    }
    fn state(checks: &[Check], required: &[Required]) -> CiCompletion {
        let head = checks.first().map_or("", |check| check.head.as_str());
        state_for(head, checks, required)
    }

    fn head() -> GitOid {
        GitOid::parse(&"1".repeat(40)).expect("head")
    }
    fn at(text: &str) -> jiff::Timestamp {
        text.parse().expect("timestamp")
    }
    fn check(name: &str, bucket: &str) -> Check {
        Check {
            head: "head".into(),
            name: name.into(),
            bucket: bucket.into(),
            app: None,
            app_slug: None,
            conclusion: None,
            registered: Some(at("2020-01-01T00:00:00Z")),
            url: None,
            excerpt: String::new(),
        }
    }
    fn need(name: &str) -> Required {
        Required {
            name: name.into(),
            app: None,
        }
    }

    #[test]
    fn recorded_rollups_only_complete_green_for_their_current_head() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/ci-rollups.json")).expect("fixture");
        let head = fixture["head"].as_str().expect("head");
        let required = [need("build")];
        let parse = |key: &str| {
            let rollup = &fixture[key];
            let runs = json!({
                "total_count": rollup["total_count"],
                "check_runs": rollup["check_runs"]
            });
            let statuses = rollup["statuses"].clone();
            super::super::observer::parse_pages(&[runs], &[statuses], head)
        };

        let queued = parse("queued_required").expect("queued rollup");
        assert_eq!(state_for(head, &queued, &required), CiCompletion::Pending);

        let stale = &fixture["stale_pass"];
        let stale_page = json!({
            "total_count": stale["total_count"],
            "check_runs": stale["check_runs"]
        });
        assert!(
            super::super::observer::parse_pages(&[stale_page], &[stale["statuses"].clone()], head)
                .is_none()
        );

        let empty = parse("no_checks").expect("empty rollup");
        assert_eq!(state_for(head, &empty, &[]), CiCompletion::Pending);

        let green = parse("all_green").expect("green rollup");
        assert_eq!(state_for(head, &green, &required), CiCompletion::Green);
    }

    #[test]
    fn rate_limit_headers_include_429_and_retry_after_dates() {
        assert!(is_rate_limit("HTTP/2 429 Too Many Requests", "", ""));
        assert!(!is_rate_limit(
            "HTTP/2 409 Conflict\r\nX-RateLimit-Remaining: 0",
            "",
            ""
        ));
        assert!(!is_rate_limit(
            "HTTP/2 405 Method Not Allowed",
            "not mergeable",
            ""
        ));
        assert!(!is_rate_limit(
            "",
            "unexpected gh arguments: api --include repos/owner/repo/pulls/7",
            ""
        ));
        let (delay, reset) = rate_delay(
            "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 4102444800\r\n",
            Duration::from_secs(1),
        );
        assert!(delay > Duration::from_secs(60));
        assert!(reset.starts_with("2100-01-01"), "{reset}");
        // Without an explicit zero remaining count the reset belongs to an
        // unrelated primary window and must not set the wait.
        let (delay, reset) =
            rate_delay("X-RateLimit-Reset: 4102444800\r\n", Duration::from_secs(1));
        assert_eq!(delay, Duration::from_secs(60));
        assert_eq!(reset, "unknown");
        // Retry-After chooses the delay, so the reported time follows it, not
        // the later primary reset.
        // 2026-11-03T00:00:00Z; the retry ends 17 s later, not at the reset.
        let (delay, reset) = rate_delay_at(
            "Retry-After: 17\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 4102444800\r\n",
            Duration::from_secs(1),
            1_793_664_000,
        );
        assert_eq!(delay, Duration::from_secs(17));
        assert_eq!(reset, "2026-11-03T00:00:17Z");
        let (delay, _) = rate_delay("Retry-After: 17\r\n", Duration::from_secs(1));
        assert_eq!(delay, Duration::from_secs(17));
        let (delay, reset) = rate_delay(
            "X-RateLimit-Remaining: 100\r\nX-RateLimit-Reset: 4102444800\r\n",
            Duration::from_secs(1),
        );
        assert_eq!(delay, Duration::from_secs(60));
        assert_eq!(reset, "unknown");
        let (delay, reset) = rate_delay("", Duration::from_secs(1));
        assert_eq!(delay, Duration::from_secs(60));
        assert_eq!(reset, "unknown");
        let (delay, reset) = rate_delay(
            "Retry-After: Fri, 01 Jan 2100 00:00:00 GMT\r\n",
            Duration::from_secs(1),
        );
        assert!(delay > Duration::from_secs(60));
        assert!(reset.starts_with("2100-01-01"), "{reset}");
    }

    #[test]
    fn paginated_headers_are_removed_without_losing_pages() {
        let response = b"[HTTP/2 200 OK\r\nX-RateLimit-Remaining: 10\r\n\r\n{\"page\":1},\nHTTP/2 200 OK\r\nX-RateLimit-Remaining: 9\r\n\r\n{\"page\":2}]";
        let (headers, body) = strip_headers(response);
        assert!(headers.contains("X-RateLimit-Remaining: 9"));
        let pages: serde_json::Value = serde_json::from_slice(&body).expect("pages");
        assert_eq!(pages, json!([{"page":1},{"page":2}]));
        // Header-looking text and escaped quotes in JSON must not consume
        // the rest of a page or the next page's real headers.
        let response = b"[HTTP/2 200 OK\r\n\r\n{\"summary\":\"[HTTP/2,HTTP/ \\\"quoted\\\" \\\\ tail\"},HTTP/2 200 OK\r\n\r\n{\"page\":2}]";
        let (_, body) = strip_headers(response);
        let pages: serde_json::Value = serde_json::from_slice(&body).expect("pages");
        assert_eq!(
            pages,
            json!([{"summary":"[HTTP/2,HTTP/ \"quoted\" \\ tail"},{"page":2}])
        );
    }

    #[test]
    fn failing_legacy_status_fails_even_with_no_named_requirement() {
        let checks = [check("build", "pass"), check("legacy", "fail")];
        assert_eq!(state(&checks, &[]), CiCompletion::Failed);
    }
    #[test]
    fn ci_requires_registration_and_rejects_failed_unknown_or_pending_checks() {
        let required = vec![need("build"), need("lint")];
        assert_eq!(
            state(&[check("build", "pass")], &required),
            CiCompletion::Pending
        );
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "pending")],
                &required
            ),
            CiCompletion::Pending
        );
        assert_eq!(
            state(&[check("build", "pass"), check("lint", "fail")], &required),
            CiCompletion::Failed
        );
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "unknown")],
                &required
            ),
            CiCompletion::Unavailable
        );
        assert_eq!(
            state(
                &[
                    check("build", "pass"),
                    check("lint", "pass"),
                    check("optional", "fail")
                ],
                &required
            ),
            CiCompletion::Green
        );
        assert_eq!(state(&[], &[]), CiCompletion::Pending);
        assert_eq!(
            state(&[check("build", "skipping")], &[]),
            CiCompletion::Unavailable
        );
    }

    #[test]
    fn optional_in_progress_check_keeps_a_required_green_head_pending() {
        let checks = [check("build", "pass"), check("slow-optional", "pending")];
        assert_eq!(
            state_for("head", &checks, &[need("build")]),
            CiCompletion::Pending
        );
    }
    #[test]
    fn a_skipped_required_check_has_not_verified_the_head() {
        let required = vec![need("build"), need("lint")];
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "skipping")],
                &required
            ),
            CiCompletion::Unavailable
        );
        // Optional skipped jobs among everything reported are harmless.
        assert_eq!(
            state(&[check("build", "pass"), check("matrix", "skipping")], &[]),
            CiCompletion::Green
        );
    }
    #[test]
    fn a_required_check_from_another_app_does_not_satisfy_the_requirement() {
        let required = vec![Required {
            name: "build".into(),
            app: Some(15368),
        }];
        let mut forged = check("build", "pass");
        forged.app = Some(999);
        assert_eq!(state(&[forged], &required), CiCompletion::Pending);
        let mut genuine = check("build", "pass");
        genuine.app = Some(15368);
        assert_eq!(state(&[genuine], &required), CiCompletion::Green);
        // A forged failure from another app is not the required check either.
        let mut other = check("build", "fail");
        other.app = Some(999);
        let mut genuine = check("build", "pass");
        genuine.app = Some(15368);
        assert_eq!(state(&[other, genuine], &required), CiCompletion::Green);
    }
}
