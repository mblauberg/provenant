//! `pr status`: one compact line per pull request.
use super::{Result, successful};
use crate::{emit_summarised, fail, roots};
use provenant_cli::Globals;
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(crate) mod batch;
mod ci;
mod membership;

/// Why a PR whose CI never reported checks stayed pending. Shared by the
/// graph gate's CI summary and by `wait --pr` at a timeout.
pub(crate) const NO_CHECKS_NEXT: &str =
    "check the workflow triggers or push an empty commit to start CI";

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
    base: Option<RestWaitBase>,
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
    #[serde(rename = "ref", default)]
    branch: String,
}
#[derive(Deserialize)]
struct RestWaitBase {
    #[serde(rename = "ref", default)]
    branch: String,
}
#[derive(Deserialize)]
struct Check {
    #[serde(default)]
    name: String,
    bucket: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FailedCheck {
    name: String,
    url: Option<String>,
    /// The observer bucket that produced this failure: `fail` or `cancel`.
    #[serde(default)]
    bucket: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Line {
    number: u64,
    state: String,
    pub(crate) head: String,
    #[serde(default, deserialize_with = "display_default")]
    draft: bool,
    #[serde(default, deserialize_with = "display_default")]
    checks_passed: Option<usize>,
    #[serde(default, deserialize_with = "display_default")]
    checks_total: Option<usize>,
    #[serde(default)]
    checks_failed: Option<usize>,
    #[serde(default)]
    checks_pending: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_workflows: Option<bool>,
    /// Failed or cancelled check names, when checks were readable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default, deserialize_with = "display_default")]
    failing_checks: Vec<String>,
    /// Names of checks still running on this head, shown when a wait times out.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default, deserialize_with = "display_default")]
    pending_checks: Vec<String>,
    /// Links from the same observation, retained for completion reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    failed_check_details: Vec<FailedCheck>,
    #[serde(default, deserialize_with = "display_default")]
    merge: String,
    #[serde(default)]
    review: String,
    /// Why no check was reported for this head, when that is known: either
    /// `merge_conflict` or `not_scheduled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cause: Option<String>,
    /// The action that unblocks the head, paired with `cause`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next: Option<String>,
    /// The exact head a waiter is watching. GitHub has not reported it yet, so
    /// the head carried in `head` belongs to an earlier push and no verdict may
    /// be drawn from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) awaiting: Option<String>,
    /// A waiter holds this reading pending: its checks would settle it, but
    /// GitHub has reported no Actions run for the head yet, so checks that
    /// start later cannot be ruled out. It lasts only a bounded grace period.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) actions_grace: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    error: Option<String>,
    /// The open batch this pull request was merged into, when Git history
    /// proves its head is carried there. Its own cancelled CI is superseded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch: Option<membership::Link>,
    /// The members and file attribution a batch row reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch_detail: Option<membership::BatchDetail>,
    /// Shared history with a batch that does not prove membership. Reported
    /// without a batch claim so the member's own CI stays visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch_uncertain: Option<String>,
}
fn display_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

impl Line {
    /// Keep this reading pending until GitHub reports the head being watched.
    pub(crate) fn await_head(&mut self, head: &str) {
        self.awaiting = Some(head.to_owned());
        self.checks_passed = None;
        self.checks_total = None;
        self.checks_failed = None;
        self.checks_pending = None;
        self.failing_checks.clear();
        self.pending_checks.clear();
        self.failed_check_details.clear();
    }
    /// Whether a waiter should hold this reading through a grace period: it
    /// would settle an open pull request although no Actions run has been seen
    /// for its head, so a workflow the push is about to start could still
    /// change the verdict. A failure settles at once unless the wait is
    /// complete, which promises to see every check.
    pub(crate) fn awaits_actions(&self, complete: bool) -> bool {
        self.error.is_none()
            && self.awaiting.is_none()
            && self.state == "OPEN"
            && self.active_workflows.is_none()
            && match self.wait_verdict(complete) {
                Some(passed) => passed || complete,
                None => false,
            }
    }
    /// Keep this reading pending while Actions runs for its head are awaited.
    pub(crate) fn hold_for_actions(&mut self) {
        self.actions_grace = true;
    }
    /// The state an operator acts on, most urgent first.
    pub(crate) fn class(&self) -> &'static str {
        if self.error.is_some() {
            "unavailable"
        } else if self.awaiting.is_some() || self.actions_grace {
            "pending"
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
        } else if self.review == "unknown" {
            "review_unknown"
        } else {
            "ready"
        }
    }
    /// The class a `pr status` summary reports. A member carried by a batch
    /// is a state report, not a failure its own cancelled runs can cause.
    pub(crate) fn display_class(&self) -> &'static str {
        if self.batch.is_some() && self.checks_failed.unwrap_or(0) == 0 {
            "superseded"
        } else {
            self.class()
        }
    }
    /// A batch supersedes the cancelled checks on its member's head, not real
    /// failures: only cancellations are dropped, so an actual failure stays
    /// visible. The batch's CI is reported alongside instead.
    pub(crate) fn suppress_own_ci(&mut self) {
        self.failed_check_details
            .retain(|check| check.bucket != "cancel");
        self.failing_checks = self
            .failed_check_details
            .iter()
            .map(|check| check.name.clone())
            .collect();
        self.checks_failed = Some(self.failed_check_details.len());
        self.batch_uncertain = None;
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
    /// Complete waits retain failures as pending until the other checks finish.
    pub(crate) fn wait_verdict(&self, complete: bool) -> Option<bool> {
        if complete && (self.checks_pending.unwrap_or(0) > 0 || self.active_workflows == Some(true))
        {
            None
        } else {
            self.verdict()
        }
    }

    pub(crate) fn failure_lines(&self) -> Vec<String> {
        self.failed_check_details
            .iter()
            .map(|check| {
                format!(
                    "#{} failed: {}{}",
                    self.number,
                    display_check_name(&check.name),
                    check
                        .url
                        .as_deref()
                        .and_then(display_check_url)
                        .map_or(String::new(), |url| format!(" {url}"))
                )
            })
            .collect()
    }
    /// True when no check has been observed for this pull request's head.
    pub(crate) fn checks_unreported(&self) -> bool {
        self.checks_total.is_none()
    }
    /// The readable check summary: head, check counts, failing names, merge
    /// and review. `wait --pr` uses this when a PR wait ends without a verdict.
    pub(crate) fn text(&self) -> String {
        if let Some(error) = &self.error {
            return format!("#{} unavailable: {error}", self.number);
        }
        if let Some(awaiting) = &self.awaiting {
            return format!(
                "#{} waiting for head {} (GitHub reports {})",
                self.number, awaiting, self.head
            );
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
                let names = self.failing_checks.join(",");
                if !names.is_empty() {
                    line.push_str(&format!(" names={names}"));
                }
            }
            if pending > 0 {
                line.push_str(&format!(" pending={pending}"));
                let names = self.pending_checks.join(",");
                if !names.is_empty() {
                    line.push_str(&format!(" pending_names={names}"));
                }
            }
        } else {
            line.push_str(" checks=unavailable");
        }
        line.push_str(&format!(" merge={} review={}", self.merge, self.review));
        if self.draft {
            line.push_str(" draft");
        }
        if self.actions_grace {
            line.push_str(" awaiting Actions runs");
        }
        if let Some(link) = &self.batch {
            line.push_str(&link.suffix());
        }
        if let Some(detail) = &self.batch_detail {
            line.push_str(&detail.suffix());
        }
        if let Some(note) = &self.batch_uncertain {
            line.push_str(note);
        }
        line.push_str(&self.cause_suffix());
        line
    }
    /// The diagnosis a waiter prints for a head whose checks never started.
    pub(crate) fn cause_suffix(&self) -> String {
        match (&self.cause, &self.next) {
            (Some(cause), Some(next)) => format!(" cause={cause} next: {next}"),
            _ => String::new(),
        }
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

/// The longest check name a status or failure line carries.
const CHECK_NAME_BYTES: usize = 120;
/// The longest check link a failure line carries. A longer link is dropped,
/// because a clipped URL would point somewhere else.
const CHECK_URL_BYTES: usize = 300;

/// A check name is provider-controlled text: escape terminal controls and
/// bound it before it reaches an output line.
fn display_check_name(name: &str) -> String {
    provenant_cli::bounded_text(
        &provenant_records::diagnostic::safe_text(name),
        CHECK_NAME_BYTES,
    )
}

/// A check link is shown only when it is a bounded http(s) URL without
/// whitespace or control characters.
fn display_check_url(url: &str) -> Option<String> {
    let web = url.starts_with("https://") || url.starts_with("http://");
    (web && url.len() <= CHECK_URL_BYTES
        && !url.chars().any(|c| c.is_whitespace() || c.is_control()))
    .then(|| url.to_owned())
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
pub(super) fn required_checks(state: &Path, project: &Path, target: &str) -> Option<Vec<String>> {
    required_policy(target, &|argv| {
        super::forge_execute(
            state,
            project,
            argv,
            &provenant_config::ports::Request::within(std::time::Duration::from_secs(60)),
            false,
            None,
        )
        .ok()
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
        format!(" next: {NO_CHECKS_NEXT}")
    } else {
        String::new()
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
fn open_numbers(state: &Path, project: &Path) -> Result<Vec<u64>> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
    }
    let bytes = successful(super::forge_execute(
        state,
        project,
        &[
            "gh", "pr", "list", "--author", "@me", "--state", "open", "--limit", "50", "--json",
            "number",
        ],
        &provenant_config::ports::Request::within(std::time::Duration::from_secs(60)),
        false,
        None,
    )?)?;
    let mut listed: Vec<Listed> = serde_json::from_slice(&bytes)?;
    listed.sort_by_key(|entry| entry.number);
    Ok(listed.into_iter().map(|entry| entry.number).collect())
}
/// Resolve the selected repository once before a `wait --pr` observation
/// loop, within the waiter's request context.
pub(crate) fn wait_repository(
    state: &Path,
    project: &Path,
    request: &provenant_config::ports::Request,
) -> Result<String> {
    Ok(super::observer::repository_for_project(
        state, project, request,
    )?)
}

/// The head GitHub reports for a pull request and the branch it came from.
/// A wait without `--head` uses the branch to find a local tip worth waiting
/// for when the two disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitHead {
    pub(crate) sha: String,
    pub(crate) branch: String,
}

/// One authoritative PR metadata read for head-pinning setup, sharing the
/// waiter's request context with the observation loop.
pub(crate) fn wait_head(
    state: &Path,
    project: &Path,
    repository: &str,
    number: u64,
    request: &provenant_config::ports::Request,
) -> Result<WaitHead> {
    let mut observer = super::observer::Observer::open(state, project, repository, request)?;
    let value = observer.resource(
        &format!("pulls/{number}"),
        std::time::Duration::ZERO,
        request,
    )?;
    let pull: RestWaitPull = serde_json::from_value(value)?;
    Ok(WaitHead {
        sha: pull.head.sha,
        branch: pull.head.branch,
    })
}

/// Revalidate PR metadata, its CI and its Actions runs through the shared
/// observer during a wait loop. The metadata read, every check page, the checks
/// read and the Actions runs read share `request`'s one deadline and
/// cancellation. Unavailable Actions runs warn once per wait.
pub(crate) fn wait_line_observed(
    project: &Path,
    state: &Path,
    repository: &str,
    number: u64,
    request: &provenant_config::ports::Request,
    max_age: std::time::Duration,
) -> Result<Line> {
    use super::observer::Observer;
    let mut observer = Observer::open(state, project, repository, request)?;
    let mut line = poll_line(&mut observer, number, None, request, max_age)?.0;
    match observer.active_workflows(&line.head, max_age, request) {
        Ok(Some(active)) => line.active_workflows = Some(active),
        Ok(None) => crate::event_delivery::warn_once(
            "Actions runs unavailable for this head (no runs found); using observed checks only",
        ),
        Err(error) => crate::event_delivery::warn_once(&format!(
            "Actions runs unavailable for this head ({error}); using observed checks only"
        )),
    }
    Ok(line)
}

/// One shared conditional host read of PR metadata and checks. Waiters use this
/// only to reconcile; the tick indexes the same projection for replay.
pub(crate) fn poll_line<F: provenant_config::Forge, C: super::observer::Clock>(
    observer: &mut super::observer::Observer<F, C>,
    number: u64,
    head_override: Option<&str>,
    request: &provenant_config::ports::Request,
    max_age: std::time::Duration,
) -> Result<(Line, Vec<super::observer::Check>)> {
    let value = observer.resource(
        &format!("pulls/{number}"),
        std::time::Duration::ZERO,
        request,
    )?;
    let pull: RestWaitPull = serde_json::from_value(value.clone())?;
    let state = if pull.merged_at.is_some() {
        "MERGED".to_owned()
    } else {
        pull.state.to_ascii_uppercase()
    };
    let base = pull.base.as_ref().map(|base| base.branch.clone());
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
    let checks = observer.checks(head_override.unwrap_or(&view.head), max_age, request)?;
    let mut line = status_line(number, view, Some(&checks));
    if checks.is_empty() {
        // Mergeability is read (and an UNKNOWN reread) only when it can change
        // the diagnosis: a head with checks needs no cause.
        let (merge_state, _) = observer.pull_merge_state(number, value, request)?;
        let cause = merge_state.no_checks_cause(base.as_deref());
        line.cause = Some(cause.code.to_owned());
        line.next = Some(cause.next);
    }
    Ok((line, checks))
}

/// `max_age` is how old an observed CI answer may be: a waiter's poll, or
/// zero for a one-shot read.
pub(crate) fn wait_line(
    project: &Path,
    state: &Path,
    number: u64,
    request: &provenant_config::ports::Request,
    max_age: std::time::Duration,
) -> Result<Line> {
    use super::observer::{ObserveError, Observer};
    let text = number.to_string();
    let bytes = successful(super::forge_execute(
        state,
        project,
        &[
            "gh",
            "pr",
            "view",
            &text,
            "--json",
            "state,headRefOid,mergeStateStatus,reviewDecision,isDraft,url",
        ],
        request,
        false,
        None,
    )?)?;
    let view: View = serde_json::from_slice(&bytes)?;
    let segments: Vec<&str> = view.url.trim_end_matches('/').split('/').collect();
    let repository = match segments.as_slice() {
        [.., owner, name, "pull", _] => format!("{owner}/{name}"),
        _ => return Err(format!("pull request URL names no repository: {}", view.url).into()),
    };
    // Unreadable or incomplete checks leave the row readable, as before; a
    // rate limit or an expired read is a failed row naming why.
    let checks = match Observer::open(state, project, &repository, request)
        .and_then(|mut observer| observer.checks(&view.head, max_age, request))
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
        active_workflows: None,
        failing_checks: checks
            .unwrap_or(&[])
            .iter()
            .filter(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
            .map(|check| display_check_name(&check.name))
            .collect(),
        pending_checks: checks
            .unwrap_or(&[])
            .iter()
            .filter(|check| check.bucket == "pending")
            .map(|check| display_check_name(&check.name))
            .collect(),
        failed_check_details: checks
            .unwrap_or(&[])
            .iter()
            .filter(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
            .map(|check| FailedCheck {
                name: display_check_name(&check.name),
                url: check.url.as_deref().and_then(display_check_url),
                bucket: check.bucket.clone(),
            })
            .collect(),
        merge: word(&view.merge),
        review: word(view.review.as_deref().unwrap_or_default()),
        state: view.state,
        head: view.head,
        draft: view.draft,
        cause: None,
        next: None,
        awaiting: None,
        actions_grace: false,
        error: None,
        batch: None,
        batch_detail: None,
        batch_uncertain: None,
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
    // One GraphQL query answers every pull request when it can; the per-PR
    // REST reads remain the fallback when the batch transport cannot.
    let batch = batch::snapshot(state, project, numbers);
    let batch_limited = matches!(batch, batch::Batch::RateLimited(_));
    let entries = match batch {
        batch::Batch::Entries(entries) => entries,
        _ => Vec::new(),
    };
    let mut lines: Vec<(_, Line)> = entries
        .iter()
        .map(|entry| (entry.number, batch::line_from_entry(entry)))
        .collect();
    // A rollup too large for one page is not evidence; re-read it exactly.
    for entry in &entries {
        if entry.truncated
            && let Some(line) = {
                let request =
                    provenant_config::ports::Request::within(std::time::Duration::from_secs(60));
                wait_line(
                    project,
                    state,
                    entry.number,
                    &request,
                    std::time::Duration::ZERO,
                )
                .ok()
            }
        {
            lines.retain(|(number, _)| *number != entry.number);
            lines.push((entry.number, line));
        }
    }
    let answered: std::collections::BTreeSet<_> = lines.iter().map(|(number, _)| *number).collect();
    for &number in numbers {
        if answered.contains(&number) {
            continue;
        }
        lines.push((number, rest_line(project, state, number, !batch_limited)));
    }
    let order: std::collections::BTreeMap<u64, usize> = numbers
        .iter()
        .enumerate()
        .map(|(index, &number)| (number, index))
        .collect();
    lines.sort_by_key(|(number, _)| order.get(number).copied().unwrap_or(usize::MAX));
    let mut lines: Vec<Line> = lines.into_iter().map(|(_, line)| line).collect();
    membership::annotate(project, state, &mut lines);
    lines
}

/// The per-PR REST read, falling back to one GraphQL query when REST itself
/// runs out of quota. `batch_limited` is false only while the GraphQL quota
/// may still answer, so a PR never loops between the two transports.
fn rest_line(project: &Path, state: &Path, number: u64, try_graphql: bool) -> Line {
    let request = provenant_config::ports::Request::within(std::time::Duration::from_secs(60));
    match wait_line(project, state, number, &request, std::time::Duration::ZERO) {
        Ok(line) => line,
        Err(error) => {
            if try_graphql
                && looks_rate_limited(error.as_ref())
                && let batch::Batch::Entries(entries) = batch::single(state, project, number)
                && let Some(entry) = entries.first()
                && !entry.truncated
            {
                return batch::line_from_entry(entry);
            }
            Line {
                number,
                state: "UNAVAILABLE".into(),
                head: "unknown".into(),
                draft: false,
                checks_passed: None,
                checks_total: None,
                checks_failed: None,
                checks_pending: None,
                active_workflows: None,
                failing_checks: Vec::new(),
                pending_checks: Vec::new(),
                failed_check_details: Vec::new(),
                merge: "unknown".into(),
                review: "unknown".into(),
                cause: None,
                next: None,
                awaiting: None,
                actions_grace: false,
                error: Some(provenant_cli::bounded_text(&error.to_string(), 256)),
                batch: None,
                batch_detail: None,
                batch_uncertain: None,
            }
        }
    }
}

/// A rate-limited read names the limit in its message, whatever transport
/// produced it.
fn looks_rate_limited(error: &dyn std::error::Error) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("rate limit")
}

pub(super) fn run(numbers: Vec<u64>, globals: &Globals) -> u8 {
    const LEAF: &str = "pr status";
    let result = (|| -> Result<Vec<Line>> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let state = roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional)?
            .roots
            .state;
        let numbers = if numbers.is_empty() {
            open_numbers(state.as_path(), &project)?
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
        "superseded",
        "draft",
        "ready",
        "merged",
        "closed",
    ]
    .into_iter()
    .map(|class| {
        (
            class,
            lines
                .iter()
                .filter(|line| line.display_class() == class)
                .count(),
        )
    })
    .collect();
    let mut action: Vec<&Line> = lines
        .iter()
        .filter(|line| order.contains(&line.display_class()))
        .collect();
    action.sort_by_key(|line| {
        order
            .iter()
            .position(|class| *class == line.display_class())
    });
    let rows: Vec<String> = action.iter().map(|line| line.text()).collect();
    let quiet = lines.len() - action.len();
    let next = match action.first() {
        Some(line) if line.class() == "failing" => {
            Some(format!("provenant ci failures pr {}", line.number))
        }
        Some(line) if line.class() == "pending" => {
            Some(format!("provenant wait --pr {}", line.number))
        }
        Some(line) => Some(format!("provenant pr status {} --full", line.number)),
        None => lines
            .iter()
            .find(|line| line.display_class() == "ready")
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
            active_workflows: None,
            failing_checks: Vec::new(),
            pending_checks: Vec::new(),
            failed_check_details: Vec::new(),
            merge: "clean".into(),
            review: "approved".into(),
            cause: None,
            next: None,
            awaiting: None,
            actions_grace: false,
            error: error.map(str::to_owned),
            batch: None,
            batch_detail: None,
            batch_uncertain: None,
        }
    }
    #[test]
    fn completion_report_retains_every_failed_check_and_link() {
        let check = |name: &str, bucket: &str, url: &str| super::super::observer::Check {
            head: "head".into(),
            name: name.into(),
            bucket: bucket.into(),
            app: None,
            app_slug: None,
            conclusion: None,
            registered: None,
            url: Some(url.into()),
            excerpt: String::new(),
        };
        let view = super::View {
            state: "OPEN".into(),
            head: "head".into(),
            merge: "".into(),
            review: None,
            draft: false,
            url: String::new(),
        };
        let checks = [
            check("one", "fail", "https://ci/one"),
            check("two", "cancel", "https://ci/two"),
        ];
        let line = super::status_line(1668, view, Some(&checks));
        assert_eq!(
            line.failure_lines(),
            [
                "#1668 failed: one https://ci/one",
                "#1668 failed: two https://ci/two"
            ]
        );
        let json = serde_json::to_value(line).unwrap();
        assert_eq!(json["failed_check_details"][1]["url"], "https://ci/two");
    }

    #[test]
    fn pending_check_names_survive_the_status_line_and_render() {
        let check = |name: &str, bucket: &str| super::super::observer::Check {
            head: "head".into(),
            name: name.into(),
            bucket: bucket.into(),
            app: None,
            app_slug: None,
            conclusion: None,
            registered: None,
            url: None,
            excerpt: String::new(),
        };
        let view = super::View {
            state: "OPEN".into(),
            head: "head".into(),
            merge: "clean".into(),
            review: Some("approved".into()),
            draft: false,
            url: String::new(),
        };
        let checks = [
            check("build", "pass"),
            check("lint", "pending"),
            check("deploy", "pending"),
        ];
        let line = super::status_line(7, view, Some(&checks));
        assert_eq!(line.pending_checks, ["lint", "deploy"]);
        let text = line.text();
        assert!(text.contains("pending=2"), "{text}");
        assert!(text.contains("pending_names=lint,deploy"), "{text}");
        let json = serde_json::to_value(&line).unwrap();
        assert_eq!(json["pending_checks"][0], "lint");
        // A new head has no checks yet, so no stale pending name survives.
        let mut waiting = line;
        waiting.await_head("new");
        assert!(waiting.pending_checks.is_empty());
        assert!(!waiting.text().contains("pending_names="));
    }

    // Spec: docs/specs/cli.md#integration-batch-membership
    #[test]
    fn a_superseded_member_renders_the_batch_and_drops_cancelled_failures() {
        use super::membership::Link;
        let mut member = row(7, "member-head", None);
        member.checks_failed = Some(2);
        member.failing_checks = vec!["ci-status".into(), "real-failure".into()];
        member.failed_check_details = vec![
            super::FailedCheck {
                name: "ci-status".into(),
                url: Some("https://ci/one".into()),
                bucket: "cancel".into(),
            },
            super::FailedCheck {
                name: "real-failure".into(),
                url: Some("https://ci/two".into()),
                bucket: "fail".into(),
            },
        ];
        member.suppress_own_ci();
        member.batch = Some(Link {
            number: Some(9),
            label: "#9".into(),
            head: "batch-head".into(),
            ci: "checks=2/2 fail=0 pending=0".into(),
            head_moved: false,
        });
        let text = member.text();
        assert!(
            text.contains(
                "in batch #9 (head merged); own CI superseded; batch ci=checks=2/2 fail=0 pending=0"
            ),
            "{text}"
        );
        assert!(text.contains("fail=1 names=real-failure"), "{text}");
        assert!(!text.contains("names=ci-status"), "{text}");
        assert_eq!(member.checks_failed, Some(1));
        assert_eq!(member.failing_checks, ["real-failure"]);
        assert_eq!(
            member.display_class(),
            "failing",
            "real failures still fail"
        );
        let json = serde_json::to_value(&member).unwrap();
        assert_eq!(json["batch"]["number"], 9);
        assert_eq!(json["batch"]["head"], "batch-head");
        let summary = super::summarise(&[member]);
        assert_eq!(summary.lines[0], "1 PRs: 1 failing");
        assert_eq!(summary.lines.len(), 2, "the real failure is an action row");
        assert!(summary.lines[1].contains("fail=1 names=real-failure"));
    }

    // Spec: docs/specs/cli.md#integration-batch-membership
    #[test]
    fn uncertain_ancestry_is_reported_without_claiming_a_batch_or_suppressing_ci() {
        let mut member = row(7, "member-head", None);
        member.checks_failed = Some(1);
        member.failing_checks = vec!["real-failure".into()];
        member.failed_check_details = vec![super::FailedCheck {
            name: "real-failure".into(),
            url: None,
            bucket: "fail".into(),
        }];
        member.batch_uncertain =
            Some(" batch=#9? shared history only; membership uncertain; own CI retained".into());
        let text = member.text();
        assert!(
            text.contains("batch=#9? shared history only; membership uncertain; own CI retained"),
            "{text}"
        );
        assert!(text.contains("fail=1 names=real-failure"), "{text}");
        assert!(member.batch.is_none(), "uncertainty is not a batch claim");
        assert_eq!(member.display_class(), "failing");
        let json = serde_json::to_value(&member).unwrap();
        assert!(
            json["batch_uncertain"]
                .as_str()
                .is_some_and(|note| note.contains("#9"))
        );
        assert!(json["batch"].is_null());
    }

    // Spec: docs/specs/cli.md#integration-batch-membership
    #[test]
    fn a_batch_row_renders_its_members_and_per_file_attribution() {
        use super::membership::{BatchDetail, FileOwner, MemberRow};
        let mut batch = row(9, "batch-head", None);
        batch.batch_detail = Some(BatchDetail {
            members: vec![
                MemberRow {
                    number: 7,
                    head_moved: false,
                },
                MemberRow {
                    number: 8,
                    head_moved: true,
                },
            ],
            files: vec![
                FileOwner {
                    path: "src/a.rs".into(),
                    member: 7,
                },
                FileOwner {
                    path: "src/b.rs".into(),
                    member: 8,
                },
            ],
        });
        let text = batch.text();
        assert!(text.contains("batch_members=7:merged,8:moved"), "{text}");
        assert!(text.contains("batch_files=src/a.rs:7,src/b.rs:8"), "{text}");
        let json = serde_json::to_value(&batch).unwrap();
        assert_eq!(json["batch_detail"]["files"][1]["member"], 8);
    }

    #[test]
    fn complete_wait_keeps_failures_pending_until_all_checks_finish() {
        let mut line = row(1668, "head", None);
        line.checks_failed = Some(1);
        line.checks_pending = Some(1);
        assert_eq!(line.wait_verdict(false), Some(false));
        assert_eq!(line.wait_verdict(true), None);
        line.checks_pending = Some(0);
        line.active_workflows = Some(true);
        assert_eq!(line.wait_verdict(true), None);
        line.checks_failed = Some(0);
        assert_eq!(line.wait_verdict(true), None);
        line.checks_failed = Some(1);
        line.active_workflows = Some(false);
        assert_eq!(line.wait_verdict(true), Some(false));
        line.state = "MERGED".into();
        line.active_workflows = Some(true);
        assert_eq!(line.wait_verdict(false), Some(true));
        assert_eq!(line.wait_verdict(true), None);
        line.active_workflows = Some(false);
        assert_eq!(line.wait_verdict(true), Some(true));
        line.await_head("new");
        assert_eq!(line.wait_verdict(true), None);
    }

    #[test]
    fn a_settling_head_without_actions_runs_awaits_them() {
        let mut passed = row(1668, "head", None);
        assert!(passed.awaits_actions(false));
        assert!(passed.awaits_actions(true));
        passed.hold_for_actions();
        assert_eq!(passed.verdict(), None);
        assert_eq!(passed.wait_verdict(true), None);
        assert_eq!(passed.class(), "pending");
        assert!(passed.text().contains("awaiting Actions runs"));
        assert!(
            !passed.awaits_actions(false),
            "a held reading is not held again"
        );

        // Observed runs end the wait for them, whether active or finished.
        for active in [true, false] {
            let mut seen = row(1668, "head", None);
            seen.active_workflows = Some(active);
            assert!(!seen.awaits_actions(false));
            assert!(!seen.awaits_actions(true));
        }

        // A failure settles a default wait at once, but a complete wait promises
        // to see every check, so it waits for the runs as well.
        let mut failed = row(1668, "head", None);
        failed.checks_failed = Some(1);
        assert!(!failed.awaits_actions(false));
        assert!(failed.awaits_actions(true));

        // Merged, closed, unavailable and pinned-elsewhere readings are not held.
        let mut merged = row(1668, "head", None);
        merged.state = "MERGED".into();
        assert!(!merged.awaits_actions(false));
        assert!(!merged.awaits_actions(true));
        assert!(!row(1668, "head", Some("HTTP 502")).awaits_actions(false));
        let mut pinned = row(1668, "head", None);
        pinned.await_head("new");
        assert!(!pinned.awaits_actions(false));
    }

    #[test]
    fn failure_lines_bound_and_sanitise_check_names_and_links() {
        let mut line = row(1668, "head", None);
        line.checks_failed = Some(4);
        let long_name = "n".repeat(10_000);
        line.failed_check_details = vec![
            super::FailedCheck {
                name: "bad\nname\u{1b}[31m".into(),
                url: Some("https://ci/ok".into()),
                bucket: "fail".into(),
            },
            super::FailedCheck {
                name: long_name,
                url: Some(format!("https://ci/{}", "u".repeat(10_000))),
                bucket: "fail".into(),
            },
            super::FailedCheck {
                name: "spaced".into(),
                url: Some("https://ci/a b".into()),
                bucket: "cancel".into(),
            },
            super::FailedCheck {
                name: "scheme".into(),
                url: Some("javascript:alert(1)".into()),
                bucket: "fail".into(),
            },
        ];
        let lines = line.failure_lines();
        assert_eq!(lines.len(), 4);
        for text in &lines {
            assert!(!text.chars().any(char::is_control), "{text:?}");
            assert!(text.len() < 200, "{} bytes", text.len());
        }
        assert_eq!(
            lines[0],
            "#1668 failed: bad\\nname\\u{1b}[31m https://ci/ok"
        );
        assert!(lines[1].ends_with("..."), "{}", lines[1]);
        assert!(
            !lines[1].contains("https://"),
            "an overlong link is dropped"
        );
        assert_eq!(lines[2], "#1668 failed: spaced");
        assert_eq!(lines[3], "#1668 failed: scheme");
    }

    #[test]
    fn awaiting_an_unreported_head_never_verdicts_the_stale_one() {
        let mut line = row(1668, "stale", None);
        line.checks_failed = Some(1);
        assert_eq!(line.verdict(), Some(false));
        line.await_head("new");
        assert_eq!(line.verdict(), None);
        assert_eq!(line.class(), "pending");
        assert!(line.text().contains("waiting for head new"));
        assert!(line.text().contains("GitHub reports stale"));
    }

    #[test]
    fn the_summary_puts_action_rows_first() {
        let mut failing = row(3, "c", None);
        failing.checks_failed = Some(1);
        let unavailable = row(2, "b", Some("HTTP 502"));
        let action_rows = vec![failing.text(), unavailable.text()];
        let lines = vec![row(1, "a", None), failing, unavailable];
        let summary = super::summarise(&lines);
        assert_eq!(&summary.lines[1..], action_rows);
        assert_eq!(summary.omitted, 0);
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

    #[test]
    fn display_fields_degrade_gracefully_while_verdict_fields_refuse_malformed_values() {
        let display_degraded = r#"{
            "number": 1,
            "state": "OPEN",
            "head": "abc",
            "draft": false,
            "checks_passed": "invalid",
            "checks_total": 5,
            "checks_failed": 0,
            "checks_pending": 0,
            "failing_checks": null,
            "merge": 123,
            "review": "approved"
        }"#;
        let line: super::Line =
            serde_json::from_str(display_degraded).expect("display fields degrade");
        assert_eq!(line.number, 1);
        assert_eq!(line.checks_passed, None);
        assert_eq!(line.failing_checks, Vec::<String>::new());
        assert_eq!(line.verdict(), Some(true));

        for malformed_verdict in [
            r#"{"number":1,"state":"OPEN","head":"a","checks_failed":"bogus"}"#,
            r#"{"number":1,"state":"OPEN","head":"a","checks_pending":"bogus"}"#,
            r#"{"number":1,"state":"OPEN","head":"a","error":123}"#,
            r#"{"number":1,"state":"OPEN","head":"a","review":123}"#,
        ] {
            assert!(
                serde_json::from_str::<super::Line>(malformed_verdict).is_err(),
                "malformed verdict field must fail deserialization: {malformed_verdict}"
            );
        }
    }
}
