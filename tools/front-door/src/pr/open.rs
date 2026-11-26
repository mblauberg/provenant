//! `pr open`: the repository's template filled from the attempts' receipts.
use super::git::{Git, plain_ref};
use super::{Refused, Result, code_of, successful};
use crate::{emit, fail, roots};
use provenant_cli::{
    Globals, PrOpen,
    pr_body::{self, Body},
    run::{AttemptFacts, Edge, attempt_facts, review_attempt_facts},
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

#[derive(Deserialize)]
struct Repository {
    #[serde(rename = "defaultBranchRef")]
    branch: Named,
}
#[derive(Deserialize)]
struct Named {
    name: String,
}

/// The forge API half of context resolution. It is authoritative for the
/// default branch: a local remote `HEAD` symref can be stale after a rename,
/// so the probe is asked before the local symref is trusted.
struct RepositoryForge<'a> {
    state: &'a Path,
    project: &'a Path,
}
impl provenant_config::ForgeProbe for RepositoryForge<'_> {
    fn default_branch(&self, _remote: &str) -> Option<String> {
        let bytes = super::forge_execute(
            self.state,
            self.project,
            &["gh", "repo", "view", "--json", "defaultBranchRef"],
            &provenant_config::ports::Request::within(Duration::from_secs(60)),
            false,
            None,
        )
        .ok()?;
        let bytes = successful(bytes).ok()?;
        serde_json::from_slice::<Repository>(&bytes)
            .ok()
            .map(|repository| repository.branch.name)
    }
}

/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
fn default_branch(state: &Path, project: &Path) -> Result<String> {
    let ports = provenant_config::PortTable::read_for(Some(project), state)?;
    let policy = provenant_config::operation_policy(state, project, &ports);
    let config = provenant_config::ForgeConfig::new(&ports, (*policy).clone());
    let probe = RepositoryForge { state, project };
    // Ask the forge first: it is authoritative for the default branch and
    // ignores the remote, so an ambiguous local remote must not stop the one
    // answer Git metadata cannot give. A renamed default branch also makes the
    // local symref stale, but the forge names the branch the PR must target.
    if let Some(branch) = provenant_config::ForgeProbe::default_branch(&probe, "") {
        return Ok(branch);
    }
    // The forge did not answer. The local symref is the only source left, so
    // resolve the remote it belongs to only now.
    let git = provenant_config::ForgeGit::new(project).map_err(|error| {
        format!("cannot read the checkout's Git metadata for the default branch: {error}")
    })?;
    let remote = provenant_config::ForgeContext::remote(&config, &git)
        .map_err(|error| {
            format!("cannot resolve the forge remote for the default branch: {error}")
        })?
        .value;
    if let Some(branch) = git.remote_head(&remote) {
        provenant_records::warning::warn(
            provenant_records::Warning::new(
                provenant_records::warning::DEFAULT_BASE_FALLBACK,
                format!("the forge did not name a default branch for {remote}; using the local remote HEAD symref"),
             provenant_records::warning::detected_source(provenant_records::warning::DEFAULT_BASE_FALLBACK))
            .once_per_change(),
        );
        return Ok(branch);
    }
    Err(format!(
        "cannot resolve the repository's default branch: neither the forge nor {remote}/HEAD named one"
    )
    .into())
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
        // Opening a pull request is not an authority act: landing still gates
        // on CI and review. The policy table says what backs the body and
        // carries on; a configured strict mode stops here instead.
        provenant_config::policy::read(resolved.roots.state.as_path(), "", &Default::default())
            .warn_or(
                "PR_ATTEMPT_STATE",
                provenant_records::policy::Facts::violation(format!(
                    "attempt {} ended {}, not succeeded or review_required; opening the pull request from its recorded branch and head anyway (source: attempt record)",
                    author.attempt_id, author.state
                )),
                |diagnostic| {
                    Refused::new(
                        "PR_ATTEMPT_STATE",
                        diagnostic.detail.unwrap_or_default(),
                    )
                },
            )?;
    }
    // Reviews named on the command resolve as targets; the receipt's own
    // reviews are already attempt ids.
    let reviews: Vec<AttemptFacts> = if !command.review.is_empty() {
        attempt_facts(&edge, &command.review).map_err(fail)?
    } else if author.reviews.is_empty() {
        Vec::new()
    } else {
        review_attempt_facts(&edge, &author.reviews).map_err(fail)?
    };
    let branch = command
        .branch
        .clone()
        .or_else(|| author.branch.clone())
        .ok_or("the attempt delivered no branch; pass --branch BRANCH, or push the lane branch and retry")?;
    let head = author
        .head
        .ok_or("the attempt recorded no head, so there is nothing to open; open the pull request from an attempt that committed")?;
    let base = match &command.base {
        Some(base) => base.clone(),
        None => default_branch(resolved.roots.state.as_path(), project)?,
    };
    if !plain_ref(&branch) || !plain_ref(&base) {
        return Err("branch names must be plain Git names".into());
    }
    let git = Git::new(resolved.roots.state.as_path())?;
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
        return Err("the summary file has no title line; start it with a one-line title (a Markdown heading is fine)".into());
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
            // The runbook owns merge authority; no decision is outstanding.
            decision: None,
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
        Err(error) => {
            return fail(
                LEAF,
                Outcome::Invalid,
                "PR",
                &error.to_string(),
                crate::REPAIR_ROOT,
                globals,
            );
        }
    };
    let project = resolved.project.clone();
    let plan = match plan(&command, globals, &project, &resolved) {
        Ok(plan) => plan,
        Err(error) => {
            let code = code_of(error.as_ref(), "PR");
            return fail(
                LEAF,
                Outcome::Invalid,
                code,
                &error.to_string(),
                "correct the reported pull request plan error and rerun `provenant pr open`",
                globals,
            );
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
    let created = (|| -> Result<Vec<u8>> {
        let ports =
            provenant_config::PortTable::read_for(Some(&project), resolved.roots.state.as_path())?;
        let policy =
            provenant_config::operation_policy(resolved.roots.state.as_path(), &project, &ports);
        let mut forge = provenant_config::ForgePort::new(
            ports.forge,
            "forge",
            &project,
            resolved.roots.state.as_path(),
            policy,
        )?;
        provenant_confinement::publication::install_forge(
            &mut forge,
            project.clone(),
            provenant_confinement::publication::Projector::ambient(),
        );
        Ok(forge
            .request(
                provenant_config::ForgeOperation::Open {
                    base: plan.base.clone(),
                    head: plan.branch.clone(),
                    title: plan.title.clone(),
                    body: plan.body.as_bytes().to_vec(),
                    draft: command.draft,
                },
                CREATE_TIMEOUT,
            )?
            .body)
    })();
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
        Err(error) => fail(
            LEAF,
            Outcome::Error,
            "PR",
            &error.to_string(),
            "resolve the reported pull request error and rerun `provenant pr open`",
            globals,
        ),
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // The PR test driver includes this module without libtest.
    use super::*;

    #[test]
    fn default_branch_reports_the_real_metadata_failure() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project)?;
        let error = default_branch(&state, &project).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot read the checkout's Git metadata"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn default_branch_asks_the_forge_before_an_ambiguous_remote_is_resolved() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state)?;
        provenant_os::fixture::private_state_dir(&state)?;
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project)?;
        provenant_os::fixture::run_git(&project, &["init", "-q"])?;
        // Two remotes with no declared git.remote make local resolution
        // ambiguous; the forge must still be able to name the default branch.
        for remote in ["alpha", "beta"] {
            provenant_os::fixture::run_git(
                &project,
                &[
                    "remote",
                    "add",
                    remote,
                    &format!("https://example.invalid/{remote}.git"),
                ],
            )?;
        }
        let script = temp.path().join("gh.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"defaultBranchRef\":{\"name\":\"trunk\"}}'\n",
        )?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        let command = serde_json::json!({
            "argv": [script.to_string_lossy()],
            "exit_codes": {"success": [0], "unknown": [75], "other": "refused"}
        });
        let port = serde_json::json!({
            "source": "host",
            "backend": {"kind": "command", "command": command}
        });
        let document = serde_json::json!({
            "schema_version": provenant_config::PORTS_SCHEMA_VERSION,
            "forge": port
        });
        std::fs::write(state.join("ports.json"), serde_json::to_vec(&document)?)?;
        assert_eq!(default_branch(&state, &project)?, "trunk");
        Ok(())
    }

    #[test]
    fn base_commit_uses_bounded_repository_facts() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let git = Git::new(&state)?;
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
        // A forced `core.bare=true` inventory is not itself evidence that an
        // unbound common directory has no project owner.
        provenant_os::fixture::run_git(&bare, &["config", "--local", "--unset", "core.bare"])?;
        git.invalidate_repositories();
        assert!(git.worktrees(&linked).is_err());
        provenant_os::fixture::run_git(&bare, &["config", "--local", "core.bare", "true"])?;
        git.invalidate_repositories();
        let listing = git.worktrees(&linked)?;
        assert!(
            listing
                .windows(b"bare\0".len())
                .any(|field| field == b"bare\0")
        );
        // A linked checkout may write its own config.worktree. It cannot make
        // the common bare repository appear to be a primary checkout.
        provenant_os::fixture::run_git(&bare, &["config", "extensions.worktreeConfig", "true"])?;
        provenant_os::fixture::run_git(&linked, &["config", "--worktree", "core.bare", "false"])?;
        git.invalidate_repositories();
        let listing = git.worktrees(&linked)?;
        assert!(
            listing
                .windows(b"bare\0".len())
                .any(|field| field == b"bare\0")
        );
        // The common config may get its bare setting through a trusted include.
        // A later include in the checkout cannot override that setting, even
        // through a further include outside the checkout.
        let trusted = temp.path().join("trusted-bare.conf");
        let outside = temp.path().join("outside-bare.conf");
        let lane = linked.join("lane-bare.conf");
        std::fs::write(&trusted, "[core]\n bare = true\n")?;
        std::fs::write(&outside, "[core]\n bare = false\n")?;
        std::fs::write(&lane, format!("[include]\n path = {}\n", outside.display()))?;
        provenant_os::fixture::run_git(&bare, &["config", "--local", "--unset", "core.bare"])?;
        provenant_os::fixture::run_git(
            &bare,
            &[
                "config",
                "--local",
                "--add",
                "include.path",
                trusted.to_str().ok_or("trusted path")?,
            ],
        )?;
        provenant_os::fixture::run_git(
            &bare,
            &[
                "config",
                "--local",
                "--add",
                "include.path",
                lane.to_str().ok_or("lane path")?,
            ],
        )?;
        let private_config = String::from_utf8(provenant_os::fixture::run_git(
            &linked,
            &["rev-parse", "--git-path", "config.worktree"],
        )?)?;
        provenant_os::fixture::run_git(
            &bare,
            &[
                "config",
                "--local",
                "--add",
                "include.path",
                private_config.trim(),
            ],
        )?;
        #[cfg(unix)]
        {
            // A lane path can itself be a symlink to an outside file.
            let lane_alias = linked.join("lane-config-alias");
            let outside_alias = temp.path().join("outside-alias.conf");
            std::fs::write(&outside_alias, "[core]\n bare = false\n")?;
            std::os::unix::fs::symlink(&outside_alias, &lane_alias)?;
            provenant_os::fixture::run_git(
                &bare,
                &[
                    "config",
                    "--local",
                    "--add",
                    "include.path",
                    lane_alias.to_str().ok_or("lane alias")?,
                ],
            )?;
            let alias = temp.path().join("private-config-alias");
            std::os::unix::fs::symlink(private_config.trim(), &alias)?;
            provenant_os::fixture::run_git(
                &bare,
                &[
                    "config",
                    "--local",
                    "--add",
                    "include.path",
                    alias.to_str().ok_or("alias")?,
                ],
            )?;
        }
        git.invalidate_repositories();
        let listing = git.worktrees(&linked)?;
        assert!(
            listing
                .windows(b"bare\0".len())
                .any(|field| field == b"bare\0")
        );
        #[cfg(unix)]
        {
            // If a lane alias names the trusted bare file, the origins may be
            // indistinguishable. Do not report a non-bare primary checkout.
            let ambiguous = linked.join("trusted-config-alias");
            std::os::unix::fs::symlink(&trusted, &ambiguous)?;
            provenant_os::fixture::run_git(
                &bare,
                &[
                    "config",
                    "--local",
                    "--add",
                    "include.path",
                    ambiguous.to_str().ok_or("ambiguous alias")?,
                ],
            )?;
            git.invalidate_repositories();
            assert!(git.worktrees(&linked).is_err());
        }
        assert_eq!(
            base_commit(&git, &project, "origin", "main")?,
            Some(expected.trim().to_owned())
        );
        std::fs::rename(&project, temp.path().join("moved-project"))?;
        std::fs::create_dir(&project)?;
        assert!(git.repository_paths(&project).is_err());
        Ok(())
    }
}
