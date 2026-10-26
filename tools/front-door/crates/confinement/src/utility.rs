//! Closed, bounded Git reads and separately typed Git custody mutations.
use crate::repo::{
    GitRefName, GitRevision, ReadOnlyRepo, RepoError, RepoObjectFormat, RepoPath, RepoPathError,
    RepoProbeData, VerifiedRepo,
};
use provenant_core::{CancelToken, GitOid};
use provenant_os::exec::{
    ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy, Termination,
};
use provenant_os::fs::{Directory, FileKind, FsError, Metadata, canonicalize, effective_uid};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

const DEFAULT_STREAM_LIMIT: usize = 8 * 1024 * 1024;
const MAX_STREAM_LIMIT: usize = 64 * 1024 * 1024;
const DEFAULT_TERMINATE_GRACE: Duration = Duration::from_millis(250);
const DEFAULT_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_UTILITY_TIMEOUT: Duration = Duration::from_secs(14_400);
const MAX_CLEANUP_BUDGET: Duration = Duration::from_secs(2);
const MAX_GIT_LOG: u64 = 1000;
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
    #[error("pinned {tool} executable changed after setup")]
    ExecutableChanged { tool: &'static str },
    #[error("utility home must be an absolute private directory")]
    InvalidHome,
    #[error("utility budget is outside its supported bounds")]
    InvalidBudget,
    #[error("utility operation has an invalid or unsupported value")]
    InvalidOperationValue,
    #[error("repository Git filters or config includes are not permitted")]
    UnsafeGitConfiguration,
    #[error("repository info/attributes is not permitted")]
    RepositoryAttributes,
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
    ResolveRevision {
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
    BranchTips,
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
    WorktreeAdd {
        branch: GitRefName,
        base: Option<GitOid>,
        path: PathBuf,
        create_branch: bool,
    },
    WorktreeRemove {
        path: PathBuf,
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
        })
    }
    pub fn new(git: impl AsRef<Path>, home: impl AsRef<Path>) -> Result<Self, UtilityError> {
        Ok(Self {
            git: ExecutablePin::new(git.as_ref(), "git")?,
            home: PrivateHome::new(home.as_ref())?,
        })
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
            verify_repo_binding(repo)?;
            reject_info_attributes(repo)?;
            verify_local_git_configuration(executor, repo, self, budget, cancel)?;
            let attribute_source = empty_attribute_source(executor, repo, self, budget, cancel)?;
            let object_format = match attribute_source.len() {
                40 => RepoObjectFormat::Sha1,
                64 => RepoObjectFormat::Sha256,
                _ => return Err(UtilityError::AttributeSourceUnsupported),
            };
            Ok(RepoProbeData {
                object_format,
                attribute_source,
            })
        })
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
            verify_repo_binding(repo)?;
            reject_info_attributes(repo)?;
            verify_local_git_configuration(executor, repo, self, budget, cancel)?;
            let attribute_source = empty_attribute_source(executor, repo, self, budget, cancel)?;
            let object_format = match attribute_source.len() {
                40 => RepoObjectFormat::Sha1,
                64 => RepoObjectFormat::Sha256,
                _ => return Err(UtilityError::AttributeSourceUnsupported),
            };
            Ok(RepoProbeData {
                object_format,
                attribute_source,
            })
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

    pub fn git_program_path(&self) -> &Path {
        &self.git.path
    }

    fn revalidate(&self) -> Result<(), UtilityError> {
        self.git.verify()?;
        self.home.verify()?;
        Ok(())
    }
}

struct ExecutablePin {
    path: PathBuf,
    parent_path: PathBuf,
    name: OsString,
    parent_identity: Metadata,
    identity: Metadata,
    tool: &'static str,
}

impl ExecutablePin {
    fn new(input: &Path, tool: &'static str) -> Result<Self, UtilityError> {
        if !input.is_absolute() || input.file_name().and_then(OsStr::to_str) != Some(tool) {
            return Err(UtilityError::InvalidExecutable { tool });
        }
        let input_parent = input
            .parent()
            .ok_or(UtilityError::InvalidExecutable { tool })?;
        let parent_path = canonicalize(input_parent)?;
        let parent = Directory::open_anchor(&parent_path)
            .map_err(|_| UtilityError::InvalidExecutable { tool })?;
        let parent_identity = parent.metadata()?;
        if parent_identity.kind != FileKind::Directory || parent_identity.mode & 0o022 != 0 {
            return Err(UtilityError::InvalidExecutable { tool });
        }
        let name = input
            .file_name()
            .ok_or(UtilityError::InvalidExecutable { tool })?
            .to_os_string();
        let identity = verify_executable_at(&parent, &name, tool)?;
        Ok(Self {
            path: parent_path.join(&name),
            parent_path,
            name,
            parent_identity,
            identity,
            tool,
        })
    }

    fn verify(&self) -> Result<(), UtilityError> {
        let fresh = Directory::open_anchor(&self.parent_path)
            .map_err(|_| UtilityError::ExecutableChanged { tool: self.tool })?;
        let fresh_identity = fresh
            .metadata()
            .map_err(|_| UtilityError::ExecutableChanged { tool: self.tool })?;
        let current = verify_executable_at(&fresh, &self.name, self.tool)?;
        if !self.parent_identity.same_identity(&fresh_identity)
            || !self.identity.same_identity(&current)
            || self.identity.mode != current.mode
            || self.identity.uid != current.uid
        {
            return Err(UtilityError::ExecutableChanged { tool: self.tool });
        }
        Ok(())
    }
}

fn verify_executable_at(
    parent: &Directory,
    name: &OsStr,
    tool: &'static str,
) -> Result<Metadata, UtilityError> {
    let before = parent
        .metadata_at(name, false)
        .map_err(|_| UtilityError::InvalidExecutable { tool })?;
    if before.kind != FileKind::Regular || before.mode & 0o111 == 0 || before.mode & 0o022 != 0 {
        return Err(UtilityError::InvalidExecutable { tool });
    }
    let file = parent
        .open_read(name)
        .map_err(|_| UtilityError::InvalidExecutable { tool })?;
    let opened = file
        .metadata()
        .map_err(|_| UtilityError::InvalidExecutable { tool })?;
    let after = parent
        .metadata_at(name, false)
        .map_err(|_| UtilityError::InvalidExecutable { tool })?;
    if opened.kind != FileKind::Regular
        || !before.same_identity(&opened)
        || !opened.same_identity(&after)
        || opened.mode & 0o111 == 0
        || opened.mode & 0o022 != 0
    {
        return Err(UtilityError::InvalidExecutable { tool });
    }
    Ok(opened)
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
    reject_info_attributes(repo)?;
    let attribute_source = repo
        .probe_data()
        .ok_or(UtilityError::UnprobedRepository)?
        .attribute_source
        .as_str();
    let (args, stdin) = command(attribute_source)?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(runtime.git.path.as_os_str().to_owned());
    argv.extend(args);
    let mut env = environment(runtime, repo);
    if release_scan {
        env.push(("GIT_SHALLOW_FILE".into(), "/dev/null".into()));
    }
    env.push(("GIT_ATTR_SOURCE".into(), attribute_source.into()));
    runtime.git.verify()?;
    let options = exec_options(
        &argv,
        repo.root_path(),
        budget,
        &env,
        stdin.as_deref(),
        Some(cancel),
    );
    validate_outcome(executor.execute(options)?)
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
                "diff".into(),
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
        GitRead::BranchTips => vec![
            "for-each-ref".into(),
            "--format=%(refname)%00%(objectname)%00".into(),
            "refs/heads".into(),
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
        (
            "GIT_CONFIG_COUNT".into(),
            GIT_CONFIG_OVERRIDES.len().to_string().into(),
        ),
    ];
    for (index, (key, value)) in GIT_CONFIG_OVERRIDES.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{index}").into(), (*key).into()));
        env.push((format!("GIT_CONFIG_VALUE_{index}").into(), (*value).into()));
    }
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

/// Inspect only Git config key names, without following include directives or
/// exposing values. Attribute-selected filters can execute only when a driver
/// is configured; includes are refused because later Git commands would follow
/// them and could supply such a driver.
fn verify_local_git_configuration(
    executor: &dyn Executor,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<(), UtilityError> {
    let command =
        ["config", "--no-includes", "--name-only", "--null", "--list"].map(OsString::from);
    let outcome = run_git_probe(executor, repo, runtime, budget, cancel, &command)?;
    if outcome.termination != Termination::Exit(0)
        || (!outcome.stdout.bytes.is_empty() && !outcome.stdout.bytes.ends_with(&[0]))
    {
        return Err(UtilityError::UnsafeGitConfiguration);
    }
    for key in outcome.stdout.bytes.split(|byte| *byte == 0) {
        let lower = key
            .iter()
            .map(|byte| byte.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if lower.starts_with(b"filter.")
            || lower.starts_with(b"include.")
            || lower.starts_with(b"includeif.")
        {
            return Err(UtilityError::UnsafeGitConfiguration);
        }
    }
    Ok(())
}

/// Refuse repository-local attribute files, which Git consults even when an
/// empty tree is selected with `--attr-source`. Both linked-worktree and common
/// Git directories are checked because Git's lookup spans those locations.
fn reject_info_attributes(repo: &VerifiedRepo) -> Result<(), UtilityError> {
    for directory in [repo.private_dir(), repo.common_dir()] {
        let info_name = OsStr::new("info");
        let info_metadata = match directory.metadata_at(info_name, false) {
            Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
            Ok(_) => return Err(UtilityError::RepositoryAttributes),
            Err(error) if is_not_found(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        let info = directory.open_verified_directory(info_name)?;
        if !info_metadata.same_identity(&info.metadata()?) {
            return Err(UtilityError::RepositoryAttributes);
        }
        match info.metadata_at(OsStr::new("attributes"), false) {
            Ok(_) => return Err(UtilityError::RepositoryAttributes),
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn is_not_found(error: &FsError) -> bool {
    matches!(error, FsError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
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
) -> Result<String, UtilityError> {
    let format_args = [
        OsString::from("rev-parse"),
        OsString::from("--show-object-format"),
    ];
    let format_outcome = run_git_probe(executor, repo, runtime, budget, cancel, &format_args)?;
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
    let preflight = run_git_probe(executor, repo, runtime, budget, cancel, &preflight_args)?;
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
    command: &[OsString],
) -> Result<ExecOutcome, UtilityError> {
    runtime.git.verify()?;
    let mut argv = vec![
        runtime.git.path.as_os_str().to_owned(),
        OsString::from("--git-dir"),
        repo.private_git_dir().as_os_str().to_owned(),
        OsString::from("--work-tree"),
        repo.root_path().as_os_str().to_owned(),
    ];
    argv.extend(GIT_COMMON.iter().map(OsString::from));
    argv.extend(command.iter().cloned());
    let env = environment(runtime, repo);
    let options = exec_options(&argv, repo.root_path(), budget, &env, None, Some(cancel));
    validate_outcome(executor.execute(options)?)
}

fn mutation_args(mutation: GitMutation) -> Result<(Vec<OsString>, Option<Vec<u8>>), UtilityError> {
    let mut args = GIT_COMMON.iter().map(OsString::from).collect::<Vec<_>>();
    let input = match mutation {
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
    use crate::repo::verify_worktree;
    use crate::test_support::{Fixture, add_poisoned_gitlink, assert_raw_git_runs_poison_filter};
    use std::error::Error;
    use std::fs;
    use std::time::Duration;

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
        assert_eq!(fixture.runtime.git_program_path(), fixture.git.as_path());
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
    fn filter_config_is_refused_and_empty_tree_source_suppresses_worktree_attributes()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let tracked = fixture.root.join("tracked.txt");
        fs::write(&tracked, "baseline\n")?;
        fixture.run(&["add", "--", "tracked.txt"])?;
        fixture.run(&["commit", "-m", "tracked fixture"])?;

        let marker = fixture._temp.path().join("filter-executed");
        let filter = format!("touch {}; cat", marker.display());
        fixture.run(&["config", "filter.poison.clean", filter.as_str()])?;
        fs::write(
            fixture.root.join(".gitattributes"),
            "tracked.txt filter=poison\n",
        )?;
        fs::write(&tracked, "modified\n")?;

        let source = &fixture
            .repo
            .probe_data()
            .ok_or("missing eagerly-probed repository data")?
            .attribute_source;
        let source_option = format!("--attr-source={source}");
        let raw = fixture.run(&[source_option.as_str(), "status", "--porcelain=v2", "-z"])?;
        assert_eq!(raw.termination, Termination::Exit(0));
        assert!(!marker.exists());

        let result = run_utility(
            &fixture.executor,
            GitRead::Status,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        );
        assert!(matches!(
            result,
            Err(UtilityError::Repo(RepoError::IdentityChanged))
        ));
        assert!(!marker.exists());
        assert!(matches!(
            fixture.runtime.open_repo(
                &fixture.executor,
                &fixture.root,
                fixture.budget()?,
                &CancelToken::new(),
            ),
            Err(UtilityError::UnsafeGitConfiguration)
        ));
        fixture.run(&["config", "--unset", "filter.poison.clean"])?;
        fixture.run(&["config", "extensions.worktreeConfig", "true"])?;
        fixture.run(&[
            "config",
            "--worktree",
            "filter.poison.process",
            filter.as_str(),
        ])?;
        assert!(matches!(
            fixture.runtime.open_repo(
                &fixture.executor,
                &fixture.root,
                fixture.budget()?,
                &CancelToken::new(),
            ),
            Err(UtilityError::UnsafeGitConfiguration)
        ));
        assert!(!marker.exists());
        assert!(matches!(
            run_utility(
                &fixture.executor,
                GitRead::Status,
                &fixture.repo,
                &fixture.runtime,
                fixture.budget()?,
                &CancelToken::new()
            ),
            Err(UtilityError::Repo(RepoError::IdentityChanged))
        ));
        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn utility_refuses_private_or_common_info_attributes() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let info = fixture.repo.common_git_dir().join("info");
        fs::create_dir_all(&info)?;
        fs::write(info.join("attributes"), "*.txt filter=unknown\n")?;

        let result = run_utility(
            &fixture.executor,
            GitRead::Status,
            &fixture.repo,
            &fixture.runtime,
            fixture.budget()?,
            &CancelToken::new(),
        );
        assert!(matches!(result, Err(UtilityError::RepositoryAttributes)));
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
