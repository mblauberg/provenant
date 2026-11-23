//! Repository-scoped linked-worktree lifecycle and merged-ref pruning.

mod archive;
pub use archive::{ArchiveError, ArchiveInfo, ArchiveReport, collect_archives};
mod census;
mod context;
mod custody;
mod doctor;
#[cfg(test)]
mod doctor_tests;
mod evidence;
mod gc;
pub use evidence::{
    EvidenceRef, LandingAssessment, matches_pattern, removal_evidence, retained_refs,
    validate_removal_evidence,
};
mod inventory;
mod landed;
mod layout;
mod lifecycle;
mod owner;
mod refs;
mod removal;
mod setup;
mod stranded;
pub use census::*;
pub use context::{
    with_context, with_context_using_facts, with_context_using_workers,
    with_context_using_workers_and_facts,
};
pub use custody::*;
pub use doctor::{
    SeedDiagnosis, WorktreeDiagnosis, WorktreeDiagnosisState, WorktreeRepairKind, doctor,
};
pub use gc::{
    CandidateReport, CollectionMode, ForgeObservation, LocalRetirementScope, OutputEntry,
    RetirementFeeds, WorktreeGcPolicy, WorktreeGcReport, acknowledge_foreign_preview,
    branch_candidate, collect, collect_from, collect_selected, first_foreign_pass, forge_context,
    forge_remote, local_retirement_scope, resume_pending_retirements,
};
pub use inventory::*;
pub use landed::{
    CommitGraph, IntegrationEdge, LandedBy, Landing, LandingTarget, RepoGraph, Unproven,
    commit_parents, landed,
};
pub use layout::{ProjectLayout, project_layout_for_checkout, project_layout_git_environment};
// Strict resolvers without bare provenance are fixture seams; production
// resolves layouts through `UtilityRuntime`.
#[cfg(any(test, feature = "fixture"))]
pub use layout::{project_layout_for_binding, project_layout_for_common};
pub(crate) use layout::{
    project_layout_for_binding_primary, project_layout_for_binding_with_bare_setting,
    project_layout_for_common_with_bare_setting,
};
pub use lifecycle::*;
pub use owner::{
    OwnedWorktreeIssue, OwnedWorktreeListing, WorktreeOrigin, WorktreeOwner, branch_ownership,
    load_owner, owned_worktree_paths, prune_owned, refresh_owner_reference_while_held,
    save_lane_owner_when_free, save_owner, update_owner_when_unchanged, worktree_owner_index,
};
pub use refs::{RefLedger, RefObservation, RefRecord};
pub use removal::{
    Artefact, ArtefactClass, ArtefactOutcome, ArtefactState, RefDeletion, branch_checked_out,
    created_branch, delete_ref_leased, delete_ref_leased_when, has_commit, landing_in_repo,
    ref_tip,
};
pub use stranded::{EditStats, edit_stats};

use crate::repo::{
    GitRefName, GitRevision, RepoError, VerifiedRepo, VerifiedWorktree, verify_worktree,
};
use crate::status::{StatusKind, StatusPath, parse_index_entries, parse_porcelain_v2_z};
use crate::utility::{
    GitMutation, GitRead, UtilityBudget, UtilityError, UtilityRuntime, execute_git_mutation,
    execute_git_prepared_ref_delete, run_utility, run_verified_git_command,
};
use provenant_config::ProjectBinding;
use provenant_core::Clock;
use provenant_core::{
    AttemptId, CancelToken, GitOid, NameKind, NameRefusal, WaitBudget, close_matches, hex,
};
use provenant_os::exec::{ExecOutcome, Executor, Termination};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, PublishMode, canonicalize};
#[cfg(test)]
use provenant_os::pid::ProcessUids;
use provenant_os::pid::{CensusIgnoreEntry, CwdObservation, CwdObserver, SystemCwdObserver};
use provenant_records::custody::{RefEndpoint, RefOrigin, RefOwnership};
use provenant_records::{PathWireError, StructuredPathWire};
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
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_WORKTREES: usize = 10_000;
const MAX_BRANCHES: usize = 10_000;
const MAX_STATE_RECORD: u64 = 64 * 1024;
const WRITER_LOCK: &str = "writer.lock";

/// The configured path for a writer created from a validated branch name.
pub fn managed_writer_relative_path(
    project: &Path,
    host_state: &Path,
    branch: &str,
) -> Result<PathBuf, WorktreeError> {
    GitRefName::branch(branch).map_err(|_| WorktreeError::InvalidRequest)?;
    let ports = provenant_config::PortTable::read_for(Some(project), host_state)
        .map_err(WorktreeError::PortConfiguration)?;
    ports
        .managed_writer_path(branch)
        .map_err(WorktreeError::PortConfiguration)
}

/// Identify the managed writer branch for a path under the configured layout
/// roots. Relative paths are matched against the declared roots; an absolute
/// path is matched against those roots resolved against `project`, so a root
/// declared outside the checkout is treated exactly like the default one.
pub fn managed_writer_branch(
    project: &Path,
    host_state: &Path,
    path: &Path,
) -> Result<Option<String>, WorktreeError> {
    let ports = provenant_config::PortTable::read_for(Some(project), host_state)
        .map_err(WorktreeError::PortConfiguration)?;
    let branch = if path.is_absolute() {
        ports.worktree_layout.branch_for_path(project, path)
    } else {
        ports.managed_writer_branch(path)
    };
    Ok(branch.map(str::to_owned))
}

/// Context-free allocation roots for a registered Git common directory.
/// Without a verified layout owner, only host declarations may be applied and
/// they remain anchored at the common directory. Git-aware permission callers
/// add project-owned roots after resolving their actual owner.
pub(crate) fn allocation_directories(
    common: &Path,
    host_state: &Path,
) -> Result<Vec<PathBuf>, WorktreeError> {
    let ports = provenant_config::PortTable::read_for(None, host_state)
        .map_err(WorktreeError::PortConfiguration)?;
    Ok(ports.worktree_roots_under(common))
}

impl From<provenant_config::ConfigError> for WorktreeError {
    fn from(error: provenant_config::ConfigError) -> Self {
        Self::Configuration(Box::new(error))
    }
}

/// Declared alternatives for admitting a checkout whose HEAD is detached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetachedWorktreeRemedy {
    CreateBranch,
    ReadOnly,
}
impl DetachedWorktreeRemedy {
    pub const ALL: &'static [Self] = &[Self::CreateBranch, Self::ReadOnly];

    fn instruction(self) -> &'static str {
        match self {
            Self::CreateBranch => "git switch -c BRANCH",
            Self::ReadOnly => "remove --worktree and pass --mode read-only",
        }
    }
}
struct DetachedWorktreeRemedies;
impl std::fmt::Display for DetachedWorktreeRemedies {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, remedy) in DetachedWorktreeRemedy::ALL.iter().enumerate() {
            if index != 0 {
                formatter.write_str(" or ")?;
            }
            formatter.write_str(remedy.instruction())?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("worktree census configuration refused: {0}")]
    Configuration(#[from] Box<provenant_config::ConfigError>),
    #[error("worktree port configuration refused: {0}")]
    PortConfiguration(#[source] io::Error),
    #[error("worktree request is invalid")]
    InvalidRequest,
    #[error(
        "registered Git project has no verified primary checkout; run `provenant worktree list` from its primary checkout to refresh the layout, then retry"
    )]
    ProjectLayoutUnresolved,
    #[error("requested worktree path does not match the configured managed writer path")]
    ManagedPathMismatch {
        requested: PathBuf,
        expected: PathBuf,
    },
    #[error("worktree name {name} matches more than one checkout")]
    AmbiguousName { name: String },
    #[error("primary checkout has a detached HEAD; an integration branch is required")]
    DetachedPrimary,
    #[error("detached HEAD at {head} is not merged into the integration branch")]
    DetachedUnmerged { head: GitOid },
    #[error("detached HEAD at {head}; {}", DetachedWorktreeRemedies)]
    DetachedWorktree { head: GitOid },
    #[error("worktree writer is busy{}", .holder.as_ref().map_or_else(String::new, |attempt| format!(", held by attempt {attempt}")))]
    WriterBusy { holder: Option<AttemptId> },
    #[error(
        "writer lease held by attempt {holder} (branch {branch}, worktree {}, reservation age {age}, live: {live})",
        .worktree.display(), live = .live.map_or("unknown", |live| if live { "true" } else { "false" })
    )]
    WriterLeaseHeld {
        holder: AttemptId,
        branch: String,
        worktree: PathBuf,
        age: String,
        live: Option<bool>,
    },
    #[error("branch name collides with existing ref {existing}")]
    BranchCollision { existing: String },
    #[error(
        "branch {branch} exists on origin but not locally; fetch it and create the local branch at origin/{branch} before starting the writer"
    )]
    RemoteOnlyBranch { branch: String },
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
    #[error(
        "landing is unproven for branch {branch}: missing current retained-ref, tree-equivalent, integration or merged-forge proof"
    )]
    UnprovenLanding { branch: String },
    #[error("worktree writer or registration is unknown")]
    UnknownWriter,
    #[error("worktree writer or registration is unknown at {}", .path.display())]
    UnknownWriterAt { path: PathBuf },
    /// The merged writer state is unknown specifically because native census
    /// evidence was incomplete. The public refusal text and code stay the same
    /// as `UnknownWriter`; lifecycle consumers get a typed retry cause.
    #[error("worktree writer or registration is unknown")]
    CensusUnknown { detail: String, path: PathBuf },
    #[error("worktree writer or registration is unknown")]
    UnknownOccupancy { detail: String, path: PathBuf },
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
    #[error("worktree Git read failed: {0}")]
    GitRead(Box<GitReadFailure>),
    #[error("{operation} failed (Git reported lock failure: {lock_failure})")]
    MutationFailed {
        operation: GitMutationOperation,
        lock_failure: bool,
    },
    #[error("worktree was created but registration publication is unconfirmed")]
    RegistrationUnconfirmed,
    #[error("worktree identity changed during the operation")]
    IdentityChanged,
    #[error("primary checkout HEAD moved during worktree creation")]
    PrimaryMoved,
    #[error("worktree result path cannot be represented safely")]
    PathEncoding,
    #[error("durable retirement authority could not be read or advanced")]
    Retirement(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("worktree creation failed: {cause}; {}", if *.artifacts_absent {
        "no checkout or new branch remains, so it is safe to retry"
    } else {
        "checkout or branch state remains or could not be verified; inspect the configured worktree layout and branch before retrying"
    })]
    CreateFailed {
        cause: Box<WorktreeError>,
        artifacts_absent: bool,
    },
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    GitConfigRead(Box<provenant_os::git_config::ConfigError<UtilityError>>),
    #[error(transparent)]
    Utility(#[from] UtilityError),
    #[error(transparent)]
    Repository(#[from] RepoError),
    #[error(transparent)]
    PathWire(#[from] PathWireError),
}

/// A Git read that ended outside its contract: the subcommand it ran, how it
/// ended, and Git's first stderr line or the output defect that was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitReadFailure {
    pub command: String,
    pub status: String,
    pub detail: String,
}

impl std::fmt::Display for GitReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`git {}` {}", self.command, self.status)?;
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

/// Closed operation names: no Git output, arguments or host paths are disclosed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum GitMutationOperation {
    #[error("git bundle create")]
    BundleCreate,
    #[error("git update-ref (preservation)")]
    PreserveRef,
    #[error("git merge --ff-only")]
    FastForward,
    #[error("git worktree add")]
    WorktreeAdd,
    #[error("git worktree remove")]
    WorktreeRemove,
    #[error("git config")]
    SetBranchConfig,
    #[error("git update-ref --delete")]
    DeleteRef,
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
    pub fn mutation_failure(&self) -> Option<(GitMutationOperation, bool)> {
        match self {
            Self::MutationFailed {
                operation,
                lock_failure,
            } => Some((*operation, *lock_failure)),
            Self::CreateFailed { cause, .. } => cause.mutation_failure(),
            _ => None,
        }
    }

    /// Returns a typed retry-relevant cause without making the error retryable
    /// by itself. The caller still owns deadlines and transition policy.
    pub fn admission_cause(&self) -> Option<AdmissionCause> {
        match self {
            Self::RegistryLockTimeout => Some(AdmissionCause::RegistryLockTimeout),
            Self::IncompleteCensus { .. } => Some(AdmissionCause::CensusIncomplete),
            Self::CensusUnknown { .. } => Some(AdmissionCause::CensusIncomplete),
            Self::UnknownWriter
            | Self::UnknownWriterAt { .. }
            | Self::UnknownOccupancy { .. }
            | Self::GitEntryHeld { .. } => Some(AdmissionCause::RegistrationOrCustodyUnknown),
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
                | Self::UnknownOccupancy { .. }
                | Self::GitEntryHeld { .. }
        )
    }

    /// Typed alternatives for the detached-checkout admission refusal.
    pub fn detached_remedies(&self) -> &'static [DetachedWorktreeRemedy] {
        match self {
            Self::DetachedWorktree { .. } => DetachedWorktreeRemedy::ALL,
            _ => &[],
        }
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
            Self::RegistryLockTimeout => {
                "wait for the worktree registry lock holder to finish, then retry"
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
            Self::UnknownWriter
            | Self::UnknownWriterAt { .. }
            | Self::CensusUnknown { .. }
            | Self::UnknownOccupancy { .. } => {
                "run `provenant worktree list` and inspect the registration and writer state before retrying"
            }
            Self::IncompleteCensus { .. } => {
                "run `provenant worktree list` and retry when writer observation is available"
            }
            Self::Primary => {
                "run `provenant worktree list` and select a linked checkout instead of the primary"
            }
            Self::UnprovenLanding { .. } => {
                "verify the branch's landing with `provenant worktree list`, or explicitly discard it with `provenant worktree remove NAME --discard`"
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

/// The report-only projection of a process census observation. A cwd or open
/// descriptor names a dweller; it never grants authority to signal it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldProcess {
    /// The settled worktree or attempt directory the process is inside.
    pub root: StructuredPathWire,
    pub pid: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub evidence: ProcessEvidenceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<StructuredPathWire>,
    /// The single advisory line rendered by gc and doctor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub report: String,
}

impl HeldProcess {
    pub fn new(root: &Path, process: &ObservedProcess) -> Option<Self> {
        let root_wire = StructuredPathWire::from_path(root).ok()?;
        let mut row = Self {
            root: root_wire,
            pid: process.pid,
            command: process.command.clone(),
            evidence: process.kind,
            path: process
                .cwd
                .as_deref()
                .and_then(|path| StructuredPathWire::from_path(path).ok()),
            report: String::new(),
        };
        row.report = row.advisory();
        Some(row)
    }

    /// One advisory line for gc and doctor. It reports, and never promises a
    /// signal: a cwd observation carries no signalling authority.
    pub fn advisory(&self) -> String {
        let command = self.command.as_deref().unwrap_or("unknown command");
        let root = self
            .root
            .to_path_buf()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<path>".into());
        format!(
            "held process {} ({command}) in {root} by {} (report only; not signalled)",
            self.pid, self.evidence
        )
    }
}

/// Dependencies at the actual OS and utility seams. The caller supplies the
/// binding and its `state/worktrees/<ProjectId>` directory.
pub struct WorktreeContext<'a> {
    pub home: &'a Path,
    /// The selected verified checkout is the Git execution anchor. It does
    /// not own project configuration or managed worktree layout.
    pub repo: &'a VerifiedRepo,
    /// Verified project configuration and allocation roots. Bare repositories
    /// have a common-directory root and no project checkout.
    pub layout: &'a ProjectLayout,
    pub binding: &'a ProjectBinding,
    pub project_state: &'a Directory,
    pub executor: &'a dyn Executor,
    pub utility: &'a UtilityRuntime,
    /// Repository observations shared by operations in this context.
    pub facts: &'a RepoFacts,
    pub workers: &'a dyn WorkerObserver,
    pub clock: &'a dyn Clock,
    /// Bounds Git reads. Keep it short: a stuck read is a defect to surface.
    pub utility_budget: UtilityBudget,
    /// Bounds Git mutations. A checkout writes the whole tree and can
    /// legitimately take minutes on a large repository, so it is far longer.
    pub mutation_budget: UtilityBudget,
    pub lock_budget: WaitBudget,
}

/// A removal intent after the caller has verified its authority against the
/// exact live reference generation and tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetirementIntent {
    pub project: provenant_core::ProjectId,
    pub related_subjects: Vec<provenant_records::custody::Subject>,
    pub reference: provenant_records::custody::RefOwnership,
    pub expected_tip: GitOid,
    pub decision: provenant_records::custody::RetirementDecision,
}

/// Port to the single durable retirement owner. Confinement verifies live Git
/// state and performs deletion; the host application stores lifecycle intent.
pub trait RetirementPort {
    fn begin_retirement(
        &self,
        intent: RetirementIntent,
    ) -> Result<Option<provenant_records::custody::RetirementRecord>, WorktreeError>;

    fn retirement_for_subject(
        &self,
        project: provenant_core::ProjectId,
        subject: &provenant_records::custody::Subject,
    ) -> Result<Option<provenant_records::custody::RetirementRecord>, WorktreeError>;

    /// Pending and resumable lifecycle records for a project. Recovery is
    /// shared by confinement; the application only supplies the durable reader.
    fn pending_retirements(
        &self,
        project: provenant_core::ProjectId,
    ) -> Result<Vec<provenant_records::custody::RetirementRecord>, WorktreeError>;

    fn advance_retirement(
        &self,
        record: &provenant_records::custody::RetirementRecord,
        step: provenant_records::custody::RetirementStep,
        state: provenant_records::custody::RetirementStepState,
    ) -> Result<provenant_records::custody::RetirementRecord, WorktreeError>;
}

impl WorktreeContext<'_> {
    /// Host config root shared by project and host port declarations.
    pub fn host_state(&self) -> Result<PathBuf, WorktreeError> {
        self.project_state
            .current_path()
            .map_err(|_| WorktreeError::InvalidRequest)?
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or(WorktreeError::InvalidRequest)
    }
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

/// A worktree's observed occupancy, including the scope and evidence when it
/// cannot be determined. Process observations are advisory; held custody is
/// determined by the writer exclusion slot and retained fences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Occupancy {
    Available,
    Held,
    Observed {
        process: Option<ObservedProcess>,
    },
    Unknown {
        scope: PathBuf,
        evidence: String,
        source: OccupancyUnknownSource,
    },
}

impl Occupancy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Available => "free",
            Self::Held => "busy",
            Self::Observed { .. } => "live",
            Self::Unknown { .. } => "unknown",
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn is_observed(&self) -> bool {
        matches!(self, Self::Observed { .. })
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown { .. })
    }

    pub fn census_unknown(scope: PathBuf, evidence: impl Into<String>) -> Self {
        Self::Unknown {
            scope,
            evidence: evidence.into(),
            source: OccupancyUnknownSource::Census,
        }
    }

    pub fn custody_unknown(scope: PathBuf, evidence: impl Into<String>) -> Self {
        Self::Unknown {
            scope,
            evidence: evidence.into(),
            source: OccupancyUnknownSource::Custody,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OccupancyUnknownSource {
    Census,
    Custody,
}

/// CLI result facts are serialize-only. Its absolute path is emitted through
/// the explicit root-plus-PathWire envelope below, never lossy `Path::display`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// Bounded host-authored notices produced while creating this checkout.
    pub warnings: Vec<String>,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: GitOid,
    pub dirty: WorktreeCondition,
    pub occupancy: Occupancy,
    /// The existing owner record, or the evidence that it could not be read.
    pub owner: Result<Option<WorktreeOwner>, String>,
    pub primary: bool,
    /// Lane name carried by `provenant-owner.json`: its declared label, or the
    /// short branch name when a lane did not declare one.
    pub lane: Option<String>,
    /// Attempt that owns the checkout, from `provenant-owner.json`.
    pub attempt: Option<AttemptId>,
    /// Base commit pinned when the checkout was created, from the owner record.
    pub base: Option<String>,
}

/// Project the chair-facing identity carried by `provenant-owner.json`. Absent
/// or unreadable owner records yield no invented lane, attempt or base.
pub(crate) fn owner_identity(
    owner: &Result<Option<WorktreeOwner>, String>,
) -> (Option<String>, Option<AttemptId>, Option<String>) {
    match owner {
        Ok(Some(owner)) => (
            owner
                .label
                .clone()
                .or_else(|| owner.branch.rsplit('/').next().map(str::to_owned)),
            owner.attempt_id,
            Some(owner.base.clone()),
        ),
        _ => (None, None, None),
    }
}

impl WorktreeInfo {
    pub fn is_unowned(&self) -> bool {
        !self.primary && matches!(self.owner, Ok(None))
    }

    /// Build the identity fields from an already-loaded owner record. Every
    /// constructor calls this so the projection cannot drift between rows.
    pub(crate) fn with_owner_identity(mut self) -> Self {
        let (lane, attempt, base) = owner_identity(&self.owner);
        self.lane = lane;
        self.attempt = attempt;
        self.base = base;
        self
    }

    fn owner_label(&self) -> &'static str {
        if self.primary {
            "primary"
        } else {
            match &self.owner {
                Ok(Some(_)) => "registered",
                Ok(None) => "unowned",
                Err(_) => "unknown",
            }
        }
    }
}

impl Serialize for WorktreeInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let path = StructuredPathWire::from_path(&self.path).map_err(serde::ser::Error::custom)?;
        let fields = 6
            + usize::from(self.is_unowned())
            + usize::from(!self.warnings.is_empty())
            + usize::from(self.lane.is_some())
            + usize::from(self.attempt.is_some())
            + usize::from(self.base.is_some());
        let mut state = serializer.serialize_struct("WorktreeInfo", fields)?;
        if !self.warnings.is_empty() {
            state.serialize_field("warnings", &self.warnings)?;
        }
        state.serialize_field("path", &path)?;
        state.serialize_field("branch", &self.branch)?;
        state.serialize_field("head", &self.head)?;
        state.serialize_field("dirty", &self.dirty)?;
        state.serialize_field("writer", self.occupancy.label())?;
        state.serialize_field("owner", self.owner_label())?;
        if let Some(lane) = &self.lane {
            state.serialize_field("lane", lane)?;
        }
        if let Some(attempt) = &self.attempt {
            state.serialize_field("attempt", attempt)?;
        }
        if let Some(base) = &self.base {
            state.serialize_field("base", base)?;
        }
        if self.is_unowned() {
            state.serialize_field("next", "No Provenant lane currently owns this checkout. Inspect its custody, then select it explicitly with --worktree <path>.")?;
        }
        state.end()
    }
}

/// A `worktree locate` query resolves to exactly one inventory row or a
/// refusal carrying ranked close matches. Matching is exact and covers every
/// query kind: full or short branch, lane name, attempt id and PR number.
///
/// Spec: docs/specs/next-batch.md#worktree-locate
pub fn locate<'a>(query: &str, rows: &'a [WorktreeInfo]) -> Result<&'a WorktreeInfo, NameRefusal> {
    let folded = query.to_lowercase();
    if let Some(row) = rows.iter().find(|row| locate_matches(row, query, &folded)) {
        return Ok(row);
    }
    let mut universe: Vec<String> = Vec::new();
    for row in rows {
        if let Some(branch) = &row.branch {
            universe.push(branch.clone());
            if let Some(short) = branch.strip_prefix("refs/heads/") {
                universe.push(short.to_owned());
            }
        }
        if let Some(lane) = &row.lane {
            universe.push(lane.clone());
        }
        if let Some(attempt) = &row.attempt {
            universe.push(attempt.to_string());
        }
    }
    let candidates = close_matches(&folded, universe.iter().map(String::as_str))
        .into_iter()
        .map(str::to_owned)
        .collect();
    Err(NameRefusal {
        kind: NameKind("worktree"),
        query: query.to_owned(),
        candidates,
    })
}

fn locate_matches(row: &WorktreeInfo, query: &str, folded: &str) -> bool {
    let short = row
        .branch
        .as_deref()
        .and_then(|branch| branch.strip_prefix("refs/heads/"));
    let branch_hit = row
        .branch
        .as_deref()
        .is_some_and(|branch| branch.eq_ignore_ascii_case(query))
        || short.is_some_and(|branch| branch.eq_ignore_ascii_case(query));
    if branch_hit
        || row
            .lane
            .as_deref()
            .is_some_and(|lane| lane.to_lowercase() == folded)
        || row
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.to_string().eq_ignore_ascii_case(query))
    {
        return true;
    }
    // A bare number, with or without a `#`, also names a pull request. Branch
    // and lane spellings that carry the number resolve without a forge call.
    let number = query.strip_prefix('#').unwrap_or(query);
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let spellings = [
        format!("pr{number}"),
        format!("pr-{number}"),
        format!("pr/{number}"),
        format!("pull/{number}"),
        format!("refs/pull/{number}/head"),
        format!("#{number}"),
    ];
    row.branch.as_deref().is_some_and(|branch| {
        spellings
            .iter()
            .any(|spelling| branch.eq_ignore_ascii_case(spelling))
    }) || row.lane.as_deref().is_some_and(|lane| {
        spellings
            .iter()
            .any(|spelling| lane.eq_ignore_ascii_case(spelling))
    })
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
    RefAbsent,
    LockedOrPrunable,
    /// Provenant did not create the branch, or cannot observe that it did.
    NotCreated,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize)]
pub struct RemoveOutcome {
    pub removed: Vec<WorktreeInfo>,
    pub pruned_refs: Vec<String>,
    /// Unmerged branches deleted with `discard`. `head` is the commit the
    /// branch pointed at, which recovers it while Git still holds the object.
    pub discarded: Vec<DiscardedRef>,
    pub archives: Vec<ArchiveInfo>,
    pub retained: Vec<RetainedBranch>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiscardedRef {
    pub branch: String,
    pub head: GitOid,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

impl UncertaintyReason {
    pub const ALL: &'static [Self] = &[
        Self::CensusUnavailable,
        Self::UnstableProcessIdentity,
        Self::CwdUnreadable,
        Self::OccupancyIncomplete,
        Self::PathUnplaceable,
        Self::ListingIncomplete,
        Self::PolicyRejected,
        Self::RootUnreadable,
    ];
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
pub type WorkerObservations = (BTreeMap<PathBuf, Occupancy>, BTreeMap<PathBuf, String>);

/// Per-root custody states and typed causes for tainted states.
pub type WorkerTaintObservations = (BTreeMap<PathBuf, Occupancy>, BTreeMap<PathBuf, TaintCause>);

#[cfg(test)]
mod census_tests;
#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detached_worktree_refusal_carries_the_head_and_declared_remedies() {
        let head = "1".repeat(40).parse::<GitOid>().unwrap();
        let error = WorktreeError::DetachedWorktree { head };
        assert!(
            matches!(&error, WorktreeError::DetachedWorktree { head: actual } if *actual == head)
        );
        let mut branch = false;
        let mut read_only = false;
        for remedy in error.detached_remedies() {
            match remedy {
                DetachedWorktreeRemedy::CreateBranch => {
                    assert!(!branch, "duplicate branch remedy");
                    branch = true;
                }
                DetachedWorktreeRemedy::ReadOnly => {
                    assert!(!read_only, "duplicate read-only remedy");
                    read_only = true;
                }
            }
        }
        assert!(branch && read_only);
        assert!(WorktreeError::InvalidRequest.detached_remedies().is_empty());
    }

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
    fn branch_tips_accept_every_remote_ref_git_lists() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let listing = format!("refs/remotes/origin/main\0{oid}\0\nrefs/remotes/pr3467\0{oid}\0\n");
        let parsed = parse_branch_tips(listing.as_bytes()).expect("Git-listed remote refs");
        let names: Vec<_> = parsed.keys().map(GitRefName::as_str).collect();
        assert_eq!(names, ["refs/remotes/origin/main", "refs/remotes/pr3467"]);
    }

    #[test]
    fn git_read_failure_names_command_status_and_detail() {
        let failure = WorktreeError::GitRead(Box::new(GitReadFailure {
            command: GitRead::RemoteBranchTips.command_label(),
            status: "exited 128".into(),
            detail: "fatal: bad object".into(),
        }));
        assert_eq!(
            failure.to_string(),
            "worktree Git read failed: `git for-each-ref --format=%(refname)%00%(objectname)%00 refs/remotes` exited 128: fatal: bad object"
        );
    }

    #[test]
    fn malformed_git_records_refuse_without_partial_interpretation() {
        let missing_head = b"worktree /tmp/tree\0branch refs/heads/topic\0\0";
        let duplicate_head = b"worktree /tmp/tree\0HEAD 0000000000000000000000000000000000000000\0HEAD 0000000000000000000000000000000000000000\0branch refs/heads/topic\0\0";
        assert!(parse_worktree_list(missing_head).is_err());
        assert!(parse_worktree_list(duplicate_head).is_err());
    }

    fn located_fixture(
        branch: &str,
        lane: Option<&str>,
        attempt: Option<[u8; 16]>,
    ) -> WorktreeInfo {
        let owner = WorktreeOwner {
            attempt_id: attempt.map(AttemptId::from_bytes),
            operation_id: None,
            branch: branch.to_owned(),
            base: "1".repeat(40),
            host: provenant_core::HostId::from_bytes([1; 16]),
            created_at: provenant_core::WallTime::from_micros(1).unwrap(),
            origin: WorktreeOrigin::Lane,
            label: lane.map(str::to_owned),
            reference: None,
        };
        WorktreeInfo {
            warnings: Vec::new(),
            path: PathBuf::from(format!("/repo/.worktrees/{branch}")),
            branch: Some(format!("refs/heads/{branch}")),
            head: GitOid::parse(&"2".repeat(40)).unwrap(),
            dirty: WorktreeCondition::Clean,
            occupancy: Occupancy::Available,
            owner: Ok(Some(owner.clone())),
            primary: false,
            lane: None,
            attempt: None,
            base: None,
        }
        .with_owner_identity()
    }

    // Spec: docs/specs/next-batch.md#worktree-locate
    #[test]
    fn locate_resolves_every_query_kind_and_refuses_with_close_matches() {
        let attempt = [7; 16];
        let attempt_text = AttemptId::from_bytes(attempt).to_string();
        let rows = vec![
            located_fixture("feature/topic", Some("w3-topic"), Some(attempt)),
            located_fixture("pr/3810", None, None),
        ];
        for query in [
            "feature/topic",
            "refs/heads/feature/topic",
            "w3-topic",
            attempt_text.as_str(),
        ] {
            assert_eq!(
                locate(query, &rows).map(|row| &row.path),
                Ok(&rows[0].path),
                "{query}"
            );
        }
        assert_eq!(
            locate("3810", &rows).map(|row| &row.path),
            Ok(&rows[1].path)
        );
        assert_eq!(
            locate("pr-3810", &rows).map(|row| &row.path),
            Ok(&rows[1].path)
        );
        let refusal = locate("featur/topic", &rows).expect_err("typo must refuse");
        assert_eq!(refusal.kind, NameKind("worktree"));
        assert!(!refusal.candidates.is_empty());
    }

    // Spec: docs/specs/next-batch.md#worktree-list-carries-lane-and-base
    #[test]
    fn worktree_info_serializes_owner_lane_attempt_and_base() {
        let row = located_fixture("feature/topic", None, Some([3; 16]));
        let value = serde_json::to_value(&row).expect("worktree row");
        assert_eq!(value["lane"], json!("feature/topic"));
        assert_eq!(value["base"], json!("1".repeat(40)));
        assert!(value["attempt"].is_string());
        assert_eq!(value["writer"], json!("free"));
    }
}

#[cfg(test)]
mod worktree_layout_tests {
    use super::*;

    #[test]
    fn managed_writer_paths_use_the_project_layout_declaration() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".provenant")).unwrap();
        let mut ports = provenant_config::PortTable::builtin();
        let root = ports.worktree_layout.roots[0].clone();
        let branch_token = ports
            .worktree_layout
            .default
            .file_name()
            .unwrap()
            .to_owned();
        ports.worktree_layout.default = root.join(branch_token);
        std::fs::write(
            project.path().join(provenant_config::PORTS_CONFIG),
            serde_json::to_vec(&ports).unwrap(),
        )
        .unwrap();

        assert_eq!(
            managed_writer_relative_path(project.path(), project.path(), "feature/topic").unwrap(),
            root.join("feature-topic"),
        );
        assert_eq!(
            managed_writer_branch(project.path(), project.path(), &root.join("other")).unwrap(),
            Some("other".to_owned()),
        );
        assert_eq!(
            managed_writer_branch(project.path(), project.path(), &root.join("feature-topic"),)
                .unwrap(),
            Some("feature-topic".to_owned()),
        );
    }

    // Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
    #[test]
    fn context_free_allocation_roots_do_not_guess_a_project_owner() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        std::fs::create_dir(project.path().join(".provenant")).unwrap();
        let mut ports = provenant_config::PortTable::builtin();
        ports.worktree_layout.roots = vec![PathBuf::from("project-writers")];
        ports.worktree_layout.default = PathBuf::from("project-writers/<branch>");
        std::fs::write(
            project.path().join(provenant_config::PORTS_CONFIG),
            serde_json::to_vec(&ports).unwrap(),
        )
        .unwrap();

        let common = project.path().join(".git");
        let host_ports = provenant_config::PortTable::read_for(None, project.path()).unwrap();
        assert_eq!(
            allocation_directories(&common, project.path()).unwrap(),
            host_ports.worktree_roots_under(&common),
        );
    }
}
