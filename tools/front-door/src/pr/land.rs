//! `land`: the existing CI watch and head-pinned merge, then local
//! follow-up. No step forces: each one refuses and says why.
use super::git::{Git, plain_ref};
use super::{Result, head, merge, successful};
use crate::{emit_summarised, roots, worktrees};
use provenant_confinement::worktrees::{ObservedProcess, WorkerObserver};
use provenant_core::GitOid;
use provenant_records::{CommandError, CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::json;
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
impl Status {
    /// The name the JSON report and the text report both give the status.
    fn name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
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
}
/// The pull request's state after a merge attempt, retrying transient reads.
// This CLI edge owns the real deadline.
#[allow(clippy::disallowed_methods)]
fn reread(project: &Path, number: &str) -> Result<Pull> {
    let reader = super::wait::Reader {
        project,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
    };
    let bytes = successful(reader.read(&[
        "gh",
        "pr",
        "view",
        number,
        "--json",
        "state,baseRefName,headRefName,isCrossRepository,title,mergeCommit,mergedAt",
    ])?)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub(super) fn pull(project: &Path, number: &str) -> Result<Pull> {
    let bytes = successful(super::execute(
        project,
        &[
            "gh",
            "pr",
            "view",
            number,
            "--json",
            "state,baseRefName,headRefName,isCrossRepository,title,mergeCommit,mergedAt",
        ],
        false,
    )?)?;
    Ok(serde_json::from_slice(&bytes)?)
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
    let listing = git
        .full(project, &["worktree", "list", "--porcelain", "-z"])?
        .ok_or("cannot list worktrees")?;
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
    let common = git
        .full(
            project,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?
        .ok_or("cannot find the common Git directory")?;
    let common = fs::canonicalize(common.trim_end_matches('\n'))?;
    let primary = &checkouts.first().ok_or("no primary checkout")?.path;
    let valid = |candidate: &Path| -> Result<bool> {
        if roots::selected_project(Some(candidate), true)
            .ok()
            .as_deref()
            != Some(candidate)
        {
            return Ok(false);
        }
        let Some(private) = git.full(candidate, &["rev-parse", "--absolute-git-dir"])? else {
            return Ok(false);
        };
        Ok(fs::canonicalize(private.trim_end_matches('\n'))
            .ok()
            .as_deref()
            == Some(&common))
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
    fn requires_complete_census(&self) -> bool {
        true
    }

    fn observe_roots(
        &self,
        roots: &[PathBuf],
    ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState> {
        self.0.observe_roots_for_land(roots)
    }

    fn live_process(&self, root: &std::path::Path) -> Option<ObservedProcess> {
        self.0.live_process_for_land(root)
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
    let Some(common) = git.full(project, &["rev-parse", "--git-common-dir"])? else {
        return Err("cannot find the common Git directory".into());
    };
    let common = project.join(common.trim_end_matches('\n'));
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
    project: &Path,
    pull: &Pull,
    merged: GitOid,
    globals: &mut provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> Result<Vec<Step>> {
    let git = Git::new()?;
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

    let checkouts = checkouts(&git, project)?;
    steps.push(fast_forward(
        &git, project, base, &remote, &upstream, &checkouts,
    )?);

    match cleanup_target(&git, project, pull, pinned, &upstream)? {
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
            let (mut worktree, branch_gone) =
                prune_worktree(globals, branch, &upstream, tip, &workers);
            if relocated && worktree.status == Status::Done {
                worktree.detail.push_str(&format!(
                    "; your shell's directory was removed; change directory to {}",
                    cleanup_project.display()
                ));
            }
            steps.push(worktree);
            steps.push(delete_branch(
                &git,
                cleanup_project,
                branch,
                base,
                &upstream,
                tip,
                branch_gone,
            )?);
        }
    }
    Ok(steps)
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
    let local = format!("refs/heads/{branch}^{{commit}}");
    let (exists, tip) = git.run(project, &["rev-parse", "--verify", "--quiet", &local])?;
    if !exists {
        return Ok(Cleanup::Skip(Status::Skipped, "no local branch".into()));
    }
    let tip = GitOid::parse(&tip)?;
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
) -> (Step, bool) {
    let landed = worktrees::land_cleanup(globals, branch, upstream, tip, workers);
    let mut branch_gone = false;
    let step = match landed {
        Ok(landed) if !landed.registered => Step::new(
            "worktree",
            Status::Skipped,
            "no registered worktree for the branch",
        ),
        Ok(landed) => {
            branch_gone = landed.branch_deleted;
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
    (step, branch_gone)
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
    let local = format!("refs/heads/{base}");
    let (exists, _) = git.run(project, &["rev-parse", "--verify", "--quiet", &local])?;
    if !exists {
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
    let (exists, _) = git.run(project, &["rev-parse", "--verify", "--quiet", &local])?;
    if !exists {
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
                let (recreated, _) =
                    git.run(project, &["rev-parse", "--verify", "--quiet", &local])?;
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

/// One land's report, rendered but not yet printed.
pub(crate) struct Landing {
    pub(crate) report: CommandResult<serde_json::Value>,
    text: String,
    summary: provenant_cli::Summary,
    globals: provenant_cli::Globals,
}

pub(crate) fn run(
    number: u64,
    expected: Option<GitOid>,
    wait: Option<u64>,
    globals: &provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> u8 {
    let landing = execute(number, expected, wait, globals, workers);
    emit_summarised(
        &landing.report,
        &landing.globals,
        &landing.text,
        false,
        Some(&landing.summary),
    )
}

/// Land a graph node's pull request: the same head-pinned merge and cleanup
/// as `provenant land`, for the open PR whose head is `branch`.
pub(crate) fn graph_land(
    project: &Path,
    state: &Path,
    branch: &str,
    head: &str,
    workers: &dyn WorkerObserver,
) -> provenant_fabric_runtime::graph::LandObservation {
    use provenant_fabric_runtime::graph::LandObservation;
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
    let number = match super::number_for_branch(project, branch) {
        Ok(number) => number,
        Err(error) => {
            return LandObservation::Pending {
                reason: format!("no pull request found for {branch} yet: {error}"),
            };
        }
    };
    let globals = provenant_cli::Globals {
        json: true,
        full: false,
        state_dir: Some(state.to_owned()),
        project: Some(project.to_owned()),
    };
    observation(&execute(number, Some(head), None, &globals, workers).report)
}

/// What a graph land node makes of a land report. Only a merged PR succeeds;
/// a state that can still change is observed again; the rest fail the node.
pub(crate) fn observation(
    report: &CommandResult<serde_json::Value>,
) -> provenant_fabric_runtime::graph::LandObservation {
    use provenant_fabric_runtime::graph::LandObservation;
    let data = &report.data;
    let number = data["number"].as_u64();
    if data["merged"].as_bool() == Some(true) {
        let cleanup = (data["cleanup_pending"].as_bool() == Some(true)).then(|| {
            format!(
                "cleanup pending; next: provenant land {} --full",
                number.unwrap_or_default()
            )
        });
        return LandObservation::Merged {
            pull_request: number,
            note: data["merge_note"].as_str().map(str::to_owned).or(cleanup),
        };
    }
    let Some(error) = &report.error else {
        return LandObservation::Pending {
            reason: format!(
                "pull request #{} is not merged yet ({})",
                number.unwrap_or_default(),
                data["state"].as_str().unwrap_or("unknown")
            ),
        };
    };
    let reason = match &error.repair {
        Some(repair) => format!("{}: {}; next: {repair}", error.code, error.message),
        None => format!("{}: {}", error.code, error.message),
    };
    match error.code.as_str() {
        "CI_PENDING" | "CI_UNAVAILABLE" | "MERGE_STATE_UNKNOWN" | "PR_UNAVAILABLE" => {
            LandObservation::Pending { reason }
        }
        _ => LandObservation::Refused { reason },
    }
}

pub(crate) fn execute(
    number: u64,
    expected: Option<GitOid>,
    wait: Option<u64>,
    globals: &provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> Landing {
    const LEAF: &str = "land";
    let mut captured = expected;
    let mut output_globals = provenant_cli::Globals {
        json: globals.json,
        full: globals.full,
        state_dir: globals.state_dir.clone(),
        project: globals.project.clone(),
    };
    let mut merged = false;
    let mut merge_note: Option<String> = None;
    let mut state = String::new();
    let mut steps = Vec::new();
    let mut failure_code = "PR_UNAVAILABLE";
    let mut landed: Option<(String, String, i64)> = None;
    let result = (|| -> Result<()> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let text = number.to_string();
        let current = head(&project, &text)?;
        captured = Some(current);
        if expected.is_some_and(|expected| current != expected) {
            failure_code = "HEAD_MOVED";
            return Err(format!("pull request head moved to {current}").into());
        }
        let view = pull(&project, &text)?;
        let attribution = std::cell::OnceCell::new();
        if !view.cross_repository {
            let binding = (|| -> Result<()> {
                let tracker = attribution
                    .get_or_init(|| super::contributions::Tracker::resolve(globals))
                    .as_ref()
                    .map_err(|error| error.to_string())?;
                let bound = tracker.bind(&view.branch, number, current);
                let closed = if matches!(view.state.as_str(), "CLOSED" | "MERGED") {
                    tracker.close(number)
                } else {
                    Ok(())
                };
                bound.and(closed)
            })();
            if let Err(error) = binding {
                super::contributions::warn(Some(number), &error);
            }
        }
        // A retry after a successful merge only retries safe local cleanup.
        // It must not wait for CI, merge again or depend on retained CI data.
        let view = if view.state != "MERGED" {
            if view.state == "CLOSED" {
                failure_code = "PR_CLOSED";
                return Err("pull request is closed without merging".into());
            }
            failure_code = "CI_UNAVAILABLE";
            super::wait::checks(&project, number, current, wait, globals.json)?;
            failure_code = "MERGE_FAILED";
            // A merge that errors or times out may still have taken effect, so
            // the PR's own state decides the outcome, never the command's.
            let merge_error = merge(&project, &text, current).err();
            match reread(&project, &text) {
                Ok(view) if view.state == "MERGED" => {
                    merge_note = merge_error.map(|error| error.to_string());
                    view
                }
                Ok(view) => match merge_error {
                    Some(error) => return Err(error),
                    None => view,
                },
                Err(error) => {
                    // The merge may or may not have happened; rerunning land
                    // is safe because a merged PR only retries cleanup.
                    if merge_error.is_none() {
                        failure_code = "MERGE_STATE_UNKNOWN";
                    }
                    return Err(format!(
                        "cannot confirm the pull request state after merging: {error}"
                    )
                    .into());
                }
            }
        } else {
            view
        };
        state = view.state.clone();
        merged = view.state == "MERGED";
        if merged {
            landed = view
                .merge_commit
                .as_ref()
                .zip(view.merged_at.as_deref())
                .and_then(|(commit, merged_at)| {
                    merged_at
                        .parse::<jiff::Timestamp>()
                        .ok()
                        .map(|at| (view.title.clone(), commit.oid.clone(), at.as_second()))
                });
        }
        if matches!(view.state.as_str(), "CLOSED" | "MERGED") && !view.cross_repository {
            let closed = (|| -> Result<()> {
                let tracker = attribution
                    .get_or_init(|| super::contributions::Tracker::resolve(globals))
                    .as_ref()
                    .map_err(|error| error.to_string())?;
                tracker.close(number)
            })();
            if let Err(error) = closed {
                super::contributions::warn(Some(number), &error);
            }
        }
        if merged {
            steps = follow_up(&project, &view, current, &mut output_globals, workers)?;
        }
        Ok(())
    })();
    if merged && let Some((title, merge_commit, merged_at)) = landed {
        // The merge is already durable. Progress logging is advisory and must
        // never change the result of landing a pull request.
        let _ = append_land_log(globals, number, &title, &merge_commit, merged_at);
    }
    let expected = captured
        .map(|head| head.to_string())
        .unwrap_or_else(|| "unknown".into());
    let refused: Vec<&str> = steps
        .iter()
        .filter(|step| step.status == Status::Refused)
        .map(|step| step.step)
        .collect();
    if let Some(error) = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<super::wait::Failure>())
    {
        failure_code = error.code;
    }
    let cleanup_pending = merged && (result.is_err() || !refused.is_empty());
    let pending = !merged && result.is_ok();
    let outcome = if merged {
        Outcome::Ok
    } else if pending {
        Outcome::Pending
    } else if failure_code == "CI_TIMEOUT" {
        Outcome::Timeout
    } else if matches!(
        failure_code,
        "CI_PENDING" | "CI_FAILED" | "HEAD_MOVED" | "PR_CLOSED"
    ) {
        Outcome::Refused
    } else {
        Outcome::Error
    };
    let mut report = CommandResult::new(
        LEAF,
        outcome,
        json!({
            "number": number,
            "head": expected,
            "result": if merged { "pass" } else if pending { "pending" } else { "fail" },
            "merged": merged,
            "reason": if cleanup_pending { "cleanup_pending" } else if merged { "landed" } else if pending { "merge_pending" } else { failure_code },
            "merge_note": merge_note,
            "cleanup_pending": cleanup_pending,
            "cleanup_error": if merged { result.as_ref().err().map(ToString::to_string) } else { None },
            "cleanup_refused": refused,
            "state": state,
            "steps": steps,
        }),
    );
    if let Err(error) = &result
        && !merged
    {
        report.error = Some(CommandError {
            code: failure_code.into(),
            message: format!("fail #{number} {expected}: {error}"),
            cause: None,
            repair: Some(match failure_code {
                "CI_PENDING" | "CI_TIMEOUT" => {
                    format!("provenant land {number} --head {expected} --wait")
                }
                // Landing again only retries cleanup if the merge did happen.
                "MERGE_FAILED" | "MERGE_STATE_UNKNOWN" => {
                    format!("provenant land {number} --head {expected}")
                }
                "CI_FAILED" => format!("gh pr checks {number}"),
                "HEAD_MOVED" => format!("provenant pr status {number} --full"),
                _ => "provenant help land".into(),
            }),
        });
    }
    let mut text = if merged {
        format!("merged #{number} {expected}\n")
    } else if pending {
        format!("pending #{number} {expected} ({state})\n")
    } else {
        format!("fail #{number} {expected}\n")
    };
    if let Some(note) = &merge_note {
        text.push_str(&format!(
            "merge reported an error but the PR is merged: {note}\n"
        ));
    }
    if cleanup_pending {
        text.push_str(&format!("cleanup pending: {}\n", refused.join(", ")));
        if let Err(error) = &result {
            text.push_str(&format!("cleanup: {error}\n"));
        }
    }
    for step in &steps {
        text.push_str(&format!(
            "  {} {} {}\n",
            step.step,
            step.status.name(),
            step.detail
        ));
    }
    let next = if cleanup_pending {
        format!("provenant land {number} --full")
    } else if !merged {
        format!("provenant pr status {number} --wait")
    } else {
        "provenant status".into()
    };
    // Compact shows the headline, any merge or cleanup note, then the steps
    // that did not simply pass; --full shows every step.
    let mut rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
    if merged && !cleanup_pending {
        rows.retain(|row| !row.starts_with("  "));
    }
    let summary = provenant_cli::Summary::new(
        text.lines().next().unwrap_or_default().to_owned(),
        rows,
        Some(next),
    );
    Landing {
        report,
        text,
        summary,
        globals: output_globals,
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
    let config =
        provenant_config::read_with_domains(&resolved.roots, &provenant_cli::domains::SECTIONS)?;
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
        let issued_at = jiff::Timestamp::now().as_second();
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
            jiff::Timestamp::now().as_second(),
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
    merged: GitOid,
    between: &[&str],
) -> Result<(String, String)> {
    let git = Git::new()?;
    let pull: Pull = serde_json::from_value(json!({
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
    Ok((step.status.name(), step.detail))
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod graph_land_tests {
    use super::*;
    use provenant_fabric_runtime::graph::LandObservation;

    fn report(data: serde_json::Value, code: Option<&str>) -> CommandResult<serde_json::Value> {
        let mut report = CommandResult::new("land", Outcome::Error, data);
        report.error = code.map(|code| CommandError {
            code: code.into(),
            message: "fail #7".into(),
            cause: None,
            repair: Some("provenant land 7".into()),
        });
        report
    }

    #[test]
    fn a_land_report_maps_to_a_graph_observation() {
        assert_eq!(
            observation(&report(
                json!({"number":7, "merged":true, "cleanup_pending":true, "merge_note":null}),
                None
            )),
            LandObservation::Merged {
                pull_request: Some(7),
                note: Some("cleanup pending; next: provenant land 7 --full".into()),
            }
        );
        assert!(matches!(
            observation(&report(json!({"number":7, "merged":false, "state":"OPEN"}), None)),
            LandObservation::Pending { reason } if reason.contains("not merged yet (OPEN)")
        ));
        for code in [
            "CI_PENDING",
            "CI_UNAVAILABLE",
            "MERGE_STATE_UNKNOWN",
            "PR_UNAVAILABLE",
        ] {
            assert!(matches!(
                observation(&report(json!({"number":7, "merged":false}), Some(code))),
                LandObservation::Pending { .. }
            ));
        }
        for code in ["CI_FAILED", "HEAD_MOVED", "PR_CLOSED", "MERGE_FAILED"] {
            assert!(matches!(
                observation(&report(json!({"number":7, "merged":false}), Some(code))),
                LandObservation::Refused { reason } if reason.starts_with(code) && reason.contains("next: provenant land 7")
            ));
        }
    }
}
