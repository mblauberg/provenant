//! `pr open`: the repository's template filled from the attempts' receipts.
use super::git::{Git, plain_ref};
use super::{Refused, Result, code_of, execute_with, successful};
use crate::{emit, fail, roots};
use provenant_cli::{
    Attempt, Globals, PrOpen, Target,
    pr_body::{self, Body},
    run::{AttemptFacts, Edge, attempt_facts},
};
use provenant_core::GitOid;
use provenant_records::{CommandResult, Outcome};
use serde::Deserialize;
use serde_json::json;
use std::{io::Read, path::Path, time::Duration};

const TEMPLATE: &str = ".github/pull_request_template.md";
const TEXT_LIMIT: u64 = 128 * 1024;
const CREATE_TIMEOUT: Duration = Duration::from_secs(120);

fn read_text(path: &Path, what: &str) -> Result<String> {
    let mut text = String::new();
    let unreadable = |error: std::io::Error| {
        Refused::new(
            "PR_FILE_UNREADABLE",
            format!("cannot read {what} {}: {error}", path.display()),
        )
    };
    std::fs::File::open(path)
        .map_err(unreadable)?
        .take(TEXT_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(unreadable)?;
    if text.len() as u64 > TEXT_LIMIT {
        return Err(format!("{what} is over {} KiB", TEXT_LIMIT / 1024).into());
    }
    Ok(text)
}

struct Plan {
    title: String,
    body: String,
    base: String,
    branch: String,
    head: GitOid,
    remote: String,
}

fn default_branch(project: &Path) -> Result<String> {
    #[derive(Deserialize)]
    struct Repository {
        #[serde(rename = "defaultBranchRef")]
        branch: Named,
    }
    #[derive(Deserialize)]
    struct Named {
        name: String,
    }
    let bytes = successful(super::execute(
        project,
        &["gh", "repo", "view", "--json", "defaultBranchRef"],
        false,
    )?)?;
    Ok(serde_json::from_slice::<Repository>(&bytes)?.branch.name)
}

/// Resolve the remote-tracking base through the command's bounded facts session.
fn base_commit(git: &Git, project: &Path, remote: &str, base: &str) -> Result<Option<String>> {
    use provenant_confinement::repo::GitRefName;
    let Ok(reference) = GitRefName::remote_tracking(format!("{remote}/{base}")) else {
        return Ok(None);
    };
    Ok(git.commit(project, reference)?.map(|oid| oid.to_string()))
}

fn plan(
    command: &PrOpen,
    globals: &Globals,
    project: &Path,
    resolved: &roots::Resolved,
) -> Result<Plan> {
    let template = read_text(&project.join(TEMPLATE), "the pull request template")?;
    let summary = read_text(&command.summary_file, "the summary file")?;
    if summary.trim().is_empty() {
        return Err(Refused::new("PR_SUMMARY_EMPTY", "the summary file is empty").into());
    }
    let edge = Edge {
        roots: &resolved.roots,
        home: &resolved.home,
        assets: &resolved.assets,
        cwd: project,
    };
    let _ = globals;
    let fail = |error: provenant_cli::run::Fail| error.message;
    let author = attempt_facts(&edge, std::slice::from_ref(&command.from_attempt))
        .map_err(fail)?
        .remove(0);
    if !matches!(author.state.as_str(), "succeeded" | "review_required") {
        return Err(Refused::new(
            "PR_ATTEMPT_STATE",
            format!(
                "attempt {} ended {}; only a succeeded or review_required attempt can back a pull request",
                author.attempt_id, author.state
            ),
        )
        .into());
    }
    let reviews: Vec<Attempt> = if command.review.is_empty() {
        author
            .reviews
            .iter()
            .map(|id| {
                Attempt(Target {
                    selection: None,
                    run: None,
                    task: None,
                    attempt: Some(*id),
                })
            })
            .collect()
    } else {
        command.review.clone()
    };
    let reviews: Vec<AttemptFacts> = if reviews.is_empty() {
        Vec::new()
    } else {
        attempt_facts(&edge, &reviews).map_err(fail)?
    };
    let branch = command
        .branch
        .clone()
        .or_else(|| author.branch.clone())
        .ok_or("the attempt delivered no branch; pass --branch")?;
    let head = author
        .head
        .ok_or("the attempt recorded no head, so there is nothing to open")?;
    let base = match &command.base {
        Some(base) => base.clone(),
        None => default_branch(project)?,
    };
    if !plain_ref(&branch) || !plain_ref(&base) {
        return Err("branch names must be plain Git names".into());
    }
    let git = Git::new()?;
    let remote = git.remote_of(project, &base)?;
    let base_sha = base_commit(&git, project, &remote, &base)?;
    if !command.dry_run {
        // The body states this head, so refuse a branch that is elsewhere.
        let listed = git
            .full(
                project,
                &["ls-remote", &remote, &format!("refs/heads/{branch}")],
            )?
            .ok_or_else(|| format!("cannot read {branch} on {remote}"))?;
        let remote_head = listed.split_whitespace().next();
        if remote_head != Some(head.to_string().as_str()) {
            return Err(format!(
                "{branch} is at {} on {remote}, not at the attempt's head {head}",
                remote_head.unwrap_or("nothing")
            )
            .into());
        }
    }
    let (title, summary) = pr_body::split_summary(&summary);
    if title.is_empty() {
        return Err("the summary file has no title line".into());
    }
    let body = pr_body::fill(
        &template,
        &Body {
            summary: &summary,
            issues: &command.issue,
            base: &base,
            base_sha: base_sha.as_deref(),
            branch: &branch,
            head,
            author: &author,
            reviews: &reviews,
        },
    )?;
    Ok(Plan {
        title,
        body,
        base,
        branch,
        head,
        remote,
    })
}

pub(super) fn run(command: PrOpen, globals: &Globals) -> u8 {
    const LEAF: &str = "pr open";
    let resolved = match roots::resolve(
        globals,
        roots::Workspace::Checkout,
        roots::Product::Required,
    ) {
        Ok(resolved) => resolved,
        Err(error) => return fail(LEAF, Outcome::Invalid, "PR", &error.to_string(), globals),
    };
    let project = resolved.project.clone();
    let plan = match plan(&command, globals, &project, &resolved) {
        Ok(plan) => plan,
        Err(error) => {
            let code = code_of(error.as_ref(), "PR");
            return fail(LEAF, Outcome::Invalid, code, &error.to_string(), globals);
        }
    };
    let mut data = json!({
        "title": plan.title,
        "base": plan.base,
        "branch": plan.branch,
        "head": plan.head,
        "remote": plan.remote,
        "dry_run": command.dry_run,
        "body": plan.body,
    });
    if command.dry_run {
        let text = format!("title: {}\n\n{}", plan.title, plan.body);
        return emit(
            &CommandResult::new(LEAF, Outcome::Ok, data),
            globals,
            &text,
            true,
        );
    }
    let mut argv = vec![
        "gh",
        "pr",
        "create",
        "--base",
        &plan.base,
        "--head",
        &plan.branch,
        "--title",
        &plan.title,
        "--body-file",
        "-",
    ];
    if command.draft {
        argv.push("--draft");
    }
    let created = execute_with(
        &project,
        &argv,
        CREATE_TIMEOUT,
        false,
        Some(plan.body.as_bytes()),
    )
    .and_then(successful);
    match created {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let url = text.lines().rev().find(|line| !line.trim().is_empty());
            let number = url
                .and_then(|url| url.trim().rsplit('/').next())
                .and_then(|tail| tail.parse::<u64>().ok());
            let captured = number
                .ok_or_else(|| "opened PR but its number could not be read".to_owned())
                .map_err(Into::into)
                .and_then(|number| {
                    super::contributions::Tracker::new(resolved)?.bind(
                        &plan.branch,
                        number,
                        plan.head,
                    )
                });
            if let Err(error) = captured {
                super::contributions::warn(number, &error);
            }
            data["url"] = json!(url.map(str::trim));
            data["number"] = json!(number);
            let line = match (number, url) {
                (Some(number), Some(url)) => format!("opened #{number} {}\n", url.trim()),
                _ => "opened\n".to_owned(),
            };
            emit(
                &CommandResult::new(LEAF, Outcome::Ok, data),
                globals,
                &line,
                false,
            )
        }
        Err(error) => fail(LEAF, Outcome::Error, "PR", &error.to_string(), globals),
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // The PR test driver includes this module without libtest.
    use super::*;

    #[test]
    fn base_commit_uses_bounded_repository_facts() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let git = Git::new()?;
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project)?;
        provenant_os::fixture::run_git(&project, &["init", "-q"])?;
        provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
        provenant_os::fixture::run_git(
            &project,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        )?;
        let expected = String::from_utf8(provenant_os::fixture::run_git(
            &project,
            &["rev-parse", "HEAD"],
        )?)?;
        assert_eq!(
            base_commit(&git, &project, "origin", "main")?,
            Some(expected.trim().to_owned())
        );
        assert_eq!(base_commit(&git, &project, "origin", "missing")?, None);
        provenant_os::fixture::run_git(&project, &["config", "user.name", "Updated"])?;
        assert!(base_commit(&git, &project, "origin", "main").is_err());
        git.invalidate_repositories();
        assert_eq!(
            base_commit(&git, &project, "origin", "main")?,
            Some(expected.trim().to_owned())
        );
        provenant_os::fixture::run_git(
            &project,
            &["update-ref", "refs/remotes/origin/moving", "HEAD"],
        )?;
        let tree = String::from_utf8(provenant_os::fixture::run_git(
            &project,
            &["rev-parse", "HEAD^{tree}"],
        )?)?;
        let moved = String::from_utf8(provenant_os::fixture::run_git(
            &project,
            &["commit-tree", tree.trim(), "-m", "moving ref"],
        )?)?;
        assert_eq!(
            base_commit(&git, &project, "origin", "moving")?,
            Some(expected.trim().to_owned())
        );
        provenant_os::fixture::run_git(
            &project,
            &["update-ref", "refs/remotes/origin/moving", moved.trim()],
        )?;
        assert_eq!(
            base_commit(&git, &project, "origin", "moving")?,
            Some(moved.trim().to_owned())
        );

        provenant_os::fixture::run_git(
            &project,
            &["update-ref", "refs/remotes/origin/équipe", "HEAD"],
        )?;
        assert_eq!(
            base_commit(&git, &project, "origin", "équipe")?,
            Some(expected.trim().to_owned())
        );
        let separate = temp.path().join("separate");
        let gitdir = temp.path().join("separate.git");
        std::fs::create_dir(&separate)?;
        provenant_os::fixture::run_git(
            &separate,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                gitdir.to_str().ok_or("gitdir")?,
            ],
        )?;
        provenant_os::fixture::run_git(&separate, &["commit", "--allow-empty", "-qm", "base"])?;
        provenant_os::fixture::run_git(
            &separate,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        )?;
        let separate_head = String::from_utf8(provenant_os::fixture::run_git(
            &separate,
            &["rev-parse", "HEAD"],
        )?)?;
        assert_eq!(
            base_commit(&git, &separate, "origin", "main")?,
            Some(separate_head.trim().to_owned())
        );
        let (root, private, common) = git.repository_paths(&separate)?;
        assert_eq!(root, std::fs::canonicalize(&separate)?);
        assert_eq!(private, std::fs::canonicalize(&gitdir)?);
        assert_eq!(common, private);
        let shared = temp.path().join("shared");
        provenant_os::fixture::run_git(
            temp.path(),
            &[
                "clone",
                "-q",
                "--shared",
                project.to_str().ok_or("project")?,
                shared.to_str().ok_or("shared")?,
            ],
        )?;
        assert!(shared.join(".git/objects/info/alternates").is_file());
        provenant_os::fixture::run_git(
            &shared,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        )?;
        assert_eq!(
            base_commit(&git, &shared, "origin", "main")?,
            Some(expected.trim().to_owned())
        );
        let bare = temp.path().join("bare.git");
        let linked = temp.path().join("bare-linked");
        provenant_os::fixture::run_git(
            temp.path(),
            &[
                "clone",
                "-q",
                "--bare",
                project.to_str().ok_or("project")?,
                bare.to_str().ok_or("bare")?,
            ],
        )?;
        provenant_os::fixture::run_git(
            &bare,
            &[
                "worktree",
                "add",
                "-qb",
                "linked",
                linked.to_str().ok_or("linked")?,
                "HEAD",
            ],
        )?;
        let listing = git.worktrees(&linked)?;
        assert_eq!(
            listing,
            provenant_os::fixture::run_git(&linked, &["worktree", "list", "--porcelain", "-z"])?
        );
        assert!(
            listing
                .windows(b"bare\0".len())
                .any(|field| field == b"bare\0")
        );
        assert!(
            listing
                .windows(b"branch refs/heads/linked\0".len())
                .any(|field| field == b"branch refs/heads/linked\0")
        );
        std::fs::rename(&project, temp.path().join("moved-project"))?;
        std::fs::create_dir(&project)?;
        assert!(git.repository_paths(&project).is_err());
        Ok(())
    }
}
