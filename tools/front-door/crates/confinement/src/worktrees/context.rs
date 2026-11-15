//! Shared local lifecycle context for dispatch and operator commands.
use super::layout::{ProjectLayout, canonical_inventory_path, verified_primary_from_inventory};
use super::*;
use provenant_core::{CancelToken, Roots};
use provenant_os::exec::SystemExecutor;
use provenant_os::fs::Directory;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

/// A verified Git command anchor paired with the stable layout owner.
pub struct OpenProjectRepo {
    /// Verified checkout used for Git reads and mutations. A bare-backed
    /// repository uses the caller's registered linked checkout here.
    pub repo: VerifiedRepo,
    /// Stable owner for managed paths; its project config source may be absent.
    pub layout: ProjectLayout,
}

pub fn with_context<T>(
    roots: &Roots,
    home: &Path,
    project: &Path,
    clock: &(dyn Clock + Sync),
    cancel: &CancelToken,
    action: impl FnOnce(&WorktreeContext<'_>) -> Result<T, WorktreeError>,
) -> Result<T, WorktreeError> {
    let facts = RepoFacts::default();
    with_context_using_facts(roots, home, project, clock, cancel, &facts, action)
}

/// Use the ordinary command census with an existing facts session.
pub fn with_context_using_facts<T>(
    roots: &Roots,
    home: &Path,
    project: &Path,
    clock: &(dyn Clock + Sync),
    cancel: &CancelToken,
    facts: &RepoFacts,
    action: impl FnOnce(&WorktreeContext<'_>) -> Result<T, WorktreeError>,
) -> Result<T, WorktreeError> {
    let policy_workers = CensusPolicyWorkers::from_roots(&SystemWorkerObserver, roots)?;
    let workers = CensusSession::new(&policy_workers, clock);
    with_context_using_workers_and_facts(
        roots, home, project, clock, cancel, &workers, facts, action,
    )
}

/// Build the ordinary worktree context around a caller-owned command census.
/// This lets commands that construct several contexts share one evidence epoch.
pub fn with_context_using_workers<T>(
    roots: &Roots,
    home: &Path,
    project: &Path,
    clock: &(dyn Clock + Sync),
    cancel: &CancelToken,
    workers: &dyn WorkerObserver,
    action: impl FnOnce(&WorktreeContext<'_>) -> Result<T, WorktreeError>,
) -> Result<T, WorktreeError> {
    let facts = RepoFacts::default();
    with_context_using_workers_and_facts(
        roots, home, project, clock, cancel, workers, &facts, action,
    )
}

/// Reuse the command's repository facts across contexts and census epochs.
#[allow(clippy::too_many_arguments)]
pub fn with_context_using_workers_and_facts<T>(
    roots: &Roots,
    home: &Path,
    project: &Path,
    clock: &(dyn Clock + Sync),
    cancel: &CancelToken,
    workers: &dyn WorkerObserver,
    facts: &RepoFacts,
    action: impl FnOnce(&WorktreeContext<'_>) -> Result<T, WorktreeError>,
) -> Result<T, WorktreeError> {
    fn child(parent: &Directory, name: &OsStr) -> Result<Directory, WorktreeError> {
        match parent.mkdir(name, 0o700) {
            Ok(()) => parent.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        Ok(parent.open_verified_directory(name)?)
    }
    let state = Directory::open_anchor(roots.state.as_path())?;
    let _home = child(&state, OsStr::new("utility-home"))?;
    let git = provenant_os::git::git_executable().ok_or(WorktreeError::InvalidRequest)?;
    let runtime = UtilityRuntime::new(
        canonicalize(&git)?,
        roots.state.as_path().join("utility-home"),
        roots.state.as_path(),
    )?;
    let executor = SystemExecutor::new(1).map_err(|_| WorktreeError::InvalidRequest)?;
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let selected = runtime
        .project_location(&executor, project, budget, cancel)?
        .repository
        .ok_or(WorktreeError::InvalidRequest)?;
    let opened = open_project_repo(&runtime, &executor, selected, budget, cancel)?;
    let repo = opened.repo;
    let binding = provenant_config::bind_project(
        roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())
            .map_err(|_| WorktreeError::InvalidRequest)?,
    )
    .map_err(|_| WorktreeError::InvalidRequest)?;
    let registry = child(&state, OsStr::new("worktrees"))?;
    let project_state = child(&registry, OsStr::new(&binding.project_id().to_string()))?;
    let lock_budget =
        WaitBudget::cli(clock.mono_now(), 5).map_err(|_| WorktreeError::InvalidRequest)?;
    action(&WorktreeContext {
        home,
        repo: &repo,
        binding: &binding,
        project_state: &project_state,
        executor: &executor,
        utility: &runtime,
        facts,
        workers,
        clock,
        utility_budget: budget,
        mutation_budget: UtilityBudget::bounded(Duration::from_secs(120))?,
        lock_budget,
    })
}

/// Resolve the verified primary checkout shared by a selected checkout and
/// linked worktrees. Repositories with a bare common directory have no primary
/// and should use `open_project_repo` to retain their explicit host-only layout.
pub fn open_primary_repo(
    runtime: &UtilityRuntime,
    executor: &dyn Executor,
    selected: VerifiedRepo,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<VerifiedRepo, WorktreeError> {
    let opened = open_project_repo(runtime, executor, selected, budget, cancel)?;
    if opened.layout.project().is_some() {
        Ok(opened.repo)
    } else {
        Err(WorktreeError::InvalidRequest)
    }
}

/// Resolve the project-layout owner from Git's worktree inventory. The common
/// directory's parent is not necessarily a checkout: separate Git directories
/// can live elsewhere, and bare repositories intentionally have no primary
/// checkout.
pub fn open_project_repo(
    runtime: &UtilityRuntime,
    executor: &dyn Executor,
    selected: VerifiedRepo,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<OpenProjectRepo, WorktreeError> {
    let selected_root = selected.root_path().to_path_buf();
    let common = selected.common_git_dir().to_path_buf();
    let common_identity = selected.common_identity();
    let read_only = runtime.open_read_repo(executor, &selected_root, budget, cancel)?;
    let facts = RepoFacts::default();
    let inventory = facts.read_only_inventory(&read_only, executor, runtime, budget, cancel)?;
    let entries = parse_worktree_list(&inventory)?;
    let selected_registered = entries.iter().any(|entry| {
        !entry.bare && canonical_inventory_path(&entry.path) == Some(selected_root.clone())
    });
    if !selected_registered {
        return Err(WorktreeError::InvalidOutput);
    }
    if let Some(primary) = verified_primary_from_inventory(&common, common_identity, &entries)? {
        let repo = runtime.open_repo(executor, primary.root_path(), budget, cancel)?;
        let layout = ProjectLayout::from_repo(&repo, true)?;
        Ok(OpenProjectRepo { repo, layout })
    } else {
        // This repository has no conventional checkout to own project files.
        // Use the registered common directory as its stable allocation root,
        // and source only host/built-in ports.
        let layout = ProjectLayout::from_common_identity(&common, common_identity)?;
        Ok(OpenProjectRepo {
            repo: selected,
            layout,
        })
    }
}
