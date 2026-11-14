//! `pr status`: one compact line per pull request.
use super::{Result, execute, successful};
use crate::{emit_summarised, fail, roots};
use provenant_cli::Globals;
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::path::Path;

mod ci;

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
    /// The pull request URL names the base repository, whose CI it runs.
    url: String,
}
#[derive(Deserialize)]
struct RestWaitPull {
    state: String,
    head: RestWaitHead,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    mergeable_state: Option<String>,
    #[serde(default)]
    draft: bool,
    html_url: String,
}
#[derive(Deserialize)]
struct RestWaitHead {
    sha: String,
}
#[derive(Deserialize)]
struct Check {
    #[serde(default)]
    name: String,
    bucket: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Line {
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
    pub(crate) fn class(&self) -> &'static str {
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
        } else if self.review.as_deref() == Some("unknown") {
            "review_unknown"
        } else {
            "ready"
        }
    }
    /// What a waiter takes from this pull request: `Some(true)` once it
    /// merged or its checks passed, `Some(false)` once they failed or it
    /// closed unmerged, and `None` while checks run or the read failed.
    pub(crate) fn verdict(&self) -> Option<bool> {
        match self.class() {
            "merged" => Some(true),
            "closed" | "failing" => Some(false),
            "unavailable" | "pending" => None,
            _ => (self.checks_pending.unwrap_or(0) == 0 && self.checks_total.is_some())
                .then_some(true),
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

/// Graph gates read checks for an exact head and recheck the target afterwards.
pub(crate) fn observe_ci(
    state: &Path,
    project: &Path,
    target: &str,
    names: &[String],
    failed_checks_max_bytes: usize,
    max_failed_checks: usize,
) -> provenant_fabric_runtime::graph::CiObservation {
    ci::observe(
        state,
        project,
        target,
        names,
        failed_checks_max_bytes,
        max_failed_checks,
    )
}

/// A conflicting pull request never starts CI, so say so rather than report
/// its checks as missing. A settled result keeps its state.
fn mark_conflict(
    observation: &mut provenant_fabric_runtime::graph::CiObservation,
    read_mergeable: impl FnOnce() -> Option<String>,
) {
    use provenant_fabric_runtime::graph::CiState;
    if !matches!(observation.state, CiState::Passed | CiState::Failed)
        && read_mergeable().as_deref() == Some("CONFLICTING")
    {
        observation.state = CiState::Conflict;
        observation
            .summary
            .push_str(" next: rebase the branch onto its base and push");
    }
}

#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes this module without libtest.
fn observe_ci_with(
    names: &[String],
    resolve: impl FnOnce() -> Option<Vec<String>>,
    read_checks: impl FnOnce() -> Option<Vec<Check>>,
    read_mergeable: impl FnOnce() -> Option<String>,
) -> provenant_fabric_runtime::graph::CiObservation {
    let required = if names.is_empty() {
        resolve().map(|names| ("branch-required", names))
    } else {
        Some(("configured", names.to_vec()))
    };
    let mut observation = summarise_ci(
        read_checks(),
        required
            .as_ref()
            .map(|(policy, names)| (*policy, names.as_slice())),
    );
    mark_conflict(&mut observation, read_mergeable);
    observation
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

/// A CI result pinned to the commit that reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CiCheck {
    pub head: String,
    pub name: String,
    pub bucket: String,
    pub app: Option<u64>,
}

/// Shared interpretation of a head's CI evidence. Callers may keep waiting on
/// `Pending`; the landing gate still decides whether a completed failure is
/// acceptable for its own policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CiCompletion {
    Pending,
    Green,
    Failed,
    Unavailable,
}

/// Decide whether CI is complete and green for exactly `head`.
pub(super) fn ci_completion(head: &str, checks: &[CiCheck], required: &[Required]) -> CiCompletion {
    if checks.iter().any(|check| check.head != head) {
        return CiCompletion::Unavailable;
    }
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
        return CiCompletion::Failed;
    }
    if checks.iter().any(|check| check.bucket == "pending") {
        return CiCompletion::Pending;
    }
    if required.iter().any(|need| {
        !checks
            .iter()
            .any(|check| need.matches(&check.name, check.app))
    }) || checks.is_empty()
    {
        return CiCompletion::Pending;
    }
    if selected
        .iter()
        .any(|check| !matches!(check.bucket.as_str(), "pass" | "skipping"))
        || (!required.is_empty() && selected.iter().any(|check| check.bucket == "skipping"))
        || !selected.iter().any(|check| check.bucket == "pass")
    {
        return CiCompletion::Unavailable;
    }
    CiCompletion::Green
}

pub(super) struct Policy {
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
    let repo = format!("repos/{}/{}", encode(owner), encode(name));
    required_policy_for(&repo, &base.branch, read)
}

pub(super) fn required_policy_for(
    repo: &str,
    base: &str,
    read: &dyn Fn(&[&str]) -> Option<provenant_os::exec::ExecOutcome>,
) -> Option<Policy> {
    let branch = encode(base);
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
        checks: parse_required_checks(protection.as_ref(), &pages)?,
    })
}

fn encode(value: &str) -> String {
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
            head_sha: None,
            failed_checks: Vec::new(),
            check_names: Vec::new(),
            truncated: false,
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
    let state = if checks.is_empty() {
        CiState::NotScheduled
    } else if total == 0
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
    let next = if state == CiState::NotScheduled {
        " next: check the workflow triggers or push an empty commit to start CI"
    } else {
        ""
    };
    CiObservation {
        extra: Default::default(),
        state,
        summary: format!(
            "policy={policy}{selection} checks={passed}/{total} fail={failed} pending={pending} skipped={skipped} missing={missing}{next}"
        ),
        head_sha: None,
        failed_checks: Vec::new(),
        check_names: selected.iter().map(|check| check.name.clone()).collect(),
        truncated: false,
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
    )?)?;
    let mut listed: Vec<Listed> = serde_json::from_slice(&bytes)?;
    listed.sort_by_key(|entry| entry.number);
    Ok(listed.into_iter().map(|entry| entry.number).collect())
}
/// Resolve the selected repository once before a `wait --pr` observation loop.
pub(crate) fn wait_repository(
    state: &Path,
    project: &Path,
    budget: std::time::Duration,
) -> Result<String> {
    Ok(super::observer::repository_for_project(
        state, project, budget,
    )?)
}

/// Revalidate PR metadata and its CI through the shared observer during a wait loop.
pub(crate) fn wait_line_observed(
    project: &Path,
    state: &Path,
    repository: &str,
    number: u64,
    timeout: std::time::Duration,
    max_age: std::time::Duration,
) -> Result<Line> {
    use super::observer::{ObserveError, Observer};
    let mut observer = Observer::open(state, project, repository, timeout)?;
    let value = observer.resource(
        &format!("pulls/{number}"),
        std::time::Duration::ZERO,
        timeout,
    )?;
    let pull: RestWaitPull = serde_json::from_value(value)?;
    let state = if pull.merged_at.is_some() {
        "MERGED".to_owned()
    } else {
        pull.state.to_ascii_uppercase()
    };
    let view = View {
        state,
        head: pull.head.sha,
        merge: pull
            .mergeable_state
            .map_or_else(|| "UNKNOWN".to_owned(), |state| state.to_ascii_uppercase()),
        review: Some("unknown".to_owned()),
        draft: pull.draft,
        url: pull.html_url,
    };
    let checks = match observer.checks(&view.head, max_age, timeout) {
        Ok(checks) => Some(checks),
        Err(ObserveError::Unavailable(_)) => None,
        Err(error) => return Err(error.to_string().into()),
    };
    Ok(status_line(number, view, checks.as_deref()))
}

/// `max_age` is how old an observed CI answer may be: a waiter's poll, or
/// zero for a one-shot read.
pub(crate) fn wait_line(
    project: &Path,
    state: &Path,
    number: u64,
    timeout: std::time::Duration,
    max_age: std::time::Duration,
    cancel: Option<&provenant_core::CancelToken>,
) -> Result<Line> {
    use super::observer::{ObserveError, Observer};
    let text = number.to_string();
    let bytes = successful(super::execute_cancellable(
        project,
        &[
            "gh",
            "pr",
            "view",
            &text,
            "--json",
            "state,headRefOid,mergeStateStatus,reviewDecision,isDraft,url",
        ],
        timeout,
        false,
        None,
        cancel,
    )?)?;
    let view: View = serde_json::from_slice(&bytes)?;
    let segments: Vec<&str> = view.url.trim_end_matches('/').split('/').collect();
    let repository = match segments.as_slice() {
        [.., owner, name, "pull", _] => format!("{owner}/{name}"),
        _ => return Err(format!("pull request URL names no repository: {}", view.url).into()),
    };
    // Unreadable or incomplete checks leave the row readable, as before; a
    // rate limit or an expired read is a failed row naming why.
    let checks = match Observer::open(state, project, &repository, timeout)
        .and_then(|mut observer| observer.checks(&view.head, max_age, timeout))
    {
        Ok(checks) => Some(checks),
        Err(ObserveError::Unavailable(_)) => None,
        Err(error) => return Err(error.to_string().into()),
    };
    Ok(status_line(number, view, checks.as_deref()))
}

/// One row from the pull request view and its observed checks.
fn status_line(number: u64, view: View, checks: Option<&[super::observer::Check]>) -> Line {
    let checks = checks.filter(|checks| !checks.is_empty());
    let count = |buckets: &[&str]| {
        checks.map(|checks| {
            checks
                .iter()
                .filter(|check| buckets.contains(&check.bucket.as_str()))
                .count()
        })
    };
    let word = |text: &str| {
        if text.is_empty() {
            "none".to_owned()
        } else {
            text.to_lowercase()
        }
    };
    Line {
        number,
        checks_passed: count(&["pass", "skipping"]),
        checks_total: checks.map(<[_]>::len),
        checks_failed: count(&["fail", "cancel"]),
        checks_pending: count(&["pending"]),
        merge: word(&view.merge),
        review: word(view.review.as_deref().unwrap_or_default()),
        state: view.state,
        head: view.head,
        draft: view.draft,
        error: None,
    }
}

/// The `pr status` class of an open, approved pull request whose head has
/// `checks`.
#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes this module without libtest.
pub(super) fn class_of(head: &str, checks: &[super::observer::Check]) -> &'static str {
    let view = View {
        state: "OPEN".into(),
        head: head.into(),
        merge: "CLEAN".into(),
        review: Some("APPROVED".into()),
        draft: false,
        url: "https://github.com/owner/repo/pull/1".into(),
    };
    status_line(1, view, Some(checks)).class()
}

fn snapshot(project: &Path, state: &Path, numbers: &[u64]) -> Vec<Line> {
    numbers
        .iter()
        .map(|&number| {
            wait_line(
                project,
                state,
                number,
                std::time::Duration::from_secs(60),
                std::time::Duration::ZERO,
                None,
            )
            .unwrap_or_else(|error| Line {
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

pub(super) fn run(numbers: Vec<u64>, globals: &Globals) -> u8 {
    const LEAF: &str = "pr status";
    let result = (|| -> Result<Vec<Line>> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let state = roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional)?
            .roots
            .state;
        let numbers = if numbers.is_empty() {
            open_numbers(&project)?
        } else {
            numbers
        };
        Ok(snapshot(&project, state.as_path(), &numbers))
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
            let summary = summarise(&lines);
            let outcome = if !lines.is_empty() && lines.iter().all(|line| line.error.is_some()) {
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
fn summarise(lines: &[Line]) -> provenant_cli::Summary {
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
    let rows: Vec<String> = action.iter().map(|line| line.text()).collect();
    let quiet = lines.len() - action.len();
    let next = match action.first() {
        Some(line) if line.class() == "failing" => Some(format!("gh pr checks {}", line.number)),
        Some(line) if line.class() == "pending" => {
            Some(format!("provenant wait --pr {}", line.number))
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
    fn the_summary_counts_states_and_puts_action_rows_first() {
        let mut failing = row(3, "c", None);
        failing.checks_failed = Some(1);
        let lines = vec![row(1, "a", None), failing, row(2, "b", Some("HTTP 502"))];
        let summary = super::summarise(&lines);
        assert_eq!(
            summary.lines[0],
            "3 PRs: 1 failing, 1 unavailable, 1 ready (1 need nothing)"
        );
        assert!(summary.lines[1].starts_with("#3 "), "{:?}", summary.lines);
        assert!(summary.lines[2].starts_with("#2 unavailable"));
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
                || None,
            );
            assert_eq!(observation.state, expected);
            assert!(observation.summary.contains("policy=all-reported"));
        }
        let empty = observe_ci_with(&[], || None, || Some(vec![]), || None);
        assert_eq!(empty.state, CiState::NotScheduled);
        assert!(empty.summary.contains("next:"));
        let conflict =
            observe_ci_with(&[], || None, || Some(vec![]), || Some("CONFLICTING".into()));
        assert_eq!(conflict.state, CiState::Conflict);
        assert!(conflict.summary.contains("next: rebase"));
        // A failed head stays failed even when the base also conflicts.
        let failed = observe_ci_with(
            &[],
            || None,
            || {
                Some(vec![Check {
                    name: "build".into(),
                    bucket: "fail".into(),
                }])
            },
            || Some("CONFLICTING".into()),
        );
        assert_eq!(failed.state, CiState::Failed);
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
        // No reported checks means CI never started: still not passed.
        assert_eq!(observe(&[], None).state, CiState::NotScheduled);
        assert_eq!(observe(&[], Some(&required)).state, CiState::NotScheduled);
        assert_eq!(summarise_ci(None, None).state, CiState::Unavailable);
        assert!(
            observe(&[("build", "pass")], Some(&required))
                .summary
                .contains("policy=configured")
        );
    }
}
