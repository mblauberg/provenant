//! `pr sync`: bring a pull request's base into its branch and publish it.
use super::{Result, observer::Observer};
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_config::ports::Request;
use provenant_core::Clock;
use provenant_records::{CommandResult, Outcome};
use serde::Deserialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Deserialize)]
struct Pull {
    state: String,
    head: Branch,
    base: Branch,
}
#[derive(Deserialize)]
struct Branch {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<Repository>,
}
#[derive(Deserialize)]
struct Repository {
    full_name: String,
}

pub(crate) fn run(numbers: Vec<u64>, repair: bool, globals: &Globals) -> u8 {
    let project = match roots::selected_project(globals.project.as_deref(), true) {
        Ok(project) => project,
        Err(error) => {
            return fail(
                "pr sync",
                Outcome::Error,
                "PR_SYNC",
                &error.to_string(),
                globals,
            );
        }
    };
    let resolved =
        match roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional) {
            Ok(resolved) => resolved,
            Err(error) => {
                return fail(
                    "pr sync",
                    Outcome::Error,
                    "PR_SYNC",
                    &error.to_string(),
                    globals,
                );
            }
        };
    let request = Request::within(Duration::from_secs(60));
    let (project_id, layout_root) = match bound_project(&resolved, &project) {
        Ok(binding) => binding,
        Err(error) => {
            return fail(
                "pr sync",
                Outcome::Error,
                "PR_SYNC",
                &error.to_string(),
                globals,
            );
        }
    };
    let repository =
        match super::status::wait_repository(resolved.roots.state.as_path(), &project, &request) {
            Ok(repository) => repository,
            Err(error) => {
                return fail(
                    "pr sync",
                    Outcome::Error,
                    "PR_SYNC",
                    &error.to_string(),
                    globals,
                );
            }
        };
    let mut git = match super::git::Git::new(resolved.roots.state.as_path()) {
        Ok(git) => git,
        Err(error) => {
            return fail(
                "pr sync",
                Outcome::Error,
                "PR_SYNC",
                &error.to_string(),
                globals,
            );
        }
    };
    let mut results = Vec::new();
    let mut lines = String::new();
    let mut errors = Vec::new();
    for number in numbers {
        let result = sync_one(
            number,
            repair,
            &project,
            &layout_root,
            resolved.roots.state.as_path(),
            project_id,
            &repository,
            &request,
            &mut git,
        );
        match result {
            Ok((path, approved_head, current_head)) => {
                lines.push_str(&format!(
                    "synced #{number} {approved_head} -> {current_head} ({})\n",
                    path.display()
                ));
                results.push(serde_json::json!({"number": number, "worktree": path, "approved_head": approved_head, "current_head": current_head, "result": "synced"}));
            }
            Err(error) => {
                let message = error.to_string();
                lines.push_str(&format!("failed #{number}: {message}\n"));
                errors.push(format!("#{number}: {message}"));
                results.push(
                    serde_json::json!({"number": number, "result": "failed", "error": message}),
                );
            }
        }
    }
    let outcome = if errors.is_empty() {
        Outcome::Ok
    } else {
        Outcome::Error
    };
    let mut report = CommandResult::new("pr sync", outcome, serde_json::json!({"pulls": results}));
    if !errors.is_empty() {
        report.error = Some(provenant_records::CommandError {
            diagnostic: None,
            code: "PR_SYNC".into(),
            message: errors.join("; "),
            cause: None,
            repair: None,
        });
    }
    emit(&report, globals, &lines, true)
}

#[allow(clippy::too_many_arguments)] // Keep sync inputs explicit at the command boundary.
fn sync_one(
    number: u64,
    repair: bool,
    project: &Path,
    layout_root: &Path,
    state: &Path,
    project_id: provenant_core::ProjectId,
    repository: &str,
    request: &Request,
    git: &mut super::git::Git,
) -> Result<(PathBuf, String, String)> {
    let mut observer = Observer::open(state, project, repository, request)?;
    let value = observer.resource(&format!("pulls/{number}"), Duration::ZERO, request)?;
    let pull: Pull = serde_json::from_value(value)?;
    if pull.state != "open" {
        return Err(format!("pull request is {}", pull.state).into());
    }
    if !super::git::plain_ref(&pull.head.name) || !super::git::plain_ref(&pull.base.name) {
        return Err("pull request contains an unsafe branch name".into());
    }
    let head_remote = match pull.head.repo.as_ref() {
        Some(head_repo) if head_repo.full_name.eq_ignore_ascii_case(repository) => {
            Some(git.remote_of(project, &pull.base.name)?)
        }
        Some(head_repo) => Some(format!("https://github.com/{}.git", head_repo.full_name)),
        None => None,
    };
    let path = branch_worktree(
        project,
        layout_root,
        state,
        &pull.head.name,
        &pull.head.sha,
        head_remote.as_deref(),
        git,
    )?;
    let mut writer = Some(acquire_writer(
        project,
        state,
        project_id,
        &path,
        &pull.head.name,
        &pull.head.sha,
        git,
    )?);
    let operation = (|| -> Result<(PathBuf, String, String)> {
        let observed = git
            .full(&path, &["rev-parse", "HEAD"])?
            .ok_or("cannot resolve pull request head")?;
        if observed.trim() != pull.head.sha {
            return Err(format!(
            "worktree branch {} is at {}, forge head is {}; synchronize the branch before retrying",
            pull.head.name,
            observed.trim(),
            pull.head.sha
        )
        .into());
        }
        let dirty = git
            .full(&path, &["status", "--porcelain"])?
            .ok_or("cannot verify pull request worktree cleanliness")?;
        if !dirty.trim().is_empty() {
            return Err(format!(
                "pull request worktree is dirty; preserving its changes: {}",
                dirty.trim()
            )
            .into());
        }
        let base_remote = match pull.base.repo.as_ref() {
            Some(base_repo) if base_repo.full_name.eq_ignore_ascii_case(repository) => {
                git.remote_of(&path, &pull.base.name)?
            }
            Some(base_repo) => format!("https://github.com/{}.git", base_repo.full_name),
            None => format!("https://github.com/{repository}.git"),
        };
        let base_ref = format!("refs/provenant/pr-sync/{number}/base");
        let base_refspec = format!("+refs/heads/{}:{base_ref}", pull.base.name);
        if !git
            .run(&path, &["fetch", "--no-tags", &base_remote, &base_refspec])?
            .0
        {
            return Err(format!(
                "could not fetch base branch {} from {base_remote}",
                pull.base.name,
            )
            .into());
        }
        let approved_head = git
            .full(&path, &["rev-parse", "HEAD"])?
            .ok_or("cannot resolve pull request head")?
            .trim()
            .to_owned();
        if !git
            .run(&path, &["merge", "--no-edit", "--no-ff", &base_ref])?
            .0
        {
            let paths = git
                .full(&path, &["diff", "--name-only", "--diff-filter=U"])?
                .ok_or("cannot read conflicted paths")?;
            let base_head = git
                .full(&path, &["rev-parse", &base_ref])?
                .ok_or("cannot resolve the fetched base")?
                .trim()
                .to_owned();
            if !git.run(&path, &["merge", "--abort"])?.0 {
                return Err(format!(
                    "merge conflict cleanup failed; MERGE_HEAD may remain; conflicted paths: {}",
                    paths.lines().collect::<Vec<_>>().join(", ")
                )
                .into());
            }
            let (merge_head_code, _, merge_head_error) =
                git.exit(&path, &["rev-parse", "--verify", "-q", "MERGE_HEAD"])?;
            if merge_head_code == 0 {
                return Err("merge conflict cleanup left MERGE_HEAD in place".into());
            }
            if !matches!(merge_head_code, 1 | 128) {
                return Err(format!("cannot verify MERGE_HEAD cleanup: {merge_head_error}").into());
            }
            let status = git
                .full(&path, &["status", "--porcelain"])?
                .ok_or("cannot verify worktree cleanliness after merge abort")?;
            if !status.trim().is_empty() {
                return Err(format!(
                    "merge conflict cleanup left a dirty worktree: {}",
                    status.trim()
                )
                .into());
            }
            let paths = paths.lines().collect::<Vec<_>>().join(", ");
            let brief = format!(
                "Merge base {base_ref} at {base_head} into this pull request branch, resolve the conflicts in {paths}, commit the resolution, and leave the worktree clean."
            );
            let next = format!(
                "provenant run implement --worktree {} --brief {}",
                shell_quote(&path),
                shell_quote_text(&brief)
            );
            if repair {
                writer
                    .take()
                    .ok_or("pull request writer custody was already released")?
                    .withdraw_unlaunched()?;
                launch_repair(project, &path, &base_ref, &base_head, &paths)?;
                return Err(format!("merge conflict in {paths}; implement lane launched for {}; rerun `provenant pr sync {number}` after it finishes", path.display()).into());
            }
            return Err(format!("merge conflict in {paths}; next: {next}").into());
        }
        let current_head = git
            .full(&path, &["rev-parse", "HEAD"])?
            .ok_or("cannot resolve synchronized head")?
            .trim()
            .to_owned();
        let base_head = git
            .full(&path, &["rev-parse", &base_ref])?
            .ok_or("cannot resolve fetched base")?
            .trim()
            .to_owned();
        let remote_branch = format!("{}:{}", pull.head.name, pull.head.name);
        let push_remote = match pull.head.repo.as_ref() {
            Some(head_repo) if head_repo.full_name.eq_ignore_ascii_case(repository) => {
                git.remote_of(&path, &pull.head.name)?
            }
            Some(head_repo) => format!("https://github.com/{}.git", head_repo.full_name),
            None => git.remote_of(&path, &pull.head.name)?,
        };
        if !git.run(&path, &["push", &push_remote, &remote_branch])?.0 {
            return Err("push of synchronized pull request branch failed".into());
        }
        super::landing::receipt::write_approval_carry(
            state,
            &super::landing::receipt::ApprovalCarryReceipt {
                repository: repository.to_owned(),
                number,
                approved_head: approved_head.clone(),
                current_head: current_head.clone(),
                base_head,
                kind: "clean_merge".into(),
                extra: Default::default(),
            },
        )?;
        Ok((path, approved_head, current_head))
    })();
    let cleanup = match writer.take() {
        Some(writer) => writer.withdraw_unlaunched(),
        None => Ok(()),
    };
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup.into()),
        (Err(error), Err(cleanup)) => {
            Err(format!("{error}; pull request writer custody cleanup failed: {cleanup}").into())
        }
    }
}

fn launch_repair(
    project: &Path,
    worktree: &Path,
    base_ref: &str,
    base_head: &str,
    conflicts: &str,
) -> Result<()> {
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .ok_or("provenant executable path is not UTF-8")?;
    let worktree = worktree.to_str().ok_or("worktree path is not UTF-8")?;
    let brief = format!(
        "Merge {base_ref} at {base_head} into this pull request worktree, resolve conflicted paths {conflicts}, commit the resolution, and leave the worktree clean."
    );
    let argv = [
        executable,
        "run",
        "implement",
        "--worktree",
        worktree,
        "--brief",
        &brief,
    ];
    let outcome = super::execute_for(project, &argv, Duration::from_secs(4 * 60 * 60), false)?;
    if outcome.termination != provenant_os::exec::Termination::Exit(0) {
        return Err("implement lane did not complete successfully".into());
    }
    Ok(())
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn shell_quote_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn branch_worktree(
    project: &Path,
    layout_root: &Path,
    state: &Path,
    branch: &str,
    head: &str,
    head_remote: Option<&str>,
    git: &mut super::git::Git,
) -> Result<PathBuf> {
    let listing = git
        .full(project, &["worktree", "list", "--porcelain"])?
        .ok_or("cannot list project worktrees")?;
    let target_branch = format!("branch refs/heads/{branch}");
    let mut path = None;
    let mut current = None;
    for line in listing.lines() {
        if let Some(value) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(value));
        }
        if line == target_branch {
            path = current.clone();
        }
    }
    if let Some(path) = path {
        return Ok(path);
    }
    let ports = provenant_config::PortTable::read_for(Some(layout_root), state)?;
    let relative = ports.managed_writer_path(branch)?;
    if !relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err("configured worktree path is not project relative".into());
    }
    let layout_root = std::fs::canonicalize(layout_root)?;
    let configured = layout_root.join(relative);
    let parent = configured
        .parent()
        .ok_or("invalid configured worktree path")?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or("configured worktree path has no existing ancestor")?;
    }
    if !std::fs::canonicalize(existing)?.starts_with(&layout_root) {
        return Err("configured worktree path resolves outside the project root".into());
    }
    std::fs::create_dir_all(parent)?;
    let parent = std::fs::canonicalize(parent)?;
    if !parent.starts_with(&layout_root) {
        return Err("configured worktree path resolves outside the project root".into());
    }
    let path = parent.join(
        configured
            .file_name()
            .ok_or("invalid configured worktree path")?,
    );
    let local = format!("refs/heads/{branch}");
    let branch_exists = git
        .full(project, &["show-ref", "--verify", "--quiet", &local])?
        .is_some();
    if !branch_exists {
        let remote = head_remote.ok_or("pull request head repository is unavailable")?;
        let refspec = format!("+refs/heads/{branch}:refs/remotes/provenant-pr/{branch}");
        if !git
            .run(project, &["fetch", "--no-tags", remote, &refspec])?
            .0
        {
            return Err(format!("cannot fetch pull request branch {branch}").into());
        }
        let fetched = format!("refs/remotes/provenant-pr/{branch}");
        let fetched_head = git
            .full(project, &["rev-parse", &fetched])?
            .ok_or("fetched pull request head is missing")?;
        if fetched_head.trim() != head {
            return Err("fetched pull request head moved during sync setup".into());
        }
        let args = vec![
            "worktree",
            "add",
            "--branch",
            branch,
            path.to_str().ok_or("worktree path is not UTF-8")?,
            &fetched,
        ];
        if !git.run(project, &args)?.0 {
            return Err(format!("cannot create branch worktree at {}", path.display()).into());
        }
        return Ok(path);
    }
    let args = vec![
        "worktree",
        "add",
        path.to_str().ok_or("worktree path is not UTF-8")?,
        branch,
    ];
    if !git.run(project, &args)?.0 {
        return Err(format!("cannot create branch worktree at {}", path.display()).into());
    }
    Ok(path)
}

fn bound_project(
    resolved: &roots::Resolved,
    project: &Path,
) -> Result<(provenant_core::ProjectId, PathBuf)> {
    let git = provenant_os::git::git_executable().ok_or("Git executable is unavailable")?;
    let runtime =
        provenant_confinement::UtilityRuntime::read_only(git, resolved.roots.state.as_path())?;
    let location = runtime
        .project_location(
            &provenant_os::exec::SystemExecutor::new(1)?,
            project,
            provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
            &provenant_core::CancelToken::new(),
        )?
        .location;
    let binding = provenant_config::lookup_project(&resolved.roots, &location)?
        .ok_or("project is unbound; register it before synchronizing a pull request")?;
    let layout = roots::project_layout_read_only(&resolved.roots, project)?;
    let layout_root = layout
        .project()
        .ok_or("registered project has no verified primary checkout")?
        .to_path_buf();
    Ok((binding.project_id(), layout_root))
}

fn child_directory(
    parent: &provenant_os::fs::Directory,
    name: &OsStr,
) -> Result<provenant_os::fs::Directory> {
    match parent.open_verified_directory(name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.is_not_found() => {
            parent.mkdir(name, 0o700)?;
            Ok(parent.open_verified_directory(name)?)
        }
        Err(error) => Err(error.into()),
    }
}

fn acquire_writer(
    project: &Path,
    state: &Path,
    project_id: provenant_core::ProjectId,
    worktree: &Path,
    branch: &str,
    head: &str,
    git: &super::git::Git,
) -> Result<provenant_supervisor::custody::CustodyLease> {
    let cancel = provenant_core::CancelToken::new();
    let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(10))?;
    let executable = provenant_os::git::git_executable().ok_or("Git executable is unavailable")?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(executable, state)?;
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let repo = runtime.open_repo(&executor, project, budget, &cancel)?;
    let verified = provenant_confinement::repo::verify_worktree(&repo, worktree)?;
    let state_root = provenant_os::fs::Directory::open_anchor(state)?;
    let worktrees = child_directory(&state_root, OsStr::new("worktrees"))?;
    let project_state = child_directory(&worktrees, OsStr::new(&project_id.to_string()))?;
    let listing = git
        .full(project, &["worktree", "list", "--porcelain"])?
        .ok_or("cannot read registered worktrees")?
        .into_bytes();
    let branch = provenant_confinement::repo::GitRefName::branch(branch)?;
    let head = provenant_core::GitOid::parse(head)?;
    let (slot, binding) = provenant_confinement::worktrees::writer_custody(
        &project_state,
        &verified,
        worktree,
        &branch,
        head,
        || Ok(listing),
    )?;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let clock = provenant_os::clock::SystemClock::new();
    let now = provenant_core::Clock::wall_now(&clock)?.as_micros();
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&(now as u64).to_be_bytes());
    id[8..12].copy_from_slice(&std::process::id().to_be_bytes());
    id[12..].copy_from_slice(&(sequence as u32).to_be_bytes());
    let scope = provenant_records::custody::Scope {
        attempt: provenant_core::AttemptId::from_bytes(id),
        generation: 1,
    };
    provenant_supervisor::custody::acquire_writer(
        slot,
        scope,
        binding,
        provenant_core::WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )
    .map_err(|error| format!("writer custody: {error:?}").into())
}
