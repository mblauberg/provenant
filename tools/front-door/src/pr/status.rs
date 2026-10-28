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
    #[serde(default)]
    name: String,
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
    let required = if names.is_empty() {
        required_checks(project, target).map(|names| ("branch-required", names))
    } else {
        Some(("configured", names.to_vec()))
    };
    summarise_ci(
        counts(project, target),
        required
            .as_ref()
            .map(|(policy, names)| (*policy, names.as_slice())),
    )
}

// Both classic protection and rulesets may apply. Only trust the resolved
// policy when both sources are readable.
fn required_checks(project: &Path, target: &str) -> Option<Vec<String>> {
    #[derive(Deserialize)]
    struct Repo {
        name: String,
    }
    #[derive(Deserialize)]
    struct Owner {
        login: String,
    }
    #[derive(Deserialize)]
    struct Base {
        #[serde(rename = "baseRefName")]
        branch: String,
        #[serde(rename = "baseRepository")]
        repo: Repo,
        #[serde(rename = "baseRepositoryOwner")]
        owner: Owner,
    }
    let bytes = successful(
        execute(
            project,
            &[
                "gh",
                "pr",
                "view",
                target,
                "--json",
                "baseRefName,baseRepository,baseRepositoryOwner",
            ],
            false,
        )
        .ok()?,
    )
    .ok()?;
    let base: Base = serde_json::from_slice(&bytes).ok()?;
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
    let repo = format!(
        "repos/{}/{}",
        encode(&base.owner.login),
        encode(&base.repo.name)
    );
    let branch = encode(&base.branch);
    let get = |path: &str, paginate: bool| -> Option<Vec<u8>> {
        let args = if paginate {
            vec!["gh", "api", "--paginate", "--slurp", path]
        } else {
            vec!["gh", "api", path]
        };
        successful(execute(project, &args, false).ok()?).ok()
    };
    let branch_info: serde_json::Value =
        serde_json::from_slice(&get(&format!("{repo}/branches/{branch}"), false)?).ok()?;
    let mut names = Vec::new();
    if branch_info.get("protected")?.as_bool()? {
        let protection: serde_json::Value = serde_json::from_slice(&get(
            &format!("{repo}/branches/{branch}/protection"),
            false,
        )?)
        .ok()?;
        if let Some(checks) = protection
            .get("required_status_checks")
            .filter(|v| !v.is_null())
        {
            for name in checks.get("contexts")?.as_array()? {
                names.push(name.as_str()?.to_owned());
            }
        }
    }
    let pages: Vec<Vec<serde_json::Value>> = serde_json::from_slice(&get(
        &format!("{repo}/rules/branches/{branch}?per_page=100"),
        true,
    )?)
    .ok()?;
    for rule in pages.into_iter().flatten() {
        if rule.get("type")?.as_str()? == "required_status_checks" {
            for check in rule
                .get("parameters")?
                .get("required_status_checks")?
                .as_array()?
            {
                names.push(check.get("context")?.as_str()?.to_owned());
            }
        }
    }
    names.sort();
    names.dedup();
    // An empty required set cannot establish that any CI ran.
    (!names.is_empty()).then_some(names)
}

fn summarise_ci(
    checks: Option<Vec<Check>>,
    required: Option<(&str, &[String])>,
) -> provenant_fabric_runtime::graph::CiObservation {
    use provenant_fabric_runtime::graph::{CiObservation, CiState};
    let policy = required.map_or("all-reported", |(policy, _)| policy);
    let Some(checks) = checks else {
        return CiObservation {
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
