//! Durable execution records and bounded, project-scoped readers.
#![forbid(unsafe_code)]
mod admission;
mod artifacts;
mod board;
pub mod build;
mod completion;
pub use board::{BoardLane, BoardPage};
pub use completion::{
    BRIEF_PREAMBLE_KNOB, BriefLayer, BriefPreamble, BriefRole, BriefStaticCheck, BuildCap,
    LaneLocation, PreambleExtras, handoff_progress_guidance, lane_preamble, lane_preamble_with,
    route_label,
};
mod verification;
pub use verification::{VerificationResolution, conflict_cause};
mod controls;
pub use controls::release_progress;
mod correlation;
pub use correlation::{WorkCoverage, WorkSelector, validate_work};
mod db;
mod delivery;
mod disk;
mod git_context;
mod holder;
mod identity;
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
pub use limits::{HostCaps, gh_budget, lane_limits, writer_budget_limits};
mod ci_scope;
pub use ci_scope::{CiScope, ResolvedCiScope};
mod gh_usage;
pub mod token_budget;
mod transcript;
pub use service::{DispatchRequest, DispatchTask, ErrorKind, RefusalContext, Service};
pub use transcript::locate as locate_transcript;
pub mod build_dir_gc;
mod writer;
pub mod writer_dirty;

pub use admission::{
    AdmissionPolicy, AdmissionTimeoutOrigin, EffectiveAdmissionTimeout, HOST_ACTIVE, HOST_QUEUED,
    LaneWait, PROJECT_ACTIVE, Resources, SystemResources,
};
pub use controls::{
    AttemptPresence, CancelReceipt, Reconciled, Resume, SteerIntent, SteerReceipt, TemporaryReason,
    TemporaryReasonKind, UncertainReason, attempt_presence, attempt_presence_with_census,
    attempt_presence_with_unavailable_census, holder_absent, overdue_at,
};
#[cfg(any(test, feature = "fixture"))]
pub use delivery::forge_for;
pub use delivery::{FilteredLanePath, ForgeOnce, HostDeliveryContext, resolve_forge};
pub use git_context::{
    GitContext, ReviewDiffContext, continuation_history, ensure_local_branch,
    ensure_recorded_local_branch, resolve_git_base, resolve_git_base_with_facts,
    resolve_review_commit, review_diff_context_at, review_merge_base, review_merge_base_between,
};
pub use holder::{
    HolderContext, HostLaunchContext, RetainedGit, RootSeed, RunHolder, SealedTask, retained_git,
    writer_base,
};
pub use identity::ProviderIdentityEvidence;
#[cfg(test)]
pub(crate) use launch::fixture_refusal;
pub use launch::{
    ContinuationDecodeContext, ContinuationExpectation, Decoded, LaunchPaths, PrepareRefusal,
    PreparedLaunch, PreparedStart, Providers, RefusalKind, Started, WriteGrantLimit,
    access_root_allowed, capture_plan, concurrency, launch_policy,
};
pub use overview::{OverviewAttempt, OverviewState, RunOverview};
pub use plan::{
    isolated_read_only_plan, live_read_only_plan, prepare_read_only_git, read_only_plan,
};
pub use retention::{
    AttemptLogRemoval, AttemptTmpRemoval, PayloadEviction, PayloadReport, ProviderStateGc,
    ProviderStateIssue, RetentionScan, ScratchOwner, ScratchOwnerResolver, ScratchPolicy,
    TOMBSTONE, gc_import_pins, payload_totals,
};
pub use writer::{
    BoundSandboxBackend, LaunchConfinement, RepinOutcome, WriterFence, WriterLaunch,
    equivalent_writer, prepare_writer, prepare_writer_with_facts, resolve_reader_source,
    resolve_writer, resolve_writer_proposal, trusted_lane_toolchain_diagnostic,
};

pub use db::{ExecutionContext, Request, RunStore};
/// Current SQLite versions, shared with the installer's header-only reset check.
pub const RUN_SCHEMA_VERSION: u32 = db::SCHEMA.version as u32;
pub const ADMISSION_SCHEMA_VERSION: u32 = ledger::SCHEMA.version as u32;
pub const CONTRIBUTIONS_SCHEMA_VERSION: u32 = pr_contribution::SCHEMA.version as u32;
use provenant_records::custody::Chair;
pub use provenant_records::run::*;
pub use provenant_supervisor::custody::AdmissionError;
pub use reader::{
    ArtifactPart, AttemptReader, Attempts, ByteRange, LanePage, LaneQuery, OutputChunk, fact,
    observed_commands,
};
pub use store::{PreparedRun, TerminalPublication};
use thiserror::Error;

/// Return the depth of the run being dispatched under this lane. Depth above
/// `domains.run.max_lane_depth` warns and proceeds by default; the owner can
/// make it a stop with `policy.LANE_DEPTH=refuse`.
pub fn next_lane_depth(
    parent_depth: u64,
    max_depth: u64,
    policy: &provenant_records::policy::Policy,
) -> Result<u64> {
    let depth = parent_depth.checked_add(1).ok_or(RunError::DepthOverflow)?;
    if depth > max_depth {
        policy.warn_or(
            "LANE_DEPTH",
            provenant_records::policy::Facts::violation(format!(
                "nested lane depth {depth} exceeds domains.run.max_lane_depth {max_depth}; raise that setting or set policy.LANE_DEPTH=refuse to stop here"
            )),
            |_| RunError::DepthExceeded { depth, max_depth },
        )?;
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
            provenant_sqlite::VersionError::Refused { found, .. } => Self::Version(found),
            provenant_sqlite::VersionError::MigrationRequired { .. } => {
                Self::Validation("run store schema needs a writable open to migrate")
            }
            provenant_sqlite::VersionError::Sqlite(error) => error.into(),
            provenant_sqlite::VersionError::MissingBaseTable { .. } => {
                Self::Validation("STATE_RESET_REQUIRED: missing known base table")
            }
            provenant_sqlite::VersionError::MissingStep { .. } => {
                Self::Validation("missing schema step; a writable open applies it")
            }
            provenant_sqlite::VersionError::InvalidStep { .. } => {
                Self::Validation("invalid additive schema step declaration")
            }
            provenant_sqlite::VersionError::InvalidMigration { .. } => {
                Self::Validation("invalid data migration declaration")
            }
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
            TransportError::Unreadable { .. }
            | TransportError::UnreadablePorts { .. }
            | TransportError::LaneRoots { .. } => Self::Unreadable,
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

/// A source error whose fixed cause the run diagnostic may disclose. Its
/// text, which can name paths or Git output, stays in the error chain only.
#[derive(Debug)]
pub(crate) struct CausedSource {
    text: String,
    cause: &'static str,
}

impl CausedSource {
    /// An I/O source carrying `text`, which remembers `cause` when given.
    pub(crate) fn io(text: String, cause: Option<&'static str>) -> std::io::Error {
        match cause {
            Some(cause) => std::io::Error::other(Self { text, cause }),
            None => std::io::Error::other(text),
        }
    }

    fn cause(source: &std::io::Error) -> Option<&'static str> {
        source
            .get_ref()
            .and_then(|error| error.downcast_ref::<Self>())
            .map(|caused| caused.cause)
    }
}

impl std::fmt::Display for CausedSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl std::error::Error for CausedSource {}

/// The typed leaf cause of a failed host delivery. Its `Display` is
/// self-contained: `RunError::Delivery` renders `stage: cause` in one message,
/// so the cause names what actually failed without relying on a source chain.
#[derive(Debug)]
pub enum DeliveryCause {
    /// A host command exited or was signalled. `stderr` is a bounded tail of
    /// its standard error, passed through because diagnostic redaction is off
    /// by default.
    Command {
        termination: provenant_os::exec::Termination,
        timed_out: bool,
        stderr: String,
    },
    /// A value returned by the host could not be parsed or was unexpected.
    Parse { detail: String },
}

impl DeliveryCause {
    /// A parse or unexpected-value cause carrying `detail`.
    pub(crate) fn parse(detail: impl std::fmt::Display) -> Self {
        Self::Parse {
            detail: detail.to_string(),
        }
    }
}

impl std::fmt::Display for DeliveryCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command {
                termination,
                timed_out,
                stderr,
            } => {
                write!(
                    formatter,
                    "command exited {termination:?} (timed out: {timed_out})"
                )?;
                if !stderr.is_empty() {
                    write!(formatter, ": {stderr}")?;
                }
                Ok(())
            }
            Self::Parse { detail } => formatter.write_str(detail),
        }
    }
}

/// A declared stage in writer preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryPrepareStep {
    ForgePort,
    IntegrationTarget,
    RemoteUrl,
    Head,
    GitDir,
    Worktree,
    CredentialHelper,
    LaneRoots,
    Environment,
    GitExecutable,
    GhExecutable,
    Fetch,
    OriginRefresh,
}

impl DeliveryPrepareStep {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ForgePort => "forge port",
            Self::IntegrationTarget => "integration target",
            Self::RemoteUrl => "remote URL",
            Self::Head => "HEAD",
            Self::GitDir => "git dir",
            Self::Worktree => "worktree",
            Self::CredentialHelper => "credential helper",
            Self::LaneRoots => "lane roots",
            Self::Environment => "environment",
            Self::GitExecutable => "git executable",
            Self::GhExecutable => "gh executable",
            Self::Fetch => "fetch",
            Self::OriginRefresh => "origin refresh",
        }
    }
}

impl std::fmt::Display for DeliveryPrepareStep {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A typed safety refusal associated with a preparation step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryPrepareRefusal {
    CredentialHelper(HelperSafety),
}

/// Why a configured credential helper cannot be used safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperSafety {
    UnsupportedSshCommand,
    OpaqueShellCommand,
    NonPlainExecutable,
    LaneLocalPath,
    Unavailable,
}

fn format_conflicts(files: &[String]) -> String {
    if files.is_empty() {
        ": merge preparation failed; inspect Git failures or resolve the target's conflict with retained child commit in a separate checkout, then retry delivery".to_owned()
    } else {
        format!("; conflicting files: {}", files.join(", "))
    }
}

/// The facts of a publication refused because its tip descends from no
/// history the host knows. Boxed inside [`RunError`] so the cause does not
/// enlarge every CLI result that carries the error.
#[derive(Debug)]
pub struct LaneTipRefusal {
    pub tip: provenant_core::GitOid,
    pub base: provenant_core::GitOid,
    pub launch_ref: Option<String>,
    pub setting: String,
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("abandonment refused: {0}")]
    AbandonBlocked(Box<settlement::AbandonmentRefusal>),
    #[error("{}", .0.detail.as_deref().unwrap_or("dispatch was refused"))]
    RecordedRefusal(Box<provenant_records::diagnostic::Diagnostic>),
    #[error("continuation preparation refused: {}", refusal.safe_reason())]
    ContinuationRefused {
        refusal: Box<launch::PrepareRefusal>,
    },
    #[error("{0}")]
    InvalidTaskTitle(String),
    #[error("{message}")]
    Input { kind: ErrorKind, message: String },
    /// The stage and its typed leaf cause are rendered together, so a printer
    /// that only formats the error still learns whether the failure was a
    /// command exit with its stderr, an I/O error or a parse error.
    #[error("host writer delivery failed at {stage:?}: {cause}")]
    Delivery {
        stage: WriterDeliveryStage,
        cause: DeliveryCause,
    },
    /// A host command run for a named config read failed. `reason` is the
    /// bounded leaf cause. The stage is reported by the receipt's delivery
    /// fact, and the file/key by the named config error that wraps this, so
    /// `Display` renders only the cause — printing the stage again here would
    /// repeat it in a transport refusal or a failed receipt.
    #[error("{reason}")]
    ConfigCommandFailed { reason: String },
    /// A host command run for a named config read failed. `Display` names the
    /// stage and the config error's own context and bounded cause in one
    /// message, so a printer that only formats the error cannot drop either.
    /// The stage is also reported as a delivery fact by the diagnostic.
    #[error("host writer delivery failed at {stage:?}: {cause}")]
    DeliveryConfigRead {
        stage: WriterDeliveryStage,
        cause: Box<provenant_os::git_config::ConfigError<RunError>>,
    },
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
        "host writer delivery failed at Integrate (integration_conflict): merge conflict with child commit {child_head}{}",
        format_conflicts(.conflicting_files)
    )]
    IntegrationConflict {
        child_head: provenant_core::GitOid,
        conflicting_files: Vec<String>,
    },
    #[error("merge conflict in git merge-tree; conflicting files: {}", .conflicting_files.join(", "))]
    MergeConflict { conflicting_files: Vec<String> },
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
    #[error("host writer delivery failed at {stage:?} (transport): {reason}")]
    DeliveryTransport {
        stage: WriterDeliveryStage,
        kind: TransportRefusal,
        reason: String,
    },
    #[error("Git transport refused: {reason}")]
    GitTransport {
        kind: TransportRefusal,
        reason: String,
    },
    #[error("host writer delivery failed at Prepare ({step}): {reason}")]
    DeliveryPrepare {
        step: DeliveryPrepareStep,
        refusal: Option<DeliveryPrepareRefusal>,
        reason: String,
    },
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
    #[error(
        "live provider session or process identity changed, or the terminal session disagrees; accepted steering input stays recorded and no fresh session was started"
    )]
    SteerSessionMismatch,
    #[error(
        "provider did not retain a resumable session for the queued steering input; accepted text stays recorded and no fresh session was started"
    )]
    SteerSessionUnavailable,
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
    #[error("writer import retained for worktree {}, lane {} (attempt {}), fence {}; run `{}` or `{}`", worktree.display(), fence.lane, fence.scope.attempt, fence.fence.display(), fence.reconcile(), fence.abandon())]
    RetainedWriterImport {
        worktree: std::path::PathBuf,
        fence: Box<provenant_supervisor::custody::PendingImport>,
    },
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error("invalid run record: attempt reservation retained")]
    AttemptReservationRetained,
    #[error("invalid run record: {0}")]
    Validation(&'static str),
    #[error("a sealed delivery bound to remote {0:?} is invalid; its record is corrupt")]
    InvalidSealedRemote(String),
    #[error(transparent)]
    Handoff(#[from] provenant_records::handoff::HandoffError),
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
    /// The lane's history forked from the host branch and the configured
    /// history policy retained it. Its commits stay in the attempt's private
    /// Git; the setting that would publish a host merge names itself here.
    #[error(
        "lane history diverged from the host branch; its commits are retained in the attempt's private Git for host recovery; {setting}; set it to allow to publish a host merge"
    )]
    PublicationDiverged { setting: String },
    /// A host merge of a diverged lane tip conflicted. The lane's commits stay
    /// retained; the conflicting files and the setting are named.
    #[error(
        "lane history diverged from the host branch and a host merge conflicts in {}; its commits are retained in the attempt's private Git for host recovery; {setting}",
        .conflicting_files.join(", ")
    )]
    PublicationConflict {
        conflicting_files: Vec<String>,
        setting: String,
    },
    /// The lane tip and the host branch share no commit, so a host merge would
    /// have to invent a common ancestor. The lane's commits stay retained.
    #[error(
        "lane history shares no commit with the host branch, so a host merge would invent a common ancestor; its commits are retained in the attempt's private Git for host recovery; push the branch the lane was rebased onto, or set it to refuse to retain the lane's private history without attempting a merge; {setting}"
    )]
    PublicationUnrelated { setting: String },
    /// The host repository configures a merge driver, which a host merge would
    /// run under the lane's attributes.
    #[error(
        "the host repository configures Git merge drivers, which a host merge would run under the lane's attributes; its commits are retained in the attempt's private Git for host recovery; remove the merge.*.driver configuration to publish, or set it to refuse the merge instead; {setting}"
    )]
    PublicationMergeDriver { setting: String },
    /// The host branch was rewound behind the pinned launch base while the lane
    /// head was not already contained in it.
    #[error(
        "the host branch was rewound to {branch_tip}, behind its pinned launch base {base}; publishing would reinstate the history the host removed, so its commits are retained in the attempt's private Git for host recovery; move the branch forward to {base} to publish the lane"
    )]
    PublicationRewound {
        branch_tip: provenant_core::GitOid,
        base: provenant_core::GitOid,
    },
    #[error(
        "lane tip {} descends from none of: its launch base {}, the origin default branch, {}, any pushed remote-tracking branch, or the host lane branch; {}; set it to warn or off to publish, or push the branch it was rebased onto",
        .0.tip,
        .0.base,
        .0.launch_ref.as_deref().map_or_else(|| "its launch ref".to_owned(), |name| format!("its launch ref {name}")),
        .0.setting
    )]
    LaneTipUnrooted(Box<LaneTipRefusal>),
    #[error("invalid Git base {0:?}: expected a resolvable commit reference")]
    InvalidBase(String),
    #[error(
        "STATE_RESET_REQUIRED: schema version {0} is not supported; move the store aside to start it fresh"
    )]
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
    PromptTooLarge { limit: u64 },
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
    /// Another holder kept a writer worktree's exclusion lock past the bounded
    /// wait. Settlement retries; contention is never a recovery failure.
    #[error("waiting for writer lock {}, held by {holder}", lock.display())]
    WriterExclusionBusy {
        lock: std::path::PathBuf,
        holder: String,
        /// Some holder may still release the lock: a live or unobservable
        /// guardian, a live or unreadable settlement owner of another
        /// reserved attempt, or a holder no reservation identifies.
        holder_live: bool,
    },
    /// A steered writer's successor waits until settlement releases the
    /// predecessor's worktree custody; the accepted steering stays queued.
    #[error(
        "steer continuation waits for the steered writer's settlement to release its worktree custody"
    )]
    SteerAwaitingCustody,
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
    #[error("stored record {} (record type {record_type}): {source}", path.display())]
    StoredRecordRead {
        path: std::path::PathBuf,
        record_type: String,
        #[source]
        source: Box<RunError>,
    },
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
impl From<provenant_config::ForgeContextError> for RunError {
    fn from(error: provenant_config::ForgeContextError) -> Self {
        Self::Input {
            kind: ErrorKind::Config,
            message: error.to_string(),
        }
    }
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
    /// A failed delivery at `stage` with its typed leaf cause.
    pub(crate) fn delivery(stage: WriterDeliveryStage, cause: DeliveryCause) -> Self {
        Self::Delivery { stage, cause }
    }

    pub(crate) fn with_record_context(
        self,
        path: impl Into<std::path::PathBuf>,
        record_type: impl Into<String>,
    ) -> Self {
        let path = path.into();
        let record_type = record_type.into();
        match self {
            Self::Decode(error) => Self::Decode(error.with_context(path, record_type)),
            source => Self::StoredRecordRead {
                path,
                record_type,
                source: Box::new(source),
            },
        }
    }

    /// A refusal whose inputs do not change on retry. Settlement seals it as a
    /// failed, non-retryable import instead of looping on it. Every publication
    /// refusal is final in this sense: only explicit host recovery changes the
    /// branch state it judged, so a retry would only keep the worktree fenced.
    pub(crate) fn import_refusal_is_final(&self) -> bool {
        self.is_publication_refusal() || matches!(self, Self::ObjectImportLimit(_))
    }

    /// A publication the host refuses on the lane's own history and the host
    /// branch it observed. Retrying the same tip and branch cannot change it
    /// without explicit host recovery, so settlement retains the attempt's
    /// private Git and releases custody instead of retrying.
    pub(crate) fn is_publication_refusal(&self) -> bool {
        matches!(
            self,
            Self::LaneTipUnrooted(..)
                | Self::PublicationDiverged { .. }
                | Self::PublicationConflict { .. }
                | Self::PublicationUnrelated { .. }
                | Self::PublicationMergeDriver { .. }
                | Self::PublicationRewound { .. }
        )
    }

    /// Writer-lock contention outlived its bounded wait. Settlement records it
    /// as a wait naming the holder, never as a recovery failure.
    pub(crate) fn is_writer_contention(&self) -> bool {
        matches!(self, Self::WriterExclusionBusy { .. })
    }

    /// A generic code can wrap multiple guards; classify the typed cause rather
    /// than assigning an unrelated hard boundary to every confinement error.
    pub fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass as B;
        match self {
            Self::WriterHeadNotLaneBranch | Self::WriterSourceMismatch { .. } => B::LaneGit,
            Self::WriterLineage(_) => B::Authority,
            Self::DeliveryPrepare {
                refusal: Some(DeliveryPrepareRefusal::CredentialHelper(safety)),
                ..
            } if *safety != HelperSafety::Unavailable => B::Credentials,
            Self::Confinement(error) => error.boundary_class(),
            Self::Plan(error) => error.boundary_class(),
            _ => ErrorKind::from(self).boundary_class(),
        }
    }

    /// Select safe facts at their owner, before an adapter formats a refusal.
    pub fn diagnostic(
        &self,
        destination: provenant_records::diagnostic::Destination,
    ) -> provenant_records::diagnostic::Diagnostic {
        if let Self::StoredRecordRead { source, .. } = self {
            return source.diagnostic(destination).detail(self.to_string());
        }
        if let Self::RecordedRefusal(diagnostic) = self {
            return diagnostic.as_ref().clone();
        }
        use provenant_confinement::{permissions::PermissionError, worktrees::WorktreeError};
        use provenant_records::diagnostic::{Diagnostic, DiagnosticFact, shell_path};
        let kind = ErrorKind::from(self);
        let mut diagnostic = Diagnostic::new(kind.code()).category(kind.category());
        match self {
            Self::Decode(error) => {
                diagnostic = diagnostic.detail(error.to_string()).next(
                    if kind == ErrorKind::UpgradeRequired {
                        "upgrade Provenant to read the named stored record"
                    } else {
                        "inspect the named stored record before retrying reconciliation"
                    },
                );
            }
            Self::AbandonBlocked(evidence) => {
                diagnostic = diagnostic.detail(evidence.to_string());
            }
            Self::ContinuationRefused { refusal } => {
                diagnostic = diagnostic.detail(refusal.safe_reason());
            }
            Self::PromptTooLarge { limit } => {
                diagnostic = diagnostic
                    .detail(format!(
                        "invalid input field `prompt`: exceeds {limit} bytes"
                    ))
                    .next("reduce the prompt input before retrying")
                    .fact(DiagnosticFact::PromptLimit { limit: *limit });
            }
            Self::Input { kind: _, message } => {
                diagnostic = diagnostic.detail(provenant_core::bounded_line(message, 4096));
            }
            Self::Confinement(PermissionError::CredentialGrant { path, access }) => {
                let access = match *access {
                    "read" => "read",
                    "write" => "write",
                    _ => "requested",
                };
                diagnostic = diagnostic
                    .detail(format!(
                        "credential root refused for {access} grant: {}",
                        path.display()
                    ))
                    .next(format!(
                        "inspect the requested {access} grant and select a permitted root"
                    ))
                    .fact(DiagnosticFact::CredentialGrant { access });
            }
            Self::Confinement(PermissionError::CredentialStore {
                path,
                root,
                declaration_source,
            }) => {
                let overlap = format!(
                    "credential grant at {} overlaps {}",
                    path.display(),
                    root.display()
                );
                diagnostic = diagnostic
                    .detail(format!("{overlap} (source: {declaration_source})"))
                    .next("select a root clear of the declared credential store");
            }
            Self::Confinement(error) => {
                if let Some(fact) = error.resource_fact() {
                    diagnostic = fact
                        .project(diagnostic)
                        .fact(DiagnosticFact::Resource(fact));
                }
            }
            Self::WriterProjectMismatch {
                worktree,
                project,
                selected,
            } => {
                diagnostic = diagnostic.detail(format!(
                    "worktree {} belongs to writer project {} rather than selected project {}",
                    worktree.display(),
                    project.display(),
                    selected.display()
                ));
                diagnostic = diagnostic.next("select a worktree belonging to the chosen project");
            }
            Self::WorktreeUnregistered(path) => {
                diagnostic =
                    diagnostic.detail(format!("worktree is not registered: {}", path.display()));
                diagnostic = diagnostic.fact(DiagnosticFact::WorktreeUnregistered).next(match shell_path(path, destination) {
                    Some(path) => format!("inspect custody, then select the checkout with --worktree {path}"),
                    None => "inspect worktree registration and select a verified checkout with --worktree".into(),
                });
            }
            Self::Writer(WorktreeError::GitConfigRead(error)) => {
                diagnostic = diagnostic.detail(error.to_string());
            }
            Self::Writer(error) => {
                if let Some(action) = error.repair() {
                    diagnostic = diagnostic.next(action);
                }
                if let WorktreeError::ManagedPathMismatch {
                    requested,
                    expected,
                } = error
                {
                    let detail = format!(
                        "requested writer path {} does not match configured project path {}",
                        requested.display(),
                        expected.display()
                    );
                    diagnostic = diagnostic.detail(detail);
                }
                if let WorktreeError::UnknownWriterAt { path }
                | WorktreeError::CensusUnknown { path, .. }
                | WorktreeError::UnknownOccupancy { path, .. } = error
                {
                    diagnostic = diagnostic
                        .detail(format!("writer custody is unknown for {}", path.display()));
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
                    let scope = path.display();
                    diagnostic = diagnostic
                        .detail(format!("writer occupancy is unknown for {scope}: {detail}"));
                }
                if let WorktreeError::BranchCollision { existing } = error {
                    // A ref Git listed may be named, but only once it is free of
                    // display-control characters. The same shared projection
                    // keeps every named fact bounded and escaped.
                    // Spec: docs/specs/run-git.md#worktrees-and-refs
                    let detail = if existing
                        .chars()
                        .any(provenant_records::diagnostic::unsafe_display_character)
                    {
                        "branch name collides with an existing ref".to_owned()
                    } else {
                        format!(
                            "branch name collides with existing ref {}",
                            provenant_records::diagnostic::safe_text(existing)
                        )
                    };
                    diagnostic = diagnostic
                        .detail(detail)
                        .next("inspect the named ref and choose a different branch name");
                }
                if let WorktreeError::WriterLeaseHeld { .. } = error {
                    diagnostic = diagnostic
                        .detail(error.to_string())
                        .next("reconcile the holding attempt before retrying writer binding");
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
            Self::Delivery { stage, .. } => {
                diagnostic = diagnostic
                    .detail(crate::delivery::prepare_reason(self))
                    .fact(DiagnosticFact::DeliveryStage(*stage));
            }
            Self::ConfigCommandFailed { reason } => {
                diagnostic = diagnostic.detail(reason.clone());
            }
            Self::DeliveryConfigRead { stage, cause } => {
                diagnostic = diagnostic
                    .detail(crate::delivery::prepare_reason(cause))
                    .fact(DiagnosticFact::DeliveryStage(*stage));
            }
            Self::DeliveryTransport { stage, reason, .. } => {
                diagnostic = diagnostic
                    .detail(format!(
                        "host writer delivery failed at {stage:?} (transport): {reason}"
                    ))
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
                diagnostic = diagnostic.detail(format!(
                    "run-store path {} is not private or contained: {check}",
                    path.display()
                ));
            }
            Self::GenerationConflict { current_generation } => {
                diagnostic =
                    diagnostic.fact(DiagnosticFact::GenerationConflict(*current_generation));
            }
            Self::Io(_) => {
                diagnostic = diagnostic.next("inspect the host I/O failure before retrying");
            }
            Self::OperationIo {
                operation, source, ..
            } => {
                diagnostic = diagnostic
                    .fact(DiagnosticFact::Filesystem {
                        operation,
                        cause: CausedSource::cause(source),
                    })
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
                diagnostic = diagnostic.fact(DiagnosticFact::Filesystem {
                    operation,
                    cause: None,
                });
            }
            Self::Admission(error) => {
                if let Some(fact) = error.resource_fact() {
                    diagnostic = fact
                        .project(diagnostic)
                        .fact(DiagnosticFact::Resource(fact));
                }
            }
            Self::RetainedWriterImport { fence, worktree } => {
                let path = fence.fence.to_string_lossy().into_owned();
                diagnostic.custody = Some(provenant_records::diagnostic::RetainedCustody {
                    scope: fence.scope,
                    fence: Some(path.clone()),
                    lane: fence.lane.clone(),
                    reconcile: fence.reconcile(),
                    abandon: fence.abandon(),
                });
                diagnostic = diagnostic
                    .detail(format!(
                        "lane {}; retained attempt {}; fence {}; worktree {}",
                        fence.lane,
                        fence.scope.attempt,
                        path,
                        worktree.display()
                    ))
                    .next(format!(
                        "run `{}` to resume import, or `{}` to release custody and recover the preserved private Git manually",
                        fence.reconcile(), fence.abandon()
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
mod remoteless_delivery_tests;
#[cfg(test)]
mod remoteless_writer_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod diagnostic_projection_tests {
    use super::*;
    use provenant_records::diagnostic::{BoundaryClass, Destination};
    use std::path::PathBuf;

    #[test]
    fn writer_config_read_diagnostic_keeps_context_and_leaf_cause() {
        let error = RunError::Writer(
            provenant_confinement::worktrees::WorktreeError::GitConfigRead(Box::new(
                provenant_os::git_config::ConfigError {
                    context: provenant_os::git_config::ConfigContext::new(
                        "read local core.bare",
                        "/p/.git/config",
                        Some("core.bare"),
                    ),
                    source: provenant_confinement::utility::UtilityError::TimedOut,
                },
            )),
        );
        let diagnostic = error.diagnostic(Destination::Local);
        let detail = diagnostic.detail.unwrap();
        assert!(
            detail.contains("read local core.bare on /p/.git/config for key core.bare"),
            "{detail}"
        );
        assert!(detail.contains("utility command timed out"), "{detail}");
    }

    #[test]
    fn delivery_config_read_display_names_stage_context_and_cause_once() {
        let error = RunError::DeliveryConfigRead {
            stage: WriterDeliveryStage::Push,
            cause: Box::new(provenant_os::git_config::ConfigError {
                context: provenant_os::git_config::ConfigContext::new(
                    "read effective Git configuration",
                    "/p/.git/config",
                    None,
                ),
                source: RunError::ConfigCommandFailed {
                    reason: "cause marker".into(),
                },
            }),
        };
        let rendered = error.to_string();
        assert!(
            rendered.contains(
                "host writer delivery failed at Push: read effective Git configuration on /p/.git/config"
            ),
            "{rendered}"
        );
        assert_eq!(rendered.matches("cause marker").count(), 1, "{rendered}");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn transport_refusal_diagnostic_keeps_stage_and_reason() {
        let reason = "check delivery transport configuration on /p: leaf marker";
        let error = RunError::DeliveryTransport {
            stage: WriterDeliveryStage::Push,
            kind: crate::TransportRefusal::Unreadable,
            reason: reason.into(),
        };
        let diagnostic = error.diagnostic(Destination::Local);
        assert_eq!(
            diagnostic.detail.as_deref(),
            Some(
                "host writer delivery failed at Push (transport): check delivery transport configuration on /p: leaf marker"
            )
        );
        assert_eq!(
            diagnostic.fact,
            Some(
                provenant_records::diagnostic::DiagnosticFact::DeliveryStage(
                    WriterDeliveryStage::Push
                )
            )
        );
    }

    #[test]
    fn stored_read_failure_keeps_context_and_underlying_kind() {
        let original = RunError::Filesystem(provenant_os::fs::FsError::ReadIo {
            path: "/fixture/process-exit.json".into(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "fixture"),
        });
        let kind = ErrorKind::from(&original);
        let error = original.with_record_context("/fixture/process-exit.json", "StoredExit");
        assert_eq!(ErrorKind::from(&error), kind);
        let detail = error.diagnostic(Destination::Local).detail.unwrap();
        assert!(detail.contains("/fixture/process-exit.json"));
        assert!(detail.contains("StoredExit"));
        let remote = error.diagnostic(Destination::Remote);
        assert!(
            remote
                .detail
                .unwrap()
                .contains("/fixture/process-exit.json")
        );
    }

    #[test]
    fn unreadable_record_diagnostic_names_the_local_file() {
        #[derive(serde::Deserialize)]
        struct ExitFixture {
            #[serde(rename = "required")]
            _required: bool,
        }
        let error = provenant_records::decode::<ExitFixture, _>(
            br#"{"schema_version":1}"#.as_slice(),
            Default::default(),
        )
        .err()
        .unwrap()
        .with_context("/fixture/process-exit.json", "ExitFixture");
        let error = RunError::Decode(error);
        let local = error.diagnostic(Destination::Local);
        let detail = local.detail.unwrap();
        for fact in [
            "/fixture/process-exit.json",
            "ExitFixture",
            "required",
            "schema version 1",
        ] {
            assert!(detail.contains(fact), "missing {fact}: {detail}");
        }
        let remote = error.diagnostic(Destination::Remote);
        assert!(
            remote
                .detail
                .unwrap()
                .contains("/fixture/process-exit.json")
        );
    }

    #[test]
    fn writer_lease_refusal_keeps_kind_and_local_holder_details() {
        let error = RunError::Writer(
            provenant_confinement::worktrees::WorktreeError::WriterLeaseHeld {
                holder: provenant_core::AttemptId::from_bytes([53; 16]),
                branch: "refs/heads/holder-branch".into(),
                worktree: "/private/holder-worktree".into(),
                age: "4d 1h".into(),
                live: Some(false),
            },
        );
        assert_eq!(ErrorKind::from(&error), ErrorKind::WriterLeaseHeld);
        let local = error.diagnostic(Destination::Local);
        assert_eq!(local.code, "writer_lease_held");
        let detail = local.detail.unwrap();
        for fact in [
            "35353535353535353535353535353535",
            "holder-branch",
            "/private/holder-worktree",
            "4d 1h",
            "live: false",
        ] {
            assert!(detail.contains(fact), "missing {fact}: {detail}");
        }
        let remote = error.diagnostic(Destination::Remote);
        assert!(remote.detail.unwrap().contains("/private/holder-worktree"));
    }

    #[test]
    fn confinement_resource_diagnostic_keeps_code_and_excludes_source_text() {
        use provenant_confinement::permissions::PermissionError;
        use provenant_records::diagnostic::{DiagnosticFact, Layer, ResourceFact};
        let error = RunError::Confinement(PermissionError::ConfigOrigin {
            source: Box::new(PermissionError::GrantLimitExceeded {
                family: "TOKEN_SENTINEL",
                limit: 17,
            }),
            origin: Layer::Host,
        });
        let diagnostic = error.diagnostic(Destination::Remote);
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
    fn dirty_writer_projection_uses_typed_path_presence() {
        use provenant_confinement::worktrees::WorktreeError;
        for (paths, names_edits) in [
            (String::new(), false),
            ("TOKEN_SENTINEL\u{1b}".into(), true),
        ] {
            let error = RunError::Writer(WorktreeError::Dirty(paths));
            for destination in [Destination::Local, Destination::Remote] {
                let diagnostic = error.diagnostic(destination);
                assert_eq!(diagnostic.code, error.code());
                assert_eq!(diagnostic.detail.is_some(), names_edits);
                assert!(
                    !serde_json::to_string(&diagnostic)
                        .unwrap()
                        .contains("TOKEN_SENTINEL")
                );
            }
        }
    }

    #[test]
    fn untrusted_error_sources_do_not_enter_diagnostics() {
        use provenant_confinement::worktrees::WorktreeError;
        use provenant_records::diagnostic::DiagnosticFact;
        let cases = [
            RunError::Io(std::io::Error::other("TOKEN_SENTINEL\u{1b}[31m")),
            RunError::Writer(WorktreeError::BranchCollision {
                existing: "TOKEN_SENTINEL\u{1b}".into(),
            }),
            RunError::Sqlite(rusqlite::Error::InvalidParameterName(
                "TOKEN_SENTINEL".into(),
            )),
            RunError::Filesystem(provenant_os::fs::FsError::ReadIo {
                path: "/private/host".into(),
                source: std::io::Error::other("TOKEN_SENTINEL"),
            }),
        ];
        for error in cases {
            for destination in [Destination::Local, Destination::Remote] {
                let projection = error.diagnostic(destination);
                let encoded = serde_json::to_string(&projection).unwrap();
                assert_eq!(projection.code, error.code());
                assert!(!encoded.contains("TOKEN_SENTINEL"));
                assert!(encoded.len() < 4096);
                assert!(!encoded.chars().any(char::is_control));
                if matches!(&error, RunError::Filesystem(_)) {
                    assert!(matches!(
                        projection.fact,
                        Some(DiagnosticFact::Filesystem {
                            operation: "read file",
                            cause: None,
                        })
                    ));
                }
            }
        }
    }

    #[test]
    fn branch_collision_diagnostic_names_the_ref() {
        use provenant_confinement::worktrees::WorktreeError;
        let error = RunError::Writer(WorktreeError::BranchCollision {
            existing: "refs/remotes/origin/team/smoke".into(),
        });
        for destination in [Destination::Local, Destination::Remote] {
            let diagnostic = error.diagnostic(destination);
            let detail = diagnostic.detail.expect("collision names the ref");
            assert!(
                detail.contains("refs/remotes/origin/team/smoke"),
                "missing ref: {detail}"
            );
            assert!(!detail.chars().any(char::is_control));
        }
    }

    #[test]
    fn project_mismatch_discloses_paths_at_each_destination() {
        let error = RunError::WriterProjectMismatch {
            worktree: "/foreign/tree".into(),
            project: "/foreign".into(),
            selected: "/selected".into(),
        };
        let local = error.diagnostic(Destination::Local);
        let detail = local.detail.unwrap();
        assert!(detail.contains("/foreign/tree"));
        assert!(detail.contains("/selected"));
        let remote = error.diagnostic(Destination::Remote);
        let detail = remote.detail.unwrap();
        assert!(detail.contains("/foreign/tree"));
        assert!(detail.contains("/selected"));
        assert!(remote.next.is_some());
    }

    #[test]
    fn credential_grants_keep_typed_access_and_disclose_full_paths() {
        use provenant_records::diagnostic::DiagnosticFact;
        for (requested, expected_access) in [
            ("TOKEN_SENTINEL", "requested"),
            ("read", "read"),
            ("write", "write"),
        ] {
            let path = PathBuf::from("/private/root");
            let error = RunError::Confinement(
                provenant_confinement::permissions::PermissionError::CredentialGrant {
                    path: path.clone(),
                    access: requested,
                },
            );
            assert_eq!(
                ErrorKind::from(&error).boundary_class(),
                BoundaryClass::Credentials
            );
            for destination in [Destination::Local, Destination::Remote] {
                let diagnostic = error.diagnostic(destination);
                assert_eq!(diagnostic.code, "CREDENTIAL_ROOT");
                assert!(diagnostic.next.is_some());
                assert!(
                    matches!(&diagnostic.fact, Some(DiagnosticFact::CredentialGrant { access }) if *access == expected_access)
                );
                assert!(
                    diagnostic
                        .detail
                        .as_deref()
                        .unwrap()
                        .contains(path.to_string_lossy().as_ref())
                );
                assert!(
                    !serde_json::to_string(&diagnostic)
                        .unwrap()
                        .contains("TOKEN_SENTINEL")
                );
            }
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
        let remote = error.diagnostic(Destination::Remote);
        assert!(
            serde_json::to_string(&remote)
                .unwrap()
                .contains("/checkout/has a")
        );
    }

    #[test]
    fn unknown_occupancy_diagnostics_name_worktree_at_each_destination() {
        use provenant_confinement::worktrees::{Occupancy, WorktreeError};
        for (error, expected_kind) in [
            (
                RunError::Writer(WorktreeError::CensusUnknown {
                    path: PathBuf::from("/checkout/tree"),
                    detail: "process 123: cwd unreadable at /private/other".into(),
                }),
                ErrorKind::WriterUnknown,
            ),
            (
                RunError::Writer(WorktreeError::UnknownOccupancy {
                    path: PathBuf::from("/checkout/tree"),
                    detail: "process 123: cwd unreadable at /private/other".into(),
                }),
                ErrorKind::WriterUnknown,
            ),
            (
                RunError::AbandonBlocked(Box::new(settlement::AbandonmentRefusal::Occupancy {
                    path: PathBuf::from("/checkout/tree"),
                    state: Occupancy::census_unknown("/checkout/tree".into(), "partial"),
                    evidence: "process 123: cwd unreadable at /private/other".into(),
                })),
                ErrorKind::AbandonBlocked,
            ),
            (
                RunError::AbandonBlocked(Box::new(settlement::AbandonmentRefusal::WriterLease {
                    evidence: "/checkout/tree: process 123: cwd unreadable at /private/other"
                        .into(),
                })),
                ErrorKind::AbandonBlocked,
            ),
        ] {
            assert_eq!(ErrorKind::from(&error), expected_kind);
            let local = error.diagnostic(Destination::Local);
            let detail = local.detail.unwrap();
            assert!(detail.contains("/checkout/tree"));
            assert!(detail.contains("process 123: cwd unreadable at /private/other"));
            let remote = error.diagnostic(Destination::Remote);
            let rendered = serde_json::to_string(&remote).unwrap();
            assert!(rendered.contains("/checkout/tree"));
        }
    }
}
