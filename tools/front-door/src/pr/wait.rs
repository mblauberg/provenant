//! Bounded CI polling for a pinned PR head. No progress goes to JSON stdout.
use super::{Result, execute_for, status::Required, successful};
use provenant_core::GitOid;
use provenant_core::retry::{Backoff, RetryCause, is_transient};
use provenant_os::exec::Termination;
use serde::Deserialize;
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
fn failure(code: &'static str, message: impl Into<String>) -> Box<dyn std::error::Error> {
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

pub(super) struct Check {
    pub name: String,
    pub bucket: String,
    /// The GitHub App that reported a check run; legacy statuses have none.
    pub app: Option<u64>,
    /// The latest time GitHub recorded activity for this check.
    pub registered: Option<jiff::Timestamp>,
}
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
    checks: &[Check],
    required: &[Required],
    now: jiff::Timestamp,
    mergeable: Option<&str>,
    expired: bool,
) -> State {
    match state(checks, required, now) {
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
    let selected: Vec<_> = checks
        .iter()
        .filter(|check| {
            required.is_empty()
                || required
                    .iter()
                    .any(|need| need.matches(&check.name, check.app))
        })
        .collect();
    if selected
        .iter()
        .any(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
    {
        return State::Failed;
    }
    if required.iter().any(|need| {
        !checks
            .iter()
            .any(|check| need.matches(&check.name, check.app))
    }) || selected.is_empty()
    {
        return State::Pending;
    }
    if selected
        .iter()
        .any(|check| !matches!(check.bucket.as_str(), "pass" | "skipping" | "pending"))
    {
        return State::Unavailable;
    }
    if selected.iter().any(|check| check.bucket == "pending") {
        return State::Pending;
    }
    // A required check that did not run has not verified this head, even where
    // GitHub's own merge button would accept it.
    if !required.is_empty() && selected.iter().any(|check| check.bucket == "skipping") {
        return State::Unavailable;
    }
    if !selected.iter().any(|check| check.bucket == "pass") {
        return State::Unavailable;
    }
    // With no named requirement, the first green check proves little: a slower
    // workflow may not have registered yet.
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
}
impl Reader<'_> {
    // This CLI edge owns the real clock and retry pause.
    #[allow(clippy::disallowed_methods)]
    pub fn read(&self, argv: &[&str]) -> Result<provenant_os::exec::ExecOutcome> {
        let backoff = Backoff::new(RETRY_FIRST, 2, RETRY_LAST);
        let mut retries = 0_u64;
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(failure(
                    "CI_TIMEOUT",
                    "CI wait expired; no merge was attempted",
                ));
            }
            let fatal =
                match execute_for(self.project, argv, left.min(Duration::from_secs(60)), false) {
                    Ok(output) if output.termination == Termination::Exit(0) => return Ok(output),
                    Ok(output) => {
                        if !is_transient(RetryCause::CommandOutput {
                            stderr: &String::from_utf8_lossy(&output.stderr.bytes),
                            stdout: &String::from_utf8_lossy(&output.stdout.bytes),
                        }) {
                            return Ok(output);
                        }
                        None
                    }
                    Err(error) if error.is::<super::TimedOut>() => None,
                    Err(error) => Some(error),
                };
            if let Some(error) = fatal {
                return Err(failure("CI_UNAVAILABLE", error.to_string()));
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(backoff.delay(retries).min(left));
            retries = retries.saturating_add(1);
        }
    }
}
/// Both check runs and legacy status contexts are addressed by immutable SHA.
/// GitHub lists statuses newest first; older states of a context cannot veto a
/// successful retry. Pagination is needed even when required checks arrive late.
fn commit_checks(
    repository: &str,
    head: GitOid,
    read: &impl Fn(&[&str]) -> Result<provenant_os::exec::ExecOutcome>,
) -> Result<Vec<Check>> {
    let load = |suffix: &str| -> Result<serde_json::Value> {
        let endpoint = format!("{repository}/commits/{head}/{suffix}");
        let output = read(&["gh", "api", "--paginate", "--slurp", &endpoint])?;
        if output.termination != Termination::Exit(0) {
            return Err(failure(
                "CI_UNAVAILABLE",
                "cannot read CI for the pinned commit",
            ));
        }
        serde_json::from_slice(&output.stdout.bytes)
            .map_err(|_| failure("CI_UNAVAILABLE", "CI did not return valid JSON"))
    };
    let runs = load("check-runs?per_page=100&filter=latest")?;
    let statuses = load("statuses?per_page=100")?;
    parse_commit_checks(&runs, &statuses, head).ok_or_else(|| {
        failure(
            "CI_UNAVAILABLE",
            "CI evidence is incomplete or belongs to another head",
        )
    })
}
/// `null` is absent; any other malformed time makes the evidence unusable.
fn timestamp(value: Option<&serde_json::Value>) -> Option<Option<jiff::Timestamp>> {
    match value {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(text) => text.as_str()?.parse().ok().map(Some),
    }
}
fn parse_commit_checks(
    runs: &serde_json::Value,
    statuses: &serde_json::Value,
    head: GitOid,
) -> Option<Vec<Check>> {
    let mut checks = Vec::new();
    let mut declared = 0;
    for page in runs.as_array()? {
        declared = page
            .get("total_count")
            .and_then(serde_json::Value::as_u64)
            .map_or(declared, |count| count.max(declared));
        for check in page.get("check_runs")?.as_array()? {
            if check.get("head_sha")?.as_str()? != head.to_string() {
                return None;
            }
            let bucket = match check.get("status")?.as_str()? {
                "completed" => match check.get("conclusion")?.as_str()? {
                    "success" => "pass",
                    "neutral" | "skipped" => "skipping",
                    "cancelled" => "cancel",
                    "failure" | "timed_out" | "action_required" | "startup_failure" | "stale" => {
                        "fail"
                    }
                    _ => return None,
                },
                "queued" | "in_progress" | "pending" | "waiting" | "requested" => "pending",
                _ => return None,
            };
            let started = timestamp(check.get("started_at"))?;
            let completed = timestamp(check.get("completed_at"))?;
            checks.push(Check {
                name: check.get("name")?.as_str()?.into(),
                bucket: bucket.into(),
                app: check
                    .get("app")
                    .and_then(|app| app.get("id"))
                    .and_then(serde_json::Value::as_u64),
                registered: started.max(completed),
            });
        }
    }
    // A short listing means a page was lost; the missing check could be the
    // failing one.
    if checks.len() < usize::try_from(declared).ok()? {
        return None;
    }
    let mut seen = std::collections::BTreeSet::new();
    for page in statuses.as_array()? {
        for check in page.as_array()? {
            let name = check.get("context")?.as_str()?;
            if !seen.insert(name) {
                continue;
            }
            let bucket = match check.get("state")?.as_str()? {
                "success" => "pass",
                "pending" | "expected" => "pending",
                "failure" | "error" => "fail",
                _ => return None,
            };
            let created = timestamp(check.get("created_at"))?;
            let updated = timestamp(check.get("updated_at"))?;
            checks.push(Check {
                name: name.into(),
                bucket: bucket.into(),
                app: None,
                registered: created.max(updated),
            });
        }
    }
    Some(checks)
}

// This CLI edge owns the real wait deadline and polling interval.
#[allow(clippy::disallowed_methods)]
pub(super) fn checks(
    project: &Path,
    number: u64,
    expected: GitOid,
    wait: Option<u64>,
    json: bool,
) -> Result<()> {
    let target = number.to_string();
    let deadline = Instant::now() + Duration::from_secs(wait.unwrap_or(60));
    let reader = Reader { project, deadline };
    let read = |argv: &[&str]| reader.read(argv);
    let policy =
        super::status::required_policy(&target, &|argv| read(argv).ok()).ok_or_else(|| {
            if Instant::now() >= deadline {
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
        #[derive(Deserialize)]
        struct Head {
            #[serde(rename = "headRefOid")]
            head: GitOid,
            #[serde(default)]
            mergeable: Option<String>,
        }
        let bytes = successful(read(&[
            "gh",
            "pr",
            "view",
            &target,
            "--json",
            "headRefOid,mergeable",
        ])?)?;
        let head: Head = serde_json::from_slice(&bytes)?;
        if head.head != expected {
            return Err(failure(
                "HEAD_MOVED",
                format!("pull request head moved from {expected}"),
            ));
        }
        Ok(head.mergeable)
    };
    let mut progress = 0;
    loop {
        let _ = check_head()?;
        // Read the pinned commit, not whichever head the PR has while gh runs.
        // Before/after head reads alone cannot protect against an A-B-A update.
        let checks = commit_checks(&policy.repository, expected, &read)?;
        let mergeable = check_head()?;
        let expired = Instant::now() >= deadline;
        match observe(
            &checks,
            &policy.checks,
            jiff::Timestamp::now(),
            mergeable.as_deref(),
            expired,
        ) {
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
        let left = deadline.saturating_duration_since(Instant::now());
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
        std::thread::sleep(left.min(Duration::from_secs(2)));
        if Instant::now() >= deadline {
            return Err(expired_failure(checks.is_empty()));
        }
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
            name: name.into(),
            bucket: bucket.into(),
            app: None,
            registered: Some(at("2020-01-01T00:00:00Z")),
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
    fn conflict_and_unscheduled_heads_are_named() {
        let now = at(NOW);
        let required = [need("build")];
        // A conflicting PR never starts CI, whatever else has registered.
        assert_eq!(
            observe(&[], &required, now, Some("CONFLICTING"), false),
            State::Conflict
        );
        assert_eq!(
            observe(
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
            observe(&[], &required, now, Some("MERGEABLE"), false),
            State::Pending
        );
        assert_eq!(
            observe(&[], &required, now, None, true),
            State::NotScheduled
        );
        // Something registered is still pending after expiry; the timeout reports it.
        assert_eq!(
            observe(&[check("lint", "pass")], &required, now, None, true),
            State::Pending
        );
    }

    #[test]
    fn commit_ci_rejects_wrong_heads_and_keeps_latest_legacy_status() {
        let head = head();
        let mut runs = json!([{"check_runs":[{"name":"build","head_sha":head,"status":"in_progress","conclusion":"success"}]}]);
        let statuses = json!([[{"context":"external","state":"success"},{"context":"external","state":"failure"}]]);
        let checks = parse_commit_checks(&runs, &statuses, head).expect("valid evidence");
        assert_eq!(
            checks
                .iter()
                .map(|check| check.bucket.as_str())
                .collect::<Vec<_>>(),
            ["pending", "pass"]
        );
        runs[0]["check_runs"][0]["head_sha"] = json!("2".repeat(40));
        assert!(parse_commit_checks(&runs, &statuses, head).is_none());
    }
    #[test]
    fn commit_ci_reads_every_page_and_rejects_a_lost_one() {
        let head = head();
        let run = |name: &str| json!({"name":name,"head_sha":head,"status":"completed","conclusion":"success","app":{"id":15368}});
        let runs = json!([
            {"total_count":3,"check_runs":[run("a"),run("b")]},
            {"total_count":3,"check_runs":[run("c")]},
        ]);
        let statuses =
            json!([[{"context":"x","state":"success"}], [{"context":"y","state":"failure"}]]);
        let checks = parse_commit_checks(&runs, &statuses, head).expect("valid");
        assert_eq!(checks.len(), 5);
        assert_eq!(checks[0].app, Some(15368));
        assert!(
            checks
                .iter()
                .any(|check| check.name == "y" && check.bucket == "fail")
        );
        let lost = json!([{"total_count":3,"check_runs":[run("a"),run("b")]}]);
        assert!(parse_commit_checks(&lost, &statuses, head).is_none());
        let bad_time = json!([{"check_runs":[{"name":"a","head_sha":head,"status":"completed","conclusion":"success","started_at":"yesterday"}]}]);
        assert!(parse_commit_checks(&bad_time, &json!([[]]), head).is_none());
    }
    #[test]
    fn failing_legacy_status_fails_even_with_no_named_requirement() {
        let head = head();
        let runs = json!([{"check_runs":[{"name":"build","head_sha":head,"status":"completed","conclusion":"success"}]}]);
        let statuses = json!([[{"context":"legacy","state":"error"}]]);
        let checks = parse_commit_checks(&runs, &statuses, head).expect("valid");
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
