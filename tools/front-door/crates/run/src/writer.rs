//! Writer identity and confinement composition. Supervisor owns the lease.
use crate::cargo_seed::codes;
use crate::{PathCheck, Result, RunError, WriterRequest};
use provenant_confinement::{
    ConfinedPlan, UtilityBudget, UtilityRuntime, VerifiedWorktree,
    permissions::{
        AccessMode, AnchoredDirectory, Anchors, BackendContext, ProfileLimits, ProviderProfile,
        prepare_writer_directories, required_anchors, system_backend_with_limits,
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

/// Distinguishes a refusal before custody from a failed withdrawal after it.
#[derive(Debug)]
pub(crate) enum WriterCustodyError {
    Admission(provenant_supervisor::custody::CustodyError),
    Withdrawal {
        admission: String,
        release: provenant_supervisor::custody::CustodyError,
    },
}

pub(crate) const SETUP_WAIT_LOCK: &str = "setup-wait.lock";

// Serialise queue changes separately from the long-lived setup lock. Each
// ticket is itself locked, so a killed waiter can be discarded by its peers.
fn queued_setup_lock(
    common: &Directory,
    wait_seconds: u64,
    cancel: &CancelToken,
    clock: &dyn provenant_core::Clock,
    limit: Option<provenant_core::WaitBudget>,
) -> Result<provenant_os::fs::LockGuard> {
    use provenant_core::{DurationMicros, Pause, ThreadPause, WaitBudget};
    use provenant_os::fs::LockMode;

    let now = clock.mono_now();
    let configured =
        WaitBudget::cli(now, wait_seconds).map_err(error("set writer setup lock budget", None))?;
    let budget = WaitBudget::up_to(
        now,
        limit.map_or(configured.remaining(now), |limit| {
            limit.remaining(now).min(configured.remaining(now))
        }),
    );
    let timeout = || RunError::SetupLockTimeout {
        seconds: wait_seconds,
    };
    let map_lock = |failure| match failure {
        FsError::LockTimeout { .. } => timeout(),
        failure => failure.into(),
    };
    let queue_name = OsStr::new("provenant-lane-setup-queue");
    match common.mkdir(queue_name, 0o700) {
        Ok(()) => {}
        Err(failure) if failure.is_exists() => {}
        Err(failure) => return Err(failure.into()),
    }
    let queue = common.open_directory(queue_name)?;
    let manager = OsStr::new("queue.lock");
    let guard = queue
        .lock(manager, LockMode::Exclusive, budget, cancel, clock)
        .map_err(map_lock)?;
    // Lock files are stable inodes. Disposable markers name a sequence and a
    // reusable slot; the queue mutex protects marker creation and removal.
    let scan = |own_slot: Option<u64>| -> Result<Vec<(u64, u64)>> {
        let mut active = Vec::new();
        for item in queue.entries()? {
            let item = item?;
            let Some((sequence, slot)) = item
                .to_str()
                .and_then(|name| name.strip_prefix("ticket-"))
                .and_then(|name| name.split_once('-'))
                .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)))
            else {
                continue;
            };
            if Some(slot) == own_slot {
                active.push((sequence, slot));
                continue;
            }
            let slot_name = format!("slot-{slot}.lock");
            match queue.try_lock_existing(OsStr::new(&slot_name), LockMode::Exclusive) {
                Ok(_stale) => queue.unlink(&item)?,
                Err(FsError::LockTimeout { .. }) => active.push((sequence, slot)),
                Err(failure) => return Err(failure.into()),
            }
        }
        Ok(active)
    };
    let active = scan(None)?;
    let sequence = active
        .iter()
        .map(|(sequence, _)| *sequence)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(RunError::Validation("setup queue exhausted"))?;
    let slot = (0..)
        .find(|slot| !active.iter().any(|(_, used)| used == slot))
        .ok_or(RunError::Validation("setup queue exhausted"))?;
    let slot_name = format!("slot-{slot}.lock");
    let held = queue.try_lock(OsStr::new(&slot_name), LockMode::Exclusive)?;
    let ticket = format!("ticket-{sequence}-{slot}");
    let ticket_name = OsStr::new(&ticket);
    queue.create_new(ticket_name)?;
    drop(guard);
    loop {
        let guard = queue
            .lock(manager, LockMode::Exclusive, budget, cancel, clock)
            .map_err(map_lock)?;
        if budget.remaining(clock.mono_now()).as_micros() == 0 {
            queue.unlink(ticket_name)?;
            return Err(timeout());
        }
        let first = scan(Some(slot))?
            .iter()
            .all(|(other, _)| *other >= sequence);
        if first {
            match common
                .try_lock_existing(OsStr::new("provenant-lane-setup.lock"), LockMode::Exclusive)
            {
                Ok(setup) => {
                    queue.unlink(ticket_name)?;
                    drop(held);
                    return Ok(setup);
                }
                Err(FsError::LockTimeout { .. }) => {}
                Err(failure) => return Err(failure.into()),
            }
        }
        drop(guard);
        ThreadPause.wait(DurationMicros::from_micros(20_000), cancel);
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_IMPORT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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
/// Retain the anchor for filesystem errors raised by held-directory helpers.
fn anchor_filesystem(error: RunError, anchor: &Path) -> RunError {
    match error {
        RunError::Filesystem(error) => anchored_error(error, anchor),
        other => other,
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
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    UtilityRuntime::new(&git, home.clone())
        .map(|runtime| {
            runtime.with_lane_roots(provenant_confinement::utility::lane_state_roots(state))
        })
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
/// Resolve a read-only graph source in the selected Git project. Unlike writer
/// authority, a reader may use the primary checkout or a detached worktree.
pub fn resolve_reader_source(state: &Path, cwd: &Path, path: &Path) -> Result<PathBuf> {
    let utility = runtime(state)?;
    let executor =
        SystemExecutor::new(1).map_err(error("initialise source system executor", None))?;
    let selected = utility
        .project_location(&executor, cwd, budget()?, &CancelToken::new())
        .map_err(error("locate source project", Some(cwd)))?;
    let repo = selected
        .repository
        .ok_or(RunError::Validation("source needs a Git project"))?;
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
    // A graph may have been invoked from a subdirectory. Bind its default
    // source to the verified repository root rather than treating that
    // subdirectory as a separate worktree.
    if canonicalize(cwd)? == path && path.starts_with(repo.root_path()) {
        return Ok(repo.root_path().to_path_buf());
    }
    // The verified invocation root can be the primary checkout, including
    // one with a separate Git directory. A Git directory's parent is not
    // evidence of a checkout: it may contain unrelated host files.
    if path == repo.root_path() {
        return Ok(path);
    }
    provenant_confinement::verify_worktree(&repo, &path)
        .map(|worktree| worktree.path().to_path_buf())
        .map_err(worktrees::WorktreeError::from)
        .map_err(Into::into)
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
            extra: Default::default(),
            project: project.to_path_buf(),
            worktree: path,
        });
    }
    let worktree = provenant_confinement::verify_worktree(&repo, &path)
        .map_err(worktrees::WorktreeError::from)?;
    Ok(WriterRequest {
        extra: Default::default(),
        project: project.to_path_buf(),
        worktree: worktree.path().to_path_buf(),
    })
}

pub struct WriterLaunch {
    pub worktree: VerifiedWorktree,
    pub branch: GitRefName,
    pub(crate) base: GitOid,
    pub(crate) custody: Option<(Directory, WriterBinding)>,
    /// Paths the project lets this writer change without review, read from the
    /// host config when the launch was prepared.
    pub(crate) review_exempt: provenant_confinement::review_exempt::ReviewExempt,
}
impl WriterLaunch {
    /// The verified branch head captured while preparing this writer.
    pub fn source_commit(&self) -> GitOid {
        self.base
    }

    /// Bind a graph writer to its immutable source. The instruction snapshot
    /// rechecks this prepared base while holding writer custody before launch.
    pub fn require_source(&self, expected: GitOid) -> Result<()> {
        if self.base != expected {
            return Err(RunError::WriterSourceMismatch {
                expected,
                actual: self.base,
            });
        }
        Ok(())
    }

    /// Bind a graph writer continuation to its predecessor head, verifying that
    /// the predecessor head descends from the original lineage source commit.
    pub fn require_continuation_source(
        &self,
        git: &Path,
        source: GitOid,
        predecessor: GitOid,
    ) -> Result<()> {
        if self.base != predecessor {
            return Err(RunError::WriterSourceMismatch {
                expected: predecessor,
                actual: self.base,
            });
        }
        let repo = self.worktree.repo();
        let descends = crate::objects::command(
            git,
            repo.common_git_dir(),
            repo.root_path(),
            &[
                "merge-base".into(),
                "--is-ancestor".into(),
                source.to_string().into(),
                predecessor.to_string().into(),
            ],
            None,
        )
        .is_ok();
        if !descends {
            return Err(RunError::WriterSourceMismatch {
                expected: source,
                actual: self.base,
            });
        }
        Ok(())
    }

    /// List paths with uncommitted predecessor edits in the writer worktree.
    pub fn uncommitted_paths(&self, git: &Path) -> Result<Vec<String>> {
        let repo = self.worktree.repo();
        let output = crate::objects::command(
            git,
            repo.private_git_dir(),
            repo.root_path(),
            &[
                "--work-tree".into(),
                repo.root_path().into(),
                "status".into(),
                "--porcelain".into(),
                "-z".into(),
            ],
            None,
        )?;
        let mut records = output.split(|byte| *byte == 0);
        let mut paths = Vec::new();
        while let Some(record) = records.next() {
            if record.is_empty() {
                break;
            }
            if record.len() < 4 || record[2] != b' ' {
                return Err(RunError::Validation("invalid Git status record"));
            }
            paths.push(String::from_utf8_lossy(&record[3..]).into_owned());
            // Porcelain -z puts the destination first and follows it with the source.
            if record[..2]
                .iter()
                .any(|status| matches!(status, b'R' | b'C'))
                && records.next().is_none_or(|source| source.is_empty())
            {
                return Err(RunError::Validation("invalid Git status rename"));
            }
        }
        Ok(paths)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ImportFence {
    schema_version: u64,
    scope: provenant_supervisor::custody::Scope,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MergeRestored {
    pub schema_version: u64,
    pub scope: provenant_supervisor::custody::Scope,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GitPins {
    schema_version: u64,
    scope: provenant_supervisor::custody::Scope,
    base: GitOid,
}

/// Remove only this attempt's host-owned retention ref after terminal commit.
/// The head-keyed import ref belongs to publish_branch's custody-protected CAS;
/// a delayed cleanup must never delete another lane's in-progress import.
pub(crate) fn cleanup_git_pins(
    state: &Path,
    writer: &WriterRequest,
    attempt: &Directory,
    scope: provenant_supervisor::custody::Scope,
    head: Option<GitOid>,
    remove_import_pin: bool,
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
    if remove_import_pin && let Some(head) = head {
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
fn writer_branch(worktree: &VerifiedWorktree) -> Result<GitRefName> {
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
    GitRefName::new(
        head.trim()
            .strip_prefix("ref: ")
            .ok_or(RunError::Validation("writer needs a branch"))?,
    )
    .map_err(error("parse writer branch", Some(&head_path)))
}

/// Reopen the persisted scope and verify current Git worktree membership.
/// The subsequent supervisor writer flock supplies exclusion.
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
    let branch = writer_branch(&worktree)?;
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
    let projects = crate::artifacts::subdir(&state_dir, OsStr::new("worktrees"), true)
        .map_err(|error| anchor_filesystem(error, state))?;
    let project_state = crate::artifacts::subdir(&projects, OsStr::new(&project.to_string()), true)
        .map_err(|error| anchor_filesystem(error, state))?;
    let (directory, binding) =
        worktrees::writer_custody(&project_state, &worktree, &branch, base, || {
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
        review_exempt: provenant_confinement::run_config::RunConfig::review_exempt_at(
            state,
            &project.to_string(),
        ),
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
    pub(crate) fn prepare_git(
        &mut self,
        attempt: &Directory,
        cancel: &CancelToken,
        setup_budget: Option<provenant_core::WaitBudget>,
    ) -> Result<Option<provenant_os::fs::LockGuard>> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(None);
        };
        writer
            .worktree
            .repo()
            .revalidate_identity()
            .map_err(|error| {
                RunError::Confinement(provenant_confinement::permissions::PermissionError::from(
                    error,
                ))
            })?;
        let AccessMode::WorktreeWrite(authority) = &self.plan.permissions().mode else {
            return Ok(None);
        };
        prepare_writer_git(
            writer,
            &self.state_root,
            &self.home,
            Some(authority),
            Some(attempt),
            cancel,
            setup_budget,
        )
        .map(Some)
    }
    /// Prepare the private Cargo caches while writer custody is held.
    pub(crate) fn seed_cargo(&self, environment: &[(OsString, OsString)], attempt_dir: &Directory) {
        let writer = self.writer.as_ref().map(|w| (&w.worktree, &w.branch));
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
                    && let Err(detail) = verified_cargo_config(
                        worktree.path(),
                        &authority.cargo_cache().path,
                        environment,
                    )
                    .and_then(|()| {
                        trusted_lane_toolchain(
                            &self.home,
                            worktree.path(),
                            environment,
                            &provenant_confinement::write_grants(
                                self.plan.permissions(),
                                &self.profile,
                            ),
                        )
                    }) =>
            {
                crate::cargo_seed::Outcome::fallback_detail("toolchain_unverified", detail)
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
    }
    pub(crate) fn command(
        &self,
        argv: &[OsString],
        attempt: &provenant_core::AttemptId,
        cancel: &CancelToken,
    ) -> Result<Vec<OsString>> {
        let executor =
            SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
        let config = provenant_confinement::run_config::RunConfig::read(&self.state_root)
            .map_err(error("read launch resource limits", None))?;
        let limits = ProfileLimits {
            grant_limit: config.grant_limit,
            profile_limit_bytes: config.profile_limit_bytes,
        };
        let backend = system_backend_with_limits(
            &BackendContext {
                executor: &executor,
                cancel,
                #[cfg(target_os = "macos")]
                env: &[],
            },
            limits,
        )
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
    attempt: Option<&Directory>,
    cancel: &CancelToken,
    limit: Option<provenant_core::WaitBudget>,
) -> Result<provenant_os::fs::LockGuard> {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::{clock::SystemClock, fs::LockMode};
    let utility = runtime(state)?;
    let root_identity = writer.worktree.identity();
    let private_identity = writer.worktree.repo().private_identity();
    let common_identity = writer.worktree.repo().common_identity();
    let executor =
        SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
    // Independent worktrees share config. Keep both writes and all subsequent
    // verification under one project-wide lock; a later launch repairs either
    // key if a process stopped between the two writes.
    let clock = SystemClock::new();
    let wait_seconds = provenant_config::LaneBuildSettings::read(state)
        .map_err(error("read Git setup lock settings", None))?
        .setup_lock_wait_seconds();
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
    // The marker is a lock, not a persistent assertion: if the launcher dies,
    // observers immediately stop reporting a wait even if the file remains.
    let waiting = attempt.and_then(|directory| {
        directory
            .lock(
                OsStr::new(SETUP_WAIT_LOCK),
                LockMode::Exclusive,
                WaitBudget::up_to(
                    clock.mono_now(),
                    provenant_core::DurationMicros::from_micros(1_000_000),
                ),
                cancel,
                &clock,
            )
            .ok()
    });
    let config_lock =
        queued_setup_lock(setup_dir, wait_seconds, cancel, &clock, limit).map_err(|error| {
            match error {
                RunError::SetupLockTimeout { .. } if limit.is_some() => {
                    RunError::LaneSetupLockTimeout
                }
                other => other,
            }
        })?;
    drop(waiting);
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
        .open_repo(&executor, &project, budget()?, cancel)
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
            extra: Default::default(),
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
            cancel,
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
            .open_repo(&executor, &project, budget()?, cancel)
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
            cancel,
        )
        .map_err(error("set writer upstream", Some(&path)))?;
    }
    let repo = utility
        .open_repo(&executor, &project, budget()?, cancel)
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
// Refusals are stable snake_case codes, recorded as the outcome `detail`.
fn verified_cargo_config(
    worktree: &Path,
    cache: &Path,
    environment: &[(OsString, OsString)],
) -> std::result::Result<(), &'static str> {
    for path in [worktree.join("tools"), worktree.join("tools/front-door")] {
        if !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(codes::WORKTREE_LAYOUT_UNEXPECTED);
        }
    }
    let cargo_home = cache.join("cargo-home");
    let mut values = environment.iter().filter(|(key, _)| key == "CARGO_HOME");
    match (values.next(), values.next()) {
        (Some((_, value)), None) if Path::new(value) == cargo_home => {}
        (None, _) => return Err(codes::CARGO_HOME_ENV_MISSING),
        _ => return Err(codes::CARGO_HOME_ENV_UNEXPECTED),
    }
    let present = |directory: &Path| {
        ["config", "config.toml"]
            .iter()
            .map(|name| directory.join(name))
            .filter(|path| {
                !matches!(std::fs::symlink_metadata(path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound)
            })
            .collect::<Vec<_>>()
    };
    // The lane's own Cargo home is lane-writable state: any file there is refused.
    if !present(&cargo_home).is_empty() {
        return Err(codes::CARGO_HOME_CONFIG_PRESENT);
    }
    // A checked-in `.cargo/config.toml` is ordinary (this repository has one).
    // It is read as data and refused only if it can select the compiler.
    for ancestor in worktree.join("tools/front-door").ancestors() {
        for path in present(&ancestor.join(".cargo")) {
            cargo_config_cannot_select_compiler(&path)?;
        }
    }
    Ok(())
}

// Read a Cargo config file as data, without executing it. A config is refused
// when it is not a small regular UTF-8 TOML file or when it can change which
// compiler runs: `build.rustc*`, an `include`, or `[env]` entries that reach
// rustup, rustc or the search path.
fn cargo_config_cannot_select_compiler(path: &Path) -> std::result::Result<(), &'static str> {
    use std::io::Read;
    if !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Err(codes::CARGO_CONFIG_UNREADABLE);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(65537).read_to_end(&mut bytes))
        .map_err(|_| codes::CARGO_CONFIG_UNREADABLE)?;
    if bytes.len() > 65536 {
        return Err(codes::CARGO_CONFIG_TOO_LARGE);
    }
    let document: toml_edit::DocumentMut = String::from_utf8(bytes)
        .map_err(|_| codes::CARGO_CONFIG_INVALID)?
        .parse()
        .map_err(|_| codes::CARGO_CONFIG_INVALID)?;
    let compiler_key = |key: &str| {
        key.starts_with("RUSTUP_")
            || key.starts_with("RUSTC")
            || key.starts_with("CARGO_BUILD_RUSTC")
            || key == "PATH"
    };
    let build = document.get("build").and_then(|item| item.as_table_like());
    let env = document.get("env").and_then(|item| item.as_table_like());
    if document.contains_key("include")
        || build.is_some_and(|table| table.iter().any(|(key, _)| key.starts_with("rustc")))
        || env.is_some_and(|table| table.iter().any(|(key, _)| compiler_key(key)))
        || (document.contains_key("build") && build.is_none())
        || (document.contains_key("env") && env.is_none())
    {
        return Err(codes::CARGO_CONFIG_SELECTS_COMPILER);
    }
    Ok(())
}

// Refuse the host probe if a lane can alter the host compiler or an earlier
// PATH entry, or if its rustc does not resolve to the host rustup shim.
fn trusted_lane_toolchain(
    home: &Path,
    worktree: &Path,
    environment: &[(OsString, OsString)],
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> std::result::Result<(), &'static str> {
    use provenant_confinement::permissions::{PathScope, WriteSource};
    use provenant_core::PathResolver;
    let resolve = |path: &Path| {
        provenant_os::fs::SystemPathResolver
            .resolve_existing_ancestor(path)
            .ok()
    };
    let Some(worktree) = resolve(worktree) else {
        return Err(codes::WORKTREE_UNRESOLVABLE);
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
    if !protected(&home.join(".rustup")) {
        return Err(codes::RUSTUP_HOME_WRITABLE);
    }
    if !protected(&home.join(".cargo/bin")) {
        return Err(codes::CARGO_BIN_WRITABLE);
    }
    let mut paths = environment.iter().filter(|(key, _)| key == "PATH");
    let Some((_, path)) = paths.next() else {
        return Err(codes::PATH_MISSING);
    };
    if paths.next().is_some() {
        return Err(codes::PATH_DUPLICATED);
    }
    let Ok(shim) = home.join(".cargo/bin/rustup").canonicalize() else {
        return Err(codes::RUSTUP_SHIM_UNRESOLVABLE);
    };
    if !protected(&shim) {
        return Err(codes::RUSTUP_SHIM_WRITABLE);
    }
    // The host puts the lane's `cargo` build-slot shim and shell hook in
    // `<scratch>/bin`, first on PATH. Scratch is writable, so the lane could add
    // a `rustc` there, but Cargo run through the rustup shim prepends the pinned
    // toolchain to PATH and resolves the compiler from it, so an entry here
    // cannot select another compiler. Any other writable entry is still refused,
    // and an existing `rustc` in this directory must be the rustup shim.
    let lane_shims: Vec<PathBuf> = grants
        .iter()
        .filter(|grant| grant.source == WriteSource::Scratch)
        .map(|grant| grant.path.join("bin"))
        .collect();
    for directory in std::env::split_paths(path) {
        if !directory.is_absolute() {
            return Err(codes::PATH_ENTRY_RELATIVE);
        }
        if !lane_shims.contains(&directory) && !protected(&directory) {
            return Err(codes::PATH_ENTRY_WRITABLE);
        }
        let executable = directory.join("rustc");
        match executable.canonicalize() {
            Ok(found) if found == shim => return Ok(()),
            Ok(_) => return Err(codes::RUSTC_NOT_RUSTUP_SHIM),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(codes::RUSTC_PATH_UNRESOLVABLE),
        }
    }
    Err(codes::RUSTC_NOT_ON_PATH)
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
    use provenant_os::fs::LockMode;

    #[test]
    #[allow(clippy::disallowed_methods)] // Exercise real concurrent waiters.
    fn setup_lock_serves_ten_slow_waiters_in_order()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let common = Directory::open_anchor(temp.path())?;
        let initial =
            common.try_lock(OsStr::new("provenant-lane-setup.lock"), LockMode::Exclusive)?;
        let (send, receive) = std::sync::mpsc::channel();
        let mut threads = Vec::new();
        for index in 0..10 {
            let path = temp.path().to_owned();
            let send = send.clone();
            threads.push(std::thread::spawn(move || {
                let common = Directory::open_anchor(&path).unwrap();
                let _lock = queued_setup_lock(
                    &common,
                    30,
                    &CancelToken::new(),
                    &provenant_os::clock::SystemClock::new(),
                    None,
                )
                .unwrap();
                send.send(index).unwrap();
                let start = std::time::Instant::now();
                provenant_test_support::wait_for(
                    "slow holder",
                    Duration::from_secs(5),
                    || start.elapsed(),
                    |elapsed| *elapsed >= Duration::from_millis(125),
                )
                .unwrap();
            }));
            provenant_test_support::wait_for(
                "waiter to queue",
                Duration::from_secs(5),
                || {
                    std::fs::read_dir(temp.path().join("provenant-lane-setup-queue"))
                        .map(|entries| {
                            entries
                                .filter_map(std::result::Result::ok)
                                .filter(|entry| {
                                    entry.file_name().to_string_lossy().starts_with("ticket-")
                                })
                                .count()
                        })
                        .unwrap_or(0)
                },
                |count| *count == index + 1,
            )?;
        }
        let released = std::time::Instant::now();
        drop(initial);
        let order: Vec<_> = (0..10)
            .map(|_| receive.recv_timeout(Duration::from_secs(30)).unwrap())
            .collect();
        // One second is a scaled old bound: the last waiter remains queued
        // beyond it rather than losing its place or being rejected.
        assert!(released.elapsed() >= Duration::from_secs(1));
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(order, (0..10).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Exercise a real process and wall-clock deadline.
    fn setup_lock_recovers_after_holder_dies() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        const CHILD_DIR: &str = "PROVENANT_TEST_SETUP_LOCK_CHILD_DIR";
        let name = OsStr::new("provenant-lane-setup.lock");
        if let Some(path) = std::env::var_os(CHILD_DIR) {
            let directory = Directory::open_anchor(Path::new(&path))?;
            let _held = directory.try_lock(name, LockMode::Exclusive)?;
            std::fs::write(Path::new(&path).join("ready"), b"ready")?;
            loop {
                std::thread::park();
            }
        }
        let temp = tempfile::tempdir()?;
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("writer::cargo_setup_tests::setup_lock_recovers_after_holder_dies")
            .env(CHILD_DIR, temp.path())
            .spawn()?;
        let ready = provenant_test_support::wait_for(
            "holder to acquire setup lock",
            Duration::from_secs(10),
            || (temp.path().join("ready").exists(), child.try_wait()),
            |(ready, exited)| *ready || !matches!(exited, Ok(None)),
        );
        if !matches!(ready, Ok((true, Ok(None)))) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("holder did not acquire the setup lock".into());
        }
        let directory = Directory::open_anchor(temp.path())?;
        let held = directory.try_lock_existing(name, LockMode::Exclusive);
        let path = temp.path().to_owned();
        let waiter = std::thread::spawn(move || {
            let directory = Directory::open_anchor(&path).map_err(|error| error.to_string())?;
            queued_setup_lock(
                &directory,
                30,
                &CancelToken::new(),
                &provenant_os::clock::SystemClock::new(),
                None,
            )
            .map(drop)
            .map_err(|error| error.to_string())
        });
        let queued = provenant_test_support::wait_for(
            "waiter to queue",
            Duration::from_secs(5),
            || {
                (
                    std::fs::read_dir(temp.path().join("provenant-lane-setup-queue"))
                        .map(|entries| {
                            entries.filter_map(std::result::Result::ok).any(|entry| {
                                entry.file_name().to_string_lossy().starts_with("ticket-")
                            })
                        })
                        .unwrap_or(false),
                    waiter.is_finished(),
                )
            },
            |(queued, finished)| *queued || *finished,
        )
        .is_ok_and(|(queued, _)| queued);
        child.kill()?;
        child.wait()?;
        assert!(queued, "waiter did not enter the queue");
        assert!(matches!(held, Err(FsError::LockTimeout { .. })));
        waiter.join().map_err(|_| "waiter panicked")??;
        Ok(())
    }

    #[test]
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
            let output = provenant_os::fixture::git_command_at(&git)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
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
        std::fs::write(project.join("old name.txt"), "tracked")?;
        run(&["add", "old name.txt"])?;
        run(&["commit", "--allow-empty", "-qm", "initial"])?;
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
            review_exempt: Default::default(),
        };
        // Use a real unrelated root, so both named commits exist.
        let tree = run(&["rev-parse", "HEAD^{tree}"])?;
        let unrelated = run(&["commit-tree", &tree, "-m", "unrelated root"])?;
        let other = GitOid::parse(&unrelated)?;
        let mismatch = writer
            .require_source(other)
            .err()
            .ok_or("different source must refuse")?;
        assert!(
            matches!(mismatch, RunError::WriterSourceMismatch { expected, actual } if expected == other && actual == writer.base)
        );
        let message = mismatch.to_string();
        assert!(message.contains(&other.to_string()));
        assert!(message.contains(&expected));
        writer.require_source(writer.base)?;
        // A continuation accepts when the predecessor descends from the original source.
        let child_commit = run(&["commit-tree", &tree, "-p", &expected, "-m", "child commit"])?;
        let child_oid = GitOid::parse(&child_commit)?;
        let source_oid = GitOid::parse(&expected)?;
        writer.base = child_oid;
        writer.require_continuation_source(utility.git_program_path(), source_oid, child_oid)?;
        // Refuses when HEAD does not descend from the pinned source, naming both commits.
        let ancestry_mismatch = writer
            .require_continuation_source(utility.git_program_path(), other, child_oid)
            .err()
            .ok_or("unrelated source must refuse")?;
        assert!(
            matches!(ancestry_mismatch, RunError::WriterSourceMismatch { expected, actual } if expected == other && actual == child_oid)
        );
        let ancestry_message = ancestry_mismatch.to_string();
        assert!(ancestry_message.contains(&other.to_string()));
        assert!(ancestry_message.contains(&child_oid.to_string()));
        // Refuses when the prepared base does not match the predecessor head, naming both commits.
        let base_mismatch = writer
            .require_continuation_source(utility.git_program_path(), source_oid, source_oid)
            .err()
            .ok_or("predecessor mismatch must refuse")?;
        assert!(
            matches!(base_mismatch, RunError::WriterSourceMismatch { expected, actual } if expected == source_oid && actual == child_oid)
        );
        let base_message = base_mismatch.to_string();
        assert!(base_message.contains(&source_oid.to_string()));
        assert!(base_message.contains(&child_oid.to_string()));
        assert_eq!(
            writer.uncommitted_paths(utility.git_program_path())?,
            Vec::<String>::new()
        );
        std::fs::write(lane.join("uncommitted.txt"), "leftover edit")?;
        assert_eq!(
            writer.uncommitted_paths(utility.git_program_path())?,
            vec!["uncommitted.txt".to_string()]
        );
        std::fs::remove_file(lane.join("uncommitted.txt"))?;
        let lane_git = |args: &[&str]| -> std::result::Result<(), Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command_at(&git)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&lane)
                .args(args)
                .output()?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
            }
            Ok(())
        };
        lane_git(&["mv", "old name.txt", "renamed.txt"])?;
        std::fs::write(lane.join("café.txt"), "untracked")?;
        assert_eq!(
            writer.uncommitted_paths(utility.git_program_path())?,
            vec!["renamed.txt", "café.txt"]
        );
        lane_git(&["mv", "renamed.txt", "old name.txt"])?;
        std::fs::remove_file(lane.join("café.txt"))?;
        writer.base = source_oid;
        let second_tree = provenant_confinement::verify_worktree(&repo, &second_lane)?;
        let mut second = WriterLaunch {
            worktree: second_tree,
            branch: GitRefName::new("refs/heads/feature/second")?,
            base: GitOid::parse(&expected)?,
            custody: None,
            review_exempt: Default::default(),
        };
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                prepare_writer_git(
                    &mut writer,
                    &state,
                    &root,
                    None,
                    None,
                    &CancelToken::new(),
                    None,
                )
                .map(drop)
                .map_err(|e| e.to_string())
            });
            let second = scope.spawn(|| {
                prepare_writer_git(
                    &mut second,
                    &state,
                    &root,
                    None,
                    None,
                    &CancelToken::new(),
                    None,
                )
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
        // Hold a real seed in its clone callback while another writer runs
        // Git setup. The handoff helper releases the actual repository lock.
        let cache = root.join("held-seed-cache");
        std::fs::create_dir(&cache)?;
        std::thread::scope(
            |scope| -> std::result::Result<(), Box<dyn std::error::Error>> {
                let held = prepare_writer_git(
                    &mut writer,
                    &state,
                    &root,
                    None,
                    None,
                    &CancelToken::new(),
                    None,
                )?;
                let (entered_tx, entered_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let root_ref = &root;
                let cache_ref = &cache;
                let seed = scope.spawn(move || {
                    let mut lock = Some(held);
                    crate::launch::hand_off::seed_without_setup_lock(&mut lock, || {
                        crate::cargo_seed::seed_with(root_ref, cache_ref, |_, _| {
                            entered_tx.send(()).unwrap();
                            provenant_test_support::wait_for(
                                "release seed",
                                Duration::from_secs(30),
                                || release_rx.try_recv().is_ok(),
                                |ready| *ready,
                            )
                            .unwrap();
                            Ok(())
                        });
                    });
                });
                provenant_test_support::wait_for(
                    "seed entered",
                    Duration::from_secs(30),
                    || entered_rx.try_recv().is_ok(),
                    |ready| *ready,
                )?;
                let result = prepare_writer_git(
                    &mut second,
                    &state,
                    &root,
                    None,
                    None,
                    &CancelToken::new(),
                    None,
                );
                release_tx.send(())?;
                result.map(drop)?;
                seed.join().map_err(|_| "seed panicked")?;
                Ok(())
            },
        )?;
        // A busy repository lock honours the configured bound and identifies
        // the setting that controls it, without changing the lock's inode.
        std::fs::write(
            state.join("config.toml"),
            "schema_version = 1\n[domains.fabric.lanes]\nsetup_lock_wait_seconds = 1\n",
        )?;
        let common = provenant_os::fs::Directory::open_anchor(&git_dir)?;
        let held = common.try_lock_existing(
            OsStr::new("provenant-lane-setup.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let attempt_path = root.join("attempt");
        std::fs::create_dir(&attempt_path)?;
        let attempt = Directory::open_anchor(&attempt_path)?;
        let refused = std::thread::scope(|scope| {
            let launch = scope.spawn(|| {
                prepare_writer_git(
                    &mut writer,
                    &state,
                    &root,
                    None,
                    Some(&attempt),
                    &CancelToken::new(),
                    None,
                )
            });
            loop {
                if matches!(
                    attempt.try_lock_existing(OsStr::new(SETUP_WAIT_LOCK), LockMode::Exclusive),
                    Err(FsError::LockTimeout { .. })
                ) {
                    break;
                }
                assert!(!launch.is_finished(), "launcher must publish its lock wait");
                std::thread::yield_now();
            }
            match launch.join().expect("launcher panicked") {
                Err(error) => error,
                Ok(_) => panic!("held Git setup lock must block preparation"),
            }
        });
        assert!(
            attempt
                .try_lock_existing(OsStr::new(SETUP_WAIT_LOCK), LockMode::Exclusive)
                .is_ok()
        );
        assert!(matches!(refused, RunError::SetupLockTimeout { seconds: 1 }));
        assert!(
            refused
                .to_string()
                .contains("domains.fabric.lanes.setup_lock_wait_seconds")
        );
        drop(held);
        std::fs::remove_file(state.join("config.toml"))?;
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
        let failure = prepare_writer_git(
            &mut writer,
            &state,
            &root,
            None,
            None,
            &CancelToken::new(),
            None,
        )
        .err()
        .ok_or("symlinked tracking reflog must refuse")?;
        assert!(failure.to_string().contains(&reflog.display().to_string()));
        assert_eq!(std::fs::read(&target)?, b"untouched");
        std::fs::remove_file(&reflog)?;
        let parent = reflog.parent().ok_or("reflog parent")?;
        std::fs::remove_dir(parent)?;
        std::os::unix::fs::symlink(&root, parent)?;
        let failure = prepare_writer_git(
            &mut writer,
            &state,
            &root,
            None,
            None,
            &CancelToken::new(),
            None,
        )
        .err()
        .ok_or("symlinked tracking reflog parent must refuse")?;
        assert!(failure.to_string().contains(&parent.display().to_string()));
        assert_eq!(std::fs::read(&target)?, b"untouched");
        std::fs::remove_file(parent)?;
        std::fs::create_dir_all(parent)?;
        let tracking_lock = git_dir.join("refs/remotes/origin/feature/nested.lock");
        std::fs::create_dir_all(tracking_lock.parent().ok_or("tracking lock parent")?)?;
        std::fs::write(&tracking_lock, b"held")?;
        let failure = prepare_writer_git(
            &mut writer,
            &state,
            &root,
            None,
            None,
            &CancelToken::new(),
            None,
        )
        .err()
        .ok_or("stale tracking lock must refuse")?;
        assert!(
            matches!(&failure, RunError::WriterRefLocked(path) if *path == tracking_lock),
            "{failure:?}"
        );
        assert_eq!(failure.code(), "WRITER_REF_LOCKED");
        assert_eq!(std::fs::read(&tracking_lock)?, b"held");
        std::fs::remove_file(&tracking_lock)?;
        run(&["remote", "remove", "origin"])?;
        let stale = git_dir.join("refs/heads/feature/nested.lock");
        std::fs::write(&stale, b"held")?;
        let failure = prepare_writer_git(
            &mut writer,
            &state,
            &root,
            None,
            None,
            &CancelToken::new(),
            None,
        )
        .err()
        .ok_or("stale lock must refuse")?;
        assert_eq!(failure.code(), "WRITER_REF_LOCKED", "{failure}");
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
        assert!(trusted_lane_toolchain(&home, &worktree, &env, &[]).is_ok());
        let grant = WriteGrant {
            source: WriteSource::WriteRoot,
            path: home.join(".rustup"),
            scope: PathScope::Subtree,
        };
        assert_eq!(
            trusted_lane_toolchain(&home, &worktree, &env, &[grant]),
            Err(codes::RUSTUP_HOME_WRITABLE)
        );
        let path = std::env::join_paths([&worktree, &bin]).unwrap();
        let unsafe_env = vec![(OsString::from("PATH"), path)];
        assert_eq!(
            trusted_lane_toolchain(&home, &worktree, &unsafe_env, &[]),
            Err(codes::PATH_ENTRY_WRITABLE)
        );
        std::fs::write(worktree.join("rustc"), b"untrusted").unwrap();
        assert_eq!(
            trusted_lane_toolchain(&home, &worktree, &unsafe_env, &[]),
            Err(codes::PATH_ENTRY_WRITABLE)
        );
    }

    // Regression for #2484: a real lane has the writable scratch `bin/` (the
    // build-slot `cargo` shim) first on PATH, which refused every lane.
    #[test]
    fn scratch_shim_directory_on_path_is_accepted_but_other_writable_entries_are_not() {
        use provenant_confinement::permissions::{PathScope, WriteGrant, WriteSource};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let worktree = tmp.path().join("worktree");
        let scratch = tmp.path().join("scratch");
        let bin = home.join(".cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(scratch.join("bin")).unwrap();
        std::fs::write(bin.join("rustup"), b"fixture").unwrap();
        std::os::unix::fs::symlink("rustup", bin.join("rustc")).unwrap();
        let grants = [WriteGrant {
            source: WriteSource::Scratch,
            path: scratch.clone(),
            scope: PathScope::Subtree,
        }];
        let path = std::env::join_paths([scratch.join("bin").as_path(), bin.as_path()]).unwrap();
        let env = vec![(OsString::from("PATH"), path)];
        assert_eq!(
            trusted_lane_toolchain(&home, &worktree, &env, &grants),
            Ok(())
        );
        // The scratch directory itself, or a sibling inside it, is not the shim directory.
        std::fs::create_dir(scratch.join("other")).unwrap();
        let other = std::env::join_paths([scratch.join("other").as_path(), bin.as_path()]).unwrap();
        assert_eq!(
            trusted_lane_toolchain(
                &home,
                &worktree,
                &[(OsString::from("PATH"), other)],
                &grants
            ),
            Err(codes::PATH_ENTRY_WRITABLE)
        );
        // A compiler already present in the shim directory must be the rustup shim.
        std::fs::write(scratch.join("bin/rustc"), b"untrusted").unwrap();
        assert_eq!(
            trusted_lane_toolchain(&home, &worktree, &env, &grants),
            Err(codes::RUSTC_NOT_RUSTUP_SHIM)
        );
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
        assert!(verified_cargo_config(&worktree, &cache, &env).is_ok());
        assert_eq!(
            verified_cargo_config(&worktree, &cache, &[]),
            Err(codes::CARGO_HOME_ENV_MISSING)
        );
        std::fs::write(
            cache.join("cargo-home/config.toml"),
            "[build]\nrustc = 'evil'\n",
        )
        .unwrap();
        assert_eq!(
            verified_cargo_config(&worktree, &cache, &env),
            Err(codes::CARGO_HOME_CONFIG_PRESENT)
        );
        std::fs::remove_file(cache.join("cargo-home/config.toml")).unwrap();
        std::fs::create_dir_all(worktree.join(".cargo")).unwrap();
        let config = worktree.join(".cargo/config.toml");
        // Selecting a compiler, directly or through include or [env], is refused.
        for (text, expected) in [
            (
                "[build]\nrustc = 'evil'\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            (
                "[build]\nrustc-wrapper = 'evil'\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            (
                "build.rustc-workspace-wrapper = 'evil'\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            (
                "include = 'elsewhere.toml'\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            (
                "[env]\nRUSTUP_TOOLCHAIN = 'nightly'\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            (
                "[env]\nPATH = { value = '/tmp', force = true }\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            ("build = 'rustc'\n", codes::CARGO_CONFIG_SELECTS_COMPILER),
            // Only text that is not TOML is "invalid".
            ("[build\n", codes::CARGO_CONFIG_INVALID),
        ] {
            std::fs::write(&config, text).unwrap();
            assert_eq!(
                verified_cargo_config(&worktree, &cache, &env),
                Err(expected),
                "{text:?}"
            );
        }
        std::fs::write(worktree.join(".cargo/config"), "[build]\nrustc = 'evil'\n").unwrap();
        std::fs::remove_file(&config).unwrap();
        assert_eq!(
            verified_cargo_config(&worktree, &cache, &env),
            Err(codes::CARGO_CONFIG_SELECTS_COMPILER)
        );
    }

    // Regression for #2484: this repository checks in `.cargo/config.toml`, which
    // refused every lane's target seed with `toolchain_unverified`. A config that
    // cannot select a compiler must verify.
    #[test]
    fn checked_in_cargo_config_that_cannot_select_a_compiler_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("worktree");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(worktree.join("tools/front-door/.cargo")).unwrap();
        std::fs::create_dir_all(cache.join("cargo-home")).unwrap();
        let env = vec![(
            OsString::from("CARGO_HOME"),
            cache.join("cargo-home").into_os_string(),
        )];
        let config = worktree.join("tools/front-door/.cargo/config.toml");
        for text in [
            "# comments only\n",
            "[alias]\nxtask = 'run -p xtask'\n[target.aarch64-apple-darwin]\nrustflags = ['-C', 'link-arg=-s']\n[env]\nFOO = 'bar'\n",
        ] {
            std::fs::write(&config, text).unwrap();
            assert_eq!(verified_cargo_config(&worktree, &cache, &env), Ok(()));
        }
        std::os::unix::fs::symlink(
            "/etc/hosts",
            worktree.join("tools/front-door/.cargo/config"),
        )
        .unwrap();
        assert_eq!(
            verified_cargo_config(&worktree, &cache, &env),
            Err(codes::CARGO_CONFIG_UNREADABLE)
        );
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
        // A fallback names the specific refusal; other outcomes carry no detail.
        let refusal = crate::cargo_seed::Outcome::fallback_detail(
            "toolchain_unverified",
            codes::PATH_ENTRY_WRITABLE,
        );
        record_cargo_setup_file(&directory, "cargo-target-setup.json", &refusal).unwrap();
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join("cargo-target-setup.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            record,
            serde_json::json!({"status":"fallback","reason":"toolchain_unverified","detail":codes::PATH_ENTRY_WRITABLE})
        );
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
    pub(crate) fn retained_import_fence(
        &self,
        scope: provenant_supervisor::custody::Scope,
    ) -> Result<bool> {
        let Some(fence) = self
            .writer_fences()?
            .into_iter()
            .find(|fence| fence.scope == scope)
        else {
            return Ok(false);
        };
        let parent = fence
            .reservation
            .parent()
            .ok_or(RunError::Validation("writer evidence missing"))?;
        let evidence = Directory::open_anchor(parent)?;
        let Some(raw) =
            crate::artifacts::read_sealed(&evidence, "import-retained.json", crate::RECORD_LIMIT)?
        else {
            return Ok(false);
        };
        let marker: ImportFence = crate::db::decode(&raw)?;
        if marker.schema_version != 1 || marker.scope != scope {
            return Err(RunError::Conflict);
        }
        Ok(true)
    }

    pub(crate) fn retain_import_fence(&self, record: &crate::AttemptRecord) -> Result<()> {
        let fence = self
            .writer_fences()?
            .into_iter()
            .find(|fence| fence.scope == Self::scope(record))
            .ok_or(RunError::Validation("retained writer reservation missing"))?;
        let parent = fence
            .reservation
            .parent()
            .ok_or(RunError::Validation("writer evidence missing"))?;
        let evidence = Directory::open_anchor(parent)?;
        crate::artifacts::seal(
            &evidence,
            "import-retained.json",
            &crate::db::bytes(&ImportFence {
                schema_version: 1,
                scope: Self::scope(record),
            })?,
            crate::RECORD_LIMIT,
        )
    }

    /// Check predecessor import fences after acquisition, while the returned
    /// lease owns writer.lock. The evidence lives with custody, so this also
    /// protects writers launched from a different RunStore sharing the registry.
    pub(crate) fn acquire_writer_fenced(
        &self,
        directory: Directory,
        scope: provenant_supervisor::custody::Scope,
        binding: WriterBinding,
        acquire: &dyn Fn(
            Directory,
            provenant_supervisor::custody::Scope,
            WriterBinding,
        ) -> provenant_supervisor::custody::Result<
            provenant_supervisor::custody::CustodyLease,
        >,
        call: &crate::Request<'_>,
    ) -> std::result::Result<provenant_supervisor::custody::CustodyLease, WriterCustodyError> {
        let slot = directory
            .try_clone()
            .map_err(|e| WriterCustodyError::Admission(e.into()))?;
        let lease = acquire(directory, scope, binding).map_err(WriterCustodyError::Admission)?;
        let mut retained_scope = None;
        let checked = (|| -> Result<()> {
            call.check()?;
            let metadata = slot.metadata()?;
            let listing = self.writer_fence_listing()?;
            for issue in listing.issues {
                for parent in issue.reservation.ancestors().skip(1).take(2) {
                    if let Ok(directory) = Directory::open_anchor(parent) {
                        let other = directory.metadata()?;
                        if metadata.device == other.device && metadata.inode == other.inode {
                            return Err(RunError::Validation(
                                "predecessor writer fence is unreadable",
                            ));
                        }
                    }
                }
            }
            for fence in listing.reservations {
                if fence.scope == scope {
                    continue;
                }
                let Some(path) = fence.custody_directory() else {
                    return Err(RunError::Validation("writer fence slot missing"));
                };
                let other = Directory::open_anchor(path)?.metadata()?;
                if metadata.device != other.device || metadata.inode != other.inode {
                    continue;
                }
                let parent = fence
                    .reservation
                    .parent()
                    .ok_or(RunError::Validation("writer fence evidence missing"))?;
                let evidence = Directory::open_anchor(parent)?;
                if let Some(raw) = crate::artifacts::read_sealed(
                    &evidence,
                    "import-retained.json",
                    crate::RECORD_LIMIT,
                )? {
                    let retained: ImportFence = crate::db::decode(&raw)?;
                    if retained.schema_version != 1 || retained.scope != fence.scope {
                        return Err(RunError::Conflict);
                    }
                    retained_scope = Some(fence.scope);
                    return Ok(());
                }
            }
            Ok(())
        })();
        if let Err(error) = checked {
            lease
                .withdraw_unlaunched()
                .map_err(|release| WriterCustodyError::Withdrawal {
                    admission: error.to_string(),
                    release,
                })?;
            return Err(WriterCustodyError::Admission(
                provenant_supervisor::custody::CustodyError::Io(std::io::Error::other(
                    error.to_string(),
                )),
            ));
        }
        if let Some(retained) = retained_scope {
            let admission = provenant_supervisor::custody::CustodyError::ImportRetained {
                run: retained.run,
                attempt: retained.attempt,
            };
            lease
                .withdraw_unlaunched()
                .map_err(|release| WriterCustodyError::Withdrawal {
                    admission: admission.to_string(),
                    release,
                })?;
            return Err(WriterCustodyError::Admission(admission));
        }
        Ok(lease)
    }

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
                let parent = fence.custody_directory().ok_or_else(|| {
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
        let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
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
        seal_writer_ownership(attempt, &self.worktree)?;
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
        let paths = git
            .writer_instruction_paths(&self.review_exempt)
            .map_err(error(
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
        let git_executable = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
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
            return Err(RunError::WriterHeadNotLaneBranch);
        }
        let import_cancel = settlement.cancellation();
        let merge_objects = source
            .as_ref()
            .map(|source| crate::objects::merge_objects(source, self.worktree.repo()))
            .transpose()?
            .unwrap_or_default();
        if settlement.phase() == crate::settlement::Phase::Import {
            settlement.begin_import()?;
            #[cfg(test)]
            if FAIL_IMPORT.with(|fault| fault.get()) {
                return Err(RunError::Io(std::io::Error::other(
                    "injected Git import failure",
                )));
            }
            crate::objects::settle_many(
                &git_executable,
                self.worktree.repo(),
                source.as_ref().map(|source| &source.objects),
                verified_tip,
                &merge_objects,
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
            if !merge_objects.is_empty()
                && crate::objects::resolve_ref(
                    &git_executable,
                    self.worktree.repo(),
                    self.branch.as_str(),
                )? != verified_tip
            {
                return Err(RunError::Validation(
                    "writer branch advanced before stopped merge could be restored",
                ));
            }
            if let Some(source) = &source
                && crate::objects::resolve_ref(
                    &git_executable,
                    self.worktree.repo(),
                    self.branch.as_str(),
                )? == verified_tip
            {
                crate::objects::restore_index(source, self.worktree.repo())?;
                if !merge_objects.is_empty() {
                    crate::artifacts::seal(
                        attempt,
                        "merge-restored.json",
                        &crate::db::bytes(&MergeRestored {
                            schema_version: 1,
                            scope: custody.scope(),
                        })?,
                        crate::RECORD_LIMIT,
                    )?;
                }
                crate::objects::clear_import_refs(
                    &git_executable,
                    self.worktree.repo(),
                    custody.scope().attempt,
                    &merge_objects,
                )?;
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
        if tip != verified_tip
            && !crate::objects::is_ancestor(
                &git_executable,
                self.worktree.repo(),
                verified_tip,
                tip,
            )
        {
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
                    right: verified_tip,
                },
            )
        })()
        .unwrap_or(snapshot.head());
        let outcome = git
            .quarantine_instructions_at(
                custody,
                &snapshot,
                comparison_base,
                verified_tip,
                &archive,
                &self.review_exempt,
            )
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
        crate::holder::seal_manifest(
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
            return Err(RunError::WriterHeadNotLaneBranch);
        }
        if !merge_objects.is_empty() {
            let source = source
                .as_ref()
                .ok_or(RunError::Validation("merge source missing"))?;
            worktrees::check_writer_terminal_with_merge(
                self.worktree.repo(),
                &crate::objects::merge_head(source)?,
            )?;
        } else {
            worktrees::check_writer_terminal(self.worktree.repo())?;
        }
        crate::artifacts::seal(
            attempt,
            "writer-terminal.json",
            &crate::db::bytes(&WriterTerminal {
                schema_version: 1,
                head: verified_tip,
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
        let paths = git
            .uncommitted_since(base, &self.review_exempt)
            .map_err(error(
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
        let one = uncommitted_reason(&[b"a.rs".to_vec(), b"b/c.rs".to_vec()]).unwrap();
        assert!(one.starts_with("writer left uncommitted edits"));
        assert!(one.contains("a.rs") && one.contains("b/c.rs"), "{one}");
        let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n:02}").into_bytes()).collect();
        let reason = uncommitted_reason(&many).unwrap();
        assert!(reason.starts_with("writer left uncommitted edits"));
        assert!(reason.len() <= 1024);
        let listed = (0..15)
            .filter(|n| reason.contains(&format!("f{n:02}")))
            .count();
        let omitted: usize = reason
            .split_whitespace()
            .rev()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(listed + omitted, many.len(), "{reason}");
        let long = uncommitted_reason(&[vec![b'x'; 4000]]).unwrap();
        assert!(long.len() <= 1024);
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
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

/// Preserve pre-upgrade launch evidence on replay without weakening its check.
pub(crate) fn seal_writer_ownership(
    attempt: &Directory,
    tree: &provenant_confinement::VerifiedWorktree,
) -> Result<()> {
    let owner = worktrees::load_owner(tree)?;
    let owned = owner
        .as_ref()
        .is_some_and(|owner| owner.origin == worktrees::WorktreeOrigin::Lane);
    let ownership = WriterOwnership {
        schema_version: 1,
        lane_owned: owned,
        owner,
    };
    if let Some(raw) =
        crate::artifacts::read_sealed(attempt, "writer-owned.json", crate::RECORD_LIMIT)?
    {
        let prior: WriterOwnership = crate::db::decode(&raw)?;
        let fields: serde_json::Value = crate::db::decode(&raw)?;
        let has_owner = fields
            .as_object()
            .is_some_and(|object| object.contains_key("owner"));
        if prior.schema_version != 1
            || prior.lane_owned != ownership.lane_owned
            || (has_owner && prior.owner != ownership.owner)
        {
            return Err(RunError::Conflict);
        }
        Ok(())
    } else {
        crate::artifacts::seal(
            attempt,
            "writer-owned.json",
            &crate::db::bytes(&ownership)?,
            crate::RECORD_LIMIT,
        )
    }
}

/// Launch-time lifecycle evidence, sealed while writer custody is held.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct WriterOwnership {
    pub schema_version: u64,
    pub lane_owned: bool,
    #[serde(default)]
    pub owner: Option<worktrees::WorktreeOwner>,
}

#[cfg(test)]
mod custody_admission_tests {
    use super::*;
    use crate::tests::{call, clock, context};
    use provenant_core::{AttemptId, RunId};
    use provenant_supervisor::custody::{self, Scope};

    #[test]
    fn failed_admission_withdrawal_retains_a_protected_error()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = crate::RunStore::open(&context(temp.path())?, &call)?;
        let path = temp.path().join("slot");
        std::fs::create_dir(&path)?;
        let scope = Scope {
            run: RunId::from_bytes([3; 16]),
            attempt: AttemptId::from_bytes([4; 16]),
            generation: 1,
        };
        let result = store.acquire_writer_fenced(
            Directory::open_anchor(&path)?,
            scope,
            WriterBinding::read_only(),
            &|directory, scope, binding| {
                let lease = custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                )?;
                std::fs::create_dir(path.join(scope.attempt.to_string()).join("settlement.json"))?;
                cancel.cancel();
                Ok(lease)
            },
            &call,
        );
        assert!(matches!(result, Err(WriterCustodyError::Withdrawal { .. })));
        assert!(custody::reservation_present(
            &Directory::open_anchor(&path)?,
            scope
        )?);
        Ok(())
    }
}
