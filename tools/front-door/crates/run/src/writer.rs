//! Writer identity and confinement composition. Supervisor owns the lease.
use crate::{PathCheck, Result, RunError, WriterRequest};
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
use provenant_os::{
    exec::SystemExecutor,
    fs::{Directory, FsError, canonicalize},
};
use provenant_supervisor::custody::WriterBinding;
use std::io::{Read, Write};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

fn error<E: std::fmt::Display>(
    operation: &'static str,
    path: Option<&Path>,
) -> impl FnOnce(E) -> RunError {
    let path = path.map(Path::to_path_buf);
    move |source| RunError::OperationIo {
        operation,
        path,
        source: std::io::Error::other(source.to_string()),
    }
}
/// Held-directory errors name paths relative to their anchor. Retain a known
/// anchor before crossing the edge; the facade omits paths with unknown anchors.
fn anchored_error(error: FsError, anchor: &Path) -> RunError {
    match error {
        FsError::Io {
            operation,
            path,
            source,
        } => FsError::Io {
            operation,
            path: anchor.join(path),
            source,
        }
        .into(),
        other => other.into(),
    }
}
fn runtime(state: &Path) -> Result<UtilityRuntime> {
    let home = state.join("writer-git-home");
    let directory = Directory::open_anchor(state)?;
    match directory.mkdir(OsStr::new("writer-git-home"), 0o700) {
        Ok(()) => directory
            .sync()
            .map_err(|error| anchored_error(error, state))?,
        Err(e) if e.is_exists() => {}
        Err(e) => return Err(anchored_error(e, state)),
    }
    let git = provenant_os::git::git_executable()
        .ok_or(RunError::Validation("Git executable unavailable"))?;
    UtilityRuntime::new(canonicalize(&git)?, home.clone())
        .map_err(error("initialise writer Git runtime", Some(&home)))
}
fn budget() -> Result<UtilityBudget> {
    UtilityBudget::bounded(Duration::from_secs(5))
        .map_err(error("set writer Git utility budget", None))
}
/// Resolve relative paths from the selected repository's primary root. Git's
/// linked-worktree registration proves membership and excludes the primary.
pub fn resolve_writer(state: &Path, cwd: &Path, path: &Path) -> Result<WriterRequest> {
    resolve_writer_with(state, cwd, path, true)
}
/// A lane's writer proposal: the path is resolved but not registry-verified,
/// because a confined lane cannot read another worktree's `.git`. The host
/// verifies the registration in `prepare_writer` before it launches the child.
pub fn resolve_writer_proposal(state: &Path, cwd: &Path, path: &Path) -> Result<WriterRequest> {
    resolve_writer_with(state, cwd, path, false)
}
fn resolve_writer_with(
    state: &Path,
    cwd: &Path,
    path: &Path,
    verify: bool,
) -> Result<WriterRequest> {
    let utility = runtime(state)?;
    let executor =
        SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
    let selected = utility
        .project_location(&executor, cwd, budget()?, &CancelToken::new())
        .map_err(error("locate writer project", Some(cwd)))?;
    let repo = selected
        .repository
        .ok_or(RunError::Validation("writer needs a Git project"))?;
    let project = repo
        .common_git_dir()
        .parent()
        .ok_or_else(|| RunError::unsafe_path(repo.common_git_dir(), PathCheck::Containment))?;
    let selected = project.join(path);
    let path = canonicalize(&selected).map_err(|error| {
        if error.is_not_found() {
            RunError::WorktreeMissing(selected)
        } else {
            error.into()
        }
    })?;
    if !verify {
        if !path.starts_with(project) {
            return Err(RunError::unsafe_path(path, PathCheck::Containment));
        }
        return Ok(WriterRequest {
            project: project.to_path_buf(),
            worktree: path,
        });
    }
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GitPins {
    schema_version: u64,
    scope: provenant_supervisor::custody::Scope,
    base: GitOid,
}

/// Remove only this attempt's host-owned retention refs. This runs before the
/// terminal record becomes visible, including cancellation and import refusal.
pub(crate) fn cleanup_git_pins(
    state: &Path,
    writer: &WriterRequest,
    attempt: &Directory,
    scope: provenant_supervisor::custody::Scope,
    head: Option<GitOid>,
) -> Result<()> {
    let pins = crate::artifacts::read_sealed(attempt, "writer-git-pins.json", crate::RECORD_LIMIT)?;
    if let Some(raw) = &pins {
        let pins: GitPins = crate::db::decode(raw)?;
        if pins.schema_version != 1 || pins.scope != scope {
            return Err(RunError::Validation("writer Git pins scope mismatch"));
        }
    } else if head.is_none() {
        // A queued withdrawal has never opened the proposed writer repository.
        return Ok(());
    }
    let utility = runtime(state)?;
    let executor =
        SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
    let repo = utility
        .open_repo(&executor, &writer.project, budget()?, &CancelToken::new())
        .map_err(error(
            "open writer repository for Git ref cleanup",
            Some(&writer.project),
        ))?;
    let mut refs = vec![format!("refs/provenant/lane-base/{}", scope.attempt)];
    if let Some(head) = head {
        refs.push(format!("refs/provenant/import/{head}"));
    }
    for reference in refs {
        crate::objects::command(
            utility.git_program_path(),
            repo.common_git_dir(),
            repo.root_path(),
            &[
                "update-ref".into(),
                "--no-deref".into(),
                "-d".into(),
                reference.into(),
            ],
            None,
        )?;
    }
    Ok(())
}
/// Reopen the persisted scope and match its active Provenant registration.
/// The subsequent supervisor reservation is the authoritative exclusion check.
pub fn prepare_writer(
    request: &WriterRequest,
    state: &Path,
    project: ProjectId,
) -> Result<WriterLaunch> {
    let utility = runtime(state)?;
    let executor =
        SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
    let repo = utility
        .open_repo(&executor, &request.project, budget()?, &CancelToken::new())
        .map_err(error("open writer repository", Some(&request.project)))?;
    let worktree = provenant_confinement::verify_worktree(&repo, &request.worktree)
        .map_err(worktrees::WorktreeError::from)?;
    let head_path = worktree.repo().private_git_dir().join("HEAD");
    let mut head = Vec::new();
    worktree
        .repo()
        .private_dir()
        .open_read(OsStr::new("HEAD"))
        .map_err(|error| anchored_error(error, worktree.repo().private_git_dir()))?
        .take(4097)
        .read_to_end(&mut head)
        .map_err(error("read writer HEAD", Some(&head_path)))?;
    if head.len() > 4096 {
        return Err(RunError::Validation("writer HEAD exceeds bound"));
    }
    let head = std::str::from_utf8(&head).map_err(error("decode writer HEAD", Some(&head_path)))?;
    let branch = GitRefName::new(
        head.trim()
            .strip_prefix("ref: ")
            .ok_or(RunError::Validation("writer needs a branch"))?,
    )
    .map_err(error("parse writer branch", Some(&head_path)))?;
    let base = provenant_confinement::run_utility(
        &executor,
        provenant_confinement::GitRead::ResolveRefOid {
            name: branch.clone(),
        },
        worktree.repo(),
        &utility,
        budget()?,
        &CancelToken::new(),
    )
    .map_err(error("resolve writer branch", Some(&request.worktree)))?;
    if base.termination != provenant_os::exec::Termination::Exit(0) || base.stdout.truncated {
        return Err(RunError::Validation("writer branch cannot be resolved"));
    }
    let base = GitOid::parse(
        std::str::from_utf8(&base.stdout.bytes)
            .map_err(error("decode writer base", Some(&request.worktree)))?
            .trim(),
    )
    .map_err(error("parse writer base", Some(&request.worktree)))?;
    let state_dir = Directory::open_anchor(state)?;
    // Git knows the worktree but Provenant holds no state for it: it was not
    // created by `provenant worktree create`.
    let unregistered = |error: provenant_os::fs::FsError| {
        if error.is_not_found() {
            RunError::WorktreeUnregistered(request.worktree.clone())
        } else {
            anchored_error(error, state)
        }
    };
    let projects = state_dir
        .open_verified_directory(OsStr::new("worktrees"))
        .map_err(unregistered)?;
    let project_state = projects
        .open_verified_directory(OsStr::new(&project.to_string()))
        .map_err(unregistered)?;
    let (directory, binding) =
        worktrees::writer_custody(&project_state, project, &worktree, &branch, base, || {
            let outcome = provenant_confinement::run_utility(
                &executor,
                provenant_confinement::GitRead::WorktreeList,
                &repo,
                &utility,
                budget().map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                &CancelToken::new(),
            )?;
            if outcome.termination != provenant_os::exec::Termination::Exit(0)
                || outcome.stdout.truncated
            {
                return Err(worktrees::WorktreeError::InvalidOutput);
            }
            Ok(outcome.stdout.bytes)
        })
        .map_err(|error| match error {
            worktrees::WorktreeError::NotFound => {
                RunError::WorktreeUnregistered(request.worktree.clone())
            }
            other => other.into(),
        })?;
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
    /// Opt in to cloning the host Cargo registry into a fresh writer cache.
    pub cargo_registry_seed: bool,
    /// Host state root containing target seed generations.
    pub state_root: PathBuf,
}
impl LaunchConfinement {
    /// Prepare Git while custody is held, then replace every handle that was
    /// bound to the common config before Git changed it.
    pub(crate) fn prepare_git(&mut self) -> Result<Option<provenant_os::fs::LockGuard>> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(None);
        };
        let AccessMode::WorktreeWrite(authority) = &self.plan.permissions().mode else {
            return Ok(None);
        };
        prepare_writer_git(writer, &self.state_root, &self.home, Some(authority)).map(Some)
    }
    pub(crate) fn command(
        &self,
        argv: &[OsString],
        environment: &[(OsString, OsString)],
        attempt: &provenant_core::AttemptId,
        cancel: &CancelToken,
        attempt_dir: &Directory,
    ) -> Result<Vec<OsString>> {
        let executor =
            SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
        let backend = system_backend(&BackendContext {
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
            prepare_writer_directories(worktree, branch, authority, &self.home)
                .map_err(RunError::Confinement)?;
        }
        let outcome = match &self.plan.permissions().mode {
            AccessMode::WorktreeWrite(authority) => crate::cargo_seed::seed_registry(
                self.cargo_registry_seed,
                &self.home,
                &authority.cargo_cache().path,
            ),
            AccessMode::ReadOnly => crate::cargo_seed::Outcome::skipped("read_only_lane"),
        };
        // Diagnostic publication must never prevent lane launch.
        if let Err(error) = record_cargo_setup(attempt_dir, &outcome) {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: could not record Cargo setup: {error}"
            );
        }
        let git_outcome = match &self.plan.permissions().mode {
            AccessMode::WorktreeWrite(authority) => crate::cargo_seed::seed_git(
                self.cargo_registry_seed,
                &self.home,
                &authority.cargo_cache().path,
            ),
            AccessMode::ReadOnly => crate::cargo_seed::Outcome::skipped("read_only_lane"),
        };
        if let Err(error) =
            record_cargo_setup_file(attempt_dir, "cargo-git-setup.json", &git_outcome)
        {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: could not record Cargo Git setup: {error}"
            );
        }
        let target_outcome = match (&self.plan.permissions().mode, writer) {
            (AccessMode::WorktreeWrite(authority), Some((worktree, _)))
                if cfg!(target_os = "macos")
                    && self.cargo_registry_seed
                    && (!verified_cargo_config(
                        worktree.path(),
                        &authority.cargo_cache().path,
                        environment,
                    ) || !trusted_lane_toolchain(
                        &self.home,
                        worktree.path(),
                        environment,
                        &provenant_confinement::write_grants(
                            self.plan.permissions(),
                            &self.profile,
                        ),
                    )) =>
            {
                crate::cargo_seed::Outcome::fallback("toolchain_unverified")
            }
            (AccessMode::WorktreeWrite(authority), Some((worktree, _))) => {
                crate::cargo_seed::seed_target(
                    self.cargo_registry_seed,
                    &self.state_root,
                    &authority.cargo_cache().path,
                    worktree.path(),
                    &self.home,
                    environment,
                )
            }
            _ => crate::cargo_seed::Outcome::skipped("read_only_lane"),
        };
        if let Err(error) =
            record_cargo_setup_file(attempt_dir, "cargo-target-setup.json", &target_outcome)
        {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: could not record Cargo target setup: {error}"
            );
        }
        let directories = required_anchors(self.plan.permissions(), &self.profile, writer)
            .into_iter()
            .map(AnchoredDirectory::open_canonical)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(RunError::Confinement)?;
        let anchors = Anchors {
            home: &self.home,
            directories: &directories,
            worktree: writer,
        };
        self.plan
            .compile(backend.as_ref(), &self.profile, &anchors)
            .map_err(RunError::Confinement)?
            .command(argv, attempt)
            .map_err(RunError::Confinement)
    }
}

fn prepare_writer_git(
    writer: &mut WriterLaunch,
    state: &Path,
    home: &Path,
    authority: Option<&provenant_confinement::permissions::WorktreeWriteAuthority>,
) -> Result<provenant_os::fs::LockGuard> {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::{clock::SystemClock, fs::LockMode};
    let utility = runtime(state)?;
    let root_identity = writer.worktree.identity();
    let private_identity = writer.worktree.repo().private_identity();
    let common_identity = writer.worktree.repo().common_identity();
    let executor =
        SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
    let cancel = CancelToken::new();
    // Independent worktrees share config. Keep both writes and all subsequent
    // verification under one project-wide lock; a later launch repairs either
    // key if a process stopped between the two writes.
    let clock = SystemClock::new();
    let setup_dir = writer.worktree.repo().common_dir();
    let setup_name = OsStr::new("provenant-lane-setup.lock");
    match setup_dir.create_new(setup_name) {
        Ok(file) => {
            file.sync()?;
            setup_dir.sync()?;
        }
        Err(failure) if failure.is_exists() => {}
        Err(failure) => {
            return Err(anchored_error(
                failure,
                writer.worktree.repo().common_git_dir(),
            ));
        }
    }
    let config_lock = writer
        .worktree
        .repo()
        .common_dir()
        .lock(
            setup_name,
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 120)
                .map_err(error("set writer setup lock budget", None))?,
            &cancel,
            &clock,
        )
        .map_err(|failure| anchored_error(failure, writer.worktree.repo().common_git_dir()))?;
    let old_repo = writer.worktree.repo();
    let project = old_repo
        .common_git_dir()
        .parent()
        .ok_or(RunError::Validation(
            "writer common Git directory has no project",
        ))?
        .to_path_buf();
    let path = writer.worktree.path().to_path_buf();
    let repo = utility
        .open_repo(&executor, &project, budget()?, &cancel)
        .map_err(error("reopen writer repository", Some(&project)))?;
    let refreshed = provenant_confinement::verify_worktree(&repo, &path)
        .map_err(worktrees::WorktreeError::from)?;
    if !root_identity.same_identity(&refreshed.identity())
        || !private_identity.same_identity(&refreshed.repo().private_identity())
        || !common_identity.same_identity(&refreshed.repo().common_identity())
    {
        return Err(RunError::Validation(
            "writer identity changed before launch",
        ));
    }
    writer.worktree = refreshed;
    verify_writer_git_paths(writer.worktree.repo().common_git_dir(), &writer.branch)?;
    let tracking_lock = writer.worktree.repo().common_git_dir().join(format!(
        "refs/remotes/origin/{}.lock",
        writer
            .branch
            .as_str()
            .strip_prefix("refs/heads/")
            .ok_or(RunError::Validation("writer branch is not a local branch"))?
    ));
    match std::fs::symlink_metadata(&tracking_lock) {
        Ok(_) => return Err(RunError::WriterRefLocked(tracking_lock)),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
        Err(failure) => {
            return Err(error("verify tracking ref lock", Some(&tracking_lock))(
                failure,
            ));
        }
    }
    if let Some(authority) = authority {
        prepare_writer_directories(&writer.worktree, &writer.branch, authority, home)
            .map_err(RunError::Confinement)?;
    }
    let repo = writer.worktree.repo();
    repo.revalidate()
        .map_err(error("verify writer repository before fetch", Some(&path)))?;
    crate::HostDeliveryContext::refresh_origin(
        &WriterRequest {
            project: project.clone(),
            worktree: path.clone(),
        },
        repo.private_git_dir(),
    )?;
    repo.revalidate()
        .map_err(error("verify writer repository after fetch", Some(&path)))?;
    let ref_path = Path::new(writer.branch.as_str());
    let parent = repo.common_git_dir().join(
        ref_path
            .parent()
            .ok_or(RunError::Validation("writer ref has no parent"))?,
    );
    let directory = Directory::open_anchor(&parent)?;
    let name = ref_path
        .file_name()
        .ok_or(RunError::Validation("writer ref has no name"))?;
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    let mut lock = directory.create_new(&lock_name).map_err(|failure| {
        if failure.is_exists() {
            RunError::WriterRefLocked(parent.join(&lock_name))
        } else {
            failure.into()
        }
    })?;
    let result = (|| -> Result<()> {
        let resolved = provenant_confinement::run_utility(
            &executor,
            provenant_confinement::GitRead::ResolveRef {
                name: writer.branch.clone(),
            },
            repo,
            &utility,
            budget()?,
            &cancel,
        )
        .map_err(error("resolve writer branch under lock", Some(&parent)))?;
        if resolved.termination != provenant_os::exec::Termination::Exit(0)
            || resolved.stdout.truncated
            || std::str::from_utf8(&resolved.stdout.bytes)
                .ok()
                .map(str::trim)
                != Some(writer.base.to_string().as_str())
        {
            return Err(RunError::Validation("writer branch changed before launch"));
        }
        writeln!(lock, "{}", writer.base).map_err(error("write writer ref lock", Some(&parent)))?;
        lock.sync()?;
        directory.publish_git_ref_lock(name, &lock)?;
        directory.sync()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = directory.discard_git_ref_lock(name, &lock);
    }
    result?;
    for merge in [false, true] {
        let repo = utility
            .open_repo(&executor, &project, budget()?, &cancel)
            .map_err(error("reopen writer repository", Some(&project)))?;
        let worktree = provenant_confinement::verify_worktree(&repo, &path)
            .map_err(worktrees::WorktreeError::from)?;
        if !root_identity.same_identity(&worktree.identity())
            || !private_identity.same_identity(&worktree.repo().private_identity())
            || !common_identity.same_identity(&worktree.repo().common_identity())
        {
            return Err(RunError::Validation(
                "writer identity changed before launch",
            ));
        }
        provenant_confinement::utility::set_lane_upstream_key(
            &executor,
            worktree.repo(),
            &utility,
            &writer.branch,
            merge,
            budget()?,
            &cancel,
        )
        .map_err(error("set writer upstream", Some(&path)))?;
    }
    let repo = utility
        .open_repo(&executor, &project, budget()?, &cancel)
        .map_err(error("reopen writer repository", Some(&project)))?;
    let refreshed = provenant_confinement::verify_worktree(&repo, &path)
        .map_err(worktrees::WorktreeError::from)?;
    if !root_identity.same_identity(&refreshed.identity())
        || !private_identity.same_identity(&refreshed.repo().private_identity())
        || !common_identity.same_identity(&refreshed.repo().common_identity())
    {
        return Err(RunError::Validation(
            "writer identity changed before launch",
        ));
    }
    let head = refreshed
        .repo()
        .private_dir()
        .read_bounded(OsStr::new("HEAD"), 4096)?;
    if std::str::from_utf8(&head).ok().map(str::trim)
        != Some(format!("ref: {}", writer.branch.as_str()).as_str())
    {
        return Err(RunError::Validation("writer branch changed before launch"));
    }
    writer.worktree = refreshed;
    Ok(config_lock)
}

// These exact paths are writable by the lane. Check them before host Git can
// follow a planted link, including during fetch and reflog updates.
pub(super) fn verify_writer_git_paths(common: &Path, branch: &GitRefName) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let suffix = branch
        .as_str()
        .strip_prefix("refs/heads/")
        .ok_or(RunError::Validation("writer branch is not a local branch"))?;
    for prefix in [
        "refs/heads",
        "logs/refs/heads",
        "refs/remotes/origin",
        "logs/refs/remotes/origin",
    ] {
        for name in [suffix.to_owned(), format!("{suffix}.lock")] {
            let relative = Path::new(prefix).join(name);
            let path = common.join(&relative);
            // lstat on the leaf still follows intermediate symlinks. A planted
            // parent could redirect Git's host-side fetch outside the repository.
            let mut parent = common.to_path_buf();
            if let Some(components) = relative.parent() {
                for component in components.components() {
                    parent.push(component);
                    match std::fs::symlink_metadata(&parent) {
                        Ok(metadata) if metadata.file_type().is_dir() => {}
                        Ok(_) => {
                            return Err(RunError::OperationIo {
                                operation: "verify lane Git parent (must be a directory)",
                                path: Some(parent),
                                source: std::io::Error::other("unsafe file type"),
                            });
                        }
                        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => break,
                        Err(failure) => {
                            return Err(error("verify lane Git parent", Some(&parent))(failure));
                        }
                    }
                }
            }
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => {}
                Ok(_) => {
                    return Err(RunError::OperationIo {
                        operation: "verify lane Git path (must be absent or a single-link regular file)",
                        path: Some(path),
                        source: std::io::Error::other("unsafe file type or link count"),
                    });
                }
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                Err(failure) => return Err(error("verify lane Git path", Some(&path))(failure)),
            }
        }
    }
    Ok(())
}

// Cargo configuration can override rustc independently of the rustup pin.
// Refuse ambiguous configurations without executing or interpreting them.
fn verified_cargo_config(
    worktree: &Path,
    cache: &Path,
    environment: &[(OsString, OsString)],
) -> bool {
    for path in [worktree.join("tools"), worktree.join("tools/front-door")] {
        if !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
            return false;
        }
    }
    let cargo_home = cache.join("cargo-home");
    let mut values = environment.iter().filter(|(key, _)| key == "CARGO_HOME");
    if !matches!(values.next(), Some((_, value)) if Path::new(value) == cargo_home)
        || values.next().is_some()
    {
        return false;
    }
    let has_config = |directory: &Path| {
        ["config", "config.toml"].iter().any(|name| {
            !matches!(std::fs::symlink_metadata(directory.join(name)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        })
    };
    !has_config(&cargo_home)
        && worktree
            .join("tools/front-door")
            .ancestors()
            .all(|ancestor| !has_config(&ancestor.join(".cargo")))
}

// Refuse the host probe if a lane can alter the host compiler or an earlier
// PATH entry, or if its rustc does not resolve to the host rustup shim.
fn trusted_lane_toolchain(
    home: &Path,
    worktree: &Path,
    environment: &[(OsString, OsString)],
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> bool {
    use provenant_confinement::permissions::PathScope;
    use provenant_core::PathResolver;
    let resolve = |path: &Path| {
        provenant_os::fs::SystemPathResolver
            .resolve_existing_ancestor(path)
            .ok()
    };
    let Some(worktree) = resolve(worktree) else {
        return false;
    };
    let protected = |path: &Path| {
        let Some(path) = resolve(path) else {
            return false;
        };
        if path.starts_with(&worktree) || worktree.starts_with(&path) {
            return false;
        }
        grants.iter().all(|grant| {
            resolve(&grant.path).is_some_and(|root| {
                !root.starts_with(&path)
                    && !(path.starts_with(&root)
                        && (grant.scope == PathScope::Subtree || root.is_dir()))
            })
        })
    };
    if !protected(&home.join(".rustup")) || !protected(&home.join(".cargo/bin")) {
        return false;
    }
    let mut paths = environment.iter().filter(|(key, _)| key == "PATH");
    let Some((_, path)) = paths.next() else {
        return false;
    };
    if paths.next().is_some() {
        return false;
    }
    let Ok(shim) = home.join(".cargo/bin/rustup").canonicalize() else {
        return false;
    };
    if !protected(&shim) {
        return false;
    }
    for directory in std::env::split_paths(path) {
        if !directory.is_absolute() || !protected(&directory) {
            return false;
        }
        let executable = directory.join("rustc");
        match executable.canonicalize() {
            Ok(found) => return found == shim,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }
    false
}

fn record_cargo_setup(
    directory: &Directory,
    outcome: &crate::cargo_seed::Outcome,
) -> std::result::Result<(), String> {
    record_cargo_setup_file(directory, "cargo-setup.json", outcome)
}

fn record_cargo_setup_file(
    directory: &Directory,
    name: &str,
    outcome: &crate::cargo_seed::Outcome,
) -> std::result::Result<(), String> {
    let bytes = serde_json::to_vec(outcome).map_err(|error| error.to_string())?;
    directory
        .atomic_write(
            OsStr::new(name),
            ".cargo-setup-",
            &bytes,
            provenant_os::fs::PublishMode::Replace,
        )
        .and_then(|publication| publication.require_durable())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod cargo_setup_tests {
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn packed_writer_ref_and_upstream_are_prepared_with_fresh_handles()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let lane = root.join("lane");
        let second_lane = root.join("second-lane");
        let state = root.join("state");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&state)?;
        let git_dir = project.join(".git");
        let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
        let run = |args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
            let output = std::process::Command::new(&git)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_DIR", &git_dir)
                .env("GIT_WORK_TREE", &project)
                .current_dir(&project)
                .args(args)
                .output()?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
            }
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        run(&["init", "-q"])?;
        run(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ])?;
        run(&[
            "worktree",
            "add",
            "-qb",
            "feature/nested",
            lane.to_str().ok_or("lane path")?,
        ])?;
        run(&[
            "worktree",
            "add",
            "-qb",
            "feature/second",
            second_lane.to_str().ok_or("second lane path")?,
        ])?;
        run(&["pack-refs", "--all"])?;
        let reference = "refs/heads/feature/nested";
        let expected = run(&["rev-parse", reference])?;
        assert!(!git_dir.join(reference).exists());
        std::fs::create_dir_all(git_dir.join("refs/heads/feature"))?;
        let utility = runtime(&state)?;
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let repo = utility.open_repo(&executor, &project, budget()?, &cancel)?;
        let worktree = provenant_confinement::verify_worktree(&repo, &lane)?;
        let mut writer = WriterLaunch {
            worktree,
            branch: GitRefName::new(reference)?,
            base: GitOid::parse(&expected)?,
            custody: None,
        };
        let second_tree = provenant_confinement::verify_worktree(&repo, &second_lane)?;
        let mut second = WriterLaunch {
            worktree: second_tree,
            branch: GitRefName::new("refs/heads/feature/second")?,
            base: GitOid::parse(&expected)?,
            custody: None,
        };
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                prepare_writer_git(&mut writer, &state, &root, None)
                    .map(drop)
                    .map_err(|e| e.to_string())
            });
            let second = scope.spawn(|| {
                prepare_writer_git(&mut second, &state, &root, None)
                    .map(drop)
                    .map_err(|e| e.to_string())
            });
            first
                .join()
                .map_err(|_| "first preparation panicked".to_owned())??;
            second
                .join()
                .map_err(|_| "second preparation panicked".to_owned())??;
            Ok::<(), String>(())
        })?;
        let repo = utility.open_repo(&executor, &project, budget()?, &cancel)?;
        provenant_confinement::verify_worktree(&repo, &lane)?
            .repo()
            .revalidate()?;
        assert_eq!(
            std::fs::read_to_string(git_dir.join(reference))?.trim(),
            expected
        );
        assert_eq!(
            run(&["show-ref", "--verify", reference])?,
            format!("{expected} {reference}")
        );
        assert_eq!(
            run(&[
                "for-each-ref",
                "--format=%(objectname) %(refname)",
                reference
            ])?,
            format!("{expected} {reference}")
        );
        assert_eq!(
            run(&["config", "--local", "--get", "branch.feature/nested.remote"])?,
            "origin"
        );
        assert_eq!(
            run(&["config", "--local", "--get", "branch.feature/nested.merge"])?,
            reference
        );
        assert_eq!(
            run(&["config", "--local", "--get", "branch.feature/second.remote"])?,
            "origin"
        );
        assert_eq!(
            run(&["config", "--local", "--get", "branch.feature/second.merge"])?,
            "refs/heads/feature/second"
        );
        run(&[
            "remote",
            "add",
            "origin",
            project.to_str().ok_or("project path")?,
        ])?;
        let target = root.join("reflog-target");
        std::fs::write(&target, b"untouched")?;
        let reflog = git_dir.join("logs/refs/remotes/origin/feature/nested");
        std::fs::create_dir_all(reflog.parent().ok_or("reflog parent")?)?;
        std::os::unix::fs::symlink(&target, &reflog)?;
        let failure = prepare_writer_git(&mut writer, &state, &root, None)
            .err()
            .ok_or("symlinked tracking reflog must refuse")?;
        assert!(failure.to_string().contains(&reflog.display().to_string()));
        assert_eq!(std::fs::read(&target)?, b"untouched");
        std::fs::remove_file(&reflog)?;
        let parent = reflog.parent().ok_or("reflog parent")?;
        std::fs::remove_dir(parent)?;
        std::os::unix::fs::symlink(&root, parent)?;
        let failure = prepare_writer_git(&mut writer, &state, &root, None)
            .err()
            .ok_or("symlinked tracking reflog parent must refuse")?;
        assert!(failure.to_string().contains(&parent.display().to_string()));
        assert_eq!(std::fs::read(&target)?, b"untouched");
        std::fs::remove_file(parent)?;
        std::fs::create_dir_all(parent)?;
        let tracking_lock = git_dir.join("refs/remotes/origin/feature/nested.lock");
        std::fs::create_dir_all(tracking_lock.parent().ok_or("tracking lock parent")?)?;
        std::fs::write(&tracking_lock, b"held")?;
        let failure = prepare_writer_git(&mut writer, &state, &root, None)
            .err()
            .ok_or("stale tracking lock must refuse")?;
        assert!(
            failure
                .to_string()
                .contains(&tracking_lock.display().to_string())
        );
        assert!(
            failure
                .to_string()
                .contains("check for a running Git process")
        );
        assert_eq!(std::fs::read(&tracking_lock)?, b"held");
        std::fs::remove_file(&tracking_lock)?;
        run(&["remote", "remove", "origin"])?;
        let stale = git_dir.join("refs/heads/feature/nested.lock");
        std::fs::write(&stale, b"held")?;
        let failure = prepare_writer_git(&mut writer, &state, &root, None)
            .err()
            .ok_or("stale lock must refuse")?;
        assert!(
            failure
                .to_string()
                .contains("check for a running Git process")
        );
        assert_eq!(std::fs::read(&stale)?, b"held");
        Ok(())
    }

    #[test]
    fn toolchain_probe_requires_immutable_host_paths_and_trusted_path_order() {
        use provenant_confinement::permissions::{PathScope, WriteGrant, WriteSource};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let worktree = tmp.path().join("worktree");
        let bin = home.join(".cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(bin.join("rustup"), b"fixture").unwrap();
        std::os::unix::fs::symlink("rustup", bin.join("rustc")).unwrap();
        let env = vec![(OsString::from("PATH"), bin.as_os_str().to_owned())];
        assert!(trusted_lane_toolchain(&home, &worktree, &env, &[]));
        let grant = WriteGrant {
            source: WriteSource::WriteRoot,
            path: home.join(".rustup"),
            scope: PathScope::Subtree,
        };
        assert!(!trusted_lane_toolchain(&home, &worktree, &env, &[grant]));
        let path = std::env::join_paths([&worktree, &bin]).unwrap();
        let unsafe_env = vec![(OsString::from("PATH"), path)];
        assert!(!trusted_lane_toolchain(&home, &worktree, &unsafe_env, &[]));
        std::fs::write(worktree.join("rustc"), b"untrusted").unwrap();
        assert!(!trusted_lane_toolchain(&home, &worktree, &unsafe_env, &[]));
    }

    #[test]
    fn cargo_config_makes_toolchain_unverifiable() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("worktree");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(worktree.join("tools/front-door")).unwrap();
        std::fs::create_dir_all(cache.join("cargo-home")).unwrap();
        let env = vec![(
            OsString::from("CARGO_HOME"),
            cache.join("cargo-home").into_os_string(),
        )];
        assert!(verified_cargo_config(&worktree, &cache, &env));
        assert!(!verified_cargo_config(&worktree, &cache, &[]));
        std::fs::write(
            cache.join("cargo-home/config.toml"),
            "[build]\nrustc = 'evil'\n",
        )
        .unwrap();
        assert!(!verified_cargo_config(&worktree, &cache, &env));
        std::fs::remove_file(cache.join("cargo-home/config.toml")).unwrap();
        std::fs::create_dir_all(worktree.join(".cargo")).unwrap();
        std::fs::write(worktree.join(".cargo/config"), "[build]\nrustc = 'evil'\n").unwrap();
        assert!(!verified_cargo_config(&worktree, &cache, &env));
    }

    #[test]
    fn cargo_setup_record_reports_writer_and_read_only_outcomes() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(tmp.path()).unwrap();
        for reason in ["disabled", "read_only_lane"] {
            let outcome = crate::cargo_seed::Outcome::skipped(reason);
            record_cargo_setup(&directory, &outcome).unwrap();
            let record: serde_json::Value = serde_json::from_slice(
                &std::fs::read(tmp.path().join("cargo-setup.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(record["status"], "skipped");
            assert_eq!(record["reason"], reason);
            record_cargo_setup_file(&directory, "cargo-git-setup.json", &outcome).unwrap();
            let git_record: serde_json::Value = serde_json::from_slice(
                &std::fs::read(tmp.path().join("cargo-git-setup.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(git_record["reason"], reason);
        }
        std::fs::remove_file(tmp.path().join("cargo-setup.json")).unwrap();
        std::fs::create_dir(tmp.path().join("cargo-setup.json")).unwrap();
        assert!(
            record_cargo_setup(&directory, &crate::cargo_seed::Outcome::skipped("disabled"))
                .is_err()
        );
    }
}

/// Whether the writer's worktree directory no longer exists.
pub(crate) fn worktree_removed(writer: &WriterRequest) -> bool {
    matches!(Directory::open_anchor(&writer.worktree), Err(error) if error.is_not_found())
}

/// A retained reservation, including an exact path for chair recovery.
pub use provenant_supervisor::custody::WriterReservation as WriterFence;
impl crate::RunStore {
    /// The writer's custody directory. When its worktree directory has been
    /// removed, the retained reservation for this attempt names it instead.
    pub(crate) fn writer_custody_directory(
        &self,
        record: &crate::AttemptRecord,
        writer: &WriterRequest,
    ) -> Result<Directory> {
        match prepare_writer(writer, &self.writer_registry, record.project_id) {
            Ok(mut launch) => Ok(launch
                .custody
                .take()
                .ok_or_else(|| RunError::unsafe_path(writer.worktree.clone(), PathCheck::Missing))?
                .0),
            Err(failure) if worktree_removed(writer) => {
                let scope = Self::scope(record);
                let fence = self
                    .writer_fences()?
                    .into_iter()
                    .find(|fence| fence.scope == scope)
                    .ok_or(failure)?;
                let parent = fence.reservation.parent().ok_or_else(|| {
                    RunError::unsafe_path(fence.reservation.clone(), PathCheck::Containment)
                })?;
                Ok(Directory::open_anchor(parent)?)
            }
            Err(failure) => Err(failure),
        }
    }
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
        let mut listing = provenant_supervisor::custody::list_writer_reservations(&directory)
            .map_err(error("list writer reservations", Some(&path)))?;
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
    /// Exercise the host's private Git preparation in manually confined fixtures.
    /// The fixture owns the attempt and worktree; installed builds omit this seam.
    #[cfg(feature = "fixture")]
    pub fn prepare_private_git_for_fixture(
        &self,
        attempt: &Directory,
        scratch: &Path,
    ) -> Result<()> {
        let git = provenant_os::git::git_executable()
            .ok_or(RunError::Validation("Git executable unavailable"))?;
        crate::objects::prepare_bound(
            &git,
            self.worktree.repo(),
            attempt,
            scratch,
            self.branch.as_str(),
            self.base,
        )
    }

    pub(crate) fn snapshot(
        &self,
        state: &Path,
        attempt: &Directory,
        scope: provenant_supervisor::custody::Scope,
    ) -> Result<()> {
        let owned = worktrees::load_owner(&self.worktree)?
            .is_some_and(|owner| owner.origin == worktrees::WorktreeOrigin::Lane);
        crate::artifacts::seal(
            attempt,
            "writer-owned.json",
            &crate::db::bytes(&WriterOwnership {
                schema_version: 1,
                lane_owned: owned,
            })?,
            crate::RECORD_LIMIT,
        )?;
        let archive = crate::artifacts::subdir(attempt, OsStr::new("quarantine"), true)?;
        let utility = runtime(state)?;
        let executor =
            SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
        let cancel = CancelToken::new();
        let git = provenant_confinement::Git::new(
            &executor,
            self.worktree.repo(),
            &utility,
            budget()?,
            &cancel,
        );
        let paths = git.writer_instruction_paths().map_err(error(
            "list writer instruction paths",
            Some(self.worktree.path()),
        ))?;
        let snapshot = git
            .snapshot_instructions(scope, &paths, &archive)
            .map_err(error(
                "snapshot writer instructions",
                Some(self.worktree.path()),
            ))?;
        if snapshot.head() != self.base {
            return Err(RunError::Validation("writer base changed before launch"));
        }
        snapshot
            .persist(&archive)
            .map_err(error("persist writer instruction snapshot", None))?;
        crate::artifacts::seal(
            attempt,
            "writer-git-pins.json",
            &crate::db::bytes(&GitPins {
                schema_version: 1,
                scope,
                base: self.base,
            })?,
            crate::RECORD_LIMIT,
        )?;
        let reference = format!("refs/provenant/lane-base/{}", scope.attempt);
        match crate::objects::resolve_ref(
            utility.git_program_path(),
            self.worktree.repo(),
            &reference,
        ) {
            Ok(current) if current == self.base => return Ok(()),
            Ok(_) => return Err(RunError::Validation("writer Git base pin changed")),
            Err(_) => {}
        }
        crate::objects::command(
            utility.git_program_path(),
            self.worktree.repo().common_git_dir(),
            self.worktree.repo().root_path(),
            &[
                "update-ref".into(),
                "--no-deref".into(),
                reference.into(),
                self.base.to_string().into(),
                "0".repeat(self.base.to_string().len()).into(),
            ],
            None,
        )?;
        Ok(())
    }
    pub(crate) fn terminal(
        &self,
        state: &Path,
        attempt: &Directory,
        custody: &provenant_supervisor::custody::Recovery,
        remote: &str,
        base_ref: Option<&str>,
        settlement: &mut crate::settlement::Driver,
    ) -> Result<bool> {
        if custody.observe() != provenant_supervisor::custody::RecoveryState::Absent {
            return Err(RunError::Validation(
                "writer object import requires absent custody",
            ));
        }
        let archive = crate::artifacts::subdir(attempt, OsStr::new("quarantine"), false)?;
        let base = custody
            .writer_base(
                self.worktree.repo().root_dir(),
                self.worktree.repo().private_dir(),
                self.branch.as_str(),
            )
            .map_err(error(
                "read writer custody base",
                Some(self.worktree.path()),
            ))?;
        settlement.set_base(base)?;
        let base = settlement
            .base()
            .ok_or(RunError::Validation("settlement base missing"))?;
        // The raw branch OID is only a name until the complete graph has been
        // hash-checked. Do this before instruction recovery, diffs or ancestry.
        let source = crate::objects::bound_source(attempt)?;
        let git_executable = provenant_os::git::git_executable()
            .ok_or(RunError::Validation("Git executable unavailable"))?;
        let shared_tip = crate::objects::resolve_ref(
            &git_executable,
            self.worktree.repo(),
            self.branch.as_str(),
        )?;
        let verified_tip = match settlement.head() {
            Some(head) => head,
            None => match &source {
                Some(source) => crate::objects::private_tip(source, self.branch.as_str())?,
                None => shared_tip,
            },
        };
        settlement.set_head(verified_tip)?;
        let head = self
            .worktree
            .repo()
            .private_dir()
            .read_stable(OsStr::new("HEAD"), 1024)?;
        if std::str::from_utf8(&head).map(str::trim).ok()
            != Some(format!("ref: {}", self.branch.as_str()).as_str())
        {
            return Err(RunError::Validation("writer HEAD is not the lane branch"));
        }
        let import_cancel = settlement.cancellation();
        if settlement.phase() == crate::settlement::Phase::Import {
            settlement.begin_import()?;
            crate::objects::settle_cancel(
                &git_executable,
                self.worktree.repo(),
                source.as_ref().map(|source| &source.objects),
                verified_tip,
                Some(&import_cancel),
                Some(custody),
            )?;
            settlement.advance(crate::settlement::Phase::Publish)?;
        }
        if crate::artifacts::read_sealed(attempt, "cancel.json", crate::RECORD_LIMIT)?.is_some() {
            return Err(RunError::Validation("lane import cancelled"));
        }
        if settlement.phase() == crate::settlement::Phase::Publish {
            crate::objects::publish_branch(
                &git_executable,
                self.worktree.repo(),
                self.branch.as_str(),
                base,
                verified_tip,
            )?;
            // The private index may contain staged edits which the host's
            // quarantine must observe. Restore it only after receive succeeds.
            if let Some(source) = &source {
                crate::objects::restore_index(source, self.worktree.repo())?;
            }
            settlement.advance(crate::settlement::Phase::Record)?;
        }
        let utility = runtime(state)?;
        let executor =
            SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
        let cancel = import_cancel;
        let git = provenant_confinement::Git::new(
            &executor,
            self.worktree.repo(),
            &utility,
            budget()?,
            &cancel,
        );
        let snapshot = git
            .recover_instruction_snapshot(custody, base, &archive)
            .map_err(error(
                "recover writer instruction snapshot",
                Some(self.worktree.path()),
            ))?;
        let oid = |repo: &provenant_confinement::repo::VerifiedRepo, operation| -> Result<GitOid> {
            let output = provenant_confinement::run_utility(
                &executor,
                operation,
                repo,
                &utility,
                budget()?,
                &cancel,
            )
            .map_err(error("resolve writer comparison", Some(repo.root_path())))?;
            if output.termination != provenant_os::exec::Termination::Exit(0)
                || output.stdout.truncated
            {
                return Err(RunError::Validation("writer comparison cannot be resolved"));
            }
            GitOid::parse(
                std::str::from_utf8(&output.stdout.bytes)
                    .map_err(error("decode writer comparison", Some(repo.root_path())))?
                    .trim(),
            )
            .map_err(error("parse writer comparison", Some(repo.root_path())))
        };
        let tip = oid(
            self.worktree.repo(),
            provenant_confinement::GitRead::RevParseHead,
        )?;
        if tip != verified_tip {
            return Err(RunError::Validation(
                "writer head changed during object verification",
            ));
        }
        // Remote refs only refine attribution. Missing or stale refs must not
        // prevent quarantine from publishing evidence against the lane start.
        let comparison_base = (|| -> Result<GitOid> {
            // Only the delivery remote's own refs count: a commit reachable
            // from another remote (a fork's origin, say) was never reviewed
            // for this delivery, so it must not be exempted.
            let head = provenant_confinement::run_utility(
                &executor,
                provenant_confinement::GitRead::RemoteHead {
                    remote: remote.into(),
                },
                self.worktree.repo(),
                &utility,
                budget()?,
                &cancel,
            )
            .map_err(error(
                "resolve writer remote HEAD",
                Some(self.worktree.path()),
            ))?;
            let name = if head.termination == provenant_os::exec::Termination::Exit(0)
                && !head.stdout.truncated
            {
                GitRefName::remote_tracking(
                    std::str::from_utf8(&head.stdout.bytes)
                        .map_err(error(
                            "decode writer remote HEAD",
                            Some(self.worktree.path()),
                        ))?
                        .trim()
                        .strip_prefix("refs/remotes/")
                        .ok_or(RunError::Validation("writer delivery target is not remote"))?,
                )
                .map_err(error(
                    "parse writer remote HEAD",
                    Some(self.worktree.path()),
                ))?
            } else {
                // No remote HEAD: use the lane's base branch on that remote.
                let base_ref = base_ref.ok_or(RunError::Validation(
                    "writer delivery target cannot be resolved",
                ))?;
                GitRefName::remote_tracking(format!("{remote}/{base_ref}"))
                    .map_err(error("parse writer base ref", Some(self.worktree.path())))?
            };
            let target = oid(
                self.worktree.repo(),
                provenant_confinement::GitRead::ResolveRef { name },
            )?;
            oid(
                self.worktree.repo(),
                provenant_confinement::GitRead::MergeBase {
                    left: target,
                    right: tip,
                },
            )
        })()
        .unwrap_or(snapshot.head());
        let outcome = git
            .quarantine_instructions(custody, &snapshot, comparison_base, &archive)
            .map_err(error(
                "quarantine writer instructions",
                Some(self.worktree.path()),
            ))?;
        let (evidence, review, protected_paths) = match outcome {
            provenant_confinement::quarantine::QuarantineOutcome::Restored {
                evidence_digest,
                ..
            } => (evidence_digest, false, Vec::new()),
            provenant_confinement::quarantine::QuarantineOutcome::ReviewRequired {
                evidence_digest,
                paths,
            } => (evidence_digest, true, paths),
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
        if oid(
            self.worktree.repo(),
            provenant_confinement::GitRead::ResolveRef {
                name: self.branch.clone(),
            },
        )? != tip
        {
            return Err(RunError::Validation("writer HEAD is not the lane branch"));
        }
        worktrees::check_writer_terminal(self.worktree.repo())?;
        crate::artifacts::seal(
            attempt,
            "writer-terminal.json",
            &crate::db::bytes(&WriterTerminal {
                schema_version: 1,
                head: tip,
                protected_paths,
            })?,
            crate::RECORD_LIMIT,
        )?;
        Ok(review)
    }
}
/// How many dirty paths a reason names before it counts the rest.
const UNCOMMITTED_LISTED: usize = 12;
impl WriterLaunch {
    /// A diagnostic naming the edits a writer left uncommitted, or `None` when
    /// it committed or its tree is clean. Call it after `terminal` succeeds.
    pub(crate) fn uncommitted(
        &self,
        state: &Path,
        custody: &provenant_supervisor::custody::Recovery,
        cancel: &CancelToken,
    ) -> Result<Option<String>> {
        let base = custody
            .writer_base(
                self.worktree.repo().root_dir(),
                self.worktree.repo().private_dir(),
                self.branch.as_str(),
            )
            .map_err(error(
                "read writer custody base",
                Some(self.worktree.path()),
            ))?;
        let utility = runtime(state)?;
        let executor =
            SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
        let git = provenant_confinement::Git::new(
            &executor,
            self.worktree.repo(),
            &utility,
            budget()?,
            cancel,
        );
        let paths = git.uncommitted_since(base).map_err(error(
            "read writer uncommitted changes",
            Some(self.worktree.path()),
        ))?;
        Ok(uncommitted_reason(&paths))
    }
}
fn uncommitted_reason(paths: &[Vec<u8>]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let listed: Vec<_> = paths
        .iter()
        .take(UNCOMMITTED_LISTED)
        .map(|path| String::from_utf8_lossy(path))
        .collect();
    let more = paths.len().saturating_sub(UNCOMMITTED_LISTED);
    let mut reason = format!(
        "writer left uncommitted edits and no new commit: {}",
        listed.join(", ")
    );
    if more > 0 {
        reason.push_str(&format!(" and {more} more"));
    }
    reason.truncate(reason.floor_char_boundary(1024));
    Some(reason)
}
#[cfg(test)]
mod uncommitted_tests {
    use super::*;

    #[test]
    fn reason_lists_dirty_paths_and_counts_the_rest() {
        assert_eq!(uncommitted_reason(&[]), None);
        let one = uncommitted_reason(&[b"a.rs".to_vec(), b"b/c.rs".to_vec()]);
        assert_eq!(
            one.as_deref(),
            Some("writer left uncommitted edits and no new commit: a.rs, b/c.rs")
        );
        let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n}").into_bytes()).collect();
        let reason = uncommitted_reason(&many).unwrap();
        assert!(
            reason.contains("f11") && !reason.contains("f12"),
            "{reason}"
        );
        assert!(reason.ends_with(" and 3 more"), "{reason}");
        let long = uncommitted_reason(&[vec![b'x'; 4000]]).unwrap();
        assert!(long.len() <= 1024);
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterTerminal {
    pub schema_version: u64,
    pub head: GitOid,
    pub protected_paths: Vec<provenant_records::PathWire>,
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

/// Launch-time lifecycle evidence, sealed while writer custody is held.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterOwnership {
    pub schema_version: u64,
    pub lane_owned: bool,
}
