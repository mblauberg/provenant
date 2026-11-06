//! Durable execution records and bounded, project-scoped readers.
#![forbid(unsafe_code)]
#![cfg_attr(
    all(test, feature = "fixture"),
    allow(
        dead_code,
        unused_imports,
        reason = "the custom continuation driver includes private fixtures without libtest"
    )
)]
mod admission;
mod artifacts;
mod board;
mod completion;
pub use board::{BoardLane, BoardPage};
pub use completion::{handoff_progress_guidance, lane_preamble};
mod controls;
mod correlation;
pub use correlation::{WORK_INDEX_FILE, WorkCoverage, WorkIndex, WorkSelector, validate_work};
mod db;
mod delivery;
mod disk;
mod git_context;
mod holder;
mod launch;
mod ledger;
mod objects;
mod overview;
mod plan;
mod policy;
pub use policy::{
    POLICY_ATTEMPT_CEILING, PolicyAction, PolicyCause, PolicyRetry, ResultAssessment,
};
mod pr_contribution;
pub use pr_contribution::{Contribution, Contributions, PullRequest, WEIGHT_ONE};
pub mod reactions;
pub use reactions::Reaction;
mod reader;
mod retention;
mod service;
#[cfg(test)]
mod service_tests;
pub mod settlement;
mod short_temporary;
mod store;
mod transcript;
pub use service::{DispatchRequest, DispatchTask, ErrorKind, Service};
pub use transcript::locate as locate_transcript;
pub mod build_dir_gc;
mod cargo_seed;
pub mod target_seed;
pub mod target_seed_gc;
mod writer;

pub use admission::{
    AdmissionPolicy, AdmissionTimeoutOrigin, AdmissionWindow, EffectiveAdmissionTimeout,
    HOST_ACTIVE, HOST_QUEUED, LaneWait, PROJECT_ACTIVE, Resources, SystemResources,
};
pub use controls::{
    CancelReceipt, Reconciled, Resume, TemporaryReason, TemporaryReasonKind, UncertainReason,
    holder_absent, overdue_at,
};
pub use delivery::HostDeliveryContext;
pub use git_context::{
    GitContext, continuation_history, ensure_local_branch, resolve_git_base, review_merge_base,
};
pub use holder::{HolderContext, HostLaunchContext, RootSeed, RunHolder, SealedTask};
pub use launch::{
    Decoded, LaunchPaths, PrepareRefusal, PreparedLaunch, PreparedStart, Providers,
    QUEUED_RUNS_PAGE_SIZE, RefusalKind, Started, ValidatedChild, access_root_allowed, capture_plan,
    concurrency,
};
pub use overview::{OverviewAttempt, OverviewState, RunOverview};
pub use plan::{isolated_read_only_plan, live_read_only_plan, read_only_plan};
pub use retention::{
    AttemptLogRemoval, AttemptTmpRemoval, ProviderStateGc, ProviderStateIssue, TOMBSTONE,
};
pub use writer::{
    LaunchConfinement, WriterFence, WriterLaunch, prepare_writer, resolve_reader_source,
    resolve_writer, resolve_writer_proposal,
};

pub use db::{ExecutionContext, Request, RunStore};
pub use provenant_records::run::*;
pub use provenant_supervisor::custody::AdmissionError;
pub use reader::{
    ArtifactPart, AttemptFilter, AttemptReader, Attempts, ByteRange, LanePage, LaneQuery,
    LaneTarget, OutputChunk, fact,
};
pub use store::{NewAttempt, PreparedRun, TerminalPublication};
use thiserror::Error;

pub const LANE_DEPTH_ENV: &str = "PROVENANT_LANE_DEPTH";
pub const PARENT_ATTEMPT_ENV: &str = "PROVENANT_PARENT_ATTEMPT_ID";

/// Return the depth of the run being dispatched under this lane.
pub fn next_lane_depth(parent_depth: u64, max_depth: u64) -> Result<u64> {
    let depth = parent_depth.checked_add(1).ok_or(RunError::DepthOverflow)?;
    if depth > max_depth {
        return Err(RunError::DepthExceeded { depth, max_depth });
    }
    Ok(depth)
}

/// The privacy or containment check that refused a run-store path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathCheck {
    /// The entry belongs to another user.
    Owner,
    /// The entry grants group or other access.
    Mode,
    /// The entry is a symlink.
    Symlink,
    /// The entry is not the regular file or directory the store requires.
    Kind,
    /// The entry has more than one hard link.
    Links,
    /// The entry resolves outside the directory it must stay within.
    Containment,
    /// The entry changed identity while it was being checked.
    Identity,
    /// The entry was expected and is absent.
    Missing,
}
impl PathCheck {
    fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Mode => "mode",
            Self::Symlink => "symlink",
            Self::Kind => "kind",
            Self::Links => "link count",
            Self::Containment => "outside the root",
            Self::Identity => "identity mismatch",
            Self::Missing => "missing",
        }
    }
    /// The first privacy check a regular file's metadata fails, if any.
    pub(crate) fn file(metadata: provenant_os::fs::Metadata) -> Option<Self> {
        use provenant_os::fs::FileKind;
        match metadata.kind {
            FileKind::Regular => {}
            FileKind::Symlink => return Some(Self::Symlink),
            _ => return Some(Self::Kind),
        }
        if metadata.links != 1 {
            Some(Self::Links)
        } else if metadata.uid != provenant_os::fs::effective_uid() {
            Some(Self::Owner)
        } else if metadata.mode & 0o077 != 0 {
            Some(Self::Mode)
        } else {
            None
        }
    }
    /// The first privacy check a directory's metadata fails, if any.
    pub(crate) fn directory(metadata: provenant_os::fs::Metadata) -> Option<Self> {
        if metadata.uid != provenant_os::fs::effective_uid() {
            Some(Self::Owner)
        } else if metadata.mode & 0o077 != 0 {
            Some(Self::Mode)
        } else {
            None
        }
    }
}
impl From<rusqlite::Error> for RunError {
    fn from(error: rusqlite::Error) -> Self {
        if provenant_sqlite::is_busy(&error) {
            Self::Contention
        } else {
            Self::Sqlite(error)
        }
    }
}

impl From<provenant_sqlite::VersionError> for RunError {
    fn from(error: provenant_sqlite::VersionError) -> Self {
        match error {
            provenant_sqlite::VersionError::Unsupported(version) => Self::Version(version),
            provenant_sqlite::VersionError::Unversioned => Self::Version(0),
            provenant_sqlite::VersionError::Sqlite(error) => error.into(),
        }
    }
}

impl From<provenant_sqlite::SnapshotError> for RunError {
    fn from(error: provenant_sqlite::SnapshotError) -> Self {
        match error {
            provenant_sqlite::SnapshotError::Filesystem(error) => error.into(),
            provenant_sqlite::SnapshotError::Sqlite(error) => error.into(),
            provenant_sqlite::SnapshotError::PathEncoding => {
                Self::Validation("backup path is not UTF-8")
            }
            provenant_sqlite::SnapshotError::ExhaustedNames => Self::Conflict,
        }
    }
}

impl From<provenant_sqlite::CompanionFault> for PathCheck {
    fn from(fault: provenant_sqlite::CompanionFault) -> Self {
        use provenant_sqlite::CompanionFault;
        match fault {
            CompanionFault::NotRegular => Self::Kind,
            CompanionFault::Symlink => Self::Symlink,
            CompanionFault::Owner => Self::Owner,
            CompanionFault::Mode => Self::Mode,
            CompanionFault::Links => Self::Links,
        }
    }
}
impl std::fmt::Display for PathCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRefusal {
    LaneConfig,
    LaneProgram,
    Unreadable,
}

impl From<&provenant_confinement::transport::TransportError> for TransportRefusal {
    fn from(error: &provenant_confinement::transport::TransportError) -> Self {
        use provenant_confinement::transport::TransportError;
        match error {
            TransportError::LaneConfig { .. } => Self::LaneConfig,
            TransportError::LaneProgram { .. } => Self::LaneProgram,
            TransportError::Unreadable => Self::Unreadable,
        }
    }
}

/// The comparison that refused an artifact. It names the check, never the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactCheck {
    /// The decoded request re-serialises to a different digest than its run record.
    RequestDigest,
    /// A recorded byte length differs from the file's length.
    RecordedLength,
    /// A recorded SHA-256 differs from the file's digest.
    RecordedDigest,
    /// A sealed artifact differs from the content proposed to replace it.
    SealConflict,
    /// A streamed artifact changed length or contents while it was read.
    Stream,
}
impl ArtifactCheck {
    fn name(self) -> &'static str {
        match self {
            Self::RequestDigest => "request digest",
            Self::RecordedLength => "recorded length",
            Self::RecordedDigest => "recorded digest",
            Self::SealConflict => "seal conflict",
            Self::Stream => "stream",
        }
    }
}
impl std::fmt::Display for ArtifactCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A non-secret value from an artifact comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactValue {
    /// A byte length.
    Length(u64),
    /// The first 12 hex digits of a SHA-256 digest.
    Digest(String),
}
impl ArtifactValue {
    /// The short form of `digest`, which prints as `sha256:<hex>`.
    pub fn digest(digest: impl std::fmt::Display) -> Self {
        let text = digest.to_string();
        let hex = text.strip_prefix("sha256:").unwrap_or(&text);
        Self::Digest(hex.chars().take(12).collect())
    }
}
impl std::fmt::Display for ArtifactValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Length(bytes) => write!(f, "{bytes}"),
            Self::Digest(prefix) => f.write_str(prefix),
        }
    }
}

/// The artifact and comparison that failed. Only names and lengths or digest
/// prefixes are kept, so the error can be shown without exposing artifact bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactProvenance {
    pub check: ArtifactCheck,
    /// The file name or path relative to the run or attempt directory.
    pub artifact: String,
    /// The recorded value. Absent when the check has no single value to show.
    pub expected: Option<ArtifactValue>,
    /// The value observed on disk or in the decoded request.
    pub actual: Option<ArtifactValue>,
}
impl std::fmt::Display for ArtifactProvenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.expected, &self.actual) {
            (Some(expected), Some(actual)) => write!(
                f,
                "{} {} expected {expected}, got {actual}",
                self.artifact, self.check
            ),
            _ => write!(
                f,
                "{} {} changed while it was read",
                self.artifact, self.check
            ),
        }
    }
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("{0}")]
    InvalidTaskTitle(String),
    #[error("{message}")]
    Input { kind: ErrorKind, message: String },
    #[error("host writer delivery failed at {0:?}")]
    Delivery(WriterDeliveryStage),
    #[error("host writer delivery could not reconcile the pull request: {reason}; next: {next}")]
    DeliveryReconciliation { reason: String, next: String },
    #[error(
        "host writer delivery failed at Integrate (integration_head_moved): the integration target moved after the round began and a loop round only fast-forwards it; rerun the graph to review the new head"
    )]
    IntegrationHeadMoved,
    #[error(
        "host writer delivery failed at Prepare (origin refresh): default branch collides with another branch name"
    )]
    OriginDefaultCollision,
    #[error(
        "host writer delivery failed at Prepare (integration target): default and primary checkout branches require PR delivery"
    )]
    ProtectedDeliveryBranch,
    #[error("host writer delivery failed at Prepare (origin refresh): {0}")]
    OriginNormalisationCollision(String),
    #[error("host writer delivery failed at Prepare (transport): {reason}")]
    DeliveryTransport {
        kind: TransportRefusal,
        reason: String,
    },
    #[error("Git transport refused: {reason}")]
    GitTransport {
        kind: TransportRefusal,
        reason: String,
    },
    #[error("host writer delivery failed at Prepare ({step}): {reason}")]
    DeliveryPrepare { step: &'static str, reason: String },
    #[error(
        "host writer delivery failed at {stage:?}: {tool} {command} output exceeded the {limit}-byte host limit"
    )]
    DeliveryLimit {
        stage: WriterDeliveryStage,
        tool: &'static str,
        command: String,
        limit: usize,
    },
    #[error("Git executable unavailable")]
    GitUnavailable,
    #[error("lane Git setup lock acquisition timed out")]
    LaneSetupLockTimeout,
    #[error("provider evidence could not be committed: {0}")]
    ProviderEvidence(String),
    #[error("confinement refused: {0}")]
    Confinement(#[from] provenant_confinement::permissions::PermissionError),
    #[error("writer scope refused: {}", .0.with_repair())]
    Writer(provenant_confinement::worktrees::WorktreeError),
    #[error(
        "worktree {} belongs to project {}, not the selected project {}",
        worktree.display(),
        project.display(),
        selected.display()
    )]
    WriterProjectMismatch {
        worktree: std::path::PathBuf,
        project: std::path::PathBuf,
        selected: std::path::PathBuf,
    },
    #[error("local worktree {0} does not exist; create it with `provenant worktree create <name>`")]
    WorktreeMissing(std::path::PathBuf),
    #[error("worktree {0} is not listed by Git; run `git worktree list --porcelain`")]
    WorktreeUnregistered(std::path::PathBuf),
    #[error("writer Git ref lock {} already exists; check for a running Git process, then remove the stale lock before retrying", .0.display())]
    WriterRefLocked(std::path::PathBuf),
    #[error(
        "Git setup lock provenant-lane-setup.lock timed out after {seconds}s; increase domains.fabric.lanes.setup_lock_wait_seconds"
    )]
    SetupLockTimeout { seconds: u64 },
    #[error("writer import retained for worktree {}, run {run}, attempt {attempt}", worktree.display())]
    RetainedWriterImport {
        worktree: std::path::PathBuf,
        run: provenant_core::RunId,
        attempt: provenant_core::AttemptId,
    },
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error("invalid run record: attempt reservation retained")]
    AttemptReservationRetained,
    #[error("invalid run record: {0}")]
    Validation(&'static str),
    #[error("invalid run record: writer HEAD is not the lane branch")]
    WriterHeadNotLaneBranch,
    #[error(
        "invalid run record: writer commit {actual} differs from pinned source commit {expected}"
    )]
    WriterSourceMismatch {
        expected: provenant_core::GitOid,
        actual: provenant_core::GitOid,
    },
    #[error("invalid run record: review required; use lane deliver --accept-review ATTEMPT")]
    ReviewAcceptanceRequired,
    #[error("lane object import refused: {0}")]
    ObjectImportLimit(&'static str),
    #[error("invalid Git base {0:?}: expected a resolvable commit reference")]
    InvalidBase(String),
    #[error("run-store schema version {0} is unsupported")]
    Version(i64),
    #[error("run-store path {} is not private or contained: {check}", path.display())]
    UnsafePath {
        path: std::path::PathBuf,
        check: PathCheck,
    },
    #[error("run-store operation conflicts with its immutable request")]
    OperationConflict,
    #[error("run record was not found in this scope")]
    NotFound,
    #[error("attempt is not terminal")]
    Nonterminal,
    #[error(
        "provider state has expired under retention.provider_state_days; this attempt can no longer be resumed; dispatch a new lane with its retained result or transcripts as context"
    )]
    ProviderStateExpired,
    #[error("run record revision conflicts or is immutable")]
    Conflict,
    #[error("continuation generation conflict; current generation is {current_generation}")]
    GenerationConflict { current_generation: u64 },
    #[error("read or record exceeds byte limit {limit}")]
    TooLarge { limit: u64 },
    #[error("artifact digest or length does not match its record: {0}")]
    ArtifactMismatch(ArtifactProvenance),
    #[error("terminal publication is already sealed; recover its original facts and result")]
    TerminalRecoveryRequired,
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    #[error("operation refused by the execution context")]
    Refused,
    #[error("nested lane depth {depth} exceeds configured maximum {max_depth}")]
    DepthExceeded { depth: u64, max_depth: u64 },
    #[error("nested lane depth overflowed")]
    DepthOverflow,
    #[error("host queue is full")]
    Capacity,
    #[error("run store stayed contended past its busy timeout")]
    Contention,
    #[error("run-store call was cancelled")]
    Cancelled,
    #[error("publication is visible but not durably synced")]
    Publication(#[source] provenant_os::fs::FsError),
    #[error(transparent)]
    Filesystem(#[from] provenant_os::fs::FsError),
    #[error("run database operation failed")]
    Sqlite(rusqlite::Error),
    #[error(transparent)]
    Decode(#[from] provenant_records::DecodeError),
    #[error(transparent)]
    Canonical(#[from] provenant_records::CanonicalError),
    #[error("run filesystem I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("run I/O operation {operation:?} failed at {path:?}: {source}")]
    OperationIo {
        operation: &'static str,
        path: Option<std::path::PathBuf>,
        #[source]
        source: std::io::Error,
    },
    #[error("run record path is invalid")]
    Path(#[from] provenant_records::PathWireError),
}
impl From<provenant_confinement::worktrees::WorktreeError> for RunError {
    fn from(error: provenant_confinement::worktrees::WorktreeError) -> Self {
        use provenant_confinement::repo::RepoError;
        use provenant_confinement::worktrees::WorktreeError;
        match error {
            WorktreeError::Repository(RepoError::ForeignProject {
                worktree,
                project,
                selected,
            }) => Self::WriterProjectMismatch {
                worktree,
                project,
                selected,
            },
            other => Self::Writer(other),
        }
    }
}
pub type Result<T> = std::result::Result<T, RunError>;
impl RunError {
    /// A generic code can wrap multiple guards; classify the typed cause rather
    /// than assigning an unrelated hard boundary to every confinement error.
    pub fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass as B;
        match self {
            Self::Confinement(error) => error.boundary_class(),
            Self::Plan(provenant_confinement::PlanError::Permission(error)) => {
                error.boundary_class()
            }
            Self::Plan(provenant_confinement::PlanError::UnconfinedExtensions) => B::Authority,
            Self::Plan(_) => B::None,
            _ => ErrorKind::from(self).boundary_class(),
        }
    }

    /// Select safe facts at their owner, before an adapter formats a refusal.
    pub fn diagnostic(
        &self,
        destination: provenant_records::diagnostic::Destination<'_>,
    ) -> provenant_records::diagnostic::Diagnostic {
        use provenant_confinement::{permissions::PermissionError, worktrees::WorktreeError};
        use provenant_records::diagnostic::{Diagnostic, DiagnosticFact, safe_path, shell_path};
        let kind = ErrorKind::from(self);
        let mut diagnostic = Diagnostic::new(kind.code()).category(kind.category());
        match self {
            Self::Confinement(PermissionError::CredentialGrant { path, access }) => {
                diagnostic = diagnostic
                    .detail(match safe_path(path, destination) {
                        Some(path) => format!("credential root refused for {access} grant: {path}"),
                        None => format!("credential root refused for {access} grant"),
                    })
                    .next(format!(
                        "inspect the requested {access} grant and select a permitted root"
                    ))
                    .fact(DiagnosticFact::CredentialGrant { access });
            }
            Self::WriterProjectMismatch {
                project, selected, ..
            } => {
                if let (Some(project), Some(selected)) = (
                    safe_path(project, destination),
                    safe_path(selected, destination),
                ) {
                    diagnostic = diagnostic.detail(format!(
                        "writer project {project} differs from selected project {selected}"
                    ));
                }
                diagnostic = diagnostic.next("select a worktree belonging to the chosen project");
            }
            Self::WorktreeUnregistered(path) => {
                if let Some(path) = safe_path(path, destination) {
                    diagnostic = diagnostic.detail(format!("worktree is not registered: {path}"));
                }
                diagnostic = diagnostic.fact(DiagnosticFact::WorktreeUnregistered).next(match shell_path(path, destination) {
                    Some(path) => format!("inspect custody, then select the checkout with --worktree {path}"),
                    None => "inspect worktree registration and select a verified checkout with --worktree".into(),
                });
            }
            Self::Writer(error) => {
                if let Some(action) = error.repair() {
                    diagnostic = diagnostic.next(action);
                }
                if let WorktreeError::UnknownWriterAt { path }
                | WorktreeError::CensusUnknown { path, .. } = error
                {
                    if let Some(path) = safe_path(path, destination) {
                        diagnostic =
                            diagnostic.detail(format!("writer custody is unknown for {path}"));
                    }
                    if let Some(path) = shell_path(path, destination) {
                        diagnostic = diagnostic.next(format!(
                            "inspect custody, then select the checkout with --worktree {path}"
                        ));
                    }
                }
                // Census evidence can name processes and host paths. The local
                // operator needs it to understand the uncertainty, but it is
                // not an approved fact for remote peers.
                if let WorktreeError::CensusUnknown { path, detail } = error
                    && matches!(
                        destination,
                        provenant_records::diagnostic::Destination::Local
                    )
                {
                    let scope = safe_path(path, destination)
                        .unwrap_or_else(|| "the requested worktree".into());
                    diagnostic = diagnostic
                        .detail(format!("writer occupancy is unknown for {scope}: {detail}"));
                }
                if let WorktreeError::BranchCollision { existing } = error {
                    diagnostic = diagnostic
                        .detail(format!("branch name collides with existing ref {existing}"));
                }
                if let WorktreeError::BranchExistsWithBase { branch, head, base } = error {
                    diagnostic = diagnostic
                        .detail(format!(
                            "branch {branch} at {head} does not contain base {base}"
                        ))
                        .next("omit --base to reuse the branch");
                }
                if let WorktreeError::WriterBusy {
                    holder: Some(holder),
                } = error
                {
                    diagnostic = diagnostic
                        .detail(format!("worktree writer is busy, held by attempt {holder}"));
                }
                if let WorktreeError::DetachedWorktree { head } = error {
                    diagnostic = diagnostic
                        .detail(format!("detached HEAD at {head}"))
                        .next("create a branch or remove --worktree and pass --mode read-only");
                }
                if matches!(error, WorktreeError::Dirty(_)) {
                    diagnostic = diagnostic.detail("the writer worktree has uncommitted edits");
                }
            }
            Self::Delivery(stage) => {
                diagnostic = diagnostic
                    .detail(format!("host writer delivery failed at {stage:?}"))
                    .fact(DiagnosticFact::DeliveryStage(*stage));
            }
            Self::DeliveryReconciliation { reason, next } => {
                diagnostic = diagnostic.detail(reason.clone()).next(next.clone()).fact(
                    DiagnosticFact::DeliveryStage(WriterDeliveryStage::PullRequest),
                );
            }
            Self::DeliveryLimit {
                stage,
                tool,
                command,
                limit,
                ..
            } => {
                if let Some(fact) =
                    provenant_records::diagnostic::DeliveryLimit::new(*stage, tool, command, *limit)
                {
                    diagnostic = diagnostic
                        .detail(fact.detail())
                        .fact(DiagnosticFact::DeliveryLimit(fact));
                }
            }
            Self::UnsafePath { path, check } => {
                if let Some(path) = safe_path(path, destination) {
                    diagnostic = diagnostic.detail(format!(
                        "run-store path {path} is not private or contained: {check}"
                    ));
                }
            }
            Self::Sqlite(error)
                if matches!(
                    destination,
                    provenant_records::diagnostic::Destination::Local
                ) =>
            {
                diagnostic = diagnostic.detail(format!("run store: {error}"));
            }
            Self::GenerationConflict { current_generation } => {
                diagnostic =
                    diagnostic.fact(DiagnosticFact::GenerationConflict(*current_generation));
            }
            Self::OperationIo { operation, .. } => {
                diagnostic = diagnostic.fact(DiagnosticFact::Filesystem { operation });
            }
            Self::Filesystem(error) | Self::Publication(error) => {
                use provenant_os::fs::FsError;
                let operation = match error {
                    FsError::Io { operation, .. } => *operation,
                    FsError::ReadIo { .. } => "read file",
                    FsError::Rename { .. } => "rename",
                    FsError::Symlink { .. } => "resolve path",
                    FsError::CrossDevice { .. } => "verify filesystem device",
                    FsError::LockTimeout { .. } | FsError::Cancelled { .. } => "acquire lock",
                    FsError::LockDeadlineOverflow { .. } => "set lock deadline",
                    _ => "verify filesystem entry",
                };
                diagnostic = diagnostic.fact(DiagnosticFact::Filesystem { operation });
            }
            Self::Admission(AdmissionError::Disk { .. }) => {
                diagnostic = diagnostic.next("free disk space above gc.free_floor before retrying")
            }
            Self::Admission(AdmissionError::MemoryPressure { .. }) => {
                diagnostic = diagnostic.next("reduce host memory pressure before retrying")
            }
            Self::Admission(AdmissionError::Processes { .. }) => {
                diagnostic = diagnostic.next("reduce host process use before retrying")
            }
            Self::Admission(AdmissionError::LaneCap { .. }) => {
                diagnostic =
                    diagnostic.next("wait for an active lane to finish or adjust the lane limit")
            }
            Self::SetupLockTimeout { seconds } => {
                diagnostic = diagnostic
                    .detail(format!("provenant-lane-setup.lock was held for the {seconds}s wait"))
                    .next("retry after the current Git setup finishes, or increase domains.fabric.lanes.setup_lock_wait_seconds");
            }
            Self::RetainedWriterImport {
                run,
                attempt,
                worktree,
            } => {
                diagnostic = diagnostic
                    .detail(match safe_path(worktree, destination) {
                        Some(path) => {
                            format!("worktree {path}; retained run {run}, attempt {attempt}")
                        }
                        None => format!("retained run {run}, attempt {attempt}"),
                    })
                    .next(format!(
                        "to release the fence and recover the preserved private Git manually, run `provenant reconcile --abandon {run}/{attempt} --reason 'recover retained work manually'`; this stops automatic import"
                    ));
            }
            _ => {}
        }
        diagnostic
    }

    /// Refuse `path`, which is relative to the state root where the store owns it
    /// and as given for a project or worktree path outside the state root.
    pub(crate) fn unsafe_path(path: impl Into<std::path::PathBuf>, check: PathCheck) -> Self {
        let path = path.into();
        Self::UnsafePath {
            path: if path.as_os_str().is_empty() {
                ".".into()
            } else {
                path
            },
            check,
        }
    }
    /// Refuse the entry `name` of a held directory, named relative to the state root.
    pub(crate) fn unsafe_entry(
        directory: &provenant_os::fs::Directory,
        name: &std::ffi::OsStr,
        check: PathCheck,
    ) -> Self {
        Self::unsafe_path(directory.anchored_path().join(name), check)
    }
    /// Refuse `artifact` because the comparison `check` failed. `expected` and
    /// `actual` are the recorded and observed values, when the check has them.
    pub(crate) fn artifact_mismatch(
        check: ArtifactCheck,
        artifact: impl Into<String>,
        expected: Option<ArtifactValue>,
        actual: Option<ArtifactValue>,
    ) -> Self {
        Self::ArtifactMismatch(ArtifactProvenance {
            check,
            artifact: artifact.into(),
            expected,
            actual,
        })
    }
    pub fn code(&self) -> &'static str {
        ErrorKind::from(self).code()
    }
}
#[cfg(test)]
mod delivery_tests;
#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "fixture"))]
mod continuation_test_driver;

// The custom integration target reuses this crate root so its same-binary
// guardian can exercise private settlement fixtures without exported test APIs.
#[cfg(all(test, feature = "fixture"))]
#[allow(dead_code)]
fn main() {
    if let Err(error) = continuation_test_driver::main() {
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "writer continuation: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod diagnostic_projection_tests {
    use super::*;
    use provenant_records::diagnostic::{BoundaryClass, Destination};
    use std::path::{Path, PathBuf};

    #[test]
    fn raw_io_source_is_not_a_safe_fact() {
        let error = RunError::Io(std::io::Error::other("TOKEN_SENTINEL\u{1b}[31m"));
        let projection = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        let encoded = serde_json::to_string(&projection).unwrap();
        assert!(!encoded.contains("TOKEN_SENTINEL"));
        assert_eq!(projection.code, "IO");
        assert!(projection.detail.is_none());
    }

    #[test]
    fn filesystem_fact_keeps_operation_without_source() {
        let error = RunError::Filesystem(provenant_os::fs::FsError::ReadIo {
            path: "/private/host".into(),
            source: std::io::Error::other("TOKEN_SENTINEL"),
        });
        let projection = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        assert!(matches!(
            projection.fact,
            Some(provenant_records::diagnostic::DiagnosticFact::Filesystem {
                operation: "read file"
            })
        ));
        assert!(
            !serde_json::to_string(&projection)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
    }

    #[test]
    fn project_mismatch_discloses_paths_only_at_safe_destinations() {
        let error = RunError::WriterProjectMismatch {
            worktree: "/foreign/tree".into(),
            project: "/foreign".into(),
            selected: "/selected".into(),
        };
        let local = error.diagnostic(Destination::Local);
        let detail = local.detail.unwrap();
        assert!(detail.contains("/foreign"));
        assert!(detail.contains("/selected"));
        let remote = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        assert!(remote.detail.is_none());
        assert!(remote.next.is_some());
    }

    #[test]
    fn credential_grants_disclose_only_safe_anchored_paths() {
        for access in ["read", "write"] {
            let error = RunError::Confinement(
                provenant_confinement::permissions::PermissionError::CredentialGrant {
                    path: PathBuf::from("/home/person/.config/private"),
                    access,
                },
            );
            let remote = error.diagnostic(Destination::Remote {
                repository: None,
                home: Some(Path::new("/home/person")),
            });
            assert_eq!(
                ErrorKind::from(&error).boundary_class(),
                BoundaryClass::Credentials
            );
            assert!(
                remote
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("~/.config/private")
            );
            assert!(remote.detail.as_deref().unwrap().contains(access));
            let opaque = error.diagnostic(Destination::Remote {
                repository: None,
                home: None,
            });
            assert!(!opaque.detail.as_deref().unwrap().contains("/home/person"));
            assert!(opaque.next.is_some());
        }
    }

    #[test]
    fn unknown_writer_action_quotes_a_local_worktree_path() {
        let error = RunError::Writer(
            provenant_confinement::worktrees::WorktreeError::UnknownWriterAt {
                path: PathBuf::from("/checkout/has a'quote"),
            },
        );
        let local = error.diagnostic(Destination::Local);
        assert_eq!(local.code, "WRITER_UNKNOWN");
        assert!(
            local
                .next
                .as_deref()
                .unwrap()
                .contains("--worktree '/checkout/has a'\\''quote'")
        );
        let remote = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        assert!(
            !serde_json::to_string(&remote)
                .unwrap()
                .contains("/checkout")
        );
    }

    #[test]
    fn unknown_census_diagnostic_names_local_scope_and_evidence() {
        let error = RunError::Writer(
            provenant_confinement::worktrees::WorktreeError::CensusUnknown {
                path: PathBuf::from("/checkout/tree"),
                detail: "process 123: cwd unreadable at /private/other".into(),
            },
        );
        let local = error.diagnostic(Destination::Local);
        let detail = local.detail.unwrap();
        assert!(detail.contains("/checkout/tree"));
        assert!(detail.contains("process 123: cwd unreadable at /private/other"));
        let remote = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        let rendered = serde_json::to_string(&remote).unwrap();
        assert!(!rendered.contains("/private/other"));
        assert!(!rendered.contains("/checkout/tree"));
    }
}
