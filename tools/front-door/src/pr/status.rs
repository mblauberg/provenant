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
/// A pinned pull request moved after its admitted head was observed.
pub const PR_HEAD_MOVED: &str = "pr_head_moved";
/// A pull request wait refused to proceed without a head at admission.
pub const HEAD_UNPINNED: &str = "head_unpinned";

pub(crate) const NO_CHECKS_NEXT: &str =
    "check the workflow triggers or push an empty commit to start CI";

#[derive(Deserialize)]
pub(super) struct View {
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
    /// Shared CI decision for this head, omitted from status JSON.
    #[serde(skip)]
    completion: Option<CiCompletion>,
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
    /// The shared per-job digest for this head, when checks failed. The same
    /// report `ci failures` builds, embedded so status and wait never point at
    /// a digest they do not show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failures: Option<crate::ci::Report>,
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
    /// The base branch's required checks, when a waiter consulted its policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required_checks: Vec<String>,
    /// Required checks absent from this head or not yet passing on it. The
    /// observed set alone cannot settle a head while any of these remain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required_pending: Vec<String>,
    /// The required policy could not be read, so no verdict may be taken from
    /// whatever checks happen to exist on the head.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    required_unreadable: bool,
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
        self.completion = None;
        self.checks_passed = None;
        self.checks_total = None;
        self.checks_failed = None;
        self.checks_pending = None;
        self.failing_checks.clear();
        self.pending_checks.clear();
        self.failed_check_details.clear();
        self.required_checks.clear();
        self.required_pending.clear();
        self.required_unreadable = true;
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
        } else if self.completion == Some(CiCompletion::Failed)
            || (self.completion.is_none() && self.checks_failed.unwrap_or(0) > 0)
        {
            "failing"
        } else if matches!(
            self.review.as_str(),
            "REVIEW_REQUIRED" | "CHANGES_REQUESTED"
        ) {
            "review_required"
        } else if self.required_unreadable
            || !self.required_pending.is_empty()
            || self.checks_pending.unwrap_or(0) > 0
            || matches!(
                self.completion,
                Some(CiCompletion::Pending | CiCompletion::Unavailable)
            )
            || (self.completion.is_none() && self.checks_total.is_none())
        {
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
        self.completion = None;
    }
    /// What a waiter takes from this pull request: `Some(true)` once it
    /// merged or its checks passed, `Some(false)` once they failed or it
    /// closed unmerged, and `None` while checks run or the read failed.
    pub(crate) fn verdict(&self) -> Option<bool> {
        if let Some(completion) = self.completion {
            return match completion {
                CiCompletion::Green => Some(true),
                CiCompletion::Failed => Some(false),
                CiCompletion::Pending | CiCompletion::Unavailable => None,
            };
        }
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
        if self.state != "OPEN" {
            self.verdict()
        } else if complete
            && (self.checks_pending.unwrap_or(0) > 0 || self.active_workflows == Some(true))
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
        if self.required_unreadable {
            line.push_str(" required_checks=unavailable");
        } else if !self.required_pending.is_empty() {
            line.push_str(&format!(
                " required_pending={}",
                self.required_pending.join(",")
            ));
        }
        line.push_str(&self.cause_suffix());
        line
    }

    /// Compact digest lines for this head's failed jobs, empty when nothing
    /// failed or the digest could not be read.
    pub(crate) fn digest_rows(&self) -> Vec<String> {
        self.failures
            .as_ref()
            .map(crate::ci::digest_rows)
            .unwrap_or_default()
    }

    /// Full digest text for this head, used by `--full` output.
    pub(crate) fn digest_human(&self) -> String {
        self.failures
            .as_ref()
            .map(crate::ci::digest_human)
            .unwrap_or_default()
    }

    /// Read the shared per-job digest for a head whose checks failed. An
    /// unreadable digest stays absent; the status line remains complete.
    pub(crate) fn attach_failure_digest(
        &mut self,
        state: &Path,
        project: &Path,
        request: &provenant_config::ports::Request,
    ) {
        if self.failed_check_details.is_empty() || self.failures.is_some() {
            return;
        }
        let checks: Vec<batch::RawCheck> = self
            .failed_check_details
            .iter()
            .map(|check| batch::RawCheck {
                name: check.name.clone(),
                bucket: "fail".to_owned(),
                url: check.url.clone(),
            })
            .collect();
        let Some(repository) = checks
            .iter()
            .find_map(|check| check.url.as_deref().and_then(crate::ci::repository_of))
            .or_else(|| batch::repository(state, project).ok())
        else {
            return;
        };
        self.failures = crate::ci::digest(
            state,
            project,
            &repository,
            self.number,
            &self.head,
            &checks,
            request,
        )
        .ok();
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
    delivery_queue: bool,
    failed_checks_max_bytes: usize,
    max_failed_checks: usize,
) -> provenant_fabric_runtime::graph::CiObservation {
    ci::observe(
        state,
        project,
        target,
        names,
        delivery_queue,
        failed_checks_max_bytes,
        max_failed_checks,
    )
}

/// Observe mergeability before a graph waits on CI for the same published head.
pub(crate) fn observe_conflict(
    state: &Path,
    project: &Path,
    target: &str,
) -> provenant_fabric_runtime::graph::ConflictObservation {
    ci::observe_conflict(state, project, target)
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

/// Report conflicts even if a check result exists: the conflict gate runs
/// before CI and routes the retained writer to merge the base.
fn mark_conflict(
    observation: &mut provenant_fabric_runtime::graph::CiObservation,
    read_mergeable: impl FnOnce() -> Option<String>,
) {
    use provenant_fabric_runtime::graph::CiState;
    if read_mergeable().as_deref() == Some("CONFLICTING") {
        observation.state = CiState::Conflict;
        observation.failed_checks.clear();
        observation.summary = format!(
            "pull request conflicts with its base; next: {}",
            provenant_fabric_runtime::graph::CONFLICT_GUIDANCE
        );
    }
}

#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes this module without libtest.
fn observe_ci_with(
    names: &[String],
    delivery_queue: bool,
    resolve: impl FnOnce() -> Option<Vec<String>>,
    read_checks: impl FnOnce() -> Option<Vec<Check>>,
    read_mergeable: impl FnOnce() -> Option<String>,
) -> provenant_fabric_runtime::graph::CiObservation {
    let resolved = names.is_empty().then(resolve);
    let policy_unavailable =
        delivery_queue && names.is_empty() && resolved.as_ref().is_some_and(Option::is_none);
    let required = if names.is_empty() {
        resolved
            .flatten()
            .filter(|names| !names.is_empty())
            .map(|names| ("branch-required", names))
    } else {
        Some(("configured", names.to_vec()))
    };
    let mut observation = summarise_ci(
        read_checks(),
        required
            .as_ref()
            .map(|(policy, names)| (*policy, names.as_slice())),
    );
    if policy_unavailable {
        observation.state = provenant_fabric_runtime::graph::CiState::Pending;
        observation.summary =
            "policy=required-unavailable; waiting for the branch check policy".into();
        observation.failed_checks.clear();
    }
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

/// How a reading established the base branch's required checks.
#[derive(Debug)]
enum Requirement {
    /// Display-only callers do not gate on a policy they did not read.
    NotConsulted,
    /// The policy could not be read: observed checks cannot settle the head.
    Unreadable,
    /// The policy was read; an empty set is a known, readable answer.
    Known(Vec<Required>),
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

/// Decide whether CI is complete and green for exactly `head`. This is the one
/// completion definition for `wait --until complete`, `pr status` and landing.
/// A terminal pull request settles before any pending check: a merged or closed
/// PR is no longer waiting for CI. Review-gate contexts come from the project's
/// `ci_observation` port; they are not CI and cannot hold a wait open.
pub(super) fn ci_completion(
    state: &str,
    head: &str,
    checks: &[CiCheck],
    required: &[Required],
    review_gate_contexts: &[String],
) -> CiCompletion {
    let required: Vec<&Required> = required
        .iter()
        .filter(|need| !is_review_gate_context(&need.name, review_gate_contexts))
        .collect();
    let checks: Vec<&CiCheck> = checks
        .iter()
        .filter(|check| !is_review_gate_context(&check.name, review_gate_contexts))
        .collect();
    if state == "MERGED" {
        return CiCompletion::Green;
    }
    if state != "OPEN" {
        return CiCompletion::Failed;
    }
    if checks.iter().any(|check| check.head != head) {
        return CiCompletion::Unavailable;
    }
    let selected: Vec<&CiCheck> = checks
        .iter()
        .copied()
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

fn is_review_gate_context(name: &str, review_gate_contexts: &[String]) -> bool {
    review_gate_contexts.iter().any(|gate| gate == name)
}

pub(crate) fn gate_contexts(project: &Path, state: &Path) -> Vec<String> {
    provenant_config::ports::PortTable::read_for(Some(project), state)
        .map(|ports| ports.ci_observation.review_gate_contexts)
        .unwrap_or_default()
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
            quota_reset_at: None,
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
        quota_reset_at: None,
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

/// Revalidate PR metadata and CI through an open observer: the metadata read,
/// every check page and the checks read share `request`'s one deadline and
/// cancellation. Returns the rendered line with the checks it was bound to, so
/// a poller can project them without a second read.
pub(crate) fn observe_line<F: provenant_config::Forge, C: super::observer::Clock>(
    observer: &mut super::observer::Observer<F, C>,
    project: &Path,
    state: &Path,
    number: u64,
    request: &provenant_config::ports::Request,
    max_age: std::time::Duration,
    pinned_head: Option<provenant_core::GitOid>,
) -> Result<(Line, Vec<super::observer::Check>)> {
    let repository = observer.repository().to_owned();
    let repository = repository.as_str();
    let graphql_entry = std::cell::RefCell::new(None);
    let graphql_head = || match batch::single_for_repository_with_request(
        state, project, repository, number, request,
    ) {
        batch::Batch::Entries(entries) => entries.first().map(|entry| {
            *graphql_entry.borrow_mut() = Some(entry.clone());
            entry.head.clone()
        }),
        _ => None,
    };
    let mut reading = pinned_head
        .map(|head| observer.observe_head(number, head, request, max_age, &graphql_head));
    let (view, base_ref, first_pull) =
        match observer.resource(&format!("pulls/{number}"), max_age, request) {
            Ok(value) => {
                let first_pull = Some(value.clone());
                let pull: RestWaitPull = serde_json::from_value(value)?;
                let base_ref = pull.base.as_ref().map(|base| base.branch.clone());
                (
                    View {
                        state: if pull.merged_at.is_some() {
                            "MERGED".into()
                        } else {
                            pull.state.to_ascii_uppercase()
                        },
                        head: pull.head.sha,
                        merge: pull
                            .mergeable_state
                            .map_or_else(|| "UNKNOWN".into(), |state| state.to_ascii_uppercase()),
                        review: Some("unknown".into()),
                        draft: pull.draft,
                    },
                    base_ref,
                    first_pull,
                )
            }
            Err(error) => {
                let entry = graphql_entry
                    .borrow_mut()
                    .take()
                    .or_else(|| {
                        match batch::single_for_repository_with_request(
                            state, project, repository, number, request,
                        ) {
                            batch::Batch::Entries(entries) => entries.into_iter().next(),
                            _ => None,
                        }
                    })
                    .ok_or(error)?;
                let base_ref = Some(entry.base_ref_name.clone());
                (view_from_batch_entry(entry), base_ref, None)
            }
        };
    let expected = match pinned_head {
        Some(expected) => expected,
        None => provenant_core::GitOid::parse(&view.head)?,
    };
    if reading.is_none() {
        reading = Some(observer.observe_head(number, expected, request, max_age, &graphql_head));
    }
    let Some(reading) = reading else {
        return Err("CI observer did not return a head reading".into());
    };
    let no_checks_for_current_head = reading.pr_head == Some(reading.expected_head)
        && reading.retry_at.is_none()
        && reading.observed.is_empty();
    let mut merge_state = reading.mergeable;
    if no_checks_for_current_head
        && merge_state == super::observer::MergeState::Unknown
        && let Some(first) = first_pull
    {
        // Only a diagnosis that mergeability can change rereads it: a head
        // with checks needs no cause. A still-UNKNOWN answer stays UNKNOWN.
        if let Ok((reread, _)) = observer.pull_merge_state(number, first, request) {
            merge_state = reread;
        }
    }
    let checks = reading.observed.clone();
    let mut line = line_from_head_reading(number, view, reading, &gate_contexts(project, state));
    // A pinned wait watches its own head: while GitHub reports another, the
    // reading names the pinned head it is waiting for, not the one it saw.
    if let (Some(pinned), Some(awaiting)) = (pinned_head, line.awaiting.as_mut()) {
        *awaiting = pinned.to_string();
    }
    if no_checks_for_current_head {
        let cause = merge_state.no_checks_cause(base_ref.as_deref());
        line.cause = Some(cause.code.to_owned());
        line.next = Some(cause.next);
    }
    Ok((line, checks))
}

/// Revalidate PR metadata, its CI and its Actions runs through the shared
/// observer during a wait loop. Unavailable Actions runs warn once per wait.
pub(crate) fn wait_line_observed(
    project: &Path,
    state: &Path,
    repository: &str,
    number: u64,
    request: &provenant_config::ports::Request,
    max_age: std::time::Duration,
    pinned_head: Option<provenant_core::GitOid>,
) -> Result<Line> {
    let mut observer = super::observer::Observer::open(state, project, repository, request)?;
    let (mut line, _) = observe_line(
        &mut observer,
        project,
        state,
        number,
        request,
        max_age,
        pinned_head,
    )?;
    if line.awaiting.is_none() {
        match observer.active_workflows(&line.head, max_age, request) {
            Ok(Some(active)) => line.active_workflows = Some(active),
            Ok(None) => provenant_records::warning::warn(
                provenant_records::warning::Warning::new(
                    provenant_records::warning::ACTIONS_RUNS_UNAVAILABLE,
                    "Actions runs unavailable for this head (no runs found); using observed checks only",
                    provenant_records::warning::detected_source(
                        provenant_records::warning::ACTIONS_RUNS_UNAVAILABLE,
                    ),
                )
                .subject(line.head.clone()),
            ),
            Err(error) => provenant_records::warning::warn(
                provenant_records::warning::Warning::new(
                    provenant_records::warning::ACTIONS_RUNS_UNAVAILABLE,
                    format!("Actions runs unavailable for this head ({error}); using observed checks only"),
                    provenant_records::warning::detected_source(
                        provenant_records::warning::ACTIONS_RUNS_UNAVAILABLE,
                    ),
                )
                .subject(line.head.clone()),
            ),
        }
    }
    Ok(line)
}

fn view_from_batch_entry(entry: batch::Entry) -> View {
    View {
        state: entry.state,
        head: entry.head,
        merge: entry.merge,
        review: entry.review,
        draft: entry.draft,
    }
}

/// One row from the pull request view and its observed checks, for display
/// paths that did not read the branch's required policy.
fn status_line(
    number: u64,
    view: View,
    checks: Option<&[super::observer::Check]>,
    review_gate_contexts: &[String],
) -> Line {
    status_line_with(
        number,
        view,
        checks,
        Requirement::NotConsulted,
        review_gate_contexts,
    )
}

/// One row from the pull request view, its observed checks and the branch's
/// required set. A required check that is absent or not passing keeps the
/// reading pending however small the observed set is.
fn status_line_with(
    number: u64,
    view: View,
    checks: Option<&[super::observer::Check]>,
    requirement: Requirement,
    review_gate_contexts: &[String],
) -> Line {
    let observed: Vec<&super::observer::Check> = checks
        .unwrap_or(&[])
        .iter()
        .filter(|check| !is_review_gate_context(&check.name, review_gate_contexts))
        .collect();
    let readable = checks.filter(|_| !observed.is_empty());
    let completion = match view.state.as_str() {
        "MERGED" | "CLOSED" => Some(ci_completion(
            &view.state,
            &view.head,
            &[],
            &[],
            review_gate_contexts,
        )),
        _ => match (&requirement, checks) {
            (Requirement::Unreadable, _) | (_, None) => Some(CiCompletion::Unavailable),
            (requirement, Some(checks)) => {
                let required = match requirement {
                    Requirement::Known(required) => required.as_slice(),
                    Requirement::NotConsulted | Requirement::Unreadable => &[],
                };
                let normalized: Vec<CiCheck> = checks
                    .iter()
                    .map(|check| CiCheck {
                        head: check.head.clone(),
                        name: check.name.clone(),
                        bucket: check.bucket.clone(),
                        app: check.app,
                    })
                    .collect();
                Some(ci_completion(
                    &view.state,
                    &view.head,
                    &normalized,
                    required,
                    review_gate_contexts,
                ))
            }
        },
    };
    let count = |buckets: &[&str]| {
        readable.map(|_| {
            observed
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
    let (required_checks, required_pending, required_unreadable) = match &requirement {
        Requirement::NotConsulted => (Vec::new(), Vec::new(), false),
        Requirement::Unreadable => (Vec::new(), Vec::new(), true),
        Requirement::Known(needs) => {
            let names = needs
                .iter()
                .filter(|need| !is_review_gate_context(&need.name, review_gate_contexts))
                .map(|need| need.name.clone())
                .collect();
            let pending = needs
                .iter()
                .filter(|need| !is_review_gate_context(&need.name, review_gate_contexts))
                .filter(|need| {
                    !checks
                        .unwrap_or(&[])
                        .iter()
                        .any(|check| need.matches(&check.name, check.app) && check.bucket == "pass")
                })
                .map(|need| need.name.clone())
                .collect();
            (names, pending, false)
        }
    };
    Line {
        number,
        completion,
        checks_passed: count(&["pass", "skipping"]),
        checks_total: readable.map(|_| observed.len()),
        checks_failed: count(&["fail", "cancel"]),
        checks_pending: count(&["pending"]),
        active_workflows: None,
        failing_checks: observed
            .iter()
            .filter(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
            .map(|check| display_check_name(&check.name))
            .collect(),
        pending_checks: observed
            .iter()
            .filter(|check| check.bucket == "pending")
            .map(|check| display_check_name(&check.name))
            .collect(),
        failed_check_details: observed
            .iter()
            .filter(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
            .map(|check| FailedCheck {
                name: display_check_name(&check.name),
                url: check.url.as_deref().and_then(display_check_url),
                bucket: check.bucket.clone(),
            })
            .collect(),
        failures: None,
        merge: word(&view.merge),
        review: word(view.review.as_deref().unwrap_or_default()),
        state: view.state,
        head: view.head,
        draft: view.draft,
        cause: None,
        next: None,
        awaiting: None,
        actions_grace: false,
        required_checks,
        required_pending,
        required_unreadable,
        error: None,
        batch: None,
        batch_detail: None,
        batch_uncertain: None,
    }
}

/// Render a batch or waiter result only after the shared observer has bound
/// the PR's required checks to the requested head.
pub(super) fn line_from_head_reading(
    number: u64,
    mut view: View,
    reading: super::observer::HeadReading,
    review_gate_contexts: &[String],
) -> Line {
    // The observation settles only conflict or no conflict; the caller's
    // metadata can say more ("BLOCKED", "BEHIND"). A conflict always shows. No
    // conflict replaces a contrary or unreported state and keeps a richer one.
    match reading.mergeable {
        super::observer::MergeState::Conflicting => view.merge = "DIRTY".into(),
        super::observer::MergeState::Mergeable
            if matches!(view.merge.as_str(), "" | "DIRTY" | "UNKNOWN") =>
        {
            view.merge = "CLEAN".into();
        }
        super::observer::MergeState::Mergeable | super::observer::MergeState::Unknown => {}
    }
    let Some(head) = reading.pr_head else {
        return status_line_with(
            number,
            view,
            None,
            Requirement::Unreadable,
            review_gate_contexts,
        );
    };
    if head != reading.expected_head {
        let mut line = status_line_with(
            number,
            view,
            None,
            Requirement::Unreadable,
            review_gate_contexts,
        );
        line.await_head(&head.to_string());
        return line;
    }
    view.head = head.to_string();
    let checks = reading.observed;
    let required = reading
        .required_policy
        .iter()
        .map(|check| Required {
            name: check.name.clone(),
            app: check.app,
        })
        .collect();
    let mut line = status_line_with(
        number,
        view,
        Some(&checks),
        Requirement::Known(required),
        review_gate_contexts,
    );
    if !reading.complete && line.state == "OPEN" {
        line.completion = Some(CiCompletion::Pending);
        // A known policy reports its own pending names; only a reading that
        // could not establish the policy is "unavailable".
        line.required_unreadable = reading.required_policy.is_empty();
    }
    line
}

#[cfg(test)]
pub(super) fn line_for(
    number: u64,
    state: &str,
    head: &str,
    checks: Option<&[super::observer::Check]>,
    review_gate_contexts: &[String],
) -> Line {
    status_line(
        number,
        View {
            state: state.to_owned(),
            head: head.to_owned(),
            merge: "CLEAN".into(),
            review: None,
            draft: false,
        },
        checks,
        review_gate_contexts,
    )
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
    };
    status_line(1, view, Some(checks), &[]).class()
}

fn snapshot(project: &Path, state: &Path, numbers: &[u64]) -> Vec<Line> {
    // One GraphQL query answers every pull request when it can; the per-PR
    // REST reads remain the fallback when the batch transport cannot.
    let review_gate_contexts = gate_contexts(project, state);
    let batch = batch::snapshot(state, project, numbers);
    let batch_limited = matches!(batch, batch::Batch::RateLimited(_));
    let entries = match batch {
        batch::Batch::Entries(entries) => entries,
        _ => Vec::new(),
    };
    let mut lines: Vec<(_, Line)> = entries
        .iter()
        .map(|entry| {
            (
                entry.number,
                batch::line_from_entry_observed(state, project, entry, &review_gate_contexts),
            )
        })
        .collect();
    let answered: std::collections::BTreeSet<_> = lines.iter().map(|(number, _)| *number).collect();
    for &number in numbers {
        if answered.contains(&number) {
            continue;
        }
        lines.push((
            number,
            rest_line(
                project,
                state,
                number,
                !batch_limited,
                &review_gate_contexts,
            ),
        ));
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

/// A status row when the batch metadata read cannot answer. CI still comes
/// from the shared head observer, which uses GraphQL only as its head fallback.
fn rest_line(
    project: &Path,
    state: &Path,
    number: u64,
    try_graphql: bool,
    review_gate_contexts: &[String],
) -> Line {
    let request = provenant_config::ports::Request::within(std::time::Duration::from_secs(60));
    let repository = match batch::repository(state, project) {
        Ok(repository) => repository,
        Err(error) => {
            return unavailable_line(number, error);
        }
    };
    match wait_line_observed(
        project,
        state,
        &repository,
        number,
        &request,
        std::time::Duration::ZERO,
        None,
    ) {
        Ok(line) => line,
        Err(error) => {
            if try_graphql
                && looks_rate_limited(error.as_ref())
                && let batch::Batch::Entries(entries) = batch::single(state, project, number)
                && let Some(entry) = entries.first()
                && !entry.truncated
            {
                return batch::line_from_entry_observed(
                    state,
                    project,
                    entry,
                    review_gate_contexts,
                );
            }
            unavailable_line(number, error.to_string())
        }
    }
}

fn unavailable_line(number: u64, error: impl std::fmt::Display) -> Line {
    Line {
        number,
        state: "UNAVAILABLE".into(),
        head: "unknown".into(),
        completion: None,
        draft: false,
        checks_passed: None,
        checks_total: None,
        checks_failed: None,
        checks_pending: None,
        active_workflows: None,
        failing_checks: Vec::new(),
        pending_checks: Vec::new(),
        failed_check_details: Vec::new(),
        failures: None,
        merge: "unknown".into(),
        review: "unknown".into(),
        cause: None,
        next: None,
        awaiting: None,
        actions_grace: false,
        required_checks: Vec::new(),
        required_pending: Vec::new(),
        required_unreadable: false,
        error: Some(provenant_cli::bounded_text(&error.to_string(), 256)),
        batch: None,
        batch_detail: None,
        batch_uncertain: None,
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
        let request = provenant_config::ports::Request::within(std::time::Duration::from_secs(60));
        let mut lines = snapshot(&project, state.as_path(), &numbers);
        for line in &mut lines {
            line.attach_failure_digest(state.as_path(), &project, &request);
        }
        Ok(lines)
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
            let text: String = lines
                .iter()
                .map(|line| {
                    let mut text = line.text() + "\n";
                    text.push_str(&line.digest_human());
                    text
                })
                .collect();
            let (summary, next) = summarise(&lines);
            let outcome = if !lines.is_empty() && lines.iter().all(|line| line.error.is_some()) {
                Outcome::Unavailable
            } else {
                Outcome::Ok
            };
            emit_summarised(
                &CommandResult::new(LEAF, outcome, lines).with_next(vec![next]),
                globals,
                &text,
                true,
                Some(&summary),
            )
        }
        Err(error) => fail(
            LEAF,
            Outcome::Error,
            "PR",
            &error.to_string(),
            "check the reported forge read error and credentials, then rerun `provenant pr status`",
            globals,
        ),
    }
}

/// Counts by state, action rows first (failing, review, pending,
/// unavailable), then one typed next command.
fn summarise(
    lines: &[Line],
) -> (
    provenant_cli::Summary,
    provenant_records::command_result::NextStep,
) {
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
    let rows: Vec<String> = action
        .iter()
        .flat_map(|line| {
            let mut rows = vec![line.text()];
            rows.extend(line.digest_rows());
            rows
        })
        .collect();
    let quiet = lines.len() - action.len();
    let command = match action.first() {
        Some(line) if line.class() == "failing" => {
            format!("provenant ci failures pr {}", line.number)
        }
        Some(line) if line.class() == "pending" => format!("provenant wait --pr {}", line.number),
        Some(line) => format!("provenant pr status {} --full", line.number),
        None => lines
            .iter()
            .find(|line| line.display_class() == "ready")
            .map(|line| format!("provenant land {} --wait", line.number))
            .unwrap_or_else(|| "provenant status".to_owned()),
    };
    let mut headline = format!(
        "{} PRs: {}",
        lines.len(),
        provenant_cli::Summary::tally(&counts)
    );
    if quiet > 0 && !action.is_empty() {
        headline.push_str(&format!(" ({quiet} need nothing)"));
    }
    let why = match command.as_str() {
        command if command.starts_with("provenant ci failures") => "inspect the failing checks",
        command if command.starts_with("provenant wait") => "wait for the pending PR",
        command if command.starts_with("provenant land") => "land the ready PR",
        _ => "review the remaining PRs",
    };
    (
        provenant_cli::Summary::new(headline, rows),
        provenant_records::command_result::NextStep::from_command(&command, why),
    )
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
