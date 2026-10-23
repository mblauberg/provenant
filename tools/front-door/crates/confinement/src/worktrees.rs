//! Repository-scoped linked-worktree lifecycle and merged-ref pruning.
//!
//! This artifact is intended to be applied after `repo.rs` and `utility.rs`
//! from `/tmp/s3-utility`. All Git effects use their closed typed utility API;
//! all filesystem effects use supervisor safe handles.
#![forbid(unsafe_code)]

use crate::repo::{GitRefName, RepoError, VerifiedRepo, VerifiedWorktree, verify_worktree};
use crate::utility::{
    GitHubRead, GitHubRepositoryBinding, GitMutation, GitRead, UtilityBudget, UtilityError,
    UtilityRead, UtilityRuntime, execute_git_mutation, run_utility,
};
use provenant_core::Clock;
use provenant_core::{CancelToken, GitOid, RelativePath, WaitBudget, hex};
use provenant_records::{PathWire, PathWireError};
use provenant_supervisor::census::{
    ProcessFact, ProcessObservation, ProcessObserver, SystemProcessObserver, current_process,
};
use provenant_supervisor::exec::{ExecOutcome, Executor, Termination};
use provenant_supervisor::fs::{
    FsError, LockGuard, LockMode, Metadata, PublishMode, SafeDir, SafeFs, canonicalize,
};
use provenant_supervisor::pid::{CwdObservation, CwdObserver, SystemCwdObserver};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_WORKTREES: usize = 10_000;
const MAX_BRANCHES: usize = 10_000;
const MAX_MERGE_COMMITS: u64 = 1_000;
const MAX_STATE_RECORD: u64 = 64 * 1024;
const REPOSITORY_LOCK: &str = "worktrees.lock";
const WRITER_LOCK: &str = "writer.lock";
const RESERVATION: &str = "reservation.json";
const IDENTITY: &str = "identity.json";
const WORKTREE_ROOT: &str = ".worktrees";

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("worktree request is invalid")]
    InvalidRequest,
    #[error("worktree or branch was not found")]
    NotFound,
    #[error("worktree is dirty")]
    Dirty,
    #[error("worktree writer is live")]
    LiveWriter,
    #[error("worktree writer or registration is unknown")]
    UnknownWriter,
    #[error("worktree operation was cancelled")]
    Cancelled,
    #[error("explicit GitHub repository identity is unavailable")]
    MissingGitHubRepository,
    #[error("worktree branch is checked out")]
    CheckedOut,
    #[error("worktree is the primary repository")]
    Primary,
    #[error("worktree Git output is malformed or incomplete")]
    InvalidOutput,
    #[error("worktree was created but registration publication is unconfirmed")]
    RegistrationUnconfirmed,
    #[error("worktree identity changed during the operation")]
    IdentityChanged,
    #[error("worktree result path cannot be represented safely")]
    PathEncoding,
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Utility(#[from] UtilityError),
    #[error(transparent)]
    Repository(#[from] RepoError),
    #[error(transparent)]
    PathWire(#[from] PathWireError),
}

/// Dependencies at the actual OS and utility seams. The project directory is
/// supplied by the caller as `state/worktrees/<ProjectId>`; confinement does
/// not resolve roots or create another filesystem abstraction.
pub struct WorktreeContext<'a> {
    pub repo: &'a VerifiedRepo,
    pub project_state: &'a dyn SafeDir,
    pub fs: &'a dyn SafeFs,
    pub executor: &'a dyn Executor,
    pub utility: &'a UtilityRuntime,
    /// Explicit project binding for gh. `None` disables squash-merge proof;
    /// worktrees never infer repository identity from a local remote.
    pub github_repository: Option<GitHubRepositoryBinding>,
    pub workers: &'a dyn WorkerObserver,
    pub clock: &'a dyn Clock,
    pub utility_budget: UtilityBudget,
    pub lock_budget: WaitBudget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeCreate {
    pub branch: String,
    pub base: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PruneScope {
    Branches(Vec<String>),
    Since(GitOid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeRemove {
    Name(String),
    Merged {
        integration: String,
        scope: PruneScope,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCondition {
    Clean,
    Dirty,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterState {
    Free,
    Live,
    Unknown,
}

/// CLI result facts are serialize-only. Its absolute path is emitted through
/// the explicit root-plus-PathWire envelope below, never lossy `Path::display`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: GitOid,
    pub dirty: WorktreeCondition,
    pub writer: WriterState,
}

impl Serialize for WorktreeInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let path = AbsolutePathWire::from_path(&self.path).map_err(serde::ser::Error::custom)?;
        let mut state = serializer.serialize_struct("WorktreeInfo", 5)?;
        state.serialize_field("path", &path)?;
        state.serialize_field("branch", &self.branch)?;
        state.serialize_field("head", &self.head)?;
        state.serialize_field("dirty", &self.dirty)?;
        state.serialize_field("writer", &self.writer)?;
        state.end()
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AbsolutePathWire {
    Root,
    Path {
        root: &'static str,
        relative: PathWire,
    },
}

impl AbsolutePathWire {
    fn from_path(path: &Path) -> Result<Self, PathWireError> {
        if !path.is_absolute() {
            return Err(PathWireError::InvalidNativePath);
        }
        let relative = path
            .strip_prefix(Path::new("/"))
            .map_err(|_| PathWireError::InvalidNativePath)?;
        if relative.as_os_str().is_empty() {
            return Ok(Self::Root);
        }
        let relative =
            RelativePath::parse(relative).map_err(|_| PathWireError::InvalidNativePath)?;
        Ok(Self::Path {
            root: "/",
            relative: PathWire::from_relative(&relative)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetainedBranch {
    pub branch: String,
    pub reason: RetainReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetainReason {
    IntegrationRef,
    Primary,
    AtIntegrationTip,
    NotMerged,
    OutsideSinceRange,
    CheckedOut,
    Dirty,
    LiveWriter,
    UnknownWriter,
    Changed,
    GitFailure,
    Unregistered,
    LockedOrPrunable,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize)]
pub struct RemoveOutcome {
    pub removed: Vec<WorktreeInfo>,
    pub pruned_refs: Vec<String>,
    pub retained: Vec<RetainedBranch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerState {
    Free,
    Live,
    Unknown,
}

/// Injectable native observation seam. Implementations return one conservative
/// state per canonical candidate root; they never grant signal authority.
pub trait WorkerObserver: Send + Sync {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWorkerObserver;

impl WorkerObserver for SystemWorkerObserver {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        let mut states: BTreeMap<_, _> = roots
            .iter()
            .cloned()
            .map(|root| (root, WorkerState::Free))
            .collect();
        if roots.is_empty() {
            return states;
        }
        let current = match current_process() {
            ProcessObservation::Present(fact) => fact.identity,
            ProcessObservation::Absent | ProcessObservation::Unknown(_) => {
                return roots
                    .iter()
                    .cloned()
                    .map(|root| (root, WorkerState::Unknown))
                    .collect();
            }
        };
        let observer = SystemProcessObserver;
        let census = match observer.census(65_536) {
            Ok(census) if census.is_complete() => census,
            Ok(_) | Err(_) => {
                return roots
                    .iter()
                    .cloned()
                    .map(|root| (root, WorkerState::Unknown))
                    .collect();
            }
        };
        let cwd_observer = SystemCwdObserver;
        let mut unknown_all = false;
        for fact in census.facts() {
            if fact.identity == current {
                continue;
            }
            match observe_same_process(&observer, &cwd_observer, fact) {
                NativeCwd::Gone => {}
                NativeCwd::Unknown => unknown_all = true,
                NativeCwd::Path(path) => {
                    let canonical = match canonicalize(&path) {
                        Ok(path) => path,
                        Err(_) => {
                            unknown_all = true;
                            continue;
                        }
                    };
                    if let Some(root) = longest_containing_root(roots, &canonical)
                        && let Some(state) = states.get_mut(root)
                    {
                        *state = WorkerState::Live;
                    }
                }
            }
        }
        if unknown_all {
            for state in states.values_mut() {
                if *state != WorkerState::Live {
                    *state = WorkerState::Unknown;
                }
            }
        }
        states
    }
}

enum NativeCwd {
    Path(PathBuf),
    Gone,
    Unknown,
}

fn observe_same_process(
    process_observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    expected: &ProcessFact,
) -> NativeCwd {
    match process_observer.observe(expected.identity.pid) {
        ProcessObservation::Present(before) if before.identity == expected.identity => {}
        ProcessObservation::Absent => return NativeCwd::Gone,
        ProcessObservation::Present(_) | ProcessObservation::Unknown(_) => {
            return NativeCwd::Unknown;
        }
    }
    let observed = match cwd_observer.cwd(expected.identity.pid) {
        Ok(CwdObservation::Path(path)) => NativeCwd::Path(path),
        Ok(CwdObservation::Gone) => return NativeCwd::Gone,
        Ok(CwdObservation::PermissionDenied | CwdObservation::Unsupported) | Err(_) => {
            return NativeCwd::Unknown;
        }
    };
    match process_observer.observe(expected.identity.pid) {
        ProcessObservation::Present(after) if after.identity == expected.identity => observed,
        ProcessObservation::Absent => NativeCwd::Gone,
        ProcessObservation::Present(_) | ProcessObservation::Unknown(_) => NativeCwd::Unknown,
    }
}

fn longest_containing_root<'a>(roots: &'a [PathBuf], cwd: &Path) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| cwd == root.as_path() || cwd.starts_with(root))
        .max_by_key(|root| root.components().count())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitWorktree {
    path: PathBuf,
    head: GitOid,
    branch: Option<GitRefName>,
    locked: bool,
    prunable: bool,
    bare: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeIdentity {
    schema_version: u64,
    active: bool,
    common_device: u64,
    common_inode: u64,
    root_device: u64,
    root_inode: u64,
    path_digest: String,
    branch: String,
    base: GitOid,
}

fn parse_worktree_list(bytes: &[u8]) -> Result<Vec<GitWorktree>, WorktreeError> {
    let mut result = Vec::new();
    let mut fields: Vec<Vec<u8>> = Vec::new();
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if !fields.is_empty() {
                result.push(parse_worktree_record(&fields)?);
                fields.clear();
                if result.len() > MAX_WORKTREES {
                    return Err(WorktreeError::InvalidOutput);
                }
            }
        } else {
            fields.push(field.to_vec());
        }
    }
    if !fields.is_empty() {
        result.push(parse_worktree_record(&fields)?);
    }
    if result.len() > MAX_WORKTREES {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(result)
}

fn parse_worktree_record(fields: &[Vec<u8>]) -> Result<GitWorktree, WorktreeError> {
    let mut path = None;
    let mut head = None;
    let mut branch = None;
    let mut detached = false;
    let mut locked = false;
    let mut prunable = false;
    let mut bare = false;
    for field in fields {
        if let Some(value) = field.strip_prefix(b"worktree ") {
            if path.is_some() || value.is_empty() || value.first() != Some(&b'/') {
                return Err(WorktreeError::InvalidOutput);
            }
            path = Some(PathBuf::from(OsString::from_vec(value.to_vec())));
        } else if let Some(value) = field.strip_prefix(b"HEAD ") {
            if head.is_some() {
                return Err(WorktreeError::InvalidOutput);
            }
            head = Some(parse_oid(value)?);
        } else if let Some(value) = field.strip_prefix(b"branch ") {
            if branch.is_some() || detached || bare {
                return Err(WorktreeError::InvalidOutput);
            }
            let text = std::str::from_utf8(value).map_err(|_| WorktreeError::InvalidOutput)?;
            branch = Some(GitRefName::new(text).map_err(|_| WorktreeError::InvalidOutput)?);
        } else if field.as_slice() == b"detached" {
            if detached || branch.is_some() {
                return Err(WorktreeError::InvalidOutput);
            }
            detached = true;
        } else if field.as_slice() == b"bare" {
            if bare {
                return Err(WorktreeError::InvalidOutput);
            }
            bare = true;
        } else if field.as_slice() == b"locked" || field.starts_with(b"locked ") {
            if locked {
                return Err(WorktreeError::InvalidOutput);
            }
            locked = true;
        } else if field.as_slice() == b"prunable" || field.starts_with(b"prunable ") {
            if prunable {
                return Err(WorktreeError::InvalidOutput);
            }
            prunable = true;
        } else {
            return Err(WorktreeError::InvalidOutput);
        }
    }
    let path = path.ok_or(WorktreeError::InvalidOutput)?;
    let head = head.ok_or(WorktreeError::InvalidOutput)?;
    if !detached && !bare && branch.is_none() {
        return Err(WorktreeError::InvalidOutput);
    }
    if bare && branch.is_some() {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(GitWorktree {
        path,
        head,
        branch,
        locked,
        prunable,
        bare,
    })
}

fn parse_oid(bytes: &[u8]) -> Result<GitOid, WorktreeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    GitOid::parse(text).map_err(|_| WorktreeError::InvalidOutput)
}

fn parse_branch_tips(bytes: &[u8]) -> Result<BTreeMap<GitRefName, GitOid>, WorktreeError> {
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut result = BTreeMap::new();
    while let Some(name) = fields.next() {
        if trim_ascii(name).is_empty() {
            continue;
        }
        let oid = fields.next().ok_or(WorktreeError::InvalidOutput)?;
        let name =
            std::str::from_utf8(trim_ascii(name)).map_err(|_| WorktreeError::InvalidOutput)?;
        let name = GitRefName::new(name).map_err(|_| WorktreeError::InvalidOutput)?;
        let oid = parse_oid(trim_ascii(oid))?;
        if result.insert(name, oid).is_some() || result.len() > MAX_BRANCHES {
            return Err(WorktreeError::InvalidOutput);
        }
    }
    Ok(result)
}

fn parse_merge_commits(bytes: &[u8]) -> Result<Vec<GitOid>, WorktreeError> {
    let mut result = Vec::new();
    for record in bytes.split(|byte| *byte == b'\n') {
        if record.is_empty() {
            continue;
        }
        let oid = record
            .split(|byte| *byte == 0)
            .next()
            .ok_or(WorktreeError::InvalidOutput)?;
        result.push(parse_oid(oid)?);
        if result.len() > MAX_MERGE_COMMITS as usize {
            return Err(WorktreeError::InvalidOutput);
        }
    }
    Ok(result)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    bytes.get(start..end).unwrap_or_default()
}

fn exit_code(outcome: &ExecOutcome) -> Result<i64, WorktreeError> {
    match outcome.termination {
        Termination::Exit(code) => Ok(code),
        Termination::Signal(_) => Err(WorktreeError::InvalidOutput),
    }
}

fn output_bytes(outcome: ExecOutcome) -> Result<Vec<u8>, WorktreeError> {
    if outcome.timed_out
        || outcome.drain_truncated
        || outcome.overflow_stopped
        || outcome.cancelled
        || outcome.stdout.truncated
        || outcome.stderr.truncated
        || outcome.stdout.read_failed
        || outcome.stderr.read_failed
        || outcome.stdout.sink_failed
        || outcome.stderr.sink_failed
        || outcome.stdout.sink_lost_bytes != 0
        || outcome.stderr.sink_lost_bytes != 0
        || outcome.stdout.oversized_frames != 0
        || outcome.stderr.oversized_frames != 0
        || outcome.stdout.decoder_lost_frames != 0
        || outcome.stderr.decoder_lost_frames != 0
        || outcome.stdout.filter_failed
        || outcome.stderr.filter_failed
        || outcome.stdout.total_bytes != outcome.stdout.bytes.len() as u64
        || outcome.stderr.total_bytes != outcome.stderr.bytes.len() as u64
    {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(outcome.stdout.bytes)
}

fn run_git(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    read: GitRead,
    cancel: &CancelToken,
) -> Result<ExecOutcome, WorktreeError> {
    run_utility(
        context.executor,
        UtilityRead::Git(read),
        repo,
        context.utility,
        context.utility_budget,
        cancel,
    )
    .map_err(WorktreeError::from)
}

fn read_worktrees(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Vec<GitWorktree>, WorktreeError> {
    let outcome = run_git(context, context.repo, GitRead::WorktreeList, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    parse_worktree_list(&output_bytes(outcome)?)
}

fn read_branch_tips(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<BTreeMap<GitRefName, GitOid>, WorktreeError> {
    let outcome = run_git(context, context.repo, GitRead::BranchTips, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    parse_branch_tips(&output_bytes(outcome)?)
}

fn resolve_ref(
    context: &WorktreeContext<'_>,
    name: GitRefName,
    cancel: &CancelToken,
) -> Result<GitOid, WorktreeError> {
    let outcome = run_git(context, context.repo, GitRead::ResolveRef { name }, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::NotFound);
    }
    let bytes = output_bytes(outcome)?;
    parse_oid(trim_ascii(&bytes))
}

fn is_ancestor(
    context: &WorktreeContext<'_>,
    ancestor: GitOid,
    tip: GitOid,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    let outcome = run_git(
        context,
        context.repo,
        GitRead::MergeBaseIsAncestor { ancestor, tip },
        cancel,
    )?;
    match exit_code(&outcome)? {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(WorktreeError::InvalidOutput),
    }
}

fn canonical_worktree_paths(
    entries: &[GitWorktree],
) -> Result<Vec<(PathBuf, &GitWorktree)>, WorktreeError> {
    entries
        .iter()
        .filter(|entry| !entry.bare)
        .map(|entry| Ok((canonicalize(&entry.path)?, entry)))
        .collect()
}

fn path_digest(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_bytes());
    hex(&digest)
}

fn state_dir_name(path: &Path) -> OsString {
    path_digest(path).into()
}

fn identity_for(
    context: &WorktreeContext<'_>,
    path: &Path,
    branch: &GitRefName,
    base: GitOid,
    root_identity: Metadata,
) -> WorktreeIdentity {
    let common = context.repo.common_identity();
    WorktreeIdentity {
        schema_version: 1,
        active: true,
        common_device: common.device,
        common_inode: common.inode,
        root_device: root_identity.device,
        root_inode: root_identity.inode,
        path_digest: path_digest(path),
        branch: branch.as_str().to_owned(),
        base,
    }
}

fn read_bounded(directory: &dyn SafeDir, name: &OsStr, limit: u64) -> Result<Vec<u8>, FsError> {
    let mut file = directory.open_read(name)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| FsError::Io {
            operation: "read worktree state",
            path: name.into(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(FsError::TooLarge { limit });
    }
    Ok(bytes)
}

fn write_identity(
    directory: &dyn SafeDir,
    identity: &WorktreeIdentity,
    mode: PublishMode,
) -> Result<(), WorktreeError> {
    let bytes = serde_json::to_vec(identity).map_err(|_| WorktreeError::InvalidOutput)?;
    match directory.atomic_write(OsStr::new(IDENTITY), ".identity-", &bytes, mode)? {
        provenant_supervisor::fs::Publication::Durable => Ok(()),
        provenant_supervisor::fs::Publication::VisibleDurabilityUnconfirmed { .. } => {
            Err(WorktreeError::RegistrationUnconfirmed)
        }
    }
}

fn writer_state(
    context: &WorktreeContext<'_>,
    path: &Path,
    branch: &GitRefName,
    root_identity: Metadata,
    cancel: &CancelToken,
) -> WorkerState {
    let name = state_dir_name(path);
    let directory = match context.project_state.open_verified_directory(&name) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return WorkerState::Unknown,
        Err(_) => return WorkerState::Unknown,
    };
    let bytes = match read_bounded(directory.as_ref(), OsStr::new(IDENTITY), MAX_STATE_RECORD) {
        Ok(bytes) => bytes,
        Err(_) => return WorkerState::Unknown,
    };
    let identity: WorktreeIdentity = match serde_json::from_slice(&bytes) {
        Ok(identity) => identity,
        Err(_) => return WorkerState::Unknown,
    };
    let common = context.repo.common_identity();
    if identity.schema_version != 1
        || !identity.active
        || identity.common_device != common.device
        || identity.common_inode != common.inode
        || identity.root_device != root_identity.device
        || identity.root_inode != root_identity.inode
        || identity.path_digest != path_digest(path)
        || identity.branch != branch.as_str()
    {
        return WorkerState::Unknown;
    }
    if match directory.metadata_at(OsStr::new(RESERVATION), false) {
        Ok(_) => true,
        Err(error) if error.is_not_found() => false,
        Err(_) => true,
    } {
        return WorkerState::Unknown;
    }
    match directory.lock(
        OsStr::new(WRITER_LOCK),
        LockMode::Exclusive,
        WaitBudget::nonblocking(context.clock.mono_now()),
        cancel,
        context.clock,
    ) {
        Ok(_guard) => match directory.metadata_at(OsStr::new(RESERVATION), false) {
            Ok(_) => WorkerState::Unknown,
            Err(error) if error.is_not_found() => WorkerState::Free,
            Err(_) => WorkerState::Unknown,
        },
        Err(error) if error.raw_os_error().is_some() => WorkerState::Unknown,
        Err(FsError::LockTimeout { .. }) => WorkerState::Live,
        Err(_) => WorkerState::Unknown,
    }
}

fn worktree_dirty(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
) -> Result<WorktreeCondition, WorktreeError> {
    let verified = verify_worktree(context.repo, &entry.path)?;
    let outcome = run_git(context, verified.repo(), GitRead::Status, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Ok(WorktreeCondition::Unknown);
    }
    let bytes = output_bytes(outcome)?;
    status_is_clean(&bytes).map(|clean| {
        if clean {
            WorktreeCondition::Clean
        } else {
            WorktreeCondition::Dirty
        }
    })
}

fn status_is_clean(bytes: &[u8]) -> Result<bool, WorktreeError> {
    let mut clean = true;
    let mut records = bytes.split(|byte| *byte == 0);
    let mut branch_oid = false;
    let mut branch_head = false;
    while let Some(record) = records.next() {
        if record.is_empty() || record.starts_with(b"# ") {
            if record.is_empty() {
                continue;
            }
            if record.starts_with(b"# branch.oid ") {
                if branch_oid || record.len() == b"# branch.oid ".len() {
                    return Err(WorktreeError::InvalidOutput);
                }
                branch_oid = true;
            } else if record.starts_with(b"# branch.head ") {
                if branch_head || record.len() == b"# branch.head ".len() {
                    return Err(WorktreeError::InvalidOutput);
                }
                branch_head = true;
            } else if !(record.starts_with(b"# branch.upstream ")
                || record.starts_with(b"# branch.ab ")
                || record.starts_with(b"# stash "))
            {
                return Err(WorktreeError::InvalidOutput);
            }
            continue;
        }
        if record.starts_with(b"1 ") || record.starts_with(b"u ") || record.starts_with(b"? ") {
            clean = false;
            continue;
        }
        if record.starts_with(b"2 ") {
            clean = false;
            let original_path = records.next().ok_or(WorktreeError::InvalidOutput)?;
            if original_path.is_empty() {
                return Err(WorktreeError::InvalidOutput);
            }
            continue;
        }
        return Err(WorktreeError::InvalidOutput);
    }
    if !branch_oid || !branch_head {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(clean)
}

struct LockedWorktree {
    verified: VerifiedWorktree,
    state: Box<dyn SafeDir>,
    identity: WorktreeIdentity,
    writer: Option<LockGuard>,
}

fn read_worktree_identity(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    require_active: bool,
) -> Result<(VerifiedWorktree, Box<dyn SafeDir>, WorktreeIdentity), WorktreeError> {
    if entry.locked || entry.prunable {
        return Err(WorktreeError::UnknownWriter);
    }
    let branch = entry.branch.as_ref().ok_or(WorktreeError::UnknownWriter)?;
    let verified = verify_worktree(context.repo, &entry.path)?;
    let canonical = canonicalize(verified.path())?;
    let name = state_dir_name(&canonical);
    let state = context
        .project_state
        .open_verified_directory(&name)
        .map_err(|_| WorktreeError::UnknownWriter)?;
    let bytes = read_bounded(state.as_ref(), OsStr::new(IDENTITY), MAX_STATE_RECORD)
        .map_err(|_| WorktreeError::UnknownWriter)?;
    let identity: WorktreeIdentity =
        serde_json::from_slice(&bytes).map_err(|_| WorktreeError::UnknownWriter)?;
    let common = context.repo.common_identity();
    if identity.schema_version != 1
        || (require_active && !identity.active)
        || identity.common_device != common.device
        || identity.common_inode != common.inode
        || identity.root_device != verified.identity().device
        || identity.root_inode != verified.identity().inode
        || identity.path_digest != path_digest(&canonical)
        || identity.branch != branch.as_str()
    {
        return Err(WorktreeError::UnknownWriter);
    }
    Ok((verified, state, identity))
}

fn ensure_unreserved(state: &dyn SafeDir) -> Result<(), WorktreeError> {
    match state.metadata_at(OsStr::new(RESERVATION), false) {
        Ok(_) => Err(WorktreeError::UnknownWriter),
        Err(error) if error.is_not_found() => Ok(()),
        Err(_) => Err(WorktreeError::UnknownWriter),
    }
}

fn lock_worktree_for_mutation(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
) -> Result<LockedWorktree, WorktreeError> {
    let (verified, state, identity) = read_worktree_identity(context, entry, true)?;
    ensure_unreserved(state.as_ref())?;
    let writer = match state.lock(
        OsStr::new(WRITER_LOCK),
        LockMode::Exclusive,
        WaitBudget::nonblocking(context.clock.mono_now()),
        cancel,
        context.clock,
    ) {
        Ok(guard) => guard,
        Err(FsError::LockTimeout { .. }) => return Err(WorktreeError::LiveWriter),
        Err(FsError::Cancelled { .. }) => return Err(WorktreeError::Cancelled),
        Err(_) => return Err(WorktreeError::UnknownWriter),
    };
    ensure_unreserved(state.as_ref())?;
    let canonical = canonicalize(verified.path())?;
    let observations = context
        .workers
        .observe_roots(std::slice::from_ref(&canonical));
    match observations
        .get(&canonical)
        .copied()
        .unwrap_or(WorkerState::Unknown)
    {
        WorkerState::Free => Ok(LockedWorktree {
            verified,
            state,
            identity,
            writer: Some(writer),
        }),
        WorkerState::Live => Err(WorktreeError::LiveWriter),
        WorkerState::Unknown => Err(WorktreeError::UnknownWriter),
    }
}

fn repository_lock<'a>(
    context: &WorktreeContext<'a>,
    mode: LockMode,
    cancel: &CancelToken,
) -> Result<LockGuard, WorktreeError> {
    Ok(context.project_state.lock(
        OsStr::new(REPOSITORY_LOCK),
        mode,
        context.lock_budget,
        cancel,
        context.clock,
    )?)
}

/// Bounded, unpaged repository-scoped worktree inventory.
pub fn list(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    let _lock = repository_lock(context, LockMode::Shared, cancel)?;
    context.repo.revalidate()?;
    let entries = read_worktrees(context, cancel)?;
    let candidates = canonical_worktree_paths(&entries)?;
    let roots: Vec<_> = candidates.iter().map(|(path, _)| path.clone()).collect();
    let worker_states = context.workers.observe_roots(&roots);
    let mut result = Vec::with_capacity(candidates.len());
    for (path, entry) in candidates {
        let branch_text = entry
            .branch
            .as_ref()
            .map(|branch| branch.as_str().to_owned());
        let (dirty, writer) = match entry.branch.as_ref() {
            Some(branch) => match verify_worktree(context.repo, &path) {
                Ok(worktree) => {
                    let dirty = worktree_dirty(context, entry, cancel)
                        .unwrap_or(WorktreeCondition::Unknown);
                    let state = writer_state(context, &path, branch, worktree.identity(), cancel);
                    let worker = worker_states
                        .get(&path)
                        .copied()
                        .unwrap_or(WorkerState::Unknown);
                    let writer = combine_worker_state(state, worker);
                    (dirty, writer)
                }
                Err(_) => (WorktreeCondition::Unknown, WriterState::Unknown),
            },
            None => (WorktreeCondition::Unknown, WriterState::Unknown),
        };
        result.push(WorktreeInfo {
            path,
            branch: branch_text,
            head: entry.head,
            dirty,
            writer,
        });
    }
    Ok(result)
}

fn combine_worker_state(state: WorkerState, native: WorkerState) -> WriterState {
    match (state, native) {
        (WorkerState::Live, _) | (_, WorkerState::Live) => WriterState::Live,
        (WorkerState::Unknown, _) | (_, WorkerState::Unknown) => WriterState::Unknown,
        (WorkerState::Free, WorkerState::Free) => WriterState::Free,
    }
}

/// Create a clean linked checkout at a pinned commit. Failed registration after
/// Git succeeds leaves a visible tree with unknown state; no guessed rollback.
pub fn create(
    context: &WorktreeContext<'_>,
    request: WorktreeCreate,
    cancel: &CancelToken,
) -> Result<WorktreeInfo, WorktreeError> {
    let branch = GitRefName::branch(&request.branch).map_err(|_| WorktreeError::InvalidRequest)?;
    if request.base.is_some() && request.base.as_ref().is_some_and(|base| base.is_empty()) {
        return Err(WorktreeError::InvalidRequest);
    }
    let _lock = repository_lock(context, LockMode::Exclusive, cancel)?;
    context.repo.revalidate()?;
    let entries = read_worktrees(context, cancel)?;
    if entries
        .iter()
        .any(|entry| entry.branch.as_ref() == Some(&branch))
        && request.base.is_some()
    {
        return Err(WorktreeError::InvalidRequest);
    }
    let branch_tips = read_branch_tips(context, cancel)?;
    let existing = branch_tips.get(&branch).copied();
    if existing.is_some() && request.base.is_some() {
        return Err(WorktreeError::InvalidRequest);
    }
    let create_branch = existing.is_none();
    let base = if let Some(base) = request.base.as_deref() {
        pin_revision(context, base, cancel)?
    } else if let Some(existing) = existing {
        existing
    } else {
        let outcome = run_git(context, context.repo, GitRead::RevParseHead, cancel)?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::InvalidOutput);
        }
        parse_oid(trim_ascii(&output_bytes(outcome)?))?
    };
    if entries
        .iter()
        .any(|entry| entry.branch.as_ref() == Some(&branch))
    {
        return Err(WorktreeError::CheckedOut);
    }
    let worktree_root = match context
        .repo
        .root_dir()
        .open_verified_directory(OsStr::new(WORKTREE_ROOT))
    {
        Ok(root) => root,
        Err(error) if error.is_not_found() => {
            context
                .repo
                .root_dir()
                .mkdir(OsStr::new(WORKTREE_ROOT), 0o700)?;
            context.repo.root_dir().sync()?;
            context
                .repo
                .root_dir()
                .open_verified_directory(OsStr::new(WORKTREE_ROOT))?
        }
        Err(error) => return Err(error.into()),
    };
    let path_name = request.branch.replace('/', "-");
    if path_name.is_empty() || path_name.len() > 240 || path_name.as_bytes().contains(&0) {
        return Err(WorktreeError::InvalidRequest);
    }
    let target = canonicalize(context.repo.root_path())?
        .join(WORKTREE_ROOT)
        .join(&path_name);
    match worktree_root.metadata_at(OsStr::new(&path_name), false) {
        Ok(_) => return Err(WorktreeError::InvalidRequest),
        Err(error) if error.is_not_found() => {}
        Err(_) => return Err(WorktreeError::UnknownWriter),
    }
    for (path, _) in canonical_worktree_paths(&entries)? {
        if path == target {
            return Err(WorktreeError::UnknownWriter);
        }
    }
    let state_name = state_dir_name(&target);
    let prior_state = match context.project_state.metadata_at(&state_name, false) {
        Ok(_) => {
            let state = context.project_state.open_verified_directory(&state_name)?;
            ensure_unreserved(state.as_ref())?;
            let bytes = read_bounded(state.as_ref(), OsStr::new(IDENTITY), MAX_STATE_RECORD)
                .map_err(|_| WorktreeError::UnknownWriter)?;
            let prior: WorktreeIdentity =
                serde_json::from_slice(&bytes).map_err(|_| WorktreeError::UnknownWriter)?;
            let common = context.repo.common_identity();
            if prior.schema_version != 1
                || prior.active
                || prior.common_device != common.device
                || prior.common_inode != common.inode
                || prior.path_digest != path_digest(&target)
                || prior.branch != branch.as_str()
            {
                return Err(WorktreeError::UnknownWriter);
            }
            let writer = match state.lock(
                OsStr::new(WRITER_LOCK),
                LockMode::Exclusive,
                WaitBudget::nonblocking(context.clock.mono_now()),
                cancel,
                context.clock,
            ) {
                Ok(guard) => guard,
                Err(FsError::LockTimeout { .. }) => return Err(WorktreeError::LiveWriter),
                Err(FsError::Cancelled { .. }) => return Err(WorktreeError::Cancelled),
                Err(_) => return Err(WorktreeError::UnknownWriter),
            };
            ensure_unreserved(state.as_ref())?;
            Some((state, writer))
        }
        Err(error) if error.is_not_found() => None,
        Err(_) => return Err(WorktreeError::UnknownWriter),
    };
    execute_mutation(
        context,
        GitMutation::WorktreeAdd {
            branch: branch.clone(),
            base: create_branch.then_some(base),
            path: target.clone(),
            create_branch,
        },
        cancel,
    )?;
    context.repo.revalidate()?;
    let fresh = read_worktrees(context, cancel)?;
    let entry = fresh
        .iter()
        .find(|entry| canonicalize(&entry.path).ok().as_deref() == Some(target.as_path()))
        .ok_or(WorktreeError::RegistrationUnconfirmed)?;
    if entry.branch.as_ref() != Some(&branch)
        || entry.head != base
        || entry.locked
        || entry.prunable
    {
        return Err(WorktreeError::RegistrationUnconfirmed);
    }
    let verified = verify_worktree(context.repo, &entry.path)?;
    if worktree_dirty(context, entry, cancel)? != WorktreeCondition::Clean {
        return Err(WorktreeError::RegistrationUnconfirmed);
    }
    let (state, _writer) = match prior_state {
        Some((state, writer)) => (state, writer),
        None => {
            match context.project_state.metadata_at(&state_name, false) {
                Ok(_) => return Err(WorktreeError::UnknownWriter),
                Err(error) if error.is_not_found() => {}
                Err(_) => return Err(WorktreeError::UnknownWriter),
            }
            context.project_state.mkdir(&state_name, 0o700)?;
            context.project_state.sync()?;
            let state = context.project_state.open_verified_directory(&state_name)?;
            let writer = state.lock(
                OsStr::new(WRITER_LOCK),
                LockMode::Exclusive,
                WaitBudget::nonblocking(context.clock.mono_now()),
                cancel,
                context.clock,
            )?;
            ensure_unreserved(state.as_ref())?;
            (state, writer)
        }
    };
    let identity = identity_for(context, &target, &branch, base, verified.identity());
    write_identity(state.as_ref(), &identity, PublishMode::Replace)?;
    let worker = context.workers.observe_roots(std::slice::from_ref(&target));
    let writer = worker.get(&target).copied().unwrap_or(WorkerState::Unknown);
    Ok(WorktreeInfo {
        path: target,
        branch: Some(branch.as_str().to_owned()),
        head: entry.head,
        dirty: WorktreeCondition::Clean,
        writer: match writer {
            WorkerState::Free => WriterState::Free,
            WorkerState::Live => WriterState::Live,
            WorkerState::Unknown => WriterState::Unknown,
        },
    })
}

fn pin_revision(
    context: &WorktreeContext<'_>,
    revision: &str,
    cancel: &CancelToken,
) -> Result<GitOid, WorktreeError> {
    if let Ok(oid) = GitOid::parse(revision) {
        return Ok(oid);
    }
    let reference = GitRefName::new(revision)
        .or_else(|_| GitRefName::branch(revision))
        .map_err(|_| WorktreeError::InvalidRequest)?;
    resolve_ref(context, reference, cancel)
}

fn execute_mutation(
    context: &WorktreeContext<'_>,
    mutation: GitMutation,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    let outcome = execute_git_mutation(
        context.executor,
        context.repo,
        context.utility,
        mutation,
        context.utility_budget,
        cancel,
    )?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    let _ = output_bytes(outcome)?;
    Ok(())
}

const MAX_PRUNE_BRANCHES: usize = 100;
const MAX_GH_MERGED_PRS: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MergeProof {
    Proven,
    NotMerged,
    OutsideSinceRange,
    AtIntegrationTip,
    IntegrationRef,
}

struct RemovedTree {
    info: WorktreeInfo,
    _writer: LockGuard,
}

fn matching_worktree<'a>(
    entries: &'a [GitWorktree],
    branch: &GitRefName,
    repo_root: &Path,
) -> Result<Option<&'a GitWorktree>, WorktreeError> {
    let repo_root = canonicalize(repo_root)?;
    let mut found = None;
    for (path, entry) in canonical_worktree_paths(entries)? {
        if entry.branch.as_ref() != Some(branch) {
            continue;
        }
        if path == repo_root {
            return Err(WorktreeError::Primary);
        }
        if found.is_some() {
            return Err(WorktreeError::InvalidOutput);
        }
        found = Some(entry);
    }
    Ok(found)
}

fn checked_out_elsewhere(
    entries: &[GitWorktree],
    branch: &GitRefName,
    allowed_path: Option<&Path>,
) -> Result<bool, WorktreeError> {
    let allowed = allowed_path.map(canonicalize).transpose()?;
    for (path, entry) in canonical_worktree_paths(entries)? {
        if entry.branch.as_ref() == Some(branch) && allowed.as_ref() != Some(&path) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn tombstone_identity(
    _context: &WorktreeContext<'_>,
    locked: &LockedWorktree,
) -> Result<(), WorktreeError> {
    let identity = WorktreeIdentity {
        schema_version: locked.identity.schema_version,
        active: false,
        common_device: locked.identity.common_device,
        common_inode: locked.identity.common_inode,
        root_device: locked.identity.root_device,
        root_inode: locked.identity.root_inode,
        path_digest: locked.identity.path_digest.clone(),
        branch: locked.identity.branch.clone(),
        base: locked.identity.base,
    };
    write_identity(locked.state.as_ref(), &identity, PublishMode::Replace)
}

fn remove_tree(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    expected_head: GitOid,
    cancel: &CancelToken,
) -> Result<RemovedTree, WorktreeError> {
    if entry.locked || entry.prunable {
        return Err(WorktreeError::UnknownWriter);
    }
    let branch = entry.branch.as_ref().ok_or(WorktreeError::InvalidRequest)?;
    let canonical = canonicalize(&entry.path)?;
    if canonical == canonicalize(context.repo.root_path())? {
        return Err(WorktreeError::Primary);
    }
    if checked_out_elsewhere(&read_worktrees(context, cancel)?, branch, Some(&canonical))? {
        return Err(WorktreeError::CheckedOut);
    }
    let mut locked = lock_worktree_for_mutation(context, entry, cancel)?;
    context.repo.revalidate()?;
    let fresh = read_worktrees(context, cancel)?;
    let current = matching_worktree(&fresh, branch, context.repo.root_path())?
        .ok_or(WorktreeError::IdentityChanged)?;
    if canonicalize(&current.path)? != canonical
        || current.head != expected_head
        || current.locked
        || current.prunable
    {
        return Err(WorktreeError::IdentityChanged);
    }
    if worktree_dirty(context, current, cancel)? != WorktreeCondition::Clean {
        return Err(WorktreeError::Dirty);
    }
    let verified_path = locked.verified.path().to_path_buf();
    execute_mutation(
        context,
        GitMutation::WorktreeRemove {
            path: verified_path.clone(),
        },
        cancel,
    )?;
    context.repo.revalidate()?;
    let after = read_worktrees(context, cancel)?;
    if after.iter().any(|candidate| {
        candidate.branch.as_ref() == Some(branch)
            || canonicalize(&candidate.path).ok().as_deref() == Some(canonical.as_path())
    }) {
        return Err(WorktreeError::RegistrationUnconfirmed);
    }
    tombstone_identity(context, &locked)?;
    let info = WorktreeInfo {
        path: canonical,
        branch: Some(branch.as_str().to_owned()),
        head: expected_head,
        dirty: WorktreeCondition::Clean,
        writer: WriterState::Free,
    };
    let writer = locked.writer.take().ok_or(WorktreeError::UnknownWriter)?;
    Ok(RemovedTree {
        info,
        _writer: writer,
    })
}

fn pin_branch_ref(
    context: &WorktreeContext<'_>,
    reference: &str,
    cancel: &CancelToken,
) -> Result<(GitRefName, GitOid), WorktreeError> {
    let reference = GitRefName::new(reference)
        .or_else(|_| GitRefName::branch(reference))
        .map_err(|_| WorktreeError::InvalidRequest)?;
    let oid = resolve_ref(context, reference.clone(), cancel)?;
    Ok((reference, oid))
}

fn run_github(
    context: &WorktreeContext<'_>,
    read: GitHubRead,
    cancel: &CancelToken,
) -> Result<ExecOutcome, WorktreeError> {
    Ok(run_utility(
        context.executor,
        UtilityRead::GitHub(read),
        context.repo,
        context.utility,
        context.utility_budget,
        cancel,
    )?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MergedPullRequest {
    number: u64,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
    #[serde(rename = "mergeCommit")]
    merge_commit: Option<MergedCommit>,
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "baseRefOid")]
    base_ref_oid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MergedCommit {
    oid: String,
}

fn gh_merge_proof(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    branch_tip: GitOid,
    integration: &GitRefName,
    integration_tip: GitOid,
    since: Option<GitOid>,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    let repository = context
        .github_repository
        .clone()
        .ok_or(WorktreeError::MissingGitHubRepository)?;
    let outcome = run_github(
        context,
        GitHubRead::PullRequestsMerged {
            repository,
            head: branch.clone(),
            limit: MAX_GH_MERGED_PRS,
        },
        cancel,
    )?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    let bytes = output_bytes(outcome)?;
    let rows: Vec<MergedPullRequest> =
        serde_json::from_slice(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    if rows.len() >= MAX_GH_MERGED_PRS as usize {
        return Err(WorktreeError::InvalidOutput);
    }
    for row in rows {
        if row.number == 0 || row.state != "MERGED" {
            return Err(WorktreeError::InvalidOutput);
        }
        let head_oid = parse_oid(row.head_ref_oid.as_bytes())?;
        let _base_oid = parse_oid(row.base_ref_oid.as_bytes())?;
        let merge_commit = row
            .merge_commit
            .as_ref()
            .map(|commit| parse_oid(commit.oid.as_bytes()))
            .transpose()?;
        if row.merged_at.as_deref().is_none_or(str::is_empty) {
            return Err(WorktreeError::InvalidOutput);
        }
        if row.head_ref_name != branch.short_name()
            || row.base_ref_name != integration.short_name()
            || head_oid != branch_tip
        {
            continue;
        }
        let Some(merge_commit) = merge_commit else {
            continue;
        };
        if !is_ancestor(context, merge_commit, integration_tip, cancel)? {
            continue;
        }
        if let Some(since_oid) = since
            && is_ancestor(context, merge_commit, since_oid, cancel)?
        {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

fn merge_proof(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    branch_tip: GitOid,
    integration: &GitRefName,
    integration_tip: GitOid,
    since: Option<GitOid>,
    cancel: &CancelToken,
) -> Result<MergeProof, WorktreeError> {
    if branch == integration {
        return Ok(MergeProof::IntegrationRef);
    }
    if branch_tip == integration_tip {
        return Ok(MergeProof::AtIntegrationTip);
    }
    if is_ancestor(context, branch_tip, integration_tip, cancel)? {
        if let Some(since_oid) = since {
            let branch_before_since = is_ancestor(context, branch_tip, since_oid, cancel)?;
            if branch_before_since {
                return Ok(MergeProof::OutsideSinceRange);
            }
            return Ok(MergeProof::Proven);
        } else {
            return Ok(MergeProof::Proven);
        }
    }
    if gh_merge_proof(
        context,
        branch,
        branch_tip,
        integration,
        integration_tip,
        since,
        cancel,
    )? {
        Ok(MergeProof::Proven)
    } else {
        Ok(MergeProof::NotMerged)
    }
}

fn retain_reason(error: &WorktreeError) -> RetainReason {
    match error {
        WorktreeError::Dirty => RetainReason::Dirty,
        WorktreeError::LiveWriter => RetainReason::LiveWriter,
        WorktreeError::UnknownWriter | WorktreeError::Cancelled => RetainReason::UnknownWriter,
        WorktreeError::CheckedOut => RetainReason::CheckedOut,
        WorktreeError::IdentityChanged => RetainReason::Changed,
        WorktreeError::Primary => RetainReason::Primary,
        WorktreeError::NotFound => RetainReason::Unregistered,
        _ => RetainReason::GitFailure,
    }
}

struct PruneRange<'a> {
    integration: &'a GitRefName,
    tip: GitOid,
    since: Option<GitOid>,
}

fn remove_merged_candidate(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    range: PruneRange<'_>,
    entries: &[GitWorktree],
    branch_tips: &BTreeMap<GitRefName, GitOid>,
    cancel: &CancelToken,
    outcome: &mut RemoveOutcome,
) -> Result<(), WorktreeError> {
    let PruneRange {
        integration,
        tip: integration_tip,
        since,
    } = range;
    if branch == integration {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason: RetainReason::IntegrationRef,
        });
        return Ok(());
    }
    let Some(tip) = branch_tips.get(branch).copied() else {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason: RetainReason::Changed,
        });
        return Ok(());
    };
    if tip == integration_tip {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason: RetainReason::AtIntegrationTip,
        });
        return Ok(());
    }
    let entry = match matching_worktree(entries, branch, context.repo.root_path()) {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            outcome.retained.push(RetainedBranch {
                branch: branch.short_name().to_owned(),
                reason: RetainReason::Unregistered,
            });
            return Ok(());
        }
        Err(error) => {
            outcome.retained.push(RetainedBranch {
                branch: branch.short_name().to_owned(),
                reason: retain_reason(&error),
            });
            return Ok(());
        }
    };
    if entry.head != tip {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason: RetainReason::Changed,
        });
        return Ok(());
    }
    let proof = match merge_proof(
        context,
        branch,
        tip,
        integration,
        integration_tip,
        since,
        cancel,
    ) {
        Ok(proof) => proof,
        Err(error) => {
            outcome.retained.push(RetainedBranch {
                branch: branch.short_name().to_owned(),
                reason: retain_reason(&error),
            });
            return Ok(());
        }
    };
    let reason = match proof {
        MergeProof::Proven => None,
        MergeProof::NotMerged => Some(RetainReason::NotMerged),
        MergeProof::OutsideSinceRange => Some(RetainReason::OutsideSinceRange),
        MergeProof::AtIntegrationTip => Some(RetainReason::AtIntegrationTip),
        MergeProof::IntegrationRef => Some(RetainReason::IntegrationRef),
    };
    if let Some(reason) = reason {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason,
        });
        return Ok(());
    }
    if entry.locked || entry.prunable {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason: RetainReason::LockedOrPrunable,
        });
        return Ok(());
    }
    let removed = match remove_tree(context, entry, tip, cancel) {
        Err(error) => {
            outcome.retained.push(RetainedBranch {
                branch: branch.short_name().to_owned(),
                reason: retain_reason(&error),
            });
            return Ok(());
        }
        Ok(removed) => removed,
    };
    outcome.removed.push(removed.info.clone());
    context.repo.revalidate()?;
    let fresh = read_worktrees(context, cancel)?;
    if checked_out_elsewhere(&fresh, branch, None)? {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason: RetainReason::CheckedOut,
        });
        return Ok(());
    }
    let current = match resolve_ref(context, branch.clone(), cancel) {
        Ok(current) if current == tip => current,
        Ok(_) | Err(WorktreeError::NotFound) => {
            outcome.retained.push(RetainedBranch {
                branch: branch.short_name().to_owned(),
                reason: RetainReason::Changed,
            });
            return Ok(());
        }
        Err(error) => {
            outcome.retained.push(RetainedBranch {
                branch: branch.short_name().to_owned(),
                reason: retain_reason(&error),
            });
            return Ok(());
        }
    };
    let _ = current;
    context.repo.revalidate()?;
    let deleted = execute_git_mutation(
        context.executor,
        context.repo,
        context.utility,
        GitMutation::DeleteRef {
            name: branch.clone(),
            expected_old_oid: tip,
        },
        context.utility_budget,
        cancel,
    );
    if deleted
        .is_ok_and(|result| exit_code(&result).ok() == Some(0) && output_bytes(result).is_ok())
    {
        outcome.pruned_refs.push(branch.short_name().to_owned());
    } else {
        outcome.retained.push(RetainedBranch {
            branch: branch.short_name().to_owned(),
            reason: RetainReason::Changed,
        });
    }
    Ok(())
}

/// Remove one clean managed linked worktree, or prune a caller-scoped set of
/// merged managed worktrees and their exact expected-old local refs.
pub fn remove(
    context: &WorktreeContext<'_>,
    request: WorktreeRemove,
    cancel: &CancelToken,
) -> Result<RemoveOutcome, WorktreeError> {
    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    context.repo.revalidate()?;
    let entries = read_worktrees(context, cancel)?;
    let mut outcome = RemoveOutcome::default();
    match request {
        WorktreeRemove::Name(name) => {
            let branch = GitRefName::branch(&name).map_err(|_| WorktreeError::InvalidRequest)?;
            let entry = matching_worktree(&entries, &branch, context.repo.root_path())?
                .ok_or(WorktreeError::NotFound)?;
            let head = entry.head;
            let removed = remove_tree(context, entry, head, cancel)?;
            outcome.removed.push(removed.info);
        }
        WorktreeRemove::Merged { integration, scope } => {
            let (integration, integration_tip) = pin_branch_ref(context, &integration, cancel)?;
            let branch_tips = read_branch_tips(context, cancel)?;
            let (since, _) = match scope {
                PruneScope::Branches(names) => {
                    if names.is_empty() || names.len() > MAX_PRUNE_BRANCHES {
                        return Err(WorktreeError::InvalidRequest);
                    }
                    let mut branches = BTreeMap::new();
                    for name in names {
                        let branch =
                            GitRefName::branch(name).map_err(|_| WorktreeError::InvalidRequest)?;
                        if branches.insert(branch.clone(), ()).is_some() {
                            return Err(WorktreeError::InvalidRequest);
                        }
                    }
                    for branch in branches.keys() {
                        remove_merged_candidate(
                            context,
                            branch,
                            PruneRange {
                                integration: &integration,
                                tip: integration_tip,
                                since: None,
                            },
                            &entries,
                            &branch_tips,
                            cancel,
                            &mut outcome,
                        )?;
                    }
                    return Ok(outcome);
                }
                PruneScope::Since(since_oid) => {
                    if !is_ancestor(context, since_oid, integration_tip, cancel)? {
                        return Err(WorktreeError::InvalidRequest);
                    }
                    let outcome = run_git(
                        context,
                        context.repo,
                        GitRead::MergeCommits {
                            since: since_oid,
                            tip: integration_tip,
                            limit: MAX_MERGE_COMMITS,
                        },
                        cancel,
                    )?;
                    if exit_code(&outcome)? != 0 {
                        return Err(WorktreeError::InvalidOutput);
                    }
                    let commits = parse_merge_commits(&output_bytes(outcome)?)?;
                    if commits.len() >= MAX_MERGE_COMMITS as usize {
                        return Err(WorktreeError::InvalidOutput);
                    }
                    for commit in &commits {
                        if !is_ancestor(context, *commit, integration_tip, cancel)?
                            || is_ancestor(context, *commit, since_oid, cancel)?
                        {
                            return Err(WorktreeError::InvalidOutput);
                        }
                    }
                    (Some(since_oid), ())
                }
            };
            let Some(since_oid) = since else {
                return Err(WorktreeError::InvalidRequest);
            };
            let mut candidates: Vec<_> = entries
                .iter()
                .filter_map(|entry| entry.branch.clone())
                .filter(|branch| branch_tips.contains_key(branch))
                .collect();
            candidates.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            candidates.dedup();
            if candidates.len() > MAX_PRUNE_BRANCHES {
                return Err(WorktreeError::InvalidRequest);
            }
            for branch in candidates {
                remove_merged_candidate(
                    context,
                    &branch,
                    PruneRange {
                        integration: &integration,
                        tip: integration_tip,
                        since: Some(since_oid),
                    },
                    &entries,
                    &branch_tips,
                    cancel,
                    &mut outcome,
                )?;
            }
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKTREE_LIST: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/external/git/worktree-list-sha1.z"
    ));
    const STATUS_CLEAN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/external/git/status-clean-sha1.z"
    ));
    const STATUS_DIRTY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/external/git/status-dirty-sha1.z"
    ));
    const MERGED_PULL_REQUEST: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/external/git/github-merged.json"
    ));

    #[test]
    fn parses_recorded_git_linked_worktree_and_status_bytes() {
        let trees = parse_worktree_list(WORKTREE_LIST.as_bytes());
        assert!(trees.is_ok());
        let Ok(trees) = trees else { return };
        assert_eq!(trees.len(), 2);
        assert_eq!(
            trees
                .get(1)
                .and_then(|tree| tree.branch.as_ref())
                .map(GitRefName::as_str),
            Some("refs/heads/feature/fixture")
        );
        assert!(matches!(status_is_clean(STATUS_CLEAN.as_bytes()), Ok(true)));
        assert!(matches!(
            status_is_clean(STATUS_DIRTY.as_bytes()),
            Ok(false)
        ));
    }

    #[test]
    fn malformed_git_records_refuse_without_partial_interpretation() {
        let missing_head = b"worktree /tmp/tree\0branch refs/heads/topic\0\0";
        let duplicate_head = b"worktree /tmp/tree\0HEAD 0000000000000000000000000000000000000000\0HEAD 0000000000000000000000000000000000000000\0branch refs/heads/topic\0\0";
        let status_without_headers = b"1 .M N... 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 file\0";
        assert!(parse_worktree_list(missing_head).is_err());
        assert!(parse_worktree_list(duplicate_head).is_err());
        assert!(status_is_clean(status_without_headers).is_err());
    }

    #[test]
    fn decodes_recorded_gh_merged_pull_request_fields() {
        let rows: Result<Vec<MergedPullRequest>, _> = serde_json::from_str(MERGED_PULL_REQUEST);
        assert!(rows.is_ok());
        let Ok(rows) = rows else { return };
        assert_eq!(rows.len(), 1);
        let Some(row) = rows.first() else {
            return;
        };
        assert_eq!(row.state, "MERGED");
        assert_eq!(row.head_ref_name, "rewrite/supervisor-s2");
        assert_eq!(row.base_ref_name, "main");
        assert!(row.merge_commit.is_some());
    }

    #[test]
    fn worktree_path_wire_is_explicit_and_absolute() {
        let wire = AbsolutePathWire::from_path(Path::new("/tmp/project/.worktrees/topic"));
        assert!(wire.is_ok());
        let encoded = serde_json::to_vec(&wire.ok());
        assert!(encoded.is_ok());
        assert_eq!(
            encoded.ok(),
            Some(br#"{"kind":"path","root":"/","relative":{"encoding":"utf8","path":"tmp/project/.worktrees/topic"}}"#.to_vec())
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::utility::tests::{Fixture, run_fixture_git};
    use crate::utility::{GitMutation, execute_git_mutation};
    use provenant_core::Clock;
    use provenant_supervisor::clock::SystemClock;
    use provenant_supervisor::fs::{LocalFs, SafeDir, SafeFs};
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct FreeWorkers;

    impl WorkerObserver for FreeWorkers {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Free))
                .collect()
        }
    }

    struct TestContext<'a> {
        fixture: &'a Fixture,
        fs: LocalFs,
        project_state: Box<dyn SafeDir>,
        workers: FreeWorkers,
        clock: SystemClock,
        lock_budget: WaitBudget,
        utility_budget: UtilityBudget,
    }

    impl<'a> TestContext<'a> {
        fn new(fixture: &'a Fixture) -> Result<Self, Box<dyn Error>> {
            let fs = LocalFs;
            let state_path = fixture._temp.path().join("project-worktrees");
            fs::create_dir(&state_path)?;
            fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))?;
            let project_state = fs.open_anchor(&state_path)?;
            let clock = SystemClock;
            let lock_budget = WaitBudget::cli(clock.mono_now(), 5)?;
            Ok(Self {
                fixture,
                fs,
                project_state,
                workers: FreeWorkers,
                clock,
                lock_budget,
                utility_budget: fixture.budget()?,
            })
        }

        fn context(&self) -> WorktreeContext<'_> {
            WorktreeContext {
                repo: &self.fixture.repo,
                project_state: self.project_state.as_ref(),
                fs: &self.fs,
                executor: &self.fixture.executor,
                utility: &self.fixture.runtime,
                github_repository: None,
                workers: &self.workers,
                clock: &self.clock,
                utility_budget: self.utility_budget,
                lock_budget: self.lock_budget,
            }
        }
    }

    fn cancel() -> CancelToken {
        CancelToken::new()
    }

    fn run_in_worktree(
        fixture: &Fixture,
        path: &Path,
        args: &[&str],
    ) -> Result<(), Box<dyn Error>> {
        let env = vec![
            ("HOME".into(), fixture.home.as_os_str().to_owned()),
            ("TMPDIR".into(), fixture._temp.path().as_os_str().to_owned()),
            (
                "PROVENANT_STATE_DIR".into(),
                fixture._temp.path().join("state").into_os_string(),
            ),
            (
                "XDG_CONFIG_HOME".into(),
                fixture._temp.path().join("config").into_os_string(),
            ),
            (
                "CLAUDE_CONFIG_DIR".into(),
                fixture._temp.path().join("claude").into_os_string(),
            ),
            (
                "CODEX_HOME".into(),
                fixture._temp.path().join("codex").into_os_string(),
            ),
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
        let path = path.to_str().ok_or("non-UTF8 worktree path")?;
        let mut git_args = vec!["-C", path];
        git_args.extend_from_slice(args);
        run_fixture_git(
            &fixture.executor,
            &fixture.git,
            &fixture.root,
            &env,
            &git_args,
        )?;
        Ok(())
    }

    #[test]
    fn create_list_remove_keeps_branch_and_refuses_primary() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();

        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/lifecycle".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        assert!(
            created
                .path
                .starts_with(canonicalize(&fixture.root)?.join(".worktrees"))
        );
        assert_eq!(created.head, fixture.head()?);
        assert_eq!(created.dirty, WorktreeCondition::Clean);
        assert_eq!(created.writer, WriterState::Free);

        let listed = list(&context, &cancellation)?;
        assert!(listed.iter().any(|entry| {
            entry.path == created.path
                && entry.branch.as_deref() == Some("refs/heads/feature/lifecycle")
                && entry.dirty == WorktreeCondition::Clean
                && entry.writer == WriterState::Free
        }));
        assert!(matches!(
            remove(
                &context,
                WorktreeRemove::Name("main".to_owned()),
                &cancellation
            ),
            Err(WorktreeError::Primary)
        ));

        let removed = remove(
            &context,
            WorktreeRemove::Name("feature/lifecycle".to_owned()),
            &cancellation,
        )?;
        assert_eq!(removed.removed.len(), 1);
        assert!(removed.pruned_refs.is_empty());
        assert!(!created.path.exists());
        let tips = read_branch_tips(&context, &cancellation)?;
        assert!(tips.contains_key(&GitRefName::branch("feature/lifecycle")?));
        Ok(())
    }

    #[test]
    fn removal_refuses_live_writer_and_any_reservation_record() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/busy".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        let state_name = state_dir_name(&created.path);
        let state = context.project_state.open_verified_directory(&state_name)?;
        let held_writer = state.lock(
            OsStr::new(WRITER_LOCK),
            LockMode::Exclusive,
            WaitBudget::nonblocking(test_context.clock.mono_now()),
            &cancellation,
            &test_context.clock,
        )?;
        assert!(matches!(
            remove(
                &context,
                WorktreeRemove::Name("feature/busy".to_owned()),
                &cancellation
            ),
            Err(WorktreeError::LiveWriter)
        ));
        drop(held_writer);

        let _publication = state.atomic_write(
            OsStr::new(RESERVATION),
            ".reservation-",
            b"malformed reservations remain authoritative",
            PublishMode::Replace,
        )?;
        assert!(matches!(
            remove(
                &context,
                WorktreeRemove::Name("feature/busy".to_owned()),
                &cancellation
            ),
            Err(WorktreeError::UnknownWriter)
        ));
        state.unlink(OsStr::new(RESERVATION))?;
        remove(
            &context,
            WorktreeRemove::Name("feature/busy".to_owned()),
            &cancellation,
        )?;
        Ok(())
    }

    #[test]
    fn merged_pruning_is_branch_scoped_and_keeps_unselected_refs() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let merged = create(
            &context,
            WorktreeCreate {
                branch: "feature/merged".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        fs::write(merged.path.join("proof.txt"), "merged fixture\n")?;
        run_in_worktree(&fixture, &merged.path, &["add", "proof.txt"])?;
        run_in_worktree(&fixture, &merged.path, &["commit", "-m", "feature commit"])?;
        fixture.run(&["merge", "--no-ff", "feature/merged", "-m", "merge feature"])?;

        let unselected = create(
            &context,
            WorktreeCreate {
                branch: "feature/unselected".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        let removed = remove(
            &context,
            WorktreeRemove::Merged {
                integration: "main".to_owned(),
                scope: PruneScope::Branches(vec!["feature/merged".to_owned()]),
            },
            &cancellation,
        )?;
        assert_eq!(removed.pruned_refs, vec!["feature/merged"]);
        assert_eq!(removed.removed.len(), 1);
        assert!(!merged.path.exists());
        assert!(unselected.path.exists());
        let tips = read_branch_tips(&context, &cancellation)?;
        assert!(!tips.contains_key(&GitRefName::branch("feature/merged")?));
        assert!(tips.contains_key(&GitRefName::branch("feature/unselected")?));
        Ok(())
    }

    #[test]
    fn delete_ref_compare_and_swap_preserves_a_moved_ref() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let stale_oid = fixture.head()?;
        fixture.run(&["branch", "feature/race"])?;
        fixture.run(&["commit", "--allow-empty", "-m", "advance integration"])?;
        let moved_oid = fixture.head()?;
        fixture.run(&["branch", "-f", "feature/race", &moved_oid.to_string()])?;

        let outcome = execute_git_mutation(
            &fixture.executor,
            &fixture.repo,
            &fixture.runtime,
            GitMutation::DeleteRef {
                name: GitRefName::branch("feature/race")?,
                expected_old_oid: stale_oid,
            },
            fixture.budget()?,
            &cancellation,
        )?;
        assert!(matches!(outcome.termination, Termination::Exit(code) if code != 0));
        let tips = read_branch_tips(&context, &cancellation)?;
        assert_eq!(
            tips.get(&GitRefName::branch("feature/race")?),
            Some(&moved_oid)
        );
        Ok(())
    }

    #[test]
    fn since_pruning_uses_pinned_graph_delta_and_retains_tip_equal_branches()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let since = fixture.head()?;
        let merged = create(
            &context,
            WorktreeCreate {
                branch: "feature/since".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        fs::write(merged.path.join("since.txt"), "after pinned base\n")?;
        run_in_worktree(&fixture, &merged.path, &["add", "since.txt"])?;
        run_in_worktree(&fixture, &merged.path, &["commit", "-m", "since commit"])?;
        fixture.run(&["merge", "--no-ff", "feature/since", "-m", "merge since"])?;
        let at_tip = create(
            &context,
            WorktreeCreate {
                branch: "feature/at-tip".to_owned(),
                base: None,
            },
            &cancellation,
        )?;

        let outcome = remove(
            &context,
            WorktreeRemove::Merged {
                integration: "main".to_owned(),
                scope: PruneScope::Since(since),
            },
            &cancellation,
        )?;
        assert_eq!(outcome.pruned_refs, vec!["feature/since"]);
        assert!(at_tip.path.exists());
        assert!(outcome.retained.iter().any(|retained| {
            retained.branch == "feature/at-tip" && retained.reason == RetainReason::AtIntegrationTip
        }));
        Ok(())
    }
}
