//! Closed, read-only Git and GitHub CLI operations for evidence collection.
//!
//! Callers select a typed operation; this module owns every argument and every
//! child environment entry. Executable pins and the isolated home are supplied
//! by trusted setup code, never by an operation request.
use crate::exec::{ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy};
use provenant_core::{CancelToken, GitOid};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

const DEFAULT_STREAM_LIMIT: usize = 8 * 1024 * 1024;
const MAX_STREAM_LIMIT: usize = 64 * 1024 * 1024;
const DEFAULT_TERMINATE_GRACE: Duration = Duration::from_millis(250);
const DEFAULT_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_UTILITY_TIMEOUT: Duration = Duration::from_secs(14_400);
const MAX_CLEANUP_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum UtilityError {
    #[error("invalid utility repository path")]
    InvalidRepo,
    #[error("invalid utility home directory")]
    InvalidHome,
    #[error("invalid pinned {tool} executable")]
    InvalidExecutable { tool: &'static str },
    #[error("pinned {tool} executable changed after setup")]
    ExecutableChanged { tool: &'static str },
    #[error("utility budget is outside its supported bounds")]
    InvalidBudget,
    #[error("utility operation has an invalid or unsupported ID or limit")]
    InvalidOperationValue,
    #[error("utility {stream} exceeded its capture limit; output cannot be used as evidence")]
    Truncated { stream: &'static str },
    #[error("utility read timed out; output cannot be used as evidence")]
    TimedOut,
    #[error("utility read was cancelled; output cannot be used as evidence")]
    Cancelled,
    #[error("utility capture or transport reported data loss; output cannot be used as evidence")]
    Incomplete,
    #[error(transparent)]
    Execution(#[from] ExecError),
}

/// Canonical directory passed as the child's working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoDir(PathBuf);
impl RepoDir {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, UtilityError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(UtilityError::InvalidRepo);
        }
        let canonical = fs::canonicalize(path).map_err(|_| UtilityError::InvalidRepo)?;
        if !canonical.is_dir() {
            return Err(UtilityError::InvalidRepo);
        }
        let git_metadata =
            fs::symlink_metadata(canonical.join(".git")).map_err(|_| UtilityError::InvalidRepo)?;
        if !(git_metadata.is_file() || git_metadata.is_dir()) {
            return Err(UtilityError::InvalidRepo);
        }
        Ok(Self(canonical))
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Debug)]
struct ExecutablePin {
    path: PathBuf,
    device: u64,
    inode: u64,
    tool: &'static str,
}
impl ExecutablePin {
    fn new(path: impl AsRef<Path>, basename: &'static str) -> Result<Self, UtilityError> {
        let path = path.as_ref();
        if !path.is_absolute() || path.file_name().and_then(|s| s.to_str()) != Some(basename) {
            return Err(UtilityError::InvalidExecutable { tool: basename });
        }
        let path = fs::canonicalize(path)
            .map_err(|_| UtilityError::InvalidExecutable { tool: basename })?;
        let metadata =
            fs::metadata(&path).map_err(|_| UtilityError::InvalidExecutable { tool: basename })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if !metadata.is_file()
                || metadata.permissions().mode() & 0o111 == 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return Err(UtilityError::InvalidExecutable { tool: basename });
            }
            Ok(Self {
                path,
                device: metadata.dev(),
                inode: metadata.ino(),
                tool: basename,
            })
        }
        #[cfg(not(unix))]
        {
            if !metadata.is_file() {
                return Err(UtilityError::InvalidExecutable { tool: basename });
            }
            Ok(Self {
                path,
                device: 0,
                inode: 0,
                tool: basename,
            })
        }
    }
    fn verify(&self) -> Result<(), UtilityError> {
        let metadata = fs::metadata(&self.path)
            .map_err(|_| UtilityError::ExecutableChanged { tool: self.tool })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if !metadata.is_file()
                || metadata.permissions().mode() & 0o111 == 0
                || metadata.permissions().mode() & 0o022 != 0
                || metadata.dev() != self.device
                || metadata.ino() != self.inode
            {
                return Err(UtilityError::ExecutableChanged { tool: self.tool });
            }
        }
        Ok(())
    }
}

/// Absolute executable paths pinned by trusted setup code. No PATH lookup is
/// used, and the resolved file identity is rechecked before each execution.
#[derive(Clone, Debug)]
struct UtilityPrograms {
    git: ExecutablePin,
    gh: Option<ExecutablePin>,
}
impl UtilityPrograms {
    fn pin(git: impl AsRef<Path>, gh: impl AsRef<Path>) -> Result<Self, UtilityError> {
        Ok(Self {
            git: ExecutablePin::new(git, "git")?,
            gh: Some(ExecutablePin::new(gh, "gh")?),
        })
    }
}

/// Fully replaced child home. In tests and service setup this must be a fresh,
/// private directory; the caller decides which GitHub CLI config is authorised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtilityHome(PathBuf);
impl UtilityHome {
    fn new(path: impl AsRef<Path>) -> Result<Self, UtilityError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(UtilityError::InvalidHome);
        }
        let canonical = fs::canonicalize(path).map_err(|_| UtilityError::InvalidHome)?;
        if !canonical.is_dir() {
            return Err(UtilityError::InvalidHome);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&canonical)
                .map_err(|_| UtilityError::InvalidHome)?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err(UtilityError::InvalidHome);
            }
        }
        Ok(Self(canonical))
    }
}

/// A process-scoped set of pinned tools and a private child home. Initialise it
/// from installation policy once, then share it across utility reads.
#[derive(Clone, Debug)]
pub struct UtilityRuntime {
    programs: UtilityPrograms,
    home: UtilityHome,
}
impl UtilityRuntime {
    /// Git evidence does not require an installed GitHub CLI.
    pub fn git_only(git: impl AsRef<Path>, home: impl AsRef<Path>) -> Result<Self, UtilityError> {
        Ok(Self {
            programs: UtilityPrograms {
                git: ExecutablePin::new(git, "git")?,
                gh: None,
            },
            home: UtilityHome::new(home)?,
        })
    }
    pub fn new(
        git: impl AsRef<Path>,
        gh: impl AsRef<Path>,
        home: impl AsRef<Path>,
    ) -> Result<Self, UtilityError> {
        Ok(Self {
            programs: UtilityPrograms::pin(git, gh)?,
            home: UtilityHome::new(home)?,
        })
    }
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
            || stdout_limit > MAX_STREAM_LIMIT
            || stderr_limit > MAX_STREAM_LIMIT
            || timeout
                .checked_add(terminate_grace)
                .and_then(|v| v.checked_add(reap_timeout))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitRead {
    Status,
    RevParseHead,
    Diff {
        base: GitOid,
        head: GitOid,
    },
    Log {
        base: GitOid,
        head: GitOid,
        limit: u64,
    },
    WorktreeList,
    MergeBase {
        left: GitOid,
        right: GitOid,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubRead {
    PullRequest { number: PullRequestNumber },
    Issue { number: IssueNumber },
    Checks { pull_request: PullRequestNumber },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UtilityRead {
    Git(GitRead),
    GitHub(GitHubRead),
}

const GIT_COMMON: &[&str] = &["--no-pager", "--no-optional-locks"];
const GIT_CONFIG_OVERRIDES: [(&str, &str); 6] = [
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("protocol.allow", "never"),
    ("log.showSignature", "false"),
    ("core.attributesFile", "/dev/null"),
    ("status.submoduleSummary", "false"),
];
const PR_FIELDS: &str =
    "number,title,state,baseRefName,baseRefOid,headRefName,headRefOid,url,reviewDecision,isDraft";
const ISSUE_FIELDS: &str = "number,title,state,url,labels,assignees,milestone";
const CHECK_FIELDS: &str = "name,state,link,workflow,startedAt,completedAt";

fn git_args(operation: GitRead) -> Result<Vec<OsString>, UtilityError> {
    let mut args = GIT_COMMON.iter().map(OsString::from).collect::<Vec<_>>();
    let tail = match operation {
        GitRead::Status => vec![
            "status".to_owned(),
            "--porcelain=v2".into(),
            "-z".into(),
            "--branch".into(),
        ],
        GitRead::RevParseHead => vec![
            "rev-parse".into(),
            "--verify".into(),
            "HEAD^{commit}".into(),
        ],
        GitRead::Diff { base, head } => vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
            "--no-color".into(),
            "--binary".into(),
            base.to_string(),
            head.to_string(),
            "--".into(),
        ],
        GitRead::Log { base, head, limit } => {
            if !(1..=1000).contains(&limit) {
                return Err(UtilityError::InvalidOperationValue);
            }
            vec![
                "log".into(),
                "--no-decorate".into(),
                "--no-color".into(),
                "--format=%H%x00%an%x00%at%x00%s%x00".into(),
                format!("--max-count={limit}"),
                format!("{}..{}", base, head),
            ]
        }
        GitRead::WorktreeList => vec![
            "worktree".into(),
            "list".into(),
            "--porcelain".into(),
            "-z".into(),
        ],
        GitRead::MergeBase { left, right } => {
            vec!["merge-base".into(), left.to_string(), right.to_string()]
        }
    };
    args.extend(tail.into_iter().map(OsString::from));
    Ok(args)
}
fn gh_args(operation: GitHubRead) -> Vec<OsString> {
    match operation {
        GitHubRead::PullRequest { number } => vec![
            "pr".into(),
            "view".into(),
            number.0.to_string().into(),
            "--json".into(),
            PR_FIELDS.into(),
        ],
        GitHubRead::Issue { number } => vec![
            "issue".into(),
            "view".into(),
            number.0.to_string().into(),
            "--json".into(),
            ISSUE_FIELDS.into(),
        ],
        GitHubRead::Checks { pull_request } => vec![
            "pr".into(),
            "checks".into(),
            pull_request.0.to_string().into(),
            "--json".into(),
            CHECK_FIELDS.into(),
        ],
    }
}

fn environment(home: &UtilityHome, git: &ExecutablePin, github: bool) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("HOME".into(), home.0.as_os_str().to_owned()),
        (
            "XDG_CONFIG_HOME".into(),
            home.0.join(".config").into_os_string(),
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
    ];
    env.push((
        "GIT_CONFIG_COUNT".into(),
        GIT_CONFIG_OVERRIDES.len().to_string().into(),
    ));
    for (index, (key, value)) in GIT_CONFIG_OVERRIDES.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{index}").into(), (*key).into()));
        env.push((format!("GIT_CONFIG_VALUE_{index}").into(), (*value).into()));
    }
    env.push(("GIT_NO_LAZY_FETCH".into(), "1".into()));
    if github {
        if let Some(parent) = git.path.parent() {
            env.push(("PATH".into(), parent.as_os_str().to_owned()));
        }
        env.extend([
            (
                "GH_CONFIG_DIR".into(),
                home.0.join(".config/gh").into_os_string(),
            ),
            ("GH_PROMPT_DISABLED".into(), "1".into()),
            ("GH_PAGER".into(), "cat".into()),
            ("GH_NO_UPDATE_NOTIFIER".into(), "1".into()),
        ]);
    }
    env
}

/// Execute one bounded read operation. This accepts no caller-supplied argv,
/// shell, GitHub field list, or write operation. Incomplete captures are errors.
pub fn run_utility(
    executor: &dyn Executor,
    operation: UtilityRead,
    repo: &RepoDir,
    runtime: &UtilityRuntime,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<ExecOutcome, UtilityError> {
    if cancel.is_cancelled() {
        return Err(UtilityError::Execution(ExecError::Cancelled));
    }
    let (program, args, github) = match operation {
        UtilityRead::Git(read) => (&runtime.programs.git, git_args(read)?, false),
        UtilityRead::GitHub(read) => (
            runtime
                .programs
                .gh
                .as_ref()
                .ok_or(UtilityError::InvalidExecutable { tool: "gh" })?,
            gh_args(read),
            true,
        ),
    };
    program.verify()?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.path.as_os_str().to_owned());
    argv.extend(args);
    let env = environment(&runtime.home, &runtime.programs.git, github);
    let mut options = ExecOptions::new(&argv, repo.as_path(), budget.timeout);
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
    if outcome.timed_out {
        return Err(UtilityError::TimedOut);
    }
    if outcome.cancelled {
        return Err(UtilityError::Cancelled);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{CapturedStream, Termination};
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Invocation {
        argv: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
        cwd: PathBuf,
        timeout: Duration,
        grace: Duration,
        reap: Duration,
        captured: bool,
        cancelled: bool,
    }
    #[derive(Default)]
    struct FakeExecutor(Mutex<Option<Invocation>>);
    impl Executor for FakeExecutor {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            assert!(options.stop_on_output_limit);
            let (captured, cancelled) = (
                matches!(&options.stdout, StreamPolicy::Capture { .. })
                    && matches!(&options.stderr, StreamPolicy::Capture { .. }),
                options.cancel.is_some(),
            );
            *self.0.lock().map_err(|_| ExecError::Cancelled)? = Some(Invocation {
                argv: options.argv.to_vec(),
                env: options.env.unwrap_or_default().to_vec(),
                cwd: options.cwd.to_path_buf(),
                timeout: options.timeout,
                grace: options.terminate_grace,
                reap: options.reap_timeout,
                captured,
                cancelled,
            });
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: b"ok".to_vec(),
                    total_bytes: 2,
                    truncated: false,
                    ..CapturedStream::default()
                },
                stderr: CapturedStream {
                    bytes: Vec::new(),
                    total_bytes: 0,
                    truncated: false,
                    ..CapturedStream::default()
                },
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        repo: RepoDir,
        home_path: PathBuf,
        bin_path: PathBuf,
        runtime: UtilityRuntime,
    }
    impl Fixture {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let repo_path = root.path().join("repo");
            let home_path = root.path().join("home");
            let bin_path = root.path().join("bin");
            fs::create_dir(&repo_path).unwrap();
            fs::create_dir(repo_path.join(".git")).unwrap();
            fs::create_dir(&home_path).unwrap();
            fs::set_permissions(&home_path, fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir(&bin_path).unwrap();
            fs::write(bin_path.join("git"), b"fixture git binary").unwrap();
            fs::write(bin_path.join("gh"), b"fixture gh binary").unwrap();
            for path in [bin_path.join("git"), bin_path.join("gh")] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self {
                repo: RepoDir::new(repo_path).unwrap(),
                home_path: fs::canonicalize(&home_path).unwrap(),
                bin_path: fs::canonicalize(&bin_path).unwrap(),
                runtime: UtilityRuntime::new(bin_path.join("git"), bin_path.join("gh"), home_path)
                    .unwrap(),
                _root: root,
            }
        }
    }
    fn budget() -> UtilityBudget {
        UtilityBudget::bounded(Duration::from_secs(5)).unwrap()
    }
    fn text(value: &OsString) -> &str {
        value.to_str().unwrap()
    }
    fn env_value<'a>(call: &'a Invocation, key: &str) -> &'a str {
        call.env
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.to_str())
            .unwrap()
    }

    #[test]
    fn git_and_gh_requests_expand_to_fixed_commands_and_clean_env() {
        let fixture = Fixture::new();
        let fake = FakeExecutor::default();
        let cancel = CancelToken::new();
        let oid_a = GitOid::parse("0123456789012345678901234567890123456789").unwrap();
        let oid_b = GitOid::parse("abcdefabcdefabcdefabcdefabcdefabcdefabcd").unwrap();
        let reads = [
            (
                UtilityRead::Git(GitRead::Status),
                vec![
                    "git",
                    "--no-pager",
                    "--no-optional-locks",
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--branch",
                ],
            ),
            (
                UtilityRead::Git(GitRead::RevParseHead),
                vec![
                    "git",
                    "--no-pager",
                    "--no-optional-locks",
                    "rev-parse",
                    "--verify",
                    "HEAD^{commit}",
                ],
            ),
            (
                UtilityRead::Git(GitRead::Diff {
                    base: oid_a,
                    head: oid_b,
                }),
                vec![
                    "git",
                    "--no-pager",
                    "--no-optional-locks",
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    "--no-color",
                    "--binary",
                    "0123456789012345678901234567890123456789",
                    "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
                    "--",
                ],
            ),
            (
                UtilityRead::Git(GitRead::Log {
                    base: oid_a,
                    head: oid_b,
                    limit: 20,
                }),
                vec![
                    "git",
                    "--no-pager",
                    "--no-optional-locks",
                    "log",
                    "--no-decorate",
                    "--no-color",
                    "--format=%H%x00%an%x00%at%x00%s%x00",
                    "--max-count=20",
                    "0123456789012345678901234567890123456789..abcdefabcdefabcdefabcdefabcdefabcdefabcd",
                ],
            ),
            (
                UtilityRead::Git(GitRead::WorktreeList),
                vec![
                    "git",
                    "--no-pager",
                    "--no-optional-locks",
                    "worktree",
                    "list",
                    "--porcelain",
                    "-z",
                ],
            ),
            (
                UtilityRead::Git(GitRead::MergeBase {
                    left: oid_a,
                    right: oid_b,
                }),
                vec![
                    "git",
                    "--no-pager",
                    "--no-optional-locks",
                    "merge-base",
                    "0123456789012345678901234567890123456789",
                    "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
                ],
            ),
            (
                UtilityRead::GitHub(GitHubRead::PullRequest {
                    number: PullRequestNumber::new(12).unwrap(),
                }),
                vec!["gh", "pr", "view", "12", "--json", PR_FIELDS],
            ),
            (
                UtilityRead::GitHub(GitHubRead::Issue {
                    number: IssueNumber::new(34).unwrap(),
                }),
                vec!["gh", "issue", "view", "34", "--json", ISSUE_FIELDS],
            ),
            (
                UtilityRead::GitHub(GitHubRead::Checks {
                    pull_request: PullRequestNumber::new(12).unwrap(),
                }),
                vec!["gh", "pr", "checks", "12", "--json", CHECK_FIELDS],
            ),
        ];
        for (read, expected) in reads {
            run_utility(
                &fake,
                read,
                &fixture.repo,
                &fixture.runtime,
                budget(),
                &cancel,
            )
            .unwrap();
            let call = fake.0.lock().unwrap().take().unwrap();
            assert_eq!(Path::new(&call.argv[0]), fixture.bin_path.join(expected[0]));
            assert_eq!(
                call.argv.iter().skip(1).map(text).collect::<Vec<_>>(),
                expected[1..]
            );
            assert_eq!(call.cwd, fixture.repo.as_path());
            assert_eq!(call.timeout, Duration::from_secs(5));
            assert!(call.captured && call.cancelled);
            assert_eq!(call.grace, DEFAULT_TERMINATE_GRACE);
            assert_eq!(call.reap, DEFAULT_REAP_TIMEOUT);
            assert!(
                !call
                    .argv
                    .iter()
                    .any(|arg| matches!(text(arg), "-c" | "sh" | "bash"))
            );
            assert_eq!(
                call.env
                    .iter()
                    .filter(|(key, _)| key.as_os_str() == "HOME")
                    .count(),
                1
            );
            assert!(!call.env.iter().any(|(key, _)| matches!(
                key.to_str(),
                Some("GITHUB_TOKEN" | "GH_TOKEN" | "GH_ENTERPRISE_TOKEN")
            )));
            let mut actual_keys = call
                .env
                .iter()
                .map(|(key, _)| key.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            actual_keys.sort();
            let mut expected_keys = [
                "HOME",
                "XDG_CONFIG_HOME",
                "LANG",
                "LC_ALL",
                "GIT_CONFIG_NOSYSTEM",
                "GIT_CONFIG_SYSTEM",
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_COUNT",
                "GIT_OPTIONAL_LOCKS",
                "GIT_NO_REPLACE_OBJECTS",
                "GIT_NO_LAZY_FETCH",
                "GIT_ATTR_NOSYSTEM",
                "GIT_TERMINAL_PROMPT",
                "GIT_PAGER",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            for index in 0..GIT_CONFIG_OVERRIDES.len() {
                expected_keys.push(format!("GIT_CONFIG_KEY_{index}"));
                expected_keys.push(format!("GIT_CONFIG_VALUE_{index}"));
            }
            if matches!(read, UtilityRead::GitHub(_)) {
                expected_keys.extend(
                    [
                        "PATH",
                        "GH_CONFIG_DIR",
                        "GH_PROMPT_DISABLED",
                        "GH_PAGER",
                        "GH_NO_UPDATE_NOTIFIER",
                    ]
                    .into_iter()
                    .map(str::to_owned),
                );
            }
            expected_keys.sort();
            assert_eq!(actual_keys, expected_keys);
            assert_eq!(env_value(&call, "GIT_CONFIG_COUNT"), "6");
            assert_eq!(env_value(&call, "GIT_NO_LAZY_FETCH"), "1");
            for (index, (key, value)) in GIT_CONFIG_OVERRIDES.iter().enumerate() {
                assert_eq!(env_value(&call, &format!("GIT_CONFIG_KEY_{index}")), *key);
                assert_eq!(
                    env_value(&call, &format!("GIT_CONFIG_VALUE_{index}")),
                    *value
                );
            }
            let home = call
                .env
                .iter()
                .find(|(key, _)| key.as_os_str() == "HOME")
                .unwrap();
            assert_eq!(Path::new(&home.1), fixture.home_path);
        }
    }

    #[test]
    fn rejects_unbounded_or_invalid_operations_before_executor() {
        assert!(matches!(
            PullRequestNumber::new(0),
            Err(UtilityError::InvalidOperationValue)
        ));
        assert!(matches!(
            IssueNumber::new(0),
            Err(UtilityError::InvalidOperationValue)
        ));
        assert!(matches!(
            UtilityBudget::bounded(Duration::ZERO),
            Err(UtilityError::InvalidBudget)
        ));
        assert!(matches!(
            UtilityBudget::new(
                Duration::from_secs(1),
                Duration::ZERO,
                Duration::ZERO,
                MAX_STREAM_LIMIT + 1,
                1
            ),
            Err(UtilityError::InvalidBudget)
        ));
        let fixture = Fixture::new();
        let fake = FakeExecutor::default();
        let cancel = CancelToken::new();
        let invalid = UtilityRead::Git(GitRead::Log {
            base: GitOid::parse("0123456789012345678901234567890123456789").unwrap(),
            head: GitOid::parse("abcdefabcdefabcdefabcdefabcdefabcdefabcd").unwrap(),
            limit: 0,
        });
        assert!(matches!(
            run_utility(
                &fake,
                invalid,
                &fixture.repo,
                &fixture.runtime,
                budget(),
                &cancel
            ),
            Err(UtilityError::InvalidOperationValue)
        ));
        assert!(fake.0.lock().unwrap().is_none());
    }

    #[test]
    fn rejects_relative_repo_and_pre_cancelled_call() {
        assert!(matches!(
            RepoDir::new("relative"),
            Err(UtilityError::InvalidRepo)
        ));
        let fixture = Fixture::new();
        let fake = FakeExecutor::default();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            run_utility(
                &fake,
                UtilityRead::Git(GitRead::Status),
                &fixture.repo,
                &fixture.runtime,
                budget(),
                &cancel
            ),
            Err(UtilityError::Execution(ExecError::Cancelled))
        ));
        assert!(fake.0.lock().unwrap().is_none());
    }

    #[test]
    fn refuses_truncated_evidence() {
        struct Truncated;
        impl Executor for Truncated {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: CapturedStream {
                        bytes: b"partial".to_vec(),
                        total_bytes: 99,
                        truncated: true,
                        ..CapturedStream::default()
                    },
                    stderr: CapturedStream {
                        bytes: Vec::new(),
                        total_bytes: 0,
                        truncated: false,
                        ..CapturedStream::default()
                    },
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        let fixture = Fixture::new();
        let result = run_utility(
            &Truncated,
            UtilityRead::Git(GitRead::Status),
            &fixture.repo,
            &fixture.runtime,
            budget(),
            &CancelToken::new(),
        );
        assert!(matches!(
            result,
            Err(UtilityError::Truncated { stream: "stdout" })
        ));
    }
}

#[cfg(test)]
mod external_contract_tests {
    use super::*;
    use crate::exec::{CapturedStream, Termination};
    use serde::Deserialize;
    #[derive(Deserialize)]
    struct GitRecording {
        recorded_with: String,
        platform: String,
        setup: String,
        argv: Vec<String>,
        exit_code: i64,
        stdout: String,
        stderr: String,
    }
    struct RecordedGit(GitRecording);
    impl Executor for RecordedGit {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let args: Vec<_> = options
                .argv
                .iter()
                .skip(1)
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(args, self.0.argv);
            Ok(ExecOutcome {
                termination: Termination::Exit(self.0.exit_code),
                stdout: CapturedStream {
                    bytes: self.0.stdout.as_bytes().to_vec(),
                    total_bytes: self.0.stdout.len() as u64,
                    ..Default::default()
                },
                stderr: CapturedStream {
                    bytes: self.0.stderr.as_bytes().to_vec(),
                    total_bytes: self.0.stderr.len() as u64,
                    ..Default::default()
                },
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 1,
            })
        }
    }
    #[test]
    fn recorded_system_git_nul_status_is_preserved_as_bytes() {
        let recording: GitRecording = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/git-read.json"
        )))
        .unwrap();
        assert!(recording.recorded_with.starts_with("git version "));
        assert!(!recording.platform.is_empty() && !recording.setup.is_empty());
        let expected = recording.stdout.as_bytes().to_vec();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let repo = RepoDir::new(root.path()).unwrap();
        let git = root.path().join("git");
        std::fs::write(&git, b"fixture executable identity").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = UtilityRuntime::git_only(git, root.path()).unwrap();
        let outcome = run_utility(
            &RecordedGit(recording),
            UtilityRead::Git(GitRead::Status),
            &repo,
            &runtime,
            UtilityBudget::bounded(Duration::from_secs(1)).unwrap(),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(outcome.stdout.bytes, expected);
        assert_eq!(outcome.termination, Termination::Exit(0));
    }
}
