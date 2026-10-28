//! `land`: the existing CI watch and head-pinned merge, then local
//! follow-up. No step forces: each one refuses and says why.
use super::git::{Git, plain_ref};
use super::{Result, checks, head, merge, successful};
use crate::{emit, roots, worktrees};
use provenant_confinement::worktrees::{ObservedProcess, WorkerObserver};
use provenant_core::GitOid;
use provenant_records::{CommandError, CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

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
            "state,baseRefName,headRefName,isCrossRepository",
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
/// checkout with no changes.
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
            let changes = git.full(dir, &["status", "--porcelain"])?;
            if changes.is_none_or(|changes| !changes.trim().is_empty()) {
                return Ok(Step::new(
                    "fast_forward",
                    Status::Refused,
                    format!("the {base} checkout at {} has local changes", dir.display()),
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

pub(crate) fn run(
    number: u64,
    expected: Option<GitOid>,
    globals: &provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> u8 {
    const LEAF: &str = "land";
    let mut captured = expected;
    let mut output_globals = provenant_cli::Globals {
        json: globals.json,
        state_dir: globals.state_dir.clone(),
        project: globals.project.clone(),
    };
    let mut merged = false;
    let mut state = String::new();
    let mut steps = Vec::new();
    let result = (|| -> Result<()> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let text = number.to_string();
        let current = head(&project, &text)?;
        captured = Some(current);
        if expected.is_some_and(|expected| current != expected) {
            return Err(format!("pull request head moved to {current}").into());
        }
        let view = pull(&project, &text)?;
        if !view.cross_repository {
            let binding = (|| -> Result<()> {
                let resolved = roots::resolve(
                    globals,
                    roots::Workspace::Checkout,
                    roots::Product::Optional,
                )?;
                let bound = super::contributions::bind(&resolved, &view.branch, number, current);
                let closed = if matches!(view.state.as_str(), "CLOSED" | "MERGED") {
                    super::contributions::close(&resolved, number)
                } else {
                    Ok(())
                };
                bound.and(closed)
            })();
            if let Err(error) = binding {
                super::contributions::warn(Some(number), &error);
            }
        }
        checks(&project, &text, current, true)?;
        merge(&project, &text, current)?;
        let view = pull(&project, &text)?;
        state = view.state.clone();
        merged = view.state == "MERGED";
        if matches!(view.state.as_str(), "CLOSED" | "MERGED") && !view.cross_repository {
            let closed = (|| -> Result<()> {
                let resolved = roots::resolve(
                    globals,
                    roots::Workspace::Checkout,
                    roots::Product::Optional,
                )?;
                super::contributions::close(&resolved, number)
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
    let expected = captured
        .map(|head| head.to_string())
        .unwrap_or_else(|| "unknown".into());
    let refused: Vec<&str> = steps
        .iter()
        .filter(|step| step.status == Status::Refused)
        .map(|step| step.step)
        .collect();
    let outcome = if result.is_err() && !merged {
        Outcome::Error
    } else if result.is_err() || !refused.is_empty() {
        Outcome::Refused
    } else {
        Outcome::Ok
    };
    let mut report = CommandResult::new(
        LEAF,
        outcome,
        json!({
            "number": number,
            "head": expected,
            "result": if merged || result.is_ok() { "pass" } else { "fail" },
            "merged": merged,
            "state": state,
            "steps": steps,
        }),
    );
    if let Err(error) = &result {
        report.error = Some(CommandError {
            code: "PR".into(),
            message: if merged {
                format!("merged #{number} {expected}, then: {error}")
            } else {
                format!("fail #{number} {expected}: {error}")
            },
            cause: None,
            repair: Some(format!(
                "run `provenant pr watch {number}`, then `provenant land {number}`"
            )),
        });
    } else if !refused.is_empty() {
        report.error = Some(CommandError {
            code: "PR_LAND".into(),
            message: format!(
                "merged #{number} {expected}, but these steps refused: {}",
                refused.join(", ")
            ),
            cause: None,
            repair: Some(format!(
                "run `provenant pr watch {number}`, then `provenant land {number}`"
            )),
        });
    }
    let mut text = if merged {
        format!("merged #{number} {expected}\n")
    } else if result.is_ok() {
        format!("pending #{number} {expected} ({state})\n")
    } else {
        format!("fail #{number} {expected}\n")
    };
    for step in &steps {
        text.push_str(&format!(
            "  {} {} {}\n",
            step.step,
            step.status.name(),
            step.detail
        ));
    }
    emit(&report, &output_globals, &text, false)
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
