//! `pr status`: one compact line per pull request.
use super::{Result, execute, successful};
use crate::{emit_summarised, fail, roots};
use provenant_cli::Globals;
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Deserialize)]
struct View {
    state: String,
    #[serde(rename = "headRefOid")]
    head: String,
    #[serde(rename = "mergeStateStatus", default)]
    merge: String,
    #[serde(rename = "reviewDecision", default)]
    review: Option<String>,
    #[serde(rename = "isDraft", default)]
    draft: bool,
    #[serde(rename = "statusCheckRollup", default)]
    checks: Option<Vec<serde_json::Value>>,
}
#[derive(Deserialize)]
struct Check {
    #[serde(default)]
    name: String,
    bucket: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Line {
    number: u64,
    state: String,
    head: String,
    draft: bool,
    checks_passed: Option<usize>,
    checks_total: Option<usize>,
    checks_failed: Option<usize>,
    checks_pending: Option<usize>,
    merge: String,
    review: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
impl Line {
    /// The state an operator acts on, most urgent first.
    fn class(&self) -> &'static str {
        if self.error.is_some() {
            "unavailable"
        } else if self.state != "OPEN" {
            if self.state == "MERGED" {
                "merged"
            } else {
                "closed"
            }
        } else if self.checks_failed.unwrap_or(0) > 0 {
            "failing"
        } else if matches!(
            self.review.as_str(),
            "REVIEW_REQUIRED" | "CHANGES_REQUESTED"
        ) {
            "review_required"
        } else if self.checks_pending.unwrap_or(0) > 0 || self.checks_total.is_none() {
            "pending"
        } else if self.draft {
            "draft"
        } else {
            "ready"
        }
    }
    fn text(&self) -> String {
        if let Some(error) = &self.error {
            return format!("#{} unavailable: {error}", self.number);
        }
        let mut line = format!("#{} {} {}", self.number, self.state, self.head);
        if let (Some(passed), Some(total), Some(failed), Some(pending)) = (
            self.checks_passed,
            self.checks_total,
            self.checks_failed,
            self.checks_pending,
        ) {
            line.push_str(&format!(" checks={passed}/{total}"));
            if failed > 0 {
                line.push_str(&format!(" fail={failed}"));
            }
            if pending > 0 {
                line.push_str(&format!(" pending={pending}"));
            }
        } else {
            line.push_str(" checks=unavailable");
        }
        line.push_str(&format!(" merge={} review={}", self.merge, self.review));
        if self.draft {
            line.push_str(" draft");
        }
        line
    }
}

/// Valid JSON is meaningful even when gh exits non-zero for pending or failed
/// checks. Missing, malformed or interrupted output is unavailable, not zero.
fn counts(project: &Path, number: &str) -> Option<Vec<Check>> {
    let output = execute(
        project,
        &["gh", "pr", "checks", number, "--json", "name,bucket"],
        false,
    )
    .ok()?;
    serde_json::from_slice(&output.stdout.bytes).ok()
}

/// Graph gates use the same bounded check reader as `pr status`. gh returns
/// non-zero for pending and failed checks, so valid JSON is authoritative.
pub(crate) fn observe_ci(
    project: &Path,
    target: &str,
    names: &[String],
) -> provenant_fabric_runtime::graph::CiObservation {
    observe_ci_with(
        names,
        || required_checks(project, target),
        || counts(project, target),
    )
}

fn observe_ci_with(
    names: &[String],
    resolve: impl FnOnce() -> Option<Vec<String>>,
    read_checks: impl FnOnce() -> Option<Vec<Check>>,
) -> provenant_fabric_runtime::graph::CiObservation {
    let required = if names.is_empty() {
        resolve().map(|names| ("branch-required", names))
    } else {
        Some(("configured", names.to_vec()))
    };
    summarise_ci(
        read_checks(),
        required
            .as_ref()
            .map(|(policy, names)| (*policy, names.as_slice())),
    )
}

// Both classic protection and rulesets may apply. Only trust the resolved
// policy when both sources are readable.
pub(super) fn required_checks(project: &Path, target: &str) -> Option<Vec<String>> {
    required_policy(target, &|argv| {
        super::execute_for(project, argv, std::time::Duration::from_secs(60), false).ok()
    })
    .map(|policy| policy.checks.into_iter().map(|check| check.name).collect())
    .filter(|names: &Vec<String>| !names.is_empty())
}

/// A required check, optionally pinned to the GitHub App that must report it.
/// Without the pin, any app (or a legacy status) of that name would satisfy it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Required {
    pub name: String,
    pub app: Option<u64>,
}
impl Required {
    pub fn matches(&self, name: &str, app: Option<u64>) -> bool {
        self.name == name && self.app.is_none_or(|wanted| Some(wanted) == app)
    }
}
pub(super) struct Policy {
    pub repository: String,
    pub checks: Vec<Required>,
}

// A known empty policy is distinct from an unavailable policy. Landing then
// waits for every reported check to settle instead of trusting the first.
pub(super) fn required_policy(
    target: &str,
    read: &dyn Fn(&[&str]) -> Option<provenant_os::exec::ExecOutcome>,
) -> Option<Policy> {
    // `gh pr view` has no base-repository JSON field. The pull request URL
    // always lives on the base repository (never the head fork), so read the
    // owner and name from it; this also holds for URL and branch targets.
    #[derive(Deserialize)]
    struct Base {
        #[serde(rename = "baseRefName")]
        branch: String,
        url: String,
    }
    let bytes = successful(read(&[
        "gh",
        "pr",
        "view",
        target,
        "--json",
        "baseRefName,url",
    ])?)
    .ok()?;
    let base: Base = serde_json::from_slice(&bytes).ok()?;
    let segments: Vec<&str> = base.url.trim_end_matches('/').split('/').collect();
    let [.., owner, name, "pull", _] = segments.as_slice() else {
        return None;
    };
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    let encode = |value: &str| -> String {
        value
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    };
    let repo = format!("repos/{}/{}", encode(owner), encode(name));
    let branch = encode(&base.branch);
    let get = |path: &str, paginate: bool| -> Option<Vec<u8>> {
        let args = if paginate {
            vec!["gh", "api", "--paginate", "--slurp", path]
        } else {
            vec!["gh", "api", path]
        };
        successful(read(&args)?).ok()
    };
    let branch_info: serde_json::Value =
        serde_json::from_slice(&get(&format!("{repo}/branches/{branch}"), false)?).ok()?;
    let protection = if branch_info.get("protected")?.as_bool()? {
        let endpoint = format!("{repo}/branches/{branch}/protection");
        let output = read(&["gh", "api", &endpoint])?;
        classic_protection(
            output.termination == provenant_os::exec::Termination::Exit(0),
            &output.stdout.bytes,
        )?
    } else {
        None
    };
    let pages: Vec<Vec<serde_json::Value>> = serde_json::from_slice(&get(
        &format!("{repo}/rules/branches/{branch}?per_page=100"),
        true,
    )?)
    .ok()?;
    Some(Policy {
        repository: repo,
        checks: parse_required_checks(protection.as_ref(), &pages)?,
    })
}

// A ruleset can mark a branch protected without classic branch protection.
// Only GitHub's explicit response proves absence; generic 404/403 is not proof.
fn classic_protection(success: bool, bytes: &[u8]) -> Option<Option<serde_json::Value>> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if success {
        return Some(Some(value));
    }
    if value["message"] == "Branch not protected" && value["status"] == "404" {
        Some(None)
    } else {
        None
    }
}

fn parse_required_checks(
    protection: Option<&serde_json::Value>,
    pages: &[Vec<serde_json::Value>],
) -> Option<Vec<Required>> {
    // An absent pin is any app; a present one must be a number (classic
    // protection uses null or -1 for "any").
    let pin = |value: Option<&serde_json::Value>| -> Option<Option<u64>> {
        match value {
            None | Some(serde_json::Value::Null) => Some(None),
            Some(value) => match value.as_i64()? {
                -1 => Some(None),
                id => Some(Some(u64::try_from(id).ok()?)),
            },
        }
    };
    let mut needs = Vec::new();
    if let Some(protection) = protection {
        let checks = protection.get("required_status_checks")?;
        if !checks.is_null() {
            if let Some(listed) = checks.get("checks") {
                for check in listed.as_array()? {
                    needs.push(Required {
                        name: check.get("context")?.as_str()?.to_owned(),
                        app: pin(check.get("app_id"))?,
                    });
                }
            }
            for name in checks.get("contexts")?.as_array()? {
                needs.push(Required {
                    name: name.as_str()?.to_owned(),
                    app: None,
                });
            }
        }
    }
    for rule in pages.iter().flatten() {
        if rule.get("type")?.as_str()? == "required_status_checks" {
            for check in rule
                .get("parameters")?
                .get("required_status_checks")?
                .as_array()?
            {
                needs.push(Required {
                    name: check.get("context")?.as_str()?.to_owned(),
                    app: pin(check.get("integration_id"))?,
                });
            }
        }
    }
    needs.sort();
    needs.dedup();
    // Classic `contexts` repeats each pinned check without its app. Drop the
    // unpinned twin so the pin is not widened.
    let pinned: Vec<_> = needs
        .iter()
        .filter(|need| need.app.is_some())
        .map(|need| need.name.clone())
        .collect();
    needs.retain(|need| need.app.is_some() || !pinned.contains(&need.name));
    Some(needs)
}

fn summarise_ci(
    checks: Option<Vec<Check>>,
    required: Option<(&str, &[String])>,
) -> provenant_fabric_runtime::graph::CiObservation {
    use provenant_fabric_runtime::graph::{CiObservation, CiState};
    let policy = required.map_or("all-reported", |(policy, _)| policy);
    let Some(checks) = checks else {
        return CiObservation {
            extra: Default::default(),
            state: CiState::Unavailable,
            summary: format!("policy={policy} checks unavailable"),
        };
    };
    let selected: Vec<&Check> = match required {
        Some((_, names)) => checks.iter().filter(|c| names.contains(&c.name)).collect(),
        None => checks.iter().collect(),
    };
    let missing = required.map_or(0, |(_, names)| {
        names
            .iter()
            .filter(|name| !selected.iter().any(|c| &c.name == *name))
            .count()
    });
    let passed = selected.iter().filter(|c| c.bucket == "pass").count();
    let skipped = selected.iter().filter(|c| c.bucket == "skipping").count();
    let failed = selected
        .iter()
        .filter(|c| matches!(c.bucket.as_str(), "fail" | "cancel"))
        .count();
    let pending = selected.iter().filter(|c| c.bucket == "pending").count();
    let total = selected.len();
    let state = if total == 0
        || missing > 0
        || passed + skipped + failed + pending != total
        || (required.is_some() && skipped > 0)
    {
        CiState::Unavailable
    } else if failed > 0 {
        CiState::Failed
    } else if pending > 0 {
        CiState::Pending
    } else if passed == 0 {
        CiState::Unavailable
    } else {
        CiState::Passed
    };
    let selection = required.map_or_else(String::new, |(_, names)| format!(" names={names:?}"));
    CiObservation {
        extra: Default::default(),
        state,
        summary: format!(
            "policy={policy}{selection} checks={passed}/{total} fail={failed} pending={pending} skipped={skipped} missing={missing}"
        ),
    }
}
fn open_numbers(project: &Path) -> Result<Vec<u64>> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
    }
    let bytes = successful(execute(
        project,
        &[
            "gh", "pr", "list", "--author", "@me", "--state", "open", "--limit", "50", "--json",
            "number",
        ],
        false,
    )?)?;
    let mut listed: Vec<Listed> = serde_json::from_slice(&bytes)?;
    listed.sort_by_key(|entry| entry.number);
    Ok(listed.into_iter().map(|entry| entry.number).collect())
}
fn line(project: &Path, number: u64, timeout: std::time::Duration) -> Result<Line> {
    let text = number.to_string();
    let bytes = successful(super::execute_for(
        project,
        &[
            "gh",
            "pr",
            "view",
            &text,
            "--json",
            "state,headRefOid,mergeStateStatus,reviewDecision,isDraft,statusCheckRollup",
        ],
        timeout,
        false,
    )?)?;
    let view: View = serde_json::from_slice(&bytes)?;
    let checks = view
        .checks
        .as_deref()
        .and_then(rollup_checks)
        .filter(|checks| !checks.is_empty());
    let passed = checks.as_ref().map(|checks| {
        checks
            .iter()
            .filter(|check| matches!(check.bucket.as_str(), "pass" | "skipping"))
            .count()
    });
    let failed = checks.as_ref().map(|checks| {
        checks
            .iter()
            .filter(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
            .count()
    });
    let pending = checks.as_ref().map(|checks| {
        checks
            .iter()
            .filter(|check| check.bucket == "pending")
            .count()
    });
    let word = |text: &str| {
        if text.is_empty() {
            "none".to_owned()
        } else {
            text.to_lowercase()
        }
    };
    Ok(Line {
        number,
        state: view.state,
        head: view.head,
        draft: view.draft,
        checks_passed: passed,
        checks_total: checks.as_ref().map(Vec::len),
        checks_failed: failed,
        checks_pending: pending,
        merge: word(&view.merge),
        review: word(view.review.as_deref().unwrap_or_default()),
        error: None,
    })
}

/// Head and check states come from the same GitHub snapshot. A completed
/// conclusion is never trusted while the check is still running.
fn rollup_checks(values: &[serde_json::Value]) -> Option<Vec<Check>> {
    values
        .iter()
        .map(|value| {
            let kind = value.get("__typename")?.as_str()?;
            let (name, bucket) = match kind {
                "CheckRun" => {
                    let status = value.get("status")?.as_str()?;
                    let bucket = match status {
                        "COMPLETED" => match value.get("conclusion")?.as_str()? {
                            "SUCCESS" => "pass",
                            "SKIPPED" | "NEUTRAL" => "skipping",
                            "CANCELLED" => "cancel",
                            "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
                            | "STALE" => "fail",
                            _ => return None,
                        },
                        "QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED" => "pending",
                        _ => return None,
                    };
                    (value.get("name")?.as_str()?, bucket)
                }
                "StatusContext" => {
                    let bucket = match value.get("state")?.as_str()? {
                        "SUCCESS" => "pass",
                        "PENDING" | "EXPECTED" => "pending",
                        "ERROR" | "FAILURE" => "fail",
                        _ => return None,
                    };
                    (value.get("context")?.as_str()?, bucket)
                }
                _ => return None,
            };
            Some(Check {
                name: name.into(),
                bucket: bucket.into(),
            })
        })
        .collect()
}

/// A change is a row that was readable both times and differs. An error row
/// says nothing about the pull request.
fn changed(before: &[Line], after: &[Line]) -> bool {
    before.len() != after.len()
        || before
            .iter()
            .zip(after)
            .any(|(was, now)| was.error.is_none() && now.error.is_none() && was != now)
}
/// Replace each error row by the last good row for that pull request.
fn keep_good(previous: &[Line], current: Vec<Line>) -> Vec<Line> {
    current
        .into_iter()
        .map(
            |line| match previous.iter().find(|old| old.number == line.number) {
                Some(old) if line.error.is_some() && old.error.is_none() => old.clone(),
                _ => line,
            },
        )
        .collect()
}

#[allow(clippy::disallowed_methods)]
fn snapshot(project: &Path, numbers: &[u64], deadline: Option<std::time::Instant>) -> Vec<Line> {
    numbers
        .iter()
        .map(|&number| {
            let timeout = deadline.map_or(std::time::Duration::from_secs(60), |end| {
                end.saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_secs(60))
            });
            let observed = if timeout.is_zero() {
                Err("status wait expired".into())
            } else {
                line(project, number, timeout)
            };
            observed.unwrap_or_else(|error| Line {
                number,
                state: "UNAVAILABLE".into(),
                head: "unknown".into(),
                draft: false,
                checks_passed: None,
                checks_total: None,
                checks_failed: None,
                checks_pending: None,
                merge: "unknown".into(),
                review: "unknown".into(),
                error: Some(provenant_cli::bounded_text(&error.to_string(), 256)),
            })
        })
        .collect()
}

// CLI polling owns the real clock and wait interval.
#[allow(clippy::disallowed_methods)]
pub(super) fn run(numbers: Vec<u64>, wait: Option<u64>, globals: &Globals) -> u8 {
    const LEAF: &str = "pr status";
    let result = (|| -> Result<(Vec<Line>, bool)> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let numbers = if numbers.is_empty() {
            open_numbers(&project)?
        } else {
            numbers
        };
        let deadline =
            wait.map(|seconds| std::time::Instant::now() + std::time::Duration::from_secs(seconds));
        let initial = snapshot(&project, &numbers, deadline);
        let Some(deadline) = deadline else {
            return Ok((initial, false));
        };
        if std::time::Instant::now() >= deadline {
            return Ok((initial, true));
        }
        if initial.is_empty() || initial.iter().all(|line| line.error.is_some()) {
            return Ok((initial, false));
        }
        // The latest good row of each PR; a transient read failure neither
        // replaces it nor counts as a change.
        let mut latest = initial.clone();
        let mut baseline = initial;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok((latest, true));
            }
            std::thread::sleep(left.min(std::time::Duration::from_secs(5)));
            if std::time::Instant::now() >= deadline {
                return Ok((latest, true));
            }
            let current = snapshot(&project, &numbers, Some(deadline));
            latest = keep_good(&latest, current.clone());
            if std::time::Instant::now() >= deadline {
                return Ok((latest, true));
            }
            if changed(&baseline, &current) {
                return Ok((latest, false));
            }
            // Rows that recovered become the new baseline for later comparison.
            baseline = keep_good(&baseline, current);
        }
    })();
    match result {
        Ok((lines, timed_out)) => {
            let attribution = std::cell::OnceCell::new();
            for line in &lines {
                if matches!(line.state.as_str(), "CLOSED" | "MERGED") {
                    let closed = (|| -> Result<()> {
                        let tracker = attribution
                            .get_or_init(|| super::contributions::Tracker::resolve(globals))
                            .as_ref()
                            .map_err(|error| error.to_string())?;
                        tracker.close(line.number)
                    })();
                    if let Err(error) = closed {
                        super::contributions::warn(Some(line.number), &error);
                    }
                }
            }
            let mut text: String = lines.iter().map(|line| line.text() + "\n").collect();
            if timed_out {
                text.push_str("snapshot unchanged within wait limit\n");
            }
            let summary = summarise(&lines, timed_out);
            let outcome = if timed_out {
                Outcome::Timeout
            } else if !lines.is_empty() && lines.iter().all(|line| line.error.is_some()) {
                Outcome::Unavailable
            } else {
                Outcome::Ok
            };
            emit_summarised(
                &CommandResult::new(LEAF, outcome, lines),
                globals,
                &text,
                true,
                Some(&summary),
            )
        }
        Err(error) => fail(LEAF, Outcome::Error, "PR", &error.to_string(), globals),
    }
}

/// Counts by state, action rows first (failing, review, pending,
/// unavailable), then one typed next command.
fn summarise(lines: &[Line], timed_out: bool) -> provenant_cli::Summary {
    let order = ["failing", "review_required", "pending", "unavailable"];
    let counts: Vec<(&str, usize)> = [
        "failing",
        "review_required",
        "pending",
        "unavailable",
        "draft",
        "ready",
        "merged",
        "closed",
    ]
    .into_iter()
    .map(|class| {
        (
            class,
            lines.iter().filter(|line| line.class() == class).count(),
        )
    })
    .collect();
    let mut action: Vec<&Line> = lines
        .iter()
        .filter(|line| order.contains(&line.class()))
        .collect();
    action.sort_by_key(|line| order.iter().position(|class| *class == line.class()));
    let mut rows: Vec<String> = action.iter().map(|line| line.text()).collect();
    let quiet = lines.len() - action.len();
    if timed_out {
        rows.push("snapshot unchanged within wait limit".into());
    }
    let next = match action.first() {
        Some(line) if line.class() == "failing" => Some(format!("gh pr checks {}", line.number)),
        Some(line) if line.class() == "pending" => {
            Some(format!("provenant pr status {} --wait", line.number))
        }
        Some(line) => Some(format!("provenant pr status {} --full", line.number)),
        None => lines
            .iter()
            .find(|line| line.class() == "ready")
            .map(|line| format!("provenant land {} --wait", line.number))
            .or_else(|| Some("provenant status".to_owned())),
    };
    let mut headline = format!(
        "{} PRs: {}",
        lines.len(),
        provenant_cli::Summary::tally(&counts)
    );
    if quiet > 0 && !action.is_empty() {
        headline.push_str(&format!(" ({quiet} need nothing)"));
    }
    provenant_cli::Summary::new(headline, rows, next)
}

#[cfg(test)]
mod tests {
    #[allow(dead_code)] // Also compiled, unrun, by the harness-less worktrees driver.
    fn row(number: u64, head: &str, error: Option<&str>) -> super::Line {
        super::Line {
            number,
            state: "OPEN".into(),
            head: head.into(),
            draft: false,
            checks_passed: Some(1),
            checks_total: Some(1),
            checks_failed: Some(0),
            checks_pending: Some(0),
            merge: "clean".into(),
            review: "approved".into(),
            error: error.map(str::to_owned),
        }
    }
    #[test]
    fn a_transient_error_row_is_not_a_change_and_never_replaces_a_good_row() {
        use super::{changed, keep_good};
        let good = vec![row(1, "a", None), row(2, "b", None)];
        let flaky = vec![row(1, "a", Some("HTTP 502")), row(2, "b", None)];
        assert!(!changed(&good, &flaky));
        assert!(!changed(&flaky, &good));
        let kept = keep_good(&good, flaky.clone());
        assert_eq!(kept, good);
        // A first-time error stays visible; a real difference is a change.
        assert_eq!(keep_good(&[], flaky.clone()), flaky);
        assert!(changed(&good, &[row(1, "c", None), row(2, "b", None)]));
        assert!(changed(&good, &good[..1]));
    }
    #[test]
    fn the_summary_counts_states_and_puts_action_rows_first() {
        let mut failing = row(3, "c", None);
        failing.checks_failed = Some(1);
        let lines = vec![row(1, "a", None), failing, row(2, "b", Some("HTTP 502"))];
        let summary = super::summarise(&lines, true);
        assert_eq!(
            summary.lines[0],
            "3 PRs: 1 failing, 1 unavailable, 1 ready (1 need nothing)"
        );
        assert!(summary.lines[1].starts_with("#3 "), "{:?}", summary.lines);
        assert!(summary.lines[2].starts_with("#2 unavailable"));
        assert_eq!(
            summary.lines.last().map(String::as_str),
            Some("snapshot unchanged within wait limit")
        );
        assert_eq!(summary.next.as_deref(), Some("gh pr checks 3"));
    }

    #[test]
    fn ruleset_only_protection_is_distinct_from_a_failed_policy_lookup() {
        assert_eq!(
            super::classic_protection(
                false,
                br#"{"message":"Branch not protected","status":"404"}"#
            ),
            Some(None)
        );
        assert_eq!(
            super::classic_protection(false, br#"{"message":"Not Found","status":"404"}"#),
            None
        );
        assert_eq!(
            super::classic_protection(
                false,
                br#"{"message":"Resource not accessible","status":"403"}"#
            ),
            None
        );
    }
    #[test]
    fn rollup_distinguishes_running_completed_and_legacy_contexts() {
        use serde_json::json;
        let checks = super::rollup_checks(&[
            json!({"__typename":"CheckRun","name":"build","status":"IN_PROGRESS","conclusion":"SUCCESS"}),
            json!({"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"FAILURE"}),
            json!({"__typename":"StatusContext","context":"external","state":"SUCCESS"}),
        ]).expect("valid rollup");
        assert_eq!(
            checks
                .iter()
                .map(|check| check.bucket.as_str())
                .collect::<Vec<_>>(),
            ["pending", "fail", "pass"]
        );
        assert!(
            super::rollup_checks(&[
                json!({"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":""})
            ])
            .is_none()
        );
        assert!(super::rollup_checks(&[json!({"__typename":"CheckRun","name":"build","status":"UNKNOWN","conclusion":"SUCCESS"})]).is_none());
    }
    #[test]
    fn failed_policy_lookup_uses_all_reported_checks() {
        use super::*;
        use provenant_fabric_runtime::graph::CiState;
        for (bucket, expected) in [("fail", CiState::Failed), ("pending", CiState::Pending)] {
            let observation = observe_ci_with(
                &[],
                || None,
                || {
                    Some(vec![Check {
                        name: "build".into(),
                        bucket: bucket.into(),
                    }])
                },
            );
            assert_eq!(observation.state, expected);
            assert!(observation.summary.contains("policy=all-reported"));
        }
        assert_eq!(
            observe_ci_with(&[], || None, || Some(vec![])).state,
            CiState::Unavailable
        );
    }

    #[test]
    fn branch_requirements_merge_protection_and_paginated_rulesets() {
        use super::*;
        use provenant_fabric_runtime::graph::CiState;
        use serde_json::json;
        let protection = json!({"required_status_checks": {"contexts": ["build", "lint"]}});
        let pages = vec![
            vec![json!({"type": "required_status_checks", "parameters": {
                "required_status_checks": [{"context": "lint"}, {"context": "integration"}]
            }})],
            vec![
                json!({"type": "required_status_checks", "parameters": {
                    "required_status_checks": [{"context": "deploy"}]
                }}),
                json!({"type": "pull_request"}),
            ],
        ];
        let names = parse_required_checks(Some(&protection), &pages).unwrap();
        let names: Vec<String> = names.into_iter().map(|required| required.name).collect();
        assert_eq!(names, ["build", "deploy", "integration", "lint"]);
        let checks = ["build", "deploy", "integration", "lint"]
            .into_iter()
            .map(|name| Check {
                name: name.into(),
                bucket: if name == "integration" {
                    "skipping"
                } else {
                    "pass"
                }
                .into(),
            })
            .collect();
        let observation = summarise_ci(Some(checks), Some(("branch-required", &names)));
        assert_eq!(observation.state, CiState::Unavailable);
        assert!(observation.summary.contains("policy=branch-required"));
    }

    #[test]
    fn branch_requirements_reject_incomplete_data_and_accept_known_empty_policy() {
        use super::*;
        use serde_json::json;
        let no_protection = json!({"required_status_checks": null});
        assert_eq!(
            parse_required_checks(Some(&no_protection), &[]),
            Some(vec![])
        );
        assert_eq!(parse_required_checks(None, &[vec![]]), Some(vec![]));
        assert_eq!(parse_required_checks(Some(&json!({})), &[]), None);
        assert_eq!(
            parse_required_checks(
                None,
                &[vec![
                    json!({"type": "required_status_checks", "parameters": {}})
                ]]
            ),
            None
        );
        let rules = vec![vec![
            json!({"type": "required_status_checks", "parameters": {
                "required_status_checks": [{"context": "build"}]
            }}),
        ]];
        assert_eq!(
            parse_required_checks(Some(&no_protection), &rules),
            Some(vec![Required {
                name: "build".into(),
                app: None
            }])
        );
    }

    #[test]
    fn gate_check_buckets_fail_closed() {
        use super::*;
        use provenant_fabric_runtime::graph::CiState;
        let observe = |buckets: &[(&str, &str)], names: Option<&[String]>| {
            summarise_ci(
                Some(
                    buckets
                        .iter()
                        .map(|(name, bucket)| Check {
                            name: (*name).into(),
                            bucket: (*bucket).into(),
                        })
                        .collect(),
                ),
                names.map(|names| ("configured", names)),
            )
        };
        let required = vec!["build".to_owned()];
        assert_eq!(
            observe(&[("build", "pass"), ("lint", "skipping")], None).state,
            CiState::Passed
        );
        assert_eq!(
            observe(&[("build", "pass"), ("lint", "pending")], None).state,
            CiState::Pending
        );
        assert_eq!(observe(&[("build", "cancel")], None).state, CiState::Failed);
        assert_eq!(
            observe(&[("build", "neutral")], None).state,
            CiState::Unavailable
        );
        assert_eq!(
            observe(&[("build", "skipping")], Some(&required)).state,
            CiState::Unavailable
        );
        assert_eq!(
            observe(&[("lint", "pass")], Some(&required)).state,
            CiState::Unavailable
        );
        assert_eq!(
            observe(&[("build", "pass"), ("lint", "fail")], Some(&required)).state,
            CiState::Passed
        );
        assert_eq!(
            observe(&[("build", "pending")], Some(&required)).state,
            CiState::Pending
        );
        assert_eq!(observe(&[], None).state, CiState::Unavailable);
        assert_eq!(summarise_ci(None, None).state, CiState::Unavailable);
        assert!(
            observe(&[("build", "pass")], Some(&required))
                .summary
                .contains("policy=configured")
        );
    }
}
