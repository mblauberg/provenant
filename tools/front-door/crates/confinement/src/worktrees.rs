//! Repository-scoped linked-worktree lifecycle and merged-ref pruning.

mod census;
mod context;
mod custody;
mod gc;
mod inventory;
mod lifecycle;
mod owner;
pub use census::*;
pub use context::{with_context, with_context_using_workers};
pub use custody::*;
pub use gc::{OutputEntry, WorktreeGcPolicy, WorktreeGcReport, collect, collect_lazy};
pub use inventory::*;
pub use lifecycle::*;
pub use owner::{
    WorktreeOrigin, WorktreeOwner, load_owner, prune_owned, save_lane_owner_when_free, save_owner,
};

use crate::repo::{
    GitRefName, GitRevision, RepoError, VerifiedRepo, VerifiedWorktree, verify_worktree,
};
use crate::status::{StatusKind, StatusPath, parse_index_entries, parse_porcelain_v2_z};
use crate::utility::{
    GitMutation, GitRead, UtilityBudget, UtilityError, UtilityRuntime, execute_git_mutation,
    run_utility,
};
use provenant_config::ProjectBinding;
use provenant_core::Clock;
use provenant_core::{AttemptId, CancelToken, GitOid, RelativePath, WaitBudget, hex};
use provenant_os::exec::{ExecOutcome, Executor, Termination};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, PublishMode, canonicalize};
use provenant_os::pid::{
    CensusIgnoreEntry, CwdObservation, CwdObserver, ProcessUids, SystemCwdObserver,
};
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
    #[error("branch {branch} at {head} does not contain base {base}; omit --base to reuse it")]
    BranchExistsWithBase {
        branch: String,
        head: GitOid,
        base: GitOid,
    },
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
    #[error("worktree is dirty{}", dirty_suffix(.0))]
    Dirty(String),
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
    #[error("worktree registry reservation timed out")]
    RegistryLockTimeout,
    #[error("worktree writer is live")]
    LiveWriter,
    #[error(
        "worktree writer is live: process {_0} has its {kind} inside the tree; run from the main checkout or another directory",
        kind = _0.kind
    )]
    LiveProcess(ObservedProcess),
    #[error("incomplete worker census: {detail}")]
    IncompleteCensus { detail: String },
    #[error("worktree writer or registration is unknown")]
    UnknownWriter,
    #[error("worktree writer or registration is unknown at {}", .path.display())]
    UnknownWriterAt { path: PathBuf },
    /// The merged writer state is unknown specifically because native census
    /// evidence was incomplete. The public refusal text and code stay the same
    /// as `UnknownWriter`; lifecycle consumers get a typed retry cause.
    #[error("worktree writer or registration is unknown")]
    CensusUnknown { detail: String, path: PathBuf },
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

/// Stable machine cause for worktree admission transitions. Presentation may
/// change; lifecycle callers must branch on this value rather than refusal text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionCause {
    RegistryLockTimeout,
    CensusIncomplete,
    RegistrationOrCustodyUnknown,
    LiveWriter,
    DirtyTree,
    PermanentFailure,
}

fn dirty_suffix(paths: &str) -> String {
    if paths.is_empty() {
        String::new()
    } else {
        format!(": {paths}")
    }
}

impl WorktreeError {
    /// Returns a typed retry-relevant cause without making the error retryable
    /// by itself. The caller still owns deadlines and transition policy.
    pub fn admission_cause(&self) -> Option<AdmissionCause> {
        match self {
            Self::RegistryLockTimeout => Some(AdmissionCause::RegistryLockTimeout),
            Self::IncompleteCensus { .. } => Some(AdmissionCause::CensusIncomplete),
            Self::CensusUnknown { .. } => Some(AdmissionCause::CensusIncomplete),
            Self::UnknownWriter | Self::UnknownWriterAt { .. } | Self::GitEntryHeld { .. } => {
                Some(AdmissionCause::RegistrationOrCustodyUnknown)
            }
            Self::LockBusy | Self::LiveWriter | Self::LiveProcess(_) | Self::WriterBusy { .. } => {
                Some(AdmissionCause::LiveWriter)
            }
            Self::Dirty(_) | Self::IgnoredPaths(_) => Some(AdmissionCause::DirtyTree),
            _ => Some(AdmissionCause::PermanentFailure),
        }
    }

    /// Refusals that mean Provenant cannot prove who owns the tree. Callers
    /// that retain such a tree treat every one of them alike.
    pub fn is_custody_unknown(&self) -> bool {
        matches!(
            self,
            Self::UnknownWriter
                | Self::UnknownWriterAt { .. }
                | Self::CensusUnknown { .. }
                | Self::GitEntryHeld { .. }
        )
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
            Self::Dirty(_) => {
                "run `git -C PATH status`, then commit or preserve the changes before retrying"
            }
            Self::IgnoredPaths(_) => {
                "run `git -C PATH status --ignored`, then preserve the named files or declare directory names in `gc.projects.<id>.regenerable` in the global config (find the project ID with `provenant fabric identity`) before retrying"
            }
            Self::GitOperationInProgress => {
                "run `git -C PATH status` and complete or abort the Git operation before retrying"
            }
            Self::UnknownWriter | Self::UnknownWriterAt { .. } | Self::CensusUnknown { .. } => {
                "run `provenant worktree list` and inspect the registration and writer state before retrying"
            }
            Self::IncompleteCensus { .. } => {
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

/// The kind of evidence placing a process inside a worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProcessEvidenceKind {
    #[default]
    WorkingDirectory,
    ExecutableImage,
    OtherHeldPath,
}

impl std::fmt::Display for ProcessEvidenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkingDirectory => write!(f, "working directory"),
            Self::ExecutableImage => write!(f, "executable image"),
            Self::OtherHeldPath => write!(f, "other held path"),
        }
    }
}

/// A process seen inside a worktree, named so an operator can tell their own
/// shell from a real writer. Diagnostic only; it never grants signal authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedProcess {
    pub pid: i64,
    pub command: Option<String>,
    pub kind: ProcessEvidenceKind,
    pub cwd: Option<PathBuf>,
}

impl ObservedProcess {
    pub fn new(pid: i64, command: Option<String>, kind: ProcessEvidenceKind) -> Self {
        Self {
            pid,
            command,
            kind,
            cwd: None,
        }
    }

    pub fn cwd(pid: i64, command: Option<String>) -> Self {
        Self {
            pid,
            command,
            kind: ProcessEvidenceKind::WorkingDirectory,
            cwd: None,
        }
    }

    pub fn executable(pid: i64, command: Option<String>) -> Self {
        Self {
            pid,
            command,
            kind: ProcessEvidenceKind::ExecutableImage,
            cwd: None,
        }
    }

    pub fn other_path(pid: i64, command: Option<String>) -> Self {
        Self {
            pid,
            command,
            kind: ProcessEvidenceKind::OtherHeldPath,
            cwd: None,
        }
    }
}

impl std::fmt::Display for ObservedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.command, &self.cwd) {
            (Some(command), Some(cwd)) => {
                write!(f, "{} ({command}) at {}", self.pid, cwd.display())
            }
            (Some(command), None) => write!(f, "{} ({command})", self.pid),
            (None, Some(cwd)) => write!(f, "{} at {}", self.pid, cwd.display()),
            (None, None) => write!(f, "{}", self.pid),
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

/// Observed lane-ownership registration; absence does not prove custody is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerStatus {
    Registered,
    Unowned,
    Unknown,
    Primary,
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
    pub owner: OwnerStatus,
}

impl Serialize for WorktreeInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let path = AbsolutePathWire::from_path(&self.path).map_err(serde::ser::Error::custom)?;
        let mut state = serializer.serialize_struct("WorktreeInfo", 7)?;
        state.serialize_field("path", &path)?;
        state.serialize_field("branch", &self.branch)?;
        state.serialize_field("head", &self.head)?;
        state.serialize_field("dirty", &self.dirty)?;
        state.serialize_field("writer", &self.writer)?;
        state.serialize_field("owner", &self.owner)?;
        if self.owner == OwnerStatus::Unowned {
            state.serialize_field("next", "This checkout has no Provenant lane owner; it may have been created outside Provenant. Inspect its custody, then select it explicitly with --worktree <path>.")?;
        }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
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

/// The typed reason a worktree root is tainted (not free).
///
/// Distinguishes contention (which can be retried with backoff)
/// from uncertainty (scoped unplaceable processes, incomplete queries),
/// and foreign live writers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaintCause {
    /// A live process occupies the worktree.
    LiveWriter(ObservedProcess),
    /// A live writer was observed without attributable process details.
    UnattributedLiveWriter,
    /// A writer lock is held.
    LockContention,
    /// Census uncertainty: the root could not be proven free.
    Uncertainty(UncertaintyReason),
}

impl TaintCause {
    pub fn is_contention(&self) -> bool {
        matches!(self, Self::LockContention)
    }

    pub fn is_uncertainty(&self) -> bool {
        matches!(self, Self::Uncertainty(_))
    }
}

impl std::fmt::Display for TaintCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LiveWriter(process) => write!(f, "live writer: process {process}"),
            Self::UnattributedLiveWriter => write!(f, "live writer"),
            Self::LockContention => write!(f, "lock contention"),
            Self::Uncertainty(reason) => write!(f, "uncertainty: {reason}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UncertaintyReason {
    CensusUnavailable,
    UnstableProcessIdentity,
    CwdUnreadable,
    OccupancyIncomplete,
    PathUnplaceable,
    ListingIncomplete,
    PolicyRejected,
    RootUnreadable,
}

impl std::fmt::Display for UncertaintyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CensusUnavailable => write!(f, "census unavailable"),
            Self::UnstableProcessIdentity => write!(f, "unstable process identity"),
            Self::CwdUnreadable => write!(f, "cwd unreadable"),
            Self::OccupancyIncomplete => write!(f, "occupancy incomplete"),
            Self::PathUnplaceable => write!(f, "path unplaceable"),
            Self::ListingIncomplete => write!(f, "listing incomplete"),
            Self::PolicyRejected => write!(f, "policy rejected"),
            Self::RootUnreadable => write!(f, "root unreadable"),
        }
    }
}

/// Per-root custody states and reasons for unknown states.
pub type WorkerObservations = (BTreeMap<PathBuf, WorkerState>, BTreeMap<PathBuf, String>);

/// Per-root custody states and typed causes for tainted states.
pub type WorkerTaintObservations = (
    BTreeMap<PathBuf, WorkerState>,
    BTreeMap<PathBuf, TaintCause>,
);

#[cfg(test)]
mod census_tests;
#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_causes_keep_transient_observation_distinct_from_custody() {
        assert_eq!(
            lifecycle::retain_reason(&WorktreeError::RegistryLockTimeout),
            RetainReason::LockBusy
        );
        assert_eq!(
            WorktreeError::RegistryLockTimeout.admission_cause(),
            Some(AdmissionCause::RegistryLockTimeout)
        );
        assert_eq!(
            WorktreeError::IncompleteCensus {
                detail: "process listing failed".into()
            }
            .admission_cause(),
            Some(AdmissionCause::CensusIncomplete)
        );
        assert_eq!(
            WorktreeError::UnknownWriter.admission_cause(),
            Some(AdmissionCause::RegistrationOrCustodyUnknown)
        );
        assert_eq!(
            WorktreeError::LockBusy.admission_cause(),
            Some(AdmissionCause::LiveWriter)
        );
        assert_eq!(
            WorktreeError::Dirty(String::new()).admission_cause(),
            Some(AdmissionCause::DirtyTree)
        );
        assert_eq!(
            WorktreeError::InvalidRequest.admission_cause(),
            Some(AdmissionCause::PermanentFailure)
        );
    }

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
