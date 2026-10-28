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
