//! Closed, bounded Git reads and separately typed Git custody mutations.
use crate::repo::{
    GitRefName, GitRevision, ReadOnlyRepo, RepoError, RepoObjectFormat, RepoPath, RepoPathError,
    RepoProbeData, VerifiedRepo, VerifiedWorktree,
};
use crate::transport::{self, ConfigEntry, LaneRoots, TransportError};
use provenant_core::{CancelToken, GitOid};
use provenant_os::{
    exec::{ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy, Termination},
    fs::{Directory, FileKind, FsError, Metadata, canonicalize, effective_uid},
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use thiserror::Error;

const DEFAULT_STREAM_LIMIT: usize = 8 * 1024 * 1024;
const MAX_STREAM_LIMIT: usize = 64 * 1024 * 1024;
const DEFAULT_TERMINATE_GRACE: Duration = Duration::from_millis(250);
const DEFAULT_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_UTILITY_TIMEOUT: Duration = Duration::from_secs(14_400);
const MAX_CLEANUP_BUDGET: Duration = Duration::from_secs(2);
const MAX_GIT_LOG: u64 = 1000;
const MAX_LANE_CONFIG_FILES: usize = 32;
const MAX_FETCH_STDERR: usize = 1024;
const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const EMPTY_TREE_SHA256: &str = "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";
const GIT_COMMON: &[&str] = &[
    "--no-pager",
    "--no-optional-locks",
    "--literal-pathspecs",
    "-c",
    "submodule.recurse=false",
    "-c",
    "diff.ignoreSubmodules=all",
];
const GIT_CONFIG_OVERRIDES: [(&str, &str); 13] = [
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("protocol.allow", "never"),
    ("log.showSignature", "false"),
    ("core.attributesFile", "/dev/null"),
    ("status.submoduleSummary", "false"),
    ("diff.external", ""),
    ("diff.trustExitCode", "false"),
    ("credential.helper", ""),
    ("credential.interactive", "false"),
    ("extensions.worktreeConfig", "true"),
    ("core.bare", "false"),
    ("core.fsmonitorHookVersion", "0"),
];

#[derive(Debug, Error)]
pub enum UtilityError {
    #[error("invalid pinned {tool} executable")]
    InvalidExecutable { tool: &'static str },
    #[error("utility home must be an absolute private directory")]
    InvalidHome,
    #[error("utility budget is outside its supported bounds")]
    InvalidBudget,
    #[error("utility operation has an invalid or unsupported value")]
    InvalidOperationValue,
    #[error("repository Git configuration could not be read")]
    UnreadableGitConfiguration,
    #[error(
        "Git configuration key {key} comes from {file}, a location lanes can write, and cannot be neutralised; remove that include or move the file out of the project"
    )]
    LaneConfiguration { file: String, key: String },
    #[error("pinned Git does not support a verified attribute source")]
    AttributeSourceUnsupported,
    #[error("verified repository was not opened with its required Git probes")]
    UnprobedRepository,
    #[error("utility {stream} exceeded its capture limit; output cannot be used as evidence")]
    Truncated { stream: &'static str },
    #[error("utility command timed out; output cannot be used as evidence")]
    TimedOut,
    #[error("utility read was cancelled; output cannot be used as evidence")]
    Cancelled,
    #[error("utility process was signalled; output cannot be used as evidence")]
    Signalled { signal: i64 },
    #[error("utility capture or transport reported data loss; output cannot be used as evidence")]
    Incomplete,
    #[error("fetching remote branch failed with exit status {status}: {stderr}")]
    FetchFailed { status: i64, stderr: String },
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Path(#[from] RepoPathError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Execution(#[from] ExecError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtilityBudget {
    timeout: Duration,
    terminate_grace: Duration,
    reap_timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
}

impl UtilityBudget {
    pub fn new(
        timeout: Duration,
        terminate_grace: Duration,
        reap_timeout: Duration,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> Result<Self, UtilityError> {
        if timeout.is_zero()
            || timeout > MAX_UTILITY_TIMEOUT
            || terminate_grace > MAX_CLEANUP_BUDGET
            || reap_timeout > MAX_CLEANUP_BUDGET
            || stdout_limit == 0
            || stderr_limit == 0
            || stdout_limit > MAX_STREAM_LIMIT
            || stderr_limit > MAX_STREAM_LIMIT
            || timeout
                .checked_add(terminate_grace)
                .and_then(|value| value.checked_add(reap_timeout))
                .is_none()
        {
            return Err(UtilityError::InvalidBudget);
        }
        Ok(Self {
            timeout,
            terminate_grace,
            reap_timeout,
            stdout_limit,
            stderr_limit,
        })
    }

    pub fn bounded(timeout: Duration) -> Result<Self, UtilityError> {
        Self::new(
            timeout,
            DEFAULT_TERMINATE_GRACE,
            DEFAULT_REAP_TIMEOUT,
            DEFAULT_STREAM_LIMIT,
            DEFAULT_STREAM_LIMIT,
        )
    }
}

/// Object kinds permitted for raw publication evidence. No caller supplies a
/// cat-file mode, format string or revision expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitObjectKind {
    Commit,
    Blob,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRead {
    RemoteHead {
        remote: String,
    },
    PushPatch {
        base: Option<GitOid>,
        head: GitOid,
    },
    Status,
    StatusIncludingIgnored,
    RevParseHead,
    ResolveRef {
        name: GitRefName,
    },
    /// Read only the ref OID, before quarantined object content is verified.
    ResolveRefOid {
        name: GitRefName,
    },
    ResolveRevision {
        revision: GitRevision,
    },
    SymbolicRevision {
        revision: GitRevision,
    },
    Diff {
        base: GitOid,
        head: GitOid,
        paths: Vec<RepoPath>,
    },
    ChangedPaths {
        base: GitOid,
        head: GitOid,
    },
    /// Staged changes against HEAD with their modes.
    StagedModes,
    /// Changed paths with their old and new modes (`diff --raw`).
    ChangedModes {
        base: GitOid,
        head: GitOid,
    },
    DiffIndex {
        paths: Vec<RepoPath>,
    },
    DiffWorktree {
        paths: Vec<RepoPath>,
    },
    Log {
        base: GitOid,
        head: GitOid,
        limit: u64,
    },
    WorktreeList,
    /// Read repository config, excluding the utility's core.bare override.
    LocalBare,
    BranchTips,
    /// Read remote-tracking refs for branch-name collision checks.
    RemoteBranchTips,
    FirstParentHistory {
        tip: GitOid,
    },
    MergeBase {
        left: GitOid,
        right: GitOid,
    },
    /// Exit(0) means ancestor, Exit(1) means not an ancestor. Other exits are
    /// Git errors; signal termination is rejected by `run_utility`.
    MergeBaseIsAncestor {
        ancestor: GitOid,
        tip: GitOid,
    },
    /// Enumerate every entry, with its raw blob size, in one selected tree.
    AllTreeEntries {
        oid: GitOid,
    },
    RawObject {
        kind: GitObjectKind,
        oid: GitOid,
    },
    /// All selected commits, including merge side branches, excluding base.
    /// The caller separately verifies ancestry and a nonempty result.
    AncestorRange {
        base: GitOid,
        head: GitOid,
    },
    TreeEntries {
        oid: GitOid,
        prefix: RepoPath,
    },
    IndexEntries {
        paths: Vec<RepoPath>,
    },
    IndexBlob {
        path: RepoPath,
    },
    Blob {
        oid: GitOid,
        path: RepoPath,
    },
}

/// A closed mutation set. It is crate-private: callers receive only the
/// narrower policy services that own each operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GitMutation {
    FastForward {
        head: GitOid,
    },
    WorktreeAdd {
        branch: GitRefName,
        base: Option<GitOid>,
        path: PathBuf,
        create_branch: bool,
    },
    WorktreeRemove {
        path: PathBuf,
    },
    SetBranchConfig {
        branch: GitRefName,
        key: &'static str,
        value: String,
    },
    DeleteRef {
        name: GitRefName,
        expected_old_oid: GitOid,
    },
}

/// Pinned absolute utility tools and a verified private home. No environment
/// from the parent process is inherited by a child.
pub struct UtilityRuntime {
    git: ExecutablePin,
    home: PrivateHome,
    /// Lane-writable locations beyond those derived from the repository, such
    /// as the run state directory that holds lane scratch.
    lane_roots: LaneRoots,
}

/// Where a run's lane scratch and provider state live below the state
/// directory: lanes write there, and the user's config never should.
pub fn lane_state_roots(state: &Path) -> Vec<PathBuf> {
    vec![state.join("runs"), state.join("children")]
}

/// Canonical project location and its verified Git origin, when present.
pub struct DiscoveredProject {
    pub location: provenant_config::ProjectLocation,
    pub repository: Option<VerifiedRepo>,
}

impl UtilityRuntime {
    /// Closed Git reads with HOME/TMPDIR pointing at a non-directory. This
    /// requires no state or temporary-directory creation and grants no mutation.
    pub fn read_only(git: impl AsRef<Path>) -> Result<Self, UtilityError> {
        Ok(Self {
            git: ExecutablePin::new(git.as_ref(), "git")?,
            home: PrivateHome {
                path: PathBuf::from("/dev/null"),
                identity: None,
            },
            lane_roots: LaneRoots::default(),
        })
    }
    pub fn new(git: impl AsRef<Path>, home: impl AsRef<Path>) -> Result<Self, UtilityError> {
        Ok(Self {
            git: ExecutablePin::new(git.as_ref(), "git")?,
            home: PrivateHome::new(home.as_ref())?,
            lane_roots: LaneRoots::default(),
        })
    }

    /// The pinned Git locator, for callers that run Git themselves.
    pub fn git_program_path(&self) -> &Path {
        &self.git.locator
    }

    /// Add locations that lanes can write, beyond the worktrees and scratch
    /// that follow from the repository itself.
    #[must_use]
    pub fn with_lane_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.lane_roots = LaneRoots::new(roots);
        self
    }

    /// Everything a lane can write that matters to `repo`: its own linked
    /// worktree, the project's worktree and scratch areas, and any extra roots.
    pub(crate) fn lane_roots(&self, repo: &VerifiedRepo) -> LaneRoots {
        LaneRoots::for_repository(
            repo.root_path(),
            repo.private_git_dir(),
            repo.common_git_dir(),
            &self.lane_roots,
        )
    }

    pub fn open_repo(
        &self,
        executor: &dyn Executor,
        path: impl AsRef<Path>,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<VerifiedRepo, UtilityError> {
        if cancel.is_cancelled() {
            return Err(UtilityError::Cancelled);
        }
        self.revalidate()?;
        VerifiedRepo::open(path, |repo| {
            probe_repository(executor, repo, self, None, budget, cancel)
        })
    }

    /// Probe a linked worktree's own effective configuration. Its branch
    /// decides which `onbranch:` includes apply and its private directory can
    /// hold `config.worktree`, so the primary checkout's probe cannot stand in.
    /// Fails for that worktree when it includes a lane-writable file whose
    /// settings cannot be neutralised.
    pub fn probe_worktree(
        &self,
        executor: &dyn Executor,
        worktree: &mut VerifiedWorktree,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<(), UtilityError> {
        if cancel.is_cancelled() {
            return Err(UtilityError::Cancelled);
        }
        self.revalidate()?;
        let repo = worktree.repo_mut();
        let inherited = repo.probe_data().cloned();
        let data = probe_repository(executor, repo, self, inherited.as_ref(), budget, cancel)?;
        repo.set_probe_data(data)?;
        Ok(())
    }

    /// Read-only evidence supports native object alternates without granting
    /// the protected repository capability required by mutation services.
    pub fn open_read_repo(
        &self,
        executor: &dyn Executor,
        path: impl AsRef<Path>,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<ReadOnlyRepo, UtilityError> {
        if cancel.is_cancelled() {
            return Err(UtilityError::Cancelled);
        }
        self.revalidate()?;
        VerifiedRepo::open_mode(path, true, |repo| {
            probe_repository(executor, repo, self, None, budget, cancel)
        })
        .map(ReadOnlyRepo)
    }

    /// One project-discovery owner for CLI/runtime callers. A Git marker is
    /// verified by open_repo; malformed metadata never falls back to non-Git.
    /// Non-Git workspaces bind their canonical directory, without a Git parser.
    pub fn project_location(
        &self,
        executor: &dyn Executor,
        cwd: &Path,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<DiscoveredProject, UtilityError> {
        let cwd = canonicalize(cwd)?;
        for ancestor in cwd.ancestors() {
            let directory = Directory::open_anchor(ancestor)?;
            match directory.metadata_at(OsStr::new(".git"), false) {
                Ok(_) => {
                    let repo = self.open_repo(executor, ancestor, budget, cancel)?;
                    let location = provenant_config::ProjectLocation::new(repo.common_git_dir())
                        .map_err(|_| UtilityError::InvalidOperationValue)?;
                    return Ok(DiscoveredProject {
                        location,
                        repository: Some(repo),
                    });
                }
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        let location = provenant_config::ProjectLocation::new(cwd)
            .map_err(|_| UtilityError::InvalidOperationValue)?;
        Ok(DiscoveredProject {
            location,
            repository: None,
        })
    }

    fn revalidate(&self) -> Result<(), UtilityError> {
        self.home.verify()?;
        Ok(())
    }
}

/// The Git executable. The pin keeps the caller's stable locator (such as
/// `/opt/homebrew/bin/git`, a symlink that `brew upgrade` repoints) and runs
/// that, so a long-lived process survives an upgrade. The locator's canonical
/// target is validated once, then re-validated only when a cheap `stat`
/// through the locator shows that the target changed, or a spawn reports
/// ENOENT.
struct ExecutablePin {
    locator: PathBuf,
    tool: &'static str,
    state: Mutex<PinState>,
}

struct PinState {
    /// Identity, owner and mode of the canonical target when it was validated.
    target: Metadata,
}

impl ExecutablePin {
    fn new(input: &Path, tool: &'static str) -> Result<Self, UtilityError> {
        let target = Self::validate(input, tool)?;
        Ok(Self {
            locator: input.to_path_buf(),
            tool,
            state: Mutex::new(PinState { target }),
        })
    }

    /// Resolve symlinks and require a regular file named `tool`, owned by the
    /// current user or root, that is not group- or world-writable. Every
    /// ancestor directory of the locator and of the canonical target must be
    /// free of world-write without the sticky bit. A group-writable ancestor
    /// owned by root or the current user is accepted: Intel Homebrew's
    /// `/usr/local/bin` is group-writable by `admin`, and only that owner or
    /// its group can replace the file.
    fn validate(input: &Path, tool: &'static str) -> Result<Metadata, UtilityError> {
        let invalid = || UtilityError::InvalidExecutable { tool };
        // Canonicalisation would silently anchor a relative path at the cwd.
        if !input.is_absolute() {
            return Err(invalid());
        }
        let path = canonicalize(input).map_err(|_| invalid())?;
        if path.file_name().and_then(OsStr::to_str) != Some(tool) {
            return Err(invalid());
        }
        for ancestor in input.ancestors().skip(1).chain(path.ancestors().skip(1)) {
            let directory = Directory::open_anchor(ancestor).map_err(|_| invalid())?;
            let identity = directory.metadata().map_err(|_| invalid())?;
            let sticky = identity.mode & 0o1000 != 0;
            let trusted_owner = identity.uid == effective_uid() || identity.uid == 0;
            if identity.kind != FileKind::Directory
                || (identity.mode & 0o002 != 0 && !sticky)
                || (identity.mode & 0o020 != 0 && !trusted_owner)
            {
                return Err(invalid());
            }
        }
        let parent =
            Directory::open_anchor(path.parent().ok_or_else(invalid)?).map_err(|_| invalid())?;
        let file = parent
            .metadata_at(path.file_name().ok_or_else(invalid)?, false)
            .map_err(|_| invalid())?;
        if !Self::acceptable_file(&file) {
            return Err(invalid());
        }
        Ok(file)
    }

    fn acceptable_file(file: &Metadata) -> bool {
        file.kind == FileKind::Regular
            && file.mode & 0o111 != 0
            && file.mode & 0o022 == 0
            && (file.uid == effective_uid() || file.uid == 0)
    }

    /// Stat through the locator: one `fstatat` that follows the symlink chain.
    fn observe(&self) -> Option<Metadata> {
        let parent = Directory::open_anchor(self.locator.parent()?).ok()?;
        parent.metadata_at(self.locator.file_name()?, true).ok()
    }

    /// The program to execute. When the stat shows the same file, owner and
    /// mode as the validated target, that is all the checking needed. When
    /// anything differs (an upgrade repointed the link, or the file was
    /// replaced or chmodded), validate the new target in full before use.
    fn program(&self) -> Result<PathBuf, UtilityError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| UtilityError::InvalidExecutable { tool: self.tool })?;
        match self.observe() {
            Some(current)
                if current.same_identity(&state.target)
                    && current.uid == state.target.uid
                    && current.mode == state.target.mode => {}
            _ => state.target = Self::validate(&self.locator, self.tool)?,
        }
        Ok(self.locator.clone())
    }

    /// Re-validate unconditionally, after a spawn reported ENOENT.
    fn revalidate(&self) -> Result<(), UtilityError> {
        let target = Self::validate(&self.locator, self.tool)?;
        self.state
            .lock()
            .map_err(|_| UtilityError::InvalidExecutable { tool: self.tool })?
            .target = target;
        Ok(())
    }

    /// Run a Git process built by `options`. If the spawn fails with ENOENT
    /// (for example while a package manager relinks the locator), re-validate
    /// the target and try once more.
    fn execute<'a>(
        &self,
        executor: &dyn Executor,
        options: impl Fn() -> ExecOptions<'a>,
    ) -> Result<ExecOutcome, UtilityError> {
        match executor.execute(options()) {
            Err(ExecError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                self.revalidate()?;
                Ok(executor.execute(options())?)
            }
            result => Ok(result?),
        }
    }
}

struct PrivateHome {
    path: PathBuf,
    identity: Option<Metadata>,
}

impl PrivateHome {
    fn new(input: &Path) -> Result<Self, UtilityError> {
        if !input.is_absolute() {
            return Err(UtilityError::InvalidHome);
        }
        let initial = Directory::open_anchor(input).map_err(|_| UtilityError::InvalidHome)?;
        let before = initial.metadata().map_err(|_| UtilityError::InvalidHome)?;
        let path = canonicalize(input).map_err(|_| UtilityError::InvalidHome)?;
        let anchor = Directory::open_anchor(&path).map_err(|_| UtilityError::InvalidHome)?;
        let identity = anchor.metadata().map_err(|_| UtilityError::InvalidHome)?;
        if identity.kind != FileKind::Directory
            || identity.uid != effective_uid()
            || identity.mode & 0o077 != 0
            || !before.same_identity(&identity)
        {
            return Err(UtilityError::InvalidHome);
        }
        Ok(Self {
            path,
            identity: Some(identity),
        })
    }

    fn verify(&self) -> Result<(), UtilityError> {
        let Some(expected) = self.identity else {
            return Ok(());
        };
        let fresh = Directory::open_anchor(&self.path).map_err(|_| UtilityError::InvalidHome)?;
        let identity = fresh.metadata().map_err(|_| UtilityError::InvalidHome)?;
        if identity.kind != FileKind::Directory
            || identity.uid != effective_uid()
            || identity.mode & 0o077 != 0
            || !expected.same_identity(&identity)
            || expected.uid != identity.uid
        {
            return Err(UtilityError::InvalidHome);
        }
        Ok(())
    }
}

pub fn run_utility(
    executor: &dyn Executor,
    operation: GitRead,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ExecOutcome, UtilityError> {
    execute_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        false,
        |attribute_source| Ok((git_args(operation, repo, attribute_source)?, None)),
    )
}

/// The one hardened way host code runs Git against a remote: no hooks, no
/// fsmonitor, only the transports delivery uses, the host's own credentials
/// and SSH configuration, and a refusal to proceed when the effective
/// configuration or a transport program lives where a lane can write.
struct HostGit<'a> {
    executor: &'a dyn Executor,
    repo: &'a VerifiedRepo,
    runtime: &'a UtilityRuntime,
    env: Vec<(OsString, OsString)>,
    budget: UtilityBudget,
    cancel: &'a CancelToken,
}

impl<'a> HostGit<'a> {
    /// `home` replaces the host's `HOME` so isolated callers and fixtures keep
    /// their own configuration.
    fn new(
        executor: &'a dyn Executor,
        repo: &'a VerifiedRepo,
        runtime: &'a UtilityRuntime,
        home: Option<&Path>,
        budget: UtilityBudget,
        cancel: &'a CancelToken,
    ) -> Result<Self, UtilityError> {
        if cancel.is_cancelled() {
            return Err(UtilityError::Cancelled);
        }
        runtime.revalidate()?;
        repo.revalidate()?;
        verify_repo_binding(repo)?;
        let env = transport::environment(
            std::env::vars_os(),
            home.map(|home| ("HOME".into(), home.as_os_str().to_owned())),
        );
        let host = Self {
            executor,
            repo,
            runtime,
            env,
            budget,
            cancel,
        };
        let roots = runtime.lane_roots(repo);
        transport::check_environment(&host.env, repo.root_path(), &roots)?;
        let listing = host.run(
            &transport::LISTING_ARGS
                .iter()
                .map(OsString::from)
                .collect::<Vec<_>>(),
        )?;
        if listing.termination != Termination::Exit(0) {
            return Err(UtilityError::UnreadableGitConfiguration);
        }
        let entries = transport::parse_listing(&listing.stdout.bytes)?;
        transport::check_listing(&entries, repo.root_path(), &roots)?;
        host.check_include_targets(&roots)?;
        Ok(host)
    }

    /// Git rereads configuration for the command itself, so a lane must not be
    /// able to create an include target after the listing above: refuse any
    /// directive that names a lane area, read or not.
    fn check_include_targets(&self, roots: &LaneRoots) -> Result<(), UtilityError> {
        let directives = self.run(
            &transport::DIRECTIVE_ARGS
                .iter()
                .map(OsString::from)
                .collect::<Vec<_>>(),
        )?;
        if directives.termination != Termination::Exit(0) {
            return Err(UtilityError::UnreadableGitConfiguration);
        }
        let top = transport::parse_listing(&directives.stdout.bytes)?;
        let base = self.repo.root_path();
        let home = self
            .env
            .iter()
            .find(|(name, _)| name == "HOME")
            .map_or(base, |(_, home)| Path::new(home));
        let mut read = |file: &Path| -> Result<Option<Vec<ConfigEntry>>, TransportError> {
            if !file.is_file() {
                return Ok(None);
            }
            let outcome = self
                .run(&transport::file_listing_args(file))
                .map_err(|_| TransportError::Unreadable)?;
            if outcome.termination != Termination::Exit(0) {
                return Err(TransportError::Unreadable);
            }
            transport::parse_listing(&outcome.stdout.bytes).map(Some)
        };
        transport::check_includes(&top, base, home, roots, &mut read)?;
        Ok(())
    }

    fn run(&self, args: &[OsString]) -> Result<ExecOutcome, UtilityError> {
        if self.cancel.is_cancelled() {
            return Err(UtilityError::Cancelled);
        }
        let mut argv = vec![
            self.runtime.git.program()?.into_os_string(),
            "--git-dir".into(),
            self.repo.private_git_dir().as_os_str().to_owned(),
            "--work-tree".into(),
            self.repo.root_path().as_os_str().to_owned(),
        ];
        argv.extend(transport::config_args());
        argv.extend_from_slice(args);
        validate_outcome(self.runtime.git.execute(self.executor, || {
            exec_options(
                &argv,
                self.repo.root_path(),
                self.budget,
                &self.env,
                Some(b""),
                Some(self.cancel),
            )
        })?)
    }
}

/// What the remote advertises for a base such as `origin/main`.
#[derive(Debug)]
pub(crate) enum RemoteBase {
    NotRemote,
    Missing,
    Found(GitOid),
}

/// Host-only remote preparation. Unlike evidence reads, this must retain the
/// host's transport and credential configuration. It never edits fetch config.
/// Fetches the branch's objects and returns the exact advertised commit.
pub(crate) fn fetch_missing_remote_branch(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    home: &Path,
    base: &str,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<RemoteBase, UtilityError> {
    let base = base.strip_prefix("refs/remotes/").unwrap_or(base);
    let Some((remote, branch)) = base.split_once('/') else {
        return Ok(RemoteBase::NotRemote);
    };
    if remote.is_empty() || remote.starts_with('-') {
        return Ok(RemoteBase::NotRemote);
    }
    let host = HostGit::new(executor, repo, runtime, Some(home), budget, cancel)?;
    let run = |args: &[OsString]| host.run(args);
    let remotes = run(&["remote".into()])?;
    if remotes.termination != Termination::Exit(0)
        || !remotes
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .any(|name| name == remote.as_bytes())
    {
        return Ok(RemoteBase::NotRemote);
    }
    // Resolve the remote's symbolic HEAD from the advertisement itself.
    let default_branch;
    let branch = if branch == "HEAD" {
        let head = run(&[
            "ls-remote".into(),
            "--symref".into(),
            "--".into(),
            remote.into(),
            "HEAD".into(),
        ])?;
        if head.termination != Termination::Exit(0) {
            return Err(UtilityError::InvalidOperationValue);
        }
        let text = std::str::from_utf8(&head.stdout.bytes)
            .map_err(|_| UtilityError::InvalidOperationValue)?;
        let mut targets = text.lines().filter_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        });
        default_branch = targets
            .next()
            .ok_or(UtilityError::InvalidOperationValue)?
            .to_owned();
        if targets.next().is_some() {
            return Err(UtilityError::InvalidOperationValue);
        }
        default_branch.as_str()
    } else {
        branch
    };
    // Only literal branch names qualify, never revision expressions or options.
    if GitRefName::branch(branch).is_err() {
        return Ok(RemoteBase::NotRemote);
    }
    let refspec = format!("refs/heads/{branch}");
    // A failed fetch can mean either a missing remote ref or a transport fault.
    let fetched = run(&[
        "fetch".into(),
        // Creation must not indirectly prune stale worktree registrations.
        "--no-auto-maintenance".into(),
        "--no-tags".into(),
        "--no-recurse-submodules".into(),
        "--no-write-fetch-head".into(),
        "--refmap=".into(),
        "--".into(),
        remote.into(),
        refspec.into(),
    ])?;
    let fetch_status = match fetched.termination {
        Termination::Exit(0) => 0,
        Termination::Exit(status) => status,
        Termination::Signal(signal) => return Err(UtilityError::Signalled { signal }),
    };
    let remote_ref = format!("refs/heads/{branch}");
    let advertised = run(&[
        "ls-remote".into(),
        "--exit-code".into(),
        "--heads".into(),
        "--".into(),
        remote.into(),
        remote_ref.clone().into(),
    ]);
    // ls-remote uses status 2 when the remote answered but advertised no
    // matching ref. Let the caller's resolution return BaseNotFound.
    match advertised {
        Ok(outcome) if outcome.termination == Termination::Exit(2) => {
            return Ok(RemoteBase::Missing);
        }
        Err(error @ (UtilityError::TimedOut | UtilityError::Cancelled)) => return Err(error),
        Err(UtilityError::Execution(ExecError::Cancelled)) => return Err(UtilityError::Cancelled),
        Ok(outcome) if fetch_status == 0 && outcome.termination == Termination::Exit(0) => {
            let output = std::str::from_utf8(&outcome.stdout.bytes)
                .map_err(|_| UtilityError::InvalidOperationValue)?;
            let mut exact = output.lines().filter_map(|line| {
                let (oid, name) = line.split_once('\t')?;
                (name == remote_ref).then_some(oid)
            });
            let oid = exact.next().ok_or(UtilityError::InvalidOperationValue)?;
            if exact.next().is_some() {
                return Err(UtilityError::InvalidOperationValue);
            }
            return GitOid::parse(oid)
                .map(RemoteBase::Found)
                .map_err(|_| UtilityError::InvalidOperationValue);
        }
        Ok(outcome) if fetch_status == 0 => {
            let status = match outcome.termination {
                Termination::Exit(status) => status,
                Termination::Signal(signal) => return Err(UtilityError::Signalled { signal }),
            };
            return Err(UtilityError::FetchFailed {
                status,
                stderr: fetch_stderr_excerpt(&outcome.stderr.bytes),
            });
        }
        _ => {}
    }
    Err(UtilityError::FetchFailed {
        status: fetch_status,
        stderr: fetch_stderr_excerpt(&fetched.stderr.bytes),
    })
}

fn fetch_stderr_excerpt(bytes: &[u8]) -> String {
    let safe = crate::redact::known_shapes(bytes)
        .unwrap_or_else(|_| b"fetch stderr could not be sanitised".to_vec());
    let mut stderr: String = String::from_utf8_lossy(&safe)
        .chars()
        .map(|character| {
            if character.is_control() || is_format_character(character) {
                ' '
            } else {
                character
            }
        })
        .collect();
    if stderr.len() > MAX_FETCH_STDERR {
        let mut end = MAX_FETCH_STDERR - '…'.len_utf8();
        while !stderr.is_char_boundary(end) {
            end -= 1;
        }
        stderr.truncate(end);
        stderr.push('…');
    }
    stderr
}

fn is_format_character(character: char) -> bool {
    matches!(
        character as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Run a read for the release scan, where shallow boundaries must not hide
/// commits from the publication history check.
pub fn run_release_read_utility(
    executor: &dyn Executor,
    operation: GitRead,
    repo: &ReadOnlyRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ExecOutcome, UtilityError> {
    execute_git(
        executor,
        &repo.0,
        runtime,
        budget,
        cancel,
        true,
        |attribute_source| Ok((git_args(operation, &repo.0, attribute_source)?, None)),
    )
}

/// Query the remote's advertised HEAD using host transport credentials.
pub(crate) fn advertised_remote_head(
    executor: &dyn Executor,
    repo: &ReadOnlyRepo,
    runtime: &UtilityRuntime,
    remote: &str,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ExecOutcome, UtilityError> {
    if remote.is_empty()
        || remote.len() > 128
        || !remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
        || remote.starts_with('-')
        || remote.contains("..")
    {
        return Err(UtilityError::InvalidOperationValue);
    }
    let host = HostGit::new(executor, &repo.0, runtime, None, budget, cancel)?;
    host.run(&[
        "ls-remote".into(),
        "--symref".into(),
        "--".into(),
        remote.into(),
        "HEAD".into(),
    ])
}

/// Same closed reads and execution boundary, with no mutation capability.
pub fn run_read_utility(
    executor: &dyn Executor,
    operation: GitRead,
    repo: &ReadOnlyRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ExecOutcome, UtilityError> {
    run_utility(executor, operation, &repo.0, runtime, budget, cancel)
}

/// Keep the authority checks, attribute source and bounded execution identical
/// for reads and the separately typed custody mutations.
fn execute_git(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    release_scan: bool,
    command: impl FnOnce(&str) -> Result<(Vec<OsString>, Option<Vec<u8>>), UtilityError>,
) -> Result<ExecOutcome, UtilityError> {
    if cancel.is_cancelled() {
        return Err(UtilityError::Cancelled);
    }
    runtime.home.verify()?;
    repo.revalidate()?;
    verify_repo_binding(repo)?;
    let attribute_source = repo
        .probe_data()
        .ok_or(UtilityError::UnprobedRepository)?
        .attribute_source
        .as_str();
    let (args, stdin) = command(attribute_source)?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(runtime.git.program()?.into_os_string());
    argv.extend(args);
    let mut env = environment(runtime, repo);
    if release_scan {
        env.push(("GIT_SHALLOW_FILE".into(), "/dev/null".into()));
    }
    env.push(("GIT_ATTR_SOURCE".into(), attribute_source.into()));
    validate_outcome(runtime.git.execute(executor, || {
        exec_options(
            &argv,
            repo.root_path(),
            budget,
            &env,
            stdin.as_deref(),
            Some(cancel),
        )
    })?)
}

pub(crate) fn validate_outcome(outcome: ExecOutcome) -> Result<ExecOutcome, UtilityError> {
    if outcome.timed_out {
        return Err(UtilityError::TimedOut);
    }
    if outcome.cancelled {
        return Err(UtilityError::Cancelled);
    }
    if let Termination::Signal(signal) = outcome.termination {
        return Err(UtilityError::Signalled { signal });
    }
    if outcome.drain_truncated
        || outcome.overflow_stopped
        || outcome.stdout.sink_lost_bytes > 0
        || outcome.stderr.sink_lost_bytes > 0
        || outcome.stdout.sink_failed
        || outcome.stderr.sink_failed
        || outcome.stdout.oversized_frames > 0
        || outcome.stderr.oversized_frames > 0
        || outcome.stdout.decoder_lost_frames > 0
        || outcome.stderr.decoder_lost_frames > 0
        || outcome.stdout.read_failed
        || outcome.stderr.read_failed
        || outcome.stdout.filter_failed
        || outcome.stderr.filter_failed
        || outcome.stdout.total_bytes != outcome.stdout.bytes.len() as u64
        || outcome.stderr.total_bytes != outcome.stderr.bytes.len() as u64
    {
        return Err(UtilityError::Incomplete);
    }
    if outcome.stdout.truncated {
        return Err(UtilityError::Truncated { stream: "stdout" });
    }
    if outcome.stderr.truncated {
        return Err(UtilityError::Truncated { stream: "stderr" });
    }
    Ok(outcome)
}

pub(crate) fn exec_options<'a>(
    argv: &'a [OsString],
    cwd: &'a Path,
    budget: UtilityBudget,
    env: &'a [(OsString, OsString)],
    stdin: Option<&'a [u8]>,
    cancel: Option<&'a CancelToken>,
) -> ExecOptions<'a> {
    let mut options = ExecOptions::new(argv, cwd, budget.timeout);
    options.env = Some(env);
    options.stdin = stdin;
    options.stop_on_output_limit = true;
    options.stdout = StreamPolicy::Capture {
        limit: budget.stdout_limit,
    };
    options.stderr = StreamPolicy::Capture {
        limit: budget.stderr_limit,
    };
    options.terminate_grace = budget.terminate_grace;
    options.reap_timeout = budget.reap_timeout;
    options.cancel = cancel;
    options
}

fn git_args(
    operation: GitRead,
    repo: &VerifiedRepo,
    attribute_source: &str,
) -> Result<Vec<OsString>, UtilityError> {
    let mut args = vec![
        OsString::from("--git-dir"),
        repo.private_git_dir().as_os_str().to_owned(),
        OsString::from("--work-tree"),
        repo.root_path().as_os_str().to_owned(),
    ];
    args.extend(GIT_COMMON.iter().map(OsString::from));
    args.push(format!("--attr-source={attribute_source}").into());
    let tail = match operation {
        GitRead::Status => vec![
            "status".into(),
            "--porcelain=v2".into(),
            "-z".into(),
            "--branch".into(),
            "--ignore-submodules=all".into(),
        ],
        GitRead::StatusIncludingIgnored => vec![
            "status".into(),
            "--porcelain=v2".into(),
            "-z".into(),
            "--branch".into(),
            "--ignore-submodules=all".into(),
            "--ignored=matching".into(),
            "--untracked-files=all".into(),
        ],
        GitRead::RevParseHead => vec![
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            "HEAD^{commit}".into(),
        ],
        GitRead::ResolveRefOid { name } => vec![
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            name.as_str().into(),
        ],
        GitRead::ResolveRef { name } => vec![
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            format!("{}^{{commit}}", name.as_str()).into(),
        ],
        GitRead::ResolveRevision { revision } => vec![
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            format!("{}^{{commit}}", revision.as_str()).into(),
        ],
        GitRead::SymbolicRevision { revision } => vec![
            "rev-parse".into(),
            "--symbolic-full-name".into(),
            "--verify".into(),
            "--end-of-options".into(),
            revision.as_str().into(),
        ],
        GitRead::ChangedPaths { base, head } => vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
            "--name-only".into(),
            "-z".into(),
            base.to_string().into(),
            head.to_string().into(),
            "--".into(),
        ],
        GitRead::StagedModes => vec![
            "diff-index".into(),
            "--cached".into(),
            "--no-renames".into(),
            "--raw".into(),
            "--no-abbrev".into(),
            "-z".into(),
            "HEAD".into(),
            "--".into(),
        ],
        GitRead::ChangedModes { base, head } => vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
            "--raw".into(),
            "--no-abbrev".into(),
            "-z".into(),
            base.to_string().into(),
            head.to_string().into(),
            "--".into(),
        ],
        GitRead::Diff { base, head, paths } => {
            let mut args = vec![
                "diff".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--no-renames".into(),
                "--no-color".into(),
                "--binary".into(),
                "--ignore-submodules=all".into(),
                base.to_string().into(),
                head.to_string().into(),
                "--".into(),
            ];
            append_paths(&mut args, &paths);
            args
        }
        GitRead::DiffIndex { paths } => {
            let mut args = vec![
                "diff".into(),
                "--cached".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--no-renames".into(),
                "--no-color".into(),
                "--binary".into(),
                "--ignore-submodules=all".into(),
                "--".into(),
            ];
            append_paths(&mut args, &paths);
            args
        }
        GitRead::DiffWorktree { paths } => {
            let mut args = vec![
                // `git diff` can refresh and rewrite the real index even with
                // optional locks disabled. The plumbing command only reads it.
                "diff-files".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--no-renames".into(),
                "--no-color".into(),
                "--binary".into(),
                "--ignore-submodules=all".into(),
                "--".into(),
            ];
            append_paths(&mut args, &paths);
            args
        }
        GitRead::Log { base, head, limit } => {
            validate_limit(limit)?;
            vec![
                "log".into(),
                "--no-show-signature".into(),
                "--no-decorate".into(),
                "--no-color".into(),
                "--format=%H%x00%an%x00%at%x00%s%x00".into(),
                format!("--max-count={limit}").into(),
                format!("{base}..{head}").into(),
                "--".into(),
            ]
        }
        GitRead::FirstParentHistory { tip } => vec![
            "rev-list".into(),
            "--first-parent".into(),
            tip.to_string().into(),
            "--".into(),
        ],
        GitRead::RemoteHead { remote } => {
            if remote.is_empty()
                || remote.len() > 128
                || !remote
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
                || remote.starts_with('-')
                || remote.contains("..")
            {
                return Err(UtilityError::InvalidOperationValue);
            }
            vec![
                "symbolic-ref".into(),
                "--quiet".into(),
                format!("refs/remotes/{remote}/HEAD").into(),
            ]
        }
        GitRead::PushPatch { base, head } => {
            let mut args = vec![
                "log".into(),
                "--no-show-signature".into(),
                "--format=".into(),
                "-m".into(),
                "-p".into(),
                "-U0".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                head.to_string().into(),
            ];
            if let Some(base) = base {
                args.push(format!("^{base}").into());
            } else {
                args.extend(["--not".into(), "--remotes".into()]);
            }
            args.push("--".into());
            args
        }
        GitRead::WorktreeList => vec![
            "worktree".into(),
            "list".into(),
            "--porcelain".into(),
            "-z".into(),
        ],
        GitRead::LocalBare => [
            "config",
            "--local",
            "--no-includes",
            "--bool",
            "--get",
            "core.bare",
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
        GitRead::BranchTips => vec![
            "for-each-ref".into(),
            "--format=%(refname)%00%(objectname)%00".into(),
            "refs/heads".into(),
        ],
        GitRead::RemoteBranchTips => vec![
            "for-each-ref".into(),
            "--format=%(refname)%00%(objectname)%00".into(),
            "refs/remotes".into(),
        ],
        GitRead::MergeBase { left, right } => {
            vec![
                "merge-base".into(),
                left.to_string().into(),
                right.to_string().into(),
            ]
        }
        GitRead::MergeBaseIsAncestor { ancestor, tip } => vec![
            "merge-base".into(),
            "--is-ancestor".into(),
            ancestor.to_string().into(),
            tip.to_string().into(),
        ],
        GitRead::AllTreeEntries { oid } => vec![
            "ls-tree".into(),
            "-r".into(),
            "-l".into(),
            "-z".into(),
            "--full-tree".into(),
            oid.to_string().into(),
            "--".into(),
        ],
        GitRead::RawObject { kind, oid } => vec![
            "cat-file".into(),
            match kind {
                GitObjectKind::Commit => "commit".into(),
                GitObjectKind::Blob => "blob".into(),
            },
            oid.to_string().into(),
        ],
        GitRead::AncestorRange { base, head } => vec![
            "rev-list".into(),
            format!("{base}..{head}").into(),
            "--".into(),
        ],
        GitRead::TreeEntries { oid, prefix } => {
            let mut args = vec![
                "ls-tree".into(),
                "-r".into(),
                "-z".into(),
                "--full-tree".into(),
                oid.to_string().into(),
                "--".into(),
            ];
            args.push(prefix.as_os_str().to_owned());
            args
        }
        GitRead::IndexEntries { paths } => {
            let mut args = vec![
                "ls-files".into(),
                "--stage".into(),
                "-z".into(),
                "--".into(),
            ];
            append_paths(&mut args, &paths);
            args
        }
        GitRead::IndexBlob { path } => {
            let mut object = OsString::from(":");
            path.append_to(&mut object);
            vec!["cat-file".into(), "blob".into(), object]
        }
        GitRead::Blob { oid, path } => {
            let mut object = OsString::from(oid.to_string());
            object.push(":");
            path.append_to(&mut object);
            vec!["cat-file".into(), "blob".into(), object]
        }
    };
    args.extend(tail);
    Ok(args)
}

fn append_paths(args: &mut Vec<OsString>, paths: &[RepoPath]) {
    args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
}

fn validate_limit(limit: u64) -> Result<(), UtilityError> {
    if (1..=MAX_GIT_LOG).contains(&limit) {
        Ok(())
    } else {
        Err(UtilityError::InvalidOperationValue)
    }
}

pub(crate) fn environment(
    runtime: &UtilityRuntime,
    repo: &VerifiedRepo,
) -> Vec<(OsString, OsString)> {
    let overrides = repo
        .probe_data()
        .map_or(&[][..], |data| data.config_overrides.as_slice());
    environment_with(runtime, repo, overrides)
}

fn environment_with(
    runtime: &UtilityRuntime,
    repo: &VerifiedRepo,
    repo_overrides: &[(String, String)],
) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("HOME".into(), runtime.home.path.as_os_str().to_owned()),
        ("TMPDIR".into(), runtime.home.path.as_os_str().to_owned()),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
        ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
        ("GIT_GRAFT_FILE".into(), "/dev/null".into()),
        ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_PAGER".into(), "cat".into()),
        ("PAGER".into(), "cat".into()),
        ("GIT_NO_LAZY_FETCH".into(), "1".into()),
        (
            "GIT_DIR".into(),
            repo.private_git_dir().as_os_str().to_owned(),
        ),
        (
            "GIT_WORK_TREE".into(),
            repo.root_path().as_os_str().to_owned(),
        ),
        (
            "GIT_COMMON_DIR".into(),
            repo.common_git_dir().as_os_str().to_owned(),
        ),
    ];
    let overrides = GIT_CONFIG_OVERRIDES
        .iter()
        .map(|(key, value)| (*key, *value))
        .chain(
            repo_overrides
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
    let mut count = 0;
    for (index, (key, value)) in overrides.enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{index}").into(), key.into()));
        env.push((format!("GIT_CONFIG_VALUE_{index}").into(), value.into()));
        count += 1;
    }
    env.push(("GIT_CONFIG_COUNT".into(), count.to_string().into()));
    env
}

fn verify_repo_binding(repo: &VerifiedRepo) -> Result<(), UtilityError> {
    // Reopening binds the held root and Git database to this Executor cwd.
    let root = Directory::open_anchor(repo.root_path())?;
    let private = Directory::open_anchor(repo.private_git_dir())?;
    let common = Directory::open_anchor(repo.common_git_dir())?;
    if !root.metadata()?.same_identity(&repo.root_identity())
        || !private.metadata()?.same_identity(&repo.private_identity())
        || !common.metadata()?.same_identity(&repo.common_identity())
    {
        return Err(RepoError::IdentityChanged.into());
    }
    Ok(())
}

pub(crate) fn execute_git_mutation(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    mutation: GitMutation,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ExecOutcome, UtilityError> {
    if runtime.home.identity.is_none() {
        return Err(UtilityError::InvalidHome);
    }
    execute_git(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        false,
        |attribute_source| {
            let (mut args, stdin) = mutation_args(mutation)?;
            args.insert(0, format!("--attr-source={attribute_source}").into());
            Ok((args, stdin))
        },
    )
}

/// Set one lane upstream key. Reopen the repository after each call because
/// Git replaces the common config file.
pub fn set_lane_upstream_key(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    branch: &GitRefName,
    merge: bool,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<(), UtilityError> {
    if branch.short_name().is_empty() {
        return Err(UtilityError::InvalidOperationValue);
    }
    let (key, value) = if merge {
        ("merge", branch.as_str().to_owned())
    } else {
        ("remote", "origin".to_owned())
    };
    let result = execute_git_mutation(
        executor,
        repo,
        runtime,
        GitMutation::SetBranchConfig {
            branch: branch.clone(),
            key,
            value,
        },
        budget,
        cancel,
    )?;
    if result.termination != Termination::Exit(0) {
        return Err(UtilityError::InvalidOperationValue);
    }
    Ok(())
}

/// What the effective configuration of one repository or worktree contains
/// that lanes can influence, and the overrides that neutralise it.
#[derive(PartialEq, Eq)]
struct ConfigScan {
    entries: Vec<ConfigEntry>,
    overrides: Vec<(String, String)>,
    files: Vec<PathBuf>,
    head_sensitive: bool,
}

/// Keys that cannot affect a utility command. Its argv is fixed, with
/// `--no-textconv`, `--no-ext-diff`, no pager, no merge, no network and no
/// signing, so these settings (transport programs and credentials, editors,
/// merge and diff tools, identity and display) are inert there. Transport
/// operations apply their own, stricter refusal in `HostGit`.
fn is_inert_for_utility(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "user.",
        "color.",
        "advice.",
        "alias.",
        "pager.",
        "credential.",
        "http.",
        "url.",
        "gpg.",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        || matches!(
            lower.as_str(),
            "core.sshcommand"
                | "core.gitproxy"
                | "core.askpass"
                | "core.pager"
                | "core.editor"
                | "sequence.editor"
                | "uploadpack.packobjectshook"
        )
        || (lower.starts_with("remote.")
            && (lower.ends_with(".uploadpack") || lower.ends_with(".receivepack")))
        || (lower.starts_with("diff.")
            && (lower.ends_with(".textconv") || lower.ends_with(".command")))
        || ["merge.", "mergetool.", "difftool."]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

/// Enumerate the configuration Git would read (includes followed, `onbranch:`
/// and `gitdir:` conditions evaluated for this repository's HEAD), find every
/// file lanes can write, and neutralise what such a file sets:
///
/// - every configured filter driver is disabled, whatever its origin, because
///   attributes can select one;
/// - a key set by a lane-writable file is pinned to the value the rest of the
///   configuration gives it, left alone when no utility command can reach it,
///   and otherwise cannot be neutralised, which fails the operation.
///
/// A lane-writable include target that Git did not read for this HEAD (another
/// branch's `onbranch:` condition) is not scanned; instead the handle is tied
/// to HEAD, so a branch change makes it stale and the next probe sees the file.
fn scan_configuration(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ConfigScan, UtilityError> {
    let listing_args = transport::LISTING_ARGS.map(OsString::from);
    let outcome = run_git_probe(executor, repo, runtime, budget, cancel, &[], &listing_args)?;
    if outcome.termination != Termination::Exit(0) {
        return Err(UtilityError::UnreadableGitConfiguration);
    }
    let entries = transport::parse_listing(&outcome.stdout.bytes)
        .map_err(|_| UtilityError::UnreadableGitConfiguration)?;
    let base = repo.root_path();
    let home = runtime.home.path.as_path();
    let roots = runtime.lane_roots(repo);
    let own: Vec<PathBuf> = [repo.private_git_dir(), repo.common_git_dir()]
        .iter()
        .flat_map(|dir| ["config", "config.worktree"].map(|name| dir.join(name)))
        .collect();

    // Files read through includes, and every declared target, present or not.
    let mut files = BTreeSet::new();
    let mut read = BTreeSet::new();
    let mut declared = Vec::new();
    for entry in &entries {
        let file = entry.file(base);
        if let Some(file) = &file
            && !own.contains(file)
            && file != Path::new("/dev/null")
        {
            files.insert(file.clone());
            read.insert(file.clone());
        }
        if let (Some(file), Some(value)) = (&file, &entry.value)
            && transport::is_include_directive(&entry.key)
        {
            let target = transport::include_target(file, value, home);
            files.insert(target.clone());
            declared.push((file.clone(), target));
        }
    }
    // A read file is lane-writable when it lies in a lane area, or when a
    // lane-writable file included it.
    let mut lane_files: BTreeSet<PathBuf> = read
        .iter()
        .filter(|file| roots.contains(file))
        .cloned()
        .collect();
    loop {
        let before = lane_files.len();
        for (from, target) in &declared {
            if lane_files.contains(from) && read.contains(target) {
                lane_files.insert(target.clone());
            }
        }
        if lane_files.len() == before {
            break;
        }
    }
    // Declared lane-writable targets that Git did not read for this HEAD can
    // still apply to a worktree on another branch, so read them directly.
    // What they set is neutralised where possible; what cannot be is left to
    // the probe of the worktree that matches.
    let mut pending: Vec<PathBuf> = declared
        .iter()
        .filter(|(from, target)| {
            !read.contains(target) && (roots.contains(target) || lane_files.contains(from))
        })
        .map(|(_, target)| target.clone())
        .collect();
    let head_sensitive = !pending.is_empty();
    let mut unread: Vec<ConfigEntry> = Vec::new();
    let mut visited = BTreeSet::new();
    while let Some(target) = pending.pop() {
        if !visited.insert(target.clone()) {
            continue;
        }
        if visited.len() > MAX_LANE_CONFIG_FILES {
            return Err(UtilityError::LaneConfiguration {
                file: target.display().to_string(),
                key: "include.path".into(),
            });
        }
        let Some(entries) = read_config_file(executor, repo, runtime, budget, cancel, &target)?
        else {
            continue;
        };
        for entry in &entries {
            if let (Some(value), true) = (&entry.value, transport::is_include_directive(&entry.key))
            {
                let nested = transport::include_target(&target, value, home);
                files.insert(nested.clone());
                pending.push(nested);
            }
        }
        unread.extend(entries);
    }

    let mut overrides = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let lane_entries = entries
        .iter()
        .filter_map(|entry| {
            let file = entry.file(base).filter(|file| lane_files.contains(file))?;
            Some((entry.key.as_str(), Some(file)))
        })
        .chain(unread.iter().map(|entry| (entry.key.as_str(), None)));
    for (key, file) in lane_entries {
        if !seen.insert(key.to_owned()) || transport::is_include_directive(key) {
            continue;
        }
        if key.starts_with("filter.")
            || GIT_CONFIG_OVERRIDES
                .iter()
                .any(|(forced, _)| forced.eq_ignore_ascii_case(key))
            || is_inert_for_utility(key)
        {
            continue;
        }
        let good: Vec<&str> = entries
            .iter()
            .filter(|other| {
                other.key == key
                    && other
                        .file(base)
                        .is_none_or(|file| !lane_files.contains(&file))
            })
            .filter_map(|other| other.value.as_deref())
            .collect();
        match (good.as_slice(), file) {
            ([value], _) => {
                overrides.insert(key.to_owned(), (*value).to_owned());
            }
            // Not active for this HEAD: the matching worktree's probe decides.
            (_, None) => {}
            (_, Some(file)) => {
                return Err(UtilityError::LaneConfiguration {
                    file: file.display().to_string(),
                    key: key.to_owned(),
                });
            }
        }
    }

    // Disable every filter driver. Section and variable names are lowercase;
    // the driver name is not.
    let drivers: BTreeSet<&str> = entries
        .iter()
        .chain(&unread)
        .filter_map(|entry| entry.key.strip_prefix("filter.")?.rsplit_once('.'))
        .map(|(driver, _)| driver)
        .collect();
    for driver in drivers {
        for (variable, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            overrides.insert(format!("filter.{driver}.{variable}"), value.to_owned());
        }
    }
    Ok(ConfigScan {
        entries,
        overrides: overrides.into_iter().collect(),
        files: files.into_iter().collect(),
        head_sensitive,
    })
}

/// Read one config file directly, without following its includes. `None` when
/// it does not exist.
fn read_config_file(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    path: &Path,
) -> Result<Option<Vec<ConfigEntry>>, UtilityError> {
    if !path.is_file() {
        return Ok(None);
    }
    let args = transport::file_listing_args(path);
    let outcome = run_git_probe(executor, repo, runtime, budget, cancel, &[], &args)?;
    if outcome.termination != Termination::Exit(0) {
        return Err(UtilityError::UnreadableGitConfiguration);
    }
    transport::parse_listing(&outcome.stdout.bytes)
        .map(Some)
        .map_err(|_| UtilityError::UnreadableGitConfiguration)
}

/// Probe one repository or worktree: neutralise lane-influenced configuration,
/// resolve its object format and prove the attribute-source option works.
/// `inherited` is the primary checkout's probe, whose object format and
/// attribute source a linked worktree shares.
fn probe_repository(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    inherited: Option<&RepoProbeData>,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<RepoProbeData, UtilityError> {
    verify_repo_binding(repo)?;
    let scan = scan_configuration(executor, repo, runtime, budget, cancel)?;
    let files_digest = repo.included_digest(&scan.files, scan.head_sensitive)?;
    let attribute_source = match inherited {
        Some(data) => data.attribute_source.clone(),
        None => empty_attribute_source(executor, repo, runtime, budget, cancel, &scan.overrides)?,
    };
    let object_format = match attribute_source.len() {
        40 => RepoObjectFormat::Sha1,
        64 => RepoObjectFormat::Sha256,
        _ => return Err(UtilityError::AttributeSourceUnsupported),
    };
    // A lane could change an included file between Git reading it and the
    // digest above. Read again: the same view and the same bytes mean the
    // overrides describe what later commands will see, and any later edit
    // makes the handle stale.
    if !scan.files.is_empty() {
        let again = scan_configuration(executor, repo, runtime, budget, cancel)?;
        if again != scan || repo.included_digest(&scan.files, scan.head_sensitive)? != files_digest
        {
            return Err(RepoError::IdentityChanged.into());
        }
    }
    Ok(RepoProbeData {
        object_format,
        attribute_source,
        config_overrides: scan.overrides,
        config_files: scan.files,
        config_files_digest: files_digest,
        config_head_sensitive: scan.head_sensitive,
    })
}

/// Resolve the repository object format once without writing an object, then
/// preflight the global attribute-source option on a submodule-safe status
/// read. The canonical empty tree need not exist in the object database.
fn empty_attribute_source(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    overrides: &[(String, String)],
) -> Result<String, UtilityError> {
    let format_args = [
        OsString::from("rev-parse"),
        OsString::from("--show-object-format"),
    ];
    let format_outcome = run_git_probe(executor, repo, runtime, budget, cancel, &[], &format_args)?;
    if format_outcome.termination != Termination::Exit(0) {
        return Err(UtilityError::AttributeSourceUnsupported);
    }
    let format = std::str::from_utf8(&format_outcome.stdout.bytes)
        .map_err(|_| UtilityError::AttributeSourceUnsupported)?
        .trim();
    let source = match format {
        "sha1" => EMPTY_TREE_SHA1,
        "sha256" => EMPTY_TREE_SHA256,
        _ => return Err(UtilityError::AttributeSourceUnsupported),
    };
    let source_option: OsString = format!("--attr-source={source}").into();
    let preflight_args = [
        source_option,
        OsString::from("status"),
        OsString::from("--porcelain=v2"),
        OsString::from("-z"),
        OsString::from("--ignore-submodules=all"),
        OsString::from("--untracked-files=no"),
    ];
    let preflight = run_git_probe(
        executor,
        repo,
        runtime,
        budget,
        cancel,
        overrides,
        &preflight_args,
    )?;
    if preflight.termination != Termination::Exit(0) {
        return Err(UtilityError::AttributeSourceUnsupported);
    }
    Ok(source.to_owned())
}

fn run_git_probe(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
    overrides: &[(String, String)],
    command: &[OsString],
) -> Result<ExecOutcome, UtilityError> {
    let mut argv = vec![
        runtime.git.program()?.into_os_string(),
        OsString::from("--git-dir"),
        repo.private_git_dir().as_os_str().to_owned(),
        OsString::from("--work-tree"),
        repo.root_path().as_os_str().to_owned(),
    ];
    argv.extend(GIT_COMMON.iter().map(OsString::from));
    argv.extend(command.iter().cloned());
    let env = environment_with(runtime, repo, overrides);
    validate_outcome(runtime.git.execute(executor, || {
        exec_options(&argv, repo.root_path(), budget, &env, None, Some(cancel))
    })?)
}

fn mutation_args(mutation: GitMutation) -> Result<(Vec<OsString>, Option<Vec<u8>>), UtilityError> {
    let mut args = GIT_COMMON.iter().map(OsString::from).collect::<Vec<_>>();
    let input = match mutation {
        GitMutation::FastForward { head } => {
            args.extend([
                "merge".into(),
                "--ff-only".into(),
                "--no-edit".into(),
                "--no-stat".into(),
                "--no-overwrite-ignore".into(),
                head.to_string().into(),
            ]);
            None
        }
        GitMutation::WorktreeAdd {
            branch,
            base,
            path,
            create_branch,
        } => {
            let path = validate_absolute_mutation_path(&path)?;
            args.extend(["worktree".into(), "add".into()]);
            if create_branch {
                args.push("-b".into());
                args.push(branch.short_name().into());
            }
            args.push("--".into());
            args.push(path.into_os_string());
            if let Some(base) = base {
                args.push(base.to_string().into());
            } else if !create_branch {
                args.push(branch.short_name().into());
            }
            None
        }
        GitMutation::WorktreeRemove { path } => {
            let path = validate_absolute_mutation_path(&path)?;
            args.extend(["worktree".into(), "remove".into(), "--".into()]);
            args.push(path.into_os_string());
            None
        }
        GitMutation::SetBranchConfig { branch, key, value } => {
            args.extend([
                "config".into(),
                "--local".into(),
                "--replace-all".into(),
                format!("branch.{}.{key}", branch.short_name()).into(),
                value.into(),
            ]);
            None
        }
        GitMutation::DeleteRef {
            name,
            expected_old_oid,
        } => {
            args.extend([
                "update-ref".into(),
                "--no-deref".into(),
                "-d".into(),
                name.as_str().into(),
                expected_old_oid.to_string().into(),
            ]);
            None
        }
    };
    Ok((args, input))
}

fn validate_absolute_mutation_path(path: &Path) -> Result<PathBuf, UtilityError> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
    {
        return Err(UtilityError::InvalidOperationValue);
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::repo::{VerifiedWorktree, verify_worktree};
    use crate::test_support::{Fixture, add_poisoned_gitlink, assert_raw_git_runs_poison_filter};
    use provenant_os::exec::{ExecError, ExecOptions, Executor, SystemExecutor};
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn executable(path: &Path, mode: u32) -> Result<(), Box<dyn Error>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, b"executable")?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    /// Point `link` at `target` atomically, as a package manager relinks.
    fn relink(link: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
        let staged = link.with_extension("new");
        std::os::unix::fs::symlink(target, &staged)?;
        fs::rename(staged, link)?;
        Ok(())
    }

    #[test]
    fn executable_pin_keeps_the_locator_and_follows_a_swapped_target() -> Result<(), Box<dyn Error>>
    {
        let temp = tempfile::tempdir()?;
        let (first, second) = (temp.path().join("v1/git"), temp.path().join("v2/git"));
        executable(&first, 0o755)?;
        executable(&second, 0o755)?;
        let link = temp.path().join("bin/git");
        fs::create_dir(temp.path().join("bin"))?;
        std::os::unix::fs::symlink(&first, &link)?;

        // The locator is what runs; it is never replaced by the canonical path.
        let pin = ExecutablePin::new(&link, "git")?;
        assert_eq!(pin.program()?, link);

        // `brew upgrade`: the link moves to a new version and the old one is
        // removed. The pin follows without the old target existing.
        relink(&link, &second)?;
        fs::remove_dir_all(temp.path().join("v1"))?;
        assert_eq!(pin.program()?, link);
        let pinned = pin.state.lock().map_err(|_| "pin poisoned")?.target;
        assert!(
            pinned.same_identity(
                &Directory::open_anchor(&temp.path().join("v2"))?
                    .metadata_at(OsStr::new("git"), false)?
            )
        );

        // Removing the target leaves nothing to run.
        fs::remove_file(&second)?;
        assert!(matches!(
            pin.program(),
            Err(UtilityError::InvalidExecutable { tool: "git" })
        ));
        Ok(())
    }

    #[test]
    fn executable_pin_revalidates_a_replaced_or_loosened_target() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target/git");
        executable(&target, 0o755)?;
        let link = temp.path().join("git");
        std::os::unix::fs::symlink(&target, &link)?;
        let pin = ExecutablePin::new(&link, "git")?;
        pin.program()?;

        // A sound replacement is accepted.
        let replacement = temp.path().join("target/replacement");
        executable(&replacement, 0o755)?;
        fs::rename(&replacement, &target)?;
        pin.program()?;

        // A world-writable replacement is not.
        executable(&replacement, 0o757)?;
        fs::rename(&replacement, &target)?;
        assert!(matches!(
            pin.program(),
            Err(UtilityError::InvalidExecutable { tool: "git" })
        ));

        // Nor is a chmod in place to group-write.
        executable(&replacement, 0o755)?;
        fs::rename(&replacement, &target)?;
        pin.program()?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o775))?;
        assert!(matches!(
            pin.program(),
            Err(UtilityError::InvalidExecutable { tool: "git" })
        ));
        Ok(())
    }

    #[test]
    fn executable_pin_retries_one_spawn_after_enoent_when_the_target_is_sound()
    -> Result<(), Box<dyn Error>> {
        struct VanishesOnce(std::sync::atomic::AtomicUsize);
        impl Executor for VanishesOnce {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    return Err(ExecError::Io {
                        operation: "spawn",
                        source: std::io::ErrorKind::NotFound.into(),
                    });
                }
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: Default::default(),
                    stderr: Default::default(),
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 1,
                })
            }
        }
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target/git");
        executable(&target, 0o755)?;
        let link = temp.path().join("git");
        std::os::unix::fs::symlink(&target, &link)?;
        let pin = ExecutablePin::new(&link, "git")?;
        let argv = [link.as_os_str().to_owned()];
        let options = || ExecOptions::new(&argv, temp.path(), Duration::from_secs(1));

        let executor = VanishesOnce(Default::default());
        pin.execute(&executor, options)?;
        assert_eq!(executor.0.load(std::sync::atomic::Ordering::SeqCst), 2);

        // With the target gone, revalidation fails and the spawn is not retried.
        let executor = VanishesOnce(Default::default());
        fs::remove_file(&target)?;
        assert!(matches!(
            pin.execute(&executor, options),
            Err(UtilityError::InvalidExecutable { tool: "git" })
        ));
        assert_eq!(executor.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn executable_pin_rejects_symlink_to_other_name() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("other");
        fs::write(&target, b"executable")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
        let link = temp.path().join("git");
        std::os::unix::fs::symlink(target, &link)?;
        assert!(matches!(
            ExecutablePin::new(&link, "git"),
            Err(UtilityError::InvalidExecutable { tool: "git" })
        ));
        Ok(())
    }

    #[test]
    fn executable_pin_rejects_relative_and_dangling_paths() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let link = temp.path().join("git");
        std::os::unix::fs::symlink(temp.path().join("missing/git"), &link)?;
        for input in [Path::new("git"), link.as_path()] {
            assert!(matches!(
                ExecutablePin::new(input, "git"),
                Err(UtilityError::InvalidExecutable { tool: "git" })
            ));
        }
        Ok(())
    }

    #[test]
    fn executable_pin_refuses_a_writable_file_and_a_loose_ancestor() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let target_dir = temp.path().join("target");
        let target = target_dir.join("git");
        executable(&target, 0o755)?;
        let link = temp.path().join("git");
        std::os::unix::fs::symlink(&target, &link)?;
        let pin = |file: u32, directory: u32| -> Result<ExecutablePin, UtilityError> {
            fs::set_permissions(&target, fs::Permissions::from_mode(file))
                .and_then(|()| {
                    fs::set_permissions(&target_dir, fs::Permissions::from_mode(directory))
                })
                .map_err(|_| UtilityError::InvalidExecutable { tool: "git" })?;
            ExecutablePin::new(&link, "git")
        };
        // Group- or world-write on the file itself lets that group replace it.
        for file in [0o775, 0o757, 0o777, 0o770] {
            assert!(
                matches!(
                    pin(file, 0o755),
                    Err(UtilityError::InvalidExecutable { tool: "git" })
                ),
                "file mode {file:o}"
            );
        }
        // A world-writable directory without the sticky bit lets anyone swap
        // the file; with it, only the owner can.
        assert!(matches!(
            pin(0o755, 0o777),
            Err(UtilityError::InvalidExecutable { tool: "git" })
        ));
        pin(0o755, 0o1777)?;
        // A group-writable directory owned by the current user (or root) is
        // accepted: Intel Homebrew's /usr/local/bin is group-writable by admin.
        pin(0o755, 0o775)?;
        // The rule covers every ancestor, not only the parent.
        let nested = target_dir.join("nested/git");
        executable(&nested, 0o755)?;
        let nested_link = temp.path().join("nested-git");
        std::os::unix::fs::symlink(&nested, &nested_link)?;
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o777))?;
        assert!(matches!(
            ExecutablePin::new(&nested_link, "git"),
            Err(UtilityError::InvalidExecutable { tool: "git" })
        ));
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    struct CredentialEchoFetch<'a>(&'a SystemExecutor);

    impl Executor for CredentialEchoFetch<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let is_fetch = options.argv.iter().any(|arg| arg == "fetch");
            let mut outcome = self.0.execute(options)?;
            if is_fetch && outcome.termination != Termination::Exit(0) {
                outcome.stderr.bytes =
                    "fatal: https://user:secret@example.invalid/repo denied\u{202e}\n"
                        .as_bytes()
                        .to_vec();
                outcome.stderr.total_bytes = outcome.stderr.bytes.len() as u64;
            }
            Ok(outcome)
        }
    }

    #[derive(Clone, Copy)]
    enum ProbeBehavior {
        Exit(i64),
        Cancelled,
        TimedOut,
    }

    struct ControlledFetch<'a> {
        executor: &'a SystemExecutor,
        fetch_termination: Termination,
        probe_behavior: ProbeBehavior,
    }

    impl Executor for ControlledFetch<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let is_fetch = options.argv.iter().any(|arg| arg == "fetch");
            let is_probe = options.argv.iter().any(|arg| arg == "ls-remote");
            if is_probe && matches!(self.probe_behavior, ProbeBehavior::Cancelled) {
                return Err(ExecError::Cancelled);
            }
            if is_probe {
                return Ok(ExecOutcome {
                    termination: match self.probe_behavior {
                        ProbeBehavior::Exit(status) => Termination::Exit(status),
                        ProbeBehavior::Cancelled => unreachable!(),
                        ProbeBehavior::TimedOut => Termination::Exit(0),
                    },
                    stdout: Default::default(),
                    stderr: Default::default(),
                    timed_out: matches!(self.probe_behavior, ProbeBehavior::TimedOut),
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                });
            }
            let mut outcome = self.executor.execute(options)?;
            if is_fetch {
                outcome.termination = self.fetch_termination;
                if self.fetch_termination != Termination::Exit(0) {
                    outcome.stderr.bytes = b"fetch failed".to_vec();
                    outcome.stderr.total_bytes = outcome.stderr.bytes.len() as u64;
                }
            }
            Ok(outcome)
        }
    }

    fn add_missing_test_remote(fixture: &mut Fixture) -> Result<(), Box<dyn Error>> {
        let missing_remote = fixture._temp.path().join("missing-remote.git");
        let url = format!("file://{}", missing_remote.display());
        fixture.run(&["remote", "add", "origin", &url])?;
        fixture.repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        Ok(())
    }

    type Call = (Vec<OsString>, Vec<(OsString, OsString)>);

    /// Records the argv and environment of every Git process it forwards.
    struct RecordsGit<'a> {
        executor: &'a SystemExecutor,
        calls: std::sync::Mutex<Vec<Call>>,
    }

    impl Executor for RecordsGit<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.calls.lock().map_err(|_| ExecError::Busy)?.push((
                options.argv.to_vec(),
                options.env.unwrap_or_default().to_vec(),
            ));
            self.executor.execute(options)
        }
    }

    #[test]
    fn host_transport_is_hardened_and_refuses_lane_writable_programs() -> Result<(), Box<dyn Error>>
    {
        let mut fixture = Fixture::new()?;
        let root = fixture.root.to_str().ok_or("non-UTF8 root")?.to_owned();
        fixture.run(&["remote", "add", "origin", &root])?;
        // A helper where lanes write, and one the user keeps elsewhere.
        let script = |path: &Path| -> Result<String, Box<dyn Error>> {
            executable(path, 0o755)?;
            Ok(path.to_str().ok_or("non-UTF8 helper")?.to_owned())
        };
        let lane = script(&fixture.root.join(".worktrees/lane-1/helper"))?;
        let user = script(&fixture._temp.path().join("bin/helper"))?;
        let budget = fixture.budget()?;
        let cancel = CancelToken::new();
        let recorder = RecordsGit {
            executor: &fixture.executor,
            calls: Default::default(),
        };
        let advertise = |fixture: &Fixture| -> Result<ExecOutcome, UtilityError> {
            let repo = fixture
                .runtime
                .open_read_repo(&recorder, &fixture.root, budget, &cancel)?;
            advertised_remote_head(
                &recorder,
                &repo,
                &fixture.runtime,
                "origin",
                budget,
                &cancel,
            )
        };
        let fetch = |fixture: &Fixture| -> Result<RemoteBase, UtilityError> {
            let repo = fixture
                .runtime
                .open_repo(&recorder, &fixture.root, budget, &cancel)?;
            fetch_missing_remote_branch(
                &recorder,
                &repo,
                &fixture.runtime,
                &fixture.home,
                "origin/main",
                budget,
                &cancel,
            )
        };

        // Baseline: the advertisement works, and every network-facing process
        // carries the same hardening and none of the ambient overrides.
        let outcome = advertise(&fixture)?;
        assert_eq!(outcome.termination, Termination::Exit(0));
        let calls = recorder.calls.lock().map_err(|_| "calls poisoned")?;
        let (argv, env) = calls
            .iter()
            .rev()
            .find(|(argv, _)| argv.iter().any(|arg| arg == "ls-remote"))
            .ok_or("no ls-remote")?;
        for (key, value) in transport::CONFIG {
            let expected = OsString::from(format!("{key}={value}"));
            assert!(argv.contains(&expected), "{key}");
        }
        assert!(env.contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())));
        drop(calls);

        // The same programs, named by the user's config, are refused where
        // lanes can write and accepted anywhere else.
        for (key, value) in [
            ("core.sshCommand", format!("{lane} -o BatchMode=yes")),
            ("credential.helper", format!("!{lane}")),
            ("credential.https://example.invalid.helper", lane.clone()),
            ("remote.origin.uploadpack", lane.clone()),
        ] {
            fixture.run(&["config", key, &value])?;
            for result in [advertise(&fixture).map(|_| ()), fetch(&fixture).map(|_| ())] {
                assert!(
                    matches!(
                        &result,
                        Err(UtilityError::Transport(TransportError::LaneProgram { key: named, .. }))
                            if named.eq_ignore_ascii_case(key)
                    ),
                    "{key}: {result:?}"
                );
            }
            fixture.run(&["config", "--unset", key])?;
        }
        for (key, value) in [
            ("core.sshCommand", format!("{user} -o BatchMode=yes")),
            ("credential.helper", format!("!{user}")),
            ("remote.origin.uploadpack", "git-upload-pack".to_owned()),
        ] {
            fixture.run(&["config", key, &value])?;
            assert_eq!(advertise(&fixture)?.termination, Termination::Exit(0));
            fixture.run(&["config", "--unset", key])?;
        }

        // An include directive that names a lane area is refused whether or
        // not Git reads its target today: a lane could create the file, or
        // make an `includeIf` condition true, before Git rereads the
        // configuration for the command itself. Relative targets resolve
        // against the including file.
        let not_yet = script_path(&fixture.root.join(".worktrees/lane-1/not-yet.cfg"))?;
        let shared = fixture._temp.path().join("shared.cfg");
        for (key, value) in [
            ("include.path", not_yet.as_str()),
            ("include.path", "../.worktrees/lane-1/not-yet.cfg"),
            (
                "includeIf.onbranch:never-checked-out.path",
                not_yet.as_str(),
            ),
            (
                "includeIf.gitdir:/nowhere/.path",
                "../.worktrees/lane-1/not-yet.cfg",
            ),
            (
                "includeIf.hasconfig:remote.*.url:never://*.path",
                not_yet.as_str(),
            ),
        ] {
            fixture.run(&["config", key, value])?;
            for result in [advertise(&fixture).map(|_| ()), fetch(&fixture).map(|_| ())] {
                assert!(
                    matches!(
                        &result,
                        Err(UtilityError::Transport(TransportError::LaneConfig { .. }))
                    ),
                    "{key}={value}: {result:?}"
                );
            }
            fixture.run(&["config", "--unset", key])?;
        }
        // The same through a file the user keeps elsewhere, which Git does read.
        fs::write(&shared, format!("[include]\n\tpath = {not_yet}\n"))?;
        fixture.run(&["config", "include.path", &script_path(&shared)?])?;
        assert!(matches!(
            advertise(&fixture),
            Err(UtilityError::Transport(TransportError::LaneConfig { .. }))
        ));
        fs::write(&shared, "[user]\n\tname = Someone\n")?;
        assert_eq!(advertise(&fixture)?.termination, Termination::Exit(0));
        fixture.run(&["config", "--unset", "include.path"])?;

        // Configuration read from a lane-writable file is refused outright.
        let included = fixture.root.join(".worktrees/lane-1/extra.cfg");
        fs::write(&included, "[user]\n\tname = Lane\n")?;
        fixture.run(&["config", "include.path", &script_path(&included)?])?;
        assert!(matches!(
            advertise(&fixture),
            Err(UtilityError::Transport(TransportError::LaneConfig { .. }))
        ));
        fixture.run(&["config", "--unset", "include.path"])?;
        fixture.root = fs::canonicalize(&fixture.root)?;
        fixture.repo = open(&fixture)?;
        assert_eq!(advertise(&fixture)?.termination, Termination::Exit(0));
        Ok(())
    }

    fn script_path(path: &Path) -> Result<String, Box<dyn Error>> {
        Ok(path.to_str().ok_or("non-UTF8 path")?.to_owned())
    }

    #[test]
    fn missing_remote_fetch_reports_bounded_sanitised_stderr() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        add_missing_test_remote(&mut fixture)?;

        let error = fetch_missing_remote_branch(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            &fixture.home,
            "origin/main",
            fixture.budget()?,
            &CancelToken::new(),
        )
        .expect_err("fetch from a missing remote must fail distinctly");
        let UtilityError::FetchFailed { status, stderr } = error else {
            return Err(format!("unexpected error: {error}").into());
        };
        assert_ne!(status, 0);
        assert!(stderr.len() <= MAX_FETCH_STDERR);

        fixture.run(&[
            "remote",
            "set-url",
            "origin",
            "http://user:secret@127.0.0.1:1/repo.git",
        ])?;
        fixture.repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let credential_echo = CredentialEchoFetch(&fixture.executor);
        let error = fetch_missing_remote_branch(
            &credential_echo,
            &fixture.repo,
            &fixture.runtime,
            &fixture.home,
            "origin/main",
            fixture.budget()?,
            &CancelToken::new(),
        )
        .expect_err("transport failure must be reported");
        let UtilityError::FetchFailed { stderr, .. } = error else {
            return Err(format!("unexpected error: {error}").into());
        };
        assert!(!stderr.contains("secret"));
        assert!(stderr.contains("[REDACTED]"));
        assert!(!stderr.chars().any(char::is_control));
        assert!(!stderr.contains('\u{202e}'));
        let long_secret = format!("{}https://user:secret@example.invalid/", "x".repeat(990));
        let excerpt = fetch_stderr_excerpt(long_secret.as_bytes());
        assert!(!excerpt.contains("secret"));
        assert!(excerpt.contains("[REDACTED]"));
        assert!(excerpt.len() <= MAX_FETCH_STDERR);
        Ok(())
    }

    #[test]
    fn missing_remote_fetch_propagates_probe_cancel_and_timeout() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        add_missing_test_remote(&mut fixture)?;
        for (behavior, expected) in [
            (ProbeBehavior::Cancelled, "cancelled"),
            (ProbeBehavior::TimedOut, "timed out"),
        ] {
            let executor = ControlledFetch {
                executor: &fixture.executor,
                fetch_termination: Termination::Exit(1),
                probe_behavior: behavior,
            };
            let error = fetch_missing_remote_branch(
                &executor,
                &fixture.repo,
                &fixture.runtime,
                &fixture.home,
                "origin/main",
                fixture.budget()?,
                &CancelToken::new(),
            )
            .expect_err("probe failure must be propagated");
            match (expected, error) {
                ("cancelled", UtilityError::Cancelled) | ("timed out", UtilityError::TimedOut) => {}
                (_, error) => return Err(format!("unexpected error: {error}").into()),
            }
        }
        Ok(())
    }

    #[test]
    fn missing_remote_fetch_signal_is_reported() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        add_missing_test_remote(&mut fixture)?;
        let executor = ControlledFetch {
            executor: &fixture.executor,
            fetch_termination: Termination::Signal(15),
            probe_behavior: ProbeBehavior::Exit(2),
        };
        let error = fetch_missing_remote_branch(
            &executor,
            &fixture.repo,
            &fixture.runtime,
            &fixture.home,
            "origin/main",
            fixture.budget()?,
            &CancelToken::new(),
        )
        .expect_err("signalled fetch must fail");
        assert!(matches!(error, UtilityError::Signalled { signal: 15 }));
        Ok(())
    }

    #[test]
    fn missing_remote_fetch_probe_success_does_not_hide_failure() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        add_missing_test_remote(&mut fixture)?;
        let executor = ControlledFetch {
            executor: &fixture.executor,
            fetch_termination: Termination::Exit(1),
            probe_behavior: ProbeBehavior::Exit(0),
        };
        let error = fetch_missing_remote_branch(
            &executor,
            &fixture.repo,
            &fixture.runtime,
            &fixture.home,
            "origin/main",
            fixture.budget()?,
            &CancelToken::new(),
        )
        .expect_err("probe success must not hide fetch failure");
        assert!(matches!(error, UtilityError::FetchFailed { status: 1, .. }));
        Ok(())
    }

    #[test]
    fn worktree_diff_preserves_index_and_reports_edits() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("AGENTS.md");
        fs::write(&path, b"baseline\n")?;
        fixture.run(&["add", "AGENTS.md"])?;
        fixture.run(&["commit", "-m", "baseline"])?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let index = crate::git_evidence::read_index_fingerprint(fixture.repo.private_dir())?;
        let read_diff = || {
            run_utility(
                &fixture.executor,
                GitRead::DiffWorktree {
                    paths: vec![RepoPath::new(Path::new("AGENTS.md"))?],
                },
                &fixture.repo,
                &fixture.runtime,
                fixture.budget()?,
                &CancelToken::new(),
            )
        };
        assert!(read_diff()?.stdout.bytes.is_empty());
        assert_eq!(
            index,
            crate::git_evidence::read_index_fingerprint(fixture.repo.private_dir())?
        );
        fs::write(&path, b"changed\n")?;
        let diff = read_diff()?.stdout.bytes;
        assert!(
            diff.windows(b"+changed\n".len())
                .any(|part| part == b"+changed\n")
        );
        assert_eq!(
            index,
            crate::git_evidence::read_index_fingerprint(fixture.repo.private_dir())?
        );
        Ok(())
    }

    #[test]
    fn utility_reads_status_and_head_through_the_executor() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let status = run_utility(
            &fixture.executor,
            GitRead::Status,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(status.termination, Termination::Exit(0));
        assert!(status.stdout.bytes.starts_with(b"# branch.oid "));
        let oid = fixture.head()?;
        assert_eq!(oid.to_string().len(), 40);
        assert_eq!(
            fixture.repo.root_path(),
            provenant_os::fs::canonicalize(&fixture.root)?
        );
        assert!(fixture.home.is_dir());
        let tiny_capture = UtilityBudget::new(
            Duration::from_secs(2),
            Duration::from_millis(100),
            Duration::from_secs(2),
            1,
            256,
        )?;
        assert!(
            run_utility(
                &fixture.executor,
                GitRead::Status,
                &fixture.repo,
                &fixture.runtime,
                tiny_capture,
                &CancelToken::new()
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn ordinary_reads_respect_shallow_history_while_release_reads_include_it()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["commit", "--allow-empty", "-m", "shallow parent"])?;
        fixture.run(&["commit", "--allow-empty", "-m", "shallow tip"])?;
        let tip = fixture.head()?;
        let parent = fixture.run(&["rev-parse", "HEAD^"])?;
        let parent = std::str::from_utf8(&parent.stdout.bytes)?.trim();
        fs::write(
            fixture.repo.private_git_dir().join("shallow"),
            format!("{parent}\n"),
        )?;

        let read_only = fixture.runtime.open_read_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let ordinary = run_read_utility(
            &fixture.executor,
            GitRead::FirstParentHistory { tip },
            &read_only,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(ordinary.termination, Termination::Exit(0));
        assert_eq!(
            ordinary
                .stdout
                .bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            2
        );

        let release = run_release_read_utility(
            &fixture.executor,
            GitRead::FirstParentHistory { tip },
            &read_only,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(release.termination, Termination::Exit(0));
        assert_eq!(
            release
                .stdout
                .bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            3
        );
        Ok(())
    }

    fn poisoned_read_fixture()
    -> Result<(Fixture, crate::test_support::PoisonedNestedRepo), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join("safe.txt"), "safe\n")?;
        fs::write(
            fixture.root.join(".gitmodules"),
            "[submodule \"sub\"]\n\tpath = sub\n\turl = https://example.invalid/sub\n",
        )?;
        fixture.run(&["add", "--", ".gitmodules", "safe.txt"])?;
        fixture.run(&["commit", "-m", "outer fixtures"])?;
        let marker = fixture._temp.path().join("nested-filter-ran");
        let nested = add_poisoned_gitlink(&fixture, &fixture.root, "sub", &marker)?;
        fixture.run(&["commit", "-m", "embedded gitlink"])?;
        assert_raw_git_runs_poison_filter(&fixture, &nested)?;
        nested.arm()?;
        Ok((fixture, nested))
    }

    fn assert_nested_read_safe(operation: GitRead) -> Result<(), Box<dyn Error>> {
        let (fixture, nested) = poisoned_read_fixture()?;
        let output = run_utility(
            &fixture.executor,
            operation,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(output.termination, Termination::Exit(0));
        assert!(
            !nested.marker.exists(),
            "utility read ran a nested clean filter"
        );
        Ok(())
    }

    #[test]
    fn embedded_gitlink_status_ignores_clean_filters() -> Result<(), Box<dyn Error>> {
        assert_nested_read_safe(GitRead::Status)?;
        assert_nested_read_safe(GitRead::StatusIncludingIgnored)
    }

    #[test]
    fn embedded_gitlink_diff_ignores_clean_filters() -> Result<(), Box<dyn Error>> {
        use crate::repo::RepoPath;
        assert_nested_read_safe(GitRead::DiffIndex {
            paths: vec![RepoPath::new("sub")?],
        })?;
        assert_nested_read_safe(GitRead::DiffWorktree {
            paths: vec![RepoPath::new("sub")?],
        })
    }

    #[test]
    fn embedded_gitlink_evidence_ignores_clean_filters() -> Result<(), Box<dyn Error>> {
        use crate::git_evidence::Git;
        use crate::repo::RepoPath;
        let (fixture, nested) = poisoned_read_fixture()?;
        let head = fixture.head()?;
        let token = CancelToken::new();
        let evidence = Git::new(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &token,
        )
        .evidence(head, &[RepoPath::new("safe.txt")?])?;
        assert_eq!(evidence.head, head);
        assert!(
            !nested.marker.exists(),
            "evidence read ran a nested clean filter"
        );
        Ok(())
    }

    #[test]
    fn filters_are_disabled_not_refused_and_empty_tree_source_suppresses_worktree_attributes()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let tracked = fixture.root.join("tracked.txt");
        let included = fixture.root.join("included.txt");
        for file in [&tracked, &included] {
            fs::write(file, "baseline\n")?;
        }
        fixture.run(&["add", "--", "tracked.txt", "included.txt"])?;
        fixture.run(&["commit", "-m", "tracked fixture"])?;

        let marker = fixture._temp.path().join("filter-executed");
        let filter = format!("touch {}; cat", marker.display());
        // One driver in local config, one reached through an include.
        fixture.run(&["config", "filter.poison.clean", filter.as_str()])?;
        let extra = fixture.repo.common_git_dir().join("extra.cfg");
        fs::write(
            &extra,
            format!("[filter \"Included\"]\n\tclean = {filter}\n"),
        )?;
        fixture.run(&["config", "include.path", "extra.cfg"])?;
        fs::write(
            fixture.root.join(".gitattributes"),
            "tracked.txt filter=poison\n",
        )?;
        let info = fixture.repo.common_git_dir().join("info");
        fs::create_dir_all(&info)?;
        fs::write(
            info.join("attributes"),
            "tracked.txt filter=poison\nincluded.txt filter=Included\n",
        )?;
        for file in [&tracked, &included] {
            fs::write(file, "modified\n")?;
        }

        // Raw Git obeys info/attributes even with an empty attribute source.
        let source = &fixture
            .repo
            .probe_data()
            .ok_or("missing eagerly-probed repository data")?
            .attribute_source;
        let source_option = format!("--attr-source={source}");
        let raw = fixture.run(&[source_option.as_str(), "status", "--porcelain=v2", "-z"])?;
        assert_eq!(raw.termination, Termination::Exit(0));
        assert!(marker.exists(), "control: raw Git should run the filter");
        fs::remove_file(&marker)?;

        // The config changed after the handle was probed, so the handle is
        // stale and runs nothing.
        let status = |repo: &VerifiedRepo| {
            run_utility(
                &fixture.executor,
                GitRead::Status,
                repo,
                &fixture.runtime,
                fixture.budget()?,
                &CancelToken::new(),
            )
        };
        assert!(matches!(
            status(&fixture.repo),
            Err(UtilityError::Repo(RepoError::IdentityChanged))
        ));
        assert!(!marker.exists());
        let open = || {
            fixture.runtime.open_repo(
                &fixture.executor,
                &fixture.root,
                fixture.budget()?,
                &CancelToken::new(),
            )
        };
        // A fresh open neutralises both drivers instead of refusing the repo.
        let repo = open()?;
        assert_eq!(status(&repo)?.termination, Termination::Exit(0));
        assert!(!marker.exists());

        // A process filter in the worktree config is covered the same way, and
        // makes the previous handle stale in turn.
        fixture.run(&["config", "extensions.worktreeConfig", "true"])?;
        fixture.run(&[
            "config",
            "--worktree",
            "filter.poison.process",
            filter.as_str(),
        ])?;
        assert!(matches!(
            status(&repo),
            Err(UtilityError::Repo(RepoError::IdentityChanged))
        ));
        assert!(!marker.exists());
        let repo = open()?;
        assert_eq!(status(&repo)?.termination, Termination::Exit(0));
        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn linked_worktree_is_accepted_only_with_git_backlink_registration()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let head = fixture.head()?;
        let branch = GitRefName::branch("s3/fixture")?;
        let worktree_path = fixture._temp.path().join("linked");
        let added = execute_git_mutation(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            GitMutation::WorktreeAdd {
                branch,
                base: Some(head),
                path: worktree_path.clone(),
                create_branch: true,
            },
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(added.termination, Termination::Exit(0));
        let worktree = verify_worktree(&fixture.repo, &worktree_path)?;
        assert!(
            worktree
                .repo()
                .common_identity()
                .same_identity(&fixture.repo.common_identity())
        );
        let status = run_utility(
            &fixture.executor,
            GitRead::Status,
            worktree.repo(),
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(status.termination, Termination::Exit(0));

        let removed = execute_git_mutation(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            GitMutation::WorktreeRemove {
                path: worktree_path,
            },
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(removed.termination, Termination::Exit(0));
        Ok(())
    }

    /// A user config that includes a file for lane branches only:
    /// `includeIf "onbranch:lane/*"`. The target is where lanes can write.
    struct LaneInclude {
        marker: PathBuf,
        filter: String,
    }

    impl LaneInclude {
        fn new(fixture: &Fixture) -> Result<Self, Box<dyn Error>> {
            let tracked = fixture.root.join("tracked.txt");
            fs::write(&tracked, "baseline\n")?;
            fixture.run(&["add", "--", "tracked.txt"])?;
            fixture.run(&["commit", "-m", "tracked fixture"])?;
            let marker = fixture._temp.path().join("filter-executed");
            let filter = format!("touch {}; cat", marker.display());
            let info = fixture.repo.common_git_dir().join("info");
            fs::create_dir_all(&info)?;
            fs::write(info.join("attributes"), "tracked.txt filter=lane\n")?;
            Ok(Self { marker, filter })
        }

        fn include(&self, fixture: &Fixture, target: &Path) -> Result<(), Box<dyn Error>> {
            let target = target.to_str().ok_or("non-UTF8 include")?;
            fixture.run(&["config", "includeIf.onbranch:lane/*.path", target])?;
            Ok(())
        }

        fn write(&self, target: &Path, extra: &str) -> Result<(), Box<dyn Error>> {
            fs::create_dir_all(target.parent().ok_or("no parent")?)?;
            fs::write(
                target,
                format!(
                    "[filter \"lane\"]\n\tclean = {0}\n\tsmudge = {0}\n{extra}",
                    self.filter
                ),
            )?;
            Ok(())
        }
    }

    fn open(fixture: &Fixture) -> Result<VerifiedRepo, Box<dyn Error>> {
        Ok(fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?)
    }

    fn add_lane_worktree(
        fixture: &Fixture,
        repo: &VerifiedRepo,
        branch: &str,
        path: &Path,
    ) -> Result<(), Box<dyn Error>> {
        let head = run_utility(
            &fixture.executor,
            GitRead::RevParseHead,
            repo,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let head = GitOid::parse(std::str::from_utf8(&head.stdout.bytes)?.trim())?;
        let added = execute_git_mutation(
            &fixture.executor,
            repo,
            &fixture.runtime,
            GitMutation::WorktreeAdd {
                branch: GitRefName::branch(branch)?,
                base: Some(head),
                path: path.to_path_buf(),
                create_branch: true,
            },
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        assert_eq!(added.termination, Termination::Exit(0));
        Ok(())
    }

    fn probe(
        fixture: &Fixture,
        repo: &VerifiedRepo,
        path: &Path,
    ) -> Result<VerifiedWorktree, Box<dyn Error>> {
        let mut tree = verify_worktree(repo, path)?;
        fixture.runtime.probe_worktree(
            &fixture.executor,
            &mut tree,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        Ok(tree)
    }

    fn status(fixture: &Fixture, repo: &VerifiedRepo) -> Result<ExecOutcome, UtilityError> {
        run_utility(
            &fixture.executor,
            GitRead::Status,
            repo,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        )
    }

    #[test]
    fn onbranch_include_of_a_lane_writable_file_is_neutralised_for_its_worktree()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let lane = LaneInclude::new(&fixture)?;
        // The project's worktree area is where lanes write.
        let target = fixture.root.join(".worktrees/lane.cfg");
        lane.write(&target, "[core]\n\tfilemode = false\n")?;
        lane.include(&fixture, &target)?;

        // The primary checkout is not on a lane branch, so Git does not read
        // the file, but the handle is tied to HEAD and already neutralises it:
        // creating a lane worktree checks files out under that include.
        let repo = open(&fixture)?;
        let data = repo.probe_data().ok_or("unprobed")?;
        assert!(data.config_head_sensitive);
        assert!(data.config_files.contains(&fs::canonicalize(&target)?));
        let overrides = &data.config_overrides;
        assert!(overrides.contains(&("filter.lane.clean".into(), String::new())));
        assert!(overrides.contains(&("core.filemode".into(), "true".into())));
        let linked = fixture._temp.path().join("linked");
        add_lane_worktree(&fixture, &repo, "lane/one", &linked)?;
        assert!(!lane.marker.exists());

        // On the lane branch Git reads the file. Control: raw Git runs the
        // filter there; the utility does not.
        let tree = probe(&fixture, &repo, &linked)?;
        assert!(
            tree.repo()
                .probe_data()
                .ok_or("unprobed")?
                .config_overrides
                .contains(&("filter.lane.clean".into(), String::new()))
        );
        fs::write(linked.join("tracked.txt"), "modified\n")?;
        let linked_text = linked.to_str().ok_or("non-UTF8 worktree")?;
        fixture.run(&["-C", linked_text, "status", "--porcelain=v2"])?;
        assert!(
            lane.marker.exists(),
            "control: raw Git runs the lane filter"
        );
        fs::remove_file(&lane.marker)?;
        assert_eq!(
            status(&fixture, tree.repo())?.termination,
            Termination::Exit(0)
        );
        assert!(!lane.marker.exists());
        Ok(())
    }

    #[test]
    fn a_lane_include_that_cannot_be_neutralised_fails_only_its_worktree()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let lane = LaneInclude::new(&fixture)?;
        let linked = fixture._temp.path().join("linked");
        let other = fixture._temp.path().join("other");
        let target = linked.join("lane.cfg");
        lane.include(&fixture, &target)?;
        let repo = open(&fixture)?;
        add_lane_worktree(&fixture, &repo, "lane/one", &linked)?;
        add_lane_worktree(&fixture, &repo, "other/two", &other)?;
        // The lane writes into its own worktree. The setting changes which
        // files Git reports, and nothing else in the configuration gives it a
        // value to pin, so it cannot be neutralised.
        lane.write(&target, "[status]\n\tshowUntrackedFiles = no\n")?;
        let repo = open(&fixture)?;

        let mut tree = verify_worktree(&repo, &linked)?;
        let error = fixture
            .runtime
            .probe_worktree(
                &fixture.executor,
                &mut tree,
                fixture.budget()?,
                &CancelToken::new(),
            )
            .err()
            .ok_or("the lane worktree must fail")?;
        assert!(
            matches!(&error, UtilityError::LaneConfiguration { key, .. } if key == "status.showuntrackedfiles"),
            "{error}"
        );
        assert!(error.to_string().contains("cannot be neutralised"));
        // The primary checkout and a worktree on another branch are unaffected.
        assert_eq!(status(&fixture, &repo)?.termination, Termination::Exit(0));
        let tree = probe(&fixture, &repo, &other)?;
        assert_eq!(
            status(&fixture, tree.repo())?.termination,
            Termination::Exit(0)
        );
        Ok(())
    }

    #[test]
    fn editing_an_included_lane_file_makes_the_handle_stale() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let lane = LaneInclude::new(&fixture)?;
        let target = fixture.root.join(".worktrees/lane.cfg");
        lane.write(&target, "")?;
        lane.include(&fixture, &target)?;
        let repo = open(&fixture)?;
        let linked = fixture._temp.path().join("linked");
        add_lane_worktree(&fixture, &repo, "lane/one", &linked)?;
        let tree = probe(&fixture, &repo, &linked)?;
        assert_eq!(
            status(&fixture, tree.repo())?.termination,
            Termination::Exit(0)
        );

        // Neither handle may be used once the included bytes change.
        lane.write(&target, "[core]\n\tfilemode = false\n")?;
        for handle in [&repo, tree.repo()] {
            assert!(matches!(
                status(&fixture, handle),
                Err(UtilityError::Repo(RepoError::IdentityChanged))
            ));
        }
        Ok(())
    }

    #[test]
    fn a_head_change_makes_a_handle_with_unread_lane_includes_stale() -> Result<(), Box<dyn Error>>
    {
        let fixture = Fixture::new()?;
        let lane = LaneInclude::new(&fixture)?;
        let target = fixture.root.join(".worktrees/lane.cfg");
        lane.write(&target, "")?;
        lane.include(&fixture, &target)?;
        let repo = open(&fixture)?;
        assert_eq!(status(&fixture, &repo)?.termination, Termination::Exit(0));
        // Switching to a lane branch would activate the include.
        fixture.run(&["checkout", "-q", "-b", "lane/primary"])?;
        assert!(matches!(
            status(&fixture, &repo),
            Err(UtilityError::Repo(RepoError::IdentityChanged))
        ));
        Ok(())
    }

    #[test]
    fn recorded_git_contract_fixtures_pin_nul_and_oid_forms() -> Result<(), Box<dyn Error>> {
        let sha1_status = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha1/status.raw"
        ));
        let sha256_status = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha256/status.raw"
        ));
        let sha1_index = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha1/index-entries.raw"
        ));
        let sha256_index = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha256/index-entries.raw"
        ));
        let sha1_worktrees = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha1/worktree-list.raw"
        ));
        let sha256_worktrees = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha256/worktree-list.raw"
        ));
        let sha1_oid = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha1/head-oid.txt"
        ));
        let sha256_oid = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha256/head-oid.txt"
        ));
        let sha1_tree = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha1/tree-entries.raw"
        ));
        let sha256_tree = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/sha256/tree-entries.raw"
        ));
        let version = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/git-version.txt"
        ));

        assert!(version.starts_with("git version "));
        assert!(sha1_status.as_bytes().contains(&0));
        assert!(sha256_status.as_bytes().contains(&0));
        assert!(sha1_status.contains("1 MM N..."));
        assert!(sha256_status.contains("1 MM N..."));
        assert!(sha1_index.as_bytes().contains(&0));
        assert!(sha256_index.as_bytes().contains(&0));
        assert!(sha1_worktrees.as_bytes().contains(&0));
        assert!(sha256_worktrees.as_bytes().contains(&0));
        assert!(sha1_tree.contains("\t.agents/guide.md\0"));
        assert!(sha256_tree.contains("\t.agents/guide.md\0"));
        assert!(matches!(GitOid::parse(sha1_oid.trim())?, GitOid::Sha1(_)));
        assert!(matches!(
            GitOid::parse(sha256_oid.trim())?,
            GitOid::Sha256(_)
        ));
        Ok(())
    }

    #[test]
    fn utility_budgets_and_paths_fail_closed() {
        assert!(UtilityBudget::new(Duration::ZERO, Duration::ZERO, Duration::ZERO, 1, 1).is_err());
        assert!(
            UtilityBudget::new(
                Duration::from_secs(1),
                Duration::from_secs(3),
                Duration::ZERO,
                1,
                1
            )
            .is_err()
        );
        assert!(RepoPath::new("../outside").is_err());
        assert!(RepoPath::new("/absolute").is_err());
        assert!(RepoPath::new("a//b").is_err());
        assert!(GitRefName::branch("../bad").is_err());
        assert!(GitRefName::new("refs/remotes/origin/main").is_err());
    }
}
