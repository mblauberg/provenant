//! `pr sync`: bring a pull request's base into its branch and publish it.
use super::{Result, observer::Observer};
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_config::ports::Request;
use provenant_core::Clock;
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::Duration,
};

const REPAIR_SCHEMA: u8 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RepairOperation {
    schema_version: u8,
    repository: String,
    number: u64,
    head_branch: String,
    head_repository: Option<String>,
    base_branch: String,
    base_repository: Option<String>,
    observed_head: String,
    base_head: String,
}

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

fn repair_key(repository: &str) -> String {
    Sha256::digest(repository.to_ascii_lowercase().as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn repair_path(state: &Path, repository: &str, number: u64) -> PathBuf {
    state
        .join("pr-sync")
        .join("repairs")
        .join(format!("{}-{number}.json", repair_key(repository)))
}

fn repository_name(repository: Option<&Repository>) -> Option<String> {
    repository.map(|repository| repository.full_name.to_ascii_lowercase())
}

fn read_repair(state: &Path, repository: &str, number: u64) -> Result<Option<RepairOperation>> {
    let path = repair_path(state, repository, number);
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!("cannot read pull request repair operation: {error}").into());
        }
    };
    let operation: RepairOperation =
        provenant_records::decode(file, provenant_records::Limits::default())?;
    if operation.schema_version != REPAIR_SCHEMA
        || operation.repository != repository.to_ascii_lowercase()
        || operation.number != number
    {
        return Err("pull request repair operation identity or schema mismatch".into());
    }
    Ok(Some(operation))
}

fn write_repair(state: &Path, operation: &RepairOperation) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(operation)?;
    super::landing::receipt::atomic_write(
        &repair_path(state, &operation.repository, operation.number),
        &bytes,
    )?;
    Ok(())
}

fn remove_repair(state: &Path, repository: &str, number: u64) -> Result<()> {
    let path = repair_path(state, repository, number);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            if let Some(parent) = path.parent()
                && let Ok(directory) = std::fs::File::open(parent)
            {
                let _ = directory.sync_all();
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(format!("cannot remove completed pull request repair operation: {error}").into())
        }
    }
}

fn repair_matches_pull(operation: &RepairOperation, repository: &str, pull: &Pull) -> bool {
    operation.repository == repository.to_ascii_lowercase()
        && operation.head_branch == pull.head.name
        && operation.head_repository == repository_name(pull.head.repo.as_ref())
        && operation.base_branch == pull.base.name
        && operation.base_repository == repository_name(pull.base.repo.as_ref())
}

fn is_repair_commit(
    git: &super::git::Git,
    path: &Path,
    head: &str,
    operation: &RepairOperation,
) -> Result<bool> {
    let parents = git
        .full(path, &["rev-list", "--parents", "-n", "1", head])?
        .ok_or("cannot inspect pull request repair commit")?;
    let mut fields = parents.split_whitespace();
    Ok(fields.next() == Some(head)
        && fields.next() == Some(operation.observed_head.as_str())
        && fields.next() == Some(operation.base_head.as_str())
        && fields.next().is_none())
}

pub(crate) fn run(numbers: Vec<u64>, no_lane: bool, globals: &Globals) -> u8 {
    let project = match roots::selected_project(globals.project.as_deref(), true) {
        Ok(project) => project,
        Err(error) => {
            return fail(
                "pr sync",
                Outcome::Error,
                "PR_SYNC",
                &error.to_string(),
                crate::REPAIR_ROOT,
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
                    crate::REPAIR_ROOT,
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
                "register this project's binding, then rerun `provenant pr sync`",
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
                    "wait for the repository state to settle, then rerun `provenant pr sync`",
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
                crate::REPAIR_IO,
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
            no_lane,
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
        report.error = Some(provenant_records::CommandError::new(
            "PR_SYNC",
            errors.join("; "),
            None,
            "resolve the reported per-PR sync errors and rerun `provenant pr sync`",
        ));
    }
    emit(&report, globals, &lines, true)
}

fn remote_url_for_repo(
    git: &mut super::git::Git,
    dir: &Path,
    branch: &str,
    target_repo: &str,
) -> Result<String> {
    let remote_name = git.remote_of(dir, branch)?;
    let config_key = format!("remote.{remote_name}.url");
    let configured_url = git
        .config_full(
            dir,
            "read remote URL configuration",
            Some("remote URL"),
            provenant_os::git_config::Read::Get(&config_key),
        )?
        .map(|s| s.trim().to_owned());
    if let Some(url) = configured_url {
        if let Some((prefix, _)) = url.rsplit_once('/')
            && let Some((owner_prefix, _)) = prefix.rsplit_once([':', '/'])
        {
            let sep = if prefix.contains(':') && !prefix.contains("://") {
                ":"
            } else {
                "/"
            };
            return Ok(format!("{owner_prefix}{sep}{target_repo}.git"));
        }
        return Ok(url);
    }
    Ok(format!("https://github.com/{target_repo}.git"))
}

#[allow(clippy::too_many_arguments)] // Keep sync inputs explicit at the command boundary.
fn sync_one(
    number: u64,
    no_lane: bool,
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
        Some(head_repo) => Some(remote_url_for_repo(
            git,
            project,
            &pull.base.name,
            &head_repo.full_name,
        )?),
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
    let mut writer = acquire_writer(
        project,
        layout_root,
        state,
        project_id,
        &path,
        &pull.head.name,
        &pull.head.sha,
        git,
    )?;
    let operation = (|| -> Result<(PathBuf, String, String)> {
        let local_head = git
            .full(&path, &["rev-parse", "HEAD"])?
            .ok_or("cannot resolve pull request head")?
            .trim()
            .to_owned();
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

        let mut repair = read_repair(state, repository, number)?;
        if let Some(operation) = repair.as_ref()
            && !repair_matches_pull(operation, repository, &pull)
        {
            return Err("pull request no longer matches its recorded conflict repair".into());
        }
        let base_ref = format!("refs/provenant/pr-sync/{number}/base");
        let (approved_head, base_head, merge_base) = if let Some(operation) = repair.as_ref() {
            if !is_repair_commit(git, &path, &local_head, operation)?
                && local_head != operation.observed_head
            {
                return Err(format!(
                    "worktree branch {} moved outside its recorded conflict repair",
                    pull.head.name
                )
                .into());
            }
            if pull.head.sha != operation.observed_head
                && !(is_repair_commit(git, &path, &local_head, operation)?
                    && pull.head.sha == local_head)
            {
                return Err(format!(
                    "forge head {} moved outside the recorded conflict repair from {}",
                    pull.head.sha, operation.observed_head
                )
                .into());
            }
            let pinned_base = git
                .full(&path, &["rev-parse", &base_ref])?
                .ok_or("recorded pull request repair base is missing")?
                .trim()
                .to_owned();
            if pinned_base != operation.base_head {
                return Err("recorded pull request repair base moved".into());
            }
            (
                operation.observed_head.clone(),
                operation.base_head.clone(),
                local_head == operation.observed_head,
            )
        } else {
            if local_head != pull.head.sha {
                return Err(format!(
                    "worktree branch {} is at {}, forge head is {}; synchronise the branch before retrying",
                    pull.head.name, local_head, pull.head.sha
                )
                .into());
            }
            let base_remote = match pull.base.repo.as_ref() {
                Some(base_repo) if base_repo.full_name.eq_ignore_ascii_case(repository) => {
                    git.remote_of(&path, &pull.base.name)?
                }
                Some(base_repo) => {
                    remote_url_for_repo(git, &path, &pull.base.name, &base_repo.full_name)?
                }
                None => git.remote_of(&path, &pull.base.name)?,
            };
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
            let base_head = git
                .full(&path, &["rev-parse", &base_ref])?
                .ok_or("cannot resolve fetched base")?
                .trim()
                .to_owned();
            (local_head.clone(), base_head, true)
        };

        let current_head = if merge_base {
            if !git
                .run(&path, &["merge", "--no-edit", "--no-ff", &base_ref])?
                .0
            {
                let paths = git
                    .full(&path, &["diff", "--name-only", "--diff-filter=U"])?
                    .ok_or("cannot read conflicted paths")?;
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
                    return Err(
                        format!("cannot verify MERGE_HEAD cleanup: {merge_head_error}").into(),
                    );
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
                let conflicts = paths.lines().collect::<Vec<_>>().join(", ");
                let operation = repair.get_or_insert_with(|| RepairOperation {
                    schema_version: REPAIR_SCHEMA,
                    repository: repository.to_ascii_lowercase(),
                    number,
                    head_branch: pull.head.name.clone(),
                    head_repository: repository_name(pull.head.repo.as_ref()),
                    base_branch: pull.base.name.clone(),
                    base_repository: repository_name(pull.base.repo.as_ref()),
                    observed_head: approved_head.clone(),
                    base_head: base_head.clone(),
                });
                write_repair(state, operation)?;
                let brief = format!(
                    "Merge base {base_ref} at {base_head} into this pull request branch, resolve the conflicts in {conflicts}, commit the resolution, and leave the worktree clean."
                );
                let next = format!(
                    "provenant run implement --worktree {} --brief {}",
                    shell_quote(&path),
                    shell_quote_text(&brief)
                );
                if !no_lane {
                    if let Some(held) = writer.take() {
                        held.withdraw_unlaunched()?;
                    }
                    launch_repair(project, &path, &base_ref, &base_head, &conflicts)?;
                    return Err(format!("merge conflict in {conflicts}; implement lane launched for {}; rerun `provenant pr sync {number}` after it finishes", path.display()).into());
                }
                return Err(format!("merge conflict in {conflicts}; next: {next}").into());
            }
            git.full(&path, &["rev-parse", "HEAD"])?
                .ok_or("cannot resolve synchronised head")?
                .trim()
                .to_owned()
        } else {
            local_head
        };

        if let Some(operation) = repair.as_ref()
            && !is_repair_commit(git, &path, &current_head, operation)?
        {
            return Err("pull request resolution is not the recorded two-parent merge".into());
        }
        let push_remote = match pull.head.repo.as_ref() {
            Some(head_repo) if head_repo.full_name.eq_ignore_ascii_case(repository) => {
                git.remote_of(&path, &pull.head.name)?
            }
            Some(head_repo) => {
                remote_url_for_repo(git, &path, &pull.head.name, &head_repo.full_name)?
            }
            None => git.remote_of(&path, &pull.head.name)?,
        };
        let push = push_args(&pull.head.name, &pull.head.sha, &push_remote, &current_head);
        if !git
            .run(&path, &push.iter().map(String::as_str).collect::<Vec<_>>())?
            .0
        {
            return Err("push of synchronised pull request branch failed".into());
        }
        if repair.is_some() {
            remove_repair(state, repository, number)?;
        } else {
            super::landing::receipt::write_approval_carry(
                state,
                &super::landing::receipt::ApprovalCarryReceipt {
                    repository: repository.to_owned(),
                    number,
                    approved_head: approved_head.clone(),
                    current_head: current_head.clone(),
                    base_head,
                    kind: provenant_cli::domains::ApprovalCarry::CleanMerge,
                    extra: Default::default(),
                },
            )?;
        }
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

/// Arguments for publishing the validated head. The source is the captured
/// object id, never the branch name, so a local ref that moves after validation
/// cannot change what is pushed; the lease pins the remote tip we observed.
fn push_args(branch: &str, expected_remote: &str, remote: &str, head: &str) -> Vec<String> {
    vec![
        "push".into(),
        format!("--force-with-lease=refs/heads/{branch}:{expected_remote}"),
        remote.into(),
        format!("{head}:refs/heads/{branch}"),
    ]
}

fn launch_repair(
    project: &Path,
    worktree: &Path,
    base_ref: &str,
    base_head: &str,
    conflicts: &str,
) -> Result<()> {
    let executable = std::env::var("PROVENANT_BIN")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_exe())?;
    let executable_str = executable
        .to_str()
        .ok_or("the running executable path is not UTF-8")?;
    let worktree_str = worktree.to_str().ok_or("worktree path is not UTF-8")?;
    let brief = format!(
        "Merge {base_ref} at {base_head} into this pull request worktree, resolve conflicted paths {conflicts}, commit the resolution, and leave the worktree clean."
    );
    #[allow(
        clippy::disallowed_methods,
        reason = "launch background repair lane subprocess"
    )]
    let mut command = std::process::Command::new(executable_str);
    command
        .args([
            "run",
            "implement",
            "--worktree",
            worktree_str,
            "--brief",
            &brief,
        ])
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
        .spawn()
        .map_err(|error| format!("failed to launch repair lane: {error}"))?;
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
        .ok_or("project is unbound; register it before synchronising a pull request")?;
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

#[allow(clippy::too_many_arguments)]
fn acquire_writer(
    project: &Path,
    layout_root: &Path,
    state: &Path,
    project_id: provenant_core::ProjectId,
    worktree: &Path,
    branch: &str,
    head: &str,
    git: &super::git::Git,
) -> Result<Option<provenant_supervisor::custody::CustodyLease>> {
    let canonical_worktree = std::fs::canonicalize(worktree)?;
    let canonical_layout = std::fs::canonicalize(layout_root)?;
    if canonical_worktree == canonical_layout {
        return Ok(None);
    }
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
        .full(project, &["worktree", "list", "--porcelain", "-z"])?
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
    let lease = provenant_supervisor::custody::acquire_writer(
        slot,
        scope,
        binding,
        provenant_core::WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )
    .map_err(|error| format!("writer custody: {error:?}"))?;
    Ok(Some(lease))
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod tests {
    use super::push_args;
    use provenant_os::exec::Termination;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) -> String {
        let exe = provenant_os::git::git_executable().unwrap();
        let mut argv = vec![
            exe.to_str().unwrap(),
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
        ];
        argv.extend_from_slice(args);
        let output =
            super::super::execute_with(dir, &argv, super::super::git::GIT_TIMEOUT, false, None)
                .unwrap();
        assert_eq!(output.termination, Termination::Exit(0), "{args:?}");
        String::from_utf8(output.stdout.bytes)
            .unwrap()
            .trim()
            .to_owned()
    }

    #[test]
    fn push_source_is_the_validated_oid_with_a_qualified_destination() {
        let args = push_args("feature", "aaaa", "origin", "bbbb");
        assert_eq!(
            args,
            [
                "push",
                "--force-with-lease=refs/heads/feature:aaaa",
                "origin",
                "bbbb:refs/heads/feature"
            ]
        );
    }

    #[test]
    fn local_branch_moving_after_validation_never_reaches_the_remote() {
        let root = tempfile::tempdir().unwrap();
        let bare = root.path().join("remote.git");
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(
            root.path(),
            &["init", "-q", "--bare", bare.to_str().unwrap()],
        );
        git(&work, &["init", "-q", "-b", "feature"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let observed = git(&work, &["rev-parse", "HEAD"]);
        git(&work, &["push", "-q", bare.to_str().unwrap(), "feature"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "validated"]);
        let validated = git(&work, &["rev-parse", "HEAD"]);

        // The local branch moves after validation and before the push resolves.
        let args = push_args("feature", &observed, bare.to_str().unwrap(), &validated);
        git(
            &work,
            &["commit", "-q", "--allow-empty", "-m", "unvalidated"],
        );
        let moved = git(&work, &["rev-parse", "feature"]);
        assert_ne!(moved, validated);

        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git(&work, &args);
        assert_eq!(git(&bare, &["rev-parse", "refs/heads/feature"]), validated);
    }
}
