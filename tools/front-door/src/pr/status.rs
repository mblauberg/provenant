//! `pr status`: one compact line per pull request.
use super::{Result, execute, successful};
use crate::{emit, fail, roots};
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
}
#[derive(Deserialize)]
struct Check {
    bucket: String,
}
#[derive(Serialize)]
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
}
impl Line {
    fn text(&self) -> String {
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
        &["gh", "pr", "checks", number, "--json", "bucket"],
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
) -> provenant_fabric_runtime::graph::CiObservation {
    summarise_ci(counts(project, target))
}

fn summarise_ci(checks: Option<Vec<Check>>) -> provenant_fabric_runtime::graph::CiObservation {
    use provenant_fabric_runtime::graph::{CiObservation, CiState};
    let Some(checks) = checks else {
        return CiObservation {
            state: CiState::Unavailable,
            summary: "checks unavailable".into(),
        };
    };
    let passed = checks.iter().filter(|c| c.bucket == "pass").count();
    let skipped = checks.iter().filter(|c| c.bucket == "skipping").count();
    let failed = checks
        .iter()
        .filter(|c| matches!(c.bucket.as_str(), "fail" | "cancel"))
        .count();
    let pending = checks.iter().filter(|c| c.bucket == "pending").count();
    let total = checks.len();
    let state = if total == 0 || passed + skipped + failed + pending != total {
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
    CiObservation {
        state,
        summary: format!(
            "checks={passed}/{total} fail={failed} pending={pending} skipped={skipped}"
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
fn line(project: &Path, number: u64) -> Result<Line> {
    let text = number.to_string();
    let bytes = successful(execute(
        project,
        &[
            "gh",
            "pr",
            "view",
            &text,
            "--json",
            "state,headRefOid,mergeStateStatus,reviewDecision,isDraft",
        ],
        false,
    )?)?;
    let view: View = serde_json::from_slice(&bytes)?;
    let checks = counts(project, &text);
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
    })
}

pub(super) fn run(numbers: Vec<u64>, globals: &Globals) -> u8 {
    const LEAF: &str = "pr status";
    let result = (|| -> Result<Vec<Line>> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let numbers = if numbers.is_empty() {
            open_numbers(&project)?
        } else {
            numbers
        };
        numbers
            .into_iter()
            .map(|number| line(&project, number))
            .collect()
    })();
    match result {
        Ok(lines) => {
            let text: String = lines.iter().map(|line| line.text() + "\n").collect();
            emit(
                &CommandResult::new(LEAF, Outcome::Ok, lines),
                globals,
                &text,
                true,
            )
        }
        Err(error) => fail(LEAF, Outcome::Error, "PR", &error.to_string(), globals),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gate_check_buckets_fail_closed() {
        use super::*;
        use provenant_fabric_runtime::graph::CiState;
        let observe = |buckets: &[&str]| {
            summarise_ci(Some(
                buckets
                    .iter()
                    .map(|bucket| Check {
                        bucket: (*bucket).into(),
                    })
                    .collect(),
            ))
        };
        assert_eq!(observe(&["pass", "skipping"]).state, CiState::Passed);
        assert_eq!(observe(&["pass", "pending"]).state, CiState::Pending);
        assert_eq!(observe(&["pending", "fail"]).state, CiState::Failed);
        assert_eq!(observe(&[]).state, CiState::Unavailable);
        assert_eq!(observe(&["skipping"]).state, CiState::Unavailable);
        assert_eq!(observe(&["pass", "unexpected"]).state, CiState::Unavailable);
        assert_eq!(summarise_ci(None).state, CiState::Unavailable);
        assert_eq!(
            observe(&["pass", "pending"]).summary,
            "checks=1/2 fail=0 pending=1 skipped=0"
        );
    }
}
