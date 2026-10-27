//! Shared local lifecycle context for dispatch and operator commands.
use super::*;
use provenant_core::Roots;
use provenant_os::exec::SystemExecutor;
use std::time::Duration;

pub fn with_context<T>(
    roots: &Roots,
    home: &Path,
    project: &Path,
    clock: &dyn Clock,
    cancel: &CancelToken,
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
        .ok_or(WorktreeError::InvalidRequest)?;
    let repo = runtime.open_repo(&executor, primary, budget, cancel)?;
    let binding = provenant_config::bind_project(
        roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())
            .map_err(|_| WorktreeError::InvalidRequest)?,
    )
    .map_err(|_| WorktreeError::InvalidRequest)?;
    let registry = child(&state, OsStr::new("worktrees"))?;
    let project_state = child(&registry, OsStr::new(&binding.project_id().to_string()))?;
    action(&WorktreeContext {
        home,
        repo: &repo,
        binding: &binding,
        project_state: &project_state,
        executor: &executor,
        utility: &runtime,
        workers: &SystemWorkerObserver,
        clock,
        utility_budget: budget,
        mutation_budget: UtilityBudget::bounded(Duration::from_secs(120))?,
        lock_budget: WaitBudget::cli(clock.mono_now(), 5)
            .map_err(|_| WorktreeError::InvalidRequest)?,
    })
}
