//! Bounded CI polling for a pinned PR head. No progress goes to JSON stdout.
use super::{Result, execute_for, successful};
use provenant_core::GitOid;
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
#[derive(Deserialize)]
struct Check {
    #[serde(default)]
    name: String,
    bucket: String,
}
#[derive(Debug, PartialEq)]
enum State {
    Passed,
    Pending,
    Failed,
    Unavailable,
}
fn state(checks: &[Check], required: &[String]) -> State {
    let selected: Vec<_> = checks
        .iter()
        .filter(|check| required.is_empty() || required.contains(&check.name))
        .collect();
    if selected
        .iter()
        .any(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
    {
        return State::Failed;
    }
    if required
        .iter()
        .any(|name| !checks.iter().any(|check| &check.name == name))
    {
        return State::Pending;
    }
    if selected.is_empty() {
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
    if !selected.iter().any(|check| check.bucket == "pass") {
        return State::Unavailable;
    }
    State::Passed
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
    let required =
        super::status::required_policy(project, &target, Some(deadline)).ok_or_else(|| {
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
    let remaining = || {
        deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(60))
    };
    let read = |argv: &[&str]| -> Result<_> {
        if remaining().is_zero() {
            return Err(failure(
                "CI_TIMEOUT",
                "CI wait expired; no merge was attempted",
            ));
        }
        execute_for(project, argv, remaining(), false).map_err(|error| {
            if Instant::now() >= deadline {
                failure("CI_TIMEOUT", "CI wait expired; no merge was attempted")
            } else {
                failure("CI_UNAVAILABLE", error.to_string())
            }
        })
    };
    let check_head = || -> Result<()> {
        #[derive(Deserialize)]
        struct Head {
            #[serde(rename = "headRefOid")]
            head: GitOid,
        }
        let bytes = successful(read(&[
            "gh",
            "pr",
            "view",
            &target,
            "--json",
            "headRefOid",
        ])?)?;
        if serde_json::from_slice::<Head>(&bytes)?.head != expected {
            return Err(failure(
                "HEAD_MOVED",
                format!("pull request head moved from {expected}"),
            ));
        }
        Ok(())
    };
    let mut progress = 0;
    loop {
        check_head()?;
        let output = read(&["gh", "pr", "checks", &target, "--json", "name,bucket"])?;
        if !matches!(output.termination, Termination::Exit(0 | 1 | 8)) {
            return Err(failure("CI_UNAVAILABLE", "cannot read CI checks"));
        }
        let checks: Vec<Check> = serde_json::from_slice(&output.stdout.bytes)
            .map_err(|_| failure("CI_UNAVAILABLE", "CI checks did not return valid JSON"))?;
        check_head()?;
        match state(&checks, &required) {
            State::Passed => return Ok(()),
            State::Failed => return Err(failure("CI_FAILED", "required CI checks failed")),
            State::Unavailable => {
                return Err(failure(
                    "CI_UNAVAILABLE",
                    "CI check states cannot establish success",
                ));
            }
            State::Pending if wait.is_none() => {
                return Err(failure(
                    "CI_PENDING",
                    "CI checks are pending; use land --wait",
                ));
            }
            State::Pending => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(failure(
                "CI_TIMEOUT",
                "CI wait expired; no merge was attempted",
            ));
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
            return Err(failure(
                "CI_TIMEOUT",
                "CI wait expired; no merge was attempted",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ci_requires_registration_and_rejects_failed_unknown_or_pending_checks() {
        use super::*;
        let check = |name: &str, bucket: &str| Check {
            name: name.into(),
            bucket: bucket.into(),
        };
        let required = vec!["build".into(), "lint".into()];
        assert_eq!(state(&[check("build", "pass")], &required), State::Pending);
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "pending")],
                &required
            ),
            State::Pending
        );
        assert_eq!(
            state(&[check("build", "pass"), check("lint", "fail")], &required),
            State::Failed
        );
        assert_eq!(
            state(
                &[check("build", "pass"), check("lint", "unknown")],
                &required
            ),
            State::Unavailable
        );
        assert_eq!(
            state(
                &[
                    check("build", "pass"),
                    check("lint", "skipping"),
                    check("optional", "fail")
                ],
                &required
            ),
            State::Passed
        );
        assert_eq!(state(&[], &[]), State::Pending);
        assert_eq!(
            state(&[check("build", "skipping")], &[]),
            State::Unavailable
        );
    }
}
