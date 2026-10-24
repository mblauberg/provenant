//! Writer identity and confinement composition. Supervisor owns the lease.
use crate::{Result, RunError, WriterRequest};
use provenant_confinement::{
    ConfinedPlan, UtilityBudget, UtilityRuntime, VerifiedWorktree,
    permissions::{
        AccessMode, AnchoredDirectory, Anchors, BackendContext, ProviderProfile,
        prepare_writer_directories, required_anchors, system_backend,
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
    path::{Path, PathBuf},
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
    pub(crate) base: GitOid,
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
        base,
        custody: Some((directory, binding)),
    })
}

/// An uncompiled provider plan. Run compiles only after acquiring custody.
pub struct LaunchConfinement {
    pub plan: ConfinedPlan,
    pub profile: ProviderProfile,
    pub writer: Option<WriterLaunch>,
    /// Canonical host home; the writer's lane cargo cache derives from it.
    pub home: PathBuf,
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
        // Run holds writer custody here: create the derived ref parents and the
        // private lane cache before they are anchored.
        if let (Some((worktree, branch)), AccessMode::WorktreeWrite(authority)) =
            (writer, &self.plan.permissions().mode)
        {
            prepare_writer_directories(&LocalFs, worktree, branch, authority, &self.home)
                .map_err(RunError::Confinement)?;
        }
        let directories = required_anchors(self.plan.permissions(), &self.profile, writer)
            .into_iter()
            .map(|p| AnchoredDirectory::open_canonical(&LocalFs, p))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(RunError::Confinement)?;
        let anchors = Anchors {
            home: &self.home,
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

/// A retained reservation, including an exact path for chair recovery.
pub use provenant_supervisor::custody::WriterReservation as WriterFence;
impl crate::RunStore {
    pub fn writer_fences(&self) -> Result<Vec<WriterFence>> {
        Ok(self.writer_fence_listing()?.reservations)
    }
    pub fn writer_fence_listing(
        &self,
    ) -> Result<provenant_supervisor::custody::ReservationListing> {
        let path = self
            .writer_registry
            .join("worktrees")
            .join(self.context.project_id.to_string());
        let directory = match Directory::open_anchor(&path) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(Default::default()),
            Err(error) => return Err(error.into()),
        };
        let mut listing =
            provenant_supervisor::custody::list_writer_reservations(&directory).map_err(error)?;
        for reservation in &mut listing.reservations {
            reservation.reservation = path.join(&reservation.reservation);
        }
        for issue in &mut listing.issues {
            issue.reservation = path.join(&issue.reservation);
        }
        Ok(listing)
    }
}

impl WriterLaunch {
    pub(crate) fn snapshot(
        &self,
        state: &Path,
        attempt: &Directory,
        scope: provenant_supervisor::custody::Scope,
    ) -> Result<()> {
        let archive = crate::artifacts::subdir(attempt, OsStr::new("quarantine"), true)?;
        let utility = runtime(state)?;
        let executor = SystemExecutor::new(1).map_err(error)?;
        let cancel = CancelToken::new();
        let git = provenant_confinement::Git::new(
            &executor,
            self.worktree.repo(),
            &utility,
            budget()?,
            &cancel,
        );
        let paths = git.writer_instruction_paths().map_err(error)?;
        let snapshot = git
            .snapshot_instructions(scope, &paths, &archive)
            .map_err(error)?;
        if snapshot.head() != self.base {
            return Err(RunError::Validation("writer base changed before launch"));
        }
        snapshot.persist(&archive).map_err(error)
    }
    pub(crate) fn terminal(
        &self,
        state: &Path,
        attempt: &Directory,
        custody: &provenant_supervisor::custody::Recovery,
    ) -> Result<bool> {
        let archive = crate::artifacts::subdir(attempt, OsStr::new("quarantine"), false)?;
        let base = custody
            .writer_base(
                self.worktree.repo().root_dir(),
                self.worktree.repo().private_dir(),
                self.branch.as_str(),
            )
            .map_err(error)?;
        let utility = runtime(state)?;
        let executor = SystemExecutor::new(1).map_err(error)?;
        let cancel = CancelToken::new();
        let git = provenant_confinement::Git::new(
            &executor,
            self.worktree.repo(),
            &utility,
            budget()?,
            &cancel,
        );
        let snapshot = git
            .recover_instruction_snapshot(custody, base, &archive)
            .map_err(error)?;
        let outcome = git
            .quarantine_instructions(custody, &snapshot, &archive)
            .map_err(error)?;
        let (evidence, review) = match outcome {
            provenant_confinement::quarantine::QuarantineOutcome::Restored {
                evidence_digest,
                ..
            } => (evidence_digest, false),
            provenant_confinement::quarantine::QuarantineOutcome::ReviewRequired {
                evidence_digest,
            } => (evidence_digest, true),
        };
        let result =
            archive.read_bounded(OsStr::new("quarantine-result.json"), crate::RECORD_LIMIT)?;
        crate::artifacts::seal(
            &archive,
            "manifest.json",
            &crate::db::bytes(&WriterManifest {
                schema_version: 1,
                scope: custody.scope(),
                kind: "quarantine_manifest",
                evidence_digest: evidence,
                result_digest: provenant_records::CanonicalV1::file_digest(&result),
                review_required: review,
            })?,
            crate::RECORD_LIMIT,
        )?;
        let tip = provenant_confinement::run_utility(
            &executor,
            provenant_confinement::GitRead::ResolveRef {
                name: self.branch.clone(),
            },
            self.worktree.repo(),
            &utility,
            budget()?,
            &cancel,
        )
        .map_err(error)?;
        let tip = GitOid::parse(
            std::str::from_utf8(&tip.stdout.bytes)
                .map_err(error)?
                .trim(),
        )
        .map_err(error)?;
        let ancestry = provenant_confinement::run_utility(
            &executor,
            provenant_confinement::GitRead::MergeBaseIsAncestor {
                ancestor: base,
                tip,
            },
            self.worktree.repo(),
            &utility,
            budget()?,
            &cancel,
        )
        .map_err(error)?;
        if ancestry.termination != provenant_supervisor::exec::Termination::Exit(0) {
            return Err(RunError::Validation(
                "writer branch no longer descends from its recorded base",
            ));
        }
        worktrees::check_writer_terminal(self.worktree.repo())?;
        Ok(review)
    }
}
#[derive(serde::Serialize)]
struct WriterManifest {
    schema_version: u64,
    scope: provenant_supervisor::custody::Scope,
    kind: &'static str,
    evidence_digest: provenant_records::FileDigest,
    result_digest: provenant_records::FileDigest,
    review_required: bool,
}

/// Remove the empty generated staging directory under attempt exclusion.
/// Provider state/history and unclassified contents remain retained.
pub(crate) fn settle_launch_staging(attempt: &Directory) -> Result<()> {
    let name = OsStr::new("scratch");
    let scratch = match attempt.open_verified_directory(name) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if scratch.entries()?.next().transpose()?.is_none() {
        attempt.rmdir(name)?;
        attempt.sync()?;
    }
    Ok(())
}
