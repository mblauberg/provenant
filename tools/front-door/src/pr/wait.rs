//! Bounded CI polling for a pinned PR head. No progress goes to JSON stdout.
use super::{
    Result, execute_for,
    observer::{Check, ObserveError},
    status::{CiCheck, CiCompletion, Required, ci_completion},
};
use provenant_core::GitOid;
use provenant_core::retry::{Backoff, RetryCause, is_transient};
use provenant_os::exec::Termination;
use std::{
    io::{self, Write},
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
fn expired_failure(unscheduled: bool) -> Box<dyn std::error::Error> {
    if unscheduled {
        failure(
            "CI_NOT_SCHEDULED",
            "no check registered for this head; next: check the workflow triggers or push an empty commit to start CI",
        )
    } else {
        failure("CI_TIMEOUT", "CI wait expired; no merge was attempted")
    }
}
/// The quiet period an unconstrained repository must show after its newest
/// check registered before landing trusts that no further check is coming.
pub(super) const SETTLE: Duration = Duration::from_secs(60);
/// First and longest pause between retries of a transient GitHub read.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LAST: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq)]
enum State {
    Passed,
    Pending,
    /// Everything reported has passed, but a further check may still register.
    Settling,
    Failed,
    Unavailable,
    /// The pull request conflicts with its base, so CI will not start.
    Conflict,
    /// The wait expired with no check registered for the head.
    NotScheduled,
}
/// Fold mergeability and an expired wait into a CI state. A conflicting pull
/// request never starts CI; an empty head after the wait was never scheduled.
fn observe(
    head: &str,
    checks: &[Check],
    required: &[Required],
    now: jiff::Timestamp,
    mergeable: Option<&str>,
    expired: bool,
) -> State {
    match state_for(head, checks, required, now) {
        State::Pending | State::Settling | State::Unavailable
            if mergeable == Some("CONFLICTING") =>
        {
            State::Conflict
        }
        State::Pending | State::Settling if expired && checks.is_empty() => State::NotScheduled,
        other => other,
    }
}
fn state(checks: &[Check], required: &[Required], now: jiff::Timestamp) -> State {
    let head = checks.first().map_or("", |check| check.head.as_str());
    state_for(head, checks, required, now)
}
fn state_for(head: &str, checks: &[Check], required: &[Required], now: jiff::Timestamp) -> State {
    let normalized: Vec<_> = checks
        .iter()
        .map(|check| CiCheck {
            head: check.head.clone(),
            name: check.name.clone(),
            bucket: check.bucket.clone(),
            app: check.app,
        })
        .collect();
    match ci_completion(head, &normalized, required) {
        CiCompletion::Pending => return State::Pending,
        CiCompletion::Failed => return State::Failed,
        CiCompletion::Unavailable => return State::Unavailable,
        CiCompletion::Green => {}
    }
    // With no named requirement, allow late workflows to register only after
    // at least one current-head check has completed successfully.
    if required.is_empty() && settle_remaining(checks, now).is_some() {
        return State::Settling;
    }
    State::Passed
}
/// Time still to wait after the newest check activity, or `None` once the
/// repository has been quiet for `SETTLE`. A check with no time is just now.
fn settle_remaining(checks: &[Check], now: jiff::Timestamp) -> Option<Duration> {
    let newest = checks
        .iter()
        .map(|check| check.registered)
        .collect::<Option<Vec<_>>>()
        .and_then(|times| times.into_iter().max());
    let Some(newest) = newest else {
        return Some(SETTLE);
    };
    let quiet = now.duration_since(newest);
    let quiet = Duration::try_from(quiet).unwrap_or_default();
    SETTLE.checked_sub(quiet).filter(|left| !left.is_zero())
}

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
        let rate_backoff = Backoff::new(Duration::from_secs(60), 2, Duration::from_secs(900));
        let mut retries = 0_u64;
        let mut rate_reset = None;
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(if let Some(reset) = rate_reset {
                    failure(
                        "RATE_LIMITED",
                        format!(
                            "GitHub rate limit; reset {reset}; next: {}",
                            self.retry_command
                        ),
                    )
                } else {
                    failure("CI_TIMEOUT", "GitHub wait budget expired")
                });
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
            let delay = if limited {
                let (delay, reset) = rate_delay(&headers, rate_backoff.delay(retries));
                rate_reset = Some(reset);
                delay
            } else {
                backoff.delay(retries)
            };
            let left = self.deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(delay.max(Duration::from_secs(1)).min(left));
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

// This CLI edge owns the real wait deadline and polling interval.
#[allow(clippy::disallowed_methods, clippy::too_many_arguments)]
pub(super) fn checks(
    reader: &Reader<'_>,
    state: &Path,
    number: u64,
    expected: GitOid,
    repository: &str,
    base: &str,
    wait: Option<u64>,
    poll: u64,
    json: bool,
) -> Result<()> {
    let mut observer = super::observer::Observer::open(
        state,
        reader.project,
        repository,
        reader.deadline.saturating_duration_since(Instant::now()),
    )
    .map_err(|error| ci_failure(error, &reader.retry_command))?;
    checks_with(
        &|argv| reader.read(argv),
        &mut |commit, revalidate| {
            // A waiter shares answers younger than its poll to keep waiting;
            // a decision revalidates.
            let max_age = if revalidate {
                Duration::ZERO
            } else {
                wait.map_or(Duration::ZERO, |_| Duration::from_secs(poll))
            };
            observer
                .checks(
                    &commit.to_string(),
                    max_age,
                    reader.deadline.saturating_duration_since(Instant::now()),
                )
                .map_err(|error| ci_failure(error, &reader.retry_command))
        },
        reader.deadline,
        &Instant::now,
        &std::thread::sleep,
        number,
        expected,
        repository,
        base,
        wait,
        poll,
        json,
    )
}

#[allow(clippy::disallowed_methods, clippy::too_many_arguments)]
fn checks_with(
    read: &impl Fn(&[&str]) -> Result<provenant_os::exec::ExecOutcome>,
    // Reads the checks of a commit; `true` demands a revalidated answer.
    ci: &mut dyn FnMut(GitOid, bool) -> Result<Vec<Check>>,
    deadline: Instant,
    now: &impl Fn() -> Instant,
    sleep: &impl Fn(Duration),
    number: u64,
    expected: GitOid,
    repository: &str,
    base: &str,
    wait: Option<u64>,
    poll: u64,
    json: bool,
) -> Result<()> {
    let target = number.to_string();
    let read_error = std::cell::RefCell::new(None);
    let policy = super::status::required_policy_for(repository, base, &|argv| match read(argv) {
        Ok(output) => Some(output),
        Err(error) => {
            *read_error.borrow_mut() = Some(error);
            None
        }
    })
    .ok_or_else(|| {
        if let Some(error) = read_error.borrow_mut().take() {
            return error;
        }
        if now() >= deadline {
            failure(
                "CI_TIMEOUT",
                "CI wait expired while reading required checks",
            )
        } else {
            failure(
                "CI_POLICY_UNAVAILABLE",
                "cannot establish required CI checks",
            )
        }
    })?;
    let check_head = || -> Result<Option<String>> {
        let pull = super::rest_pull(read, &target)?;
        if pull.head.sha != expected {
            return Err(failure(
                "HEAD_MOVED",
                format!("pull request head moved from {expected}"),
            ));
        }
        Ok(pull.mergeable.map(|value| {
            if value {
                "MERGEABLE".to_owned()
            } else {
                "CONFLICTING".to_owned()
            }
        }))
    };
    let mut progress = 0;
    let mut pending = 0_u64;
    let pacing = Backoff::new(
        Duration::from_secs(poll),
        2,
        Duration::from_secs(poll.saturating_mul(4)),
    );
    loop {
        let _ = check_head()?;
        // Read the pinned commit, not whichever head the PR has while gh runs.
        // Before/after head reads alone cannot protect against an A-B-A update.
        // A shared answer may keep a waiter waiting, but a pass is decided
        // only on an answer revalidated with GitHub.
        let mut revalidated = wait.is_none();
        let (checks, state) = loop {
            let checks = ci(expected, revalidated)?;
            let mergeable = check_head()?;
            let state = observe(
                expected.as_str(),
                &checks,
                &policy.checks,
                jiff::Timestamp::now(),
                mergeable.as_deref(),
                now() >= deadline,
            );
            if state == State::Passed && !revalidated {
                revalidated = true;
                continue;
            }
            break (checks, state);
        };
        match state {
            State::Conflict => {
                return Err(failure(
                    "CI_CONFLICT",
                    "pull request conflicts with its base, so CI will not start; next: rebase onto the base branch and push",
                ));
            }
            State::NotScheduled => {
                return Err(failure(
                    "CI_NOT_SCHEDULED",
                    "no check registered for this head; next: check the workflow triggers or push an empty commit to start CI",
                ));
            }
            State::Passed => return Ok(()),
            State::Failed => return Err(failure("CI_FAILED", "required CI checks failed")),
            State::Unavailable => {
                return Err(failure(
                    "CI_UNAVAILABLE",
                    "CI check states cannot establish success",
                ));
            }
            State::Pending | State::Settling if wait.is_none() => {
                return Err(failure(
                    "CI_PENDING",
                    "CI checks are pending or may still register; use land --wait",
                ));
            }
            State::Pending | State::Settling => {}
        }
        let left = deadline.saturating_duration_since(now());
        if left.is_zero() {
            return Err(expired_failure(checks.is_empty()));
        }
        // Eight lines for the whole wait, regardless of duration or churn.
        if !json && progress < 8 {
            let _ = writeln!(
                io::stderr().lock(),
                "waiting #{number} {expected}: CI pending ({}s left)",
                left.as_secs()
            );
            progress += 1;
        }
        sleep(left.min(pacing.delay(pending)));
        pending = pending.saturating_add(1);
        if now() >= deadline {
            return Err(expired_failure(checks.is_empty()));
        }
    }
}

/// The land wait's verdict on `checks` with no named requirement.
#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes this module without libtest.
pub(super) fn verdict(checks: &[Check], now: jiff::Timestamp) -> &'static str {
    match state(checks, &[], now) {
        State::Passed => "passed",
        State::Pending | State::Settling => "pending",
        State::Failed => "failed",
        State::Unavailable => "unavailable",
        State::Conflict => "conflict",
        State::NotScheduled => "not scheduled",
    }
}
#[cfg(test)]
#[allow(dead_code, unused_imports)] // Also compiled, unrun, by the harness-less worktrees driver.
mod tests {
    use super::*;
    use serde_json::json;

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
    const NOW: &str = "2026-01-01T00:00:00Z";

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
        assert_eq!(state_for(head, &queued, &required, at(NOW)), State::Pending);

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
        assert_eq!(state_for(head, &empty, &[], at(NOW)), State::Pending);

        let green = parse("all_green").expect("green rollup");
        assert_eq!(
            state_for(head, &green, &required, at("2026-01-01T00:02:00Z")),
            State::Passed
        );
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
    #[allow(clippy::disallowed_methods)]
    fn a_shared_pass_is_revalidated_before_it_is_decided() {
        use provenant_os::exec::{CapturedStream, ExecOutcome, Termination};
        use std::cell::RefCell;
        let clock = Instant::now();
        let oid = head();
        let read = |args: &[&str]| -> Result<ExecOutcome> {
            let endpoint = args
                .iter()
                .find(|arg| arg.starts_with("repos/"))
                .unwrap_or(&"");
            let body = if endpoint.contains("/pulls/") {
                json!({"state":"open", "head":{"sha":oid,"ref":"feat","repo":{"full_name":"owner/repo"}}, "base":{"sha":oid,"ref":"main","repo":{"full_name":"owner/repo"}}, "mergeable":true,"mergeable_state":"clean","merged_at":null,"merge_commit_sha":null,"title":"test"})
            } else if endpoint.contains("/rules/") {
                json!([[]])
            } else {
                json!({"protected":false})
            };
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: serde_json::to_vec(&body)?,
                    ..Default::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        };
        let asked = RefCell::new(Vec::new());
        // The shared answer is green, but GitHub now reports a failure.
        let result = checks_with(
            &read,
            &mut |_, revalidate| {
                asked.borrow_mut().push(revalidate);
                Ok(vec![check(
                    "build",
                    if revalidate { "fail" } else { "pass" },
                )])
            },
            clock + Duration::from_secs(600),
            &|| clock,
            &|_| {},
            7,
            oid,
            "repos/owner/repo",
            "main",
            Some(600),
            10,
            true,
        );
        assert!(result.is_err(), "a stale green must not pass");
        assert_eq!(*asked.borrow(), [false, true]);
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn default_pacing_limits_calls_over_ninety_minutes() {
        use provenant_os::exec::{CapturedStream, ExecOutcome, Termination};
        use std::cell::Cell;
        let clock = Cell::new(Instant::now());
        let deadline = clock.get() + Duration::from_secs(90 * 60);
        let calls = Cell::new(0);
        let oid = head();
        let read = |args: &[&str]| -> Result<ExecOutcome> {
            calls.set(calls.get() + 1);
            let endpoint = args
                .iter()
                .find(|arg| arg.starts_with("repos/"))
                .unwrap_or(&"");
            let body = if endpoint.contains("/pulls/") {
                json!({"state":"open", "head":{"sha":oid,"ref":"feat","repo":{"full_name":"owner/repo"}}, "base":{"sha":oid,"ref":"main","repo":{"full_name":"owner/repo"}}, "mergeable":true,"mergeable_state":"clean","merged_at":null,"merge_commit_sha":null,"title":"test"})
            } else if endpoint.contains("/rules/") {
                json!([[]])
            } else {
                json!({"protected":false})
            };
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: serde_json::to_vec(&body)?,
                    ..Default::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        };
        let result = checks_with(
            &read,
            &mut |_, _| {
                calls.set(calls.get() + 1);
                Ok(vec![check("build", "pending")])
            },
            deadline,
            &|| clock.get(),
            &|delay| clock.set(clock.get() + delay),
            7,
            oid,
            "repos/owner/repo",
            "main",
            Some(90 * 60),
            provenant_cli::domains::DEFAULT_LAND_POLL_SECONDS,
            true,
        );
        let error = result.expect_err("pending CI must exhaust the wait");
        assert_eq!(
            error.downcast_ref::<Failure>().expect("typed failure").code,
            "CI_TIMEOUT"
        );
        assert_eq!(clock.get(), deadline);
        assert!(calls.get() <= 200, "{} calls", calls.get());
    }

    #[test]
    fn conflict_and_unscheduled_heads_are_named() {
        let now = at(NOW);
        let required = [need("build")];
        // A conflicting PR never starts CI, whatever else has registered.
        assert_eq!(
            observe("head", &[], &required, now, Some("CONFLICTING"), false),
            State::Conflict
        );
        assert_eq!(
            observe(
                "head",
                &[check("lint", "pending")],
                &required,
                now,
                Some("CONFLICTING"),
                false
            ),
            State::Conflict
        );
        // A failed required check stays failed.
        assert_eq!(
            observe(
                "head",
                &[check("build", "fail")],
                &required,
                now,
                Some("CONFLICTING"),
                true
            ),
            State::Failed
        );
        // Nothing registered: pending while the wait runs, unscheduled once it expires.
        assert_eq!(
            observe("head", &[], &required, now, Some("MERGEABLE"), false),
            State::Pending
        );
        assert_eq!(
            observe("head", &[], &required, now, None, true),
            State::NotScheduled
        );
        // Something registered is still pending after expiry; the timeout reports it.
        assert_eq!(
            observe("head", &[check("lint", "pass")], &required, now, None, true),
            State::Pending
        );
    }

    #[test]
    fn failing_legacy_status_fails_even_with_no_named_requirement() {
        let checks = [check("build", "pass"), check("legacy", "fail")];
        assert_eq!(state(&checks, &[], at(NOW)), State::Failed);
    }
    #[test]
    fn ci_requires_registration_and_rejects_failed_unknown_or_pending_checks() {
        let required = vec![need("build"), need("lint")];
        let now = at(NOW);
        assert_eq!(
            state(&[check("build", "pass")], &required, now),
            State::Pending
        );
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "pending")],
                &required,
                now
            ),
            State::Pending
        );
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "fail")],
                &required,
                now
            ),
            State::Failed
        );
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "unknown")],
                &required,
                now
            ),
            State::Unavailable
        );
        assert_eq!(
            state(
                &[
                    check("build", "pass"),
                    check("lint", "pass"),
                    check("optional", "fail")
                ],
                &required,
                now
            ),
            State::Passed
        );
        assert_eq!(state(&[], &[], now), State::Pending);
        assert_eq!(
            state(&[check("build", "skipping")], &[], now),
            State::Unavailable
        );
    }

    #[test]
    fn optional_in_progress_check_keeps_a_required_green_head_pending() {
        let checks = [check("build", "pass"), check("slow-optional", "pending")];
        assert_eq!(
            state_for("head", &checks, &[need("build")], at(NOW)),
            State::Pending
        );
    }
    #[test]
    fn a_skipped_required_check_has_not_verified_the_head() {
        let required = vec![need("build"), need("lint")];
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "skipping")],
                &required,
                at(NOW)
            ),
            State::Unavailable
        );
        // Optional skipped jobs among everything reported are harmless.
        assert_eq!(
            state(
                &[check("build", "pass"), check("matrix", "skipping")],
                &[],
                at(NOW)
            ),
            State::Passed
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
        assert_eq!(state(&[forged], &required, at(NOW)), State::Pending);
        let mut genuine = check("build", "pass");
        genuine.app = Some(15368);
        assert_eq!(state(&[genuine], &required, at(NOW)), State::Passed);
        // A forged failure from another app is not the required check either.
        let mut other = check("build", "fail");
        other.app = Some(999);
        let mut genuine = check("build", "pass");
        genuine.app = Some(15368);
        assert_eq!(state(&[other, genuine], &required, at(NOW)), State::Passed);
    }
    #[test]
    fn an_empty_requirement_settles_after_the_newest_registration() {
        let mut green = check("build", "pass");
        green.registered = Some(at("2025-12-31T23:59:30Z"));
        assert_eq!(state(&[green], &[], at(NOW)), State::Settling);
        let mut late = check("slow", "pass");
        late.registered = Some(at("2025-12-31T23:59:50Z"));
        let mut old = check("build", "pass");
        old.registered = Some(at("2025-12-31T23:50:00Z"));
        // A later registration restarts the quiet period for the whole set.
        assert_eq!(
            settle_remaining(&[old, late], at(NOW)),
            Some(Duration::from_secs(50))
        );
        let quiet = check("build", "pass");
        assert_eq!(state(&[quiet], &[], at(NOW)), State::Passed);
        let mut untimed = check("build", "pass");
        untimed.registered = None;
        assert_eq!(state(&[untimed], &[], at(NOW)), State::Settling);
        // A clock ahead of GitHub's must not shorten the quiet period.
        let mut future = check("build", "pass");
        future.registered = Some(at("2026-01-01T00:00:10Z"));
        assert_eq!(state(&[future], &[], at(NOW)), State::Settling);
    }
}
