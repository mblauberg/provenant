//! Writer identity and confinement composition. Supervisor owns the lease.
use crate::{Result, RunError, WriterRequest};
use provenant_confinement::{
    ConfinedPlan, UtilityBudget, UtilityRuntime, VerifiedWorktree,
    permissions::{
        AnchoredDirectory, Anchors, BackendContext, ProviderProfile, required_anchors,
        system_backend,
    },
    repo::GitRefName,
    worktrees,
};
use provenant_core::{CancelToken, GitOid, ProjectId};
use provenant_supervisor::{
    custody::WriterBinding,
    exec::SystemExecutor,
    fs::{Directory, LocalFs, canonicalize},
};
use std::io::Read;
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    sync::Arc,
    time::Duration,
};

fn error(error: impl std::fmt::Display) -> RunError {
    RunError::Io(std::io::Error::other(error.to_string()))
}
fn runtime(state: &Path) -> Result<UtilityRuntime> {
    let home = state.join("writer-git-home");
    let state = Directory::open_anchor(state)?;
    match state.mkdir(OsStr::new("writer-git-home"), 0o700) {
        Ok(()) => state.sync()?,
        Err(e) if e.is_exists() => {}
        Err(e) => return Err(e.into()),
    }
    let git = [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/usr/bin/git",
        "/bin/git",
    ]
    .into_iter()
    .map(Path::new)
    .find(|p| provenant_supervisor::sys::is_executable(p))
    .ok_or(RunError::Validation("Git executable unavailable"))?;
    UtilityRuntime::new(Arc::new(LocalFs), canonicalize(git)?, home).map_err(error)
}
fn budget() -> Result<UtilityBudget> {
    UtilityBudget::bounded(Duration::from_secs(5)).map_err(error)
}
/// Resolve relative paths from the selected repository's primary root. Git's
/// linked-worktree registration proves membership and excludes the primary.
pub fn resolve_writer(state: &Path, cwd: &Path, path: &Path) -> Result<WriterRequest> {
    let utility = runtime(state)?;
    let executor = SystemExecutor::new(1).map_err(error)?;
    let selected = utility
        .project_location(&executor, cwd, budget()?, &CancelToken::new())
        .map_err(error)?;
    let repo = selected
        .repository
        .ok_or(RunError::Validation("writer needs a Git project"))?;
    let project = repo.common_git_dir().parent().ok_or(RunError::UnsafePath)?;
    let selected = project.join(path);
    let path = canonicalize(&selected).map_err(|error| {
        if error.is_not_found() {
            RunError::WorktreeMissing(selected)
        } else {
            error.into()
        }
    })?;
    let worktree = provenant_confinement::verify_worktree(&repo, &path)
        .map_err(worktrees::WorktreeError::from)?;
    Ok(WriterRequest {
        project: project.to_path_buf(),
        worktree: worktree.path().to_path_buf(),
    })
}

pub struct WriterLaunch {
    pub worktree: VerifiedWorktree,
    pub branch: GitRefName,
    pub(crate) custody: Option<(Directory, WriterBinding)>,
}
/// Reopen the persisted scope and match its active Provenant registration.
/// The subsequent supervisor reservation is the authoritative exclusion check.
pub fn prepare_writer(
    request: &WriterRequest,
    state: &Path,
    project: ProjectId,
) -> Result<WriterLaunch> {
    let utility = runtime(state)?;
    let executor = SystemExecutor::new(1).map_err(error)?;
    let repo = utility
        .open_repo(&executor, &request.project, budget()?, &CancelToken::new())
        .map_err(error)?;
    let worktree = provenant_confinement::verify_worktree(&repo, &request.worktree)
        .map_err(worktrees::WorktreeError::from)?;
    let mut head = Vec::new();
    worktree
        .repo()
        .private_dir()
        .open_read(OsStr::new("HEAD"))?
        .take(4097)
        .read_to_end(&mut head)?;
    if head.len() > 4096 {
        return Err(RunError::Validation("writer HEAD exceeds bound"));
    }
    let head = std::str::from_utf8(&head).map_err(error)?;
    let branch = GitRefName::new(
        head.trim()
            .strip_prefix("ref: ")
            .ok_or(RunError::Validation("writer needs a branch"))?,
    )
    .map_err(error)?;
    let base = provenant_confinement::run_utility(
        &executor,
        provenant_confinement::GitRead::ResolveRef {
            name: branch.clone(),
        },
        worktree.repo(),
        &utility,
        budget()?,
        &CancelToken::new(),
    )
    .map_err(error)?;
    if base.termination != provenant_supervisor::exec::Termination::Exit(0) || base.stdout.truncated
    {
        return Err(RunError::Validation("writer branch cannot be resolved"));
    }
    let base = GitOid::parse(
        std::str::from_utf8(&base.stdout.bytes)
            .map_err(error)?
            .trim(),
    )
    .map_err(error)?;
    let state_dir = Directory::open_anchor(state)?;
    let projects = state_dir.open_verified_directory(OsStr::new("worktrees"))?;
    let project_state = projects.open_verified_directory(OsStr::new(&project.to_string()))?;
    let (directory, binding) =
        worktrees::writer_custody(&project_state, project, &worktree, &branch, base)?;
    Ok(WriterLaunch {
        worktree,
        branch,
        custody: Some((directory, binding)),
    })
}

/// An uncompiled provider plan. Run compiles only after acquiring custody.
pub struct LaunchConfinement {
    pub plan: ConfinedPlan,
    pub profile: ProviderProfile,
    pub writer: Option<WriterLaunch>,
}
impl LaunchConfinement {
    pub(crate) fn command(&self, argv: &[OsString], cancel: &CancelToken) -> Result<Vec<OsString>> {
        let executor = SystemExecutor::new(1).map_err(error)?;
        let backend = system_backend(&BackendContext {
            safe_fs: &LocalFs,
            executor: &executor,
            cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        })
        .map_err(RunError::Confinement)?;
        let writer = self.writer.as_ref().map(|w| (&w.worktree, &w.branch));
        let directories = required_anchors(self.plan.permissions(), &self.profile, writer)
            .into_iter()
            .map(|p| AnchoredDirectory::open_canonical(&LocalFs, p))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(RunError::Confinement)?;
        let anchors = Anchors {
            directories: &directories,
            safe_fs: &LocalFs,
            worktree: writer,
        };
        self.plan
            .compile(backend.as_ref(), &self.profile, &anchors)
            .map_err(RunError::Confinement)?
            .command(argv)
            .map_err(RunError::Confinement)
    }
}

/// A retained reservation, including one whose terminal record was committed
/// just before its guardian died. The exact path supports chair recovery.
#[derive(serde::Serialize)]
pub struct WriterFence {
    pub scope: provenant_supervisor::custody::Scope,
    pub reservation: std::path::PathBuf,
}
impl crate::RunStore {
    pub fn writer_fences(&self) -> Result<Vec<WriterFence>> {
        let path = self
            .context
            .state_root
            .as_path()
            .join("worktrees")
            .join(self.context.project_id.to_string());
        let directory = match Directory::open_anchor(&path) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut fences = Vec::new();
        for (index, name) in directory.entries()?.enumerate() {
            if index >= 10_000 {
                return Err(RunError::TooLarge { limit: 10_000 });
            }
            let name = name?;
            if directory.metadata_at(&name, false)?.kind
                != provenant_supervisor::fs::FileKind::Directory
            {
                continue;
            }
            let child = directory.open_verified_directory(&name)?;
            let bytes =
                match child.read_bounded(OsStr::new("reservation.json"), crate::RECORD_LIMIT) {
                    Ok(bytes) => bytes,
                    Err(error) if error.is_not_found() => continue,
                    Err(error) => return Err(error.into()),
                };
            #[derive(serde::Deserialize)]
            struct Reservation {
                schema_version: u64,
                scope: provenant_supervisor::custody::Scope,
            }
            let reservation: Reservation = crate::db::decode(&bytes)?;
            if reservation.schema_version != 1 || reservation.scope.generation == 0 {
                return Err(RunError::Validation("writer reservation version or scope"));
            }
            fences.push(WriterFence {
                scope: reservation.scope,
                reservation: path.join(name).join("reservation.json"),
            });
        }
        Ok(fences)
    }
}
