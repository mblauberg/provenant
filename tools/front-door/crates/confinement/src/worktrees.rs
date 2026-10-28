//! Repository-scoped linked-worktree lifecycle and merged-ref pruning.

mod context;
mod gc;
mod owner;
pub use context::with_context;
pub use gc::{WorktreeGcPolicy, WorktreeGcReport, collect};
pub use owner::{
    WorktreeOrigin, WorktreeOwner, adopt, load_owner, prune_owned, save_lane_owner_when_free,
    save_owner,
};

use crate::repo::{
    GitRefName, GitRevision, RepoError, VerifiedRepo, VerifiedWorktree, verify_worktree,
};
use crate::status::{StatusKind, parse_index_entries, parse_porcelain_v2_z};
use crate::utility::{
    GitMutation, GitRead, UtilityBudget, UtilityError, UtilityRuntime, execute_git_mutation,
    run_utility,
};
use provenant_config::ProjectBinding;
use provenant_core::Clock;
use provenant_core::{CancelToken, GitOid, ProjectId, RelativePath, WaitBudget, hex};
use provenant_os::exec::{ExecOutcome, Executor, Termination};
use provenant_os::fs::{
    Directory, FsError, LockGuard, LockMode, Metadata, PublishMode, canonicalize,
};
use provenant_os::pid::{CwdObservation, CwdObserver, SystemCwdObserver};
use provenant_records::{PathWire, PathWireError};
use provenant_supervisor::census::{
    Ancestry, ProcessFact, ProcessLookup, ProcessObservation, ProcessObserver,
    SystemProcessObserver, current_process,
};
use provenant_supervisor::custody::WriterBinding;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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
    #[error("base {base} did not resolve to a commit")]
    BaseNotFound { base: String },
    #[error("worktree registration is stale; git worktree prune clears it")]
    StaleRegistration,
    #[error("worktree is dirty")]
    Dirty,
    #[error("worktree has undeclared ignored paths: {0}")]
    IgnoredPaths(String),
    #[error("worktree has an in-progress Git operation")]
    GitOperationInProgress,
    #[error("worktree writer is live")]
    LiveWriter,
    #[error(
        "incomplete worker census or ancestry; retry `provenant land` when process observation is complete"
    )]
    IncompleteCensus,
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
    #[error("primary checkout HEAD moved during worktree creation")]
    PrimaryMoved,
    #[error("worktree result path cannot be represented safely")]
    PathEncoding,
    #[error("worktree creation failed: {cause}; {}", if *.rolled_back {
        "the partial checkout and new branch were removed, so it is safe to retry"
    } else {
        "the partial checkout or branch it made could not be fully removed; inspect .worktrees and the branch before retrying"
    })]
    CreateFailed {
        cause: Box<WorktreeError>,
        rolled_back: bool,
    },
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
    pub home: &'a Path,
    pub repo: &'a VerifiedRepo,
    pub binding: &'a ProjectBinding,
    pub project_state: &'a Directory,
    pub executor: &'a dyn Executor,
    pub utility: &'a UtilityRuntime,
    pub workers: &'a dyn WorkerObserver,
    pub clock: &'a dyn Clock,
    /// Bounds Git reads. Keep it short: a stuck read is a defect to surface.
    pub utility_budget: UtilityBudget,
    /// Bounds Git mutations. A checkout writes the whole tree and can
    /// legitimately take minutes on a large repository, so it is far longer.
    pub mutation_budget: UtilityBudget,
    pub lock_budget: WaitBudget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeCreate {
    pub branch: String,
    pub base: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeRemove {
    /// Remove one clean worktree. Its branch is deleted when merged into the
    /// primary checkout's integration branch; `discard` also deletes it when
    /// unmerged.
    Name {
        name: String,
        discard: bool,
        /// Integration ref to check containment against; the primary's branch
        /// when absent.
        integration: Option<String>,
        /// The HEAD the caller verified. Removal refuses, under the worktree
        /// lock, when the worktree is no longer at it.
        expected_head: Option<GitOid>,
    },
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
    /// Only ignored files, such as build output, are present. Removal still
    /// refuses it; a hand-off treats it as clean.
    Ignored,
    Dirty,
    GitOperationInProgress,
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
    GitOperationInProgress,
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
    /// Unmerged branches deleted with `discard`. `head` is the commit the
    /// branch pointed at, which recovers it while Git still holds the object.
    pub discarded: Vec<DiscardedRef>,
    pub retained: Vec<RetainedBranch>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiscardedRef {
    pub branch: String,
    pub head: GitOid,
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

    /// Landing requires positive observation of a free root. Ordinary removal
    /// still relies on writer custody when the host census is unobservable.
    fn requires_complete_census(&self) -> bool {
        false
    }

    /// Landing may remove its caller's checkout. Exclude only the caller and
    /// its verified ancestors through the session leader, never other workers.
    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        self.observe_roots(roots)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWorkerObserver;

impl WorkerObserver for SystemWorkerObserver {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        system_worker_roots(roots, false)
    }

    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        system_worker_roots(roots, true)
    }
}

fn system_worker_roots(roots: &[PathBuf], ancestors: bool) -> BTreeMap<PathBuf, WorkerState> {
    let ProcessObservation::Present(current) = current_process() else {
        return roots
            .iter()
            .cloned()
            .map(|root| (root, WorkerState::Unknown))
            .collect();
    };
    observe_worker_roots(
        roots,
        &SystemProcessObserver,
        &SystemCwdObserver,
        &current,
        ancestors,
    )
}

fn observe_worker_roots(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
) -> BTreeMap<PathBuf, WorkerState> {
    let mut states: BTreeMap<_, _> = roots
        .iter()
        .cloned()
        .map(|root| (root, WorkerState::Free))
        .collect();
    if roots.is_empty() {
        return states;
    }
    let Ok(census) = observer.census(65_536) else {
        return roots
            .iter()
            .cloned()
            .map(|root| (root, WorkerState::Unknown))
            .collect();
    };
    let mut unknown_all = !census.is_complete();
    let mut excluded = BTreeSet::from([current.identity.clone()]);
    if ancestors {
        let mut child = current;
        while child.identity.pid != current.identity.session_id {
            let ProcessLookup::Present(parent) = census.process(child.parent_pid) else {
                unknown_all = true;
                break;
            };
            if parent.identity.boot != current.identity.boot
                || parent.identity.session_id != current.identity.session_id
                || census.ancestry(&child.identity, &parent.identity) != Ancestry::Yes
                || !excluded.insert(parent.identity.clone())
            {
                unknown_all = true;
                break;
            }
            child = parent;
        }
    }
    // An incomplete census must not hide positively observed live workers.
    for fact in census.facts() {
        if excluded.contains(&fact.identity) {
            continue;
        }
        match observe_same_process(observer, cwd_observer, fact) {
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
pub(crate) struct GitWorktree {
    pub(crate) path: PathBuf,
    pub(crate) head: GitOid,
    pub(crate) branch: Option<GitRefName>,
    locked: bool,
    prunable: bool,
    pub(crate) bare: bool,
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

pub(crate) fn parse_worktree_list(bytes: &[u8]) -> Result<Vec<GitWorktree>, WorktreeError> {
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
        // A prunable registration names a path that Git has already observed
        // as missing. It has no canonical filesystem path to protect here.
        .filter(|entry| !entry.bare && !entry.prunable)
        .map(|entry| Ok((canonicalize(&entry.path)?, entry)))
        .collect()
}

fn path_digest(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_bytes());
    hex(&digest)
}

/// The registry entry name for the canonical worktree `path`, below
/// `<state>/worktrees/<project>/`.
pub fn state_dir_name(path: &Path) -> OsString {
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

fn read_bounded(directory: &Directory, name: &OsStr, limit: u64) -> Result<Vec<u8>, FsError> {
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
    directory: &Directory,
    identity: &WorktreeIdentity,
    mode: PublishMode,
) -> Result<(), WorktreeError> {
    let bytes = serde_json::to_vec(identity).map_err(|_| WorktreeError::InvalidOutput)?;
    directory
        .atomic_write(OsStr::new(IDENTITY), ".identity-", &bytes, mode)?
        .require_durable()
        .map_err(|_| WorktreeError::RegistrationUnconfirmed)
}

fn writer_state(
    context: &WorktreeContext<'_>,
    path: &Path,
    branch: &GitRefName,
    root_identity: Metadata,
) -> WorkerState {
    let name = state_dir_name(path);
    let directory = match context.project_state.open_verified_directory(&name) {
        Ok(directory) => directory,
        Err(_) => return WorkerState::Unknown,
    };
    let bytes = match read_bounded(&directory, OsStr::new(IDENTITY), MAX_STATE_RECORD) {
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
    match directory.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive) {
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

/// A writer must pass this check before its terminal result is accepted.
/// Index and working-tree cleanliness do not prove a Git operation has ended.
pub fn check_writer_terminal(repo: &VerifiedRepo) -> Result<(), WorktreeError> {
    repo.revalidate()?;
    for marker in [
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "sequencer",
    ] {
        match repo.private_dir().metadata_at(OsStr::new(marker), false) {
            Ok(_) => return Err(WorktreeError::GitOperationInProgress),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    repo.revalidate()?;
    Ok(())
}

fn worktree_dirty_at(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<WorktreeCondition, WorktreeError> {
    match check_writer_terminal(repo) {
        Ok(()) => {}
        Err(WorktreeError::GitOperationInProgress) => {
            return Ok(WorktreeCondition::GitOperationInProgress);
        }
        Err(error) => return Err(error),
    }
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
    } else if records
        .iter()
        .all(|record| record.kind == StatusKind::Ignored)
    {
        WorktreeCondition::Ignored
    } else {
        WorktreeCondition::Dirty
    })
}

struct LockedWorktree {
    verified: VerifiedWorktree,
    state: Directory,
    identity: WorktreeIdentity,
    writer: Option<LockGuard>,
}

fn read_worktree_identity(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    require_active: bool,
) -> Result<(VerifiedWorktree, Directory, WorktreeIdentity), WorktreeError> {
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
    let bytes = read_bounded(&state, OsStr::new(IDENTITY), MAX_STATE_RECORD)
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

/// Turns a verified, registered worktree into the supervisor's writer custody:
/// its `state/worktrees/<ProjectId>/<digest>` directory and a binding that
/// names the worktree and Git directory by identity, never by raw path. The
/// registration must still be active, for this project and branch, so a
/// launch cannot claim custody of a worktree Provenant did not create. A
/// missing state record is `NotFound`; every other failure is `UnknownWriter`.
pub fn writer_custody(
    project_state: &Directory,
    project: ProjectId,
    worktree: &VerifiedWorktree,
    branch: &GitRefName,
    base: GitOid,
) -> Result<(Directory, WriterBinding), WorktreeError> {
    worktree.repo().revalidate()?;
    let canonical = canonicalize(worktree.path())?;
    // No state record at all means `provenant worktree create` never made this
    // tree; callers report that apart from a damaged or stale record.
    let state = project_state
        .open_verified_directory(&state_dir_name(&canonical))
        .map_err(|error| {
            if error.is_not_found() {
                WorktreeError::NotFound
            } else {
                WorktreeError::UnknownWriter
            }
        })?;
    let bytes = read_bounded(&state, OsStr::new(IDENTITY), MAX_STATE_RECORD)
        .map_err(|_| WorktreeError::UnknownWriter)?;
    let identity: WorktreeIdentity =
        serde_json::from_slice(&bytes).map_err(|_| WorktreeError::UnknownWriter)?;
    let root = worktree.identity();
    if identity.schema_version != WORKTREE_IDENTITY_SCHEMA
        || !identity.active
        || identity.project_id != project
        || identity.root_device != root.device
        || identity.root_inode != root.inode
        || identity.path_digest != path_digest(&canonical)
        || identity.branch != branch.as_str()
    {
        return Err(WorktreeError::UnknownWriter);
    }
    let binding = WriterBinding::from_verified(
        worktree.repo().root_dir(),
        worktree.repo().private_dir(),
        branch.as_str(),
        base,
    )
    .map_err(|_| WorktreeError::IdentityChanged)?;
    Ok((state, binding))
}

fn ensure_unreserved(state: &Directory) -> Result<(), WorktreeError> {
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
    ensure_unreserved(&state)?;
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
    ensure_unreserved(&state)?;
    let canonical = canonicalize(verified.path())?;
    let observations = context
        .workers
        .observe_roots(std::slice::from_ref(&canonical));
    // The writer lock and the absence of a reservation are the custody proof.
    // An unobservable process does not make a free lease live, since the
    // native census marks every root unknown when one process's directory
    // cannot be read. Only a positive observation of a live process refuses.
    match observations
        .get(&canonical)
        .copied()
        .unwrap_or(WorkerState::Unknown)
    {
        WorkerState::Unknown if context.workers.requires_complete_census() => {
            Err(WorktreeError::IncompleteCensus)
        }
        WorkerState::Free | WorkerState::Unknown => Ok(LockedWorktree {
            verified,
            state,
            identity,
            writer: Some(writer),
        }),
        WorkerState::Live => Err(WorktreeError::LiveWriter),
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
    // A stale Git registration must not hide the remaining worktrees from GC.
    let candidates: Vec<_> = entries
        .iter()
        .filter(|entry| !entry.bare)
        .filter_map(|entry| canonicalize(&entry.path).ok().map(|path| (path, entry)))
        .collect();
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
                        writer_state(context, &path, branch, worktree.identity()),
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

pub(crate) fn combine_worker_state(state: WorkerState, native: WorkerState) -> WriterState {
    match (state, native) {
        (WorkerState::Live, _) | (_, WorkerState::Live) => WriterState::Live,
        (WorkerState::Unknown, _) | (_, WorkerState::Unknown) => WriterState::Unknown,
        (WorkerState::Free, WorkerState::Free) => WriterState::Free,
    }
}

/// Registration observation for disk reports. Missing or unsafe state stays
/// unknown; existing writer locks are opened read-only and never created.
pub(crate) fn disk_writer_state(
    binding: &ProjectBinding,
    project_state: &Directory,
    path: &Path,
    branch: Option<&str>,
) -> WorkerState {
    cache_writer_guard(binding, project_state, path, branch)
        .map(|_guard| WorkerState::Free)
        .unwrap_or_else(|state| state)
}

/// Hold writer exclusion for an idle cache. Missing or unsafe registration and
/// reservations are unobservable; a busy existing lock identifies a live writer.
pub fn cache_writer_guard(
    binding: &ProjectBinding,
    project_state: &Directory,
    path: &Path,
    branch: Option<&str>,
) -> Result<LockGuard, WorkerState> {
    let observe = || -> Result<LockGuard, WorktreeError> {
        let directory = project_state.open_verified_directory(&state_dir_name(path))?;
        let bytes = read_bounded(&directory, OsStr::new(IDENTITY), MAX_STATE_RECORD)?;
        let identity: WorktreeIdentity =
            serde_json::from_slice(&bytes).map_err(|_| WorktreeError::UnknownWriter)?;
        let root = Directory::open_anchor(path)?.metadata()?;
        if identity.schema_version != WORKTREE_IDENTITY_SCHEMA
            || !identity.active
            || identity.project_id != binding.project_id()
            || identity.root_device != root.device
            || identity.root_inode != root.inode
            || identity.path_digest != path_digest(path)
            || Some(identity.branch.as_str()) != branch
        {
            return Err(WorktreeError::UnknownWriter);
        }
        let unreserved = || {
            matches!(directory.metadata_at(OsStr::new(RESERVATION), false),
            Err(error) if error.is_not_found())
        };
        match directory.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive) {
            Ok(guard) if unreserved() => Ok(guard),
            Err(FsError::LockTimeout { .. }) => Err(WorktreeError::LiveWriter),
            _ => Err(WorktreeError::UnknownWriter),
        }
    };
    observe().map_err(|error| match error {
        WorktreeError::LiveWriter => WorkerState::Live,
        _ => WorkerState::Unknown,
    })
}

/// Create a clean linked checkout at a pinned commit. A failed or killed
/// `worktree add` is rolled back (see `roll_back_failed_add`). Failed
/// registration after Git succeeds leaves a visible tree with unknown state;
/// no guessed rollback.
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
        .filter(|entry| !entry.prunable)
        .any(|entry| entry.branch.as_ref() == Some(&branch));
    if checked_out {
        return Err(if request.base.is_some() {
            WorktreeError::InvalidRequest
        } else {
            WorktreeError::CheckedOut
        });
    }
    if entries
        .iter()
        .any(|entry| entry.prunable && entry.branch.as_ref() == Some(&branch))
    {
        return Err(WorktreeError::StaleRegistration);
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
    if root_identity.uid != provenant_os::fs::effective_uid()
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
            ensure_unreserved(&state)?;
            let bytes = read_bounded(&state, OsStr::new(IDENTITY), MAX_STATE_RECORD)
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
            ensure_unreserved(&state)?;
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
    let primary_head = primary_head(context, cancel)?;
    if let Err(cause) = execute_mutation(
        context,
        GitMutation::WorktreeAdd {
            branch: branch.clone(),
            base: create_branch.then_some(base),
            path: target.clone(),
            create_branch,
        },
        cancel,
    ) {
        let rolled_back = roll_back_failed_add(
            context,
            &target,
            create_branch.then_some((&branch, base)),
            cancel,
        );
        return Err(WorktreeError::CreateFailed {
            cause: Box::new(cause),
            rolled_back,
        });
    }
    // Creating a linked worktree must leave the primary's HEAD, branch or
    // detached, exactly as it was (#1331). A hook or race that moved it is
    // reported rather than hidden; the worktree stays for the operator.
    if primary_head != self::primary_head(context, cancel)? {
        return Err(WorktreeError::PrimaryMoved);
    }
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
            ensure_unreserved(&state)?;
            (state, writer)
        }
    };
    let identity = identity_for(context, &target, &branch, verified.identity());
    write_identity(&state, &identity, PublishMode::Replace)?;
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

/// Undo a `worktree add` that failed or was killed part-way. Only what this
/// call made is touched: `target` was verified absent before the add, its
/// admin entry is found by the `gitdir` file that points at it, and the branch
/// goes only when this call created it, it still points at `base` and no
/// worktree has it checked out. Anything that cannot be confirmed is left and
/// reported as not rolled back. Returns whether nothing of ours remains.
fn roll_back_failed_add(
    context: &WorktreeContext<'_>,
    target: &Path,
    created_branch: Option<(&GitRefName, GitOid)>,
    cancel: &CancelToken,
) -> bool {
    let mut clean = remove_admin_entry(context.repo.common_git_dir(), target);
    clean &= match std::fs::remove_dir_all(target) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    let Some((branch, base)) = created_branch else {
        return clean;
    };
    let Ok(entries) = read_worktrees(context, cancel) else {
        return false;
    };
    if entries
        .iter()
        .any(|entry| entry.branch.as_ref() == Some(branch))
    {
        return false;
    }
    match read_branch_tips(context, cancel) {
        Ok(tips) => match tips.get(branch) {
            None => clean,
            Some(tip) if *tip == base => {
                clean
                    && execute_mutation(
                        context,
                        GitMutation::DeleteRef {
                            name: branch.clone(),
                            expected_old_oid: base,
                        },
                        cancel,
                    )
                    .is_ok()
            }
            Some(_) => false,
        },
        Err(_) => false,
    }
}

/// Remove the `<common git dir>/worktrees/<name>` entry whose `gitdir` names
/// `target`. Returns false when a matching entry could not be removed.
fn remove_admin_entry(common_git_dir: &Path, target: &Path) -> bool {
    let wanted = target.join(".git");
    let Ok(entries) = std::fs::read_dir(common_git_dir.join("worktrees")) else {
        return true;
    };
    let Ok(entries) = entries
        .take(MAX_WORKTREES + 1)
        .collect::<std::io::Result<Vec<_>>>()
    else {
        return false;
    };
    if entries.len() > MAX_WORKTREES {
        return false;
    }
    let mut removed = true;
    for entry in entries {
        let Ok(text) = std::fs::read(entry.path().join("gitdir")) else {
            continue;
        };
        if text.trim_ascii_end() == wanted.as_os_str().as_encoded_bytes() {
            removed &= std::fs::remove_dir_all(entry.path()).is_ok();
        }
    }
    removed
}

/// The primary checkout's `HEAD` file (it lives in the common git dir, so a
/// linked worktree finds it too) and, when symbolic, the commit it resolves to,
/// so a reset or commit on the same branch also counts as a move. Read
/// failures propagate: an unreadable snapshot must not disable the check.
fn primary_head(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<(Vec<u8>, Option<GitOid>), WorktreeError> {
    let bytes = std::fs::read(context.repo.common_git_dir().join("HEAD"))
        .map_err(|_| WorktreeError::InvalidOutput)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let Some(name) = text.trim_end().strip_prefix("ref: ") else {
        return Ok((bytes, None));
    };
    let name = GitRefName::new(name).map_err(|_| WorktreeError::InvalidOutput)?;
    let outcome = run_git(context, context.repo, GitRead::ResolveRef { name }, cancel)?;
    let commit = if exit_code(&outcome)? == 0 {
        Some(parse_oid(trim_ascii(&outcome.stdout.bytes))?)
    } else {
        None
    };
    Ok((bytes, commit))
}

fn pin_revision(
    context: &WorktreeContext<'_>,
    revision: &str,
    cancel: &CancelToken,
) -> Result<GitOid, WorktreeError> {
    let parsed = GitRevision::new(revision).map_err(|_| WorktreeError::InvalidRequest)?;
    let resolve = || {
        run_git(
            context,
            context.repo,
            GitRead::ResolveRevision {
                revision: parsed.clone(),
            },
            cancel,
        )
    };
    let mut outcome = resolve()?;
    if exit_code(&outcome)? != 0 {
        crate::utility::fetch_missing_remote_branch(
            context.executor,
            context.repo,
            context.utility,
            context.home,
            revision,
            context.mutation_budget,
            cancel,
        )?;
        outcome = resolve()?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::BaseNotFound {
                base: revision.to_owned(),
            });
        }
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
        context.mutation_budget,
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
    lane_cache: Option<PathBuf>,
    _writer: LockGuard,
}

fn matching_worktree<'a>(
    entries: &'a [GitWorktree],
    branch: &GitRefName,
    repo_root: &Path,
) -> Result<Option<&'a GitWorktree>, WorktreeError> {
    let repo_root = canonicalize(repo_root)?;
    let mut found = None;
    for entry in entries
        .iter()
        .filter(|entry| !entry.prunable && entry.branch.as_ref() == Some(branch))
    {
        let path = canonicalize(&entry.path)?;
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
    for entry in entries
        .iter()
        .filter(|entry| !entry.prunable && entry.branch.as_ref() == Some(branch))
    {
        if allowed.is_none() {
            return Ok(true);
        }
        let path = canonicalize(&entry.path)?;
        if allowed.as_ref() != Some(&path) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn tombstone_identity(locked: &LockedWorktree) -> Result<(), WorktreeError> {
    let mut identity = locked.identity.clone();
    identity.active = false;
    write_identity(&locked.state, &identity, PublishMode::Replace)
}

fn remove_tree(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    expected_head: GitOid,
    cancel: &CancelToken,
    output: &provenant_config::ProjectGcSettings,
) -> Result<RemovedTree, WorktreeError> {
    remove_tree_when(
        context,
        entry,
        expected_head,
        cancel,
        &|| true,
        Some(output),
    )
}

fn remove_tree_when(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    expected_head: GitOid,
    cancel: &CancelToken,
    eligible: &dyn Fn() -> bool,
    output: Option<&provenant_config::ProjectGcSettings>,
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
    if !eligible() {
        return Err(WorktreeError::UnknownWriter);
    }
    if worktree_dirty_at(context, locked.verified.repo(), cancel)? == WorktreeCondition::Ignored {
        let settings = output.cloned().unwrap_or_default();
        gc::prepare_removal(
            context,
            locked.verified.repo(),
            &settings.regenerable,
            &settings.keep,
            cancel,
        )?;
    }
    match worktree_dirty_at(context, locked.verified.repo(), cancel)? {
        WorktreeCondition::Clean => {}
        WorktreeCondition::Ignored | WorktreeCondition::Dirty => {
            return Err(WorktreeError::Dirty);
        }
        WorktreeCondition::GitOperationInProgress => {
            return Err(WorktreeError::GitOperationInProgress);
        }
        WorktreeCondition::Unknown => return Err(WorktreeError::UnknownWriter),
    }
    // A cache that cannot be derived must not block removal; warn and skip it.
    let cargo_cache = crate::permissions::worktree_cargo_cache(context.home, &locked.verified)
        .map_err(|error| warn_cache_removal(&error))
        .ok();
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
    // Keep writer exclusion through cache detachment and verified deletion.
    // Cleanup remains best-effort after Git has removed the worktree.
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
        lane_cache: cargo_cache.map(|cache| cache.path),
        _writer: writer,
    })
}

#[allow(clippy::print_stderr)]
fn warn_cache_removal(error: &crate::permissions::PermissionError) {
    eprintln!("warning: could not remove worktree lane cache: {error}");
}

/// Pins an integration ref given by name. A local branch wins, then a
/// remote-tracking ref such as `origin/main`, so a stale local branch does not
/// hide a merge that landed on the remote.
fn pin_integration(
    context: &WorktreeContext<'_>,
    text: &str,
    cancel: &CancelToken,
) -> Result<(GitRefName, GitOid), WorktreeError> {
    let candidates = [
        GitRefName::new(text),
        GitRefName::branch(text),
        GitRefName::remote_tracking(text),
    ];
    let mut invalid = true;
    for candidate in candidates.into_iter().flatten() {
        invalid = false;
        match resolve_ref(context, candidate.clone(), cancel) {
            Ok(oid) => return Ok((candidate, oid)),
            Err(WorktreeError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Err(if invalid {
        WorktreeError::InvalidRequest
    } else {
        WorktreeError::NotFound
    })
}

/// The worktree a name refers to: its branch, or its directory name under
/// `.worktrees/`. Two different worktrees matching one name are ambiguous.
fn named_worktree<'a>(
    entries: &'a [GitWorktree],
    name: &str,
    root: &Path,
) -> Result<&'a GitWorktree, WorktreeError> {
    let branch = GitRefName::branch(name).ok();
    let repo_root = canonicalize(root)?;
    let mut found: Option<&GitWorktree> = None;
    for (path, entry) in canonical_worktree_paths(entries)? {
        let by_branch = branch
            .as_ref()
            .is_some_and(|branch| entry.branch.as_ref() == Some(branch));
        let by_directory = path.file_name() == Some(OsStr::new(name));
        if !by_branch && !by_directory {
            continue;
        }
        if path == repo_root {
            return Err(WorktreeError::Primary);
        }
        match found {
            Some(first) if !std::ptr::eq(first, entry) => {
                return Err(WorktreeError::InvalidRequest);
            }
            _ => found = Some(entry),
        }
    }
    if let Some(entry) = found {
        return Ok(entry);
    }
    if entries.iter().any(|entry| {
        entry.prunable
            && (branch
                .as_ref()
                .is_some_and(|branch| entry.branch.as_ref() == Some(branch))
                || entry.path.file_name() == Some(OsStr::new(name)))
    }) {
        return Err(WorktreeError::StaleRegistration);
    }
    Err(WorktreeError::NotFound)
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
        WorktreeError::Dirty | WorktreeError::IgnoredPaths(_) => RetainReason::Dirty,
        WorktreeError::GitOperationInProgress => RetainReason::GitOperationInProgress,
        WorktreeError::LiveWriter => RetainReason::LiveWriter,
        WorktreeError::UnknownWriter
        | WorktreeError::Cancelled
        | WorktreeError::IncompleteCensus => RetainReason::UnknownWriter,
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

/// Which removal asked for the branch, which decides how contained branches
/// are treated.
#[derive(Clone, Copy, Debug)]
enum Policy {
    Sweep,
    Named { discard: bool },
}

/// What becomes of a candidate branch. Decided before any worktree is touched,
/// so an unmerged worktree is never removed by a merged sweep.
#[derive(Clone, Copy, Debug)]
enum Disposition {
    Keep(RetainReason),
    Merged,
    Discard,
}

fn keep_disposition(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    tip: GitOid,
    range: PruneRange<'_>,
    policy: Policy,
    cancel: &CancelToken,
) -> Disposition {
    let PruneRange {
        integration,
        tip: integration_tip,
    } = range;
    if branch == integration {
        return Disposition::Keep(RetainReason::IntegrationRef);
    }
    // A named removal deletes a branch whose commits are all contained in the
    // integration branch. A sweep keeps those, since a fresh lane branch at
    // the tip may be about to get work.
    if tip == integration_tip {
        return match policy {
            Policy::Sweep => Disposition::Keep(RetainReason::AtIntegrationTip),
            Policy::Named { .. } => Disposition::Merged,
        };
    }
    match is_ancestor(context, tip, integration_tip, cancel) {
        Ok(true) => {}
        Ok(false) => {
            return match policy {
                Policy::Named { discard: true } => Disposition::Discard,
                _ => Disposition::Keep(RetainReason::NotMerged),
            };
        }
        Err(error) => return Disposition::Keep(retain_reason(&error)),
    }
    if let Policy::Named { .. } = policy {
        return Disposition::Merged;
    }
    match is_first_parent_commit(context, integration_tip, tip, cancel) {
        Ok(false) => Disposition::Merged,
        Ok(true) => Disposition::Keep(RetainReason::NoUniqueCommit),
        Err(error) => Disposition::Keep(retain_reason(&error)),
    }
}

/// Delete the local ref once its worktree is gone, under the exact expected
/// tip, after rechecking that no other worktree has it checked out.
fn settle_branch(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    tip: GitOid,
    disposition: Disposition,
    cancel: &CancelToken,
    outcome: &mut RemoveOutcome,
) -> Result<(), WorktreeError> {
    let discard = match disposition {
        Disposition::Keep(reason) => {
            retain(outcome, branch, reason);
            return Ok(());
        }
        Disposition::Merged => false,
        Disposition::Discard => true,
    };
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
    }
    context.repo.revalidate()?;
    let deleted = execute_git_mutation(
        context.executor,
        context.repo,
        context.utility,
        GitMutation::DeleteRef {
            name: branch.clone(),
            expected_old_oid: tip,
        },
        context.mutation_budget,
        cancel,
    );
    if deleted.is_ok_and(|result| exit_code(&result).ok() == Some(0)) {
        let short = branch.short_name().to_owned();
        if discard {
            outcome.discarded.push(DiscardedRef {
                branch: short,
                head: tip,
            });
        } else {
            outcome.pruned_refs.push(short);
        }
    } else {
        retain(outcome, branch, RetainReason::Changed);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn remove_merged_candidate(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    range: PruneRange<'_>,
    entries: &[GitWorktree],
    branch_tips: &BTreeMap<GitRefName, GitOid>,
    cancel: &CancelToken,
    outcome: &mut RemoveOutcome,
    detached: &mut Vec<RemovedTree>,
    output: &provenant_config::ProjectGcSettings,
) -> Result<(), WorktreeError> {
    let Some(tip) = branch_tips.get(branch).copied() else {
        retain(outcome, branch, RetainReason::Changed);
        return Ok(());
    };
    let disposition = keep_disposition(context, branch, tip, range, Policy::Sweep, cancel);
    if let Disposition::Keep(reason) = disposition {
        retain(outcome, branch, reason);
        return Ok(());
    }
    let entry = match matching_worktree(entries, branch, context.repo.root_path()) {
        Ok(entry) => entry,
        Err(error) => {
            retain(outcome, branch, retain_reason(&error));
            return Ok(());
        }
    };
    // Held until the ref is settled, so the writer lock covers the deletion.
    let _held = match entry {
        None => None,
        Some(entry) => {
            if entry.head != tip {
                retain(outcome, branch, RetainReason::Changed);
                return Ok(());
            }
            if entry.locked || entry.prunable {
                retain(outcome, branch, RetainReason::LockedOrPrunable);
                return Ok(());
            }
            match remove_tree(context, entry, tip, cancel, output) {
                Ok(removed) => {
                    outcome.removed.push(removed.info.clone());
                    detached.push(removed);
                    Some(())
                }
                Err(error) => {
                    retain(outcome, branch, retain_reason(&error));
                    return Ok(());
                }
            }
        }
    };
    settle_branch(context, branch, tip, disposition, cancel, outcome)
}

/// The integration branch the primary checkout is on, pinned to its tip. A
/// detached primary names no integration branch, so removal by name refuses.
fn primary_integration(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<(GitRefName, GitOid), WorktreeError> {
    let bytes = std::fs::read(context.repo.common_git_dir().join("HEAD"))
        .map_err(|_| WorktreeError::InvalidOutput)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let name = text
        .trim_end()
        .strip_prefix("ref: ")
        .ok_or(WorktreeError::InvalidRequest)?;
    pin_branch_ref(context, name, cancel)
}

/// Read-only dirty/output prechecks before a caller releases recovered writer
/// custody. The caller holds the writer lock; removal rechecks after recovery.
pub fn check_removal_preconditions(
    context: &WorktreeContext<'_>,
    worktree: &VerifiedWorktree,
    output: &provenant_config::ProjectGcSettings,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    worktree.repo().revalidate()?;
    match worktree_dirty_at(context, worktree.repo(), cancel)? {
        WorktreeCondition::Clean => Ok(()),
        WorktreeCondition::Ignored => gc::precheck_removal(
            context,
            worktree.repo(),
            &output.regenerable,
            &output.keep,
            cancel,
        ),
        WorktreeCondition::Dirty => Err(WorktreeError::Dirty),
        WorktreeCondition::GitOperationInProgress => Err(WorktreeError::GitOperationInProgress),
        WorktreeCondition::Unknown => Err(WorktreeError::UnknownWriter),
    }
}

/// Remove one clean managed linked worktree, or prune a caller-scoped set of
/// merged managed worktrees and their exact expected-old local refs.
pub fn remove(
    context: &WorktreeContext<'_>,
    request: WorktreeRemove,
    cancel: &CancelToken,
) -> Result<RemoveOutcome, WorktreeError> {
    remove_with_output(context, request, cancel, &Default::default())
}

pub fn remove_with_output(
    context: &WorktreeContext<'_>,
    request: WorktreeRemove,
    cancel: &CancelToken,
    output: &provenant_config::ProjectGcSettings,
) -> Result<RemoveOutcome, WorktreeError> {
    validate_binding(context)?;
    let repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let mut detached = Vec::new();
    let result = (|| {
        context.repo.revalidate()?;
        let entries = read_worktrees(context, cancel)?;
        let mut outcome = RemoveOutcome::default();
        match request {
            WorktreeRemove::Name {
                name,
                discard,
                integration,
                expected_head,
            } => {
                let (integration, integration_tip) = match integration {
                    Some(text) => pin_integration(context, &text, cancel)?,
                    None => primary_integration(context, cancel)?,
                };
                let branch_tips = read_branch_tips(context, cancel)?;
                let entry = named_worktree(&entries, &name, context.repo.root_path())?;
                let branch = entry.branch.clone().ok_or(WorktreeError::InvalidRequest)?;
                let tip = branch_tips
                    .get(&branch)
                    .copied()
                    .filter(|tip| *tip == entry.head)
                    .ok_or(WorktreeError::IdentityChanged)?;
                // `remove_tree` rechecks this tip under the worktree lock, so
                // a commit made after the caller's check cannot be removed.
                if expected_head.is_some_and(|expected| expected != tip) {
                    return Err(WorktreeError::IdentityChanged);
                }
                let disposition = keep_disposition(
                    context,
                    &branch,
                    tip,
                    PruneRange {
                        integration: &integration,
                        tip: integration_tip,
                    },
                    Policy::Named { discard },
                    cancel,
                );
                // A dirty or live worktree fails here, before any branch changes.
                let removed = remove_tree(context, entry, tip, cancel, output)?;
                outcome.removed.push(removed.info.clone());
                detached.push(removed);
                settle_branch(context, &branch, tip, disposition, cancel, &mut outcome)?;
            }
            WorktreeRemove::Merged { integration, scope } => {
                let (integration, integration_tip) =
                    pin_integration(context, &integration, cancel)?;
                let branch_tips = read_branch_tips(context, cancel)?;
                let mut branches = BTreeMap::new();
                match scope {
                    PruneScope::Branches(names) => {
                        if names.is_empty() || names.len() > MAX_PRUNE_BRANCHES {
                            return Err(WorktreeError::InvalidRequest);
                        }
                        for name in names {
                            let branch = GitRefName::branch(name)
                                .map_err(|_| WorktreeError::InvalidRequest)?;
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
                        &mut detached,
                        output,
                    )?;
                }
            }
        }
        Ok(outcome)
    })();
    drop(repository);
    for removed in detached {
        if let Some(cache) = removed.lane_cache.as_deref()
            && let Err(error) = crate::cache::prune_worktree_cargo_cache(context.home, cache)
        {
            warn_cache_removal(&error);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_admin_entry_handles_large_directory_and_keeps_other_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let common = temporary.path();
        let admin = common.join("worktrees");
        std::fs::create_dir(&admin)?;
        let target = common.join("target");
        for index in 0..1024 {
            let entry = admin.join(format!("entry-{index:04}"));
            std::fs::create_dir(&entry)?;
            std::fs::write(
                entry.join("gitdir"),
                target.join(".git").as_os_str().as_encoded_bytes(),
            )?;
        }
        let keep = admin.join("keep");
        std::fs::create_dir(&keep)?;
        std::fs::write(keep.join("gitdir"), b"/another/worktree/.git")?;
        assert!(remove_admin_entry(common, &target));
        assert_eq!(std::fs::read_dir(&admin)?.count(), 1);
        assert!(keep.is_dir());
        Ok(())
    }

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
    use provenant_os::clock::SystemClock;
    use provenant_os::fs::Directory;
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct FreeWorkers;

    #[test]
    fn lane_owner_reassignment_holds_custody_and_preserves_adopted_owners()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "lane-owner".into(),
                base: None,
            },
            &cancel(),
        )?;
        let tree = verify_worktree(&fixture.repo, &info.path)?;
        let mut owner = WorktreeOwner {
            attempt_id: None,
            operation_id: Some("first-owner".parse()?),
            branch: "lane-owner".into(),
            base: info.head.to_string(),
            host: provenant_core::HostId::from_bytes([3; 16]),
            created_at: context.clock.wall_now()?,
            origin: WorktreeOrigin::Lane,
            label: None,
        };
        save_lane_owner_when_free(&context, &tree, &owner, &cancel())?;
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&info.path))?;
        let held = state.lock(
            OsStr::new(WRITER_LOCK),
            LockMode::Exclusive,
            WaitBudget::nonblocking(context.clock.mono_now()),
            &cancel(),
            context.clock,
        )?;
        let next = WorktreeOwner {
            operation_id: Some("next-owner".parse()?),
            ..owner.clone()
        };
        assert!(matches!(
            save_lane_owner_when_free(&context, &tree, &next, &cancel()),
            Err(WorktreeError::LiveWriter)
        ));
        assert_eq!(load_owner(&tree)?, Some(owner.clone()));
        drop(held);
        fs::write(info.path.join("dirty"), "keep")?;
        assert!(matches!(
            save_lane_owner_when_free(&context, &tree, &next, &cancel()),
            Err(WorktreeError::Dirty)
        ));
        fs::remove_file(info.path.join("dirty"))?;
        owner.origin = WorktreeOrigin::Adopted;
        owner.label = Some("manual".into());
        save_owner(&tree, &owner)?;
        assert!(save_lane_owner_when_free(&context, &tree, &next, &cancel()).is_err());
        assert_eq!(load_owner(&tree)?, Some(owner));
        Ok(())
    }

    #[test]
    fn adopted_owner_proves_membership_and_enables_custody() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let native = fixture.root.join(".worktrees/native");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native",
            native.to_str().ok_or("path")?,
        ])?;
        let tree = verify_worktree(&fixture.repo, &native)?;
        assert!(load_owner(&tree)?.is_none());
        let owner = WorktreeOwner {
            attempt_id: None,
            operation_id: None,
            branch: "native".into(),
            base: fixture.head()?.to_string(),
            host: provenant_core::HostId::from_bytes([3; 16]),
            created_at: context.clock.wall_now()?,
            origin: WorktreeOrigin::Adopted,
            label: Some("manual".into()),
        };
        let lane_owner = WorktreeOwner {
            origin: WorktreeOrigin::Lane,
            operation_id: Some("not-adopted".parse()?),
            ..owner.clone()
        };
        assert!(matches!(
            adopt(&context, &native, &lane_owner, &cancel()),
            Err(WorktreeError::InvalidRequest)
        ));
        assert!(load_owner(&tree)?.is_none());
        // A failed owner publication must not register custody identity.
        let owner_path = tree.repo().private_git_dir().join("provenant-owner.json");
        fs::create_dir(&owner_path)?;
        assert!(adopt(&context, &native, &owner, &cancel()).is_err());
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&native))?;
        assert!(
            state
                .read_bounded(OsStr::new(IDENTITY), MAX_STATE_RECORD)
                .is_err_and(|error| error.is_not_found())
        );
        fs::remove_dir(owner_path)?;
        adopt(&context, &native, &owner, &cancel())?;
        assert_eq!(load_owner(&tree)?, Some(owner.clone()));
        assert_eq!(
            list(&context, &cancel())?
                .into_iter()
                .find(|tree| tree.path == native)
                .ok_or("native missing")?
                .writer,
            WriterState::Free
        );
        let foreign = Fixture::new()?;
        let foreign_path = foreign.root.join(".worktrees/foreign");
        foreign.run(&[
            "worktree",
            "add",
            "-b",
            "foreign",
            foreign_path.to_str().ok_or("path")?,
        ])?;
        assert!(adopt(&context, &foreign_path, &owner, &cancel()).is_err());
        assert!(adopt(&context, &fixture.root, &owner, &cancel()).is_err());
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&native))?;
        state
            .atomic_write(
                OsStr::new(RESERVATION),
                ".test-",
                b"{}",
                PublishMode::Replace,
            )?
            .require_durable()?;
        assert!(adopt(&context, &native, &owner, &cancel()).is_err());
        Ok(())
    }

    #[test]
    fn owned_cleanup_rechecks_regenerated_output_and_keep_paths() -> Result<(), Box<dyn Error>> {
        for keep in [false, true] {
            let fixture = Fixture::new()?;
            fs::write(fixture.root.join(".git/info/exclude"), "build/\nsaved/\n")?;
            let test = TestContext::new(&fixture)?;
            let context = test.context();
            let info = create(
                &context,
                WorktreeCreate {
                    branch: "regenerated".into(),
                    base: None,
                },
                &cancel(),
            )?;
            let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
            let tree = verify_worktree(&fixture.repo, &info.path)?;
            save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(attempt),
                    operation_id: Some("regenerated-create".parse()?),
                    branch: "regenerated".into(),
                    base: info.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: context.clock.wall_now()?,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            )?;
            let output = info.path.join("saved/new");
            fs::create_dir_all(info.path.join("build"))?;
            fs::write(info.path.join("build/old"), "initial output")?;
            let calls = std::cell::Cell::new(0);
            let terminal = |_: &Path| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    // Recreate ignored output after collection, before final removal.
                    assert!(!info.path.join("build/old").exists());
                    assert!(fs::create_dir_all(output.parent().unwrap_or(&info.path)).is_ok());
                    assert!(fs::write(&output, "regenerated output").is_ok());
                }
                true
            };
            let kept = if keep {
                vec!["saved/new".into()]
            } else {
                vec![]
            };
            let policy = WorktreeGcPolicy {
                upstream: Some(info.head),
                terminal: &terminal,
                no_live_lanes: &|_| true,
                regenerable: &["build".into(), "saved".into()],
                keep: &kept,
                written_before_ns: i128::MAX,
                foreign_merged_clean: false,
                foreign_idle_before_ns: None,
                dry_run: false,
            };
            let result = prune_owned(
                &context,
                &info.path,
                attempt,
                info.head,
                info.head,
                &policy,
                &cancel(),
            );
            assert_eq!(calls.get(), 2);
            if keep {
                assert!(matches!(result, Err(WorktreeError::IgnoredPaths(_))));
                assert_eq!(fs::read_to_string(&output)?, "regenerated output");
            } else {
                assert_eq!(result?.removed.len(), 1);
                assert!(!info.path.exists());
            }
        }
        Ok(())
    }

    #[test]
    fn owned_cleanup_refuses_dirty_unmerged_and_adopted_trees() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let base = fixture.head()?;
        let info = create(
            &context,
            WorktreeCreate {
                branch: "owned".into(),
                base: None,
            },
            &cancel(),
        )?;
        let tree = verify_worktree(&fixture.repo, &info.path)?;
        let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
        let mut owner = WorktreeOwner {
            attempt_id: Some(attempt),
            operation_id: Some("owned-create".parse()?),
            branch: "owned".into(),
            base: base.to_string(),
            host: provenant_core::HostId::from_bytes([3; 16]),
            created_at: context.clock.wall_now()?,
            origin: WorktreeOrigin::Lane,
            label: None,
        };
        save_owner(&tree, &owner)?;
        let policy = WorktreeGcPolicy {
            upstream: Some(base),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            foreign_merged_clean: false,
            foreign_idle_before_ns: None,
            dry_run: false,
        };
        fs::write(info.path.join("dirty"), "keep")?;
        assert!(
            prune_owned(
                &context,
                &info.path,
                attempt,
                base,
                base,
                &policy,
                &cancel()
            )
            .is_err()
        );
        fs::remove_file(info.path.join("dirty"))?;
        owner.origin = WorktreeOrigin::Adopted;
        save_owner(&tree, &owner)?;
        assert!(
            prune_owned(
                &context,
                &info.path,
                attempt,
                base,
                base,
                &policy,
                &cancel()
            )
            .is_err()
        );
        owner.origin = WorktreeOrigin::Lane;
        save_owner(&tree, &owner)?;
        let denied = WorktreeGcPolicy {
            no_live_lanes: &|_| false,
            ..policy
        };
        assert!(
            prune_owned(
                &context,
                &info.path,
                attempt,
                base,
                base,
                &denied,
                &cancel()
            )
            .is_err()
        );
        assert!(info.path.exists());
        fixture.run(&[
            "-C",
            info.path.to_str().ok_or("path")?,
            "commit",
            "--allow-empty",
            "-m",
            "unmerged lane commit",
        ])?;
        let head = list(&context, &cancel())?
            .into_iter()
            .find(|lane| lane.path == info.path)
            .ok_or("lane missing")?
            .head;
        assert!(
            prune_owned(
                &context,
                &info.path,
                attempt,
                head,
                base,
                &policy,
                &cancel()
            )
            .is_err()
        );
        let pushed_policy = WorktreeGcPolicy {
            upstream: None,
            ..policy
        };
        let removed = prune_owned(
            &context,
            &info.path,
            attempt,
            head,
            head,
            &pushed_policy,
            &cancel(),
        )?;
        assert_eq!(removed.removed.len(), 1);
        assert!(!info.path.exists());
        assert!(removed.pruned_refs.is_empty());
        assert!(
            removed
                .retained
                .iter()
                .any(|retained| retained.branch == "owned")
        );
        assert!(read_branch_tips(&context, &cancel())?.contains_key(&GitRefName::branch("owned")?));
        Ok(())
    }

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
        project_state: Directory,
        workers: FreeWorkers,
        clock: SystemClock,
        lock_budget: WaitBudget,
        utility_budget: UtilityBudget,
        mutation_budget: UtilityBudget,
    }

    impl<'a> TestContext<'a> {
        fn new(fixture: &'a Fixture) -> Result<Self, Box<dyn Error>> {
            let binding_state = fixture._temp.path().join("bindings");
            fs::create_dir(&binding_state)?;
            let mut inputs =
                provenant_core::RootInputs::new(fixture.home.clone(), fixture.root.clone());
            inputs.state_dir = Some(binding_state);
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let binding = provenant_config::bind_project(
                &roots,
                provenant_config::ProjectLocation::new(fixture.repo.common_git_dir())?,
            )?;
            let state_root = fixture._temp.path().join("worktrees");
            fs::create_dir(&state_root)?;
            fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))?;
            let state_path = state_root.join(binding.project_id().to_string());
            fs::create_dir(&state_path)?;
            fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))?;
            let project_state = Directory::open_anchor(&state_path)?;
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
                mutation_budget: fixture.budget()?,
            })
        }

        fn context(&self) -> WorktreeContext<'_> {
            WorktreeContext {
                home: &self.fixture.home,
                repo: &self.fixture.repo,
                binding: &self.binding,
                project_state: &self.project_state,
                executor: &self.fixture.executor,
                utility: &self.fixture.runtime,
                workers: &self.workers,
                clock: &self.clock,
                utility_budget: self.utility_budget,
                mutation_budget: self.mutation_budget,
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

    /// Runs `git worktree add` to completion, then reports it timed out: the
    /// tree and branch exist, as when a slow checkout is killed part-way.
    struct TimesOutWorktreeAdd<'a>(&'a provenant_os::exec::SystemExecutor);

    struct ObservesFetch<'a> {
        executor: &'a dyn Executor,
        seen: std::sync::atomic::AtomicBool,
    }

    impl Executor for ObservesFetch<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options.argv.iter().any(|arg| arg == "fetch") {
                self.seen.store(true, std::sync::atomic::Ordering::Relaxed);
                assert!(
                    options
                        .argv
                        .iter()
                        .any(|arg| arg == "--no-auto-maintenance")
                );
            }
            self.executor.execute(options)
        }
    }

    impl Executor for TimesOutWorktreeAdd<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            let adds = options.argv.iter().any(|arg| arg == "add");
            let mut outcome = self.0.execute(options)?;
            if adds {
                outcome.timed_out = true;
                outcome.termination = Termination::Signal(15);
            }
            Ok(outcome)
        }
    }

    fn branch_exists(fixture: &Fixture, name: &str) -> bool {
        fixture
            .run(&["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")])
            .is_ok()
    }

    fn gc_merged_worktree(
        fixture: &Fixture,
        context: &WorktreeContext<'_>,
        name: &str,
    ) -> Result<WorktreeInfo, Box<dyn Error>> {
        let created = create(
            context,
            WorktreeCreate {
                branch: name.to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        run_in_worktree(
            fixture,
            &created.path,
            &["commit", "--allow-empty", "-m", name],
        )?;
        fixture.run(&["merge", "--no-ff", name, "-m", "merge GC fixture"])?;
        list(context, &cancel())?
            .into_iter()
            .find(|lane| lane.path == created.path)
            .ok_or_else(|| "missing GC fixture worktree".into())
    }

    #[test]
    fn gc_idle_output_requires_ignored_idle_and_free_writer() -> Result<(), Box<dyn Error>> {
        for scenario in ["idle", "recent", "lease", "tracked", "untracked", "lane"] {
            let fixture = Fixture::new()?;
            fs::write(
                fixture.root.join(".gitignore"),
                if scenario == "untracked" {
                    ".env\n"
                } else {
                    "target/\n.env\n"
                },
            )?;
            if scenario == "tracked" {
                fs::create_dir(fixture.root.join("target"))?;
                fs::write(fixture.root.join("target/source"), "tracked")?;
                fixture.run(&["add", "-f", "target/source"])?;
            }
            fixture.run(&["add", ".gitignore"])?;
            fixture.run(&["commit", "-m", "output rules"])?;
            let test_context = TestContext::new(&fixture)?;
            let context = test_context.context();
            let created = create(
                &context,
                WorktreeCreate {
                    branch: "idle-output".into(),
                    base: None,
                },
                &cancel(),
            )?;
            fs::create_dir_all(created.path.join("target"))?;
            fs::write(created.path.join("target/cache"), "output")?;
            let lanes = list(&context, &cancel())?;
            if scenario == "untracked" {
                // Call the pruning owner directly so the early dirty-lane gate
                // cannot mask the non-ignored output protection.
                let entries = read_worktrees(&context, &cancel())?;
                let entry = entries
                    .iter()
                    .find(|entry| entry.path == created.path)
                    .ok_or("worktree")?;
                let locked = lock_worktree_for_mutation(&context, entry, &cancel())?;
                let refused = gc::prepare_removal(
                    &context,
                    locked.verified.repo(),
                    &["target".into()],
                    &[],
                    &cancel(),
                );
                assert!(
                    matches!(refused, Err(WorktreeError::IgnoredPaths(ref paths)) if paths.contains("target"))
                );
                assert_eq!(
                    fs::read_to_string(created.path.join("target/cache"))?,
                    "output"
                );
            }
            let state = context
                .project_state
                .open_verified_directory(&state_dir_name(&created.path))?;
            let lease = if scenario == "lease" {
                Some(state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?)
            } else {
                None
            };
            let report = collect(
                &context,
                &lanes,
                &WorktreeGcPolicy {
                    foreign_idle_before_ns: None,
                    foreign_merged_clean: false,
                    upstream: None,
                    terminal: &|_| false,
                    no_live_lanes: &|_| scenario != "lane",
                    regenerable: &["target".into()],
                    keep: &[],
                    written_before_ns: if scenario == "recent" { 0 } else { i128::MAX },
                    dry_run: false,
                },
                &cancel(),
            )?;
            assert_eq!(
                created.path.join("target/cache").exists(),
                scenario != "idle",
                "{scenario}"
            );
            assert_eq!(
                report.idle_output.iter().filter(|row| row.removed).count(),
                usize::from(scenario == "idle"),
                "{scenario}"
            );
            assert!(created.path.is_dir());
            drop(lease);
        }
        Ok(())
    }

    #[test]
    fn gc_protects_nested_repositories_under_ignored_directories() -> Result<(), Box<dyn Error>> {
        for linked in [false, true] {
            for parent_output in [false, true] {
                let fixture = Fixture::new()?;
                fs::write(fixture.root.join(".gitignore"), ".claude/\ntarget/\n")?;
                fixture.run(&["add", ".gitignore"])?;
                fixture.run(&["commit", "-m", "ignore native worktrees"])?;
                let test_context = TestContext::new(&fixture)?;
                let context = test_context.context();
                let created = create(
                    &context,
                    WorktreeCreate {
                        branch: "nested-host".into(),
                        base: None,
                    },
                    &cancel(),
                )?;
                let parent = created.path.join(if parent_output {
                    "target"
                } else {
                    ".claude/worktrees"
                });
                fs::create_dir_all(&parent)?;
                let nested = parent.join("native");
                if linked {
                    fixture.run(&[
                        "worktree",
                        "add",
                        "-b",
                        "nested-native",
                        nested.to_str().ok_or("path")?,
                    ])?;
                } else {
                    fixture.run(&[
                        "clone",
                        fixture.root.to_str().ok_or("root")?,
                        nested.to_str().ok_or("path")?,
                    ])?;
                }
                fs::create_dir(nested.join("build"))?;
                fs::write(nested.join("build/source"), "committed")?;
                run_in_worktree(&fixture, &nested, &["add", "-f", "build/source"])?;
                run_in_worktree(
                    &fixture,
                    &nested,
                    &[
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.test",
                        "commit",
                        "-m",
                        "nested source",
                    ],
                )?;
                fs::write(nested.join("build/source"), "uncommitted edit")?;
                let report = collect(
                    &context,
                    &list(&context, &cancel())?,
                    &WorktreeGcPolicy {
                        upstream: None,
                        terminal: &|_| false,
                        no_live_lanes: &|_| true,
                        regenerable: &["build".into(), "target".into()],
                        keep: &[],
                        written_before_ns: i128::MAX,
                        dry_run: false,
                        foreign_merged_clean: false,
                        foreign_idle_before_ns: None,
                    },
                    &cancel(),
                )?;
                assert_eq!(
                    fs::read_to_string(nested.join("build/source"))?,
                    "uncommitted edit"
                );
                assert!(report.idle_output.iter().all(|entry| !entry.removed));
            }
        }
        Ok(())
    }

    #[test]
    fn remove_declared_ignored_output_but_names_undeclared_env() -> Result<(), Box<dyn Error>> {
        for secret in [false, true] {
            let fixture = Fixture::new()?;
            fs::write(fixture.root.join(".gitignore"), "target/\n.env\n")?;
            fixture.run(&["add", ".gitignore"])?;
            fixture.run(&["commit", "-m", "output rules"])?;
            let test_context = TestContext::new(&fixture)?;
            let context = test_context.context();
            let created = create(
                &context,
                WorktreeCreate {
                    branch: "remove-output".into(),
                    base: None,
                },
                &cancel(),
            )?;
            fs::create_dir(created.path.join("target"))?;
            fs::write(created.path.join("target/artifact"), "output")?;
            if secret {
                fs::write(created.path.join(".env"), "private")?;
            }
            let result = remove_with_output(
                &context,
                WorktreeRemove::Name {
                    name: "remove-output".into(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
                &cancel(),
                &provenant_config::ProjectGcSettings {
                    regenerable: vec!["target".into()],
                    ..Default::default()
                },
            );
            if secret {
                let error = result.err().ok_or("removed ignored secret")?;
                assert!(error.to_string().contains(".env"), "{error}");
                assert_eq!(fs::read_to_string(created.path.join(".env"))?, "private");
                assert!(
                    created.path.join("target/artifact").exists(),
                    "preview must refuse before pruning"
                );
            } else {
                assert_eq!(result?.removed.len(), 1);
                assert!(!created.path.exists());
            }
        }
        Ok(())
    }

    #[test]
    fn gc_foreign_merged_clean_is_opt_in_idle_and_requires_complete_census()
    -> Result<(), Box<dyn Error>> {
        for scenario in [
            "default",
            "recent",
            "unknown",
            "unmerged",
            "live-lane",
            "dirty",
            "preview",
            "apply",
        ] {
            let fixture = Fixture::new()?;
            let test_context = TestContext::new(&fixture)?;
            let mut context = test_context.context();
            let path = fixture._temp.path().join("native");
            fixture.run(&[
                "worktree",
                "add",
                "-b",
                "native",
                path.to_str().ok_or("path")?,
            ])?;
            if scenario == "unmerged" {
                run_in_worktree(
                    &fixture,
                    &path,
                    &["commit", "--allow-empty", "-m", "unmerged"],
                )?;
            }
            if scenario == "dirty" {
                fs::write(path.join("notes"), "keep")?;
            }
            let unknown = UnknownWorkers;
            if scenario == "unknown" {
                context.workers = &unknown;
            }
            let lanes = list(&context, &cancel())?;
            let report = collect(
                &context,
                &lanes,
                &WorktreeGcPolicy {
                    foreign_idle_before_ns: None,
                    foreign_merged_clean: scenario != "default",
                    upstream: Some(fixture.head()?),
                    terminal: &|_| false,
                    no_live_lanes: &|_| scenario != "live-lane",
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: if scenario == "recent" { 0 } else { i128::MAX },
                    dry_run: scenario == "preview",
                },
                &cancel(),
            )?;
            assert_eq!(
                report.worktrees.removed.len(),
                usize::from(matches!(scenario, "preview" | "apply")),
                "{scenario}: {report:?}"
            );
            let expected = match scenario {
                "recent" | "unknown" | "live-lane" => Some(RetainReason::UnknownWriter),
                "unmerged" => Some(RetainReason::NotMerged),
                "dirty" => Some(RetainReason::Dirty),
                _ => None,
            };
            assert_eq!(
                report.worktrees.retained,
                expected
                    .into_iter()
                    .map(|reason| RetainedBranch {
                        branch: "native".into(),
                        reason
                    })
                    .collect::<Vec<_>>(),
                "{scenario}"
            );
            assert_eq!(path.exists(), scenario != "apply", "{scenario}");
            assert!(
                matches!(
                    context.project_state.open_verified_directory(&state_dir_name(&path)),
                    Err(error) if error.is_not_found()
                ),
                "temporary state remains after {scenario}"
            );
            assert!(
                branch_exists(&fixture, "native"),
                "foreign collection preserves refs"
            );
        }
        Ok(())
    }

    #[test]
    fn gc_output_pruning_holds_writer_but_not_repository_lock() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore output"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let mut lane = gc_merged_worktree(&fixture, &context, "gc-output-lock")?;
        let target = lane.path.join("target");
        fs::create_dir(&target)?;
        fs::write(target.join("artifact"), "output")?;
        lane.dirty = WorktreeCondition::Ignored;
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&lane.path))?;
        let observations = std::cell::Cell::new(None);
        let calls = std::cell::Cell::new(0);
        let terminal = |_: &Path| {
            let count = calls.get() + 1;
            calls.set(count);
            if count == 1 {
                let repository_free = context
                    .project_state
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .is_ok();
                let writer_held = matches!(
                    state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                    Err(FsError::LockTimeout { .. })
                );
                observations.set(Some((repository_free, writer_held)));
            }
            true
        };
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                foreign_idle_before_ns: None,
                foreign_merged_clean: false,
                upstream: None,
                terminal: &terminal,
                no_live_lanes: &|_| true,
                regenerable: &["target".to_owned()],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
            },
            &cancel(),
        )?;
        assert_eq!(observations.get(), Some((true, true)));
        assert!(!target.exists());
        assert_eq!(report.output.len(), 1);
        assert!(report.output[0].removed);
        Ok(())
    }

    #[test]
    fn gc_foreign_rechecks_changes_before_removal() -> Result<(), Box<dyn Error>> {
        for change in ["ignored", "head"] {
            let fixture = Fixture::new()?;
            fs::write(fixture.root.join(".gitignore"), ".env\n")?;
            fixture.run(&["add", ".gitignore"])?;
            fixture.run(&["commit", "-m", "ignore local environment"])?;
            let test_context = TestContext::new(&fixture)?;
            let context = test_context.context();
            let path = fixture._temp.path().join("native-recheck");
            fixture.run(&[
                "worktree",
                "add",
                "-b",
                "native-recheck",
                path.to_str().ok_or("path")?,
            ])?;
            let lanes = list(&context, &cancel())?;
            let changed = std::cell::Cell::new(false);
            let calls = std::cell::Cell::new(0);
            let no_live = |_: &Path| {
                let count = calls.get() + 1;
                calls.set(count);
                let repository_held = context
                    .project_state
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .is_err();
                if !changed.get()
                    && ((change == "ignored" && repository_held)
                        || (change == "head" && count == 3))
                {
                    changed.set(true);
                    if change == "ignored" {
                        fs::write(path.join(".env"), "new secret").unwrap();
                    } else {
                        run_in_worktree(
                            &fixture,
                            &path,
                            &["commit", "--allow-empty", "-m", "concurrent work"],
                        )
                        .unwrap();
                    }
                }
                true
            };
            let report = collect(
                &context,
                &lanes,
                &WorktreeGcPolicy {
                    upstream: Some(fixture.head()?),
                    terminal: &|_| false,
                    no_live_lanes: &no_live,
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run: false,
                    foreign_merged_clean: true,
                    foreign_idle_before_ns: None,
                },
                &cancel(),
            )?;
            assert!(changed.get());
            assert!(path.is_dir());
            assert!(report.worktrees.removed.is_empty());
            assert_eq!(
                report.worktrees.retained,
                vec![RetainedBranch {
                    branch: "native-recheck".into(),
                    reason: if change == "ignored" {
                        RetainReason::Dirty
                    } else {
                        RetainReason::Changed
                    },
                }],
                "{report:?}"
            );
            if change == "ignored" {
                assert_eq!(fs::read_to_string(path.join(".env"))?, "new secret");
            }
        }
        Ok(())
    }

    #[test]
    fn land_worker_census_excludes_only_ancestors_through_the_session_leader()
    -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
        struct Processes(Census);
        impl ProcessObserver for Processes {
            fn boot_identity(&self) -> Result<String, CensusError> {
                Ok("boot".into())
            }
            fn observe(&self, pid: i64) -> ProcessObservation {
                match self.0.process(pid) {
                    ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
                    _ => ProcessObservation::Absent,
                }
            }
            fn census(&self, _: usize) -> Result<Census, CensusError> {
                Ok(self.0.clone())
            }
        }
        struct Cwds(BTreeMap<i64, PathBuf>);
        impl CwdObserver for Cwds {
            fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
                Ok(self
                    .0
                    .get(&pid)
                    .cloned()
                    .map(CwdObservation::Path)
                    .unwrap_or(CwdObservation::Gone))
            }
        }
        let fixture = Fixture::new()?;
        let root = fixture.root.clone();
        let outside = fixture._temp.path().to_path_buf();
        let fact = |pid, parent_pid, session_id| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: session_id,
                session_id,
            },
            parent_pid,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let current = fact(100, 90, 10);
        let facts = vec![
            fact(1, 0, 1),
            fact(10, 1, 10),
            fact(80, 10, 10),
            fact(90, 80, 10),
            current.clone(),
            fact(95, 80, 10),
            fact(110, 100, 10),
            fact(200, 1, 200),
        ];
        for complete in [true, false] {
            let processes = Processes(Census::from_observations(
                facts.clone(),
                [],
                [],
                complete,
                false,
                32,
            )?);
            let mut cwds = Cwds(
                facts
                    .iter()
                    .map(|fact| {
                        (
                            fact.identity.pid,
                            if [10, 80, 90, 100].contains(&fact.identity.pid) {
                                root.clone()
                            } else {
                                outside.clone()
                            },
                        )
                    })
                    .collect(),
            );
            let observe = |cwds: &Cwds, ancestors| {
                observe_worker_roots(
                    std::slice::from_ref(&root),
                    &processes,
                    cwds,
                    &current,
                    ancestors,
                )[&root]
            };
            assert_eq!(observe(&cwds, false), WorkerState::Live);
            assert_eq!(
                observe(&cwds, true),
                if complete {
                    WorkerState::Free
                } else {
                    WorkerState::Unknown
                }
            );
            // Above-session ancestors, siblings, descendants and independent
            // sessions remain live, even when the census is incomplete.
            for pid in [1, 95, 110, 200] {
                cwds.0.insert(pid, root.clone());
                assert_eq!(observe(&cwds, true), WorkerState::Live, "pid {pid}");
                cwds.0.insert(pid, outside.clone());
            }
        }
        for ambiguity in [
            "equal_stamp",
            "younger_parent",
            "missing_hop",
            "cycle",
            "reused_current",
        ] {
            let mut ambiguous = facts.clone();
            match ambiguity {
                "equal_stamp" => {
                    ambiguous
                        .iter_mut()
                        .find(|fact| fact.identity.pid == 90)
                        .ok_or("parent")?
                        .identity
                        .native_start = 100
                }
                "younger_parent" => {
                    ambiguous
                        .iter_mut()
                        .find(|fact| fact.identity.pid == 90)
                        .ok_or("parent")?
                        .identity
                        .native_start = 101
                }
                "missing_hop" => ambiguous.retain(|fact| fact.identity.pid != 90),
                "cycle" => {
                    ambiguous
                        .iter_mut()
                        .find(|fact| fact.identity.pid == 80)
                        .ok_or("parent")?
                        .parent_pid = 90
                }
                "reused_current" => {
                    ambiguous
                        .iter_mut()
                        .find(|fact| fact.identity.pid == 100)
                        .ok_or("current")?
                        .identity
                        .native_start += 1
                }
                _ => unreachable!(),
            }
            let processes = Processes(Census::from_observations(
                ambiguous,
                [],
                [],
                true,
                false,
                32,
            )?);
            let cwds = Cwds(
                facts
                    .iter()
                    .map(|fact| (fact.identity.pid, outside.clone()))
                    .collect(),
            );
            assert_eq!(
                observe_worker_roots(
                    std::slice::from_ref(&root),
                    &processes,
                    &cwds,
                    &current,
                    true
                )[&root],
                WorkerState::Unknown,
                "{ambiguity}"
            );
        }
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // A real child provides the cwd observation fixture.
    fn gc_foreign_retains_a_process_with_cwd_inside_the_worktree() -> Result<(), Box<dyn Error>> {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct CwdWorker(ProcessFact);
        impl WorkerObserver for CwdWorker {
            fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
                let mut states = roots
                    .iter()
                    .cloned()
                    .map(|root| (root, WorkerState::Free))
                    .collect();
                match observe_same_process(&SystemProcessObserver, &SystemCwdObserver, &self.0) {
                    NativeCwd::Path(path) => record_live_path(&mut states, &path),
                    _ => {
                        for state in states.values_mut() {
                            *state = WorkerState::Unknown;
                        }
                    }
                }
                states
            }
        }
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let path = fixture._temp.path().join("native-cwd");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native-cwd",
            path.to_str().ok_or("path")?,
        ])?;
        fs::create_dir(path.join("src"))?;
        // Obtain a clean listing before the native process starts.
        let lanes = list(&context, &cancel())?;
        let child = Child(
            std::process::Command::new("/bin/sh")
                .args(["-c", "read line"])
                .current_dir(path.join("src"))
                .stdin(std::process::Stdio::piped())
                .spawn()?,
        );
        let ProcessObservation::Present(fact) =
            SystemProcessObserver.observe(i64::from(child.0.id()))
        else {
            return Err("child process unobservable".into());
        };
        let worker = CwdWorker(fact);
        assert_eq!(
            worker.observe_roots(std::slice::from_ref(&path)).get(&path),
            Some(&WorkerState::Live)
        );
        context.workers = &worker;
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
                foreign_merged_clean: true,
                foreign_idle_before_ns: None,
            },
            &cancel(),
        )?;
        assert_eq!(
            report.worktrees.retained,
            vec![RetainedBranch {
                branch: "native-cwd".into(),
                reason: RetainReason::UnknownWriter
            }]
        );
        assert!(report.worktrees.removed.is_empty());
        assert!(path.is_dir());
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Bounded real lock contention.
    fn gc_foreign_without_state_holds_writer_through_final_recheck() -> Result<(), Box<dyn Error>> {
        for scenario in [
            "remove",
            "final-refusal",
            "contended-refusal",
            "cancelled-refusal",
        ] {
            let refuse = scenario != "remove";
            let fixture = Fixture::new()?;
            let test_context = TestContext::new(&fixture)?;
            let mut context = test_context.context();
            let clock = provenant_core::TestClock::new(
                context.clock.wall_now()?,
                provenant_core::MonoTime::from_micros(0),
            );
            context.clock = &clock;
            context.lock_budget = WaitBudget::cli(clock.mono_now(), 1)?;
            let path = fixture._temp.path().join("native-no-state");
            fixture.run(&[
                "worktree",
                "add",
                "-b",
                "native-no-state",
                path.to_str().ok_or("path")?,
            ])?;
            let lanes = list(&context, &cancel())?;
            let state_name = state_dir_name(&path);
            assert!(
                context
                    .project_state
                    .open_verified_directory(&state_name)
                    .is_err()
            );
            let final_writer_held = std::cell::Cell::new(false);
            let holder = std::cell::RefCell::new(None);
            let collection_cancel = cancel();
            let no_live = |_: &Path| {
                if scenario == "contended-refusal"
                    && let Ok(state) = context.project_state.open_verified_directory(&state_name)
                    && matches!(
                        state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                        Err(FsError::LockTimeout { .. })
                    )
                {
                    clock
                        .wait_until(provenant_core::MonoTime::from_micros(10_000_000))
                        .unwrap();
                    let repository = context
                        .project_state
                        .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                        .unwrap();
                    holder.replace(Some(std::thread::spawn(move || {
                        std::thread::park_timeout(std::time::Duration::from_millis(50));
                        drop(repository);
                    })));
                    return false;
                }
                if matches!(
                    context
                        .project_state
                        .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive),
                    Err(FsError::LockTimeout { .. })
                ) {
                    let Ok(state) = context.project_state.open_verified_directory(&state_name)
                    else {
                        // A concurrent test's fork can briefly inherit the
                        // repository lock before this call establishes state.
                        return true;
                    };
                    final_writer_held.set(matches!(
                        state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                        Err(FsError::LockTimeout { .. })
                    ));
                }
                if scenario == "cancelled-refusal" && final_writer_held.get() {
                    collection_cancel.cancel();
                }
                !refuse || !final_writer_held.get()
            };
            let report = collect(
                &context,
                &lanes,
                &WorktreeGcPolicy {
                    upstream: Some(fixture.head()?),
                    terminal: &|_| false,
                    no_live_lanes: &no_live,
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run: false,
                    foreign_merged_clean: true,
                    foreign_idle_before_ns: None,
                },
                &collection_cancel,
            )?;
            if refuse {
                assert_eq!(
                    report.worktrees.retained,
                    vec![RetainedBranch {
                        branch: "native-no-state".into(),
                        reason: RetainReason::UnknownWriter,
                    }]
                );
                assert!(path.is_dir());
            } else {
                assert_eq!(
                    report
                        .worktrees
                        .removed
                        .iter()
                        .map(|tree| &tree.path)
                        .collect::<Vec<_>>(),
                    vec![&path],
                    "{report:?}"
                );
                assert!(!path.exists());
            }
            if let Some(holder) = holder.into_inner() {
                holder.join().unwrap();
            } else {
                assert!(final_writer_held.get());
            }
            assert!(
                matches!(context.project_state.open_verified_directory(&state_name), Err(error) if error.is_not_found()),
                "temporary state remains after {scenario}"
            );
        }
        Ok(())
    }

    #[test]
    fn gc_foreign_prunes_under_writer_then_locks_repository() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore output"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let mut lane = gc_merged_worktree(&fixture, &context, "foreign-output-lock")?;
        fs::create_dir(lane.path.join("target"))?;
        fs::write(lane.path.join("target/artifact"), "output")?;
        lane.writer = WriterState::Unknown;
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&lane.path))?;
        let observations = std::cell::RefCell::new(Vec::new());
        let no_live_lanes = |_: &Path| {
            if state
                .try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
                .is_err()
            {
                observations.borrow_mut().push(
                    context
                        .project_state
                        .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                        .is_ok(),
                );
            }
            true
        };
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &no_live_lanes,
                regenerable: &["target".into()],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
                foreign_merged_clean: true,
                foreign_idle_before_ns: None,
            },
            &cancel(),
        )?;
        assert_eq!(report.worktrees.removed.len(), 1, "{report:?}");
        let observed = observations.borrow();
        assert_eq!(observed.first(), Some(&true));
        assert_eq!(observed.last(), Some(&false));
        Ok(())
    }

    #[test]
    fn gc_failed_prune_retains_once_and_continues() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let first = gc_merged_worktree(&fixture, &context, "gc-prune-failed")?;
        let second = gc_merged_worktree(&fixture, &context, "gc-prune-next")?;
        let calls = std::cell::Cell::new(0);
        let terminal = |path: &Path| {
            if path == first.path {
                calls.set(calls.get() + 1);
                false
            } else {
                true
            }
        };
        let report = collect(
            &context,
            &[first.clone(), second.clone()],
            &WorktreeGcPolicy {
                foreign_idle_before_ns: None,
                foreign_merged_clean: false,
                upstream: Some(fixture.head()?),
                terminal: &terminal,
                no_live_lanes: &|_| true,
                regenerable: &["target".to_owned()],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
            },
            &cancel(),
        )?;
        assert!(
            calls.get() >= 2,
            "prune and removal both recheck terminality"
        );
        assert!(first.path.is_dir());
        assert!(branch_exists(&fixture, "gc-prune-failed"));
        assert_eq!(report.worktrees.retained.len(), 1);
        assert_eq!(report.worktrees.retained[0].branch, "gc-prune-failed");
        assert_eq!(report.worktrees.removed, vec![second.clone()]);
        assert_eq!(report.worktrees.pruned_refs, vec!["gc-prune-next"]);
        assert!(!second.path.exists());
        Ok(())
    }

    #[test]
    fn gc_skips_unmanaged_worktrees_and_their_output() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore output"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let unmanaged = fixture._temp.path().join("unmanaged");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "gc-unmanaged",
            unmanaged.to_str().ok_or("non-UTF8 worktree path")?,
        ])?;
        run_in_worktree(
            &fixture,
            &unmanaged,
            &["commit", "--allow-empty", "-m", "gc-unmanaged"],
        )?;
        fixture.run(&["merge", "--no-ff", "gc-unmanaged", "-m", "merge GC fixture"])?;
        let target = unmanaged.join("target");
        fs::create_dir(&target)?;
        fs::write(target.join("artifact"), "keep")?;
        let canonical = canonicalize(&unmanaged)?;
        let mut lane = list(&context, &cancel())?
            .into_iter()
            .find(|lane| lane.path == canonical)
            .ok_or("missing unmanaged fixture worktree")?;
        lane.dirty = WorktreeCondition::Ignored;
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                foreign_idle_before_ns: None,
                foreign_merged_clean: false,
                upstream: Some(fixture.head()?),
                terminal: &|_| true,
                no_live_lanes: &|_| true,
                regenerable: &["target".to_owned()],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
            },
            &cancel(),
        )?;
        assert!(target.join("artifact").is_file());
        assert!(unmanaged.is_dir());
        assert!(branch_exists(&fixture, "gc-unmanaged"));
        assert!(report.worktrees.removed.is_empty());
        assert!(report.worktrees.retained.is_empty());
        assert!(report.output.is_empty());
        Ok(())
    }

    #[test]
    fn gc_dry_run_retains_merged_worktree_with_unknown_writer() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let lane = gc_merged_worktree(&fixture, &context, "gc-dry-unknown")?;
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                foreign_idle_before_ns: None,
                foreign_merged_clean: false,
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: true,
            },
            &cancel(),
        )?;
        assert_eq!(report.worktrees.retained.len(), 1);
        assert_eq!(
            report.worktrees.retained[0].reason,
            RetainReason::UnknownWriter
        );
        assert!(report.worktrees.removed.is_empty());
        Ok(())
    }

    #[test]
    fn gc_dry_run_retains_preview_dirty_worktree() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "cache/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore cache"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let mut lane = gc_merged_worktree(&fixture, &context, "gc-dry-dirty")?;
        // Ignored content outside the regenerable output keeps the worktree
        // dirty once the preview has pruned.
        fs::create_dir(lane.path.join("cache"))?;
        fs::write(lane.path.join("cache/artifact"), "keep")?;
        lane.dirty = WorktreeCondition::Ignored;
        let report = collect(
            &context,
            &[lane.clone()],
            &WorktreeGcPolicy {
                foreign_idle_before_ns: None,
                foreign_merged_clean: false,
                upstream: Some(fixture.head()?),
                terminal: &|_| true,
                no_live_lanes: &|_| true,
                regenerable: &["target".to_owned()],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: true,
            },
            &cancel(),
        )?;
        assert_eq!(report.worktrees.retained.len(), 1);
        assert_eq!(report.worktrees.retained[0].reason, RetainReason::Dirty);
        assert!(report.worktrees.removed.is_empty());
        assert!(lane.path.join("cache/artifact").is_file());
        Ok(())
    }

    /// Fail the first worktree-list read after a successful removal, precisely
    /// where branch settlement rechecks other checkouts. Later reads succeed.
    struct FailsFirstGcSettlement<'a> {
        executor: &'a provenant_os::exec::SystemExecutor,
        removed: std::sync::atomic::AtomicBool,
        failed: std::sync::atomic::AtomicBool,
    }

    impl Executor for FailsFirstGcSettlement<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            use std::sync::atomic::Ordering;
            let worktree = options.argv.iter().any(|arg| arg == "worktree");
            let remove = worktree && options.argv.iter().any(|arg| arg == "remove");
            let list = worktree && options.argv.iter().any(|arg| arg == "list");
            let mut outcome = self.executor.execute(options)?;
            if remove && outcome.termination == Termination::Exit(0) {
                self.removed.store(true, Ordering::SeqCst);
            } else if list
                && self.removed.load(Ordering::SeqCst)
                && !self.failed.swap(true, Ordering::SeqCst)
            {
                outcome.termination = Termination::Exit(1);
            }
            Ok(outcome)
        }
    }

    #[test]
    fn gc_settlement_error_retains_ref_and_continues() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let first = gc_merged_worktree(&fixture, &context, "gc-settle-failed")?;
        let second = gc_merged_worktree(&fixture, &context, "gc-settle-next")?;
        let faulty = FailsFirstGcSettlement {
            executor: &fixture.executor,
            removed: std::sync::atomic::AtomicBool::new(false),
            failed: std::sync::atomic::AtomicBool::new(false),
        };
        context.executor = &faulty;
        let report = collect(
            &context,
            &[first.clone(), second.clone()],
            &WorktreeGcPolicy {
                foreign_idle_before_ns: None,
                foreign_merged_clean: false,
                upstream: Some(fixture.head()?),
                terminal: &|_| true,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
            },
            &cancel(),
        )?;
        assert_eq!(
            report.worktrees.removed,
            vec![first.clone(), second.clone()]
        );
        assert_eq!(report.worktrees.retained.len(), 1);
        assert_eq!(report.worktrees.retained[0].branch, "gc-settle-failed");
        assert_eq!(report.worktrees.pruned_refs, vec!["gc-settle-next"]);
        assert!(branch_exists(&fixture, "gc-settle-failed"));
        assert!(!first.path.exists());
        assert!(!second.path.exists());
        Ok(())
    }

    #[test]
    fn timed_out_add_rolls_back_its_checkout_and_new_branch() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let faulty = TimesOutWorktreeAdd(&fixture.executor);
        let mut context = test_context.context();
        context.executor = &faulty;
        let request = || WorktreeCreate {
            branch: "feature/slow".to_owned(),
            base: Some("HEAD".to_owned()),
        };
        let failed = create(&context, request(), &cancel());
        let Err(WorktreeError::CreateFailed { cause, rolled_back }) = failed else {
            return Err(format!("expected CreateFailed, got {failed:?}").into());
        };
        assert!(rolled_back);
        assert!(matches!(
            *cause,
            WorktreeError::Utility(UtilityError::TimedOut)
        ));
        assert!(
            !fixture
                .root
                .join(WORKTREE_ROOT)
                .join("feature-slow")
                .exists()
        );
        assert!(!branch_exists(&fixture, "feature/slow"));
        let admin = fixture.repo.common_git_dir().join("worktrees");
        assert!(!admin.exists() || fs::read_dir(admin)?.next().is_none());
        // The retry that the leftover state used to block now succeeds.
        let retried = create(&test_context.context(), request(), &cancel())?;
        assert!(retried.path.is_dir());
        Ok(())
    }

    #[test]
    fn timed_out_add_keeps_a_branch_it_did_not_create() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "feature/existing"])?;
        let test_context = TestContext::new(&fixture)?;
        let faulty = TimesOutWorktreeAdd(&fixture.executor);
        let mut context = test_context.context();
        context.executor = &faulty;
        let failed = create(
            &context,
            WorktreeCreate {
                branch: "feature/existing".to_owned(),
                base: None,
            },
            &cancel(),
        );
        assert!(matches!(
            failed,
            Err(WorktreeError::CreateFailed {
                rolled_back: true,
                ..
            })
        ));
        assert!(branch_exists(&fixture, "feature/existing"));
        assert!(
            !fixture
                .root
                .join(WORKTREE_ROOT)
                .join("feature-existing")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn create_fetches_a_remote_base_with_a_main_only_refspec() -> Result<(), Box<dyn Error>> {
        let source = Fixture::new()?;
        source.run(&["branch", "-M", "main"])?;
        source.run(&["checkout", "-b", "chore/cleanup-code-sweep"])?;
        source.run(&["commit", "--allow-empty", "-m", "remote-only commit"])?;
        let base = source.head()?;
        source.run(&["checkout", "main"])?;
        let mut fixture = Fixture::new()?;
        let clone = fixture._temp.path().join("clone");
        fixture.run(&[
            "clone",
            "--single-branch",
            "--branch",
            "main",
            source.root.to_str().ok_or("non-UTF8 source")?,
            clone.to_str().ok_or("non-UTF8 clone")?,
        ])?;
        fixture.root = clone;
        fixture.repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &cancel(),
        )?;
        let refspec = fixture.run(&["config", "--get-all", "remote.origin.fetch"])?;
        assert_eq!(
            refspec.stdout.bytes,
            b"+refs/heads/main:refs/remotes/origin/main\n"
        );
        assert!(
            fixture
                .run(&["for-each-ref", "refs/remotes/origin/chore/"])?
                .stdout
                .bytes
                .is_empty()
        );
        let test_context = TestContext::new(&fixture)?;

        let observes_fetch = ObservesFetch {
            executor: &fixture.executor,
            seen: std::sync::atomic::AtomicBool::new(false),
        };
        let mut context = test_context.context();
        context.executor = &observes_fetch;
        let created = create(
            &context,
            WorktreeCreate {
                branch: "chore/cleanup-code-sweep-2".to_owned(),
                base: Some("origin/chore/cleanup-code-sweep".to_owned()),
            },
            &cancel(),
        )?;

        assert_eq!(created.head, base);
        assert!(
            observes_fetch
                .seen
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        let tracking =
            fixture.run(&["rev-parse", "refs/remotes/origin/chore/cleanup-code-sweep"])?;
        assert_eq!(
            String::from_utf8(tracking.stdout.bytes)?.trim(),
            base.to_string()
        );
        assert_eq!(
            fixture
                .run(&["config", "--get-all", "remote.origin.fetch"])?
                .stdout
                .bytes,
            refspec.stdout.bytes
        );
        Ok(())
    }

    #[test]
    fn unresolved_base_error_names_the_base() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        fixture.run(&[
            "remote",
            "add",
            "origin",
            fixture.root.to_str().ok_or("non-UTF8 root")?,
        ])?;
        fixture.repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &cancel(),
        )?;
        let test_context = TestContext::new(&fixture)?;
        let error = create(
            &test_context.context(),
            WorktreeCreate {
                branch: "feature/missing-base".to_owned(),
                base: Some("origin/missing".to_owned()),
            },
            &cancel(),
        )
        .expect_err("missing remote branch must fail");
        assert!(matches!(
            error,
            WorktreeError::BaseNotFound { ref base } if base == "origin/missing"
        ));

        let error = create(
            &test_context.context(),
            WorktreeCreate {
                branch: "feature/missing-base".to_owned(),
                base: Some("unknown/branch".to_owned()),
            },
            &cancel(),
        )
        .expect_err("missing base must fail");
        assert_eq!(
            error.to_string(),
            "base unknown/branch did not resolve to a commit"
        );
        Ok(())
    }

    #[test]
    fn create_skips_prunable_worktree_registrations() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let stale = create(
            &context,
            WorktreeCreate {
                branch: "scratch/stale".to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        fs::remove_dir_all(&stale.path)?;
        let listing = fixture.run(&["worktree", "list", "--porcelain"])?;
        assert!(String::from_utf8_lossy(&listing.stdout.bytes).contains("prunable"));

        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/after-prunable".to_owned(),
                base: None,
            },
            &cancel(),
        )?;

        assert!(created.path.is_dir());
        assert!(!stale.path.exists());
        assert!(
            read_worktrees(&context, &cancel())?
                .iter()
                .any(|entry| entry.path == stale.path && entry.prunable)
        );
        Ok(())
    }

    #[test]
    fn create_with_prunable_registration_for_same_branch() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let stale = create(
            &context,
            WorktreeCreate {
                branch: "scratch/stale".to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        fs::remove_dir_all(&stale.path)?;
        let error = create(
            &context,
            WorktreeCreate {
                branch: "scratch/stale".to_owned(),
                base: None,
            },
            &cancel(),
        )
        .expect_err("stale registration must fail with prune hint");
        assert!(matches!(error, WorktreeError::StaleRegistration));
        assert!(error.to_string().contains("git worktree prune"), "{error}");

        fixture.run(&["commit", "--allow-empty", "-m", "advance HEAD"])?;
        let head = fixture.head()?;
        fixture.run(&["update-ref", "-d", "refs/heads/scratch/stale"])?;
        fixture.run(&["branch", "scratch/stale", &head.to_string()])?;
        let error_recreated = create(
            &context,
            WorktreeCreate {
                branch: "scratch/stale".to_owned(),
                base: None,
            },
            &cancel(),
        )
        .expect_err("recreated branch with stale registration must fail with prune hint");
        assert!(matches!(error_recreated, WorktreeError::StaleRegistration));
        assert!(
            error_recreated.to_string().contains("git worktree prune"),
            "{error_recreated}"
        );
        Ok(())
    }

    #[test]
    fn remove_reports_stale_registration_by_branch_and_directory() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let stale = create(
            &context,
            WorktreeCreate {
                branch: "scratch/stale".to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        fs::remove_dir_all(&stale.path)?;
        for name in ["scratch/stale", "scratch-stale"] {
            let error = remove(
                &context,
                WorktreeRemove::Name {
                    name: name.to_owned(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
                &cancel(),
            )
            .expect_err("stale registration must be reported");
            assert!(
                error.to_string().contains("registration is stale"),
                "{error}"
            );
            assert!(error.to_string().contains("git worktree prune"), "{error}");
        }
        assert!(branch_exists(&fixture, "scratch/stale"));
        assert!(
            read_worktrees(&context, &cancel())?
                .iter()
                .any(|entry| entry.path == stale.path && entry.prunable)
        );
        Ok(())
    }

    #[test]
    fn prune_treats_a_prunable_branch_as_not_checked_out() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let stale = create(
            &context,
            WorktreeCreate {
                branch: "scratch/stale".to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        run_in_worktree(
            &fixture,
            &stale.path,
            &["commit", "--allow-empty", "-m", "stale work"],
        )?;
        fixture.run(&[
            "merge",
            "--no-ff",
            "scratch/stale",
            "-m",
            "merge stale work",
        ])?;
        fs::remove_dir_all(&stale.path)?;
        let entries = read_worktrees(&context, &cancel())?;
        let branch = GitRefName::branch("scratch/stale")?;
        assert!(matching_worktree(&entries, &branch, &fixture.root)?.is_none());
        assert!(!checked_out_elsewhere(&entries, &branch, None)?);
        assert!(!checked_out_elsewhere(
            &entries,
            &branch,
            Some(&fixture.root)
        )?);
        let integration = primary_integration(&context, &cancel())?.0;
        let result = remove(
            &context,
            WorktreeRemove::Merged {
                integration: integration.as_str().to_owned(),
                scope: PruneScope::Branches(vec!["scratch/stale".to_owned()]),
            },
            &cancel(),
        )?;
        assert_eq!(result.pruned_refs, vec!["scratch/stale"]);
        assert!(result.retained.is_empty());
        assert!(!branch_exists(&fixture, "scratch/stale"));
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
    fn clean_linked_worktree_reports_and_retains_in_progress_git_operations()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/sequencer".into(),
                base: None,
            },
            &cancellation,
        )?;
        let verified = verify_worktree(&fixture.repo, &created.path)?;
        check_writer_terminal(verified.repo())?;
        for (marker, directory) in [
            ("CHERRY_PICK_HEAD", false),
            ("REVERT_HEAD", false),
            ("REBASE_HEAD", false),
            ("rebase-merge", true),
            ("rebase-apply", true),
            ("MERGE_HEAD", false),
            ("sequencer", true),
        ] {
            let marker_path = verified.repo().private_git_dir().join(marker);
            if directory {
                fs::create_dir(&marker_path)?;
            } else {
                fs::write(&marker_path, format!("{}\n", created.head))?;
            }
            assert!(
                matches!(
                    check_writer_terminal(verified.repo()),
                    Err(WorktreeError::GitOperationInProgress)
                ),
                "{marker}"
            );
            check_writer_terminal(&fixture.repo)?;
            let listed = list(&context, &cancellation)?;
            let entry = listed
                .iter()
                .find(|entry| entry.path == created.path)
                .ok_or("worktree missing")?;
            assert_eq!(
                entry.dirty,
                WorktreeCondition::GitOperationInProgress,
                "{marker}"
            );
            assert!(
                matches!(
                    remove(
                        &context,
                        WorktreeRemove::Name {
                            name: "feature/sequencer".into(),
                            discard: false,
                            integration: None,
                            expected_head: None,
                        },
                        &cancellation,
                    ),
                    Err(WorktreeError::GitOperationInProgress)
                ),
                "{marker}"
            );
            assert!(created.path.is_dir());
            if directory {
                fs::remove_dir(&marker_path)?;
            } else {
                fs::remove_file(&marker_path)?;
            }
        }
        check_writer_terminal(verified.repo())?;
        let listed = list(&context, &cancellation)?;
        assert!(
            listed
                .iter()
                .any(|entry| entry.path == created.path && entry.dirty == WorktreeCondition::Clean)
        );
        remove(
            &context,
            WorktreeRemove::Name {
                name: "feature/sequencer".into(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
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
                WorktreeRemove::Name {
                    name: "main".to_owned(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
                &cancellation
            ),
            Err(WorktreeError::Primary)
        ));

        let unknown_context = WorktreeContext {
            workers: &UnknownWorkers,
            ..test_context.context()
        };
        // An unobservable process with a free lease and a clean tree is removed.
        remove(
            &unknown_context,
            WorktreeRemove::Name {
                name: "feature/lifecycle".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        )?;
        assert!(!created.path.exists());
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
        let condition = |cancellation: &CancelToken| -> Result<_, Box<dyn Error>> {
            Ok(list(&context, cancellation)?
                .into_iter()
                .find(|entry| entry.path == created.path)
                .map(|entry| entry.dirty))
        };
        assert_eq!(condition(&cancellation)?, Some(WorktreeCondition::Ignored));

        assert!(matches!(
            remove(
                &context,
                WorktreeRemove::Name {
                    name: "feature/ignored".to_owned(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
                &cancellation,
            ),
            Err(WorktreeError::IgnoredPaths(ref paths)) if paths.contains("ignored.txt")
        ));
        assert_eq!(
            fs::read_to_string(created.path.join("ignored.txt"))?,
            "keep me\n"
        );
        fs::write(created.path.join("notes.txt"), "draft\n")?;
        assert_eq!(condition(&cancellation)?, Some(WorktreeCondition::Dirty));
        Ok(())
    }

    #[test]
    fn writer_custody_requires_the_active_registration() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/custody".to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        let worktree = verify_worktree(&fixture.repo, &created.path)?;
        let project = test_context.binding.project_id();
        let state_path = fixture
            ._temp
            .path()
            .join("worktrees")
            .join(project.to_string());
        let project_state = Directory::open_anchor(&state_path)?;
        let branch = GitRefName::branch("feature/custody")?;
        let base = created.head;
        let (state, _binding) = writer_custody(&project_state, project, &worktree, &branch, base)?;
        assert!(state.metadata_at(OsStr::new(IDENTITY), false).is_ok());
        let other = GitRefName::branch("feature/other")?;
        assert!(matches!(
            writer_custody(&project_state, project, &worktree, &other, base),
            Err(WorktreeError::UnknownWriter)
        ));
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
                WorktreeRemove::Name {
                    name: "feature/busy".to_owned(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
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
                WorktreeRemove::Name {
                    name: "feature/busy".to_owned(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
                &cancellation
            ),
            Err(WorktreeError::UnknownWriter)
        ));
        state.unlink(OsStr::new(RESERVATION))?;
        remove(
            &context,
            WorktreeRemove::Name {
                name: "feature/busy".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        )?;
        Ok(())
    }

    #[test]
    fn named_removal_refuses_a_head_that_moved_after_the_callers_check()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let cancellation = cancel();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/raced".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        // The caller verified the worktree at its creation head, then a clean
        // commit landed before removal started.
        let verified = created.head;
        run_in_worktree(
            &fixture,
            &created.path,
            &[
                "-c",
                "user.name=raced",
                "-c",
                "user.email=raced@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "after the check",
            ],
        )?;
        let branch = GitRefName::branch("feature/raced")?;
        let moved = read_branch_tips(&context, &cancellation)?
            .get(&branch)
            .copied()
            .ok_or("branch tip")?;
        assert_ne!(moved, verified);
        let request = |expected_head| WorktreeRemove::Name {
            name: "feature/raced".to_owned(),
            discard: false,
            integration: None,
            expected_head,
        };
        assert!(matches!(
            remove(&context, request(Some(verified)), &cancellation),
            Err(WorktreeError::IdentityChanged)
        ));
        assert!(created.path.exists(), "the checkout must survive");
        assert_eq!(
            read_branch_tips(&context, &cancellation)?.get(&branch),
            Some(&moved)
        );
        // Verifying the head it is really at lets the removal proceed.
        let outcome = remove(&context, request(Some(moved)), &cancellation)?;
        assert_eq!(outcome.removed.len(), 1);
        assert!(!created.path.exists());
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
                WorktreeRemove::Name {
                    name: "feature/direct-gitlink".to_owned(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
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
