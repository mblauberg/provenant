//! Compose the worktree owner with verified project and fresh state roots.
use crate::{emit, fail, roots};
use provenant_cli::{Globals, Worktree};
use provenant_confinement::{
    UtilityBudget, UtilityRuntime,
    worktrees::{
        self, RetainReason, WorktreeContext, WorktreeCreate, WorktreeError, WorktreeRemove,
    },
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_records::{CommandResult, Outcome};
use provenant_supervisor::{
    clock::SystemClock,
    exec::SystemExecutor,
    fs::{Directory, FileKind, FsError, LocalFs, SafeDir, canonicalize},
};
use serde::Serialize;
use std::{ffi::OsStr, path::Component, sync::Arc, time::Duration};
use thiserror::Error;

/// A checkout writes the whole tree and a 1.8 GB repository took well over the
/// 5 second read budget. Ten minutes covers very large checkouts yet still ends
/// a hung `git worktree add`; `create` rolls back what a timeout leaves.
const MUTATION_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    Root(#[from] roots::RootError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Permissions(#[from] provenant_confinement::permissions::PermissionError),
    #[error(transparent)]
    Utility(#[from] provenant_confinement::utility::UtilityError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] provenant_supervisor::fs::FsError),
    #[error(transparent)]
    Exec(#[from] provenant_supervisor::exec::ExecError),
    #[error("Git executable unavailable")]
    GitUnavailable,
    #[error("cannot establish worktree lock budget")]
    Budget,
    #[error("invalid worktree removal scope")]
    InvalidScope,
}

#[allow(clippy::print_stderr)]
fn warn_cache_sweep(error: &impl std::fmt::Display) {
    eprintln!("warning: could not sweep orphaned lane caches: {error}");
}

#[allow(clippy::print_stderr)]
fn note_kept_unmerged() {
    eprintln!(
        "note: the branch is not merged into the integration branch and was kept; pass --discard to delete it"
    );
}

impl Error {
    fn outcome(&self) -> Outcome {
        match self {
            Self::Root(roots::RootError::Checkout | roots::RootError::State(_))
            | Self::InvalidScope
            | Self::Worktree(
                WorktreeError::InvalidRequest | WorktreeError::NotFound | WorktreeError::CheckedOut,
            ) => Outcome::Invalid,
            Self::Worktree(
                WorktreeError::Dirty
                | WorktreeError::GitOperationInProgress
                | WorktreeError::LiveWriter
                | WorktreeError::UnknownWriter
                | WorktreeError::Primary
                | WorktreeError::IdentityChanged
                | WorktreeError::PrimaryMoved,
            ) => Outcome::Refused,
            Self::Worktree(WorktreeError::Cancelled) => Outcome::Cancelled,
            _ => Outcome::Error,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Data {
    Created(worktrees::WorktreeInfo),
    Listed(Vec<worktrees::WorktreeInfo>),
    Removed(worktrees::RemoveOutcome),
}

pub fn run(command: Worktree, globals: &Globals) -> u8 {
    let name = match &command {
        Worktree::Create { .. } => "worktree create",
        Worktree::List => "worktree list",
        Worktree::Remove(_) => "worktree remove",
    };
    let read_only = matches!(command, Worktree::List);
    match execute(command, globals) {
        Ok(data) => match serde_json::to_string_pretty(&data) {
            Ok(human) => emit(
                &CommandResult::new(name, Outcome::Ok, data),
                globals,
                &(human + "\n"),
                read_only,
            ),
            Err(_) => fail(
                name,
                Outcome::Error,
                "ENCODE",
                "cannot encode worktree result",
                globals,
            ),
        },
        Err(error) => fail(
            name,
            error.outcome(),
            "WORKTREE",
            &error.to_string(),
            globals,
        ),
    }
}

fn execute(command: Worktree, globals: &Globals) -> Result<Data, Error> {
    let assets = roots::product_root()?;
    let project = roots::selected_project(globals.project.as_deref(), true)?;
    let roots = roots::roots(globals, &assets, &project)?;
    // Create the fresh state hierarchy through held, no-follow directories.
    let mut state = Directory::root(true)?;
    for component in roots.state.as_path().components() {
        if let Component::Normal(name) = component {
            match state.mkdir(name, 0o700) {
                Ok(()) => state.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            state = match state.open_verified_directory(name) {
                Ok(child) => child,
                // Only the configured absolute state path may cross mounts.
                // Re-anchor the held no-follow child after checking identity.
                Err(error) if error.is_cross_device() => {
                    let before = state.metadata_at(name, false)?;
                    let child = state.open_directory(name)?;
                    let opened = child.metadata()?;
                    let after = state.metadata_at(name, false)?;
                    if before.kind != FileKind::Directory
                        || !before.same_identity(&opened)
                        || !opened.same_identity(&after)
                    {
                        return Err(FsError::RaceRefused { name: name.into() }.into());
                    }
                    child.into_anchor()?
                }
                Err(error) => return Err(error.into()),
            };
        }
    }
    let utility_home = child(&state, "utility-home")?;
    let git = super::roots::git_binary().ok_or(Error::GitUnavailable)?;
    let runtime = UtilityRuntime::new(
        Arc::new(LocalFs),
        canonicalize(&git)?,
        roots.state.as_path().join("utility-home"),
    )?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let repo = runtime.open_repo(&executor, &project, budget, &cancel)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
        &LocalFs,
    )?;
    let worktrees_state = child(&state, "worktrees")?;
    let project_state = child(worktrees_state.as_ref(), &binding.project_id().to_string())?;
    let clock = SystemClock;
    let context = WorktreeContext {
        repo: &repo,
        binding: &binding,
        project_state: project_state.as_ref(),
        executor: &executor,
        utility: &runtime,
        #[cfg(not(test))]
        workers: &worktrees::SystemWorkerObserver,
        #[cfg(test)]
        workers: &crate::worktree_tests::FreeWorkers,
        clock: &clock,
        utility_budget: budget,
        mutation_budget: UtilityBudget::bounded(MUTATION_TIMEOUT)?,
        lock_budget: WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Budget)?,
    };
    let result = match command {
        Worktree::Create { branch, base } => Data::Created(worktrees::create(
            &context,
            WorktreeCreate { branch, base },
            &cancel,
        )?),
        Worktree::List => Data::Listed(worktrees::list(&context, &cancel)?),
        Worktree::Remove(args) => {
            let named = args.name.is_some();
            let request = if let Some(name) = args.name {
                WorktreeRemove::Name {
                    name,
                    discard: args.discard,
                    integration: args.integration,
                }
            } else {
                WorktreeRemove::Merged {
                    integration: args.integration.ok_or(Error::InvalidScope)?,
                    scope: match args.since {
                        Some(oid) => worktrees::PruneScope::Since(oid),
                        None => worktrees::PruneScope::Branches(args.branch),
                    },
                }
            };
            let removed = worktrees::remove(&context, request, &cancel)?;
            if named
                && !args.discard
                && removed
                    .retained
                    .iter()
                    .any(|kept| kept.reason == RetainReason::NotMerged)
            {
                note_kept_unmerged();
            }
            match roots::home() {
                Ok(home) => {
                    if let Err(error) =
                        provenant_confinement::permissions::prune_orphaned_worktree_cargo_caches(
                            &home,
                        )
                    {
                        warn_cache_sweep(&error);
                    }
                }
                Err(error) => warn_cache_sweep(&error),
            }
            Data::Removed(removed)
        }
    };
    drop(utility_home);
    Ok(result)
}

fn child(parent: &dyn SafeDir, name: &str) -> Result<Box<dyn SafeDir>, Error> {
    let name = OsStr::new(name);
    match parent.mkdir(name, 0o700) {
        Ok(()) => parent.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(parent.open_verified_directory(name)?)
}
