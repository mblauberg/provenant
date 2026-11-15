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
        roots.state.as_path(),
    )?;
    let executor = SystemExecutor::new(1).map_err(|_| WorktreeError::InvalidRequest)?;
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let selected = runtime
        .project_location(&executor, project, budget, cancel)?
        .repository
        .ok_or(WorktreeError::InvalidRequest)?;
    let location = if selected.private_git_dir() == selected.common_git_dir() {
        provenant_config::ProjectLocation::from_verified_primary_checkout(
            selected.common_git_dir(),
            selected.root_path(),
        )
    } else {
        provenant_config::ProjectLocation::new(selected.common_git_dir())
    }
    .map_err(|_| WorktreeError::InvalidRequest)?;
    let binding = provenant_config::bind_project(roots, location)
        .map_err(|_| WorktreeError::InvalidRequest)?;
    let layout = project_layout_for_binding(
        &executor,
        &binding,
        runtime.git_program_path(),
        &project_layout_git_environment(),
        Duration::from_secs(5),
        cancel,
    )?;
    let registry = child(&state, OsStr::new("worktrees"))?;
    let project_state = child(&registry, OsStr::new(&binding.project_id().to_string()))?;
    let lock_budget =
        WaitBudget::cli(clock.mono_now(), 5).map_err(|_| WorktreeError::InvalidRequest)?;
    action(&WorktreeContext {
        home,
        repo: &selected,
        layout: &layout,
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
