//! Repository-scoped linked-worktree lifecycle and merged-ref pruning.

mod context;
mod gc;
mod owner;
pub use context::with_context;
pub use gc::{WorktreeGcPolicy, WorktreeGcReport, collect};
pub use owner::{
    WorktreeOrigin, WorktreeOwner, load_owner, prune_owned, save_lane_owner_when_free, save_owner,
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
use provenant_core::{AttemptId, CancelToken, GitOid, RelativePath, WaitBudget, hex};
use provenant_os::exec::{ExecOutcome, Executor, Termination};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, PublishMode, canonicalize};
use provenant_os::pid::{CensusIgnoreEntry, CwdObservation, CwdObserver, SystemCwdObserver};
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
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_WORKTREES: usize = 10_000;
const MAX_BRANCHES: usize = 10_000;
const MAX_STATE_RECORD: u64 = 64 * 1024;
const REPOSITORY_LOCK: &str = "worktrees.lock";
const WRITER_LOCK: &str = "writer.lock";
const WORKTREE_ROOT: &str = ".worktrees";

impl From<provenant_config::ConfigError> for WorktreeError {
    fn from(error: provenant_config::ConfigError) -> Self {
        Self::Configuration(Box::new(error))
    }
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("worktree census configuration refused: {0}")]
    Configuration(#[from] Box<provenant_config::ConfigError>),
    #[error("worktree request is invalid")]
    InvalidRequest,
    #[error("worktree name {name} matches more than one checkout")]
    AmbiguousName { name: String },
    #[error("primary checkout has a detached HEAD; an integration branch is required")]
    DetachedPrimary,
    #[error("detached HEAD at {head} is not merged into the integration branch")]
    DetachedUnmerged { head: GitOid },
    #[error(
        "detached HEAD at {head}; git switch -c BRANCH or remove --worktree and pass --mode read-only"
    )]
    DetachedWorktree { head: GitOid },
    #[error("worktree writer is busy{}", .holder.as_ref().map_or_else(String::new, |attempt| format!(", held by attempt {attempt}")))]
    WriterBusy { holder: Option<AttemptId> },
    #[error("branch name collides with existing ref {existing}")]
    BranchCollision { existing: String },
    #[error("worktree or branch was not found")]
    NotFound,
    #[error("base {base} did not resolve to a commit")]
    BaseNotFound { base: String },
    #[error(
        "base {base} names a remote-tracking ref that its remote did not advertise; use the remote's exact spelling, such as origin/main"
    )]
    BaseUnverified { base: String },
    #[error("worktree registration is stale; git worktree prune clears it")]
    StaleRegistration,
    #[error("worktree is dirty")]
    Dirty,
    #[error("worktree HEAD {current} is not an ancestor of source commit {target}")]
    SourceDiverged { current: GitOid, target: GitOid },
    #[error("worktree could not fast-forward to source commit {target}")]
    SourceAdvanceFailed { target: GitOid },
    #[error("worktree has undeclared ignored paths: {0}")]
    IgnoredPaths(String),
    #[error("worktree has an in-progress Git operation")]
    GitOperationInProgress,
    #[error("worktree writer lock busy")]
    LockBusy,
    #[error("worktree writer is live")]
    LiveWriter,
    #[error("worktree writer is live: process {0} has its working directory inside the tree")]
    LiveProcess(ObservedProcess),
    #[error("incomplete worker census: {detail}")]
    IncompleteCensus { detail: String },
    #[error("worktree writer or registration is unknown")]
    UnknownWriter,
    #[error("Git marks the worktree {}", if *.locked { "locked" } else { "prunable" })]
    GitEntryHeld { locked: bool },
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

impl WorktreeError {
    /// Refusals that mean Provenant cannot prove who owns the tree. Callers
    /// that retain such a tree treat every one of them alike.
    pub fn is_custody_unknown(&self) -> bool {
        matches!(self, Self::UnknownWriter | Self::GitEntryHeld { .. })
    }

    /// The operator's next step for a refusal that has one.
    pub fn repair(&self) -> Option<&'static str> {
        Some(match self {
            Self::StaleRegistration => "run `git worktree prune`, then retry",
            Self::GitEntryHeld { locked: true } => "run `git worktree unlock <path>`, then retry",
            Self::GitEntryHeld { locked: false } => {
                "run `git worktree prune` to clear the missing checkout, or restore its directory, then retry"
            }
            Self::LiveProcess(_) => {
                "stop that process or change its directory out of the tree, then run `provenant worktree list` and retry"
            }
            Self::LockBusy | Self::LiveWriter | Self::WriterBusy { .. } => {
                "run `provenant status` and wait for the writer holding the lock to finish, then retry"
            }
            Self::Dirty => {
                "run `git -C PATH status`, then commit or preserve the changes before retrying"
            }
            Self::IgnoredPaths(_) => {
                "run `git -C PATH status --ignored`, then preserve the named files or declare directory names in `gc.projects.<id>.regenerable` in the global config (find the project ID with `provenant fabric identity`) before retrying"
            }
            Self::GitOperationInProgress => {
                "run `git -C PATH status` and complete or abort the Git operation before retrying"
            }
            Self::UnknownWriter | Self::IncompleteCensus { .. } => {
                "run `provenant worktree list` and retry when writer observation is available"
            }
            Self::Primary => {
                "run `provenant worktree list` and select a linked checkout instead of the primary"
            }
            Self::IdentityChanged => {
                "run `provenant worktree list` and retry against the current checkout identity"
            }
            Self::AmbiguousName { .. } => {
                "run `provenant worktree list` and select an unambiguous branch or directory name"
            }
            Self::DetachedPrimary => "run `provenant worktree remove NAME --integration BRANCH`",
            Self::DetachedUnmerged { .. } => {
                "run `git -C PATH branch NAME HEAD` to preserve the commit before repeating `provenant worktree remove NAME --discard`, or use --discard directly if the commit is no longer needed"
            }
            _ => return None,
        })
    }

    /// The message with its repair appended, for surfaces that print one line.
    pub fn with_repair(&self) -> String {
        match self.repair() {
            Some(repair) => format!("{self}; {repair}"),
            None => self.to_string(),
        }
    }
}

/// A process seen inside a worktree, named so an operator can tell their own
/// shell from a real writer. Diagnostic only; it never grants signal authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedProcess {
    pub pid: i64,
    pub command: Option<String>,
}

impl std::fmt::Display for ObservedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.command {
            Some(command) => write!(f, "{} ({command})", self.pid),
            None => write!(f, "{}", self.pid),
        }
    }
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
    Busy,
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
    LockBusy,
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
    Busy,
    Live,
    Unknown,
}

/// Per-root custody states and reasons for unknown states.
pub type WorkerObservations = (BTreeMap<PathBuf, WorkerState>, BTreeMap<PathBuf, String>);

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

    /// One process observed inside `root`, for a refusal that names it. The
    /// answer never decides custody; an observer without one returns `None`.
    fn live_process(&self, _root: &Path) -> Option<ObservedProcess> {
        None
    }

    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        self.live_process(root)
    }

    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        (self.observe_roots(roots), BTreeMap::new())
    }

    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        (self.observe_roots_for_land(roots), BTreeMap::new())
    }

    /// Composition supplies host configuration. Injected observers retain
    /// their own refusal semantics; the native observer uses these entries.
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        _entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        if for_land {
            self.observe_roots_for_land_with_details(roots)
        } else {
            self.observe_roots_with_details(roots)
        }
    }

    fn live_process_with_policy(
        &self,
        root: &Path,
        _entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> Option<ObservedProcess> {
        if for_land {
            self.live_process_for_land(root)
        } else {
            self.live_process(root)
        }
    }

    /// A diagnostic for an incomplete census. It never affects custody; the
    /// default is used by observers that cannot identify the observation.
    fn incomplete_census_detail(&self) -> String {
        "process census unavailable".into()
    }
}

/// Immutable host policy around either native observations or an injected seam.
pub struct CensusPolicyWorkers<'a> {
    inner: &'a dyn WorkerObserver,
    entries: Vec<CensusIgnoreEntry>,
}
impl<'a> CensusPolicyWorkers<'a> {
    pub fn from_roots(
        inner: &'a dyn WorkerObserver,
        roots: &provenant_core::Roots,
    ) -> Result<Self, WorktreeError> {
        Ok(Self {
            inner,
            entries: provenant_config::read(roots)?.census_ignore_unreadable()?,
        })
    }
}
impl WorkerObserver for CensusPolicyWorkers<'_> {
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, entries, for_land)
    }
    fn live_process_with_policy(
        &self,
        root: &Path,
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> Option<ObservedProcess> {
        self.inner.live_process_with_policy(root, entries, for_land)
    }
    fn requires_complete_census(&self) -> bool {
        self.inner.requires_complete_census()
    }
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        self.observe_roots_with_details(roots).0
    }
    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        self.observe_roots_for_land_with_details(roots).0
    }
    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, &self.entries, false)
    }
    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, &self.entries, true)
    }
    fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
        self.inner
            .live_process_with_policy(root, &self.entries, false)
    }
    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        self.inner
            .live_process_with_policy(root, &self.entries, true)
    }
    fn incomplete_census_detail(&self) -> String {
        self.inner.incomplete_census_detail()
    }
}

/// The refusal for a root observed live: it names the process when the
/// observer can, and is the plain live-writer refusal otherwise.
pub fn live_refusal(workers: &dyn WorkerObserver, root: &Path) -> WorktreeError {
    workers
        .live_process(root)
        .map_or(WorktreeError::LiveWriter, WorktreeError::LiveProcess)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWorkerObserver;

impl WorkerObserver for SystemWorkerObserver {
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        system_worker_roots_with_policy(roots, for_land, entries)
    }
    fn live_process_with_policy(
        &self,
        root: &Path,
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> Option<ObservedProcess> {
        system_live_process_with_policy(root, for_land, entries)
    }
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        system_worker_roots(roots, false).0
    }

    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        system_worker_roots(roots, true).0
    }

    fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
        system_live_process(root, false)
    }

    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        system_live_process(root, true)
    }

    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        system_worker_roots(roots, false)
    }

    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        system_worker_roots(roots, true)
    }
}

fn system_live_process(root: &Path, ancestors: bool) -> Option<ObservedProcess> {
    system_live_process_with_policy(root, ancestors, &[])
}

fn system_live_process_with_policy(
    root: &Path,
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
) -> Option<ObservedProcess> {
    let ProcessObservation::Present(current) = current_process() else {
        return None;
    };
    let roots = [root.to_path_buf()];
    observe_worker_holders(
        &roots,
        &SystemProcessObserver,
        &PolicyCwdObserver(entries),
        &current,
        ancestors,
    )
    .1
    .remove(root)
}

fn system_worker_roots(roots: &[PathBuf], ancestors: bool) -> WorkerObservations {
    system_worker_roots_with_policy(roots, ancestors, &[])
}

struct PolicyCwdObserver<'a>(&'a [CensusIgnoreEntry]);
impl CwdObserver for PolicyCwdObserver<'_> {
    fn unreadable_policy_euid(
        &self,
        pid: i64,
        _: &[CensusIgnoreEntry],
    ) -> Result<u32, provenant_os::pid::UnreadablePolicyFailure> {
        SystemCwdObserver.unreadable_policy_euid(pid, self.0)
    }
    fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
        SystemCwdObserver.cwd(pid)
    }
    fn ignored_unreadable_euid(&self, pid: i64, _: &[CensusIgnoreEntry]) -> Option<u32> {
        SystemCwdObserver.ignored_unreadable_euid(pid, self.0)
    }
    fn process_details(&self, pid: i64) -> Option<String> {
        SystemCwdObserver.process_details(pid)
    }
}

fn system_worker_roots_with_policy(
    roots: &[PathBuf],
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
) -> WorkerObservations {
    let ProcessObservation::Present(current) = current_process() else {
        return (
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
                .collect(),
            roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root,
                        process_detail(
                            &PolicyCwdObserver(entries),
                            i64::from(std::process::id()),
                            "current process unavailable",
                        ),
                    )
                })
                .collect(),
        );
    };
    observe_worker_roots(
        roots,
        &SystemProcessObserver,
        &PolicyCwdObserver(entries),
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
) -> WorkerObservations {
    let (states, _, details) =
        observe_worker_holders(roots, observer, cwd_observer, current, ancestors);
    (states, details)
}

/// The per-root state, and the first process seen inside each root.
fn observe_worker_holders(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
) -> (
    BTreeMap<PathBuf, WorkerState>,
    BTreeMap<PathBuf, ObservedProcess>,
    BTreeMap<PathBuf, String>,
) {
    observe_worker_holders_with_audit(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        &mut |_, _| {},
    )
}

/// Audit the same decisions that produce custody, without a second census or
/// diagnostic re-query deciding why an image failed the policy.
fn observe_worker_holders_with_audit(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
) -> (
    BTreeMap<PathBuf, WorkerState>,
    BTreeMap<PathBuf, ObservedProcess>,
    BTreeMap<PathBuf, String>,
) {
    let mut states: BTreeMap<_, _> = roots
        .iter()
        .cloned()
        .map(|root| (root, WorkerState::Free))
        .collect();
    let mut holders = BTreeMap::new();
    let mut details = BTreeMap::new();
    if roots.is_empty() {
        return (states, holders, details);
    }
    let census = match observer.census(65_536) {
        Ok(census) => census,
        Err(error) => {
            unplaceable(None, Some("complete process census unavailable"));
            let unknown = roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
                .collect();
            let detail = if matches!(
                error,
                provenant_supervisor::census::CensusError::ProcessList(_)
            ) {
                "process listing failed"
            } else {
                "process census unavailable"
            };
            return (
                unknown,
                holders,
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, detail.into()))
                    .collect(),
            );
        }
    };
    // Custody is decided per root. A gap that could hide a process anywhere
    // (an incomplete or truncated listing, or a process that could not be
    // described or placed) leaves every root that is not positively live
    // unknown. Current credentials and permissions cannot disprove a retained
    // cwd, including one acquired before a permission or credential change.
    for (&pid, &issue) in census.unknown_pids() {
        unplaceable(Some(pid), Some("stable process identity unavailable"));
        let detail = process_detail(cwd_observer, pid, &format!("{issue:?}"));
        record_unknown_all(&mut states, &mut details, &detail);
    }
    // Exclude the caller's verified ancestors. A walk that stops early leaves
    // the remaining ancestors in the census, where one inside a root still
    // makes it live, so an incomplete ancestry hides nothing.
    let mut excluded = BTreeSet::from([current.identity.clone()]);
    if ancestors {
        let mut child = current;
        while child.identity.pid != current.identity.session_id {
            let ProcessLookup::Present(parent) = census.process(child.parent_pid) else {
                break;
            };
            if parent.identity.boot != current.identity.boot
                || parent.identity.session_id != current.identity.session_id
                || census.ancestry(&child.identity, &parent.identity) != Ancestry::Yes
                || !excluded.insert(parent.identity.clone())
            {
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
            NativeCwd::PolicyExcluded(euid) => {
                // Accepted risk for sealed Apple code, applied per root owner.
                // Same-owner processes never earn either exception.
                for (root, state) in &mut states {
                    let excluded =
                        std::fs::metadata(root).is_ok_and(|metadata| euid != metadata.uid());
                    if !excluded {
                        unplaceable(
                            Some(fact.identity.pid),
                            Some("effective uid differs from root owner not established"),
                        );
                    }
                    if !excluded && *state != WorkerState::Live {
                        *state = WorkerState::Unknown;
                        details.entry(root.clone()).or_insert_with(|| {
                            process_detail(cwd_observer, fact.identity.pid, "cwd unreadable")
                        });
                    }
                }
            }
            NativeCwd::PolicyRejected(failure) => {
                unplaceable(Some(fact.identity.pid), failure.builtin_condition);
                let condition = failure.condition;
                record_unknown_all(
                    &mut states,
                    &mut details,
                    &process_detail(
                        cwd_observer,
                        fact.identity.pid,
                        &format!("cwd unreadable; policy_condition={condition}"),
                    ),
                );
            }
            NativeCwd::Unknown(category) => {
                let condition = if category.starts_with("cwd ") {
                    "actual cwd EPERM required"
                } else {
                    "stable process identity unavailable"
                };
                unplaceable(Some(fact.identity.pid), Some(condition));
                record_unknown_all(
                    &mut states,
                    &mut details,
                    &process_detail(cwd_observer, fact.identity.pid, &category),
                );
            }
            NativeCwd::Path(path) => {
                let Some(canonical) = canonical_cwd(&path) else {
                    unplaceable(
                        Some(fact.identity.pid),
                        Some("actual cwd EPERM required (readable cwd placement failed)"),
                    );
                    record_unknown_all(
                        &mut states,
                        &mut details,
                        &process_detail(cwd_observer, fact.identity.pid, "cwd unreadable"),
                    );
                    continue;
                };
                let mut placement_failed = false;
                let placed = record_live_path_with_audit(&mut states, &canonical, &mut || {
                    placement_failed = true
                });
                if placement_failed {
                    unplaceable(
                        Some(fact.identity.pid),
                        Some("actual cwd EPERM required (directory identity placement failed)"),
                    );
                }
                for (root, state) in &states {
                    if *state == WorkerState::Unknown {
                        details.entry(root.clone()).or_insert_with(|| {
                            process_detail(
                                cwd_observer,
                                fact.identity.pid,
                                "cwd or root identity unreadable",
                            )
                        });
                    }
                }
                for root in placed {
                    let holder = holders
                        .entry(root.clone())
                        .or_insert_with(|| ObservedProcess {
                            pid: fact.identity.pid,
                            command: fact.command.clone(),
                        });
                    details.insert(root, format!("process {}: cwd inside root", holder.pid));
                }
            }
        }
    }
    if !census.listing_is_complete() {
        unplaceable(None, Some("complete process listing required"));
        record_unknown_all(
            &mut states,
            &mut details,
            "process listing truncated or incomplete",
        );
    }
    (states, holders, details)
}

/// Canonicalise a process's working directory. A directory that has since been
/// deleted, such as a removed plugin cache or a pruned worktree, cannot be
/// resolved, but its place is still known: resolve the nearest existing
/// ancestor and re-append the rest. A process inside a deleted subdirectory of
/// a root therefore still counts as inside that root. Any other failure is
/// unobservable.
fn canonical_cwd(path: &Path) -> Option<PathBuf> {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        match canonicalize(existing) {
            Ok(resolved) => return Some(rest.iter().rev().fold(resolved, |p, c| p.join(c))),
            Err(FsError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        rest.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
}

enum NativeCwd {
    Path(PathBuf),
    Gone,
    PolicyExcluded(u32),
    PolicyRejected(provenant_os::pid::UnreadablePolicyFailure),
    Unknown(String),
}

fn process_detail(observer: &dyn CwdObserver, pid: i64, category: &str) -> String {
    let mut detail = format!("process {pid}: {category}");
    if let Some(fields) = observer.process_details(pid) {
        detail.push_str("; ");
        detail.push_str(&fields);
    }
    detail
}

fn record_unknown_all(
    states: &mut BTreeMap<PathBuf, WorkerState>,
    details: &mut BTreeMap<PathBuf, String>,
    detail: &str,
) {
    for (root, state) in states {
        if *state != WorkerState::Live {
            *state = WorkerState::Unknown;
            details
                .entry(root.clone())
                .or_insert_with(|| detail.to_owned());
        }
    }
}

fn observe_same_process(
    process_observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    expected: &ProcessFact,
) -> NativeCwd {
    match process_observer.observe(expected.identity.pid) {
        ProcessObservation::Present(before) if before.identity == expected.identity => {}
        ProcessObservation::Absent => return NativeCwd::Gone,
        ProcessObservation::Present(_) => return NativeCwd::Unknown("identity race".into()),
        ProcessObservation::Unknown(issue) => return NativeCwd::Unknown(format!("{issue:?}")),
    }
    let observed = match cwd_observer.cwd(expected.identity.pid) {
        Ok(CwdObservation::Path(path)) => NativeCwd::Path(path),
        Ok(CwdObservation::Gone) => return NativeCwd::Gone,
        Ok(CwdObservation::PermissionDenied) => cwd_observer
            .unreadable_policy_euid(expected.identity.pid, &[])
            .map(NativeCwd::PolicyExcluded)
            .unwrap_or_else(NativeCwd::PolicyRejected),
        Ok(CwdObservation::Unsupported) => NativeCwd::Unknown("cwd unsupported".into()),
        Err(_) => NativeCwd::Unknown("cwd query failed".into()),
    };
    match process_observer.observe(expected.identity.pid) {
        ProcessObservation::Present(after) if after.identity == expected.identity => observed,
        ProcessObservation::Absent => NativeCwd::Gone,
        ProcessObservation::Present(_) => NativeCwd::Unknown("identity race".into()),
        ProcessObservation::Unknown(issue) => NativeCwd::Unknown(format!("{issue:?}")),
    }
}

/// Place by directory identity, including the nearest extant ancestor of a
/// deleted cwd. Darwin's Data firmlink can give one directory two canonical
/// path spellings; textual prefix comparisons cannot establish occupancy.
#[cfg(test)]
fn record_live_path(states: &mut BTreeMap<PathBuf, WorkerState>, cwd: &Path) -> Vec<PathBuf> {
    record_live_path_with_audit(states, cwd, &mut || {})
}

fn record_live_path_with_audit(
    states: &mut BTreeMap<PathBuf, WorkerState>,
    cwd: &Path,
    unplaceable: &mut dyn FnMut(),
) -> Vec<PathBuf> {
    let ancestors = cwd.ancestors().try_fold(BTreeSet::new(), |mut ids, path| {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                ids.insert((metadata.dev(), metadata.ino()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return None,
        }
        Some(ids)
    });
    let mut placed = Vec::new();
    for (root, state) in states {
        let inside = ancestors.as_ref().and_then(|ids| {
            let metadata = std::fs::metadata(root).ok()?;
            metadata
                .is_dir()
                .then(|| ids.contains(&(metadata.dev(), metadata.ino())))
        });
        match inside {
            Some(true) => {
                *state = WorkerState::Live;
                placed.push(root.clone());
            }
            Some(false) => {}
            None => {
                unplaceable();
                if *state != WorkerState::Live {
                    *state = WorkerState::Unknown;
                }
            }
        }
    }
    placed
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
        let name = if let Some(remote) = name.strip_prefix("refs/remotes/") {
            GitRefName::remote_tracking(remote)
        } else {
            GitRefName::new(name)
        }
        .map_err(|_| WorktreeError::InvalidOutput)?;
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

/// The exclusion slot name for the canonical worktree `path`, below
/// `<state>/worktrees/<project>/`.
pub fn state_dir_name(path: &Path) -> OsString {
    path_digest(path).into()
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

/// A gitlink only runs a nested clean filter when its checkout exists. An
/// uninitialised gitlink has no `.git` marker, so status cannot follow it into
/// lane-controlled configuration and the tree can be reported as it is.
fn initialised_gitlink(root: &Path, path: &[u8]) -> Result<bool, WorktreeError> {
    let relative = Path::new(OsStr::from_bytes(path));
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(WorktreeError::InvalidOutput);
    }
    match std::fs::symlink_metadata(root.join(relative).join(".git")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(WorktreeError::UnknownWriter),
    }
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
    for entry in
        parse_index_entries(&index.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        let entry = entry.map_err(|_| WorktreeError::InvalidOutput)?;
        if entry.mode == 0o160000 && initialised_gitlink(repo.root_path(), entry.path)? {
            return Ok(WorktreeCondition::Unknown);
        }
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
    writer: Option<LockGuard>,
}

/// Canonical paths locate exclusion slots, never identity records. Keep slots
/// after removal so concurrent callers cannot lock different inodes.
fn writer_directory(project_state: &Directory, path: &Path) -> Result<Directory, WorktreeError> {
    let name = state_dir_name(path);
    match project_state.mkdir(&name, 0o700) {
        Ok(()) => project_state.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(project_state.open_verified_directory(&name)?)
}

/// Verify a registered worktree and probe its own effective Git configuration,
/// so every host Git operation on it neutralises what lanes can influence
/// there (an `onbranch:` include of a lane-writable file, for one).
fn verify_probed_worktree(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<VerifiedWorktree, WorktreeError> {
    let mut tree = verify_worktree(context.repo, path)?;
    context
        .utility
        .probe_worktree(context.executor, &mut tree, context.utility_budget, cancel)?;
    Ok(tree)
}

/// Git's inventory and linked-worktree verification establish membership.
/// Supervisor keeps per-attempt process evidence in the exclusion slot.
pub fn writer_custody(
    project_state: &Directory,
    worktree: &VerifiedWorktree,
    branch: &GitRefName,
    base: GitOid,
    worktree_list: impl FnOnce() -> Result<Vec<u8>, WorktreeError>,
) -> Result<(Directory, WriterBinding), WorktreeError> {
    worktree.repo().revalidate()?;
    let canonical = canonicalize(worktree.path())?;
    let entries = parse_worktree_list(&worktree_list()?)?;
    let entry = entries
        .iter()
        .find(|entry| !entry.bare && canonicalize(&entry.path).ok().as_deref() == Some(&canonical))
        .ok_or(WorktreeError::NotFound)?;
    if entry.locked || entry.prunable {
        return Err(WorktreeError::GitEntryHeld {
            locked: entry.locked,
        });
    }
    if entry.branch.as_ref() != Some(branch) {
        return Err(WorktreeError::IdentityChanged);
    }
    let state = writer_directory(project_state, &canonical)?;
    let binding = WriterBinding::from_verified(
        worktree.repo().root_dir(),
        worktree.repo().private_dir(),
        branch.as_str(),
        base,
    )
    .map_err(|_| WorktreeError::IdentityChanged)?;
    Ok((state, binding))
}

fn lock_worktree_for_mutation(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
) -> Result<LockedWorktree, WorktreeError> {
    lock_worktree(context, entry, cancel, false)
}

fn lock_worktree(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
    preview: bool,
) -> Result<LockedWorktree, WorktreeError> {
    if entry.locked || entry.prunable {
        return Err(WorktreeError::GitEntryHeld {
            locked: entry.locked,
        });
    }
    let verified = verify_probed_worktree(context, &entry.path, cancel)?;
    let writer = if preview {
        cache_writer_probe(context.project_state, verified.path()).map_err(|state| match state {
            WorkerState::Busy => WorktreeError::LockBusy,
            _ => WorktreeError::UnknownWriter,
        })?
    } else {
        let state = writer_directory(context.project_state, verified.path())?;
        let writer = match state.lock(
            OsStr::new(WRITER_LOCK),
            LockMode::Exclusive,
            WaitBudget::nonblocking(context.clock.mono_now()),
            cancel,
            context.clock,
        ) {
            Ok(guard) => guard,
            Err(FsError::LockTimeout { .. }) => return Err(WorktreeError::LockBusy),
            Err(FsError::Cancelled { .. }) => return Err(WorktreeError::Cancelled),
            Err(_) => return Err(WorktreeError::UnknownWriter),
        };
        Some(writer)
    };
    let canonical = canonicalize(verified.path())?;
    let (observations, details) = context
        .workers
        .observe_roots_with_details(std::slice::from_ref(&canonical));
    // The writer lock establishes exclusion.
    // A census gap does not make a free lease live; only a positive
    // observation of a live process refuses. Landing also needs this root
    // observed free, which a process the kernel refuses to describe does not
    // prevent.
    match observations
        .get(&canonical)
        .copied()
        .unwrap_or(WorkerState::Unknown)
    {
        WorkerState::Unknown if context.workers.requires_complete_census() => {
            Err(WorktreeError::IncompleteCensus {
                detail: details
                    .get(&canonical)
                    .cloned()
                    .unwrap_or_else(|| context.workers.incomplete_census_detail()),
            })
        }
        WorkerState::Free | WorkerState::Unknown => Ok(LockedWorktree { verified, writer }),
        WorkerState::Live => Err(live_refusal(context.workers, &canonical)),
        WorkerState::Busy => Err(WorktreeError::LockBusy),
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

/// Advance a free, registered linked worktree to a pinned descendant commit.
/// Writer exclusion covers verification and mutation. Local edits, divergence
/// and ignored files that would be overwritten always refuse the handoff. A
/// checkout already ahead of the pin is left as it is.
pub fn fast_forward(
    context: &WorktreeContext<'_>,
    path: &Path,
    head: GitOid,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    validate_binding(context)?;
    let _registry = repository_lock(context, LockMode::Exclusive, cancel)?;
    context.repo.revalidate()?;
    let path = canonicalize(path)?;
    if path == canonicalize(context.repo.root_path())? {
        return Err(WorktreeError::Primary);
    }
    let entries = read_worktrees(context, cancel)?;
    let entry = entries
        .iter()
        .find(|entry| canonicalize(&entry.path).ok().as_ref() == Some(&path))
        .ok_or(WorktreeError::NotFound)?;
    let branch = entry.branch.as_ref().ok_or(WorktreeError::InvalidRequest)?;
    let locked = lock_worktree_for_mutation(context, entry, cancel)?;
    let fresh = read_worktrees(context, cancel)?;
    let current = matching_worktree(&fresh, branch, context.repo.root_path())?
        .ok_or(WorktreeError::IdentityChanged)?;
    if canonicalize(&current.path)? != path
        || current.head != entry.head
        || current.locked
        || current.prunable
    {
        return Err(WorktreeError::IdentityChanged);
    }
    let repo = locked.verified.repo();
    match worktree_dirty_at(context, repo, cancel)? {
        WorktreeCondition::Clean | WorktreeCondition::Ignored => {}
        WorktreeCondition::Dirty => return Err(WorktreeError::Dirty),
        WorktreeCondition::GitOperationInProgress => {
            return Err(WorktreeError::GitOperationInProgress);
        }
        WorktreeCondition::Unknown => return Err(WorktreeError::UnknownWriter),
    }
    // A retry or fallback writer resumes in a checkout that already holds work
    // committed past the pin; that descendant of the pin is not a divergence.
    if current.head != head && is_ancestor(context, head, current.head, cancel)? {
        return Ok(());
    }
    if !is_ancestor(context, current.head, head, cancel)? {
        return Err(WorktreeError::SourceDiverged {
            current: current.head,
            target: head,
        });
    }
    repo.revalidate()?;
    let actual = run_git(context, repo, GitRead::RevParseHead, cancel)?;
    if exit_code(&actual)? != 0 || parse_oid(trim_ascii(&actual.stdout.bytes))? != current.head {
        return Err(WorktreeError::IdentityChanged);
    }
    if current.head != head {
        let outcome = execute_git_mutation(
            context.executor,
            repo,
            context.utility,
            GitMutation::FastForward { head },
            context.mutation_budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::SourceAdvanceFailed { target: head });
        }
    }
    repo.revalidate()?;
    check_writer_terminal(repo)?;
    let actual = run_git(context, repo, GitRead::RevParseHead, cancel)?;
    if exit_code(&actual)? != 0 || parse_oid(trim_ascii(&actual.stdout.bytes))? != head {
        return Err(WorktreeError::IdentityChanged);
    }
    Ok(())
}

/// Bounded, unpaged repository-scoped worktree inventory.
pub fn list(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    validate_binding(context)?;
    let _lock = repository_lock(context, LockMode::Exclusive, cancel)?;
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
        let (dirty, writer) = if path == primary {
            (
                worktree_dirty_at(context, context.repo, cancel)
                    .unwrap_or(WorktreeCondition::Unknown),
                combine_worker_state(WorkerState::Unknown, observed),
            )
        } else {
            match verify_probed_worktree(context, &path, cancel) {
                Ok(worktree) => (
                    worktree_dirty_at(context, worktree.repo(), cancel)
                        .unwrap_or(WorktreeCondition::Unknown),
                    if entry.locked || entry.prunable {
                        WriterState::Unknown
                    } else {
                        combine_worker_state(
                            disk_writer_state(context.project_state, &path),
                            observed,
                        )
                    },
                ),
                Err(_) => (WorktreeCondition::Unknown, WriterState::Unknown),
            }
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
        (WorkerState::Busy, _) | (_, WorkerState::Busy) => WriterState::Busy,
        (WorkerState::Unknown, _) | (_, WorkerState::Unknown) => WriterState::Unknown,
        (WorkerState::Free, WorkerState::Free) => WriterState::Free,
    }
}

/// Observe exclusion without creating state during listing or disk reports.
pub(crate) fn disk_writer_state(project_state: &Directory, path: &Path) -> WorkerState {
    match cache_writer_probe(project_state, path) {
        Ok(_) => WorkerState::Free,
        Err(state) => state,
    }
}

/// Probe an existing exclusion slot without creating state during preview.
pub fn cache_writer_probe(
    project_state: &Directory,
    path: &Path,
) -> Result<Option<LockGuard>, WorkerState> {
    let directory = match project_state.open_verified_directory(&state_dir_name(path)) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(_) => return Err(WorkerState::Unknown),
    };
    match directory.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive) {
        Ok(guard) => Ok(Some(guard)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(FsError::LockTimeout { .. }) => Err(WorkerState::Busy),
        Err(_) => Err(WorkerState::Unknown),
    }
}

/// Hold writer exclusion for cache mutation. Membership comes from the
/// caller's Git inventory; the slot supplies only exclusion.
pub fn cache_writer_guard(
    project_state: &Directory,
    path: &Path,
) -> Result<LockGuard, WorkerState> {
    let directory = writer_directory(project_state, path).map_err(|_| WorkerState::Unknown)?;
    directory
        .try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => WorkerState::Busy,
            _ => WorkerState::Unknown,
        })
}

/// Create a clean linked checkout at a pinned commit. A failed or killed
/// `worktree add` is rolled back (see `roll_back_failed_add`). Failed
/// verification after Git succeeds leaves a visible tree for inspection.
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
    if create_branch {
        let remote = run_git(context, context.repo, GitRead::RemoteBranchTips, cancel)?;
        if exit_code(&remote)? != 0 {
            return Err(WorktreeError::InvalidOutput);
        }
        let remote_tips = parse_branch_tips(&remote.stdout.bytes)?;
        for name in branch_tips.keys().chain(remote_tips.keys()) {
            let text = name.as_str();
            let local_collision = text
                .strip_prefix("refs/heads/")
                .is_some_and(|candidate| provenant_core::refs_collide(&request.branch, candidate));
            // Remote names can themselves contain slashes. Consider every
            // possible remote/branch boundary to avoid missing an alias.
            let remote_collision = text.strip_prefix("refs/remotes/").is_some_and(|rest| {
                rest.match_indices('/').any(|(index, _)| {
                    provenant_core::refs_collide(&request.branch, &rest[index + 1..])
                })
            });
            if local_collision || remote_collision {
                return Err(WorktreeError::BranchCollision {
                    existing: text.to_owned(),
                });
            }
        }
    }
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
    let state = writer_directory(context.project_state, &target)?;
    let _writer = state
        .try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => WorktreeError::LockBusy,
            other => other.into(),
        })?;
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
    let verified = verify_probed_worktree(context, &entry.path, cancel)?;
    if worktree_dirty_at(context, verified.repo(), cancel)? != WorktreeCondition::Clean {
        return Err(WorktreeError::RegistrationUnconfirmed);
    }
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
            WorkerState::Busy => WriterState::Busy,
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
    GitRevision::new(revision).map_err(|_| WorktreeError::InvalidRequest)?;
    // A tracking ref can alias another ref on the host filesystem. Verify the
    // root against the remote's advertisement and apply any operators to that.
    let (root, suffix) = provenant_core::split_revision_root(revision);
    let parsed = GitRevision::new(root).map_err(|_| WorktreeError::InvalidRequest)?;
    let symbolic = run_git(
        context,
        context.repo,
        GitRead::SymbolicRevision { revision: parsed },
        cancel,
    )?;
    let symbolic_name = if exit_code(&symbolic)? == 0 {
        std::str::from_utf8(trim_ascii(&symbolic.stdout.bytes))
            .map_err(|_| WorktreeError::InvalidOutput)?
    } else {
        ""
    };
    let tracking = symbolic_name.starts_with("refs/remotes/");
    let remote_head = (tracking && !root.contains('/') && GitRefName::branch(root).is_ok())
        .then(|| format!("{root}/HEAD"));
    // `remotes/origin/HEAD` and `refs/remotes/origin/HEAD` name the same
    // advertised default as `origin/HEAD`; symbolic resolution has already
    // followed them to a branch, so recover the remote from the spelling.
    let explicit_head = root
        .strip_prefix("refs/remotes/")
        .or_else(|| root.strip_prefix("remotes/"))
        .filter(|rest| rest.ends_with("/HEAD"));
    let candidate = if let Some(head) = &remote_head {
        head.as_str()
    } else if let Some(head) = explicit_head {
        head
    } else if root.ends_with("/HEAD") {
        root
    } else if tracking {
        symbolic_name
    } else {
        root
    };
    let remote = crate::utility::fetch_missing_remote_branch(
        context.executor,
        context.repo,
        context.utility,
        context.home,
        candidate,
        context.mutation_budget,
        cancel,
    )?;
    let verified_revision = match remote {
        crate::utility::RemoteBase::Found(oid) => format!("{oid}{suffix}"),
        crate::utility::RemoteBase::Missing => {
            return Err(WorktreeError::BaseNotFound {
                base: revision.to_owned(),
            });
        }
        // Git resolved the root to a remote-tracking ref, but the remote could
        // not vouch for it (for example `Origin/main` on a case-insensitive
        // filesystem, where the lookup finds `origin/main`). Falling back to the
        // local tracking OID would trust a ref the remote never advertised.
        crate::utility::RemoteBase::NotRemote if tracking => {
            return Err(WorktreeError::BaseUnverified {
                base: revision.to_owned(),
            });
        }
        crate::utility::RemoteBase::NotRemote => revision.to_owned(),
    };
    let parsed = GitRevision::new(&verified_revision).map_err(|_| WorktreeError::InvalidRequest)?;
    let outcome = run_git(
        context,
        context.repo,
        GitRead::ResolveRevision { revision: parsed },
        cancel,
    )?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::BaseNotFound {
            base: revision.to_owned(),
        });
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
        return Err(WorktreeError::GitEntryHeld {
            locked: entry.locked,
        });
    }
    let canonical = canonicalize(&entry.path)?;
    if canonical == canonicalize(context.repo.root_path())? {
        return Err(WorktreeError::Primary);
    }
    let mut locked = lock_worktree_for_mutation(context, entry, cancel)?;
    context.repo.revalidate()?;
    let fresh = read_worktrees(context, cancel)?;
    let current = fresh
        .iter()
        .find(|tree| canonicalize(&tree.path).ok().as_deref() == Some(&canonical))
        .ok_or(WorktreeError::IdentityChanged)?;
    if canonicalize(&current.path)? != canonical
        || current.branch != entry.branch
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
    // Keep writer exclusion through cache detachment and verified deletion.
    // Cleanup remains best-effort after Git has removed the worktree.
    let info = WorktreeInfo {
        path: canonical,
        branch: entry
            .branch
            .as_ref()
            .map(|branch| branch.as_str().to_owned()),
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
            Ok(oid) => {
                let oid = if candidate.as_str().starts_with("refs/remotes/") {
                    pin_revision(context, candidate.as_str(), cancel)?
                } else {
                    oid
                };
                return Ok((candidate, oid));
            }
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
                return Err(WorktreeError::AmbiguousName { name: name.into() });
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
        WorktreeError::LiveWriter | WorktreeError::LiveProcess(_) => RetainReason::LiveWriter,
        WorktreeError::LockBusy => RetainReason::LockBusy,
        WorktreeError::Cancelled | WorktreeError::IncompleteCensus { .. } => {
            RetainReason::UnknownWriter
        }
        error if error.is_custody_unknown() => RetainReason::UnknownWriter,
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
        .ok_or(WorktreeError::DetachedPrimary)?;
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
                let tip = entry.head;
                // `remove_tree` rechecks this tip under the worktree lock, so
                // a commit made after the caller's check cannot be removed.
                if expected_head.is_some_and(|expected| expected != tip) {
                    return Err(WorktreeError::IdentityChanged);
                }
                let Some(branch) = &entry.branch else {
                    if !discard && !is_ancestor(context, tip, integration_tip, cancel)? {
                        return Err(WorktreeError::DetachedUnmerged { head: tip });
                    }
                    let removed = remove_tree(context, entry, tip, cancel, output)?;
                    outcome.removed.push(removed.info.clone());
                    detached.push(removed);
                    return Ok(outcome);
                };
                if branch_tips.get(branch) != Some(&tip) {
                    return Err(WorktreeError::IdentityChanged);
                }
                let disposition = keep_disposition(
                    context,
                    branch,
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
                settle_branch(context, branch, tip, disposition, cancel, &mut outcome)?;
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
    fn builtin_audit_has_only_rejections(
        conditions: impl IntoIterator<Item = Option<&'static str>>,
    ) -> bool {
        conditions.into_iter().all(|condition| condition.is_some())
    }
    use super::*;
    use crate::test_support::{
        Fixture, add_poisoned_gitlink, assert_raw_git_runs_poison_filter, fixture_env,
        run_fixture_git,
    };
    use crate::utility::{GitMutation, execute_git_mutation};
    use provenant_core::Clock;
    use provenant_os::clock::SystemClock;
    use provenant_os::fs::Directory;
    use provenant_supervisor::census::ObservationIssue;
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct FreeWorkers;

    #[test]
    fn gc_native_output_preview_creates_no_writer_state() -> Result<(), Box<dyn Error>> {
        for existing_slot in [false, true] {
            let fixture = Fixture::new()?;
            fs::write(fixture.root.join(".gitignore"), ".worktrees/\ntarget/\n")?;
            fixture.run(&["add", ".gitignore"])?;
            fixture.run(&["commit", "-m", "ignore build output"])?;
            let test = TestContext::new(&fixture)?;
            let context = test.context();
            let path = fixture.root.join(".worktrees/native");
            fixture.run(&[
                "worktree",
                "add",
                "-b",
                "native",
                path.to_str().ok_or("path")?,
            ])?;
            fs::create_dir(path.join("target"))?;
            fs::write(path.join("target/artifact"), "cached output")?;
            let slot = context
                .project_state
                .current_path()?
                .join(state_dir_name(&path));
            if existing_slot {
                writer_directory(context.project_state, &path)?;
            }
            assert!(
                cache_writer_probe(context.project_state, &path)
                    .map_err(|_| "probe failed")?
                    .is_none()
            );
            let lanes = list(&context, &cancel())?;
            let regenerable = ["target".into()];
            let policy = WorktreeGcPolicy {
                upstream: None,
                terminal: &|_| false,
                no_live_lanes: &|_| true,
                regenerable: &regenerable,
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: true,
                foreign_merged_clean: false,
                foreign_idle_before_ns: None,
            };
            let preview = collect(&context, &lanes, &policy, &cancel())?;
            assert_eq!(preview.idle_output.len(), 1);
            assert_eq!(preview.idle_output[0].reason, "would_remove");
            assert!(path.join("target/artifact").is_file());
            assert_eq!(slot.exists(), existing_slot);
            assert!(!slot.join(WRITER_LOCK).exists());
            let applied = collect(
                &context,
                &lanes,
                &WorktreeGcPolicy {
                    dry_run: false,
                    ..policy
                },
                &cancel(),
            )?;
            assert_eq!(applied.idle_output.len(), 1);
            assert!(!path.join("target").exists());
            assert!(slot.join(WRITER_LOCK).is_file());
            let held =
                cache_writer_guard(context.project_state, &path).map_err(|_| "guard failed")?;
            assert!(matches!(
                cache_writer_probe(context.project_state, &path),
                Err(WorkerState::Busy)
            ));
            drop(held);
        }
        Ok(())
    }

    #[test]
    fn git_inventory_accepts_unregistered_and_recreated_worktrees() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let path = fixture.root.join(".worktrees/native");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native",
            path.to_str().ok_or("path")?,
        ])?;
        let name = state_dir_name(&path);
        assert!(
            context
                .project_state
                .open_verified_directory(&name)
                .is_err()
        );
        let inventory = || {
            Ok(
                run_git(&context, &fixture.repo, GitRead::WorktreeList, &cancel())?
                    .stdout
                    .bytes,
            )
        };
        let bind = |branch: &str| -> Result<_, WorktreeError> {
            writer_custody(
                context.project_state,
                &verify_worktree(&fixture.repo, &path)?,
                &GitRefName::branch(branch).map_err(|_| WorktreeError::InvalidRequest)?,
                fixture.head().map_err(|_| WorktreeError::InvalidOutput)?,
                inventory,
            )
        };
        assert_eq!(
            list(&context, &cancel())?
                .iter()
                .find(|tree| tree.path == path)
                .ok_or("tree")?
                .writer,
            WriterState::Free
        );
        assert!(
            context
                .project_state
                .open_verified_directory(&name)
                .is_err(),
            "listing creates no state"
        );
        let (state, _) = bind("native")?;
        assert!(
            state
                .metadata_at(OsStr::new("identity.json"), false)
                .is_err()
        );
        assert!(matches!(bind("other"), Err(WorktreeError::IdentityChanged)));
        // Legacy identity and registry reservations are never membership gates.
        fs::write(
            state.current_path()?.join("identity.json"),
            b"invalid old schema and inode",
        )?;
        fs::write(
            state.current_path()?.join("reservation.json"),
            b"legacy registry data",
        )?;
        fixture.run(&["worktree", "remove", path.to_str().ok_or("path")?])?;
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "recreated",
            path.to_str().ok_or("path")?,
        ])?;
        bind("recreated")?;
        let outcome = remove(
            &context,
            WorktreeRemove::Name {
                name: "recreated".into(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
        )?;
        assert_eq!(outcome.removed.len(), 1);
        // Reusing a removed path has no tombstone/schema/branch refusal.
        create(
            &context,
            WorktreeCreate {
                branch: "native".into(),
                base: None,
            },
            &cancel(),
        )?;
        Ok(())
    }

    #[test]
    fn writer_lock_busy_is_distinct_from_observed_live_process() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "busy".into(),
                base: None,
            },
            &cancel(),
        )?;
        let state = writer_directory(context.project_state, &info.path)?;
        let before = state.metadata_at(OsStr::new(WRITER_LOCK), false)?;
        let held = state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?;
        let request = || WorktreeRemove::Name {
            name: "busy".into(),
            discard: false,
            integration: None,
            expected_head: None,
        };
        let error = remove(&context, request(), &cancel())
            .err()
            .ok_or("removed busy tree")?;
        assert!(matches!(error, WorktreeError::LockBusy));
        assert_eq!(
            list(&context, &cancel())?
                .iter()
                .find(|tree| tree.path == info.path)
                .ok_or("tree")?
                .writer,
            WriterState::Busy
        );
        let report = collect(
            &context,
            &list(&context, &cancel())?,
            &WorktreeGcPolicy {
                upstream: Some(fixture.head()?),
                terminal: &|_| true,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: true,
                foreign_merged_clean: false,
                foreign_idle_before_ns: None,
            },
            &cancel(),
        )?;
        assert!(
            report
                .worktrees
                .retained
                .iter()
                .any(|tree| tree.reason == RetainReason::LockBusy)
        );
        drop(held);
        let live = WorktreeContext {
            workers: &NamedLive,
            ..test.context()
        };
        assert!(matches!(
            remove(&live, request(), &cancel()),
            Err(WorktreeError::LiveProcess(_))
        ));
        remove(&context, request(), &cancel())?;
        assert!(before.same_identity(&state.metadata_at(OsStr::new(WRITER_LOCK), false)?));
        Ok(())
    }

    #[test]
    fn foreign_gc_checks_whole_tree_idleness_before_pruning_output() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore output"])?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let path = fixture._temp.path().join("foreign");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "foreign",
            path.to_str().ok_or("path")?,
        ])?;
        fs::create_dir(path.join("target"))?;
        fs::write(path.join("target/artifact"), "old output")?;
        fs::File::open(path.join("target/artifact"))?.set_modified(std::time::UNIX_EPOCH)?;
        fs::File::open(path.join("target"))?.set_modified(std::time::UNIX_EPOCH)?;
        let lanes = list(&context, &cancel())?;
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &|_| true,
                regenerable: &["target".into()],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
                foreign_merged_clean: true,
                foreign_idle_before_ns: Some(1),
            },
            &cancel(),
        )?;
        assert!(report.worktrees.removed.is_empty());
        assert_eq!(
            fs::read_to_string(path.join("target/artifact"))?,
            "old output"
        );
        Ok(())
    }

    #[test]
    fn source_fast_forward_advances_clean_tree_and_preserves_ignored_output()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore build output"])?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "source-writer".into(),
                base: None,
            },
            &cancel(),
        )?;
        fs::create_dir(info.path.join("target"))?;
        fs::write(info.path.join("target/cache"), "retained output")?;
        fs::write(fixture.root.join("source-file"), "pinned source")?;
        fixture.run(&["add", "source-file"])?;
        fixture.run(&["commit", "-m", "combined source"])?;
        let target = fixture.head()?;
        let hooks = fixture.repo.common_git_dir().join("hooks");
        fs::create_dir_all(&hooks)?;
        fs::write(hooks.join("post-merge"), "#!/bin/sh\n: > hook-ran\n")?;
        fs::set_permissions(hooks.join("post-merge"), fs::Permissions::from_mode(0o700))?;
        fast_forward(&context, &info.path, target, &cancel())?;
        let actual = list(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing source writer")?;
        assert_eq!(actual.head, target);
        assert_eq!(actual.dirty, WorktreeCondition::Ignored);
        assert_eq!(
            fs::read_to_string(info.path.join("source-file"))?,
            "pinned source"
        );
        assert_eq!(
            fs::read_to_string(info.path.join("target/cache"))?,
            "retained output"
        );
        assert!(!info.path.join("hook-ran").exists());
        fast_forward(&context, &info.path, target, &cancel())?;
        Ok(())
    }

    #[test]
    fn fallback_writer_after_a_partial_commit_continues_past_the_pin() -> Result<(), Box<dyn Error>>
    {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "source-writer".into(),
                base: None,
            },
            &cancel(),
        )?;
        let pin = fixture.head()?;
        fast_forward(&context, &info.path, pin, &cancel())?;
        run_in_worktree(
            &fixture,
            &info.path,
            &["commit", "--allow-empty", "-m", "partial work"],
        )?;
        let ahead = read_worktrees(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing writer")?
            .head;
        assert_ne!(ahead, pin);
        fast_forward(&context, &info.path, pin, &cancel())?;
        let after = read_worktrees(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing writer")?
            .head;
        assert_eq!(after, ahead);
        Ok(())
    }

    #[test]
    fn source_fast_forward_refuses_dirty_diverged_or_leased_tree() -> Result<(), Box<dyn Error>> {
        for scenario in ["dirty", "diverged", "leased"] {
            let fixture = Fixture::new()?;
            let test = TestContext::new(&fixture)?;
            let context = test.context();
            let info = create(
                &context,
                WorktreeCreate {
                    branch: "source-writer".into(),
                    base: None,
                },
                &cancel(),
            )?;
            fixture.run(&["commit", "--allow-empty", "-m", "combined source"])?;
            let target = fixture.head()?;
            let state = context
                .project_state
                .open_verified_directory(&state_dir_name(&info.path))?;
            let lease = if scenario == "leased" {
                Some(state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?)
            } else {
                None
            };
            if scenario == "dirty" {
                fs::write(info.path.join("keep"), "uncommitted")?;
            } else if scenario == "diverged" {
                run_in_worktree(
                    &fixture,
                    &info.path,
                    &["commit", "--allow-empty", "-m", "diverged writer"],
                )?;
            }
            let before = read_worktrees(&context, &cancel())?
                .into_iter()
                .find(|entry| entry.path == info.path)
                .ok_or("missing writer")?
                .head;
            let error = fast_forward(&context, &info.path, target, &cancel())
                .err()
                .ok_or_else(|| format!("{scenario} source writer advanced"))?;
            match scenario {
                "dirty" => assert!(matches!(error, WorktreeError::Dirty)),
                "diverged" => assert!(
                    matches!(error, WorktreeError::SourceDiverged { current, target: requested } if current == before && requested == target)
                ),
                "leased" => assert!(matches!(error, WorktreeError::LockBusy)),
                _ => unreachable!(),
            }
            let after = read_worktrees(&context, &cancel())?
                .into_iter()
                .find(|entry| entry.path == info.path)
                .ok_or("missing writer")?
                .head;
            assert_eq!(after, before, "{scenario}");
            if scenario == "dirty" {
                assert_eq!(fs::read_to_string(info.path.join("keep"))?, "uncommitted");
            }
            drop(lease);
        }
        Ok(())
    }

    #[test]
    fn source_fast_forward_preserves_ignored_files_that_become_tracked()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "cache\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore local cache"])?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "source-writer".into(),
                base: None,
            },
            &cancel(),
        )?;
        fs::write(info.path.join("cache"), "keep ignored content")?;
        fs::write(fixture.root.join("cache"), "tracked source content")?;
        fixture.run(&["add", "-f", "cache"])?;
        fixture.run(&["commit", "-m", "track cache path"])?;
        let target = fixture.head()?;
        assert!(matches!(
            fast_forward(&context, &info.path, target, &cancel()),
            Err(WorktreeError::SourceAdvanceFailed { target: requested }) if requested == target
        ));
        let after = read_worktrees(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing writer")?
            .head;
        assert_eq!(after, info.head);
        assert_eq!(
            fs::read_to_string(info.path.join("cache"))?,
            "keep ignored content"
        );
        Ok(())
    }

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
            Err(WorktreeError::LockBusy)
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

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::print_stderr, clippy::disallowed_methods)] // Real process list and child are the acceptance fixture.
    fn native_full_census_places_clean_merged_worktree_and_same_user_sleep()
    -> Result<(), Box<dyn Error>> {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test = TestContext::new(&fixture)?;
        let created = create(
            &test.context(),
            WorktreeCreate {
                branch: "full-census-merged".into(),
                base: None,
            },
            &cancel(),
        )?;
        let root = canonicalize(&created.path)?;
        // Use the same host config selection and policy composition as runtime,
        // so native CI exercises host-owned exceptions rather than task knobs.
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME unavailable")?);
        let mut inputs = provenant_core::RootInputs::new(home, fixture.root.clone());
        inputs.environment_state_dir =
            std::env::var_os(provenant_core::STATE_DIRECTORY_ENV).map(PathBuf::from);
        let host_roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let complete = provenant_config::read(&host_roots)?.census_host_list_complete()?;
        let workers = CensusPolicyWorkers::from_roots(&SystemWorkerObserver, &host_roots)?;
        let ProcessObservation::Present(current) = current_process() else {
            return Err("native audit requires caller identity".into());
        };
        let mut unplaceable = BTreeMap::new();
        let mut incomplete = Vec::new();
        let (clean, _, clean_details) = observe_worker_holders_with_audit(
            std::slice::from_ref(&root),
            &SystemProcessObserver,
            &PolicyCwdObserver(&workers.entries),
            &current,
            true,
            &mut |pid, condition| {
                if let Some(pid) = pid {
                    let detail = process_detail(
                        &SystemCwdObserver,
                        pid,
                        condition.unwrap_or("ADMISSIBLE under built-in policy"),
                    );
                    unplaceable.insert(pid, (condition, detail));
                } else {
                    incomplete.push(condition.unwrap_or("unclassified census failure"));
                }
            },
        );
        let first = clean_details.get(&root).map_or("none", String::as_str);
        eprintln!(
            "FULL_CENSUS: clean merged worktree {:?}; first unplaceable detail: {first}",
            clean[&root]
        );
        for (_, detail) in unplaceable.values() {
            eprintln!("UNPLACEABLE: {detail}");
        }
        assert!(
            incomplete.is_empty(),
            "native census audit incomplete: {incomplete:?}"
        );
        assert!(
            builtin_audit_has_only_rejections(
                unplaceable.values().map(|(condition, _)| *condition)
            ),
            "every unplaceable process must fail a named built-in condition"
        );
        let mut child = Child(
            std::process::Command::new("/bin/sleep")
                .arg("60")
                .current_dir(&root)
                .spawn()?,
        );
        assert!(child.0.try_wait()?.is_none());
        let pid = i64::from(child.0.id());
        assert_eq!(
            SystemCwdObserver.sealed_platform_euid(pid),
            None,
            "same-user /bin/sleep cannot earn daemon exception"
        );
        let (occupied, occupied_details) =
            workers.observe_roots_for_land_with_details(std::slice::from_ref(&root));
        eprintln!(
            "FULL_CENSUS: same-user sleep {pid}: {:?}; placement detail: {:?}",
            occupied[&root],
            occupied_details.get(&root)
        );
        // A readable occupant is positively Live, a stronger refusal than
        // Unknown. Never weaken that existing refusal to satisfy the fixture.
        assert_eq!(occupied[&root], WorkerState::Live);
        assert_eq!(
            occupied_details[&root],
            format!("process {pid}: cwd inside root")
        );
        eprintln!(
            "ASSERTED: native full census audited {} unplaceable processes with named built-in failures; same-user sleep {pid} Live with placement detail",
            unplaceable.len()
        );
        if complete {
            assert_eq!(
                clean[&root],
                WorkerState::Free,
                "native clean full census: {first}"
            );
            eprintln!(
                "ASSERTED: native full census clean merged worktree Free (host list declared complete)"
            );
        } else {
            eprintln!(
                "SKIP: clean-Free assertion: confinement.census_host_list_complete=false; host exceptions are not declared complete; land cleanup may report the first unlisted daemon: {first}"
            );
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::print_stderr)] // Native fixture skips must explain missing host evidence.
    fn native_unreadable_windowserver_allows_cleanup_of_a_merged_clean_tree()
    -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, CensusError};

        // Select the real bystander to isolate its effect on cleanup, but
        // retain native identity rechecks and cwd observation.
        struct WindowServerWorkers {
            census: Census,
            current: ProcessFact,
        }
        impl ProcessObserver for WindowServerWorkers {
            fn boot_identity(&self) -> Result<String, CensusError> {
                SystemProcessObserver::boot_identity()
            }
            fn observe(&self, pid: i64) -> ProcessObservation {
                SystemProcessObserver.observe(pid)
            }
            fn census(&self, _: usize) -> Result<Census, CensusError> {
                Ok(self.census.clone())
            }
        }
        impl WorkerObserver for WindowServerWorkers {
            fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
                self.observe_roots_with_details(roots).0
            }
            fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
                observe_worker_roots(roots, self, &SystemCwdObserver, &self.current, true)
            }
            fn requires_complete_census(&self) -> bool {
                true
            }
        }
        let unavailable = |reason: String| -> Result<(), Box<dyn Error>> {
            if std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref() == Ok("1") {
                Err(reason.into())
            } else {
                eprintln!("SKIP: {reason}");
                Ok(())
            }
        };
        let census = match SystemProcessObserver.census(65_536) {
            Ok(census) => census,
            Err(error) => {
                return unavailable(format!("native process census unavailable: {error}"));
            }
        };
        let Some(windowserver) = census
            .facts()
            .find(|fact| fact.command.as_deref() == Some("WindowServer") && !fact.zombie)
        else {
            return unavailable("live WindowServer not visible in native process census".into());
        };
        let pid = windowserver.identity.pid;
        match SystemCwdObserver.cwd(pid) {
            Ok(CwdObservation::PermissionDenied) => {}
            Ok(observation) => {
                return unavailable(format!(
                    "WindowServer cwd is not unreadable: {observation:?}"
                ));
            }
            Err(error) => return unavailable(format!("WindowServer cwd query failed: {error}")),
        }
        let ProcessObservation::Present(current) =
            SystemProcessObserver.observe(i64::from(std::process::id()))
        else {
            return Err("native caller identity unavailable".into());
        };
        let workers = WindowServerWorkers {
            census: Census::from_observations(
                [current.clone(), windowserver.clone()],
                [],
                [],
                true,
                false,
                32,
            )?,
            current,
        };
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let created = create(
            &test_context.context(),
            WorktreeCreate {
                branch: "landed".into(),
                base: None,
            },
            &cancel(),
        )?;
        fs::set_permissions(&created.path, fs::Permissions::from_mode(0o700))?;
        let result = remove(
            &WorktreeContext {
                workers: &workers,
                ..test_context.context()
            },
            WorktreeRemove::Name {
                name: "landed".into(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
        );
        assert!(result.is_ok(), "WindowServer {pid}: {result:?}");
        assert!(!created.path.exists());
        assert!(!branch_exists(&fixture, "landed"));
        assert!(
            matches!(
                observe_same_process(&SystemProcessObserver, &SystemCwdObserver, windowserver),
                NativeCwd::PolicyExcluded(euid) if euid < 500 && euid != provenant_os::fs::effective_uid()
            ),
            "WindowServer must still be unreadable, running sealed platform code and foreign"
        );
        eprintln!(
            "ASSERTED: unreadable WindowServer {pid} allowed merged-clean land cleanup and branch deletion"
        );
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // A real child provides the cwd observation fixture.
    fn system_observer_names_the_process_inside_a_tree() -> Result<(), Box<dyn Error>> {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let fixture = Fixture::new()?;
        let path = canonicalize(fixture._temp.path())?.join("named-cwd");
        fs::create_dir(&path)?;
        let child = Child(
            std::process::Command::new("/bin/sh")
                .args(["-c", "read line"])
                .current_dir(&path)
                .stdin(std::process::Stdio::piped())
                .spawn()?,
        );
        let pid = i64::from(child.0.id());
        assert!(matches!(
            live_refusal(&SystemWorkerObserver, &path),
            WorktreeError::LiveProcess(process) if process.pid == pid
        ));
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

    /// A worker observer that sees one named process inside every root.
    struct NamedLive;
    impl WorkerObserver for NamedLive {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Live))
                .collect()
        }

        fn live_process(&self, _root: &Path) -> Option<ObservedProcess> {
            Some(ObservedProcess {
                pid: 4242,
                command: Some("zsh".into()),
            })
        }
    }

    struct UnknownWorkers;

    #[test]
    fn remove_registered_detached_worktree() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "detached".into(),
                base: None,
            },
            &cancel(),
        )?;
        run_in_worktree(&fixture, &info.path, &["checkout", "--detach"])?;
        let removed = remove(
            &context,
            WorktreeRemove::Name {
                name: "detached".into(),
                discard: false,
                integration: None,
                expected_head: Some(info.head),
            },
            &cancel(),
        )?;
        assert_eq!(removed.removed.len(), 1);
        assert_eq!(removed.removed[0].branch, None);
        assert_eq!(removed.removed[0].head, info.head);
        assert!(!info.path.exists());
        assert!(removed.pruned_refs.is_empty());
        assert!(
            read_branch_tips(&context, &cancel())?.contains_key(&GitRefName::branch("detached")?)
        );
        Ok(())
    }

    #[test]
    fn detached_removal_keeps_dirty_unmerged_and_live_work() -> Result<(), Box<dyn Error>> {
        for scenario in ["dirty", "unmerged", "live", "changed-head"] {
            let fixture = Fixture::new()?;
            let test = TestContext::new(&fixture)?;
            let mut context = test.context();
            let info = create(
                &context,
                WorktreeCreate {
                    branch: "detached".into(),
                    base: None,
                },
                &cancel(),
            )?;
            run_in_worktree(&fixture, &info.path, &["checkout", "--detach"])?;
            match scenario {
                "dirty" => fs::write(info.path.join("notes"), "preserve")?,
                "unmerged" | "changed-head" => run_in_worktree(
                    &fixture,
                    &info.path,
                    &["commit", "--allow-empty", "-m", "detached work"],
                )?,
                "live" => context.workers = &NamedLive,
                _ => {}
            }
            let request = |discard| WorktreeRemove::Name {
                name: "detached".into(),
                discard,
                integration: None,
                expected_head: (scenario == "changed-head").then_some(info.head),
            };
            let error = remove(&context, request(false), &cancel())
                .err()
                .ok_or("removed protected tree")?;
            assert!(
                match scenario {
                    "dirty" => matches!(error, WorktreeError::Dirty),
                    "unmerged" => matches!(error, WorktreeError::DetachedUnmerged { .. }),
                    "live" => matches!(error, WorktreeError::LiveProcess(_)),
                    "changed-head" => matches!(error, WorktreeError::IdentityChanged),
                    _ => false,
                },
                "{scenario}: {error}"
            );
            assert!(error.repair().is_some(), "{scenario}: {error}");
            assert!(info.path.exists());
            if scenario == "unmerged" {
                let removed = remove(&context, request(true), &cancel())?;
                assert_eq!(removed.removed.len(), 1);
                assert_ne!(removed.removed[0].head, info.head);
                assert!(removed.pruned_refs.is_empty());
                assert!(!info.path.exists());
            }
        }
        Ok(())
    }

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
    fn ignored_paths_refusal_names_regenerable_setting() {
        let message = WorktreeError::IgnoredPaths("target, .build".into()).with_repair();
        assert!(message.contains("target, .build"));
        assert!(message.contains("gc.projects.<id>.regenerable"));
        assert!(message.contains("provenant fabric identity"));
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
            assert_eq!(
                context
                    .project_state
                    .open_verified_directory(&state_dir_name(&path))
                    .is_ok(),
                !matches!(scenario, "default" | "preview" | "live-lane" | "unmerged"),
                "exclusion state after {scenario}"
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
    fn worker_census_places_a_process_in_a_deleted_directory_by_its_nearest_ancestor()
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
        struct Cwd(PathBuf);
        impl CwdObserver for Cwd {
            fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
                Ok(CwdObservation::Path(self.0.clone()))
            }
        }
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let outside = canonicalize(fixture._temp.path())?;
        let fact = |pid: i64, session_id| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: session_id,
                session_id,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let current = fact(100, 100);
        let processes = Processes(Census::from_observations(
            [current.clone(), fact(200, 200)],
            [],
            [],
            true,
            false,
            32,
        )?);
        let observe = |cwd: PathBuf| {
            observe_worker_roots(
                std::slice::from_ref(&root),
                &processes,
                &Cwd(cwd),
                &current,
                false,
            )
            .0[&root]
        };
        // A deleted directory elsewhere is unrelated to the root.
        assert_eq!(
            observe(outside.join("gone").join("deeper")),
            WorkerState::Free
        );
        // A deleted subdirectory of the root still holds the process inside it.
        assert_eq!(observe(root.join("gone").join("deeper")), WorkerState::Live);
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
                )
                .0[&root]
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
            let mut cwds = Cwds(
                facts
                    .iter()
                    .map(|fact| (fact.identity.pid, outside.clone()))
                    .collect(),
            );
            let observe = |cwds: &Cwds| {
                observe_worker_roots(
                    std::slice::from_ref(&root),
                    &processes,
                    cwds,
                    &current,
                    true,
                )
                .0[&root]
            };
            // An ambiguous ancestry excludes no unverified ancestor, so it
            // says nothing about a root no process is inside.
            assert_eq!(observe(&cwds), WorkerState::Free, "{ambiguity}");
            // The session leader is never verified once the walk stops, so
            // inside the root it still makes the root live.
            cwds.0.insert(10, root.clone());
            assert_eq!(observe(&cwds), WorkerState::Live, "{ambiguity}");
        }
        Ok(())
    }

    /// The native census over injected process and directory observations,
    /// with landing's demand for a complete census.
    struct InjectedCensus {
        census: provenant_supervisor::census::Census,
        cwds: BTreeMap<i64, CwdObservation>,
        current: ProcessFact,
        platform_euids: BTreeMap<i64, u32>,
    }
    impl ProcessObserver for InjectedCensus {
        fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
            Ok("boot".into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            match self.census.process(pid) {
                ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
                _ => ProcessObservation::Absent,
            }
        }
        fn census(
            &self,
            _: usize,
        ) -> Result<provenant_supervisor::census::Census, provenant_supervisor::census::CensusError>
        {
            Ok(self.census.clone())
        }
    }
    impl CwdObserver for InjectedCensus {
        fn sealed_platform_euid(&self, pid: i64) -> Option<u32> {
            self.platform_euids.get(&pid).copied()
        }
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(self.cwds.get(&pid).cloned().unwrap_or(CwdObservation::Gone))
        }
    }
    impl WorkerObserver for InjectedCensus {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            observe_worker_roots(roots, self, self, &self.current, true).0
        }
        fn requires_complete_census(&self) -> bool {
            true
        }
        fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
            observe_worker_roots(roots, self, self, &self.current, true)
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_worker_census_places_both_data_firmlink_spellings() -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        let fixture = Fixture::new()?;
        let resolved = canonicalize(&fixture.root)?;
        let root = resolved
            .strip_prefix("/System/Volumes/Data")
            .unwrap_or(&resolved);
        let data = Path::new("/System/Volumes/Data").join(root.strip_prefix("/")?);
        assert_ne!(root, data);
        let fact = |pid| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: Some("occupant".into()),
        };
        let mut observer = InjectedCensus {
            census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
            cwds: BTreeMap::new(),
            current: fact(100),
            platform_euids: BTreeMap::new(),
        };
        let sibling = root.with_file_name("outside");
        fs::create_dir(&sibling)?;
        fs::create_dir(root.join("child"))?;
        for (registered, reported) in [
            (root.to_path_buf(), data.clone()),
            (data, root.to_path_buf()),
        ] {
            for cwd in [
                reported.clone(),
                reported.join("child"),
                reported.join("deleted/descendant"),
                sibling.clone(),
            ] {
                observer.cwds.insert(200, CwdObservation::Path(cwd.clone()));
                let (states, holders, _) = observe_worker_holders(
                    std::slice::from_ref(&registered),
                    &observer,
                    &observer,
                    &observer.current,
                    false,
                );
                let inside = cwd != sibling;
                assert_eq!(
                    states[&registered],
                    if inside {
                        WorkerState::Live
                    } else {
                        WorkerState::Free
                    },
                    "root={registered:?}, cwd={cwd:?}"
                );
                assert_eq!(
                    holders.get(&registered).map(|holder| holder.pid),
                    inside.then_some(200)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn unreadable_platform_policy_rechecks_identity_and_preserves_other_unknowns()
    -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let fact = |pid| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let observer = InjectedCensus {
            census: Census::from_observations(
                [fact(100), fact(200), fact(300)],
                [],
                [],
                true,
                false,
                32,
            )?,
            cwds: BTreeMap::from([
                (200, CwdObservation::PermissionDenied),
                (300, CwdObservation::Unsupported),
            ]),
            current: fact(100),
            platform_euids: BTreeMap::from([(
                200,
                if provenant_os::fs::effective_uid() == 88 {
                    89
                } else {
                    88
                },
            )]),
        };
        assert_eq!(
            observer.observe_roots(std::slice::from_ref(&root))[&root],
            WorkerState::Unknown
        );
        struct Racing<'a> {
            observer: &'a InjectedCensus,
            calls: AtomicUsize,
            race_at: usize,
        }
        impl ProcessObserver for Racing<'_> {
            fn boot_identity(&self) -> Result<String, CensusError> {
                self.observer.boot_identity()
            }
            fn census(&self, limit: usize) -> Result<Census, CensusError> {
                self.observer.census(limit)
            }
            fn observe(&self, pid: i64) -> ProcessObservation {
                let mut result = self.observer.observe(pid);
                if self.calls.fetch_add(1, Ordering::SeqCst) == self.race_at
                    && let ProcessObservation::Present(fact) = &mut result
                {
                    fact.identity.native_start += 1;
                }
                result
            }
        }
        for race_at in [0, 1] {
            let processes = Racing {
                observer: &observer,
                calls: AtomicUsize::new(0),
                race_at,
            };
            assert!(
                matches!(observe_same_process(&processes, &observer, &fact(200)), NativeCwd::Unknown(reason) if reason == "identity race")
            );
        }
        let mut observer = observer;
        observer
            .cwds
            .insert(300, CwdObservation::Path(root.clone()));
        assert_eq!(
            observer.observe_roots(std::slice::from_ref(&root))[&root],
            WorkerState::Live
        );
        // Owner/identity lookup failure never licenses exclusion or placement.
        let missing = root.join("missing-root");
        assert_eq!(
            observer.observe_roots(std::slice::from_ref(&missing))[&missing],
            WorkerState::Unknown
        );
        Ok(())
    }

    #[test]
    fn worker_census_audit_keeps_all_unplaceable_processes_and_first_detail()
    -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let fact = |pid: i64| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let observer = InjectedCensus {
            census: Census::from_observations(
                [fact(1), fact(100), fact(200)],
                [],
                [],
                true,
                false,
                32,
            )?,
            cwds: BTreeMap::from([
                (1, CwdObservation::PermissionDenied),
                (200, CwdObservation::Unsupported),
            ]),
            current: fact(100),
            platform_euids: BTreeMap::new(),
        };
        let mut audit = BTreeMap::new();
        let (states, _, details) = observe_worker_holders_with_audit(
            std::slice::from_ref(&root),
            &observer,
            &observer,
            &observer.current,
            false,
            &mut |pid, condition| {
                audit.insert(pid, condition);
            },
        );
        assert_eq!(states[&root], WorkerState::Unknown);
        assert_eq!(
            details[&root],
            "process 1: cwd unreadable; policy_condition=image policy evidence unavailable"
        );
        assert_eq!(
            audit.keys().copied().collect::<Vec<_>>(),
            vec![Some(1), Some(200)]
        );
        assert!(builtin_audit_has_only_rejections(audit.values().copied()));
        // An unplaceable record with fully admissible built-in evidence must
        // fail acceptance, even when other records have real refusals.
        struct ListedOnlyFailure;
        impl CwdObserver for ListedOnlyFailure {
            fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
                Ok(CwdObservation::PermissionDenied)
            }
            fn unreadable_policy_euid(
                &self,
                _: i64,
                _: &[CensusIgnoreEntry],
            ) -> Result<u32, provenant_os::pid::UnreadablePolicyFailure> {
                Err(provenant_os::pid::UnreadablePolicyFailure {
                    condition: "stable signing team required",
                    builtin_condition: None,
                })
            }
        }
        audit.clear();
        let (states, _, _) = observe_worker_holders_with_audit(
            std::slice::from_ref(&root),
            &observer,
            &ListedOnlyFailure,
            &observer.current,
            false,
            &mut |pid, condition| {
                audit.insert(pid, condition);
            },
        );
        assert_eq!(states[&root], WorkerState::Unknown);
        assert_eq!(audit.len(), 2);
        assert!(!builtin_audit_has_only_rejections(audit.values().copied()));
        Ok(())
    }

    #[test]
    fn worker_census_detail_reports_incomplete_listing() -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let current = ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid: 100,
                native_start: 100,
                group_id: 100,
                session_id: 100,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let observer = InjectedCensus {
            census: Census::from_observations([current.clone()], [], [], false, false, 32)?,
            cwds: BTreeMap::new(),
            current,
            platform_euids: BTreeMap::new(),
        };
        let mut audit = Vec::new();
        let (states, _, details) = observe_worker_holders_with_audit(
            std::slice::from_ref(&root),
            &observer,
            &observer,
            &observer.current,
            false,
            &mut |pid, condition| audit.push((pid, condition)),
        );
        assert_eq!(states[&root], WorkerState::Unknown);
        assert_eq!(details[&root], "process listing truncated or incomplete");
        assert_eq!(
            audit,
            vec![(None, Some("complete process listing required"))]
        );
        Ok(())
    }

    #[test]
    fn unplaceable_path_failures_retain_process_image_diagnostics() -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        struct DetailedCwd(PathBuf);
        impl CwdObserver for DetailedCwd {
            fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
                Ok(CwdObservation::Path(self.0.clone()))
            }
            fn process_details(&self, _: i64) -> Option<String> {
                Some("path=/kernel/image, euid=501, cs_flags=0x20000001".into())
            }
        }
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let fact = |pid| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let observer = InjectedCensus {
            census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
            cwds: BTreeMap::new(),
            current: fact(100),
            platform_euids: BTreeMap::new(),
        };
        let loop_path = root.join("loop");
        std::os::unix::fs::symlink("loop", &loop_path)?;
        let file_path = root.join("file");
        fs::write(&file_path, "not a directory")?;
        for path in [loop_path, file_path] {
            let (states, details) = observe_worker_roots(
                std::slice::from_ref(&root),
                &observer,
                &DetailedCwd(path),
                &observer.current,
                false,
            );
            assert_eq!(states[&root], WorkerState::Unknown);
            let detail = &details[&root];
            for field in [
                "process 200:",
                "path=/kernel/image",
                "euid=501",
                "cs_flags=0x20000001",
            ] {
                assert!(detail.contains(field), "missing {field}: {detail}");
            }
        }
        Ok(())
    }

    #[test]
    fn host_policy_is_scoped_to_config_roots_and_preserves_injected_refusal()
    -> Result<(), Box<dyn Error>> {
        struct Probe(std::sync::Mutex<Vec<Vec<CensusIgnoreEntry>>>);
        impl WorkerObserver for Probe {
            fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, WorkerState::Unknown))
                    .collect()
            }
            fn observe_roots_with_policy(
                &self,
                roots: &[PathBuf],
                entries: &[CensusIgnoreEntry],
                for_land: bool,
            ) -> WorkerObservations {
                assert!(for_land);
                self.0.lock().unwrap().push(entries.to_vec());
                (self.observe_roots(roots), BTreeMap::new())
            }
        }
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let state = fixture._temp.path().join("host-config");
        fs::create_dir(&state)?;
        fs::write(
            state.join("config.toml"),
            "schema_version=1\n[knobs]\n'confinement.census_ignore_unreadable'=[{path='/Library/Daemon',team_id='TEAM123'}]\n",
        )?;
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(state);
        let configured_roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        inputs.state_dir = Some(fixture._temp.path().join("empty-config"));
        let empty_roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let probe = Probe(Default::default());
        let first = CensusPolicyWorkers::from_roots(&probe, &configured_roots)?;
        assert_eq!(
            first.observe_roots_for_land(std::slice::from_ref(&root))[&root],
            WorkerState::Unknown
        );
        let second = CensusPolicyWorkers::from_roots(&first, &empty_roots)?;
        assert_eq!(
            second.observe_roots_for_land(std::slice::from_ref(&root))[&root],
            WorkerState::Unknown
        );
        let seen = probe.0.lock().unwrap();
        assert_eq!(
            seen[0],
            vec![CensusIgnoreEntry {
                path: "/Library/Daemon".into(),
                team_id: "TEAM123".into()
            }]
        );
        assert!(
            seen[1].is_empty(),
            "policy must not leak across roots or wrappers"
        );
        Ok(())
    }

    #[test]
    fn listed_process_is_excluded_only_after_eperm_and_never_for_the_owner()
    -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        struct Listed {
            uid: u32,
            denied: bool,
        }
        impl CwdObserver for Listed {
            fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
                if self.denied {
                    Ok(CwdObservation::PermissionDenied)
                } else {
                    Err(provenant_os::pid::PidError::Io {
                        pid: pid as i32,
                        source: std::io::Error::other("cwd failure"),
                    })
                }
            }
            fn ignored_unreadable_euid(&self, _: i64, _: &[CensusIgnoreEntry]) -> Option<u32> {
                assert!(self.denied, "non-EPERM must never consult listing policy");
                Some(self.uid)
            }
        }
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let fact = |pid| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let observer = InjectedCensus {
            census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
            cwds: BTreeMap::new(),
            current: fact(100),
            platform_euids: BTreeMap::new(),
        };
        let owner = fs::metadata(&root)?.uid();
        for (name, uid, denied, expected) in [
            (
                "accepted foreign listing",
                owner.wrapping_add(1),
                true,
                WorkerState::Free,
            ),
            ("owner uid refused", owner, true, WorkerState::Unknown),
            (
                "non-EPERM refused",
                owner.wrapping_add(1),
                false,
                WorkerState::Unknown,
            ),
        ] {
            assert_eq!(
                observe_worker_roots(
                    std::slice::from_ref(&root),
                    &observer,
                    &Listed { uid, denied },
                    &observer.current,
                    false
                )
                .0[&root],
                expected,
                "{name}"
            );
        }
        Ok(())
    }

    #[test]
    fn denied_daemon_policy_distinguishes_system_owner_human_and_missing_evidence()
    -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let fact = |pid: i64| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let owner = fs::metadata(&root)?.uid();
        let mut observer = InjectedCensus {
            census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
            cwds: BTreeMap::from([(200, CwdObservation::PermissionDenied)]),
            current: fact(100),
            platform_euids: BTreeMap::new(),
        };
        for (name, evidence, expected) in [
            (
                "root daemon",
                Some(0),
                if owner == 0 {
                    WorkerState::Unknown
                } else {
                    WorkerState::Free
                },
            ),
            (
                "foreign service daemon",
                Some(if owner == 88 { 89 } else { 88 }),
                WorkerState::Free,
            ),
            (
                "last system account",
                Some(499),
                if owner == 499 {
                    WorkerState::Unknown
                } else {
                    WorkerState::Free
                },
            ),
            ("first human account", Some(500), WorkerState::Unknown),
            ("worktree owner", Some(owner), WorkerState::Unknown),
            (
                "missing image/signing/uid evidence",
                None,
                WorkerState::Unknown,
            ),
        ] {
            observer.platform_euids = evidence
                .map(|uid| BTreeMap::from([(200, uid)]))
                .unwrap_or_default();
            assert_eq!(
                observer.observe_roots(std::slice::from_ref(&root))[&root],
                expected,
                "{name}"
            );
        }
        // A denied fact cannot borrow the cwd exception without a pinned identity.
        observer.census = Census::from_observations(
            [fact(100)],
            [(200, ObservationIssue::PermissionDenied)],
            [],
            true,
            false,
            32,
        )?;
        observer.platform_euids = BTreeMap::from([(200, 88)]);
        assert_eq!(
            observer.observe_roots(std::slice::from_ref(&root))[&root],
            WorkerState::Unknown,
            "undescribed process"
        );
        Ok(())
    }

    #[test]
    fn cwd_query_errors_never_consult_the_daemon_policy() -> Result<(), Box<dyn Error>> {
        struct FailedCwd;
        impl CwdObserver for FailedCwd {
            fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
                Err(provenant_os::pid::PidError::Io {
                    pid: pid as i32,
                    source: std::io::Error::from_raw_os_error(5),
                })
            }
            fn sealed_platform_euid(&self, _: i64) -> Option<u32> {
                panic!("non-EPERM cwd failure reached daemon policy")
            }
        }
        use provenant_supervisor::census::{Census, ProcessIdentity};
        let fact = ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid: 200,
                native_start: 200,
                group_id: 200,
                session_id: 200,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let observer = InjectedCensus {
            census: Census::from_observations([fact.clone()], [], [], true, false, 32)?,
            cwds: BTreeMap::new(),
            current: fact.clone(),
            platform_euids: BTreeMap::new(),
        };
        assert!(matches!(
            observe_same_process(&observer, &FailedCwd, &fact),
            NativeCwd::Unknown(_)
        ));
        Ok(())
    }

    #[test]
    fn unreadable_occupant_cwd_keeps_the_tree_for_removal() -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let cancellation = cancel();
        let fact = |pid: i64| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let current = fact(100);
        let occupant = fact(200);
        let observer = InjectedCensus {
            census: Census::from_observations(
                [current.clone(), occupant],
                [],
                [],
                true,
                false,
                32,
            )?,
            cwds: BTreeMap::from([(200, CwdObservation::PermissionDenied)]),
            current,
            platform_euids: BTreeMap::new(),
        };
        let created = create(
            &test_context.context(),
            WorktreeCreate {
                branch: "landed".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        // The occupant is same-uid, so its unreadable cwd cannot be excluded.
        fs::set_permissions(&created.path, fs::Permissions::from_mode(0o755))?;
        let refused = remove(
            &WorktreeContext {
                workers: &observer,
                ..test_context.context()
            },
            WorktreeRemove::Name {
                name: "landed".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        );
        assert!(
            matches!(&refused, Err(WorktreeError::IncompleteCensus { detail }) if detail == "process 200: cwd unreadable; policy_condition=image policy evidence unavailable"),
            "{refused:?}"
        );
        assert!(created.path.exists());
        Ok(())
    }

    #[test]
    fn only_a_placed_foreign_process_allows_pruning_a_merged_clean_tree()
    -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, ProcessIdentity};
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let test_context = TestContext::new(&fixture)?;
        let cancellation = cancel();
        let fact = |pid: i64| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let outside = canonicalize(fixture._temp.path())?;
        // The caller, a readable bystander, a process whose directory the
        // kernel will not reveal, and a listed PID it will not describe.
        let census = |unknown: ObservationIssue| {
            Census::from_observations(
                [fact(100), fact(200), fact(300)],
                [(400, unknown)],
                [],
                true,
                false,
                32,
            )
        };
        let observer = |unknown, inside: Option<&Path>| -> Result<_, Box<dyn Error>> {
            let mut cwds = BTreeMap::from([
                (200, CwdObservation::Path(outside.clone())),
                (300, CwdObservation::PermissionDenied),
            ]);
            if let Some(path) = inside {
                cwds.insert(200, CwdObservation::Path(path.to_path_buf()));
            }
            Ok(InjectedCensus {
                census: census(unknown)?,
                cwds,
                current: fact(100),
                platform_euids: BTreeMap::new(),
            })
        };
        let remove_named = |workers: &InjectedCensus, name: &str| {
            remove(
                &WorktreeContext {
                    workers,
                    ..test_context.context()
                },
                WorktreeRemove::Name {
                    name: name.to_owned(),
                    discard: false,
                    integration: None,
                    expected_head: None,
                },
                &cancellation,
            )
        };
        let created = create(
            &test_context.context(),
            WorktreeCreate {
                branch: "landed".to_owned(),
                base: None,
            },
            &cancellation,
        )?;
        // Make the permission evidence independent of the test runner's umask.
        fs::set_permissions(&created.path, fs::Permissions::from_mode(0o755))?;

        // Uncertainty that could place a process anywhere keeps the tree.
        let unplaced = observer(ObservationIssue::Io, None)?;
        assert_eq!(
            unplaced.observe_roots(std::slice::from_ref(&created.path))[&created.path],
            WorkerState::Unknown
        );
        assert!(matches!(
            remove_named(&unplaced, "landed"),
            Err(WorktreeError::IncompleteCensus { ref detail }) if detail == "process 400: Io"
        ));
        // A readable process inside the tree still keeps it.
        let live = observer(ObservationIssue::PermissionDenied, Some(&created.path))?;
        assert!(matches!(
            remove_named(&live, "landed"),
            Err(WorktreeError::LiveWriter)
        ));
        assert!(created.path.exists());

        // Foreign credentials and unwritable root permissions cannot prove
        // where an unreadable process has its cwd.
        let foreign = observer(ObservationIssue::PermissionDenied, None)?;
        assert_eq!(
            foreign.observe_roots(std::slice::from_ref(&created.path))[&created.path],
            WorkerState::Unknown
        );
        assert!(matches!(
            remove_named(&foreign, "landed"),
            Err(WorktreeError::IncompleteCensus { ref detail }) if detail == "process 400: PermissionDenied"
        ));
        assert!(created.path.exists());

        // A complete, identity-checked cwd observation outside the tree
        // provides the positive non-occupancy evidence credentials cannot.
        let placed = InjectedCensus {
            census: Census::from_observations(
                [fact(100), fact(200), fact(300), fact(400)],
                [],
                [],
                true,
                false,
                32,
            )?,
            cwds: [200, 300, 400]
                .into_iter()
                .map(|pid| (pid, CwdObservation::Path(outside.clone())))
                .collect(),
            ..foreign
        };
        let listed = list(
            &WorktreeContext {
                workers: &placed,
                ..test_context.context()
            },
            &cancellation,
        )?;
        assert!(
            listed
                .iter()
                .any(|entry| entry.path == created.path && entry.writer == WriterState::Free)
        );
        remove_named(&placed, "landed")?;
        assert!(!created.path.exists());
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
                    NativeCwd::Path(path) => {
                        record_live_path(&mut states, &path);
                    }
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
                context
                    .project_state
                    .open_verified_directory(&state_name)
                    .is_ok(),
                "exclusion slot must remain after {scenario}"
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
        let tracking = fixture.run(&[
            "for-each-ref",
            "refs/remotes/origin/chore/cleanup-code-sweep",
        ])?;
        assert!(tracking.stdout.bytes.is_empty());
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
    fn remote_base_uses_exact_advertisement_instead_of_tracking_ref() -> Result<(), Box<dyn Error>>
    {
        let source = Fixture::new()?;
        source.run(&["branch", "-M", "main"])?;
        let parent = source.head()?;
        source.run(&["commit", "--allow-empty", "-m", "advertised tip"])?;
        let expected = source.head()?;
        let mut fixture = Fixture::new()?;
        let clone = fixture._temp.path().join("clone");
        fixture.run(&[
            "clone",
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
        fixture.run(&["config", "user.name", "Fixture"])?;
        fixture.run(&["config", "user.email", "fixture@example.invalid"])?;
        fixture.repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &cancel(),
        )?;
        fixture.run(&["commit", "--allow-empty", "-m", "different tracking tip"])?;
        let wrong = fixture.head()?;
        assert_ne!(wrong, expected);
        fixture.run(&["update-ref", "refs/remotes/origin/main", &wrong.to_string()])?;
        fixture.repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &cancel(),
        )?;
        let test_context = TestContext::new(&fixture)?;
        for alias in [
            "remotes/origin/main",
            "origin",
            "@{u}",
            "main@{upstream}",
            "@{push}",
        ] {
            assert_eq!(
                pin_revision(&test_context.context(), alias, &cancel())?,
                expected,
                "{alias}"
            );
        }
        for expression in [
            "origin/main~0",
            "remotes/origin/main^0",
            "@{u}~0",
            "main@{upstream}^{commit}",
            "@{push}^0",
        ] {
            assert_eq!(
                pin_revision(&test_context.context(), expression, &cancel())?,
                expected,
                "{expression}"
            );
        }
        // Every spelling of a tracking root verifies against the advertisement
        // and applies its suffix to that, never to the stale local tip (whose
        // first parent is `expected`, not `parent`).
        for root in [
            "origin/main",
            "remotes/origin/main",
            "refs/remotes/origin/main",
            "origin/HEAD",
            "remotes/origin/HEAD",
            "refs/remotes/origin/HEAD",
            "origin",
            "@{u}",
            "@{push}",
        ] {
            for (suffix, oid) in [
                ("", expected),
                ("~1", parent),
                ("^", parent),
                ("^{commit}", expected),
                ("~1^{commit}", parent),
            ] {
                assert_eq!(
                    pin_revision(
                        &test_context.context(),
                        &format!("{root}{suffix}"),
                        &cancel()
                    )?,
                    oid,
                    "{root}{suffix}"
                );
            }
        }
        let (_, integration) = pin_integration(&test_context.context(), "origin/main", &cancel())?;
        assert_eq!(integration, expected);
        let (_, default_integration) =
            pin_integration(&test_context.context(), "origin/HEAD", &cancel())?;
        assert_eq!(default_integration, expected);
        let created = create(
            &test_context.context(),
            WorktreeCreate {
                branch: "feature/verified-base".to_owned(),
                base: Some("origin/main".to_owned()),
            },
            &cancel(),
        )?;
        assert_eq!(created.head, expected);
        let tracking = fixture.run(&["rev-parse", "refs/remotes/origin/main"])?;
        assert_eq!(
            String::from_utf8(tracking.stdout.bytes)?.trim(),
            wrong.to_string()
        );
        Ok(())
    }

    #[test]
    fn tracking_base_the_remote_cannot_vouch_for_fails_closed() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        let head = fixture.head()?;
        fixture.run(&[
            "remote",
            "add",
            "origin",
            fixture.root.to_str().ok_or("non-UTF8 root")?,
        ])?;
        // A tracking ref whose remote is not configured, and one whose remote
        // is spelled in another case: Git resolves both locally.
        fixture.run(&["update-ref", "refs/remotes/gone/main", &head.to_string()])?;
        fixture.run(&["update-ref", "refs/remotes/origin/main", &head.to_string()])?;
        fixture.repo = fixture.runtime.open_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &cancel(),
        )?;
        let test_context = TestContext::new(&fixture)?;
        let mut unverifiable = vec!["gone/main", "gone/main~1", "remotes/gone/main^{commit}"];
        // On a case-insensitive filesystem `Origin/main` finds origin/main.
        let probe = fixture._temp.path().join("CaseProbe");
        fs::write(&probe, b"")?;
        let insensitive = fixture._temp.path().join("caseprobe").exists();
        if insensitive {
            unverifiable.extend(["Origin/main", "Origin/main~1", "remotes/Origin/main"]);
        }
        for base in unverifiable {
            let error = pin_revision(&test_context.context(), base, &cancel())
                .err()
                .ok_or_else(|| format!("{base} must not fall back to the local tracking OID"))?;
            assert!(
                matches!(&error, WorktreeError::BaseUnverified { base: named } if named == base),
                "{base}: {error}"
            );
        }
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
            assert!(matches!(error, WorktreeError::StaleRegistration));
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
    fn nested_worker_protects_all_containing_worktrees() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let outer = fixture.root.join("outer");
        let nested = outer.join("nested");
        let sibling = fixture.root.join("sibling");
        fs::create_dir_all(nested.join("src"))?;
        fs::create_dir(&sibling)?;
        let mut states = BTreeMap::from([
            (outer.clone(), WorkerState::Free),
            (nested.clone(), WorkerState::Free),
            (sibling.clone(), WorkerState::Free),
        ]);
        record_live_path(&mut states, &nested.join("src"));
        assert_eq!(states[&outer], WorkerState::Live);
        assert_eq!(states[&nested], WorkerState::Live);
        assert_eq!(states[&sibling], WorkerState::Free);
        Ok(())
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
    fn create_refuses_aliases_of_local_and_remote_branches() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "Feature/Topic"])?;
        fixture.run(&["update-ref", "refs/remotes/origin/Remote/Topic", "HEAD"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        for (branch, existing) in [
            ("feature/topic", "refs/heads/Feature/Topic"),
            ("remote/topic", "refs/remotes/origin/Remote/Topic"),
        ] {
            let error = create(
                &context,
                WorktreeCreate {
                    branch: branch.into(),
                    base: None,
                },
                &cancel(),
            );
            assert!(
                matches!(error, Err(WorktreeError::BranchCollision { existing: ref found }) if found == existing),
                "unexpected result for {branch}: {error:?}"
            );
            assert!(
                !fixture
                    .root
                    .join(".worktrees")
                    .join(branch.replace('/', "-"))
                    .exists()
            );
        }
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
    fn create_accepts_uninitialised_gitlinks_and_still_rejects_initialised_ones()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let head = fixture.head()?;
        fixture.run(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},sub"),
        ])?;
        fixture.run(&["commit", "-m", "tracked but uninitialised gitlink"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "feature/uninitialised-gitlink".to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        assert_eq!(created.dirty, WorktreeCondition::Clean);
        assert!(list(&context, &cancel())?.iter().any(|entry| {
            entry.branch.as_deref() == Some("refs/heads/feature/uninitialised-gitlink")
                && entry.dirty == WorktreeCondition::Clean
        }));

        let marker = fixture._temp.path().join("initialised-filter-ran");
        add_poisoned_gitlink(&fixture, &created.path, "sub", &marker)?;
        assert!(list(&context, &cancel())?.iter().any(|entry| {
            entry.branch.as_deref() == Some("refs/heads/feature/uninitialised-gitlink")
                && entry.dirty == WorktreeCondition::Unknown
        }));
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

    #[test]
    fn detached_worktree_names_the_head_and_both_fixes() -> Result<(), Box<dyn std::error::Error>> {
        let head = GitOid::parse("0123456789abcdef0123456789abcdef01234567")?;
        assert_eq!(
            WorktreeError::DetachedWorktree { head }.to_string(),
            "detached HEAD at 0123456789abcdef0123456789abcdef01234567; git switch -c BRANCH or remove --worktree and pass --mode read-only"
        );
        Ok(())
    }

    #[test]
    fn writer_busy_names_the_holder_only_when_known() {
        let holder = AttemptId::from_bytes([7; 16]);
        let known = WorktreeError::WriterBusy {
            holder: Some(holder),
        }
        .to_string();
        assert!(
            known.starts_with("worktree writer is busy, held by attempt "),
            "{known}"
        );
        assert_eq!(
            WorktreeError::WriterBusy { holder: None }.to_string(),
            "worktree writer is busy"
        );
    }
}
