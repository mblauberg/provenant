//! `pr land`: the existing CI watch and head-pinned merge, then local
//! follow-up. No step forces: each one refuses and says why.
use super::{Result, checks, execute_for, head, merge, successful};
use crate::{emit, roots, worktrees};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::GitOid;
use provenant_os::exec::Termination;
use provenant_records::{CommandError, CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{path::Path, time::Duration};

const GIT_TIMEOUT: Duration = Duration::from_secs(300);

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
struct Pull {
    state: String,
    #[serde(rename = "baseRefName")]
    base: String,
    #[serde(rename = "headRefName")]
    branch: String,
}
fn pull(project: &Path, number: &str) -> Result<Pull> {
    let bytes = successful(super::execute(
        project,
        &[
            "gh",
            "pr",
            "view",
            number,
            "--json",
            "state,baseRefName,headRefName",
        ],
        false,
    )?)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// A branch or remote name that cannot be read as an option or a revision
/// expression, so it is safe as a Git argument.
fn plain_ref(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\@{".contains(c))
}

struct Git {
    executable: String,
}
impl Git {
    fn new() -> Result<Self> {
        let git = provenant_os::git::git_executable().ok_or("Git executable is unavailable")?;
        let executable = git
            .to_str()
            .ok_or("Git executable path is not UTF-8")?
            .to_owned();
        Ok(Self { executable })
    }
    /// Whether Git exited zero, and its first line of output or error.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<(bool, String)> {
        let mut argv = vec![self.executable.as_str()];
        argv.extend_from_slice(args);
        let output = execute_for(dir, &argv, GIT_TIMEOUT, false)?;
        let succeeded = output.termination == Termination::Exit(0);
        let bytes = if succeeded {
            &output.stdout.bytes
        } else {
            &output.stderr.bytes
        };
        let text = String::from_utf8_lossy(bytes);
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        Ok((succeeded, line.chars().take(200).collect()))
    }
    fn full(&self, dir: &Path, args: &[&str]) -> Result<Option<String>> {
        let mut argv = vec![self.executable.as_str()];
        argv.extend_from_slice(args);
        let output = execute_for(dir, &argv, GIT_TIMEOUT, false)?;
        Ok((output.termination == Termination::Exit(0))
            .then(|| String::from_utf8_lossy(&output.stdout.bytes).into_owned()))
    }
}

/// The path of the checkout that has `branch` checked out, if any.
fn checkout_of(git: &Git, project: &Path, branch: &str) -> Result<Option<String>> {
    let Some(listing) = git.full(project, &["worktree", "list", "--porcelain"])? else {
        return Err("cannot list worktrees".into());
    };
    let wanted = format!("branch refs/heads/{branch}");
    let mut path = None;
    for line in listing.lines() {
        if let Some(found) = line.strip_prefix("worktree ") {
            path = Some(found);
        } else if line == wanted {
            return Ok(path.map(str::to_owned));
        }
    }
    Ok(None)
}

fn follow_up(
    project: &Path,
    pull: &Pull,
    merged: GitOid,
    globals: &provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> Result<Vec<Step>> {
    let git = Git::new()?;
    let (base, branch) = (pull.base.as_str(), pull.branch.as_str());
    if !plain_ref(base) || !plain_ref(branch) {
        return Err("pull request branch names are not plain Git names".into());
    }
    let configured = git.full(
        project,
        &["config", "--get", &format!("branch.{base}.remote")],
    )?;
    let remote = configured
        .as_deref()
        .map(str::trim)
        .filter(|name| plain_ref(name) && *name != ".")
        .unwrap_or("origin")
        .to_owned();
    let upstream = format!("{remote}/{base}");
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

    steps.push(fast_forward(&git, project, base, &remote, &upstream)?);

    let landed = worktrees::land_cleanup(globals, branch, &upstream, workers);
    let mut branch_gone = false;
    steps.push(match landed {
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
    });

    steps.push(delete_branch(
        &git,
        project,
        branch,
        base,
        &upstream,
        branch_gone,
    )?);
    Ok(steps)
}

/// Move the local base branch to the fetched one, only forward and only in a
/// checkout with no changes.
fn fast_forward(
    git: &Git,
    project: &Path,
    base: &str,
    remote: &str,
    upstream: &str,
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
    let outcome = match checkout_of(git, project, base)? {
        Some(path) => {
            let dir = Path::new(&path);
            let changes = git.full(dir, &["status", "--porcelain"])?;
            if changes.is_none_or(|changes| !changes.trim().is_empty()) {
                return Ok(Step::new(
                    "fast_forward",
                    Status::Refused,
                    format!("the {base} checkout at {path} has local changes"),
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

/// Delete the merged local branch with `git branch -d`, which itself refuses a
/// branch that is not fully merged.
fn delete_branch(
    git: &Git,
    project: &Path,
    branch: &str,
    base: &str,
    upstream: &str,
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
    Ok(match git.run(project, &["branch", "-d", branch])? {
        (true, _) => Step::new("branch", Status::Done, format!("{branch} deleted")),
        (false, detail) => Step::new("branch", Status::Refused, detail),
    })
}

pub(super) fn run(
    number: u64,
    expected: GitOid,
    globals: &provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> u8 {
    const LEAF: &str = "pr land";
    let mut merged = false;
    let mut state = String::new();
    let mut steps = Vec::new();
    let result = (|| -> Result<()> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let text = number.to_string();
        let current = head(&project, &text)?;
        if current != expected {
            return Err(format!("pull request head moved to {current}").into());
        }
        checks(&project, &text, current, true)?;
        merge(&project, &text, current)?;
        let view = pull(&project, &text)?;
        state = view.state.clone();
        merged = view.state == "MERGED";
        if merged {
            steps = follow_up(&project, &view, current, globals, workers)?;
        }
        Ok(())
    })();
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
            repair: None,
        });
    } else if !refused.is_empty() {
        report.error = Some(CommandError {
            code: "PR_LAND".into(),
            message: format!(
                "merged #{number} {expected}, but these steps refused: {}",
                refused.join(", ")
            ),
            cause: None,
            repair: None,
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
            serde_json::to_value(step.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            step.detail
        ));
    }
    emit(&report, globals, &text, false)
}
