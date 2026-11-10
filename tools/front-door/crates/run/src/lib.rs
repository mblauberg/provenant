//! Durable execution records and bounded, project-scoped readers.
#![forbid(unsafe_code)]
mod admission;
mod artifacts;
mod automatic;
mod board;
mod completion;
pub use board::{BoardLane, BoardPage};
pub use completion::{handoff_progress_guidance, lane_preamble, sanitise_handoff};
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
mod telemetry_context;
pub use telemetry_context::set_build_identity;
mod limits;
pub use limits::lane_limits;
pub mod token_budget;
mod transcript;
pub use service::{DispatchRequest, DispatchTask, ErrorKind, RefusalContext, Service};
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
    GitContext, continuation_history, ensure_local_branch, ensure_recorded_local_branch,
    resolve_git_base, resolve_git_base_with_facts, review_merge_base, review_merge_base_between,
};
pub use holder::{
    HolderContext, HostLaunchContext, RetainedGit, RootSeed, RunHolder, SealedTask, retained_git,
};
pub use launch::{
    Decoded, LaunchPaths, PrepareRefusal, PreparedLaunch, PreparedStart, Providers, RefusalKind,
    Started, WriteGrantLimit, access_root_allowed, capture_plan, concurrency,
};
pub use overview::{OverviewAttempt, OverviewState, RunOverview};
pub use plan::{
    isolated_read_only_plan, live_read_only_plan, prepare_read_only_git, read_only_plan,
};
pub use retention::{
    AttemptLogRemoval, AttemptTmpRemoval, PayloadEviction, PayloadReport, ProviderStateGc,
    ProviderStateIssue, TOMBSTONE, payload_totals,
};
pub use writer::{
    LaunchConfinement, WriterFence, WriterLaunch, equivalent_writer, prepare_writer,
    prepare_writer_with_facts, resolve_reader_source, resolve_writer, resolve_writer_proposal,
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
    #[error("{}", .0.detail.as_deref().unwrap_or("dispatch was refused"))]
    RecordedRefusal(Box<provenant_records::diagnostic::Diagnostic>),
    #[error("continuation preparation refused: {}", cause.detail())]
    ContinuationRefused {
        cause: provenant_records::run::TerminalCause,
    },
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
        "host writer delivery deferred at Integrate (parent_busy): the target branch is checked out; commit or preserve the parent's edits and explicitly merge child commit {child_head}, or detach the target checkout and retry delivery; the child commit is retained"
    )]
    IntegrationParentBusy { child_head: provenant_core::GitOid },
    #[error(
        "host writer delivery failed at Integrate (integration_conflict): merge preparation failed; inspect Git failures or resolve the target's conflict with retained child commit {child_head} in a separate checkout, then retry delivery"
    )]
    IntegrationConflict { child_head: provenant_core::GitOid },
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
    #[error("host writer delivery failed at {stage:?}: {source}")]
    DeliveryFact {
        stage: WriterDeliveryStage,
        source: provenant_confinement::worktrees::WorktreeError,
    },
    #[error("Git executable unavailable")]
    GitUnavailable,
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
    #[error("invalid run record: {0}")]
    WriterLineage(&'static str),
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
    #[error("read or record exceeds byte limit {limit}")]
    PromptTooLarge {
        task_index: Option<usize>,
        limit: u64,
    },
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
            Self::WriterHeadNotLaneBranch | Self::WriterSourceMismatch { .. } => B::LaneGit,
            Self::WriterLineage(_) => B::Authority,
            Self::Confinement(error) => error.boundary_class(),
            Self::Plan(error) => error.boundary_class(),
            _ => ErrorKind::from(self).boundary_class(),
        }
    }

    /// Select safe facts at their owner, before an adapter formats a refusal.
    pub fn diagnostic(
        &self,
        destination: provenant_records::diagnostic::Destination<'_>,
    ) -> provenant_records::diagnostic::Diagnostic {
        if let Self::RecordedRefusal(diagnostic) = self {
            return diagnostic.as_ref().clone();
        }
        use provenant_confinement::{permissions::PermissionError, worktrees::WorktreeError};
        use provenant_records::diagnostic::{Diagnostic, DiagnosticFact, safe_path, shell_path};
        let kind = ErrorKind::from(self);
        let mut diagnostic = Diagnostic::new(kind.code()).category(kind.category());
        match self {
            Self::ContinuationRefused { cause } => {
                diagnostic = diagnostic.detail(cause.detail());
            }
            Self::PromptTooLarge { task_index, limit } => {
                let field = task_index.map_or_else(
                    || "tasks".to_owned(),
                    |index| format!("tasks[{index}].prompt"),
                );
                diagnostic = diagnostic
                    .detail(format!(
                        "invalid input field `{field}`: exceeds {limit} bytes"
                    ))
                    .next("reduce the prompt input before retrying")
                    .fact(DiagnosticFact::PromptLimit {
                        task_index: *task_index,
                        limit: *limit,
                    });
            }
            Self::Confinement(PermissionError::CredentialGrant { path, access }) => {
                let access = match *access {
                    "read" => "read",
                    "write" => "write",
                    _ => "requested",
                };
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
            Self::Confinement(error) => {
                if let Some(fact) = error.resource_fact() {
                    diagnostic = fact
                        .project(diagnostic)
                        .fact(DiagnosticFact::Resource(fact));
                }
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
                | WorktreeError::CensusUnknown { path, .. }
                | WorktreeError::UnknownOccupancy { path, .. } = error
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
                if let WorktreeError::CensusUnknown { path, detail }
                | WorktreeError::UnknownOccupancy { path, detail } = error
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
                if matches!(error, WorktreeError::BranchCollision { .. }) {
                    diagnostic = diagnostic
                        .detail("branch name collides with an existing ref")
                        .next("inspect existing Git refs and choose a different branch name");
                }
                if let WorktreeError::BranchExistsWithBase { head, base, .. } = error {
                    diagnostic = diagnostic
                        .detail(format!("branch at {head} does not contain base {base}"))
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
                // An empty path list means the tree was not found edited, only
                // refused for another reason, so claim nothing about edits.
                if matches!(error, WorktreeError::Dirty(paths) if !paths.is_empty()) {
                    diagnostic = diagnostic.detail("the writer worktree has uncommitted edits");
                }
            }
            Self::Delivery(stage) => {
                diagnostic = diagnostic
                    .detail(format!("host writer delivery failed at {stage:?}"))
                    .fact(DiagnosticFact::DeliveryStage(*stage));
            }
            Self::DeliveryFact { stage, source } => {
                use provenant_confinement::utility::UtilityError;
                let detail = match source {
                    WorktreeError::Utility(UtilityError::Truncated { .. }) => {
                        "repository fact output exceeded the session cap"
                    }
                    WorktreeError::Utility(UtilityError::Cancelled) => {
                        "repository fact was cancelled"
                    }
                    WorktreeError::Utility(UtilityError::TimedOut) => "repository fact timed out",
                    _ => "repository fact output was incomplete",
                };
                diagnostic = diagnostic
                    .detail(format!(
                        "host writer delivery failed at {stage:?}: {detail}"
                    ))
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
            Self::GenerationConflict { current_generation } => {
                diagnostic =
                    diagnostic.fact(DiagnosticFact::GenerationConflict(*current_generation));
            }
            Self::Io(_) => {
                diagnostic = diagnostic.next("inspect the host I/O failure before retrying");
            }
            Self::OperationIo { operation, .. } => {
                diagnostic = diagnostic
                    .fact(DiagnosticFact::Filesystem { operation })
                    .next("inspect the host I/O failure before retrying");
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
            Self::Admission(error) => {
                if let Some(fact) = error.resource_fact() {
                    diagnostic = fact
                        .project(diagnostic)
                        .fact(DiagnosticFact::Resource(fact));
                }
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
        if diagnostic.next.is_none() {
            diagnostic = diagnostic
                .next("inspect the current state and request before taking further action");
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
mod launch_refusal_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod diagnostic_projection_tests {
    use super::*;
    use provenant_records::diagnostic::{BoundaryClass, Destination};
    use std::path::{Path, PathBuf};

    #[test]
    fn launch_guard_inventory_has_valid_classifications() {
        let inventory = include_str!("../assets/launch-guards.toml")
            .parse::<toml_edit::DocumentMut>()
            .expect("launch guard inventory must be valid TOML");
        assert_eq!(inventory["schema_version"].as_integer(), Some(1));
        let complete = inventory["complete"].as_bool().expect("completion flag");
        assert!(complete, "launch guard inventory must be fully classified");
        let guards = inventory["guard"].as_array_of_tables().expect("guard rows");
        assert!(!guards.is_empty());
        for guard in guards {
            let site = guard
                .get("site")
                .and_then(|v| v.as_str())
                .expect("guard site");
            assert!(site.contains('#'), "invalid guard site: {site}");
            let reason = guard
                .get("reason")
                .and_then(|v| v.as_str())
                .expect("guard reason");
            assert!(!reason.trim().is_empty(), "missing reason for {site}");
            let boundary = guard
                .get("boundary")
                .and_then(|v| v.as_str())
                .expect("guard classification");
            match boundary {
                "lane_git" | "credentials" | "object_integrity" | "authority" | "warning" => {}
                // Operational setup failures stop preparation without
                // claiming a security-boundary violation.
                "operational" => {}
                "configurable_default" => {
                    assert!(
                        guard
                            .get("setting")
                            .and_then(|v| v.as_str())
                            .is_some_and(|s| !s.trim().is_empty()),
                        "missing setting for {site}"
                    );
                    let code = guard
                        .get("code")
                        .and_then(|value| value.as_str())
                        .expect("configurable refusal code");
                    assert!(
                        ErrorKind::ALL.iter().any(|kind| kind.code() == code),
                        "unknown code {code} at {site}"
                    );
                }
                _ => panic!("invalid or incomplete classification {boundary} at {site}"),
            }
        }
        let guardian = RunError::Admission(AdmissionError::GuardianIdentity);
        assert_eq!(guardian.boundary_class(), BoundaryClass::Authority);
        let code = ErrorKind::from(&guardian).code();
        assert!(guards.iter().any(|guard| {
            guard.get("code").and_then(|value| value.as_str()) == Some(code)
                && guard.get("boundary").and_then(|value| value.as_str()) == Some("authority")
        }));
        let invalid_roots = RunError::Plan(provenant_confinement::PlanError::InvalidRoots);
        assert_eq!(invalid_roots.boundary_class(), BoundaryClass::Authority);
        assert_eq!(
            RunError::WriterHeadNotLaneBranch.boundary_class(),
            BoundaryClass::LaneGit
        );
        let lineage = RunError::WriterLineage("continuation worktree belongs to another lane");
        assert_eq!(lineage.boundary_class(), BoundaryClass::Authority);
        assert_eq!(ErrorKind::from(&lineage), ErrorKind::Invalid);
        assert_eq!(
            lineage.to_string(),
            "invalid run record: continuation worktree belongs to another lane"
        );
        assert_eq!(
            RefusalKind::HandoffBoundary(BoundaryClass::Authority).boundary_class(),
            BoundaryClass::Authority
        );
        assert!(guards.iter().any(|guard| {
            guard.get("site").and_then(|value| value.as_str())
                == Some("run/src/launch/hand_off.rs#RunStore::hand_off")
                && guard.get("boundary").and_then(|value| value.as_str()) == Some("authority")
        }));
    }

    #[test]
    fn every_inventoried_configurable_refusal_has_a_safe_setting_and_source() {
        use provenant_records::diagnostic::{
            LaneLimit, PacingReserveSetting, PacingSettingSource, PortSettingSource, PressureLevel,
            PressureThreshold, ResourceFact, SettingSource,
        };
        let configured = SettingSource::Configuration;
        let mut examples = vec![
            (
                ResourceFact::Disk {
                    free: 1,
                    floor: 2,
                    origin: configured,
                },
                "host configuration",
            ),
            (
                ResourceFact::ArgvPromptLimit {
                    limit: 2,
                    origin: configured,
                },
                "host configuration",
            ),
            (
                ResourceFact::WriteGrantLimit {
                    limit: 2,
                    origin: configured,
                },
                "host configuration",
            ),
            (
                ResourceFact::GrantLimit {
                    limit: 2,
                    origin: configured,
                },
                "host configuration",
            ),
            (
                ResourceFact::ProfileLimit {
                    bytes: 3,
                    limit: 2,
                    origin: configured,
                },
                "host configuration",
            ),
            (
                ResourceFact::LanePorts {
                    blocks: 2,
                    origin: PortSettingSource::Configuration,
                },
                "host configuration",
            ),
            (
                ResourceFact::MemoryPressure {
                    level: PressureLevel::Critical,
                    threshold: PressureThreshold::Warning,
                    origin: configured,
                },
                "host configuration",
            ),
            (
                ResourceFact::Processes {
                    used: 3,
                    limit: 4,
                    reserved: 1,
                },
                "operating system",
            ),
        ];
        for setting in [
            PacingReserveSetting::AdapterFiveHour,
            PacingReserveSetting::AdapterSevenDay,
            PacingReserveSetting::Account,
            PacingReserveSetting::Free,
        ] {
            examples.push((
                ResourceFact::PacingReserve {
                    percent: 15,
                    setting,
                    origin: PacingSettingSource::Configuration,
                },
                "host configuration",
            ));
        }
        for lane_limit in [
            LaneLimit::HostWriters,
            LaneLimit::ProjectWriters,
            LaneLimit::HostTotal,
            LaneLimit::ProjectTotal,
        ] {
            examples.push((
                ResourceFact::LaneCap {
                    active: 2,
                    limit: 2,
                    lane_limit,
                    origin: configured,
                },
                "host configuration",
            ));
        }
        for setting in [
            provenant_records::diagnostic::ContinuationCopyLimit::Bytes,
            provenant_records::diagnostic::ContinuationCopyLimit::Entries,
            provenant_records::diagnostic::ContinuationCopyLimit::Depth,
        ] {
            examples.push((
                ResourceFact::ContinuationCopy {
                    limit: 2,
                    setting,
                    origin: configured,
                    available_bytes: None,
                },
                "host configuration",
            ));
        }
        let inventory = include_str!("../assets/launch-guards.toml")
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        // Inverse coverage: deleting a row must fail even when every remaining
        // inventory row still has a valid projection.
        let guards = inventory["guard"].as_array_of_tables().unwrap();
        for (fact, source) in &examples {
            let diagnostic = RefusalKind::HandoffResource(*fact).diagnostic().unwrap();
            let detail = diagnostic.detail.as_deref().unwrap();
            assert!(detail.contains(source));
            assert!(
                guards.iter().any(|row| {
                    row.get("boundary").and_then(|v| v.as_str()) == Some("configurable_default")
                        && row.get("code").and_then(|v| v.as_str())
                            == Some(diagnostic.code.as_str())
                        && row
                            .get("setting")
                            .and_then(|v| v.as_str())
                            .is_some_and(|setting| detail.contains(setting))
                }),
                "resource refusal is omitted from inventory: {fact:?}"
            );
        }
        for row in inventory["guard"]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .filter(|row| {
                row.get("boundary").and_then(|v| v.as_str()) == Some("configurable_default")
            })
        {
            let site = row.get("site").and_then(|v| v.as_str()).unwrap();
            let setting = row.get("setting").and_then(|v| v.as_str()).unwrap();
            let code = row.get("code").and_then(|v| v.as_str()).unwrap();
            assert!(
                examples.iter().any(|(fact, source)| {
                    let Some(diagnostic) = RefusalKind::HandoffResource(*fact).diagnostic() else {
                        return false;
                    };
                    diagnostic.code == code
                        && diagnostic.detail.as_deref().is_some_and(|detail| {
                            detail.contains(setting) && detail.contains(source)
                        })
                        && diagnostic.next.is_some()
                }),
                "no safe resource projection for {site} ({code}, {setting})"
            );
        }
        assert!(guards.iter().any(|row| {
            row.get("site").and_then(|v| v.as_str())
                == Some("cli/src/run/provider/preparation.rs#dependency_preparation_event")
                && row.get("boundary").and_then(|v| v.as_str()) == Some("warning")
                && row
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .is_some_and(|reason| reason.contains("lanes.prepare_deps"))
        }));
    }

    #[test]
    fn confinement_resource_diagnostic_keeps_code_and_excludes_source_text() {
        use provenant_confinement::permissions::PermissionError;
        use provenant_records::diagnostic::{DiagnosticFact, ResourceFact, SettingSource};
        let error = RunError::Confinement(PermissionError::ConfigOrigin {
            source: Box::new(PermissionError::GrantLimitExceeded {
                family: "TOKEN_SENTINEL",
                limit: 17,
            }),
            origin: SettingSource::Configuration,
        });
        let diagnostic = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        assert_eq!(diagnostic.code, ErrorKind::Confinement.code());
        let detail = diagnostic.detail.unwrap();
        assert!(detail.contains("domains.run.grant_limit"));
        assert!(detail.contains("host configuration"));
        assert!(!detail.contains("TOKEN_SENTINEL"));
        assert!(matches!(
            diagnostic.fact,
            Some(DiagnosticFact::Resource(ResourceFact::GrantLimit {
                limit: 17,
                ..
            }))
        ));
    }

    #[test]
    fn resource_refusals_name_limits_and_corrective_actions() {
        let inventory = include_str!("../assets/launch-guards.toml")
            .parse::<toml_edit::DocumentMut>()
            .expect("launch guard inventory must be valid TOML");
        let guards = inventory["guard"].as_array_of_tables().unwrap();
        let cases = [
            (
                AdmissionError::Disk {
                    free: 1,
                    floor: 2,
                    origin: provenant_records::diagnostic::SettingSource::Configuration,
                },
                "gc.free_floor",
                "disk",
            ),
            (
                AdmissionError::MemoryPressure {
                    level: provenant_records::diagnostic::PressureLevel::Critical,
                    threshold: provenant_records::diagnostic::PressureThreshold::Warning,
                    origin: provenant_records::diagnostic::SettingSource::Configuration,
                },
                "memory_pressure_threshold",
                "memory",
            ),
            (
                AdmissionError::Processes { used: 3, limit: 4 },
                "operating system RLIMIT_NPROC",
                "process",
            ),
            (
                AdmissionError::LaneCap {
                    active: 5,
                    limit: 5,
                    kind: provenant_records::diagnostic::LaneLimit::ProjectWriters,
                    origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
                },
                "max_project_writer_lanes",
                "writer",
            ),
            (
                AdmissionError::LaneCap {
                    active: 5,
                    limit: 5,
                    kind: provenant_records::diagnostic::LaneLimit::HostWriters,
                    origin: provenant_records::diagnostic::SettingSource::Configuration,
                },
                "max_writer_lanes",
                "writer",
            ),
            (
                AdmissionError::LaneCap {
                    active: 5,
                    limit: 5,
                    kind: provenant_records::diagnostic::LaneLimit::HostTotal,
                    origin: provenant_records::diagnostic::SettingSource::Configuration,
                },
                "max_lanes",
                "lane",
            ),
            (
                AdmissionError::LaneCap {
                    active: 5,
                    limit: 5,
                    kind: provenant_records::diagnostic::LaneLimit::ProjectTotal,
                    origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
                },
                "max_project_lanes",
                "lane",
            ),
        ];
        for (cause, limit, corrective) in cases {
            let error = RunError::Admission(cause);
            let code = ErrorKind::from(&error).code();
            assert_eq!(error.boundary_class(), BoundaryClass::None);
            let guard = guards
                .iter()
                .find(|row| {
                    row.get("code").and_then(|v| v.as_str()) == Some(code)
                        && row
                            .get("setting")
                            .and_then(|v| v.as_str())
                            .is_some_and(|s| s.contains(limit))
                })
                .expect("resource refusal must be inventoried");
            assert_eq!(guard["boundary"].as_str(), Some("configurable_default"));
            let setting = guard["setting"].as_str().unwrap();
            let diagnostic = error.diagnostic(Destination::Remote {
                repository: None,
                home: None,
            });
            assert_eq!(diagnostic.code, code);
            assert!(diagnostic.detail.as_deref().unwrap().contains(limit));
            assert!(diagnostic.detail.as_deref().unwrap().contains(setting));
            assert!(
                diagnostic
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains(if code == "HOST_PROCESSES" {
                        "shipped"
                    } else {
                        "source:"
                    })
            );
            assert!(diagnostic.next.as_deref().unwrap().contains(corrective));
            assert!(
                !serde_json::to_string(&diagnostic)
                    .unwrap()
                    .contains("TOKEN_SENTINEL")
            );
        }
    }

    #[test]
    fn writer_refusal_without_named_edits_does_not_claim_edits() {
        use provenant_confinement::worktrees::WorktreeError;
        let error = RunError::Writer(WorktreeError::Dirty(String::new()));
        let diagnostic = error.diagnostic(Destination::Local);
        assert!(
            !diagnostic
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("uncommitted")
        );
    }

    #[test]
    fn dirty_writer_projection_preserves_edits_without_source_text() {
        use provenant_confinement::worktrees::WorktreeError;
        let error = RunError::Writer(WorktreeError::Dirty("TOKEN_SENTINEL\u{1b}".into()));
        let diagnostic = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        assert_eq!(diagnostic.code, "WRITER_SCOPE");
        assert!(
            diagnostic
                .detail
                .as_deref()
                .unwrap()
                .contains("uncommitted")
        );
        let action = diagnostic.next.as_deref().unwrap();
        assert!(action.contains("commit") || action.contains("preserve"));
        assert!(
            !serde_json::to_string(&diagnostic)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
    }

    #[test]
    fn raw_io_source_is_not_a_safe_fact() {
        for source in [
            "TOKEN_SENTINEL\u{1b}[31m".to_owned(),
            format!("TOKEN_SENTINEL{}", "é".repeat(2048)),
        ] {
            let error = RunError::Io(std::io::Error::other(source));
            for destination in [
                Destination::Local,
                Destination::Remote {
                    repository: None,
                    home: None,
                },
            ] {
                let projection = error.diagnostic(destination);
                let encoded = serde_json::to_string(&projection).unwrap();
                assert!(!encoded.contains("TOKEN_SENTINEL"));
                assert!(encoded.len() < 4096);
                assert!(!encoded.chars().any(char::is_control));
                assert_eq!(projection.code, "IO");
                assert!(projection.detail.is_none());
                assert!(projection.next.is_some());
            }
        }
    }

    #[test]
    fn branch_refusal_omits_untrusted_ref_text() {
        use provenant_confinement::worktrees::WorktreeError;
        let error = RunError::Writer(WorktreeError::BranchCollision {
            existing: "TOKEN_SENTINEL\u{1b}".into(),
        });
        for destination in [
            Destination::Local,
            Destination::Remote {
                repository: None,
                home: None,
            },
        ] {
            let diagnostic = error.diagnostic(destination);
            assert_eq!(diagnostic.code, "WRITER_SCOPE");
            assert!(diagnostic.detail.as_deref().unwrap().contains("collides"));
            assert!(diagnostic.next.as_deref().unwrap().contains("branch"));
            assert!(
                !serde_json::to_string(&diagnostic)
                    .unwrap()
                    .contains("TOKEN_SENTINEL")
            );
        }
    }

    #[test]
    fn source_errors_are_not_diagnostic_detail_even_locally() {
        let error = RunError::Sqlite(rusqlite::Error::InvalidParameterName(
            "TOKEN_SENTINEL".into(),
        ));
        let projection = error.diagnostic(Destination::Local);
        let encoded = serde_json::to_string(&projection).unwrap();
        assert!(!encoded.contains("TOKEN_SENTINEL"));
        assert!(projection.next.is_some());
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
    fn credential_grant_does_not_echo_unknown_access_text() {
        let error = RunError::Confinement(
            provenant_confinement::permissions::PermissionError::CredentialGrant {
                path: PathBuf::from("/private/root"),
                access: "TOKEN_SENTINEL",
            },
        );
        let diagnostic = error.diagnostic(Destination::Remote {
            repository: None,
            home: None,
        });
        assert_eq!(diagnostic.code, "CREDENTIAL_ROOT");
        assert!(diagnostic.next.is_some());
        let wire = serde_json::to_string(&diagnostic).unwrap();
        assert!(!wire.contains("TOKEN_SENTINEL"));
        assert!(!wire.contains("/private"));
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
    fn unknown_occupancy_diagnostics_name_local_scope_without_leaking_remote_evidence() {
        for cause in [
            provenant_confinement::worktrees::WorktreeError::CensusUnknown {
                path: PathBuf::from("/checkout/tree"),
                detail: "process 123: cwd unreadable at /private/other".into(),
            },
            provenant_confinement::worktrees::WorktreeError::UnknownOccupancy {
                path: PathBuf::from("/checkout/tree"),
                detail: "process 123: cwd unreadable at /private/other".into(),
            },
        ] {
            let error = RunError::Writer(cause);
            assert_eq!(ErrorKind::from(&error), ErrorKind::WriterUnknown);
            let local = error.diagnostic(Destination::Local);
            let detail = local.detail.unwrap();
            assert!(detail.contains("/checkout/tree"));
            assert!(detail.contains("process 123: cwd unreadable at /private/other"));
            let remote = error.diagnostic(Destination::Remote {
                repository: None,
                home: None,
            });
            let rendered = serde_json::to_string(&remote).unwrap();
            assert!(!rendered.contains("process 123"));
            assert!(!rendered.contains("/private/other"));
            assert!(!rendered.contains("/checkout/tree"));
        }
    }
}
