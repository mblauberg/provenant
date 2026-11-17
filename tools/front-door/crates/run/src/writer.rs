//! Writer identity and confinement composition. Supervisor owns the lease.
use crate::cargo_seed::codes;
use crate::{PathCheck, Result, RunError, WriterRequest};
use provenant_confinement::{
    ConfinedPlan, UtilityBudget, UtilityRuntime, VerifiedWorktree,
    permissions::{
        AccessMode, AnchoredDirectory, Anchors, Availability, AvailabilityReason, BackendContext,
        ConfinementBackend, PermissionError, ProfileLimits, ProviderProfile,
        prepare_writer_directories, required_anchors, system_backend_with_limits_and_policy,
    },
    repo::GitRefName,
    worktrees,
};
use provenant_core::{CancelToken, Clock, GitOid, ProjectId};
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

/// Held while the writer's Cargo cache is seeded, so observers can name the phase.
pub(crate) const SEED_WAIT_LOCK: &str = "seed-wait.lock";

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
/// A utility failure keeps its fixed cause, when it has one, beside its text.
fn utility_error(
    operation: &'static str,
    path: Option<&Path>,
) -> impl FnOnce(provenant_confinement::utility::UtilityError) -> RunError {
    let path = path.map(Path::to_path_buf);
    move |source| RunError::OperationIo {
        operation,
        path,
        source: crate::CausedSource::io(source.to_string(), source.safe_cause()),
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
    UtilityRuntime::new(&git, home.clone(), state)
        .map(|runtime| {
            runtime.with_lane_roots(provenant_confinement::utility::lane_state_roots(state))
        })
        .map_err(error("initialise writer Git runtime", Some(&home)))
}
fn budget() -> Result<UtilityBudget> {
    UtilityBudget::bounded(Duration::from_secs(5))
        .map_err(error("set writer Git utility budget", None))
}

fn project_location(
    repo: &provenant_confinement::repo::VerifiedRepo,
) -> Result<provenant_config::ProjectLocation> {
    if repo.private_git_dir() == repo.common_git_dir() {
        Ok(
            provenant_config::ProjectLocation::from_verified_primary_checkout(
                repo.common_git_dir(),
                repo.root_path(),
            )
            .map_err(error(
                "resolve writer project location",
                Some(repo.common_git_dir()),
            ))?,
        )
    } else {
        Ok(
            provenant_config::ProjectLocation::new(repo.common_git_dir()).map_err(error(
                "resolve writer project location",
                Some(repo.common_git_dir()),
            ))?,
        )
    }
}

fn registered_project_layout(
    state: &Path,
    repo: &provenant_confinement::repo::VerifiedRepo,
    executor: &dyn provenant_os::exec::Executor,
    utility: &UtilityRuntime,
    cancel: &CancelToken,
) -> Result<(provenant_config::ProjectBinding, worktrees::ProjectLayout)> {
    if repo.private_git_dir() != repo.common_git_dir() {
        // A linked checkout is a Git executor only after Git's inventory
        // proves that it belongs to this common repository.
        provenant_confinement::verify_worktree(repo, repo.root_path())
            .map_err(worktrees::WorktreeError::from)?;
    }
    let location = project_location(repo)?;
    let binding = provenant_config::lookup_project_at_state(state, &location)
        .map_err(error(
            "read writer project binding",
            Some(repo.common_git_dir()),
        ))?
        .ok_or(RunError::Validation(
            "writer custody project is not registered",
        ))?;
    let layout = utility
        .project_layout_for_repository(executor, repo, budget()?, cancel)
        .map_err(utility_error(
            "resolve writer project layout",
            Some(repo.common_git_dir()),
        ))?;
    Ok((binding, layout))
}

fn same_project_layout(
    expected: &worktrees::ProjectLayout,
    actual: &worktrees::ProjectLayout,
) -> Result<bool> {
    if expected.root() != actual.root() || expected.project() != actual.project() {
        return Ok(false);
    }
    Ok(expected
        .directory()
        .metadata()?
        .same_identity(&actual.directory().metadata()?))
}

/// Resolve relative paths from the selected repository's primary root. Git's
/// linked-worktree registration proves membership and excludes the primary.
pub fn resolve_writer(state: &Path, cwd: &Path, path: &Path) -> Result<WriterRequest> {
    resolve_writer_with(state, cwd, path, true)
}
/// Resolve a read-only graph source in the selected Git project. Unlike writer
/// authority, a reader may use the primary checkout or a detached worktree.
pub fn resolve_reader_source(state: &Path, cwd: &Path, path: &Path) -> Result<PathBuf> {
    // Source admission also serves `run --explain`: verification must not
    // allocate product state or a writer Git home.
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    let utility = UtilityRuntime::read_only(&git, state)
        .map_err(error("initialise source Git runtime", None))?
        .with_lane_roots(provenant_confinement::utility::lane_state_roots(state));
    let executor =
        SystemExecutor::new(1).map_err(error("initialise source system executor", None))?;
    let selected = utility
        .project_location(&executor, cwd, budget()?, &CancelToken::new())
        .map_err(utility_error("locate source project", Some(cwd)))?;
    let repo = selected
        .repository
        .ok_or(RunError::Validation("source needs a Git project"))?;
    if repo.private_git_dir() != repo.common_git_dir() {
        provenant_confinement::verify_worktree(&repo, repo.root_path())
            .map_err(worktrees::WorktreeError::from)?;
    }
    let location = project_location(&repo)?;
    let layout = if provenant_config::lookup_project_at_state(state, &location)
        .map_err(error(
            "read source project binding",
            Some(repo.common_git_dir()),
        ))?
        .is_some()
    {
        utility
            .project_layout_for_repository(&executor, &repo, budget()?, &CancelToken::new())
            .map_err(utility_error(
                "resolve source project layout",
                Some(repo.common_git_dir()),
            ))?
    } else {
        let read_repo = utility
            .open_read_repo(&executor, repo.root_path(), budget()?, &CancelToken::new())
            .map_err(utility_error(
                "open read-only source repository",
                Some(repo.root_path()),
            ))?;
        worktrees::project_layout_for_checkout(
            &executor,
            &read_repo,
            &utility,
            budget()?,
            &CancelToken::new(),
        )?
    };
    let project = layout.project().unwrap_or_else(|| repo.root_path());
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
        .map_err(utility_error("locate writer project", Some(cwd)))?;
    let repo = selected
        .repository
        .ok_or(RunError::Validation("writer needs a Git project"))?;
    let (binding, layout) =
        registered_project_layout(state, &repo, &executor, &utility, &CancelToken::new())?;
    let managed_root = layout.root();
    let selected = managed_root.join(path);
    let path = canonicalize(&selected).map_err(|error| {
        if error.is_not_found() {
            RunError::WorktreeMissing(selected)
        } else {
            error.into()
        }
    })?;
    let custody_project = binding.project_id();
    let extra = provenant_records::ExtraFields::default();
    if !verify {
        if !path.starts_with(managed_root) {
            return Err(RunError::unsafe_path(path, PathCheck::Containment));
        }
        return Ok(WriterRequest {
            extra,
            custody_project: Some(custody_project),
            project: repo.root_path().to_path_buf(),
            worktree: path,
        });
    }
    let worktree = provenant_confinement::verify_worktree(&repo, &path)
        .map_err(worktrees::WorktreeError::from)?;
    Ok(WriterRequest {
        extra,
        custody_project: Some(custody_project),
        project: repo.root_path().to_path_buf(),
        worktree: worktree.path().to_path_buf(),
    })
}

#[derive(Default)]
pub(crate) struct TerminalPolicy<'a> {
    pub preservation_stop: Option<crate::settlement::PreservationStop>,
    pub budget_stop: bool,
    pub work: Option<StopWorkPolicy<'a>>,
}
pub(crate) struct StopWorkPolicy<'a> {
    pub trigger: provenant_records::run::StopWorkTrigger,
    pub publish: Option<&'a crate::HostDeliveryContext>,
    pub writer: &'a provenant_records::run::WriterRequest,
    pub publish_granted: bool,
}

pub struct WriterLaunch {
    pub worktree: VerifiedWorktree,
    pub branch: GitRefName,
    pub(crate) layout: worktrees::ProjectLayout,
    pub(crate) executor_root: PathBuf,
    pub(crate) executor_root_identity: provenant_os::fs::Metadata,
    pub(crate) base: GitOid,
    pub(crate) custody: Option<(Directory, WriterBinding)>,
    /// Repository observations and caps shared by this writer operation.
    pub(crate) facts: worktrees::RepoFacts,
}
impl WriterLaunch {
    /// The verified checkout that owns project configuration, if one exists.
    pub fn project_root(&self) -> Option<&Path> {
        self.layout.project()
    }

    /// Root used for managed worktree paths. Bare repositories use their
    /// common Git directory and deliberately have no project config owner.
    pub fn managed_root(&self) -> &Path {
        self.layout.root()
    }

    /// Safe checkout to exclude from automatic tool lookup. Bare-backed
    /// writers use the verified selected linked checkout here.
    pub fn lane_path_roots(&self) -> Vec<&Path> {
        self.project_root()
            .into_iter()
            .chain(std::iter::once(self.executor_root.as_path()))
            .collect()
    }

    /// The verified branch head captured while preparing this writer.
    pub fn source_commit(&self) -> GitOid {
        self.base
    }

    /// Bind a graph writer to its immutable source. The launch preparation
    /// rechecks this prepared base while holding writer custody before launch.
    pub fn require_source(
        &self,
        expected: GitOid,
        _policy: &provenant_records::policy::Policy,
    ) -> Result<()> {
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
        policy: &provenant_records::policy::Policy,
    ) -> Result<()> {
        if self.base != predecessor {
            return Err(policy.boundary_error(
                "WRITER_WRITERLAUNCH_REQUIRE_CONTINUATION_SOURCE_GIT_ISOLATION",
                provenant_records::policy::Facts::violation(
                    "prepared writer base does not match the pinned predecessor head",
                ),
                RunError::WriterSourceMismatch {
                    expected: predecessor,
                    actual: self.base,
                },
            ));
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
            return Err(policy.boundary_error(
                "WRITER_WRITERLAUNCH_REQUIRE_CONTINUATION_SOURCE_GIT_ISOLATION",
                provenant_records::policy::Facts::violation(
                    "pinned predecessor head does not descend from the writer source",
                ),
                RunError::WriterSourceMismatch {
                    expected: source,
                    actual: self.base,
                },
            ));
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
        status_paths(&output)
    }

    fn ahead_of_remote(&self, git: &Path, head: GitOid) -> Result<Option<u64>> {
        let branch = self
            .branch
            .as_str()
            .strip_prefix("refs/heads/")
            .ok_or(RunError::Validation("writer needs a local branch"))?;
        let upstream = format!("refs/remotes/origin/{branch}");
        let revision = match crate::objects::resolve_ref(git, self.worktree.repo(), &upstream) {
            Ok(base) => vec![format!("{base}..{head}").into()],
            // A new lane branch has no remote counterpart. Count commits not
            // reachable from any recorded origin ref rather than losing the
            // evidence for the most common stopped-writer case.
            Err(_) => vec![
                head.to_string().into(),
                "--not".into(),
                "--remotes=origin".into(),
            ],
        };
        let mut args = vec!["rev-list".into(), "--count".into()];
        args.extend(revision);
        let count = crate::objects::command(
            git,
            self.worktree.repo().common_git_dir(),
            self.worktree.repo().root_path(),
            &args,
            None,
        )?;
        let count = std::str::from_utf8(&count)
            .map_err(|_| RunError::Validation("remote ahead count"))?
            .trim()
            .parse::<u64>()
            .map_err(|_| RunError::Validation("remote ahead count"))?;
        Ok(Some(count))
    }

    /// A budget clean point needs both no edits and a locally recorded push of
    /// the effective lane head. An unreadable Git fact is not proof of either.
    pub(crate) fn budget_clean_point(&self, attempt: &Directory) -> Result<bool> {
        let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
        let (head, native) = self.effective_head(attempt, self.base)?;
        let repo = self.worktree.repo();
        let source = crate::objects::bound_source(attempt)?
            .ok_or(RunError::Validation("clean point needs private Git state"))?;
        let index_dir = if native {
            repo.private_dir()
        } else {
            &source.refs
        };
        for directory in [repo.private_dir(), &source.refs] {
            for name in [
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "rebase-merge",
                "rebase-apply",
            ] {
                match directory.metadata_at(OsStr::new(name), false) {
                    Ok(_) => return Ok(false),
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        index_dir.read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
        let index = index_dir.current_path()?.join("index");
        let mut indexes = vec![index];
        if head == self.base {
            // Before the first native commit the heads cannot identify which
            // index owns staged edits. Neither index may carry pending work.
            repo.private_dir()
                .read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
            let native_index = repo.private_dir().current_path()?.join("index");
            if !indexes.contains(&native_index) {
                indexes.push(native_index);
            }
        }
        let objects = source.objects.current_path()?;
        let alternate = repo.common_git_dir().join("objects");
        let clean = crate::objects::with_clean_view(&git, repo, head, None, |_, path| {
            let run = |args: &[&str], index: &Path| {
                crate::objects::execute_bounded(
                    &git,
                    path,
                    repo.root_path(),
                    &args.iter().map(OsString::from).collect::<Vec<_>>(),
                    None,
                    crate::objects::Execution {
                        object_directory: Some(&objects),
                        index_file: Some(index),
                        alternate_directory: Some(&alternate),
                        ..Default::default()
                    },
                )
            };
            run(&["update-ref", "HEAD", &head.to_string()], &indexes[0])?;
            for index in &indexes {
                if !run(
                    &[
                        "--no-optional-locks",
                        "--work-tree",
                        repo.root_path()
                            .to_str()
                            .ok_or(RunError::Validation("worktree path"))?,
                        "status",
                        "--porcelain",
                        "-z",
                        "-uall",
                    ],
                    index,
                )?
                .is_empty()
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        if !clean {
            return Ok(false);
        }
        let upstream_dir = if native {
            // Branch tracking config can be worktree-local when Git enables
            // extensions.worktreeConfig; resolve the upstream from this
            // worktree's private Git directory while refs stay shared.
            repo.private_git_dir().to_path_buf()
        } else {
            source.refs.current_path()?
        };
        let branch = self
            .branch
            .as_str()
            .strip_prefix("refs/heads/")
            .ok_or(RunError::Validation("writer needs a local branch"))?;
        let upstream = crate::objects::command(
            &git,
            &upstream_dir,
            repo.root_path(),
            &[
                "rev-parse".into(),
                "--verify".into(),
                format!("{branch}@{{upstream}}").into(),
            ],
            None,
        )?;
        Ok(std::str::from_utf8(&upstream).is_ok_and(|tip| tip.trim() == head.to_string()))
    }

    /// Select the lane head while settlement holds writer exclusion. Native Git
    /// advances the shared branch without updating the private candidate ref.
    fn effective_head(&self, attempt: &Directory, base: GitOid) -> Result<(GitOid, bool)> {
        let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
        if let Some(source) = crate::objects::bound_source(attempt)? {
            let private = crate::objects::private_tip(&source, self.branch.as_str())?;
            if private != base {
                return Ok((private, false));
            }
        }
        let head = crate::objects::resolve_ref(&git, self.worktree.repo(), self.branch.as_str())?;
        Ok((head, head != base))
    }

    /// When a writer lane passes its token budget and stops, commit its
    /// uncommitted work in the lane's worktree so it ends up committed on its branch.
    fn commit_budget_stop(&self, attempt: &Directory, base: GitOid) -> Result<BudgetCommit> {
        let repo = self.worktree.repo();
        let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
        let source = crate::objects::bound_source(attempt)?.ok_or(RunError::Validation(
            "budget commit needs private Git state",
        ))?;
        let (tip, native) = self.effective_head(attempt, base)?;
        let index_dir = if native {
            repo.private_dir()
        } else {
            &source.refs
        };
        // Never stage away conflict stages or finish a merge, cherry-pick,
        // revert or rebase on the lane's behalf. A merge is restored by the
        // existing stopped-merge import; the others stay as the lane left them.
        for (name, operation) in [
            ("MERGE_HEAD", "merge"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase"),
        ] {
            match index_dir.metadata_at(OsStr::new(name), false) {
                Ok(_) => return Ok(BudgetCommit::note(in_progress_note(operation))),
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        let objects = crate::objects::prepare_budget_objects(&source.objects, repo)?;
        if native {
            // Snapshot through a private index; do not mutate the native index
            // before the new objects have passed import verification.
            crate::objects::copy_index(&git, index_dir, &source.refs, repo.root_path())?;
        }
        // Reuse stable, no-follow reading before passing the index path to Git.
        source
            .refs
            .read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
        let alternate = repo.common_git_dir().join("objects");
        let index = source.refs.current_path()?.join("index");
        crate::objects::with_clean_view(&git, repo, tip, None, |_, path| {
            let run_git = |args: &[&str]| {
                let argv = [OsString::from("--work-tree"), repo.root_path().into()]
                    .into_iter()
                    .chain(args.iter().map(OsString::from))
                    .collect::<Vec<_>>();
                crate::objects::execute_bounded(
                    &git,
                    path,
                    repo.root_path(),
                    &argv,
                    None,
                    crate::objects::Execution {
                        object_directory: Some(&objects),
                        index_file: Some(&index),
                        alternate_directory: Some(&alternate),
                        ..Default::default()
                    },
                )
            };
            run_git(&["update-ref", "HEAD", &tip.to_string()])?;
            if !run_git(&["ls-files", "--unmerged", "-z"])?.is_empty() {
                return Ok(BudgetCommit::note(in_progress_note("merge")));
            }
            let status = run_git(&["status", "--porcelain", "-z", "-uall"])?;
            if status.is_empty() {
                return Ok(BudgetCommit::note("budget stop: worktree already clean"));
            }
            let paths = status_paths(&status)?;
            let message = budget_commit_message(&paths);
            run_git(&["add", "-A"])?;
            run_git(&[
                "-c",
                "user.name=provenant",
                "-c",
                "user.email=provenant@invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                &message,
            ])?;
            let bytes = run_git(&["rev-parse", "HEAD"])?;
            let head = GitOid::parse(
                std::str::from_utf8(&bytes)
                    .map_err(|_| RunError::Validation("budget commit head"))?
                    .trim(),
            )
            .map_err(|_| RunError::Validation("budget commit head"))?;
            crate::objects::set_private_tip(&source, self.branch.as_str(), head)?;
            Ok(BudgetCommit {
                note: "work committed on its branch",
                committed: Some(BudgetWork {
                    sha: head.to_string().chars().take(SHORT_SHA).collect(),
                    files: paths.len(),
                    paths: paths.into_iter().take(BUDGET_PATHS).collect(),
                }),
            })
        })
    }
}

/// The paths in `git status --porcelain -z` output.
fn status_paths(output: &[u8]) -> Result<Vec<String>> {
    Ok(status_paths_raw(output, false)?
        .into_iter()
        .map(|path| String::from_utf8_lossy(&path).into_owned())
        .collect())
}
fn status_paths_raw(output: &[u8], include_sources: bool) -> Result<Vec<Vec<u8>>> {
    let mut records = output.split(|byte| *byte == 0);
    let mut paths = Vec::new();
    while let Some(record) = records.next() {
        if record.is_empty() {
            break;
        }
        if record.len() < 4 || record[2] != b' ' {
            return Err(RunError::Validation("invalid Git status record"));
        }
        paths.push(record[3..].to_vec());
        if record[..2]
            .iter()
            .any(|status| matches!(status, b'R' | b'C'))
        {
            let source = records
                .next()
                .filter(|source| !source.is_empty())
                .ok_or(RunError::Validation("invalid Git status rename"))?;
            if include_sources {
                paths.push(source.to_vec());
            }
        }
    }
    Ok(paths)
}

/// Hex digits in a short commit name.
pub(crate) const SHORT_SHA: usize = 9;
/// Changed paths named in a budget commit message and placeholder result.
pub(crate) const BUDGET_PATHS: usize = 20;

/// What a budget stop did to the lane's branch.
struct BudgetCommit {
    note: &'static str,
    committed: Option<BudgetWork>,
}

impl BudgetCommit {
    fn note(note: &'static str) -> Self {
        Self {
            note,
            committed: None,
        }
    }
}

/// Work a budget stop committed.
struct BudgetWork {
    sha: String,
    files: usize,
    paths: Vec<String>,
}

/// Path names only, with control characters made visible so a name cannot
/// reshape the message around it.
fn listed_path(path: &str) -> String {
    path.chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .collect()
}

/// The paths a budget stop names, capped, with a count of the rest.
pub(crate) fn path_list(paths: &[String], total: usize) -> Vec<String> {
    let mut lines: Vec<String> = paths.iter().map(|path| listed_path(path)).collect();
    if total > paths.len() {
        lines.push(format!("and {} more", total - paths.len()));
    }
    lines
}

fn budget_commit_message(paths: &[String]) -> String {
    let shown = &paths[..paths.len().min(BUDGET_PATHS)];
    let mut message = format!("wip: stopped on budget ({} files)\n", paths.len());
    for line in path_list(shown, paths.len()) {
        message.push('\n');
        message.push_str(&line);
    }
    message
}

/// The settlement note for a budget stop that left an in-progress operation
/// uncommitted.
fn in_progress_note(operation: &str) -> &'static str {
    match operation {
        "merge" => "budget commit skipped: in-progress merge preserved for stopped-merge handling",
        "cherry-pick" => "budget commit skipped: in-progress cherry-pick left uncommitted",
        "revert" => "budget commit skipped: in-progress revert left uncommitted",
        _ => "budget commit skipped: in-progress rebase left uncommitted",
    }
}

use provenant_supervisor::custody::ImportFence;

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct MergeRestored {
    #[serde(default, flatten)]
    pub extra: provenant_records::ExtraFields,
    pub schema_version: u64,
    pub scope: provenant_records::custody::Scope,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GitPins {
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
    schema_version: u64,
    scope: provenant_records::custody::Scope,
    base: GitOid,
}

/// Remove only this attempt's host-owned retention ref after terminal commit.
/// The head-keyed import ref belongs to publish_branch's custody-protected CAS;
/// a delayed cleanup must never delete another lane's in-progress import.
pub(crate) fn cleanup_git_pins(
    state: &Path,
    writer: &WriterRequest,
    attempt: &Directory,
    scope: provenant_records::custody::Scope,
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
        .map_err(utility_error(
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

/// Read the required source-project custody binding from a current writer request.
pub(crate) fn writer_project_id(request: &WriterRequest) -> Result<ProjectId> {
    request
        .custody_project
        .ok_or(RunError::Validation("writer custody project is not sealed"))
}

/// Compare the complete persisted writer identity.
pub fn equivalent_writer(saved: &WriterRequest, resolved: &WriterRequest) -> bool {
    saved == resolved
}

impl crate::RunStore {
    /// The persisted worktree slot holding one attempt's writer custody, or
    /// `None` for a reader. Custody has no other location, so callers need
    /// not inventory the host's slots.
    pub(crate) fn attempt_writer_slot(
        &self,
        attempt: provenant_core::AttemptId,
        call: &crate::Request<'_>,
    ) -> Result<Option<PathBuf>> {
        let request = self.request(attempt, call)?;
        request
            .task
            .writer
            .as_ref()
            .map(|writer| self.writer_slot(writer))
            .transpose()
    }
    pub(crate) fn writer_slot(&self, writer: &WriterRequest) -> Result<PathBuf> {
        Ok(self
            .writer_registry
            .join("worktrees")
            .join(writer_project_id(writer)?.to_string())
            .join(worktrees::state_dir_name(&writer.worktree)))
    }
}

/// Reopen the persisted scope and verify current Git worktree membership.
/// The subsequent supervisor writer flock supplies exclusion.
pub fn prepare_writer(
    request: &WriterRequest,
    state: &Path,
    project: ProjectId,
) -> Result<WriterLaunch> {
    let facts = worktrees::RepoFacts::default();
    prepare_writer_with_facts(request, state, project, &facts)
}

/// Prepare a writer using the command's repository observations and caps.
pub fn prepare_writer_with_facts(
    request: &WriterRequest,
    state: &Path,
    project: ProjectId,
    facts: &worktrees::RepoFacts,
) -> Result<WriterLaunch> {
    use provenant_os::clock::SystemClock;
    let clock = SystemClock::new();
    prepare_writer_with_facts_and_clock(request, state, project, facts, &clock)
}

fn prepare_writer_with_facts_and_clock(
    request: &WriterRequest,
    state: &Path,
    project: ProjectId,
    facts: &worktrees::RepoFacts,
    clock: &dyn Clock,
) -> Result<WriterLaunch> {
    use provenant_core::WaitBudget;
    use provenant_os::fs::LockMode;
    let utility = runtime(state)?;
    let executor =
        SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
    // Discover only the structural anchor before taking the lock. Even the
    // first probed open reads config snapshots and must exclude sibling writes.
    let common = provenant_confinement::repo::VerifiedRepo::open_common_directory(&request.project)
        .map_err(worktrees::WorktreeError::from)?;
    let cancel = CancelToken::new();
    let _config_lock = worktrees::lock_repository(
        &common,
        LockMode::Shared,
        WaitBudget::cli(clock.mono_now(), 60)
            .map_err(error("set writer config lock budget", None))?,
        &cancel,
        clock,
    )?;
    let repo = utility
        .open_repo(&executor, &request.project, budget()?, &cancel)
        .map_err(utility_error(
            "open writer repository",
            Some(&request.project),
        ))?;
    if !common.metadata()?.same_identity(&repo.common_identity()) {
        return Err(RunError::Validation(
            "writer repository changed during setup",
        ));
    }
    let worktree = provenant_confinement::verify_worktree(&repo, &request.worktree)
        .map_err(worktrees::WorktreeError::from)?;
    let branch = writer_branch(&worktree)?;
    let base = facts.resolve_repo_commit(
        worktree.repo(),
        &executor,
        &utility,
        budget()?,
        branch.clone(),
        &CancelToken::new(),
    )?;
    let custody_project = writer_project_id(request)?;
    let (binding, layout) = registered_project_layout(state, &repo, &executor, &utility, &cancel)?;
    if binding.project_id() != custody_project {
        return Err(RunError::Validation(
            "writer custody project does not match its repository",
        ));
    }
    match (
        repo.private_git_dir() == repo.common_git_dir(),
        layout.project(),
    ) {
        (true, Some(primary)) if primary == repo.root_path() => {}
        // The selected linked checkout remains the Git executor. Its verified
        // inventory membership and the binding's common directory establish
        // its relation to the separately owned project layout.
        (false, _) => {}
        _ => return Err(worktrees::WorktreeError::ProjectLayoutUnresolved.into()),
    }
    let executor_root = repo.root_path().to_path_buf();
    let executor_root_identity = repo.root_identity();
    let state_dir = Directory::open_anchor(state)?;
    if custody_project != project {
        let references =
            crate::artifacts::subdir(&state_dir, OsStr::new(WRITER_PROJECTS_DIRECTORY), true)?;
        let parent = crate::artifacts::subdir(&references, OsStr::new(&project.to_string()), true)?;
        crate::artifacts::subdir(&parent, OsStr::new(&custody_project.to_string()), true)?;
    }
    let project = custody_project;
    let projects = crate::artifacts::subdir(&state_dir, OsStr::new("worktrees"), true)
        .map_err(|error| anchor_filesystem(error, state))?;
    let project_state = crate::artifacts::subdir(&projects, OsStr::new(&project.to_string()), true)
        .map_err(|error| anchor_filesystem(error, state))?;
    let (directory, binding) = worktrees::writer_custody(
        &project_state,
        &worktree,
        &request.worktree,
        &branch,
        base,
        || {
            facts.writer_inventory(
                &repo,
                &executor,
                &utility,
                budget().map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                &CancelToken::new(),
            )
        },
    )
    .map_err(|error| match error {
        worktrees::WorktreeError::NotFound => {
            RunError::WorktreeUnregistered(request.worktree.clone())
        }
        other => other.into(),
    })?;
    Ok(WriterLaunch {
        worktree,
        branch,
        layout,
        executor_root,
        executor_root_identity,
        base,
        custody: Some((directory, binding)),
        facts: facts.clone(),
    })
}

/// An uncompiled provider plan. Run compiles only after acquiring custody.
pub struct LaunchConfinement {
    pub plan: ConfinedPlan,
    pub profile: ProviderProfile,
    pub writer: Option<WriterLaunch>,
    /// Canonical host home; the writer's lane cargo cache derives from it.
    pub home: PathBuf,
    /// Opt in to private host target seeding.
    pub cargo_target_seed: bool,
    /// Resolved once by the host, reused after all late environment additions.
    pub cargo_home: provenant_confinement::cargo::CargoHome,
    /// Host state root containing target seed generations.
    pub state_root: PathBuf,
    /// Whether the lane runs under the compiled sandbox profile. Without it
    /// the plan still prepares writer directories and records access, but the
    /// provider starts directly, like the same CLI started by hand.
    pub sandbox: provenant_confinement::run_config::SandboxMode,
    /// Bound once before sealing launch evidence; compilation uses this exact
    /// probed backend under writer custody, without a second selection.
    pub backend: Option<BoundSandboxBackend>,
}
/// A probed backend and the limits whose sources were sealed with it.
pub struct BoundSandboxBackend {
    backend: Box<dyn ConfinementBackend>,
    limits: ResolvedLaunchLimits,
}

struct ResolvedLaunchLimits {
    limits: ProfileLimits,
    grant_origin: provenant_records::diagnostic::SettingSource,
    profile_origin: provenant_records::diagnostic::SettingSource,
}

impl ResolvedLaunchLimits {
    fn annotate(
        &self,
        error: provenant_confinement::permissions::PermissionError,
    ) -> provenant_confinement::permissions::PermissionError {
        use provenant_confinement::permissions::PermissionError;
        let origin = match &error {
            PermissionError::GrantLimitExceeded { .. } => self.grant_origin,
            PermissionError::ProfileTooLarge { .. } => self.profile_origin,
            _ => return error,
        };
        PermissionError::ConfigOrigin {
            source: Box::new(error),
            origin,
        }
    }
}

fn launch_resource_limits(
    state: &Path,
    policy: &provenant_records::policy::Policy,
) -> ResolvedLaunchLimits {
    match provenant_confinement::run_config::RunConfig::read_with_keys(state) {
        Ok((config, keys)) => {
            let origin = |key: &str| {
                if keys.contains(key) {
                    provenant_records::diagnostic::SettingSource::Configuration
                } else {
                    provenant_records::diagnostic::SettingSource::ShippedDefault
                }
            };
            ResolvedLaunchLimits {
                limits: ProfileLimits {
                    grant_limit: config.grant_limit,
                    profile_limit_bytes: config.profile_limit_bytes,
                    grant_origin: origin("grant_limit"),
                    profile_origin: origin("profile_limit_bytes"),
                },
                grant_origin: origin("grant_limit"),
                profile_origin: origin("profile_limit_bytes"),
            }
        }
        Err(error) => {
            let defaults = ProfileLimits::default();
            if let provenant_records::policy::Effect::Warn(message) = policy.check(
                "WRITER_LAUNCH_RESOURCE_LIMITS_NONE",
                provenant_records::policy::Facts::violation(format!(
                    "launch resource limits unavailable ({error}); using compiled defaults domains.run.grant_limit={} and domains.run.profile_limit_bytes={}",
                    defaults.grant_limit, defaults.profile_limit_bytes
                )),
            ) {
                let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
            }
            ResolvedLaunchLimits {
                limits: defaults,
                grant_origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
                profile_origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
            }
        }
    }
}

impl LaunchConfinement {
    /// Backend absence is advisory. Plan validation and profile compilation
    /// still fail at their typed credential, Git and integrity boundaries.
    pub(crate) fn bind_backend(
        &mut self,
        cancel: &CancelToken,
        policy: &provenant_records::policy::Policy,
    ) -> Result<Availability> {
        let executor =
            SystemExecutor::new(1).map_err(error("initialise sandbox probe executor", None))?;
        let limits = launch_resource_limits(&self.state_root, policy);
        let selected = system_backend_with_limits_and_policy(
            &BackendContext {
                executor: &executor,
                cancel,
                #[cfg(target_os = "macos")]
                env: &[],
            },
            limits.limits,
            policy,
        );
        let reason = match selected {
            Ok(backend) => {
                let kind = backend.kind();
                self.backend = Some(BoundSandboxBackend { backend, limits });
                return Ok(Availability::Available { backend: kind });
            }
            Err(PermissionError::BackendNotAvailable(reason)) => reason,
            Err(
                PermissionError::BackendUnavailable
                | PermissionError::Execution(_)
                | PermissionError::Filesystem(_),
            ) => AvailabilityReason::ProbeFailed,
            Err(error) => return Err(RunError::Confinement(error)),
        };
        self.backend = None;
        Ok(Availability::Unavailable { reason })
    }

    /// Prepare Git while custody is held, then replace every handle that was
    /// bound to the common config before Git changed it.
    pub(crate) fn prepare_git(
        &mut self,
        cancel: &CancelToken,
        policy: &provenant_records::policy::Policy,
    ) -> Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
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
            return Ok(());
        };
        prepare_writer_git(
            writer,
            &self.state_root,
            &self.home,
            Some((authority, &self.plan.permissions().credential_roots)),
            cancel,
            policy,
        )
    }
    /// Prepare a private target from host-built output while writer custody is held.
    pub(crate) fn seed_cargo(
        &self,
        environment: &mut Vec<(OsString, OsString)>,
        attempt_dir: &Directory,
        cancel: &CancelToken,
    ) {
        if cancel.is_cancelled() {
            return;
        }
        let cache = match &self.plan.permissions().mode {
            AccessMode::WorktreeWrite(authority) => &authority.cargo_cache().path,
            AccessMode::ReadOnly => &self.plan.permissions().scratch.path,
        };
        self.cargo_home
            .lane_cargo_environment(&self.home, cache, environment);
        self.cargo_home
            .source_environment(self.plan.permissions().network, environment);
        let writer = self.writer.as_ref().map(|w| (&w.worktree, &w.branch));
        let target_outcome = match (&self.plan.permissions().mode, writer) {
            (AccessMode::WorktreeWrite(authority), Some((worktree, _))) => {
                self.seed_cargo_target(worktree, &authority.cargo_cache().path, environment, cancel)
            }
            _ => crate::cargo_seed::Outcome::skipped("read_only_lane"),
        };
        if let Some(warning) = target_outcome.fallback_warning("the Cargo target") {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "{warning}");
        }
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

    fn seed_cargo_target(
        &self,
        worktree: &VerifiedWorktree,
        cache: &Path,
        environment: &[(OsString, OsString)],
        cancel: &CancelToken,
    ) -> crate::cargo_seed::Outcome {
        use crate::cargo_seed::Outcome;
        if !self.cargo_target_seed {
            return Outcome::skipped("disabled");
        }
        let build =
            match crate::target_seed::cargo_build_directory(worktree.path(), &self.state_root) {
                Ok(Some(build)) => build,
                Ok(None) => return Outcome::skipped("non_cargo_project"),
                Err(_) => {
                    return Outcome::fallback_detail(
                        "toolchain_unverified",
                        codes::CARGO_WORKSPACE_UNREADABLE,
                    );
                }
            };
        if cfg!(unix)
            && let Err(detail) =
                verified_cargo_config(&build, &self.cargo_home.path).and_then(|()| {
                    trusted_lane_toolchain_diagnostic(
                        &self.home,
                        worktree.path(),
                        environment,
                        &provenant_confinement::write_grants(
                            self.plan.permissions(),
                            &self.profile,
                        ),
                    )
                })
        {
            return Outcome::fallback_detail("toolchain_unverified", detail);
        }
        crate::cargo_seed::seed_target(
            &self.state_root,
            cache,
            worktree.path(),
            &crate::target_seed::project_key(worktree.repo().common_git_dir()),
            &self.home,
            environment,
            cancel,
        )
    }
    /// Run holds writer custody here: create the derived ref parents and the
    /// private lane cache, before they are anchored or used unsandboxed.
    pub(crate) fn prepare_writer_directories(&self) -> Result<()> {
        if let (Some(writer), AccessMode::WorktreeWrite(authority)) =
            (&self.writer, &self.plan.permissions().mode)
        {
            prepare_writer_directories(
                &writer.worktree,
                &writer.branch,
                authority,
                &self.home,
                &self.plan.permissions().credential_roots,
            )
            .map_err(RunError::Confinement)?;
        }
        Ok(())
    }

    pub(crate) fn command(
        &mut self,
        argv: &[OsString],
        attempt: &provenant_core::AttemptId,
    ) -> Result<provenant_confinement::permissions::ConfinedCommand> {
        let backend = self.backend.as_ref().ok_or(RunError::Validation(
            "sandbox backend was not bound before launch",
        ))?;
        let writer = self.writer.as_ref().map(|w| (&w.worktree, &w.branch));
        self.prepare_writer_directories()?;
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
            .compile(backend.backend.as_ref(), &self.profile, &anchors)
            .map_err(|error| RunError::Confinement(backend.limits.annotate(error)))?
            .command(
                argv,
                attempt,
                &Directory::open_anchor(&self.state_root)
                    .map_err(error("open profile staging directory", None))?,
            )
            .map_err(RunError::Confinement)
    }
}

fn prepare_writer_git(
    writer: &mut WriterLaunch,
    state: &Path,
    home: &Path,
    authority: Option<(
        &provenant_confinement::permissions::WorktreeWriteAuthority,
        &provenant_core::CredentialRoots,
    )>,
    cancel: &CancelToken,
    _policy: &provenant_records::policy::Policy,
) -> Result<()> {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::{clock::SystemClock, fs::LockMode};
    let utility = runtime(state)?;
    let root_identity = writer.worktree.identity();
    let private_identity = writer.worktree.repo().private_identity();
    let common_identity = writer.worktree.repo().common_identity();
    let executor =
        SystemExecutor::new(1).map_err(error("initialise writer system executor", None))?;
    let old_repo = writer.worktree.repo();
    let project = writer.executor_root.clone();
    let path = writer.worktree.path().to_path_buf();
    let common = old_repo.common_dir().try_clone()?;
    let lock_config = |mode| -> Result<provenant_os::fs::LockGuard> {
        let clock = SystemClock::new();
        Ok(worktrees::lock_repository(
            &common,
            mode,
            WaitBudget::cli(clock.mono_now(), 60)
                .map_err(error("set writer config lock budget", None))?,
            cancel,
            &clock,
        )?)
    };
    // Sibling config replacements can exhaust even a bounded stable read.
    // Share the initial snapshot phase, then release before the fetch so
    // overlapping origin requests can still coalesce without this lock.
    let config_lock = lock_config(LockMode::Shared)?;
    let repo = utility
        .open_repo(&executor, &project, budget()?, cancel)
        .map_err(utility_error("reopen writer repository", Some(&project)))?;
    if repo.root_path() != project
        || !writer
            .executor_root_identity
            .same_identity(&repo.root_identity())
        || !repo.common_identity().same_identity(&common_identity)
    {
        return Err(worktrees::WorktreeError::ProjectLayoutUnresolved.into());
    }
    let (_, layout) = registered_project_layout(state, &repo, &executor, &utility, cancel)?;
    if !same_project_layout(&writer.layout, &layout)? {
        return Err(worktrees::WorktreeError::ProjectLayoutUnresolved.into());
    }
    writer.layout = layout;
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
    if let Some((authority, credentials)) = authority {
        prepare_writer_directories(
            &writer.worktree,
            &writer.branch,
            authority,
            home,
            credentials,
        )
        .map_err(RunError::Confinement)?;
    }
    let repo = writer.worktree.repo();
    repo.revalidate()
        .map_err(error("verify writer repository before fetch", Some(&path)))?;
    drop(config_lock);
    crate::HostDeliveryContext::refresh_origin(
        &WriterRequest {
            extra: Default::default(),
            custody_project: None,
            project: project.clone(),
            worktree: path.clone(),
        },
        repo.private_git_dir(),
        state,
        cancel,
        &writer.facts,
    )?;
    // Keep post-fetch snapshots and both upstream writes in one exclusive
    // phase. Reopening outside this phase races siblings' atomic config writes.
    let config_lock = lock_config(LockMode::Exclusive)?;
    repo.revalidate_identity()
        .map_err(error("verify writer repository after fetch", Some(&path)))?;
    // Configuration may legitimately change while fetch owns no mutation lock.
    // Refresh every dependent snapshot only after regaining the common owner.
    let fresh_repo = utility
        .open_repo(&executor, &project, budget()?, cancel)
        .map_err(utility_error(
            "reopen writer repository after fetch",
            Some(&project),
        ))?;
    if fresh_repo.root_path() != project
        || !writer
            .executor_root_identity
            .same_identity(&fresh_repo.root_identity())
        || !fresh_repo.common_identity().same_identity(&common_identity)
    {
        return Err(worktrees::WorktreeError::ProjectLayoutUnresolved.into());
    }
    let (_, layout) = registered_project_layout(state, &fresh_repo, &executor, &utility, cancel)?;
    if !same_project_layout(&writer.layout, &layout)? {
        return Err(worktrees::WorktreeError::ProjectLayoutUnresolved.into());
    }
    writer.layout = layout;
    let refreshed = provenant_confinement::verify_worktree(&fresh_repo, &path)
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
    let repo = writer.worktree.repo();
    let ref_path = Path::new(writer.branch.as_str());
    let parent = repo.common_git_dir().join(
        ref_path
            .parent()
            .ok_or(RunError::Validation("writer ref has no parent"))?,
    );
    // Packed refs need not have a loose-ref parent. Walk from the held common
    // directory without following links and create missing components while
    // each branch is protected by its own Git ref lock.
    let mut directory = repo.common_dir().try_clone()?;
    for component in ref_path
        .parent()
        .ok_or(RunError::Validation("writer ref has no parent"))?
        .components()
    {
        let std::path::Component::Normal(name) = component else {
            return Err(RunError::Validation("invalid writer ref parent"));
        };
        directory = match directory.open_directory(name) {
            Ok(child) => child,
            Err(failure) if failure.is_not_found() => {
                match directory.mkdir(name, 0o777) {
                    Ok(()) => {}
                    Err(failure) if failure.is_exists() => {}
                    Err(failure) => return Err(failure.into()),
                }
                directory.open_directory(name)?
            }
            Err(failure) => return Err(failure.into()),
        };
    }
    let name = ref_path
        .file_name()
        .ok_or(RunError::Validation("writer ref has no name"))?;
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    // A fetch or another Git user may own the ref lock. The branch already
    // exists (possibly packed); a collision does not require rewriting it.
    let mut lock = match directory.create_new(&lock_name) {
        Ok(lock) => Some(lock),
        Err(failure) if failure.is_exists() => None,
        Err(failure) => return Err(failure.into()),
    };
    let result = (|| -> Result<()> {
        let resolved = writer.facts.resolve_repo_commit(
            repo,
            &executor,
            &utility,
            budget()?,
            writer.branch.clone(),
            cancel,
        )?;
        if resolved != writer.base {
            return Err(RunError::Validation("writer branch changed before launch"));
        }
        if let Some(lock) = lock.as_mut() {
            writeln!(lock, "{}", writer.base)
                .map_err(error("write writer ref lock", Some(&parent)))?;
            lock.sync()?;
            directory.publish_git_ref_lock(name, lock)?;
            directory.sync()?;
        }
        Ok(())
    })();
    if result.is_err()
        && let Some(lock) = lock.as_ref()
    {
        let _ = directory.discard_git_ref_lock(name, lock);
    }
    result?;
    for merge in [false, true] {
        let repo = utility
            .open_repo(&executor, &project, budget()?, cancel)
            .map_err(utility_error("reopen writer repository", Some(&project)))?;
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
        .map_err(utility_error("reopen writer repository", Some(&project)))?;
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
    drop(config_lock);
    Ok(())
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
fn verified_cargo_config(build: &Path, cargo_home: &Path) -> std::result::Result<(), &'static str> {
    let present = |directory: &Path| {
        provenant_confinement::cargo::CONFIG_FILES
            .iter()
            .map(|name| directory.join(name))
            .filter(|path| {
                !matches!(std::fs::symlink_metadata(path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound)
            })
            .collect::<Vec<_>>()
    };
    // The host config is data, with the same seed-eligibility check as project config.
    for path in present(cargo_home) {
        cargo_config_ineligible_for_seed(&path)?;
    }
    // A checked-in `.cargo/config.toml` is ordinary (this repository has one).
    // It is read as data and refused only for unrepresented build settings.
    for ancestor in build.ancestors() {
        for path in present(&ancestor.join(".cargo")) {
            cargo_config_ineligible_for_seed(&path)?;
        }
    }
    Ok(())
}

// Read a Cargo config file as data, without executing it. A config is refused
// when it is not a small regular UTF-8 TOML file or can select a different
// compiler/build target or unrecorded build flags.
fn cargo_config_ineligible_for_seed(path: &Path) -> std::result::Result<(), &'static str> {
    use std::io::Read;
    if !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Err(codes::CARGO_CONFIG_UNREADABLE);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(65537).read_to_end(&mut bytes))
        .map_err(|_| codes::CARGO_CONFIG_UNREADABLE)?;
    crate::target_seed::cargo_config_seed_eligibility(&bytes)
}

// Refuse the host probe if a lane can alter the host compiler or an earlier
// PATH entry, or if its rustc does not resolve to the host rustup shim.
pub fn trusted_lane_toolchain_diagnostic(
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

    type FixtureResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    struct WriterFixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        project: PathBuf,
        state: PathBuf,
        git_dir: PathBuf,
        git: PathBuf,
        custody_project: ProjectId,
        source: String,
    }

    impl WriterFixture {
        fn new() -> FixtureResult<Self> {
            let temp = tempfile::tempdir()?;
            let root = std::fs::canonicalize(temp.path())?;
            let project = root.join("project");
            let home = root.join("home");
            let state = root.join("state");
            std::fs::create_dir(&project)?;
            std::fs::create_dir(&home)?;
            provenant_os::fixture::private_state_dir(&state)?;
            let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
            let git_dir = project.join(".git");
            let run = |args: &[&str]| -> FixtureResult<String> {
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
            let mut roots = provenant_core::RootInputs::new(home, project.clone());
            roots.state_dir = Some(state.clone());
            let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
            let custody_project = provenant_config::bind_project(
                &roots,
                provenant_config::ProjectLocation::new(git_dir.clone())?,
            )?
            .project_id();
            let source = run(&["rev-parse", "HEAD"])?;
            Ok(Self {
                _temp: temp,
                root,
                project,
                state,
                git_dir,
                git,
                custody_project,
                source,
            })
        }

        fn run_git(&self, args: &[&str]) -> FixtureResult<String> {
            let output = provenant_os::fixture::git_command_at(&self.git)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_DIR", &self.git_dir)
                .env("GIT_WORK_TREE", &self.project)
                .current_dir(&self.project)
                .args(args)
                .output()?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
            }
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        }

        fn lane_git(&self, lane: &Path, args: &[&str]) -> FixtureResult<()> {
            let output = provenant_os::fixture::git_command_at(&self.git)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(lane)
                .args(args)
                .output()?;
            if output.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).into_owned().into())
            }
        }

        fn add_worktree(&self, branch: &str, name: &str) -> FixtureResult<PathBuf> {
            let lane = self.root.join(name);
            self.run_git(&[
                "worktree",
                "add",
                "-qb",
                branch,
                lane.to_str().ok_or("lane path")?,
            ])?;
            Ok(lane)
        }

        fn request(&self, lane: &Path) -> WriterRequest {
            WriterRequest {
                extra: Default::default(),
                custody_project: Some(self.custody_project),
                project: self.project.clone(),
                worktree: lane.to_path_buf(),
            }
        }

        fn prepare(&self, lane: &Path) -> FixtureResult<WriterLaunch> {
            Ok(prepare_writer_with_facts(
                &self.request(lane),
                &self.state,
                self.custody_project,
                &worktrees::RepoFacts::default(),
            )?)
        }

        fn prepare_git(&self, writer: &mut WriterLaunch) -> FixtureResult<()> {
            prepare_writer_git(
                writer,
                &self.state,
                &self.root,
                None,
                &CancelToken::new(),
                &provenant_records::policy::Policy::default(),
            )?;
            Ok(())
        }
    }

    fn clean_environment_child(test_filter: &str) -> FixtureResult<bool> {
        const CHILD: &str = "PROVENANT_WRITER_CONFIG_FIXTURE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            return Ok(false);
        }
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        std::fs::create_dir(&home)?;
        #[allow(
            clippy::disallowed_methods,
            reason = "the test re-runs itself in a clean home"
        )]
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([test_filter, "--nocapture"])
            .current_dir(temp.path())
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(CHILD, "1")
            .status()?;
        assert!(status.success());
        Ok(true)
    }

    // Spec: docs/specs/run-git.md#project-layout-ownership
    #[test]
    fn source_and_writer_resolution_use_the_registered_primary_with_separate_git_dir()
    -> FixtureResult<()> {
        if clean_environment_child(stringify!(
            source_and_writer_resolution_use_the_registered_primary_with_separate_git_dir
        ))? {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let git_dir = root.join("unrelated/metadata/project.git");
        let caller = root.join("caller");
        let writer = project.join(".worktrees/writer");
        let home = root.join("home");
        let state = root.join("state");
        for path in [
            &project,
            git_dir.parent().ok_or("metadata parent")?,
            writer.parent().ok_or("writer parent")?,
            &home,
        ] {
            std::fs::create_dir_all(path)?;
        }
        provenant_os::fixture::private_state_dir(&state)?;
        let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
        let run_git = |cwd: &Path, args: &[&str]| -> FixtureResult<String> {
            let output = provenant_os::fixture::git_command_at(&git)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(cwd)
                .args(args)
                .output()?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
            }
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        run_git(
            &root,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                git_dir.to_str().ok_or("git dir path")?,
                project.to_str().ok_or("project path")?,
            ],
        )?;
        run_git(&project, &["config", "user.name", "Fixture"])?;
        run_git(
            &project,
            &["config", "user.email", "fixture@example.invalid"],
        )?;
        run_git(&project, &["commit", "--allow-empty", "-qm", "initial"])?;
        run_git(
            &project,
            &[
                "worktree",
                "add",
                "-qb",
                "chore/caller",
                caller.to_str().ok_or("caller path")?,
            ],
        )?;
        run_git(
            &project,
            &[
                "worktree",
                "add",
                "-qb",
                "chore/writer",
                writer.to_str().ok_or("writer path")?,
            ],
        )?;

        let mut roots = provenant_core::RootInputs::new(home, project.clone());
        roots.state_dir = Some(state.clone());
        let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
        let utility = UtilityRuntime::read_only(&git, &state)?;
        let executor = SystemExecutor::new(1)?;
        let discovered =
            utility.project_location(&executor, &project, budget()?, &CancelToken::new())?;
        let binding = provenant_config::bind_project(&roots, discovered.location)?;

        let request = resolve_writer(&state, &caller, Path::new(".worktrees/writer"))?;
        assert_eq!(request.project, caller);
        assert_eq!(request.worktree, writer);
        assert_eq!(
            resolve_reader_source(&state, &caller, Path::new(".worktrees/writer"))?,
            writer
        );
        assert_eq!(binding.common_dir(), git_dir);
        let launch = prepare_writer(&request, &state, binding.project_id())?;
        assert_eq!(launch.project_root(), Some(project.as_path()));
        assert_eq!(launch.managed_root(), project);

        let primary_bin = project.join("tools");
        let linked_bin = caller.join("tools");
        let safe_bin = root.join("safe-tools");
        for path in [&primary_bin, &linked_bin, &safe_bin] {
            std::fs::create_dir_all(path)?;
        }
        let path = std::env::join_paths([&primary_bin, &linked_bin, &safe_bin])?;
        let excluded = launch
            .lane_path_roots()
            .into_iter()
            .chain(std::iter::once(launch.worktree.path()))
            .collect::<Vec<_>>();
        let filtered = crate::FilteredLanePath::from_path_excluding_roots(&path, &excluded)?;
        assert_eq!(
            std::env::split_paths(filtered.path()).collect::<Vec<_>>(),
            vec![safe_bin]
        );
        Ok(())
    }

    // Spec: docs/specs/run-git.md#project-layout-ownership
    #[test]
    fn bare_backed_writer_uses_common_layout_and_verified_linked_executor() -> FixtureResult<()> {
        if clean_environment_child(stringify!(
            bare_backed_writer_uses_common_layout_and_verified_linked_executor
        ))? {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let seed = root.join("seed");
        let common = root.join("metadata/project.git");
        let source = root.join("source");
        let writer_path = common.join(".worktrees/lane");
        let home = root.join("home");
        let state = root.join("state");
        std::fs::create_dir_all(&seed)?;
        std::fs::create_dir_all(common.parent().ok_or("common parent")?)?;
        std::fs::create_dir(&home)?;
        provenant_os::fixture::private_state_dir(&state)?;
        let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
        let run_git = |cwd: &Path, args: &[&str]| -> FixtureResult<String> {
            let output = provenant_os::fixture::git_command_at(&git)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(cwd)
                .args(args)
                .output()?;
            if output.status.success() {
                Ok(String::from_utf8(output.stdout)?.trim().to_owned())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).into_owned().into())
            }
        };
        run_git(
            &root,
            &[
                "init",
                "-q",
                "--initial-branch=main",
                seed.to_str().ok_or("seed path")?,
            ],
        )?;
        run_git(&seed, &["config", "user.name", "Fixture"])?;
        run_git(&seed, &["config", "user.email", "fixture@example.invalid"])?;
        std::fs::write(seed.join("README.md"), "bare layout\n")?;
        run_git(&seed, &["add", "README.md"])?;
        run_git(&seed, &["commit", "-qm", "initial"])?;
        run_git(
            &root,
            &[
                "clone",
                "--bare",
                seed.to_str().ok_or("seed path")?,
                common.to_str().ok_or("common path")?,
            ],
        )?;
        run_git(
            &root,
            &[
                "--git-dir",
                common.to_str().ok_or("common path")?,
                "worktree",
                "add",
                source.to_str().ok_or("source path")?,
                "main",
            ],
        )?;
        std::fs::create_dir_all(writer_path.parent().ok_or("writer parent")?)?;
        run_git(
            &root,
            &[
                "--git-dir",
                common.to_str().ok_or("common path")?,
                "worktree",
                "add",
                "-b",
                "lane",
                writer_path.to_str().ok_or("writer path")?,
            ],
        )?;

        let mut roots = provenant_core::RootInputs::new(home, source.clone());
        roots.state_dir = Some(state.clone());
        let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(common.clone())?,
        )?;
        let request = resolve_writer(&state, &source, Path::new(".worktrees/lane"))?;
        assert_eq!(request.project, source);
        assert_eq!(request.worktree, writer_path);
        assert_eq!(
            resolve_reader_source(&state, &source, Path::new("."))?,
            source
        );
        let launch = prepare_writer(&request, &state, binding.project_id())?;
        assert_eq!(launch.project_root(), None);
        assert_eq!(launch.managed_root(), common);
        assert_eq!(launch.lane_path_roots(), vec![source.as_path()]);
        Ok(())
    }

    #[test]
    fn writer_source_binding_rejects_unrelated_lineage() -> FixtureResult<()> {
        if clean_environment_child(stringify!(writer_source_binding_rejects_unrelated_lineage))? {
            return Ok(());
        }
        let fixture = WriterFixture::new()?;
        let lane = fixture.add_worktree("chore/source", "lane")?;
        let mut writer = fixture.prepare(&lane)?;
        let source = GitOid::parse(&fixture.source)?;
        writer.require_source(source, &provenant_records::policy::Policy::default())?;

        let tree = fixture.run_git(&["rev-parse", "HEAD^{tree}"])?;
        let unrelated =
            GitOid::parse(&fixture.run_git(&["commit-tree", &tree, "-m", "unrelated root"])?)?;
        assert!(matches!(
            writer.require_source(unrelated, &provenant_records::policy::Policy::default()),
            Err(RunError::WriterSourceMismatch { expected, actual })
                if expected == unrelated && actual == source
        ));

        let child = GitOid::parse(&fixture.run_git(&[
            "commit-tree",
            &tree,
            "-p",
            &fixture.source,
            "-m",
            "child commit",
        ])?)?;
        writer.base = child;
        let utility = runtime(&fixture.state)?;
        writer.require_continuation_source(
            utility.git_program_path(),
            source,
            child,
            &provenant_records::policy::Policy::default(),
        )?;
        assert!(matches!(
            writer.require_continuation_source(utility.git_program_path(), unrelated, child, &provenant_records::policy::Policy::default()),
            Err(RunError::WriterSourceMismatch { expected, actual })
                if expected == unrelated && actual == child
        ));
        assert!(matches!(
            writer.require_continuation_source(utility.git_program_path(), source, source, &provenant_records::policy::Policy::default()),
            Err(RunError::WriterSourceMismatch { expected, actual })
                if expected == source && actual == child
        ));
        Ok(())
    }

    #[test]
    fn writer_reports_uncommitted_paths_independent_of_git_output_order() -> FixtureResult<()> {
        if clean_environment_child(stringify!(
            writer_reports_uncommitted_paths_independent_of_git_output_order
        ))? {
            return Ok(());
        }
        let fixture = WriterFixture::new()?;
        let lane = fixture.add_worktree("chore/paths", "lane")?;
        let writer = fixture.prepare(&lane)?;
        let utility = runtime(&fixture.state)?;
        assert!(
            writer
                .uncommitted_paths(utility.git_program_path())?
                .is_empty()
        );

        fixture.lane_git(&lane, &["mv", "old name.txt", "renamed.txt"])?;
        std::fs::write(lane.join("café.txt"), "untracked")?;
        let observed: std::collections::BTreeSet<_> = writer
            .uncommitted_paths(utility.git_program_path())?
            .into_iter()
            .collect();
        let expected = ["renamed.txt".to_owned(), "café.txt".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(observed, expected);
        Ok(())
    }

    #[test]
    fn writer_git_preparation_restores_packed_branch_and_upstream() -> FixtureResult<()> {
        if clean_environment_child(stringify!(
            writer_git_preparation_restores_packed_branch_and_upstream
        ))? {
            return Ok(());
        }
        let fixture = WriterFixture::new()?;
        let branch = "refs/heads/chore/x";
        let lane = fixture.add_worktree("chore/x", "lane")?;
        let mut writer = fixture.prepare(&lane)?;
        let expected = fixture.run_git(&["rev-parse", branch])?;
        fixture.run_git(&["pack-refs", "--all"])?;
        let ref_parent = fixture.git_dir.join("refs/heads/chore");
        if ref_parent.exists() {
            std::fs::remove_dir(&ref_parent)?;
        }
        assert!(!fixture.git_dir.join(branch).exists());
        assert!(!ref_parent.exists());
        fixture.run_git(&[
            "remote",
            "add",
            "origin",
            fixture.project.to_str().ok_or("project path")?,
        ])?;

        fixture.prepare_git(&mut writer)?;

        assert_eq!(fixture.run_git(&["rev-parse", branch])?, expected);
        assert_eq!(
            std::fs::read_to_string(fixture.git_dir.join(branch))?.trim(),
            expected
        );
        assert_eq!(
            fixture.run_git(&["config", "--local", "--get", "branch.chore/x.remote"])?,
            "origin"
        );
        assert_eq!(
            fixture.run_git(&["config", "--local", "--get", "branch.chore/x.merge"])?,
            branch
        );
        let common = Directory::open_anchor(&fixture.git_dir)?;
        drop(common.try_lock_existing(
            OsStr::new(worktrees::REPOSITORY_LOCK),
            provenant_os::fs::LockMode::Exclusive,
        )?);
        Ok(())
    }

    #[test]
    fn writer_git_preparation_refuses_symlinked_reflog_paths() -> FixtureResult<()> {
        if clean_environment_child(stringify!(
            writer_git_preparation_refuses_symlinked_reflog_paths
        ))? {
            return Ok(());
        }
        let fixture = WriterFixture::new()?;
        let lane = fixture.add_worktree("chore/x", "lane")?;
        let mut writer = fixture.prepare(&lane)?;
        let target = fixture.root.join("reflog-target");
        std::fs::write(&target, b"untouched")?;
        let reflog = fixture.git_dir.join("logs/refs/remotes/origin/chore/x");
        std::fs::create_dir_all(reflog.parent().ok_or("reflog parent")?)?;
        std::os::unix::fs::symlink(&target, &reflog)?;
        let failure = fixture
            .prepare_git(&mut writer)
            .err()
            .ok_or("leaf symlink refusal missing")?;
        assert!(matches!(
            failure.downcast_ref::<RunError>(),
            Some(RunError::OperationIo { path: Some(path), .. }) if path == &reflog
        ));
        assert_eq!(std::fs::read(&target)?, b"untouched");

        std::fs::remove_file(&reflog)?;
        let parent = reflog.parent().ok_or("reflog parent")?;
        std::fs::remove_dir(parent)?;
        std::os::unix::fs::symlink(&fixture.root, parent)?;
        let failure = fixture
            .prepare_git(&mut writer)
            .err()
            .ok_or("parent symlink refusal missing")?;
        assert!(matches!(
            failure.downcast_ref::<RunError>(),
            Some(RunError::OperationIo { path: Some(path), .. }) if path == parent
        ));
        assert_eq!(std::fs::read(&target)?, b"untouched");
        Ok(())
    }

    #[test]
    fn writer_git_preparation_preserves_existing_ref_locks() -> FixtureResult<()> {
        if clean_environment_child(stringify!(
            writer_git_preparation_preserves_existing_ref_locks
        ))? {
            return Ok(());
        }
        let fixture = WriterFixture::new()?;
        let lane = fixture.add_worktree("chore/x", "lane")?;
        let mut writer = fixture.prepare(&lane)?;
        fixture.run_git(&[
            "remote",
            "add",
            "origin",
            fixture.project.to_str().ok_or("project path")?,
        ])?;
        let tracking_lock = fixture.git_dir.join("refs/remotes/origin/chore/x.lock");
        std::fs::create_dir_all(tracking_lock.parent().ok_or("tracking lock parent")?)?;
        std::fs::write(&tracking_lock, b"held")?;
        fixture.prepare_git(&mut writer)?;
        assert_eq!(std::fs::read(&tracking_lock)?, b"held");

        std::fs::remove_file(&tracking_lock)?;
        fixture.run_git(&["remote", "remove", "origin"])?;
        let branch_lock = fixture.git_dir.join("refs/heads/chore/x.lock");
        std::fs::write(&branch_lock, b"held")?;
        fixture.prepare_git(&mut writer)?;
        assert_eq!(std::fs::read(&branch_lock)?, b"held");
        Ok(())
    }

    #[test]
    fn writer_preparation_refuses_incomplete_config_while_config_lock_is_held() -> FixtureResult<()>
    {
        if clean_environment_child(stringify!(
            writer_preparation_refuses_incomplete_config_while_config_lock_is_held
        ))? {
            return Ok(());
        }
        let fixture = WriterFixture::new()?;
        let lane = fixture.add_worktree("chore/config", "lane")?;
        let request = fixture.request(&lane);
        let common = Directory::open_anchor(&fixture.git_dir)?;
        let config_lock = common.try_lock(
            OsStr::new(worktrees::REPOSITORY_LOCK),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let config = common.read_bounded(OsStr::new("config"), 1024 * 1024)?;
        common
            .atomic_write(
                OsStr::new("config"),
                ".test-config-",
                b"[unfinished",
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()?;

        let clock = crate::tests::clock();
        let result = prepare_writer_with_facts_and_clock(
            &request,
            &fixture.state,
            fixture.custody_project,
            &worktrees::RepoFacts::default(),
            &clock,
        );
        let observed = common.read_bounded(OsStr::new("config"), 1024 * 1024);

        let restored = common
            .atomic_write(
                OsStr::new("config"),
                ".test-config-",
                &config,
                provenant_os::fs::PublishMode::Replace,
            )
            .and_then(|publication| publication.require_durable());
        drop(config_lock);
        restored?;
        assert!(matches!(
            result,
            Err(RunError::Filesystem(FsError::LockTimeout { path, timeout }))
                if path.file_name() == Some(OsStr::new(worktrees::REPOSITORY_LOCK))
                    && timeout.is_zero()
        ));
        assert_eq!(observed?, b"[unfinished");
        Ok(())
    }

    #[test]
    fn toolchain_probe_accepts_only_declared_path_entries() {
        use provenant_confinement::permissions::{PathScope, WriteGrant, WriteSource};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let worktree = tmp.path().join("worktree");
        let scratch = tmp.path().join("scratch");
        let bin = home.join(".cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        std::fs::create_dir_all(scratch.join("bin")).unwrap();
        std::fs::create_dir(scratch.join("other")).unwrap();
        std::fs::write(bin.join("rustup"), b"fixture").unwrap();
        std::os::unix::fs::symlink("rustup", bin.join("rustc")).unwrap();
        let grants = [WriteGrant {
            source: WriteSource::Scratch,
            path: scratch.clone(),
            scope: PathScope::Subtree,
        }];
        let writable_home = WriteGrant {
            source: WriteSource::WriteRoot,
            path: home.join(".rustup"),
            scope: PathScope::Subtree,
        };
        let cases = [
            ("immutable host", vec![bin.clone()], &[][..], Ok(())),
            (
                "scratch shim",
                vec![scratch.join("bin"), bin.clone()],
                &grants[..],
                Ok(()),
            ),
            (
                "worktree",
                vec![worktree.clone(), bin.clone()],
                &grants[..],
                Err(codes::PATH_ENTRY_WRITABLE),
            ),
            (
                "other scratch directory",
                vec![scratch.join("other"), bin.clone()],
                &grants[..],
                Err(codes::PATH_ENTRY_WRITABLE),
            ),
        ];
        for (entry, path_entries, path_grants, expected) in cases {
            let path = std::env::join_paths(path_entries).unwrap();
            let env = vec![(OsString::from("PATH"), path)];
            assert_eq!(
                trusted_lane_toolchain_diagnostic(&home, &worktree, &env, path_grants),
                expected,
                "{entry}"
            );
        }
        assert_eq!(
            trusted_lane_toolchain_diagnostic(
                &home,
                &worktree,
                &[(OsString::from("PATH"), bin.as_os_str().to_owned())],
                &[writable_home]
            ),
            Err(codes::RUSTUP_HOME_WRITABLE)
        );
        std::fs::write(scratch.join("bin/rustc"), b"untrusted").unwrap();
        let scratch_path =
            std::env::join_paths([scratch.join("bin").as_path(), bin.as_path()]).unwrap();
        let scratch_env = vec![(OsString::from("PATH"), scratch_path)];
        assert_eq!(
            trusted_lane_toolchain_diagnostic(&home, &worktree, &scratch_env, &grants),
            Err(codes::RUSTC_NOT_RUSTUP_SHIM)
        );
    }

    #[test]
    fn cargo_config_makes_toolchain_unverifiable() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("worktree");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(cache.join(".cargo")).unwrap();
        assert!(verified_cargo_config(&worktree, &cache.join(".cargo")).is_ok());
        let host_config = cache.join(".cargo/config.toml");
        std::fs::write(&host_config, "[build]\nrustc = 'evil'\n").unwrap();
        assert_eq!(
            verified_cargo_config(&worktree, &cache.join(".cargo")),
            Err(codes::CARGO_CONFIG_SELECTS_COMPILER)
        );
        std::fs::write(&host_config, "[build]\njobs = 2\n").unwrap();
        assert!(verified_cargo_config(&worktree, &cache.join(".cargo")).is_ok());
        std::fs::create_dir_all(worktree.join(".cargo")).unwrap();
        let config = worktree.join(".cargo/config.toml");
        // Selecting a compiler/target, directly or through include or [env], is refused.
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
                "[build]\ntarget = 'x86_64-unknown-linux-gnu'\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
            ),
            (
                "[target.x86_64-unknown-linux-gnu]\nlinker = 'secret-linker'\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
            ),
            (
                "[target.aarch64-apple-darwin]\nrustflags = ['-C', 'link-arg=-s']\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
            ),
            (
                "[env]\nCARGO_BUILD_TARGET = 'x86_64-unknown-linux-gnu'\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
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
                verified_cargo_config(&worktree, &cache.join(".cargo")),
                Err(expected),
                "{text:?}"
            );
        }
        std::fs::write(worktree.join(".cargo/config"), "[build]\nrustc = 'evil'\n").unwrap();
        std::fs::remove_file(&config).unwrap();
        assert_eq!(
            verified_cargo_config(&worktree, &cache.join(".cargo")),
            Err(codes::CARGO_CONFIG_SELECTS_COMPILER)
        );
    }

    // Regression for #2484: harmless checked-in Cargo configuration must not
    // disable seed use, while unrecorded target flags must.
    #[test]
    fn checked_in_cargo_config_without_unrecorded_build_settings_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("worktree");
        let cache = tmp.path().join("cache");
        let nested = worktree.join("any/depth/rust/workspace");
        std::fs::create_dir_all(nested.join(".cargo")).unwrap();
        std::fs::create_dir_all(worktree.join(".provenant")).unwrap();
        let mut ports = provenant_config::PortTable::builtin();
        ports.verify.workspace = "any/depth/rust/workspace".into();
        std::fs::write(
            worktree.join(provenant_config::PORTS_CONFIG),
            serde_json::to_vec(&ports).unwrap(),
        )
        .unwrap();
        std::fs::write(nested.join("Cargo.toml"), "[workspace]\n").unwrap();
        let build = crate::target_seed::cargo_build_directory(&worktree, tmp.path())
            .unwrap()
            .unwrap();
        std::fs::create_dir_all(cache.join(".cargo")).unwrap();
        let config = nested.join(".cargo/config.toml");
        for text in [
            "# comments only\n",
            "[alias]\nxtask = 'run -p xtask'\n[env]\nFOO = 'bar'\n",
        ] {
            std::fs::write(&config, text).unwrap();
            assert_eq!(verified_cargo_config(&build, &cache.join(".cargo")), Ok(()));
        }
        std::fs::write(
            &config,
            "[target.aarch64-apple-darwin]\nrustflags = ['-C', 'link-arg=-s']\n",
        )
        .unwrap();
        assert_eq!(
            verified_cargo_config(&build, &cache.join(".cargo")),
            Err(codes::CARGO_CONFIG_CHANGES_BUILD_TARGET)
        );
        // Keep the ancestor-compiler assertion independent of the target flag
        // already rejected above; the first ineligible config owns the result.
        std::fs::remove_file(&config).unwrap();
        // An intermediate config belongs to this Cargo invocation's cone too.
        let ancestor_config = nested.parent().unwrap().join(".cargo/config.toml");
        std::fs::create_dir_all(ancestor_config.parent().unwrap()).unwrap();
        std::fs::write(&ancestor_config, "[build]\nrustc = 'evil'\n").unwrap();
        assert_eq!(
            verified_cargo_config(&build, &cache.join(".cargo")),
            Err(codes::CARGO_CONFIG_SELECTS_COMPILER)
        );
        std::fs::remove_file(ancestor_config).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", nested.join(".cargo/config")).unwrap();
        assert_eq!(
            verified_cargo_config(&build, &cache.join(".cargo")),
            Err(codes::CARGO_CONFIG_UNREADABLE)
        );
    }

    #[test]
    fn target_setup_record_reports_seed_outcomes_and_publication_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(tmp.path()).unwrap();
        for reason in ["disabled", "read_only_lane"] {
            let outcome = crate::cargo_seed::Outcome::skipped(reason);
            record_cargo_setup_file(&directory, "cargo-target-setup.json", &outcome).unwrap();
            let record: serde_json::Value = serde_json::from_slice(
                &std::fs::read(tmp.path().join("cargo-target-setup.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(record["status"], "skipped");
            assert_eq!(record["reason"], reason);
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
        std::fs::remove_file(tmp.path().join("cargo-target-setup.json")).unwrap();
        std::fs::create_dir(tmp.path().join("cargo-target-setup.json")).unwrap();
        assert!(
            record_cargo_setup_file(
                &directory,
                "cargo-target-setup.json",
                &crate::cargo_seed::Outcome::skipped("disabled")
            )
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
        scope: provenant_records::custody::Scope,
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
        Ok(matches!(
            provenant_supervisor::custody::retained_import_decision(&evidence, scope)
                .map_err(error("classify retained import", None))?,
            Some(provenant_supervisor::custody::CustodyDecision::Pending(_))
        ))
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
                lane: record.key.clone(),
                run_attempt: self.attempt_dir(record.attempt_id, false)?.current_path()?,
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
        scope: provenant_records::custody::Scope,
        binding: WriterBinding,
        acquire: &dyn Fn(
            Directory,
            provenant_records::custody::Scope,
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
            retained_scope = provenant_supervisor::custody::pending_import(&slot)
                .map_err(error("classify predecessor writer custody", None))?;
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
                fence: Box::new(retained),
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
        match prepare_writer_with_facts(
            writer,
            &self.writer_registry,
            record.project_id,
            &self.repo_facts,
        ) {
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
    fn writer_registry_projects(&self) -> Result<Vec<ProjectId>> {
        let mut projects = vec![self.context.project_id];
        let root = match Directory::open_anchor(&self.writer_registry) {
            Ok(root) => root,
            Err(error) if error.is_not_found() => return Ok(projects),
            Err(error) => return Err(error.into()),
        };
        let references = match root.open_verified_directory(OsStr::new(WRITER_PROJECTS_DIRECTORY)) {
            Ok(root) => match root
                .open_verified_directory(OsStr::new(&self.context.project_id.to_string()))
            {
                Ok(root) => root,
                Err(error) if error.is_not_found() => return Ok(projects),
                Err(error) => return Err(error.into()),
            },
            Err(error) if error.is_not_found() => return Ok(projects),
            Err(error) => return Err(error.into()),
        };
        for (index, name) in references.entries()?.enumerate() {
            if index >= 10_000 {
                return Err(RunError::Validation(
                    "writer project reference limit exceeded",
                ));
            }
            let name = name?;
            references.open_verified_directory(&name)?;
            let project = name
                .to_str()
                .and_then(|name| ProjectId::parse(name).ok())
                .ok_or(RunError::Validation("invalid writer project reference"))?;
            if !projects.contains(&project) {
                projects.push(project);
            }
        }
        Ok(projects)
    }

    pub fn writer_fence_listing(
        &self,
    ) -> Result<provenant_supervisor::custody::ReservationListing> {
        self.writer_fence_listing_for_admission(false)
    }

    fn writer_fence_listing_for_admission(
        &self,
        all_scopes: bool,
    ) -> Result<provenant_supervisor::custody::ReservationListing> {
        let mut result = provenant_supervisor::custody::ReservationListing::default();
        for project in self.writer_registry_projects()? {
            let path = self
                .writer_registry
                .join("worktrees")
                .join(project.to_string());
            let directory = match Directory::open_anchor(&path) {
                Ok(directory) => directory,
                Err(error) if error.is_not_found() => continue,
                Err(error) => return Err(error.into()),
            };
            let listing = provenant_supervisor::custody::list_writer_reservations(&directory)
                .map_err(error("list writer reservations", Some(&path)))?;
            for mut reservation in listing.reservations {
                // Referenced projects may also have unrelated runs. Observation
                // follows only this graph project's accepted run namespace.
                if !all_scopes && project != self.context.project_id {
                    let belongs: bool = self.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND project=?2 AND refused=0)",
                        rusqlite::params![
                            reservation.scope.attempt.to_string(),
                            self.context.project_id.to_string()
                        ],
                        |row| row.get(0),
                    )?;
                    if !belongs {
                        continue;
                    }
                }
                reservation.reservation = path.join(&reservation.reservation);
                result.reservations.push(reservation);
            }
            for mut issue in listing.issues {
                issue.reservation = path.join(&issue.reservation);
                result.issues.push(issue);
            }
        }
        Ok(result)
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
        scope: provenant_records::custody::Scope,
    ) -> Result<()> {
        let policy = writer_policy_for_attempt(state, attempt);
        seal_writer_ownership(attempt, &self.worktree)?;
        let utility = runtime(state)?;
        let head = self
            .worktree
            .repo()
            .private_dir()
            .read_stable(OsStr::new("HEAD"), 1024)?;
        if std::str::from_utf8(&head).map(str::trim).ok()
            != Some(format!("ref: {}", self.branch.as_str()).as_str())
        {
            return Err(policy.boundary_error(
                "WRITER_WRITERLAUNCH_SNAPSHOT_GIT_ISOLATION",
                provenant_records::policy::Facts::violation(
                    "writer worktree HEAD no longer names the selected lane branch",
                ),
                RunError::WriterHeadNotLaneBranch,
            ));
        }
        let head = crate::objects::resolve_ref(
            utility.git_program_path(),
            self.worktree.repo(),
            self.branch.as_str(),
        )?;
        if head != self.base {
            return Err(policy.boundary_error(
                "WRITER_WRITERLAUNCH_SNAPSHOT_GIT_ISOLATION",
                provenant_records::policy::Facts::violation(
                    "writer selected branch head changed before launch",
                ),
                RunError::Validation("writer base changed before launch"),
            ));
        }
        crate::artifacts::seal(
            attempt,
            "writer-git-pins.json",
            &crate::db::bytes(&GitPins {
                extra: Default::default(),
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
            Ok(_) => {
                return Err(policy.boundary_error(
                    "WRITER_WRITERLAUNCH_SNAPSHOT_GIT_ISOLATION",
                    provenant_records::policy::Facts::violation(
                        "writer lane-base reference changed after preparation",
                    ),
                    RunError::Validation("writer Git base pin changed"),
                ));
            }
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
    fn report_write_scope(
        &self,
        state: &Path,
        attempt: &Directory,
        base: GitOid,
        head: GitOid,
        source: Option<&crate::objects::ObjectSource>,
    ) -> Result<()> {
        let Some(raw) =
            crate::artifacts::read_sealed(attempt, "request.json", crate::RECORD_LIMIT)?
        else {
            return Ok(());
        };
        let request: crate::CanonicalRequest = crate::db::decode(&raw)?;
        let scope = &request.task.profile.write_scope;
        if scope.is_empty() {
            return Ok(());
        }
        let scope: Vec<_> = scope.iter().map(Path::new).collect();
        if scope.is_empty() {
            return Ok(());
        }
        let project = request
            .task
            .profile
            .policy_project
            .map(|project| project.to_string())
            .unwrap_or_default();
        let policy = crate::launch_policy(state, &project, &request.task);
        let saved = crate::artifacts::read_sealed(
            attempt,
            "policy-write-scope.json",
            provenant_records::run::TERMINAL_RECORD_LIMIT,
        )
        .map_err(|_| invalid_write_scope_snapshot(&policy))?;
        if let Some(raw) = saved {
            let report: provenant_records::handoff::WriteScopeReport = crate::db::decode(&raw)?;
            let provenant_records::handoff::WriteScopeReport::Full {
                schema_version,
                code,
                base: saved_base,
                head: saved_head,
                out_of_scope_paths,
                ..
            } = report
            else {
                return Err(invalid_write_scope_snapshot(&policy));
            };
            if schema_version != 1
                || code != "WRITE_SCOPE"
                || saved_base != base.to_string()
                || saved_head != head.to_string()
            {
                return Err(invalid_write_scope_snapshot(&policy));
            }
            let effect = policy.check(
                "WRITE_SCOPE",
                provenant_records::policy::Facts {
                    violated: !out_of_scope_paths.is_empty(),
                    detail: format!(
                        "{} paths outside declared write scope; see policy-write-scope.json",
                        out_of_scope_paths.len()
                    ),
                },
            );
            if let Some(message) = effect.message() {
                let _ = writeln!(std::io::stderr().lock(), "policy: {message}");
            }
            if effect.blocks() {
                return Err(RunError::Validation("WRITE_SCOPE: strict scope policy"));
            }
            return Ok(());
        }
        let repo = self.worktree.repo();
        let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
        let source_refs = source
            .map(|source| source.refs.current_path())
            .transpose()?;
        let source_objects = source
            .map(|source| source.objects.current_path())
            .transpose()?;
        let refs = source_refs.as_deref().unwrap_or(repo.private_git_dir());
        let ref_index = refs.join("index");
        if let Some(source) = source {
            source
                .refs
                .read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
        } else {
            repo.private_dir()
                .read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
        }
        let mut indexes = vec![ref_index];
        // The native index may contain staged deletions after the lane HEAD
        // advances, so inspect it alongside private Git on the first snapshot.
        repo.private_dir()
            .read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
        let native_index = repo.private_dir().current_path()?.join("index");
        if !indexes.contains(&native_index) {
            indexes.push(native_index);
        }
        let source_objects = source_objects.as_deref();
        let alternate = repo.common_git_dir().join("objects");
        let (changed, status) =
            crate::objects::with_clean_view(&git, repo, head, None, |_, path| {
                let run = |arguments: &[&str], index: &Path| {
                    crate::objects::execute_bounded(
                        &git,
                        path,
                        repo.root_path(),
                        &arguments.iter().map(OsString::from).collect::<Vec<_>>(),
                        None,
                        crate::objects::Execution {
                            object_directory: source_objects,
                            index_file: Some(index),
                            alternate_directory: Some(&alternate),
                            ..Default::default()
                        },
                    )
                };
                run(&["update-ref", "HEAD", &head.to_string()], &indexes[0])?;
                let changed = run(
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--no-renames",
                        "--name-only",
                        "-z",
                        &base.to_string(),
                        &head.to_string(),
                        "--",
                    ],
                    &indexes[0],
                )?;
                let mut status = Vec::new();
                for index in &indexes {
                    status.extend(run(
                        &[
                            "--no-optional-locks",
                            "--work-tree",
                            repo.root_path()
                                .to_str()
                                .ok_or(RunError::Validation("worktree path"))?,
                            "status",
                            "--porcelain",
                            "-z",
                            "--untracked-files=all",
                            "--ignore-submodules=all",
                        ],
                        index,
                    )?);
                }
                Ok((changed, status))
            })?;
        let mut paths: Vec<Vec<u8>> = changed
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        paths.extend(status_paths_raw(&status, true)?);
        use std::os::unix::ffi::OsStringExt;
        paths.retain(|path| {
            let path = PathBuf::from(OsString::from_vec(path.clone()));
            !scope
                .iter()
                .any(|root| *root == Path::new(".") || path.starts_with(root))
        });
        paths.sort();
        paths.dedup();
        let paths: Vec<_> = paths
            .into_iter()
            .map(|path| {
                let path = PathBuf::from(OsString::from_vec(path));
                let relative = provenant_core::RelativePath::parse(&path)
                    .map_err(|_| RunError::Validation("invalid scope path"))?;
                provenant_records::PathWire::from_relative(&relative)
                    .map_err(|_| RunError::Validation("scope path encoding"))
            })
            .collect::<Result<_>>()?;
        let effect = policy.check(
            "WRITE_SCOPE",
            provenant_records::policy::Facts {
                violated: !paths.is_empty(),
                detail: format!(
                    "{} paths outside declared write scope; see policy-write-scope.json",
                    paths.len()
                ),
            },
        );
        crate::artifacts::seal(
            attempt,
            "policy-write-scope.json",
            &crate::db::bytes(&provenant_records::handoff::WriteScopeReport::Full {
                schema_version: 1,
                code: "WRITE_SCOPE".into(),
                base: base.to_string(),
                head: head.to_string(),
                strength: policy.rule("WRITE_SCOPE").rule.strength,
                source: policy.rule("WRITE_SCOPE").source,
                out_of_scope_paths: paths,
            })?,
            provenant_records::run::TERMINAL_RECORD_LIMIT,
        )?;
        if let Some(message) = effect.message() {
            let _ = writeln!(std::io::stderr().lock(), "policy: {message}");
        }
        if effect.blocks() {
            return Err(RunError::Validation("WRITE_SCOPE: strict scope policy"));
        }
        Ok(())
    }

    pub(crate) fn terminal(
        &self,
        state: &Path,
        attempt: &Directory,
        custody: &provenant_supervisor::custody::Recovery,
        settlement: &mut crate::settlement::Driver,
        policy: TerminalPolicy<'_>,
    ) -> Result<()> {
        let TerminalPolicy {
            preservation_stop,
            budget_stop,
            work,
        } = policy;
        if custody.observe() != provenant_supervisor::custody::RecoveryState::Absent {
            return Err(RunError::Validation(
                "writer object import requires absent custody",
            ));
        }
        if let Some(stop) = preservation_stop {
            if stop.marker_vetoes(attempt)? {
                return Err(RunError::Validation("lane import cancelled"));
            }
        } else if crate::store::StopMarker::read(attempt, custody.scope())?
            != crate::store::StopMarker::Absent
        {
            return Err(RunError::Validation("lane import cancelled"));
        }
        let archive = crate::artifacts::subdir(attempt, OsStr::new("quarantine"), true)?;
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
        // hash-checked. Do this before ancestry checks.
        let source = crate::objects::bound_source(attempt)?;
        let git_executable = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
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
        if budget_stop
            && settlement.head().is_none()
            && crate::artifacts::read_sealed(attempt, "budget-commit.json", crate::RECORD_LIMIT)?
                .is_none()
        {
            // Recovery holds writer exclusion and custody is absent. Commit
            // failure is diagnostic and must not replace the budget outcome.
            let (note, work) = match self.commit_budget_stop(attempt, base) {
                Ok(commit) => (commit.note.to_owned(), commit.committed),
                Err(error) => (format!("budget commit failed: {error}"), None),
            };
            let record = match work {
                Some(work) => serde_json::json!({
                    "schema_version": 1,
                    "note": note,
                    "commit": work.sha,
                    "files": work.files,
                    "paths": work.paths,
                }),
                None => serde_json::json!({"schema_version":1,"note":note}),
            };
            crate::artifacts::seal(
                attempt,
                "budget-commit.json",
                &crate::db::bytes(&record)?,
                crate::RECORD_LIMIT,
            )?;
        }
        let verified_tip = match settlement.head() {
            Some(head) => head,
            None => self.effective_head(attempt, base)?.0,
        };
        settlement.set_head(verified_tip)?;
        if let Err(error) =
            self.report_write_scope(state, attempt, base, verified_tip, source.as_ref())
        {
            if error.boundary_class() != provenant_records::diagnostic::BoundaryClass::None {
                return Err(error);
            }
            let request: crate::CanonicalRequest = crate::db::decode(
                &crate::artifacts::read_sealed(attempt, "request.json", crate::RECORD_LIMIT)?
                    .ok_or(RunError::NotFound)?,
            )?;
            let project = request
                .task
                .profile
                .policy_project
                .map(|project| project.to_string())
                .unwrap_or_default();
            let policy = crate::launch_policy(state, &project, &request.task);
            let effect = policy.check(
                "WRITE_SCOPE",
                provenant_records::policy::Facts::violation(format!(
                    "scope report unavailable: {error}"
                )),
            );
            if effect.blocks() {
                return Err(error);
            }
            if let Some(message) = effect.message() {
                let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
            }
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
        let import_cancelled = if let Some(stop) = preservation_stop {
            stop.marker_vetoes(attempt)?
        } else {
            crate::store::StopMarker::read(attempt, custody.scope())?
                != crate::store::StopMarker::Absent
        };
        if import_cancelled {
            return Err(RunError::Validation("lane import cancelled"));
        }
        if settlement.phase() == crate::settlement::Phase::Publish {
            let request: crate::CanonicalRequest = crate::db::decode(
                &crate::artifacts::read_sealed(attempt, "request.json", crate::RECORD_LIMIT)?
                    .ok_or(RunError::NotFound)?,
            )?;
            let project = request
                .task
                .profile
                .policy_project
                .map(|project| project.to_string())
                .unwrap_or_default();
            let policy = crate::launch_policy(state, &project, &request.task);
            let ancestor = crate::objects::publish_branch(
                &git_executable,
                self.worktree.repo(),
                self.branch.as_str(),
                base,
                request.task.base_ref.as_deref(),
                verified_tip,
                &policy,
            )?;
            if let Some(warning) = ancestor.warning(self.branch.as_str(), base, verified_tip) {
                // Best effort: the ancestor is also in the branch reflog.
                let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
                let _ = attempt
                    .atomic_write(
                        OsStr::new("publication-ancestor.json"),
                        ".run-",
                        &crate::db::bytes(&serde_json::json!({
                            "schema_version": 1,
                            "branch": self.branch.as_str(),
                            "tip": verified_tip,
                            "launch_base": base,
                            "warning": warning,
                        }))?,
                        provenant_os::fs::PublishMode::Replace,
                    )
                    .and_then(|publication| publication.require_durable());
            }
            // Restore the private index only after receive succeeds.
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
                && crate::objects::private_tip(source, self.branch.as_str())? == verified_tip
                && crate::objects::resolve_ref(
                    &git_executable,
                    self.worktree.repo(),
                    self.branch.as_str(),
                )? == verified_tip
            {
                self.advance_files(attempt, verified_tip)?;
                crate::objects::restore_index(source, self.worktree.repo())?;
                if !merge_objects.is_empty() {
                    crate::artifacts::seal(
                        attempt,
                        "merge-restored.json",
                        &crate::db::bytes(&MergeRestored {
                            extra: Default::default(),
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
        let tip = self.facts.repo_head(
            self.worktree.repo(),
            &executor,
            &utility,
            budget()?,
            &cancel,
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
        crate::holder::seal_manifest(
            &archive,
            "manifest.json",
            &crate::db::bytes(&WriterManifest {
                extra: Default::default(),
                schema_version: 1,
                scope: custody.scope(),
                kind: "quarantine_manifest",
                disposition: "writer_settled",
            })?,
            crate::RECORD_LIMIT,
        )?;
        if self.facts.resolve_repo_commit(
            self.worktree.repo(),
            &executor,
            &utility,
            budget()?,
            self.branch.clone(),
            &cancel,
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
        // The remote ref and dirty census can change after the first pass.
        // Once sealed, this attempt's summary is immutable even if its effect
        // receipt transaction failed. Reuse it after revalidating the pinned head.
        if let Some(raw) =
            crate::artifacts::read_sealed(attempt, "writer-terminal.json", crate::RECORD_LIMIT)?
        {
            let saved: WriterTerminal = crate::db::decode(&raw)?;
            if saved.schema_version != 1
                || saved.head != verified_tip
                || saved.stop_work.as_ref().is_some_and(|summary| {
                    summary.schema_version != 1 || summary.head != Some(verified_tip)
                })
            {
                return Err(RunError::Conflict);
            }
            return Ok(());
        }
        let stop_work = work.map(|policy| {
            use provenant_records::run::StopWorkPush;
            let dirty_paths = self
                .uncommitted_paths(&git_executable)
                .ok()
                .and_then(|paths| u64::try_from(paths.len()).ok());
            let ahead = self
                .ahead_of_remote(&git_executable, verified_tip)
                .ok()
                .flatten();
            let recovered = if policy.publish.is_none() && policy.publish_granted {
                (|| -> Result<crate::HostDeliveryContext> {
                    let branch = self
                        .branch
                        .as_str()
                        .strip_prefix("refs/heads/")
                        .ok_or(RunError::Validation("writer needs a local branch"))?;
                    let request =
                        crate::HostDeliveryContext::destination(crate::DeliveryMode::Push, branch)?
                            .ok_or(RunError::Validation("stop publication destination missing"))?;
                    crate::HostDeliveryContext::capture()
                        .with_repo_facts(&self.facts)
                        .with_host_state(state)
                        .prepare_stop_publish_recovery(
                            policy.writer,
                            &request,
                            attempt,
                            custody.scope(),
                            verified_tip,
                            base,
                        )
                })()
                .map_err(|error| {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "warning: stopped work remains unpushed: {error}"
                    );
                    error
                })
                .ok()
            } else {
                None
            };
            let host = policy.publish.or(recovered.as_ref());
            let push = if !policy.publish_granted {
                StopWorkPush::NotGranted
            } else if ahead == Some(0) {
                StopWorkPush::NotNeeded
            } else if let Some(host) = host {
                let intent_still_matches = match preservation_stop {
                    Some(stop) => !stop.marker_vetoes(attempt).unwrap_or(true),
                    None => crate::store::StopMarker::read(attempt, custody.scope())
                        .is_ok_and(|intent| intent == crate::store::StopMarker::Absent),
                };
                if !intent_still_matches || cancel.is_cancelled() {
                    return provenant_records::run::StopWorkSummary {
                        extra: Default::default(),
                        schema_version: 1,
                        trigger: policy.trigger,
                        head: Some(verified_tip),
                        ahead,
                        dirty_paths,
                        push: StopWorkPush::NotAttempted,
                    };
                }
                match host.clone().cancellable(&cancel).push_stopped_head(
                    policy.writer,
                    attempt,
                    custody.scope(),
                    verified_tip,
                ) {
                    Ok(()) => StopWorkPush::Pushed,
                    Err(error) => {
                        let _ = writeln!(
                            std::io::stderr().lock(),
                            "warning: stopped work remains unpushed: {error}"
                        );
                        StopWorkPush::Failed
                    }
                }
            } else {
                StopWorkPush::NotAttempted
            };
            provenant_records::run::StopWorkSummary {
                extra: Default::default(),
                schema_version: 1,
                trigger: policy.trigger,
                head: Some(verified_tip),
                ahead,
                dirty_paths,
                push,
            }
        });
        crate::artifacts::seal(
            attempt,
            "writer-terminal.json",
            &crate::db::bytes(&WriterTerminal {
                schema_version: 1,
                // Replay may observe a later writer's descendant on the branch.
                // Attribute only the head pinned for this attempt's settlement.
                head: verified_tip,
                stop_work,
            })?,
            crate::RECORD_LIMIT,
        )?;
        Ok(())
    }
}
impl WriterLaunch {
    /// Put the bound worktree's files back at the branch the host still holds
    /// when the lane's commits could not be published, and say where the work
    /// is. Only files that still hold the lane's own content move. Best effort:
    /// the note says what happened either way.
    pub(crate) fn unpublish(&self, attempt: &Directory) -> String {
        let attempt_work = || -> Result<String> {
            let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
            let Some(source) = crate::objects::bound_source(attempt)? else {
                return Ok(String::new());
            };
            let repo = self.worktree.repo();
            let branch = self.branch.as_str();
            let tip = crate::objects::private_tip(&source, branch)?;
            let held = crate::objects::resolve_ref(&git, repo, branch)?;
            let location = format!(
                "the lane's work is commit {tip} on branch {branch}, retained in its private Git"
            );
            if tip == self.base || held != self.base {
                return Ok(String::new());
            }
            let scratch = source.refs.current_path()?;
            let objects = source.objects.current_path()?;
            let moved = crate::objects::worktree::converge(
                &git,
                repo,
                &crate::objects::worktree::Stores {
                    git_dir: &scratch,
                    objects: Some(&objects),
                },
                tip,
                self.base,
            )?;
            crate::objects::worktree::seal_reverted(attempt, tip)?;
            let mut note = format!(
                "worktree restored to {} ({} files); {location}",
                self.base, moved.moved
            );
            if !moved.kept.is_empty() {
                let listed: Vec<_> = moved
                    .kept
                    .iter()
                    .take(UNCOMMITTED_LISTED)
                    .cloned()
                    .collect();
                note.push_str(&format!("; left as found: {}", listed.join(", ")));
            }
            Ok(note)
        };
        attempt_work().unwrap_or_else(|error| format!("worktree files not restored: {error}"))
    }

    /// After a revert, publication moves the files forward again so the files,
    /// index and HEAD agree at the published commit.
    fn advance_files(&self, attempt: &Directory, tip: GitOid) -> Result<()> {
        if !crate::objects::worktree::reverted(attempt)? || tip == self.base {
            return Ok(());
        }
        let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
        let repo = self.worktree.repo();
        crate::objects::worktree::converge(
            &git,
            repo,
            &crate::objects::worktree::Stores {
                git_dir: repo.common_git_dir(),
                objects: None,
            },
            self.base,
            tip,
        )?;
        Ok(())
    }
}
/// How many dirty paths a reason names before it counts the rest.
const UNCOMMITTED_LISTED: usize = 12;
const WRITER_PROJECTS_DIRECTORY: &str = "writer-projects";

#[cfg(test)]
fn writer_projects_path(registry: &Path) -> PathBuf {
    registry.join(WRITER_PROJECTS_DIRECTORY)
}

fn invalid_write_scope_snapshot(policy: &provenant_records::policy::Policy) -> RunError {
    policy.boundary_error(
        "ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_DATA_INTEGRITY",
        provenant_records::policy::Facts::violation(
            "sealed write-scope report does not match this writer settlement",
        ),
        RunError::artifact_mismatch(
            crate::ArtifactCheck::SealConflict,
            "policy-write-scope.json",
            None,
            None,
        ),
    )
}

fn writer_policy_for_attempt(
    state: &Path,
    attempt: &Directory,
) -> provenant_records::policy::Policy {
    let request = crate::artifacts::read_sealed(attempt, "request.json", crate::RECORD_LIMIT)
        .ok()
        .flatten()
        .and_then(|raw| crate::db::decode::<crate::CanonicalRequest>(&raw).ok());
    let Some(request) = request else {
        return Default::default();
    };
    let project = request
        .task
        .profile
        .policy_project
        .map(|project| project.to_string())
        .unwrap_or_default();
    crate::launch_policy(state, &project, &request.task)
}

impl WriterLaunch {
    /// A diagnostic naming the edits a writer left uncommitted, or `None` when
    /// it committed or its tree is clean. Call it after `terminal` succeeds.
    pub(crate) fn uncommitted(
        &self,
        review_exempt: &provenant_confinement::review_exempt::ReviewExempt,
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
        let paths = git.uncommitted_since(base, review_exempt).map_err(error(
            "read writer uncommitted changes",
            Some(self.worktree.path()),
        ))?;
        Ok(uncommitted_reason(&paths).map(|work| work.render()))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct UncommittedWork {
    paths: Vec<String>,
    omitted: usize,
}

impl UncommittedWork {
    fn render(&self) -> String {
        let mut reason = format!(
            "writer left uncommitted edits and no new commit: {}",
            self.paths.join(", ")
        );
        if self.omitted > 0 {
            reason.push_str(&format!(" and {} more", self.omitted));
        }
        reason.truncate(reason.floor_char_boundary(1024));
        reason
    }
}

fn uncommitted_reason(paths: &[Vec<u8>]) -> Option<UncommittedWork> {
    if paths.is_empty() {
        return None;
    }
    let listed: Vec<_> = paths
        .iter()
        .take(UNCOMMITTED_LISTED)
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    Some(UncommittedWork {
        omitted: paths.len().saturating_sub(listed.len()),
        paths: listed,
    })
}
#[cfg(test)]
mod uncommitted_tests {
    use super::*;

    #[test]
    fn dirty_work_projection_keeps_paths_and_omitted_count_structured() {
        assert_eq!(uncommitted_reason(&[]), None);
        let one = uncommitted_reason(&[b"a.rs".to_vec(), b"b/c.rs".to_vec()]).unwrap();
        assert_eq!(
            one.paths.iter().map(String::as_str).collect::<Vec<_>>(),
            ["a.rs", "b/c.rs"]
        );
        assert_eq!(one.omitted, 0);
        let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n:02}").into_bytes()).collect();
        let reason = uncommitted_reason(&many).unwrap();
        assert_eq!(reason.paths.len(), UNCOMMITTED_LISTED);
        assert_eq!(reason.omitted, many.len() - reason.paths.len());
        assert!(reason.render().len() <= 1024);
        let long = uncommitted_reason(&[vec![b'x'; 4000]]).unwrap();
        assert!(long.render().len() <= 1024);
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct WriterTerminal {
    pub schema_version: u64,
    pub head: GitOid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_work: Option<provenant_records::run::StopWorkSummary>,
}
#[derive(serde::Serialize)]
struct WriterManifest {
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    schema_version: u64,
    scope: provenant_records::custody::Scope,
    kind: &'static str,
    disposition: &'static str,
}

#[cfg(test)]
mod stored_shape_tests {
    use super::*;
    use serde::{Serialize, de::DeserializeOwned};

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn unknown_field_survives<T>(record: T) -> TestResult
    where
        T: DeserializeOwned + Serialize,
    {
        let mut payload = serde_json::to_value(record)?;
        let future = serde_json::json!({"value": 2});
        payload
            .as_object_mut()
            .ok_or("stored record object")?
            .insert("future_minor_field".into(), future.clone());
        let bytes = serde_json::to_vec(&payload)?;
        let decoded: T =
            provenant_records::decode(bytes.as_slice(), provenant_records::Limits::default())?;
        let encoded = provenant_records::encode(&decoded)?;
        let round_trip: serde_json::Value =
            provenant_records::decode(encoded.as_slice(), provenant_records::Limits::default())?;
        assert_eq!(round_trip.get("future_minor_field"), Some(&future));
        Ok(())
    }

    #[test]
    fn persisted_writer_records_preserve_fields_from_newer_minors() -> TestResult {
        let scope = provenant_records::custody::Scope {
            attempt: provenant_core::AttemptId::from_bytes([1; 16]),
            generation: 1,
        };
        let oid = GitOid::parse("1111111111111111111111111111111111111111")?;
        unknown_field_survives(GitPins {
            extra: Default::default(),
            schema_version: 1,
            scope,
            base: oid,
        })?;
        unknown_field_survives(MergeRestored {
            extra: Default::default(),
            schema_version: 1,
            scope,
        })?;
        unknown_field_survives(WriterOwnership {
            extra: Default::default(),
            schema_version: 1,
            lane_owned: false,
            owner: None,
        })?;
        Ok(())
    }
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

/// Preserve sealed current launch ownership evidence on replay.
pub(crate) fn seal_writer_ownership(
    attempt: &Directory,
    tree: &provenant_confinement::VerifiedWorktree,
) -> Result<()> {
    let owner = worktrees::load_owner(tree)?;
    let owned = owner
        .as_ref()
        .is_some_and(|owner| owner.origin == worktrees::WorktreeOrigin::Lane);
    let ownership = WriterOwnership {
        extra: Default::default(),
        schema_version: 1,
        lane_owned: owned,
        owner,
    };
    if let Some(raw) =
        crate::artifacts::read_sealed(attempt, "writer-owned.json", crate::RECORD_LIMIT)?
    {
        let prior: WriterOwnership = crate::db::decode(&raw)?;
        if prior.schema_version != 1
            || prior.lane_owned != ownership.lane_owned
            || prior.owner != ownership.owner
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
    #[serde(default, flatten)]
    pub extra: provenant_records::ExtraFields,
    pub schema_version: u64,
    pub lane_owned: bool,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub owner: Option<worktrees::WorktreeOwner>,
}

#[cfg(test)]
mod writer_ownership_shape_tests {
    use super::WriterOwnership;

    #[test]
    fn writer_ownership_requires_owner_member_but_accepts_unknown_owner() {
        let current = serde_json::json!({
            "schema_version": 1,
            "lane_owned": false,
            "owner": null,
        });
        let parsed: WriterOwnership = serde_json::from_value(current.clone()).unwrap();
        assert_eq!(parsed.owner, None);

        let mut absent = current;
        absent.as_object_mut().unwrap().remove("owner");
        assert!(serde_json::from_value::<WriterOwnership>(absent).is_err());
    }
}

#[cfg(test)]
mod custody_admission_tests {
    use super::*;
    use crate::tests::{call, clock, context};
    use provenant_core::AttemptId;
    use provenant_records::custody::Scope;
    use provenant_supervisor::custody::{self};

    #[test]
    fn foreign_writer_recovery_is_scoped_but_admission_keeps_predecessor_fences()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = crate::RunStore::open(&context, &call)?;
        let source_project = ProjectId::from_bytes([99; 16]);
        let root = context.state_root.as_path();
        let refs = writer_projects_path(root)
            .join(context.project_id.to_string())
            .join(source_project.to_string());
        std::fs::create_dir_all(&refs)?;
        let slot = root
            .join("worktrees")
            .join(source_project.to_string())
            .join("shared");
        std::fs::create_dir_all(&slot)?;
        let predecessor = Scope {
            attempt: AttemptId::from_bytes([82; 16]),
            generation: 1,
        };
        let lease = custody::acquire_writer(
            Directory::open_anchor(&slot)?,
            predecessor,
            WriterBinding::read_only(),
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let evidence = Directory::open_anchor(&slot.join(predecessor.attempt.to_string()))?;
        crate::artifacts::seal(
            &evidence,
            "import-retained.json",
            &crate::db::bytes(&ImportFence {
                schema_version: 1,
                scope: predecessor,
                lane: provenant_records::WorkKey::new(source_project, "retained", "writer", 1)?,
                run_attempt: temp.path().join("retained-run"),
            })?,
            crate::RECORD_LIMIT,
        )?;
        drop(lease);
        assert!(store.writer_fence_listing()?.reservations.is_empty());
        let all = store.writer_fence_listing_for_admission(true)?;
        assert!(
            all.reservations
                .iter()
                .any(|fence| fence.scope == predecessor)
        );
        // An unrelated, malformed project reference must never be scanned
        // while admitting this worktree. Its own retained import still fences it.
        std::fs::create_dir(
            writer_projects_path(root)
                .join(context.project_id.to_string())
                .join("unrelated-invalid-project"),
        )?;
        let next = Scope {
            attempt: AttemptId::from_bytes([84; 16]),
            generation: 1,
        };
        let refused = store.acquire_writer_fenced(
            Directory::open_anchor(&slot)?,
            next,
            WriterBinding::read_only(),
            &|directory, scope, binding| {
                custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                )
            },
            &call,
        );
        assert!(
            matches!(refused, Err(WriterCustodyError::Admission(custody::CustodyError::ImportRetained {fence})) if fence.scope == predecessor)
        );
        assert!(custody::reservation_present(
            &Directory::open_anchor(&slot)?,
            predecessor
        )?);
        assert!(!custody::reservation_present(
            &Directory::open_anchor(&slot)?,
            next
        )?);
        let writer = WriterRequest {
            extra: Default::default(),
            custody_project: Some(source_project),
            project: temp.path().join("source"),
            worktree: temp.path().join("checkout"),
        };
        assert_eq!(
            store.writer_slot(&writer)?,
            root.join("worktrees")
                .join(source_project.to_string())
                .join(worktrees::state_dir_name(&writer.worktree))
        );
        Ok(())
    }

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

#[cfg(test)]
mod launch_resource_limit_tests {
    use super::*;

    #[test]
    fn unavailable_resource_settings_use_compiled_limits() -> std::io::Result<()> {
        let state = tempfile::tempdir()?;
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version=1\n[domains.run]\ngrant_limit=0\n",
        )?;
        let policy = provenant_records::policy::Policy::default();
        let limits = launch_resource_limits(state.path(), &policy);
        let defaults = ProfileLimits::default();
        assert_eq!(limits.limits.grant_limit, defaults.grant_limit);
        assert_eq!(
            limits.grant_origin,
            provenant_records::diagnostic::SettingSource::ShippedDefault
        );
        assert_eq!(
            limits.limits.profile_limit_bytes,
            defaults.profile_limit_bytes
        );
        assert_eq!(
            limits.profile_origin,
            provenant_records::diagnostic::SettingSource::ShippedDefault
        );
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version=1\n[domains.run]\ngrant_limit=128\n",
        )?;
        let explicit_default = launch_resource_limits(state.path(), &policy);
        assert_eq!(
            explicit_default.grant_origin,
            provenant_records::diagnostic::SettingSource::Configuration
        );
        assert_eq!(
            explicit_default.profile_origin,
            provenant_records::diagnostic::SettingSource::ShippedDefault
        );
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version=1\n[domains.run]\ngrant_limit=256\nprofile_limit_bytes=1048576\n",
        )?;
        let configured = launch_resource_limits(state.path(), &policy);
        assert_eq!(configured.limits.grant_limit, 256);
        assert_eq!(
            configured.grant_origin,
            provenant_records::diagnostic::SettingSource::Configuration
        );
        assert_eq!(configured.limits.profile_limit_bytes, 1048576);
        assert_eq!(
            configured.profile_origin,
            provenant_records::diagnostic::SettingSource::Configuration
        );
        let unrelated = configured
            .annotate(provenant_confinement::permissions::PermissionError::ProtectedOverlap);
        assert!(matches!(
            unrelated,
            provenant_confinement::permissions::PermissionError::ProtectedOverlap
        ));
        Ok(())
    }
}
