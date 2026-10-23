//! Closed, bounded Git and GitHub CLI reads plus separately typed Git custody
//! mutations. Callers cannot provide argv, shell text, field names, or verbs.
use crate::repo::{GitRefName, RepoError, RepoPath, RepoPathError, VerifiedRepo};
use provenant_core::{CancelToken, GitOid};
use provenant_supervisor::exec::{
    ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy, Termination,
};
use provenant_supervisor::fs::{
    FileKind, FsError, Metadata, SafeDir, SafeFs, canonicalize, effective_uid,
};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const DEFAULT_STREAM_LIMIT: usize = 8 * 1024 * 1024;
const MAX_STREAM_LIMIT: usize = 64 * 1024 * 1024;
const DEFAULT_TERMINATE_GRACE: Duration = Duration::from_millis(250);
const DEFAULT_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_UTILITY_TIMEOUT: Duration = Duration::from_secs(14_400);
const MAX_CLEANUP_BUDGET: Duration = Duration::from_secs(2);
const MAX_GIT_LOG: u64 = 1000;
const GIT_COMMON: &[&str] = &["--no-pager", "--no-optional-locks", "--literal-pathspecs"];
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
    ("extensions.worktreeConfig", "false"),
    ("core.bare", "false"),
    ("core.fsmonitorHookVersion", "0"),
];
const PR_FIELDS: &str = "number,title,state,baseRefName,baseRefOid,headRefName,headRefOid,mergeCommit,mergedAt,url,reviewDecision,isDraft";
const ISSUE_FIELDS: &str = "number,title,state,url,labels,assignees,milestone";
const CHECK_FIELDS: &str = "name,state,link,workflow,startedAt,completedAt";
const MERGED_PR_FIELDS: &str =
    "number,state,headRefName,headRefOid,mergeCommit,mergedAt,baseRefName,baseRefOid";

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
    #[error("GitHub repository identity is bound to another Git common directory")]
    RepositoryBindingMismatch,
    #[error("utility {stream} exceeded its capture limit; output cannot be used as evidence")]
    Truncated { stream: &'static str },
    #[error("utility read timed out; output cannot be used as evidence")]
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
    pub timeout: Duration,
    pub terminate_grace: Duration,
    pub reap_timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
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

    fn validate(self) -> Result<Self, UtilityError> {
        Self::new(
            self.timeout,
            self.terminate_grace,
            self.reap_timeout,
            self.stdout_limit,
            self.stderr_limit,
        )
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullRequestNumber(u64);

impl PullRequestNumber {
    pub fn new(value: u64) -> Result<Self, UtilityError> {
        (value > 0)
            .then_some(Self(value))
            .ok_or(UtilityError::InvalidOperationValue)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IssueNumber(u64);

impl IssueNumber {
    pub fn new(value: u64) -> Result<Self, UtilityError> {
        (value > 0)
            .then_some(Self(value))
            .ok_or(UtilityError::InvalidOperationValue)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRead {
    Status,
    RevParseHead,
    ResolveRef {
        name: GitRefName,
    },
    Diff {
        base: GitOid,
        head: GitOid,
        paths: Vec<RepoPath>,
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
    MergeCommits {
        since: GitOid,
        tip: GitOid,
        limit: u64,
    },
    WorktreeList,
    BranchTips,
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

/// Explicit GitHub repository identity. Read operations never infer a target
/// from mutable Git remotes or GitHub CLI configuration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitHubRepository {
    owner: String,
    name: String,
}

impl GitHubRepository {
    pub fn new(owner: impl AsRef<str>, name: impl AsRef<str>) -> Result<Self, UtilityError> {
        let owner = owner.as_ref();
        let name = name.as_ref();
        if !valid_github_owner(owner) || !valid_github_repo_name(name) {
            return Err(UtilityError::InvalidOperationValue);
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn qualified_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

fn valid_github_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_github_repo_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRepositoryBinding {
    repository: GitHubRepository,
    common_identity: Metadata,
}

impl GitHubRepositoryBinding {
    pub fn repository(&self) -> &GitHubRepository {
        &self.repository
    }

    fn qualified_name(&self) -> String {
        self.repository.qualified_name()
    }
}

/// Bind an explicit, trusted caller repository identity to the held common
/// Git directory. The identity is an authority assertion by the caller; this
/// function never infers it from a mutable Git remote or CLI configuration.
pub fn bind_github_repository(
    repo: &VerifiedRepo,
    repository: GitHubRepository,
) -> Result<GitHubRepositoryBinding, UtilityError> {
    repo.revalidate()?;
    Ok(GitHubRepositoryBinding {
        repository,
        common_identity: repo.common_identity(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubRead {
    PullRequest {
        repository: GitHubRepositoryBinding,
        number: PullRequestNumber,
    },
    Issue {
        repository: GitHubRepositoryBinding,
        number: IssueNumber,
    },
    Checks {
        repository: GitHubRepositoryBinding,
        pull_request: PullRequestNumber,
    },
    PullRequestsMerged {
        repository: GitHubRepositoryBinding,
        head: GitRefName,
        limit: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UtilityRead {
    Git(GitRead),
    GitHub(GitHubRead),
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
    fs: Arc<dyn SafeFs>,
    git: ExecutablePin,
    gh: Option<ExecutablePin>,
    home: PrivateHome,
}

impl UtilityRuntime {
    pub fn new(
        fs: Arc<dyn SafeFs>,
        git: impl AsRef<Path>,
        gh: Option<impl AsRef<Path>>,
        home: impl AsRef<Path>,
    ) -> Result<Self, UtilityError> {
        Ok(Self {
            git: ExecutablePin::new(fs.as_ref(), git.as_ref(), "git")?,
            gh: gh
                .map(|path| ExecutablePin::new(fs.as_ref(), path.as_ref(), "gh"))
                .transpose()?,
            home: PrivateHome::new(fs.as_ref(), home.as_ref())?,
            fs,
        })
    }

    pub fn git_only(
        fs: Arc<dyn SafeFs>,
        git: impl AsRef<Path>,
        home: impl AsRef<Path>,
    ) -> Result<Self, UtilityError> {
        Self::new(fs, git, None::<&Path>, home)
    }

    pub fn open_repo(&self, path: impl AsRef<Path>) -> Result<VerifiedRepo, UtilityError> {
        Ok(VerifiedRepo::open(self.fs.clone(), path)?)
    }

    pub fn git_program_path(&self) -> &Path {
        &self.git.path
    }

    pub fn gh_program_path(&self) -> Option<&Path> {
        self.gh.as_ref().map(|program| program.path.as_path())
    }

    fn revalidate(&self) -> Result<(), UtilityError> {
        self.git.verify(self.fs.as_ref())?;
        if let Some(gh) = &self.gh {
            gh.verify(self.fs.as_ref())?;
        }
        self.home.verify(self.fs.as_ref())?;
        Ok(())
    }
}

struct ExecutablePin {
    path: PathBuf,
    parent_path: PathBuf,
    name: OsString,
    parent: Box<dyn SafeDir>,
    parent_identity: Metadata,
    identity: Metadata,
    tool: &'static str,
}

impl ExecutablePin {
    fn new(fs: &dyn SafeFs, input: &Path, tool: &'static str) -> Result<Self, UtilityError> {
        if !input.is_absolute() || input.file_name().and_then(OsStr::to_str) != Some(tool) {
            return Err(UtilityError::InvalidExecutable { tool });
        }
        let input_parent = input
            .parent()
            .ok_or(UtilityError::InvalidExecutable { tool })?;
        let parent_path = canonicalize(input_parent)?;
        let parent = fs
            .open_anchor(&parent_path)
            .map_err(|_| UtilityError::InvalidExecutable { tool })?;
        let parent_identity = parent.metadata()?;
        if parent_identity.kind != FileKind::Directory || parent_identity.mode & 0o022 != 0 {
            return Err(UtilityError::InvalidExecutable { tool });
        }
        let name = input
            .file_name()
            .ok_or(UtilityError::InvalidExecutable { tool })?
            .to_os_string();
        let identity = verify_executable_at(parent.as_ref(), &name, tool)?;
        Ok(Self {
            path: parent_path.join(&name),
            parent_path,
            name,
            parent,
            parent_identity,
            identity,
            tool,
        })
    }

    fn verify(&self, fs: &dyn SafeFs) -> Result<(), UtilityError> {
        let held = self
            .parent
            .metadata()
            .map_err(|_| UtilityError::ExecutableChanged { tool: self.tool })?;
        let fresh = fs
            .open_anchor(&self.parent_path)
            .map_err(|_| UtilityError::ExecutableChanged { tool: self.tool })?;
        let fresh_identity = fresh
            .metadata()
            .map_err(|_| UtilityError::ExecutableChanged { tool: self.tool })?;
        let current = verify_executable_at(fresh.as_ref(), &self.name, self.tool)?;
        if !self.parent_identity.same_identity(&held)
            || !self.parent_identity.same_identity(&fresh_identity)
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
    parent: &dyn SafeDir,
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
    anchor: Box<dyn SafeDir>,
    identity: Metadata,
}

impl PrivateHome {
    fn new(fs: &dyn SafeFs, input: &Path) -> Result<Self, UtilityError> {
        if !input.is_absolute() {
            return Err(UtilityError::InvalidHome);
        }
        let initial = fs
            .open_anchor(input)
            .map_err(|_| UtilityError::InvalidHome)?;
        let before = initial.metadata().map_err(|_| UtilityError::InvalidHome)?;
        let path = canonicalize(input).map_err(|_| UtilityError::InvalidHome)?;
        let anchor = fs
            .open_anchor(&path)
            .map_err(|_| UtilityError::InvalidHome)?;
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
            anchor,
            identity,
        })
    }

    fn verify(&self, fs: &dyn SafeFs) -> Result<(), UtilityError> {
        let held = self
            .anchor
            .metadata()
            .map_err(|_| UtilityError::InvalidHome)?;
        let fresh = fs
            .open_anchor(&self.path)
            .map_err(|_| UtilityError::InvalidHome)?;
        let identity = fresh.metadata().map_err(|_| UtilityError::InvalidHome)?;
        if identity.kind != FileKind::Directory
            || identity.uid != effective_uid()
            || identity.mode & 0o077 != 0
            || !self.identity.same_identity(&held)
            || !self.identity.same_identity(&identity)
            || self.identity.uid != identity.uid
        {
            return Err(UtilityError::InvalidHome);
        }
        Ok(())
    }
}

pub fn run_utility(
    executor: &dyn Executor,
    operation: UtilityRead,
    repo: &VerifiedRepo,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ExecOutcome, UtilityError> {
    if cancel.is_cancelled() {
        return Err(UtilityError::Cancelled);
    }
    let budget = budget.validate()?;
    if let UtilityRead::GitHub(read) = &operation {
        let binding = match read {
            GitHubRead::PullRequest { repository, .. }
            | GitHubRead::Issue { repository, .. }
            | GitHubRead::Checks { repository, .. }
            | GitHubRead::PullRequestsMerged { repository, .. } => repository,
        };
        if !binding
            .common_identity
            .same_identity(&repo.common_identity())
        {
            return Err(UtilityError::RepositoryBindingMismatch);
        }
    }
    runtime.revalidate()?;
    repo.revalidate()?;
    verify_repo_binding(runtime, repo)?;
    let (program, args, github) = match operation {
        UtilityRead::Git(read) => (&runtime.git, git_args(read, repo)?, false),
        UtilityRead::GitHub(read) => (
            runtime
                .gh
                .as_ref()
                .ok_or(UtilityError::InvalidExecutable { tool: "gh" })?,
            gh_args(read)?,
            true,
        ),
    };
    program.verify(runtime.fs.as_ref())?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.path.as_os_str().to_owned());
    argv.extend(args);
    let env = environment(runtime, repo, github);
    let mut options = ExecOptions::new(&argv, repo.root_path(), budget.timeout);
    options.env = Some(&env);
    options.stop_on_output_limit = true;
    options.stdout = StreamPolicy::Capture {
        limit: budget.stdout_limit,
    };
    options.stderr = StreamPolicy::Capture {
        limit: budget.stderr_limit,
    };
    options.terminate_grace = budget.terminate_grace;
    options.reap_timeout = budget.reap_timeout;
    options.cancel = Some(cancel);
    let outcome = executor.execute(options)?;
    validate_outcome(outcome)
}

fn validate_outcome(outcome: ExecOutcome) -> Result<ExecOutcome, UtilityError> {
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

fn git_args(operation: GitRead, repo: &VerifiedRepo) -> Result<Vec<OsString>, UtilityError> {
    let mut args = vec![
        OsString::from("--git-dir"),
        repo.private_git_dir().as_os_str().to_owned(),
        OsString::from("--work-tree"),
        repo.root_path().as_os_str().to_owned(),
    ];
    args.extend(GIT_COMMON.iter().map(OsString::from));
    let tail = match operation {
        GitRead::Status => vec![
            "status".into(),
            "--porcelain=v2".into(),
            "-z".into(),
            "--branch".into(),
            "--ignore-submodules=none".into(),
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
        GitRead::Diff { base, head, paths } => {
            let mut args = vec![
                "diff".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--no-renames".into(),
                "--no-color".into(),
                "--binary".into(),
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
        GitRead::MergeCommits { since, tip, limit } => {
            validate_limit(limit)?;
            vec![
                "log".into(),
                "--merges".into(),
                "--ancestry-path".into(),
                "--no-decorate".into(),
                "--no-color".into(),
                "--format=%H%x00%P%x00".into(),
                format!("--max-count={limit}").into(),
                format!("{since}..{tip}").into(),
                "--".into(),
            ]
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

fn gh_args(operation: GitHubRead) -> Result<Vec<OsString>, UtilityError> {
    match operation {
        GitHubRead::PullRequest { repository, number } => Ok(vec![
            "pr".into(),
            "view".into(),
            number.0.to_string().into(),
            "--repo".into(),
            repository.qualified_name().into(),
            "--json".into(),
            PR_FIELDS.into(),
        ]),
        GitHubRead::Issue { repository, number } => Ok(vec![
            "issue".into(),
            "view".into(),
            number.0.to_string().into(),
            "--repo".into(),
            repository.qualified_name().into(),
            "--json".into(),
            ISSUE_FIELDS.into(),
        ]),
        GitHubRead::Checks {
            repository,
            pull_request,
        } => Ok(vec![
            "pr".into(),
            "checks".into(),
            pull_request.0.to_string().into(),
            "--repo".into(),
            repository.qualified_name().into(),
            "--json".into(),
            CHECK_FIELDS.into(),
        ]),
        GitHubRead::PullRequestsMerged {
            repository,
            head,
            limit,
        } => {
            if !(1..=MAX_GIT_LOG).contains(&limit) {
                return Err(UtilityError::InvalidOperationValue);
            }
            Ok(vec![
                "pr".into(),
                "list".into(),
                "--repo".into(),
                repository.qualified_name().into(),
                "--state".into(),
                "merged".into(),
                "--head".into(),
                head.short_name().into(),
                "--limit".into(),
                limit.to_string().into(),
                "--json".into(),
                MERGED_PR_FIELDS.into(),
            ])
        }
    }
}

fn environment(
    runtime: &UtilityRuntime,
    repo: &VerifiedRepo,
    github: bool,
) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("HOME".into(), runtime.home.path.as_os_str().to_owned()),
        ("TMPDIR".into(), runtime.home.path.as_os_str().to_owned()),
        (
            "PROVENANT_STATE_DIR".into(),
            runtime.home.path.join(".state").into_os_string(),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            runtime.home.path.join(".config").into_os_string(),
        ),
        (
            "CLAUDE_CONFIG_DIR".into(),
            runtime.home.path.join(".claude").into_os_string(),
        ),
        (
            "CODEX_HOME".into(),
            runtime.home.path.join(".codex").into_os_string(),
        ),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
        ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
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
    if github {
        if let Some(git_parent) = runtime.git.path.parent() {
            let mut path = git_parent.as_os_str().to_owned();
            if let Some(gh_parent) = runtime.gh.as_ref().and_then(|gh| gh.path.parent()) {
                path.push(":");
                path.push(gh_parent.as_os_str());
            }
            env.push(("PATH".into(), path));
        }
        env.extend([
            (
                "GH_CONFIG_DIR".into(),
                runtime.home.path.join(".config/gh").into_os_string(),
            ),
            ("GH_PROMPT_DISABLED".into(), "1".into()),
            ("GH_PAGER".into(), "cat".into()),
            ("GH_NO_UPDATE_NOTIFIER".into(), "1".into()),
        ]);
    }
    env
}

fn verify_repo_binding(runtime: &UtilityRuntime, repo: &VerifiedRepo) -> Result<(), UtilityError> {
    // A VerifiedRepo created with another SafeFs implementation cannot bind a
    // different root or Git database to this Executor cwd.
    let root = runtime.fs.open_anchor(repo.root_path())?;
    let private = runtime.fs.open_anchor(repo.private_git_dir())?;
    let common = runtime.fs.open_anchor(repo.common_git_dir())?;
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
    if cancel.is_cancelled() {
        return Err(UtilityError::Cancelled);
    }
    let budget = budget.validate()?;
    runtime.revalidate()?;
    repo.revalidate()?;
    verify_repo_binding(runtime, repo)?;
    let (args, stdin) = mutation_args(mutation)?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(runtime.git.path.as_os_str().to_owned());
    argv.extend(args);
    let env = environment(runtime, repo, false);
    let mut options = ExecOptions::new(&argv, repo.root_path(), budget.timeout);
    options.env = Some(&env);
    options.stdin = stdin.as_deref();
    options.stdout = StreamPolicy::Capture {
        limit: budget.stdout_limit,
    };
    options.stderr = StreamPolicy::Capture {
        limit: budget.stderr_limit,
    };
    options.stop_on_output_limit = true;
    options.terminate_grace = budget.terminate_grace;
    options.reap_timeout = budget.reap_timeout;
    options.cancel = Some(cancel);
    let outcome = executor.execute(options)?;
    validate_outcome(outcome)
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
                args.push(branch.as_str().into());
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
    use provenant_supervisor::exec::{DEFAULT_CAPTURE_LIMIT, SystemExecutor};
    use provenant_supervisor::fs::LocalFs;
    use std::error::Error;
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    pub(crate) struct Fixture {
        pub(crate) _temp: tempfile::TempDir,
        pub(crate) root: PathBuf,
        pub(crate) git: PathBuf,
        pub(crate) home: PathBuf,
        pub(crate) executor: SystemExecutor,
        pub(crate) runtime: UtilityRuntime,
        pub(crate) repo: VerifiedRepo,
    }

    impl Fixture {
        pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
            let git = PathBuf::from("/usr/bin/git");
            if !git.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "system Git fixture is unavailable",
                )
                .into());
            }
            let temp = tempfile::tempdir_in("/tmp")?;
            let root = temp.path().join("repo");
            let home = temp.path().join("home");
            let state = temp.path().join("state");
            let config = temp.path().join("config");
            let claude = temp.path().join("claude");
            let codex = temp.path().join("codex");
            fs::create_dir(&root)?;
            fs::create_dir(&home)?;
            fs::create_dir(&state)?;
            fs::create_dir(&config)?;
            fs::create_dir(&claude)?;
            fs::create_dir(&codex)?;
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
            let executor = SystemExecutor::new(4)?;
            let env = vec![
                ("HOME".into(), home.as_os_str().to_owned()),
                ("TMPDIR".into(), temp.path().as_os_str().to_owned()),
                ("PROVENANT_STATE_DIR".into(), state.into_os_string()),
                ("XDG_CONFIG_HOME".into(), config.into_os_string()),
                ("CLAUDE_CONFIG_DIR".into(), claude.into_os_string()),
                ("CODEX_HOME".into(), codex.into_os_string()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_CONFIG_COUNT".into(), "0".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GIT_PAGER".into(), "cat".into()),
                ("PAGER".into(), "cat".into()),
                ("LANG".into(), "C".into()),
                ("LC_ALL".into(), "C".into()),
            ];
            run_fixture_git(
                &executor,
                &git,
                temp.path(),
                &env,
                &[
                    "init",
                    "--quiet",
                    root.to_str().ok_or("non-UTF8 test root")?,
                ],
            )?;
            run_fixture_git(
                &executor,
                &git,
                temp.path(),
                &env,
                &[
                    "-C",
                    root.to_str().ok_or("non-UTF8 test root")?,
                    "config",
                    "user.name",
                    "Fixture",
                ],
            )?;
            run_fixture_git(
                &executor,
                &git,
                temp.path(),
                &env,
                &[
                    "-C",
                    root.to_str().ok_or("non-UTF8 test root")?,
                    "config",
                    "user.email",
                    "fixture@example.invalid",
                ],
            )?;
            run_fixture_git(
                &executor,
                &git,
                temp.path(),
                &env,
                &[
                    "-C",
                    root.to_str().ok_or("non-UTF8 test root")?,
                    "commit",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ],
            )?;
            let fs: Arc<dyn SafeFs> = Arc::new(LocalFs);
            let runtime = UtilityRuntime::git_only(fs.clone(), &git, &home)?;
            let repo = runtime.open_repo(&root)?;
            Ok(Self {
                _temp: temp,
                root,
                git,
                home,
                executor,
                runtime,
                repo,
            })
        }

        pub(crate) fn run(&self, args: &[&str]) -> Result<ExecOutcome, Box<dyn Error>> {
            let env = environment(&self.runtime, &self.repo, false);
            run_fixture_git(&self.executor, &self.git, self.repo.root_path(), &env, args)
        }

        pub(crate) fn budget(&self) -> Result<UtilityBudget, UtilityError> {
            UtilityBudget::new(
                Duration::from_secs(10),
                Duration::from_millis(250),
                Duration::from_secs(2),
                DEFAULT_CAPTURE_LIMIT,
                DEFAULT_CAPTURE_LIMIT,
            )
        }

        pub(crate) fn head(&self) -> Result<GitOid, Box<dyn Error>> {
            let outcome = run_utility(
                &self.executor,
                UtilityRead::Git(GitRead::RevParseHead),
                &self.repo,
                &self.runtime,
                self.budget()?,
                &CancelToken::new(),
            )?;
            let text = std::str::from_utf8(&outcome.stdout.bytes)?;
            Ok(GitOid::parse(text.trim())?)
        }
    }

    pub(crate) fn run_fixture_git(
        executor: &SystemExecutor,
        git: &Path,
        cwd: &Path,
        env: &[(OsString, OsString)],
        args: &[&str],
    ) -> Result<ExecOutcome, Box<dyn Error>> {
        let mut argv = vec![git.as_os_str().to_owned()];
        argv.extend(args.iter().map(OsString::from));
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(10));
        options.env = Some(env);
        options.stdout = StreamPolicy::Capture {
            limit: DEFAULT_CAPTURE_LIMIT,
        };
        options.stderr = StreamPolicy::Capture {
            limit: DEFAULT_CAPTURE_LIMIT,
        };
        options.terminate_grace = Duration::from_millis(250);
        options.reap_timeout = Duration::from_secs(2);
        let outcome = executor.execute(options)?;
        if outcome.termination != Termination::Exit(0) || outcome.timed_out || outcome.cancelled {
            return Err(io::Error::other("bounded Git fixture setup failed").into());
        }
        Ok(outcome)
    }

    #[test]
    fn utility_reads_status_and_head_through_the_executor() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let status = run_utility(
            &fixture.executor,
            UtilityRead::Git(GitRead::Status),
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
            provenant_supervisor::fs::canonicalize(&fixture.root)?
        );
        assert_eq!(fixture.runtime.git_program_path(), fixture.git.as_path());
        assert!(fixture.home.is_dir());
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
            UtilityRead::Git(GitRead::Status),
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
        assert!(GitHubRepository::new("owner/name", "repo").is_err());
        assert!(GitHubRepository::new("owner", "repo --help").is_err());
    }
}
