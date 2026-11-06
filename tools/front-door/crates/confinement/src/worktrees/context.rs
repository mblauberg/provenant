//! Shared local lifecycle context for dispatch and operator commands.
use super::*;
use provenant_core::Roots;
use provenant_os::exec::SystemExecutor;
use std::time::Duration;

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
    )?;
    let executor = SystemExecutor::new(1).map_err(|_| WorktreeError::InvalidRequest)?;
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let selected = runtime.open_repo(&executor, project, budget, cancel)?;
    let primary = selected
        .common_git_dir()
        .parent()
        .ok_or(WorktreeError::InvalidRequest)?
        .to_path_buf();
    // The selected checkout is commonly the primary itself. Its verified
    // handle already contains the Git probes needed by this context.
    let repo = if selected.root_path() == primary {
        selected
    } else {
        runtime.open_repo(&executor, &primary, budget, cancel)?
    };
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
