//! `land`: the existing CI watch and head-pinned merge, then local
//! follow-up. No step forces: each one refuses and says why.
use super::git::{Git, plain_ref};
use super::{Result, successful};
use crate::{roots, worktrees};
use provenant_confinement::worktrees::{ObservedProcess, WorkerObserver};
use provenant_core::GitOid;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct MergeCommit {
    oid: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Done,
    /// Nothing to do, or a precondition made the step unnecessary.
    Skipped,
    /// Left as it was because proceeding would be unsafe.
    Refused,
}
#[derive(Debug, Serialize)]
struct Step {
    step: &'static str,
    status: Status,
    detail: String,
}
impl Step {
    fn new(step: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            step,
            status,
            detail: detail.into(),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct Pull {
    pub(super) state: String,
    #[serde(rename = "baseRefName")]
    base: String,
    #[serde(rename = "headRefName")]
    pub(super) branch: String,
    /// The head branch lives in another repository, such as a fork.
    #[serde(rename = "isCrossRepository")]
    pub(super) cross_repository: bool,
    #[serde(default)]
    title: String,
    #[serde(rename = "mergeCommit", default)]
    merge_commit: Option<MergeCommit>,
    #[serde(rename = "mergedAt", default)]
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
}
fn from_rest(rest: super::RestPull) -> Pull {
    let cross_repository = match (&rest.head.repo, &rest.base.repo) {
        (Some(head), Some(base)) => head.full_name != base.full_name,
        _ => true,
    };
    Pull {
        state: if rest.merged_at.is_some() {
            "MERGED".into()
        } else {
            rest.state.to_ascii_uppercase()
        },
        base: rest.base.branch,
        branch: rest.head.branch,
        cross_repository,
        title: rest.title,
        merge_commit: rest.merge_commit_sha.map(|oid| MergeCommit { oid }),
        merged_at: rest.merged_at,
        closed_at: rest.closed_at,
    }
}
pub(super) fn pull(project: &Path, number: &str) -> Result<Pull> {
    Ok(from_rest(super::rest_pull(
        &|argv| super::execute(project, argv),
        number,
    )?))
}

// Landing already has the base repository and PR snapshot. Keep contribution
// bookkeeping local and use its bounded reader for stale bindings so this path
// does not make separate GraphQL requests.
struct LandTracker {
    resolved: roots::Resolved,
    project: Option<provenant_core::ProjectId>,
    repository: String,
}
impl LandTracker {
    fn resolve(globals: &provenant_cli::Globals, repository: &str) -> Result<Self> {
        let resolved = roots::resolve(
            globals,
            roots::Workspace::Checkout,
            roots::Product::Optional,
        )?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let runtime =
            provenant_confinement::UtilityRuntime::read_only(git, resolved.roots.state.as_path())?;
        let location = runtime
            .project_location(
                &provenant_os::exec::SystemExecutor::new(1)?,
                &resolved.project,
                provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))?,
                &provenant_core::CancelToken::new(),
            )?
            .location;
        let project = provenant_config::lookup_project(&resolved.roots, &location)?
            .map(|binding| binding.project_id());
        Ok(Self {
            resolved,
            project,
            repository: repository.to_ascii_lowercase(),
        })
    }
    fn pr(&self, number: u64) -> provenant_run::PullRequest {
        provenant_run::PullRequest {
            repository: self.repository.clone(),
            number,
        }
    }
    fn project(&self) -> Result<provenant_core::ProjectId> {
        self.project.ok_or_else(|| {
            "project is unbound; register the project before binding contributions".into()
        })
    }
    fn observed_end(
        &self,
        reader: &super::wait::Reader<'_>,
        pr: &provenant_run::PullRequest,
    ) -> Result<(String, Option<provenant_core::WallTime>)> {
        let endpoint = format!("repos/{}/pulls/{}", pr.repository, pr.number);
        let bytes = successful(reader.read(&["gh", "api", &endpoint])?)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let merged = value.get("merged_at").and_then(serde_json::Value::as_str);
        let closed = value.get("closed_at").and_then(serde_json::Value::as_str);
        let state = if merged.is_some() {
            "MERGED".to_owned()
        } else {
            value
                .get("state")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing pull request state")?
                .to_ascii_uppercase()
        };
        Ok((state, end_time(merged.or(closed))?))
    }
    fn bind(
        &self,
        branch: &str,
        number: u64,
        head: GitOid,
        reader: &super::wait::Reader<'_>,
    ) -> Result<()> {
        let project = self.project()?;
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let db = provenant_run::Contributions::open(self.resolved.roots.state.as_path(), &call)?;
        let pr = self.pr(number);
        for candidate in db.open_bindings(project, branch, &call)? {
            if candidate == pr {
                continue;
            }
            let swept = (|| -> Result<()> {
                let (state, ended_at) = self.observed_end(reader, &candidate)?;
                if matches!(state.as_str(), "CLOSED" | "MERGED") {
                    db.close_at(project, &candidate, ended_at, &call)?;
                }
                Ok(())
            })();
            if let Err(error) = swept {
                if error
                    .downcast_ref::<super::wait::Failure>()
                    .is_some_and(|failure| failure.code == "RATE_LIMITED")
                {
                    return Err(error);
                }
                super::contributions::warn(
                    Some(candidate.number),
                    &format!("stale binding sweep failed: {error}"),
                );
            }
        }
        db.bind(project, branch, &pr, head, &call)?;
        Ok(())
    }
    fn close(&self, number: u64, pull: &Pull) -> Result<()> {
        let project = self.project()?;
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let closed = if provenant_run::Contributions::open_read_only(
            self.resolved.roots.state.as_path(),
            &call,
        )?
        .is_some()
        {
            let ended_at = end_time(pull.merged_at.as_deref().or(pull.closed_at.as_deref()))
                .unwrap_or_else(|error| {
                    super::contributions::warn(
                        Some(number),
                        &format!("PR end time unavailable: {error}"),
                    );
                    None
                });
            provenant_run::Contributions::open(self.resolved.roots.state.as_path(), &call)?
                .close_at(project, &self.pr(number), ended_at, &call)?
        } else {
            false
        };
        if !closed {
            super::contributions::warn(
                Some(number),
                &"no open contribution binding to close; the binding is missing or was already closed",
            );
        }
        Ok(())
    }
}
fn end_time(value: Option<&str>) -> Result<Option<provenant_core::WallTime>> {
    value
        .map(|text| -> Result<provenant_core::WallTime> {
            let stamp: jiff::Timestamp = text.parse()?;
            provenant_core::WallTime::from_micros(stamp.as_microsecond()).map_err(Into::into)
        })
        .transpose()
}

struct Checkout {
    path: PathBuf,
    branch: Option<String>,
}

/// NUL fields avoid Git's C quoting for paths containing tabs or quotes.
fn checkouts(git: &Git, project: &Path) -> Result<Vec<Checkout>> {
    discover_checkouts(git, project).map_err(|error| {
        format!("worktree inventory cannot identify the primary checkout: {error}").into()
    })
}

fn discover_checkouts(git: &Git, project: &Path) -> Result<Vec<Checkout>> {
    let listing = git.worktrees(project)?;
    let listing = String::from_utf8_lossy(&listing);
    let mut checkouts: Vec<Checkout> = Vec::new();
    for field in listing.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            // The primary anchor must resolve. Preserve other registrations
            // for branch-holder checks even if their checkout disappeared.
            let path = match fs::canonicalize(path) {
                Ok(path) => path,
                Err(error) if checkouts.is_empty() => return Err(error.into()),
                Err(_) => PathBuf::from(path),
            };
            checkouts.push(Checkout { path, branch: None });
        } else if let Some(branch) = field.strip_prefix("branch refs/heads/") {
            checkouts
                .last_mut()
                .ok_or("worktree branch without a path")?
                .branch = Some(branch.to_owned());
        }
    }
    let (_, _, common) = git
        .repository_paths(project)
        .map_err(|error| format!("cannot find the common Git directory: {error}"))?;
    let primary = &checkouts.first().ok_or("no primary checkout")?.path;
    let valid = |candidate: &Path| -> Result<bool> {
        match fs::symlink_metadata(candidate.join(".git")) {
            Ok(_) => {
                let (root, private, _) = git.repository_paths(candidate)?;
                Ok(root == candidate && private == common)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    };
    if valid(primary)? {
        return Ok(checkouts);
    }

    // Git may list the separate Git directory instead of the primary path.
    // core.worktree, when configured, is relative to the Git directory.
    if let Some(value) = git.full(project, &["config", "--path", "--get", "core.worktree"])?
        && let Ok(candidate) = fs::canonicalize(common.join(value.trim_end_matches('\n')))
        && valid(&candidate)?
    {
        checkouts[0].path = candidate;
        return Ok(checkouts);
    }

    // Managed lanes can be below the primary checkout. Inspect gitfiles
    // directly, without invoking Git at each ancestor.
    for candidate in project.ancestors() {
        let Ok(gitfile) = fs::read_to_string(candidate.join(".git")) else {
            continue;
        };
        let Some(path) = gitfile
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("gitdir: ")
        else {
            continue;
        };
        if fs::canonicalize(candidate.join(path)).ok().as_deref() == Some(&common)
            && valid(candidate)?
        {
            checkouts[0].path = candidate.to_path_buf();
            return Ok(checkouts);
        }
    }
    Err("Git did not provide a usable primary checkout path".into())
}

/// A landing census exempts only this invocation's callers. Other commands
/// continue to use the ordinary observer, which includes those processes.
struct LandWorkers<'a>(&'a dyn WorkerObserver);
impl WorkerObserver for LandWorkers<'_> {
    fn observe_evidence_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[provenant_os::pid::CensusIgnoreEntry],
        _: bool,
    ) -> provenant_confinement::worktrees::WorkerEvidence {
        self.0.observe_evidence_with_policy(roots, entries, true)
    }
    fn epoch_policy(&self) -> Option<provenant_confinement::worktrees::CensusEpochPolicy> {
        self.0.epoch_policy()
    }
    fn observe_evidence(
        &self,
        roots: &[PathBuf],
        _: bool,
    ) -> provenant_confinement::worktrees::WorkerEvidence {
        self.0.observe_evidence(roots, true)
    }
    fn invalidate(&self) {
        self.0.invalidate()
    }
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[provenant_os::pid::CensusIgnoreEntry],
        _: bool,
    ) -> provenant_confinement::worktrees::WorkerObservations {
        self.0.observe_roots_with_policy(roots, entries, true)
    }
    fn live_process_with_policy(
        &self,
        root: &std::path::Path,
        entries: &[provenant_os::pid::CensusIgnoreEntry],
        _: bool,
    ) -> Option<ObservedProcess> {
        self.0.live_process_with_policy(root, entries, true)
    }
    fn requires_complete_census(&self) -> bool {
        true
    }

    fn observe_roots(
        &self,
        roots: &[PathBuf],
    ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy> {
        self.0.observe_roots_for_land(roots)
    }

    fn observe_roots_with_details(
        &self,
        roots: &[PathBuf],
    ) -> provenant_confinement::worktrees::WorkerObservations {
        self.0.observe_roots_for_land_with_details(roots)
    }

    fn live_process(&self, root: &std::path::Path) -> Option<ObservedProcess> {
        self.0.live_process_for_land(root)
    }

    fn incomplete_census_detail(&self) -> String {
        self.0.incomplete_census_detail()
    }
}

/// How a worktree is using a branch.
enum Use {
    CheckedOut,
    Rebased,
    Bisected,
}

/// The worktree using `branch` and how: checked out, or, on a detached HEAD,
/// paused in a rebase of it or a bisect started from it. `git branch -d`
/// refuses all three, but `git worktree list` reports only the first.
///
/// This reads Git's administrative directories as Git does: the main
/// repository's and each `<common>/worktrees/<id>/`. A checkout that is
/// missing, unmounted or unreadable still holds its branch there. A missing
/// state file means not held; any other read error or an unparseable entry is
/// an error, which refuses the deletion.
fn holder_of(git: &Git, project: &Path, branch: &str) -> Result<Option<(String, Use)>> {
    let (_, _, common) = git
        .repository_paths(project)
        .map_err(|error| format!("cannot find the common Git directory: {error}"))?;
    for dir in administrative_dirs(&common)? {
        if let Some(how) = use_in(&dir, branch)? {
            return Ok(Some((checkout_path(&dir, &common), how)));
        }
    }
    Ok(None)
}

/// The main repository's administrative directory, then each linked
/// worktree's `<common>/worktrees/<id>/`.
fn administrative_dirs(common: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = vec![common.to_path_buf()];
    let linked = common.join("worktrees");
    match fs::read_dir(&linked) {
        Ok(listing) => {
            for entry in listing {
                let entry =
                    entry.map_err(|error| format!("cannot read {}: {error}", linked.display()))?;
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    return Err(format!("unexpected entry {}", entry.path().display()).into());
                }
                dirs.push(entry.path());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read {}: {error}", linked.display()).into()),
    }
    Ok(dirs)
}

/// How the checkout whose administrative directory is `dir` uses `branch`.
fn use_in(dir: &Path, branch: &str) -> Result<Option<Use>> {
    let head = format!("refs/heads/{branch}");
    let read = |file: &str| -> Result<Option<String>> {
        match fs::read_to_string(dir.join(file)) {
            Ok(text) => Ok(Some(text.trim().to_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read {file} in {}: {error}", dir.display()).into()),
        }
    };
    // A present state file must hold what Git writes there; anything else
    // (an empty or truncated file, say) refuses rather than reads as free.
    let names = |file: &str, wanted: &str, valid: fn(&str) -> bool| -> Result<bool> {
        match read(file)? {
            Some(text) if valid(&text) => Ok(text == wanted),
            Some(_) => Err(format!("cannot parse {file} in {}", dir.display()).into()),
            None => Ok(false),
        }
    };
    if let Some(text) = read("HEAD")? {
        match text.strip_prefix("ref: ").map(str::trim) {
            Some(to) if to == head => return Ok(Some(Use::CheckedOut)),
            Some(to) if is_ref(to) => {}
            None if is_object_id(&text) => {}
            _ => return Err(format!("cannot parse HEAD in {}", dir.display()).into()),
        }
    }
    if names("rebase-merge/head-name", &head, is_head_name)?
        || names("rebase-apply/head-name", &head, is_head_name)?
    {
        return Ok(Some(Use::Rebased));
    }
    if names("BISECT_START", branch, is_bisect_start)? {
        return Ok(Some(Use::Bisected));
    }
    Ok(None)
}

/// A full ref name as Git writes it into `HEAD`.
fn is_ref(text: &str) -> bool {
    text.starts_with("refs/") && is_ref_name(text)
}

/// A name that `git check-ref-format --allow-onelevel` accepts.
fn is_ref_name(text: &str) -> bool {
    text != "@"
        && !text.ends_with('.')
        && !text.contains("..")
        && !text.contains("@{")
        && !text
            .bytes()
            .any(|b| b < 0x20 || b == 0x7f || b" ~^:?*[\\".contains(&b))
        && text
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// A full SHA-1 or SHA-256 object id.
fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A rebase's `head-name`: the branch being rebased, or `detached HEAD`.
fn is_head_name(text: &str) -> bool {
    text == "detached HEAD" || is_ref(text)
}

/// `BISECT_START`: the branch name or commit the bisect started from.
fn is_bisect_start(text: &str) -> bool {
    is_object_id(text) || is_ref_name(text)
}

/// The checkout that the administrative directory `dir` belongs to, or the
/// directory itself when that cannot be told.
fn checkout_path(dir: &Path, common: &Path) -> String {
    let checkout = if dir == common {
        common.parent().map(Path::to_path_buf)
    } else {
        fs::read_to_string(dir.join("gitdir"))
            .ok()
            .and_then(|text| Path::new(text.trim()).parent().map(Path::to_path_buf))
    };
    checkout
        .unwrap_or_else(|| dir.to_path_buf())
        .display()
        .to_string()
}

fn follow_up(
    git: &Git,
    project: &Path,
    pull: &Pull,
    merged: GitOid,
    globals: &mut provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> Result<Vec<Step>> {
    let (base, branch) = (pull.base.as_str(), pull.branch.as_str());
    if !plain_ref(base) || !plain_ref(branch) {
        return Err("pull request branch names are not plain Git names".into());
    }
    let remote = git.remote_of(project, base)?;
    let upstream = format!("{remote}/{base}");
    let pinned = merged;
    let merged = merged.to_string();
    let mut steps = Vec::new();
    let skipped = |steps: &mut Vec<Step>, names: &[&'static str], why: &str| {
        for name in names {
            steps.push(Step::new(name, Status::Skipped, why));
        }
    };

    let (fetched, detail) = git.run(project, &["fetch", &remote, base])?;
    if !fetched {
        steps.push(Step::new("fetch", Status::Refused, detail));
        skipped(
            &mut steps,
            &["fast_forward", "worktree", "branch"],
            "fetch did not complete",
        );
        return Ok(steps);
    }
    let (contained, _) = git.run(
        project,
        &["merge-base", "--is-ancestor", &merged, &upstream],
    )?;
    if !contained {
        steps.push(Step::new(
            "fetch",
            Status::Refused,
            format!("{upstream} does not contain the merged head yet"),
        ));
        skipped(
            &mut steps,
            &["fast_forward", "worktree", "branch"],
            "merged head is not on the fetched base",
        );
        return Ok(steps);
    }
    steps.push(Step::new(
        "fetch",
        Status::Done,
        format!("{upstream} updated"),
    ));

    let checkouts = checkouts(git, project)?;
    steps.push(fast_forward(
        git, project, base, &remote, &upstream, &checkouts,
    )?);

    match cleanup_target(git, project, pull, pinned, &upstream)? {
        Cleanup::Skip(status, why) => {
            for name in ["worktree", "branch"] {
                steps.push(Step::new(name, status, why.as_str()));
            }
        }
        Cleanup::Prune(tip) => {
            let cleanup_project = &checkouts.first().ok_or("no primary checkout")?.path;
            // Move this process and output publication to the surviving checkout
            // before removal. A child cannot change its invoking shell's cwd.
            let caller_cwd = std::env::current_dir()?;
            let relocated = checkouts.iter().any(|checkout| {
                checkout.branch.as_deref() == Some(branch) && caller_cwd.starts_with(&checkout.path)
            });
            if relocated {
                std::env::set_current_dir(cleanup_project)?;
            }
            globals.project = Some(cleanup_project.to_path_buf());
            let workers = LandWorkers(workers);
            let (mut worktree, branch_gone, worktree_kept) =
                prune_worktree(globals, branch, &upstream, tip, &workers);
            if relocated && worktree.status == Status::Done {
                worktree.detail.push_str(&format!(
                    "; your shell's directory was removed; change directory to {}",
                    cleanup_project.display()
                ));
            }
            steps.push(worktree);
            if worktree_kept {
                steps.push(Step::new(
                    "branch",
                    Status::Skipped,
                    format!("{branch} deletion skipped because its registered worktree was kept"),
                ));
            } else {
                steps.push(delete_branch(
                    git,
                    cleanup_project,
                    branch,
                    base,
                    &upstream,
                    tip,
                    branch_gone,
                )?);
            }
        }
    }
    Ok(steps)
}

fn attribute(
    tracker: &LandTracker,
    view: &Pull,
    number: u64,
    head: GitOid,
    reader: &super::wait::Reader<'_>,
) -> Result<()> {
    let bound = tracker.bind(&view.branch, number, head, reader);
    let closed = if matches!(view.state.as_str(), "CLOSED" | "MERGED") {
        tracker.close(number, view)
    } else {
        Ok(())
    };
    bound.and(closed)
}

pub(super) struct BatchTracking {
    tracker: Option<LandTracker>,
    view: Pull,
}
impl BatchTracking {
    pub(super) fn repository(&self) -> Option<&str> {
        self.tracker
            .as_ref()
            .map(|tracker| tracker.repository.as_str())
    }
}

/// Record the batch PR attribution before attempting its gate.
#[allow(
    clippy::disallowed_methods,
    reason = "bound forge reads for attribution"
)]
pub(super) fn batch_track(
    project: &Path,
    number: u64,
    globals: &provenant_cli::Globals,
) -> Result<BatchTracking> {
    let reader = super::wait::Reader {
        project,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        retry_command: format!("provenant land {number}"),
    };
    let rest = super::rest_pull(&|argv| reader.read(argv), &number.to_string())?;
    let head = rest.head.sha;
    let repository = rest
        .base
        .repo
        .as_ref()
        .ok_or("pull request has no base repository")?
        .full_name
        .clone();
    let view = from_rest(rest);
    let tracker = if view.cross_repository {
        None
    } else {
        match LandTracker::resolve(globals, &repository) {
            Ok(tracker) => {
                if let Err(error) = attribute(&tracker, &view, number, head, &reader) {
                    if error
                        .downcast_ref::<super::wait::Failure>()
                        .is_some_and(|failure| failure.code == "RATE_LIMITED")
                    {
                        return Err(error);
                    }
                    super::contributions::warn(Some(number), &error);
                }
                Some(tracker)
            }
            Err(error) => {
                super::contributions::warn(Some(number), &error);
                None
            }
        }
    };
    Ok(BatchTracking { tracker, view })
}

/// Run the existing local cleanup owner for a proven batch landing.
pub(super) fn batch_follow_up(
    git: &Git,
    project: &Path,
    number: u64,
    head: GitOid,
    globals: &mut provenant_cli::Globals,
    workers: &dyn WorkerObserver,
    tracking: Option<&BatchTracking>,
) -> Result<serde_json::Value> {
    if let Some(tracking) = tracking.filter(|tracking| {
        tracking.view.state == "MERGED"
            && (tracking.view.cross_repository || tracking.tracker.is_some())
    }) {
        log_landed(globals, number, &tracking.view);
        let steps = follow_up(git, project, &tracking.view, head, globals, workers)?;
        return Ok(serde_json::to_value(steps)?);
    }
    let rest = super::rest_pull(&|argv| super::execute(project, argv), &number.to_string())?;
    let repository = rest.base.repo.as_ref().map(|repo| repo.full_name.clone());
    let pull = from_rest(rest);
    if !pull.cross_repository
        && let Some(repository) = repository
    {
        let closed = if let Some(tracker) = tracking.and_then(|tracking| tracking.tracker.as_ref())
        {
            tracker.close(number, &pull)
        } else {
            LandTracker::resolve(globals, &repository)
                .and_then(|tracker| tracker.close(number, &pull))
        };
        if let Err(error) = closed {
            super::contributions::warn(Some(number), &error);
        }
    }
    log_landed(globals, number, &pull);
    let steps = follow_up(git, project, &pull, head, globals, workers)?;
    Ok(serde_json::to_value(steps)?)
}

/// Note a proven landing in the chair log. The merge is already durable, so
/// logging is advisory and never changes the landing's result.
fn log_landed(globals: &provenant_cli::Globals, number: u64, pull: &Pull) {
    if pull.state != "MERGED" {
        return;
    }
    let landed = pull
        .merge_commit
        .as_ref()
        .zip(pull.merged_at.as_deref())
        .and_then(|(commit, merged_at)| {
            merged_at
                .parse::<jiff::Timestamp>()
                .ok()
                .map(|at| (commit.oid.clone(), at.as_second()))
        });
    if let Some((commit, merged_at)) = landed {
        let _ = append_land_log(globals, number, &pull.title, &commit, merged_at);
    }
}

/// Whether the local branch named like the pull request's may be cleaned up.
enum Cleanup {
    /// The local branch tip, which the worktree removal must still find.
    Prune(GitOid),
    Skip(Status, String),
}

/// A local branch is the pull request's branch only when its head lives in
/// this repository and its tip is the head the pull request merged. It is safe
/// to remove only when the base also contains that tip. A fork's `feature`
/// says nothing about a local `feature`, a branch recreated at the base is not
/// the merged branch, and commits made after the merged head belong to neither.
/// A tip beyond the merged head is not matched even when the base contains it,
/// because that cannot tell later work from a branch reused at the base.
fn cleanup_target(
    git: &Git,
    project: &Path,
    pull: &Pull,
    merged: GitOid,
    upstream: &str,
) -> Result<Cleanup> {
    let branch = pull.branch.as_str();
    if pull.cross_repository {
        return Ok(Cleanup::Skip(
            Status::Skipped,
            format!(
                "the pull request head is in another repository, so a local {branch} is not its branch"
            ),
        ));
    }
    let Some(tip) = git.branch_commit(project, branch)? else {
        return Ok(Cleanup::Skip(Status::Skipped, "no local branch".into()));
    };
    let (contained, _) = git.run(
        project,
        &["merge-base", "--is-ancestor", &tip.to_string(), upstream],
    )?;
    if !contained {
        return Ok(Cleanup::Skip(
            Status::Refused,
            format!(
                "{branch} has commits that {upstream} does not contain, so its worktree is kept"
            ),
        ));
    }
    if tip != merged {
        return Ok(Cleanup::Skip(
            Status::Skipped,
            format!(
                "the local {branch} is at {tip}, not the merged pull request head {merged}, so it is not that branch"
            ),
        ));
    }
    Ok(Cleanup::Prune(tip))
}

/// The worktree step, and whether removing it also deleted the branch.
fn prune_worktree(
    globals: &provenant_cli::Globals,
    branch: &str,
    upstream: &str,
    tip: GitOid,
    workers: &dyn WorkerObserver,
) -> (Step, bool, bool) {
    let landed = worktrees::land_cleanup(globals, branch, upstream, tip, workers);
    let mut branch_gone = false;
    let mut worktree_kept = false;
    let step = match landed {
        Ok(landed) if !landed.registered => Step::new(
            "worktree",
            Status::Skipped,
            "no registered worktree for the branch",
        ),
        Ok(landed) => {
            branch_gone = landed.branch_deleted;
            worktree_kept = landed.registered && !landed.removed;
            let output = if landed.output.is_empty() {
                String::new()
            } else {
                format!("; deleted {} build output paths", landed.output.len())
            };
            match (&landed.kept, landed.removed) {
                (None, true) => Step::new("worktree", Status::Done, format!("removed{output}")),
                (Some(kept), true) => {
                    Step::new("worktree", Status::Done, format!("removed{output}; {kept}"))
                }
                (kept, false) => Step::new(
                    "worktree",
                    Status::Refused,
                    format!(
                        "kept{output}: {}",
                        kept.as_deref().unwrap_or("removal did not complete")
                    ),
                ),
            }
        }
        Err(error) => Step::new("worktree", Status::Refused, error.to_string()),
    };
    (step, branch_gone, worktree_kept)
}

/// Move the local base branch to the fetched one, only forward and only in a
/// checkout with no changes to tracked files. Untracked files do not count:
/// `git merge --ff-only` itself refuses if one would be overwritten.
fn fast_forward(
    git: &Git,
    project: &Path,
    base: &str,
    remote: &str,
    upstream: &str,
    checkouts: &[Checkout],
) -> Result<Step> {
    if git.branch_ref(project, base)?.is_none() {
        return Ok(Step::new(
            "fast_forward",
            Status::Skipped,
            format!("no local {base} branch"),
        ));
    }
    let outcome = match checkouts
        .iter()
        .find(|checkout| checkout.branch.as_deref() == Some(base))
    {
        Some(checkout) => {
            let dir = &checkout.path;
            let changes = git.full(dir, &["status", "--porcelain", "--untracked-files=no"])?;
            if changes.is_none_or(|changes| !changes.trim().is_empty()) {
                return Ok(Step::new(
                    "fast_forward",
                    Status::Refused,
                    format!(
                        "the {base} checkout at {} has local changes to tracked files",
                        dir.display()
                    ),
                ));
            }
            git.run(dir, &["merge", "--ff-only", upstream])?
        }
        None => git.run(project, &["fetch", remote, &format!("{base}:{base}")])?,
    };
    Ok(match outcome {
        (true, _) => Step::new(
            "fast_forward",
            Status::Done,
            format!("{base} is at {upstream}"),
        ),
        (false, detail) => Step::new("fast_forward", Status::Refused, detail),
    })
}

/// Delete the merged local branch, but only while it is still at `tip`, the
/// head the cleanup check verified: `git update-ref -d` compares and deletes in
/// one locked ref transaction, so a branch moved since the check is kept.
fn delete_branch(
    git: &Git,
    project: &Path,
    branch: &str,
    base: &str,
    upstream: &str,
    tip: GitOid,
    already_gone: bool,
) -> Result<Step> {
    if already_gone {
        return Ok(Step::new(
            "branch",
            Status::Done,
            format!("{branch} deleted with its worktree"),
        ));
    }
    if branch == base {
        return Ok(Step::new(
            "branch",
            Status::Refused,
            "the pull request branch is the base branch",
        ));
    }
    let local = format!("refs/heads/{branch}");
    if git.branch_ref(project, branch)?.is_none() {
        return Ok(Step::new("branch", Status::Skipped, "no local branch"));
    }
    let (contained, _) = git.run(project, &["merge-base", "--is-ancestor", &local, upstream])?;
    if !contained {
        return Ok(Step::new(
            "branch",
            Status::Refused,
            format!("{branch} has commits that {upstream} does not contain"),
        ));
    }
    // `git branch -d` would refuse a branch that any worktree has checked out,
    // is rebasing or is bisecting, but it deletes whatever the branch holds by
    // then. Keep that refusal here and make the deletion itself conditional on
    // the merged head.
    let holder = match holder_of(git, project, branch) {
        Ok(holder) => holder,
        Err(error) => {
            return Ok(Step::new(
                "branch",
                Status::Refused,
                format!("{branch} is kept because its use could not be checked: {error}"),
            ));
        }
    };
    if let Some((path, how)) = holder {
        let how = match how {
            Use::CheckedOut => "checked out",
            Use::Rebased => "being rebased",
            Use::Bisected => "being bisected",
        };
        return Ok(Step::new(
            "branch",
            Status::Refused,
            format!("{branch} is {how} at {path}"),
        ));
    }
    let tip = tip.to_string();
    Ok(
        match git.run(project, &["update-ref", "-d", &local, &tip])? {
            (true, _) => {
                // Like `git branch -d`, drop the branch's tracking settings, but
                // only when no other process has recreated the branch meanwhile.
                let recreated = git.branch_ref(project, branch)?.is_some();
                if !recreated {
                    git.run(
                        project,
                        &["config", "--remove-section", &format!("branch.{branch}")],
                    )?;
                }
                Step::new("branch", Status::Done, format!("{branch} deleted"))
            }
            (false, detail) => Step::new(
                "branch",
                Status::Refused,
                format!("{branch} is no longer at the merged head {tip}, so it is kept: {detail}"),
            ),
        },
    )
}

/// Land a graph node's pull request through the landing owner: the same
/// pinned head, candidate gate, merge and clean-up as `provenant land`, for the
/// PR whose head is `branch`. Without authority the owner only observes
/// whether the chair merged the pinned head.
pub(crate) fn graph_land(
    project: &Path,
    state: &Path,
    request: &provenant_fabric_runtime::graph::LandRequest<'_>,
    workers: &dyn WorkerObserver,
) -> provenant_fabric_runtime::graph::LandObservation {
    use provenant_fabric_runtime::graph::LandObservation;
    let (branch, head) = (request.branch, request.head);
    let Ok(head) = head.parse::<GitOid>() else {
        return LandObservation::Refused {
            reason: format!("{head} is not a full commit ID"),
        };
    };
    if !plain_ref(branch) {
        return LandObservation::Refused {
            reason: format!("{branch} is not a plain branch name"),
        };
    }
    let Some(number) = request.pull_request else {
        return match super::pull_for_branch(project, branch, head) {
            Ok(pull) => LandObservation::Identified {
                pull_request: pull.number,
            },
            Err(error) => LandObservation::Pending {
                reason: format!("no pull request found for {branch} yet: {error}"),
            },
        };
    };
    let globals = provenant_cli::Globals {
        json: true,
        full: false,
        state_dir: Some(state.to_owned()),
        project: Some(project.to_owned()),
    };
    let authority = match request.authority {
        None => provenant_records::landing::Authority::Observe,
        Some(provenant_fabric_runtime::graph::LandAuthority::Policy) => {
            provenant_records::landing::Authority::Policy
        }
        Some(provenant_fabric_runtime::graph::LandAuthority::Chair { .. }) => {
            provenant_records::landing::Authority::Chair
        }
    };
    let land = provenant_records::landing::LandRequest {
        members: vec![provenant_records::landing::LandMember {
            pr: number,
            head: Some(head.to_string()),
            base: None,
            // The landing policy reads the graph's typed exact-head review record.
            review_proof: provenant_records::landing::ReviewProof::Policy,
        }],
        authority,
        policy: provenant_records::landing::PolicyChoice::default(),
        ..Default::default()
    };
    let submission = super::landing::prepare(&globals, None, None, None, None)
        .and_then(|prepared| super::landing::submit(&prepared, land, false, &globals, workers));
    let submission = match submission {
        Ok(submission) => submission,
        Err(error) => {
            return LandObservation::Pending {
                reason: format!("landing pull request #{number}: {error}"),
            };
        }
    };
    observation(
        number,
        submission
            .record
            .as_ref()
            .and_then(|record| record.outcomes.get(&number)),
        submission.failure.as_ref(),
    )
}

/// What a graph land node makes of its member's landing outcome. Only a
/// landed member succeeds; one that can still change is observed again.
pub(crate) fn observation(
    number: u64,
    outcome: Option<&provenant_records::landing::MemberOutcome>,
    failure: Option<&super::landing::LandingError>,
) -> provenant_fabric_runtime::graph::LandObservation {
    use provenant_fabric_runtime::graph::LandObservation;
    use provenant_records::landing::MemberOutcome;
    match outcome {
        Some(MemberOutcome::Landed { .. }) => LandObservation::Merged {
            pull_request: Some(number),
            note: None,
        },
        Some(MemberOutcome::Refused { reason }) => LandObservation::Refused {
            reason: reason.clone(),
        },
        Some(MemberOutcome::Pending { reason } | MemberOutcome::Unknown { reason }) => {
            LandObservation::Pending {
                reason: reason.clone(),
            }
        }
        None => LandObservation::Pending {
            reason: failure.map_or_else(
                || format!("pull request #{number} has no landing outcome yet"),
                ToString::to_string,
            ),
        },
    }
}

fn append_land_log(
    globals: &provenant_cli::Globals,
    number: u64,
    title: &str,
    merge_commit: &str,
    merged_at: i64,
) -> Result<()> {
    use provenant_workflow::session::{HostSessionId, chair_session_for};
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let context = provenant_cli::fabric::Context::open(
        resolved.roots.clone(),
        &resolved.home,
        &resolved.project,
    )?;
    let session = provenant_cli::fabric::current_session()
        .and_then(|session| HostSessionId::parse(&session).ok())
        .or_else(|| {
            chair_session_for(
                &resolved.roots.state,
                context.identity.project,
                context.identity.inbox.actor,
            )
        });
    let Some(session) = session else {
        return Ok(());
    };
    let config = provenant_cli::config_reads::read_for(&resolved.roots, "knobs")?;
    append_land_entry(
        &resolved.roots,
        LandEntry {
            project: context.identity.project,
            host: context.identity.host,
            session,
            number,
            title: title.into(),
            merge_commit: merge_commit.into(),
            issued_at: merged_at,
        },
        &config.retention,
    )
}

struct LandEntry {
    project: provenant_core::ProjectId,
    host: provenant_core::HostId,
    session: provenant_workflow::session::HostSessionId,
    number: u64,
    title: String,
    merge_commit: String,
    issued_at: i64,
}

fn append_land_entry(
    roots: &provenant_core::Roots,
    entry: LandEntry,
    settings: &provenant_config::RetentionSettings,
) -> Result<()> {
    use provenant_workflow::log::{Append, LogStore, Retention};
    let now = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())?
        .as_micros()
        .div_euclid(1_000_000);
    let retention = Retention {
        max_age_seconds: settings.chair_log_days * 86400,
        replay_horizon_seconds: settings.chair_log_replay_seconds(),
        max_entries: settings.chair_log_entries,
        max_bytes: settings.chair_log_bytes,
    };
    let operation_id = provenant_core::OperationId::parse(&format!(
        "land-{}-{}",
        entry.number, entry.merge_commit
    ))?;
    LogStore::new(roots.state.clone(), entry.project, entry.session).append(
        Append {
            operation_id,
            issued_at: entry.issued_at,
            host: entry.host.to_string(),
            kind: "pull_request_landed".into(),
            markdown: format!(
                "Landed PR #{}: {} (merge commit {}).",
                entry.number, entry.title, entry.merge_commit
            ),
            request_ids: Vec::new(),
            attempt_ids: Vec::new(),
            issue_ids: Vec::new(),
            pr_ids: vec![entry.number.to_string()],
            channel: "provenant".into(),
            correction_of: None,
        },
        retention,
        now,
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod land_log_tests {
    use super::{LandEntry, append_land_entry};
    use provenant_workflow::{log::LogStore, session::HostSessionId};

    #[test]
    fn land_policy_preserves_combined_evidence_and_ancestor_mode()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::worktrees::{
            CensusPolicyWorkers, CensusSession, Occupancy, WorkerEvidence, WorkerObserver,
        };
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Observer(AtomicUsize);
        impl WorkerObserver for Observer {
            fn observe_roots(&self, _: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
                panic!("combined evidence required")
            }
            fn observe_evidence_with_policy(
                &self,
                roots: &[PathBuf],
                _: &[provenant_os::pid::CensusIgnoreEntry],
                land: bool,
            ) -> WorkerEvidence {
                assert!(land);
                self.0.fetch_add(1, Ordering::SeqCst);
                WorkerEvidence {
                    states: roots
                        .iter()
                        .cloned()
                        .map(|root| (root, Occupancy::Observed { process: None }))
                        .collect(),
                    holders: roots
                        .iter()
                        .cloned()
                        .map(|root| {
                            (
                                root.clone(),
                                super::ObservedProcess {
                                    pid: 42,
                                    command: Some("worker".into()),
                                    kind: provenant_confinement::worktrees::ProcessEvidenceKind::WorkingDirectory,
                                    cwd: Some(root),
                                },
                            )
                        })
                        .collect(),
                    ..WorkerEvidence::default()
                }
            }
        }
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let state = base.join("state");
        let checkout = base.join("checkout");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&state)?;
        std::fs::create_dir(&checkout)?;
        let mut input = provenant_core::RootInputs::new(home, checkout.clone());
        input.state_dir = Some(state);
        let roots = input.resolve(&provenant_os::fs::SystemPathResolver)?;
        let observer = Observer(AtomicUsize::new(0));
        let land = super::LandWorkers(&observer);
        let policy = CensusPolicyWorkers::from_roots(&land, &roots)?;
        let clock = provenant_os::clock::SystemClock;
        let session = CensusSession::new(&policy, &clock);
        assert!(session.requires_complete_census());
        assert!(session.observe_roots(std::slice::from_ref(&checkout))[&checkout].is_observed());
        assert!(
            session
                .observe_roots_with_details(std::slice::from_ref(&checkout))
                .0[&checkout]
                .is_observed()
        );
        assert_eq!(
            session.live_process(&checkout).map(|holder| holder.pid),
            Some(42)
        );
        assert_eq!(observer.0.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn a_landed_pull_request_appends_one_readable_entry() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        std::fs::create_dir_all(base.join("home"))?;
        std::fs::create_dir_all(base.join("assets"))?;
        std::fs::create_dir_all(base.join("state"))?;
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let project = provenant_core::ProjectId::from_bytes([3; 16]);
        let host = provenant_core::HostId::from_bytes([1; 16]);
        let session = HostSessionId::parse("land-test")?;
        let issued_at =
            provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let settings = provenant_config::RetentionSettings::default();

        for _ in 0..2 {
            append_land_entry(
                &roots,
                LandEntry {
                    project,
                    host,
                    session: session.clone(),
                    number: 42,
                    title: "A readable title".into(),
                    merge_commit: "0123456789abcdef0123456789abcdef01234567".into(),
                    issued_at,
                },
                &settings,
            )?;
        }

        let retention = provenant_workflow::log::Retention {
            max_age_seconds: settings.chair_log_days * 86400,
            replay_horizon_seconds: settings.chair_log_replay_seconds(),
            max_entries: settings.chair_log_entries,
            max_bytes: settings.chair_log_bytes,
        };
        let page = LogStore::new(roots.state, project, session).page(
            None,
            50,
            retention,
            provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new()),
        )?;
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].update.pr_ids, ["42"]);
        assert_eq!(
            page.entries[0].update.markdown,
            "Landed PR #42: A readable title (merge commit 0123456789abcdef0123456789abcdef01234567)."
        );
        Ok(())
    }
}

/// Test seam: the cleanup check for `feat` merged at `merged` into `main`, then
/// the Git command `between` (another process acting, none when empty), then
/// the branch deletion. The status and detail of the `branch` step.
#[cfg(test)]
pub(crate) fn delete_after_check(
    project: &Path,
    host_state: &Path,
    merged: GitOid,
    between: &[&str],
) -> Result<(String, String)> {
    let git = Git::new(host_state)?;
    let pull: Pull = serde_json::from_value(serde_json::json!({
        "state": "MERGED",
        "baseRefName": "main",
        "headRefName": "feat",
        "isCrossRepository": false,
    }))?;
    let upstream = "origin/main";
    let tip = match cleanup_target(&git, project, &pull, merged, upstream)? {
        Cleanup::Prune(tip) => tip,
        Cleanup::Skip(_, why) => return Err(format!("the branch was not prunable: {why}").into()),
    };
    if !between.is_empty() {
        let (moved, detail) = git.run(project, between)?;
        if !moved {
            return Err(format!("the interleaved command failed: {detail}").into());
        }
    }
    let step = delete_branch(&git, project, "feat", "main", upstream, tip, false)?;
    let status = serde_json::to_value(step.status)?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    Ok((status, step.detail))
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod graph_land_tests {
    use super::*;
    use provenant_fabric_runtime::graph::LandObservation;

    #[test]
    fn graph_land_resolves_its_pull_request_via_rest() {
        use std::os::unix::fs::PermissionsExt;
        const MARKER: &str = "PROVENANT_GRAPH_LAND_REST_CHILD";
        const HEAD: &str = "1111111111111111111111111111111111111111";
        mod rest_calls {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/rest_calls.rs"
            ));
        }
        if let Some(path) = std::env::var_os(MARKER) {
            let project = std::path::PathBuf::from(path);
            let observed = graph_land(
                &project,
                &project,
                &provenant_fabric_runtime::graph::LandRequest {
                    graph_id: "g",
                    node_id: "land",
                    branch: "feature",
                    head: HEAD,
                    pull_request: None,
                    tier: provenant_records::workflow::RiskTier::Routine,
                    authority: None,
                },
                &provenant_confinement::worktrees::SystemWorkerObserver,
            );
            assert!(matches!(
                observed,
                LandObservation::Identified { pull_request: 7 }
            ));
            let calls = std::fs::read_to_string(project.join("calls")).unwrap();
            assert!(rest_calls::rest_only(&calls), "{calls}");
            assert!(!rest_calls::rest_only(&format!(
                "{calls}pr list --head feature\n"
            )));
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(&gh, r##"#!/bin/sh
echo "$*" >> calls
case "$*" in
  *"/pulls"*) echo '[[{"number":7,"state":"open","merged_at":null,"head":{"sha":"1111111111111111111111111111111111111111","ref":"feature","repo":{"full_name":"owner/repo"}},"base":{"sha":"1111111111111111111111111111111111111111","ref":"main","repo":{"full_name":"owner/repo"}}}]]' ;;
  *) echo '{"owner":{"login":"owner"}}' ;;
esac
"##).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap()
        );
        let argv = [
            std::env::current_exe().unwrap().into_os_string(),
            "--exact".into(),
            "pr::land::graph_land_tests::graph_land_resolves_its_pull_request_via_rest".into(),
            "--nocapture".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| key != MARKER && key != "PATH")
            .collect();
        env.push((MARKER.into(), dir.path().into()));
        env.push(("PATH".into(), path.into()));
        let output = provenant_os::sys::fixture_command(
            &argv,
            &env,
            &std::env::current_dir().unwrap(),
            false,
        )
        .unwrap()
        .output()
        .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_member_outcome_maps_to_a_graph_observation() {
        use provenant_records::landing::MemberOutcome;
        assert_eq!(
            observation(7, Some(&MemberOutcome::Landed { commit: None }), None),
            LandObservation::Merged {
                pull_request: Some(7),
                note: None,
            }
        );
        for outcome in [
            MemberOutcome::Pending {
                reason: "ci".into(),
            },
            MemberOutcome::Unknown {
                reason: "lost".into(),
            },
        ] {
            assert!(matches!(
                observation(7, Some(&outcome), None),
                LandObservation::Pending { .. }
            ));
        }
        assert!(matches!(
            observation(7, Some(&MemberOutcome::Refused { reason: "HEAD_MOVED: x".into() }), None),
            LandObservation::Refused { reason } if reason.starts_with("HEAD_MOVED")
        ));
        assert!(matches!(
            observation(7, None, None),
            LandObservation::Pending { .. }
        ));
    }
}
