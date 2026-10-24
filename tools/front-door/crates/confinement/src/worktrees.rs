//! Repository-scoped linked-worktree lifecycle and merged-ref pruning.

use crate::repo::{
    GitRefName, GitRevision, RepoError, VerifiedRepo, VerifiedWorktree, verify_worktree,
};
use crate::status::{parse_index_entries, parse_porcelain_v2_z};
use crate::utility::{
    GitMutation, GitRead, UtilityBudget, UtilityError, UtilityRuntime, execute_git_mutation,
    run_utility,
};
use provenant_config::ProjectBinding;
use provenant_core::Clock;
use provenant_core::{CancelToken, GitOid, ProjectId, RelativePath, WaitBudget, hex};
use provenant_records::{PathWire, PathWireError};
use provenant_supervisor::census::{
    ProcessFact, ProcessObservation, ProcessObserver, SystemProcessObserver, current_process,
};
use provenant_supervisor::exec::{ExecOutcome, Executor, Termination};
use provenant_supervisor::fs::{
    FsError, LockGuard, LockMode, Metadata, PublishMode, SafeDir, canonicalize,
};
use provenant_supervisor::pid::{CwdObservation, CwdObserver, SystemCwdObserver};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_WORKTREES: usize = 10_000;
const MAX_BRANCHES: usize = 10_000;
const MAX_STATE_RECORD: u64 = 64 * 1024;
const WORKTREE_IDENTITY_SCHEMA: u64 = 3;
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

/// Dependencies at the actual OS and utility seams. The caller supplies the
/// binding and its `state/worktrees/<ProjectId>` directory.
pub struct WorktreeContext<'a> {
    pub repo: &'a VerifiedRepo,
    pub binding: &'a ProjectBinding,
    pub project_state: &'a dyn SafeDir,
    pub executor: &'a dyn Executor,
    pub utility: &'a UtilityRuntime,
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
pub enum WorktreeRemove {
    Name(String),
    Merged {
        integration: String,
        scope: PruneScope,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PruneScope {
    Branches(Vec<String>),
    Since(GitOid),
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
    CheckedOut,
    Dirty,
    NoUniqueCommit,
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
                    record_live_path(&mut states, &canonical);
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

fn record_live_path(states: &mut BTreeMap<PathBuf, WorkerState>, cwd: &Path) {
    for (root, state) in states {
        if cwd == root.as_path() || cwd.starts_with(root) {
            *state = WorkerState::Live;
        }
    }
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeIdentity {
    schema_version: u64,
    active: bool,
    project_id: ProjectId,
    root_device: u64,
    root_inode: u64,
    path_digest: String,
    branch: String,
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

fn run_git(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    read: GitRead,
    cancel: &CancelToken,
) -> Result<ExecOutcome, WorktreeError> {
    run_utility(
        context.executor,
        read,
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
    parse_worktree_list(&outcome.stdout.bytes)
}

fn read_branch_tips(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<BTreeMap<GitRefName, GitOid>, WorktreeError> {
    let outcome = run_git(context, context.repo, GitRead::BranchTips, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    parse_branch_tips(&outcome.stdout.bytes)
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
    parse_oid(trim_ascii(&outcome.stdout.bytes))
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

fn is_first_parent_commit(
    context: &WorktreeContext<'_>,
    history_tip: GitOid,
    candidate: GitOid,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    let outcome = run_git(
        context,
        context.repo,
        GitRead::FirstParentHistory { tip: history_tip },
        cancel,
    )?;
    if exit_code(&outcome)? != 0 || !outcome.stdout.bytes.ends_with(b"\n") {
        return Err(WorktreeError::InvalidOutput);
    }
    let history = outcome
        .stdout
        .bytes
        .strip_suffix(b"\n")
        .ok_or(WorktreeError::InvalidOutput)?;
    if history.is_empty() {
        return Err(WorktreeError::InvalidOutput);
    }
    let mut found = false;
    for line in history.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            return Err(WorktreeError::InvalidOutput);
        }
        found |= parse_oid(line)? == candidate;
    }
    Ok(found)
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
    root_identity: Metadata,
) -> WorktreeIdentity {
    WorktreeIdentity {
        schema_version: WORKTREE_IDENTITY_SCHEMA,
        active: true,
        project_id: context.binding.project_id(),
        root_device: root_identity.device,
        root_inode: root_identity.inode,
        path_digest: path_digest(path),
        branch: branch.as_str().to_owned(),
    }
}

fn read_bounded(directory: &dyn SafeDir, name: &OsStr, limit: u64) -> Result<Vec<u8>, FsError> {
    crate::stable_read::read_regular(directory, name, limit)
        .map(|(bytes, _)| bytes)
        .map_err(|error| match error {
            crate::stable_read::ReadError::Unsupported => FsError::NotRegular { name: name.into() },
            crate::stable_read::ReadError::TooLarge => FsError::TooLarge { limit },
            crate::stable_read::ReadError::Changed => FsError::RaceRefused { name: name.into() },
            crate::stable_read::ReadError::Filesystem(error) => error,
            crate::stable_read::ReadError::Io(source) => FsError::Io {
                operation: "read worktree state",
                path: name.into(),
                source,
            },
        })
}

fn write_identity(
    directory: &dyn SafeDir,
    identity: &WorktreeIdentity,
    mode: PublishMode,
) -> Result<(), WorktreeError> {
    let bytes = serde_json::to_vec(identity).map_err(|_| WorktreeError::InvalidOutput)?;
    match directory.atomic_write(OsStr::new(IDENTITY), ".identity-", &bytes, 0o600, mode)? {
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
    if identity.schema_version != WORKTREE_IDENTITY_SCHEMA
        || !identity.active
        || identity.project_id != context.binding.project_id()
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

fn worktree_dirty_at(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<WorktreeCondition, WorktreeError> {
    let index = run_git(
        context,
        repo,
        GitRead::IndexEntries { paths: Vec::new() },
        cancel,
    )?;
    if exit_code(&index)? != 0 {
        return Ok(WorktreeCondition::Unknown);
    }
    let mut has_gitlink = false;
    for entry in
        parse_index_entries(&index.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        has_gitlink |= entry.map_err(|_| WorktreeError::InvalidOutput)?.mode == 0o160000;
    }
    if has_gitlink {
        return Ok(WorktreeCondition::Unknown);
    }
    let outcome = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Ok(WorktreeCondition::Unknown);
    }
    let records =
        parse_porcelain_v2_z(&outcome.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    Ok(if records.is_empty() {
        WorktreeCondition::Clean
    } else {
        WorktreeCondition::Dirty
    })
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
    if identity.schema_version != WORKTREE_IDENTITY_SCHEMA
        || (require_active && !identity.active)
        || identity.project_id != context.binding.project_id()
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

fn validate_binding(context: &WorktreeContext<'_>) -> Result<(), WorktreeError> {
    if context.binding.common_dir() != context.repo.common_git_dir() {
        return Err(RepoError::InvalidRegistration.into());
    }
    Ok(())
}

/// Bounded, unpaged repository-scoped worktree inventory.
pub fn list(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    validate_binding(context)?;
    let _lock = repository_lock(context, LockMode::Shared, cancel)?;
    context.repo.revalidate()?;
    let entries = read_worktrees(context, cancel)?;
    let candidates = canonical_worktree_paths(&entries)?;
    let roots: Vec<_> = candidates.iter().map(|(path, _)| path.clone()).collect();
    let worker_states = context.workers.observe_roots(&roots);
    let primary = canonicalize(context.repo.root_path())?;
    let mut result = Vec::with_capacity(candidates.len());
    for (path, entry) in candidates {
        let branch_text = entry
            .branch
            .as_ref()
            .map(|branch| branch.as_str().to_owned());
        let observed = worker_states
            .get(&path)
            .copied()
            .unwrap_or(WorkerState::Unknown);
        let (dirty, writer) = match (entry.branch.as_ref(), path == primary) {
            (Some(_), true) => (
                worktree_dirty_at(context, context.repo, cancel)
                    .unwrap_or(WorktreeCondition::Unknown),
                combine_worker_state(WorkerState::Unknown, observed),
            ),
            (Some(branch), false) => match verify_worktree(context.repo, &path) {
                Ok(worktree) => (
                    worktree_dirty_at(context, worktree.repo(), cancel)
                        .unwrap_or(WorktreeCondition::Unknown),
                    combine_worker_state(
                        writer_state(context, &path, branch, worktree.identity(), cancel),
                        observed,
                    ),
                ),
                Err(_) => (WorktreeCondition::Unknown, WriterState::Unknown),
            },
            (None, _) => (WorktreeCondition::Unknown, WriterState::Unknown),
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
    validate_binding(context)?;
    let branch = GitRefName::branch(&request.branch).map_err(|_| WorktreeError::InvalidRequest)?;
    let _lock = repository_lock(context, LockMode::Exclusive, cancel)?;
    context.repo.revalidate()?;
    let entries = read_worktrees(context, cancel)?;
    let checked_out = entries
        .iter()
        .any(|entry| entry.branch.as_ref() == Some(&branch));
    if checked_out {
        return Err(if request.base.is_some() {
            WorktreeError::InvalidRequest
        } else {
            WorktreeError::CheckedOut
        });
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
        parse_oid(trim_ascii(&outcome.stdout.bytes))?
    };
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
    let root_identity = worktree_root.metadata()?;
    if root_identity.uid != provenant_supervisor::fs::effective_uid()
        || root_identity.mode & 0o022 != 0
        || root_identity.device != context.repo.root_identity().device
    {
        return Err(WorktreeError::UnknownWriter);
    }
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
            if prior.schema_version != WORKTREE_IDENTITY_SCHEMA
                || prior.active
                || prior.project_id != context.binding.project_id()
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
    let named_root = context
        .repo
        .root_dir()
        .metadata_at(OsStr::new(WORKTREE_ROOT), false)?;
    if !root_identity.same_identity(&named_root) {
        return Err(WorktreeError::UnknownWriter);
    }
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
    if worktree_dirty_at(context, verified.repo(), cancel)? != WorktreeCondition::Clean {
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
    let identity = identity_for(context, &target, &branch, verified.identity());
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
    let revision = GitRevision::new(revision).map_err(|_| WorktreeError::InvalidRequest)?;
    let outcome = run_git(
        context,
        context.repo,
        GitRead::ResolveRevision { revision },
        cancel,
    )?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::NotFound);
    }
    parse_oid(trim_ascii(&outcome.stdout.bytes))
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
    Ok(())
}

const MAX_PRUNE_BRANCHES: usize = 100;

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

fn tombstone_identity(locked: &LockedWorktree) -> Result<(), WorktreeError> {
    let mut identity = locked.identity.clone();
    identity.active = false;
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
    match worktree_dirty_at(context, locked.verified.repo(), cancel)? {
        WorktreeCondition::Clean => {}
        WorktreeCondition::Dirty => return Err(WorktreeError::Dirty),
        WorktreeCondition::Unknown => return Err(WorktreeError::UnknownWriter),
    }
    let verified_path = locked.verified.path().to_path_buf();
    execute_mutation(
        context,
        GitMutation::WorktreeRemove {
            path: verified_path,
        },
        cancel,
    )?;
    context.repo.revalidate()?;
    tombstone_identity(&locked)?;
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

fn retain(outcome: &mut RemoveOutcome, branch: &GitRefName, reason: RetainReason) {
    outcome.retained.push(RetainedBranch {
        branch: branch.short_name().to_owned(),
        reason,
    });
}

struct PruneRange<'a> {
    integration: &'a GitRefName,
    tip: GitOid,
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
    } = range;
    if branch == integration {
        retain(outcome, branch, RetainReason::IntegrationRef);
        return Ok(());
    }
    let Some(tip) = branch_tips.get(branch).copied() else {
        retain(outcome, branch, RetainReason::Changed);
        return Ok(());
    };
    if tip == integration_tip {
        retain(outcome, branch, RetainReason::AtIntegrationTip);
        return Ok(());
    }
    let entry = match matching_worktree(entries, branch, context.repo.root_path()) {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            retain(outcome, branch, RetainReason::Unregistered);
            return Ok(());
        }
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
    };
    if entry.head != tip {
        retain(outcome, branch, RetainReason::Changed);
        return Ok(());
    }
    let merged = match is_ancestor(context, tip, integration_tip, cancel) {
        Ok(merged) => merged,
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
    };
    if !merged {
        retain(outcome, branch, RetainReason::NotMerged);
        return Ok(());
    }
    match is_first_parent_commit(context, integration_tip, tip, cancel) {
        Ok(true) => {
            retain(outcome, branch, RetainReason::NoUniqueCommit);
            return Ok(());
        }
        Ok(false) => {}
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
    }
    if entry.locked || entry.prunable {
        retain(outcome, branch, RetainReason::LockedOrPrunable);
        return Ok(());
    }
    let removed = match remove_tree(context, entry, tip, cancel) {
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
        Ok(removed) => removed,
    };
    outcome.removed.push(removed.info.clone());
    context.repo.revalidate()?;
    let fresh = read_worktrees(context, cancel)?;
    if checked_out_elsewhere(&fresh, branch, None)? {
        retain(outcome, branch, RetainReason::CheckedOut);
        return Ok(());
    }
    match resolve_ref(context, branch.clone(), cancel) {
        Ok(current) if current == tip => {}
        Ok(_) | Err(WorktreeError::NotFound) => {
            retain(outcome, branch, RetainReason::Changed);
            return Ok(());
        }
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
    };
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
    if deleted.is_ok_and(|result| exit_code(&result).ok() == Some(0)) {
        outcome.pruned_refs.push(branch.short_name().to_owned());
    } else {
        retain(outcome, branch, RetainReason::Changed);
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
    validate_binding(context)?;
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
            let mut branches = BTreeMap::new();
            match scope {
                PruneScope::Branches(names) => {
                    if names.is_empty() || names.len() > MAX_PRUNE_BRANCHES {
                        return Err(WorktreeError::InvalidRequest);
                    }
                    for name in names {
                        let branch =
                            GitRefName::branch(name).map_err(|_| WorktreeError::InvalidRequest)?;
                        if branches.insert(branch, ()).is_some() {
                            return Err(WorktreeError::InvalidRequest);
                        }
                    }
                }
                PruneScope::Since(bound) => {
                    if !is_ancestor(context, bound, integration_tip, cancel)? {
                        return Err(WorktreeError::InvalidRequest);
                    }
                    // Scope ancestry-proven tips to the pinned graph delta.
                    // Squash merges lack exact-head proof here and stay put.
                    for (branch, tip) in &branch_tips {
                        if is_ancestor(context, *tip, integration_tip, cancel)?
                            && !is_ancestor(context, *tip, bound, cancel)?
                        {
                            branches.insert(branch.clone(), ());
                        }
                    }
                    if branches.len() > MAX_PRUNE_BRANCHES {
                        return Err(WorktreeError::InvalidRequest);
                    }
                }
            }
            for branch in branches.keys() {
                remove_merged_candidate(
                    context,
                    branch,
                    PruneRange {
                        integration: &integration,
                        tip: integration_tip,
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
    #[test]
    fn parses_recorded_git_linked_worktree_list() {
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
    }

    #[test]
    fn recorded_multiple_branch_tips_keep_git_record_newlines_separate() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/external/git/branch-tips-sha1.z"
        ));
        let parsed = parse_branch_tips(fixture.as_bytes()).expect("recorded Git branch tips");
        assert_eq!(parsed.len(), 2);
        assert!(parsed.keys().any(|name| name.as_str() == "refs/heads/main"));
        assert!(
            parsed
                .keys()
                .any(|name| name.as_str() == "refs/heads/feature/recorded")
        );
    }

    #[test]
    fn malformed_git_records_refuse_without_partial_interpretation() {
        let missing_head = b"worktree /tmp/tree\0branch refs/heads/topic\0\0";
        let duplicate_head = b"worktree /tmp/tree\0HEAD 0000000000000000000000000000000000000000\0HEAD 0000000000000000000000000000000000000000\0branch refs/heads/topic\0\0";
        assert!(parse_worktree_list(missing_head).is_err());
        assert!(parse_worktree_list(duplicate_head).is_err());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::test_support::{
        Fixture, add_poisoned_gitlink, assert_raw_git_runs_poison_filter, fixture_env,
        run_fixture_git,
    };
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

    struct UnknownWorkers;

    impl WorkerObserver for UnknownWorkers {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
                .collect()
        }
    }

    struct TestContext<'a> {
        fixture: &'a Fixture,
        binding: ProjectBinding,
        project_state: Box<dyn SafeDir>,
        workers: FreeWorkers,
        clock: SystemClock,
        lock_budget: WaitBudget,
        utility_budget: UtilityBudget,
    }

    impl<'a> TestContext<'a> {
        fn new(fixture: &'a Fixture) -> Result<Self, Box<dyn Error>> {
            let fs = LocalFs;
            let binding_state = fixture._temp.path().join("bindings");
            fs::create_dir(&binding_state)?;
            let mut inputs =
                provenant_core::RootInputs::new(fixture.home.clone(), fixture.root.clone());
            inputs.state_dir = Some(binding_state);
            let roots = inputs.resolve(&fs)?;
            let binding = provenant_config::bind_project(
                &roots,
                provenant_config::ProjectLocation::new(fixture.repo.common_git_dir())?,
                &fs,
            )?;
            let state_root = fixture._temp.path().join("worktrees");
            fs::create_dir(&state_root)?;
            fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))?;
            let state_path = state_root.join(binding.project_id().to_string());
            fs::create_dir(&state_path)?;
            fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))?;
            let project_state = fs.open_anchor(&state_path)?;
            let clock = SystemClock;
            let lock_budget = WaitBudget::cli(clock.mono_now(), 5)?;
            Ok(Self {
                fixture,
                binding,
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
                binding: &self.binding,
                project_state: self.project_state.as_ref(),
                executor: &self.fixture.executor,
                utility: &self.fixture.runtime,
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
        let env = fixture_env(&fixture.home, fixture._temp.path());
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
    fn nested_worker_protects_all_containing_worktrees() {
        let mut states = BTreeMap::from([
            (PathBuf::from("/project/outer"), WorkerState::Free),
            (PathBuf::from("/project/outer/nested"), WorkerState::Free),
            (PathBuf::from("/project/sibling"), WorkerState::Free),
        ]);
        record_live_path(&mut states, Path::new("/project/outer/nested/src"));
        assert_eq!(
            states.get(Path::new("/project/outer")),
            Some(&WorkerState::Live)
        );
        assert_eq!(
            states.get(Path::new("/project/outer/nested")),
            Some(&WorkerState::Live)
        );
        assert_eq!(
            states.get(Path::new("/project/sibling")),
            Some(&WorkerState::Free)
        );
    }

    #[test]
    fn creation_refuses_a_shared_writable_worktree_parent() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::create_dir(fixture.root.join(WORKTREE_ROOT))?;
        fs::set_permissions(
            fixture.root.join(WORKTREE_ROOT),
            fs::Permissions::from_mode(0o777),
        )?;
        let test_context = TestContext::new(&fixture)?;
        assert!(matches!(
            create(
                &test_context.context(),
                WorktreeCreate {
                    branch: "feature/refused".to_owned(),
                    base: None
                },
                &cancel()
            ),
            Err(WorktreeError::UnknownWriter)
        ));
        assert!(
            !fixture
                .root
                .join(WORKTREE_ROOT)
                .join("feature-refused")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn worktree_operations_require_the_repo_project_binding() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let mut test_context = TestContext::new(&fixture)?;
        let other_fixture = Fixture::new()?;
        test_context.binding = TestContext::new(&other_fixture)?.binding;
        assert!(matches!(
            list(&test_context.context(), &cancel()),
            Err(WorktreeError::Repository(RepoError::InvalidRegistration))
        ));
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

        let unknown_context = WorktreeContext {
            workers: &UnknownWorkers,
            ..test_context.context()
        };
        assert!(matches!(
            remove(
                &unknown_context,
                WorktreeRemove::Name("feature/lifecycle".to_owned()),
                &cancellation,
            ),
            Err(WorktreeError::UnknownWriter)
        ));
        assert!(created.path.is_dir());

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
    fn create_on_existing_branch_attaches_and_can_be_removed() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        fixture.run(&["branch", "feature/existing"])?;
        let expected_head = fixture.head()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();

        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/existing".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        assert_eq!(created.head, expected_head);
        assert_eq!(
            created.branch.as_deref(),
            Some("refs/heads/feature/existing")
        );

        let removed = remove(
            &context,
            WorktreeRemove::Name("feature/existing".to_owned()),
            &cancellation,
        )?;
        assert_eq!(removed.removed.len(), 1);
        assert!(!created.path.exists());
        assert!(
            read_branch_tips(&context, &cancellation)?
                .contains_key(&GitRefName::branch("feature/existing")?)
        );
        Ok(())
    }

    #[test]
    fn base_oid_must_name_an_existing_commit() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        assert!(matches!(
            pin_revision(
                &test_context.context(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &cancel()
            ),
            Err(WorktreeError::NotFound)
        ));
        assert_eq!(
            pin_revision(
                &test_context.context(),
                &fixture.head()?.to_string(),
                &cancel()
            )?,
            fixture.head()?
        );
        Ok(())
    }

    #[test]
    fn base_revision_accepts_origin_branch_and_pins_its_commit() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let base = fixture.head()?;
        fixture.run(&["update-ref", "refs/remotes/origin/main", &base.to_string()])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();

        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/remote-base".to_owned(),
                base: Some("origin/main".to_owned()),
            },
            &cancellation,
        )?;
        assert_eq!(created.head, base);
        remove(
            &context,
            WorktreeRemove::Name("feature/remote-base".to_owned()),
            &cancellation,
        )?;
        Ok(())
    }

    #[test]
    fn ignored_files_prevent_worktree_removal() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        fs::write(fixture.root.join(".gitignore"), "ignored.txt\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore fixture file"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/ignored".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        fs::write(created.path.join("ignored.txt"), "keep me\n")?;

        assert!(matches!(
            remove(
                &context,
                WorktreeRemove::Name("feature/ignored".to_owned()),
                &cancellation,
            ),
            Err(WorktreeError::Dirty)
        ));
        assert_eq!(
            fs::read_to_string(created.path.join("ignored.txt"))?,
            "keep me\n"
        );
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
            0o600,
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
    fn merged_pruning_keeps_old_ancestor_and_unmerged_ignored_files() -> Result<(), Box<dyn Error>>
    {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        fs::write(fixture.root.join(".gitignore"), "ignored.txt\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore fixture file"])?;
        let old_base = fixture.head()?;
        fixture.run(&[
            "commit",
            "--allow-empty",
            "-m",
            "advance before branch creation",
        ])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let old = create(
            &context,
            WorktreeCreate {
                branch: "feature/old-ancestor".to_owned(),
                base: Some(old_base.to_string()),
            },
            &cancellation,
        )?;
        let unmerged = create(
            &context,
            WorktreeCreate {
                branch: "feature/unmerged".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        fs::write(unmerged.path.join("unique.txt"), "branch-only commit\n")?;
        run_in_worktree(&fixture, &unmerged.path, &["add", "unique.txt"])?;
        run_in_worktree(
            &fixture,
            &unmerged.path,
            &["commit", "-m", "unique branch commit"],
        )?;
        fs::write(old.path.join("ignored.txt"), "old branch data\n")?;
        fs::write(unmerged.path.join("ignored.txt"), "unmerged branch data\n")?;
        fixture.run(&["commit", "--allow-empty", "-m", "advance integration"])?;

        let outcome = remove(
            &context,
            WorktreeRemove::Merged {
                integration: "main".to_owned(),
                scope: PruneScope::Branches(vec![
                    "feature/old-ancestor".to_owned(),
                    "feature/unmerged".to_owned(),
                ]),
            },
            &cancellation,
        )?;
        assert!(outcome.removed.is_empty());
        assert!(outcome.pruned_refs.is_empty());
        assert_eq!(outcome.retained.len(), 2);
        assert!(outcome.retained.iter().any(|retained| {
            retained.branch == "feature/old-ancestor"
                && retained.reason == RetainReason::NoUniqueCommit
        }));
        assert!(old.path.exists());
        assert!(unmerged.path.exists());
        assert_eq!(
            fs::read_to_string(old.path.join("ignored.txt"))?,
            "old branch data\n"
        );
        assert_eq!(
            fs::read_to_string(unmerged.path.join("ignored.txt"))?,
            "unmerged branch data\n"
        );
        Ok(())
    }

    #[test]
    fn gitlinks_make_remove_and_prune_require_review() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let direct = create(
            &context,
            WorktreeCreate {
                branch: "feature/direct-gitlink".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        let direct_marker = fixture._temp.path().join("direct-filter-ran");
        let direct_poison = add_poisoned_gitlink(&fixture, &direct.path, "sub", &direct_marker)?;
        assert_raw_git_runs_poison_filter(&fixture, &direct_poison)?;

        let merged = create(
            &context,
            WorktreeCreate {
                branch: "feature/merged-gitlink".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        fs::write(merged.path.join("merged.txt"), "merged feature\n")?;
        run_in_worktree(&fixture, &merged.path, &["add", "merged.txt"])?;
        run_in_worktree(&fixture, &merged.path, &["commit", "-m", "feature commit"])?;
        fixture.run(&[
            "merge",
            "--no-ff",
            "feature/merged-gitlink",
            "-m",
            "merge feature",
        ])?;
        let merged_marker = fixture._temp.path().join("merged-filter-ran");
        let merged_poison = add_poisoned_gitlink(&fixture, &merged.path, "sub", &merged_marker)?;
        assert_raw_git_runs_poison_filter(&fixture, &merged_poison)?;

        direct_poison.arm()?;
        merged_poison.arm()?;
        let listed = list(&context, &cancellation)?;
        assert!(
            !direct_marker.exists(),
            "worktree listing ran a nested clean filter"
        );
        assert!(
            !merged_marker.exists(),
            "worktree listing ran a nested clean filter"
        );
        for branch in ["feature/direct-gitlink", "feature/merged-gitlink"] {
            assert!(listed.iter().any(|entry| {
                entry.branch.as_deref() == Some(format!("refs/heads/{branch}").as_str())
                    && entry.dirty == WorktreeCondition::Unknown
            }));
        }
        direct_poison.arm()?;
        merged_poison.arm()?;
        assert!(matches!(
            remove(
                &context,
                WorktreeRemove::Name("feature/direct-gitlink".to_owned()),
                &cancellation,
            ),
            Err(WorktreeError::UnknownWriter)
        ));
        assert!(
            !direct_marker.exists(),
            "worktree removal ran a nested clean filter"
        );
        direct_poison.arm()?;
        merged_poison.arm()?;
        let pruned = remove(
            &context,
            WorktreeRemove::Merged {
                integration: "main".to_owned(),
                scope: PruneScope::Branches(vec!["feature/merged-gitlink".to_owned()]),
            },
            &cancellation,
        )?;
        assert!(pruned.removed.is_empty());
        assert!(pruned.pruned_refs.is_empty());
        assert_eq!(pruned.retained.len(), 1);
        assert_eq!(pruned.retained[0].reason, RetainReason::UnknownWriter);
        assert!(
            !merged_marker.exists(),
            "worktree pruning ran a nested clean filter"
        );
        assert!(direct.path.join("sub/.git").exists());
        assert!(merged.path.join("sub/.git").exists());
        assert!(!direct_marker.exists());
        assert!(!merged_marker.exists());
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
}
