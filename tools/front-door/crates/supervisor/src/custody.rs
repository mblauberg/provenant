//! Guardian channels and persistent writer reservations. A PID is never a handle.
pub(crate) mod liveness;
use crate::census::{
    CensusError, ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver,
    boot_observer,
};
use crate::machine::StopReason;
use crate::process_os::handoff::HandoffFailure;
pub use liveness::{GuardianLossRecovery, LossUnproven};
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, Digest, GitOid, WaitBudget};
use provenant_os::{
    exec::{CapturedStream, Termination},
    fs::{Directory, FsError, LockGuard, LockMode, Metadata, Publication, PublishMode},
};
use provenant_records::{ExtraFields, Limits, decode};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use thiserror::Error;

mod capture;
mod indexes;
mod launch_failure;
pub use launch_failure::{LaunchFailure, LaunchPhase};
mod writer_release;
pub use indexes::{
    RecordedAttemptIndex, attempt_index_retired, recorded_attempt_indexes, register_attempt_index,
};
#[cfg(any(test, feature = "fixture"))]
pub(crate) use writer_release::WRITER_LOCK_WAIT_HOOK;
pub use writer_release::{lock_writer_exclusion, lock_writer_occupancy, release_in_progress};

pub(crate) const RECORD_LIMIT: u64 = 1024 * 1024;
pub(crate) const EXIT_LIMIT: u64 = 16 * 1024;
pub const HOLDER_LOCK: &str = "holder.lock";
/// Internal, non-secret mark passed to the provider at spawn. Descendants
/// inherit it across fork, exec and setsid, so a host without an OS
/// containment mark can still name a detached process after guardian death.
pub(crate) const ATTEMPT_MARK_ENV: &str = "PROVENANT_ATTEMPT_MARK";
use provenant_records::custody::Scope;

/// Run seals this beside the reservation when private Git must be retained.
/// The run directory is verified against the reservation's device and inode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportFence {
    pub schema_version: u64,
    pub scope: Scope,
    pub lane: provenant_records::WorkKey,
    pub run_attempt: PathBuf,
}

/// Storage decoder for Run's durable import marker. The public handoff record
/// stays constructible by Run while this DTO retains fields added by later minors.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredImportFence {
    schema_version: u64,
    scope: Scope,
    lane: provenant_records::WorkKey,
    run_attempt: PathBuf,
    #[serde(flatten)]
    extra: ExtraFields,
}

/// A recovery target, carried intact from custody admission to the CLI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingImport {
    pub scope: Scope,
    pub fence: PathBuf,
    pub lane: provenant_records::WorkKey,
}
impl PendingImport {
    pub fn reconcile(&self) -> String {
        format!("provenant reconcile {}", self.lane)
    }
    pub fn abandon(&self) -> String {
        format!(
            "provenant reconcile --abandon {} --reason 'recover retained work manually'",
            self.lane
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustodyDecision {
    Pending(PendingImport),
    Settled(Scope),
}
#[derive(Debug, Error)]
pub enum CustodyError {
    #[error("worktree writer lock busy")]
    Busy,
    #[error("writer observation gate unavailable: {0}")]
    ObservationGate(String),
    #[error("writer reservation requires recovery")]
    Fenced,
    #[error("writer import retained for lane {} at {}; run `{}` or `{}`", fence.lane, fence.fence.display(), fence.reconcile(), fence.abandon())]
    ImportRetained { fence: Box<PendingImport> },
    #[error("custody identity is unknown")]
    UnknownIdentity,
    #[error("record is invalid or belongs to another generation")]
    InvalidRecord,
    #[error("capture plan {field} is {value}, outside {bound}")]
    CapturePlan {
        field: &'static str,
        value: u64,
        bound: String,
    },
    #[error(
        "attempt evidence was written before shared worktree leases and a holder may still be live; stop that holder and run `provenant reconcile` to clear the reservation"
    )]
    LegacyEvidence,
    #[error("launch handoff is unconfirmed ({0}); reservation remains fenced")]
    HandoffUnknown(String),
    /// This host cannot hold provider custody; nothing was handed off.
    #[error("process custody is supported only on macOS")]
    UnsupportedPlatform,
    #[error("sandbox profile transport handoff is unconfirmed ({0}); reservation remains fenced")]
    ProfileHandoffUnknown(String),
    /// The guardian never held the whole launch message; nothing launched.
    #[error("launch message never reached the guardian ({0}); the provider never launched")]
    HandoffUndelivered(String),
    /// The sandbox profile failed in transit or the guardian refused it before
    /// accepting custody; nothing launched.
    #[error(
        "sandbox profile transport failed before the guardian accepted the launch ({0}); the provider never launched"
    )]
    ProfileHandoffUndelivered(String),
    #[error("operation was cancelled; custody is retained")]
    Cancelled,
    #[error("another stop request already owns this attempt")]
    StopConflict,
    #[error("wait budget expired; custody is retained")]
    Timeout,
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    #[error(transparent)]
    Decode(#[from] provenant_records::DecodeError),
    #[error("custody reservation may be retained: {0}")]
    ReservationRetained(String),
    /// Retained custody reached its limit with processes unresolved. The
    /// guardian released its lease; the unresolved exit and fence remain.
    #[error("{0}")]
    RetentionLimit(Box<RetentionLimitReached>),
    #[error("custody I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(
        "reading stored record {} (record type {record_type}) failed: {source}",
        path.display()
    )]
    StoredRecordRead {
        path: PathBuf,
        record_type: String,
        #[source]
        source: Box<FsError>,
    },
}

impl CustodyError {
    /// The envelope status and stable error code that `internal supervise`
    /// reports, so the guardian's exit code distinguishes a retention limit
    /// (`timeout`, 124) from a supervisor fault (`error`, 3).
    pub fn outcome(&self) -> (provenant_records::Outcome, &'static str) {
        match self {
            Self::RetentionLimit(_) => (provenant_records::Outcome::Timeout, "RETENTION_LIMIT"),
            _ => (provenant_records::Outcome::Error, "SUPERVISOR"),
        }
    }
    /// A stored record that is present but does not decode, whether or not the
    /// decoder kept its path and type. Callers that classify a malformed
    /// record (as an invalid stop intent, a replaceable stale record or an
    /// authority boundary) match this rather than one variant.
    pub fn is_invalid_record(&self) -> bool {
        matches!(self, Self::InvalidRecord | Self::Decode(_))
    }
}

#[derive(Debug, Error)]
#[error("boot identity unavailable")]
struct BootIdentityUnavailable(#[source] CensusError);
/// Why admission could not judge a lane start. The guardian publishes these
/// refusals, so each message is fixed text that cannot expose a path.
#[derive(Debug, Error)]
pub enum AdmissionError {
    /// Admission could not judge startup spacing. The reason is fixed text, so
    /// it can reach the user without any path.
    #[error(
        "startup spacing cannot be judged: the host monotonic clock or boot identity cannot be read"
    )]
    Clock,
    #[error("invalid host lane settings: check domains.fabric.lanes")]
    BuildSettings,
    #[error("guardian process identity unavailable; retry when process observation is available")]
    GuardianIdentity,
    /// Numeric host observations are safe to publish as the lane's refusal.
    #[error(
        "insufficient disk space: {free} bytes available, gc.critical_free {floor} bytes (source: {source_label}); run `provenant gc --apply`",
        source_label = .origin.description()
    )]
    Disk {
        free: u64,
        floor: u64,
        origin: provenant_records::diagnostic::Layer,
    },
    #[error("memory pressure is {level:?}; domains.fabric.lanes.memory_pressure_threshold is {threshold_label} (source: {source_label}); wait for host memory pressure to fall", threshold_label = .threshold.name(), source_label = .origin.description())]
    MemoryPressure {
        level: provenant_records::diagnostic::PressureLevel,
        threshold: provenant_records::diagnostic::PressureThreshold,
        origin: provenant_records::diagnostic::Layer,
    },
    #[error(
        "user process count {used} of {limit}; wait for processes to exit or raise the per-user process limit"
    )]
    Processes { used: u64, limit: u64 },
    #[error(
        "lane cap reached: {active} of {limit} {description} are active; {setting} (source: {source_label}); wait for a lane to finish",
        description = .kind.description(), setting = .kind.setting(), source_label = .origin.description()
    )]
    LaneCap {
        active: u64,
        limit: u64,
        kind: provenant_records::diagnostic::LaneLimit,
        origin: provenant_records::diagnostic::Layer,
    },
}
impl AdmissionError {
    /// Configuration keys associated with this refusal, derived from its
    /// typed cause and the declaration that supplied the limit.
    pub fn setting_names(&self) -> Vec<&'static str> {
        match self {
            Self::Clock | Self::GuardianIdentity | Self::Processes { .. } => Vec::new(),
            Self::BuildSettings => vec!["domains.fabric.lanes"],
            Self::Disk { .. } => vec!["gc.critical_free"],
            Self::MemoryPressure { .. } => {
                vec!["domains.fabric.lanes.memory_pressure_threshold"]
            }
            Self::LaneCap { kind, .. } => vec![kind.setting()],
        }
    }

    /// Closed host resource facts retained by both custody handoff and guardian
    /// admission. Operational failures carry no freely formatted safe detail.
    pub fn resource_fact(&self) -> Option<provenant_records::diagnostic::ResourceFact> {
        use provenant_records::diagnostic::ResourceFact;
        match *self {
            Self::Disk {
                free,
                floor,
                origin,
            } => Some(ResourceFact::Disk {
                free,
                floor,
                origin,
            }),
            Self::MemoryPressure {
                level,
                threshold,
                origin,
            } => Some(ResourceFact::MemoryPressure {
                level,
                threshold,
                origin,
            }),
            Self::LaneCap {
                active,
                limit,
                kind,
                origin,
            } => Some(ResourceFact::LaneCap {
                active,
                limit,
                lane_limit: kind,
                origin,
            }),
            Self::Processes { used, limit } => Some(ResourceFact::Processes {
                used,
                limit,
                reserved: provenant_os::sys::resources::ProcessBudget { used, limit }.headroom(),
            }),
            Self::Clock | Self::BuildSettings | Self::GuardianIdentity => None,
        }
    }
}

/// Only closed admission causes may publish their text. Arbitrary source
/// errors can contain paths, prompts or credentials.
pub(crate) fn admission_refusal(error: &CustodyError) -> String {
    match error {
        CustodyError::Admission(_) => error.to_string(),
        _ => "admission refused".into(),
    }
}

pub type Result<T> = std::result::Result<T, CustodyError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LaunchState {
    Prepared,
    HandingOff,
    NeverSpawned,
    Spawned,
    Settling,
}
/// Confinement supplies verified worktree and Git directory handles, never a
/// caller-selected PID or a path that supervisor resolves again. A read-only
/// binding holds custody in the attempt directory and grants no worktree.
#[derive(Clone, Debug, Serialize)]
pub struct WriterBinding(StoredWriterBinding);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum StoredWriterBinding {
    Worktree {
        worktree_device: u64,
        worktree_inode: u64,
        git_device: u64,
        git_inode: u64,
        branch: String,
        base: GitOid,
        #[serde(flatten)]
        extra: ExtraFields,
    },
    ReadOnly {
        #[serde(flatten)]
        extra: ExtraFields,
    },
}
impl WriterBinding {
    /// The binding of a read-only attempt, which holds no worktree.
    pub fn read_only() -> Self {
        Self(StoredWriterBinding::ReadOnly {
            extra: ExtraFields::default(),
        })
    }
    pub fn from_verified(
        worktree: &Directory,
        git: &Directory,
        branch: &str,
        base: GitOid,
    ) -> Result<Self> {
        let worktree = worktree.metadata()?;
        let git = git.metadata()?;
        let binding = StoredWriterBinding::Worktree {
            worktree_device: worktree.device,
            worktree_inode: worktree.inode,
            git_device: git.device,
            git_inode: git.inode,
            branch: branch.into(),
            base,
            extra: ExtraFields::default(),
        };
        // The confinement owner also verifies Git ref semantics. Here reject
        // control bytes and unbounded input at the storage edge.
        binding.validate()?;
        Ok(Self(binding))
    }
}
impl StoredWriterBinding {
    /// Branch and base may advance between generations; the held worktree may not.
    fn same_custody(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Worktree {
                    worktree_device: a_device,
                    worktree_inode: a_inode,
                    ..
                },
                Self::Worktree {
                    worktree_device: b_device,
                    worktree_inode: b_inode,
                    ..
                },
            ) => a_device == b_device && a_inode == b_inode,
            (Self::ReadOnly { .. }, Self::ReadOnly { .. }) => true,
            _ => false,
        }
    }
    fn validate(&self) -> Result<()> {
        if let Self::Worktree { branch, .. } = self
            && (branch.is_empty()
                || branch.len() > 1024
                || branch.bytes().any(|b| b.is_ascii_control()))
        {
            return Err(CustodyError::InvalidRecord);
        }
        Ok(())
    }

    /// Handoff messages are authority-bearing input and must not smuggle
    /// extension data through the permissive persisted-record shape.
    pub(crate) fn has_extensions(&self) -> bool {
        match self {
            Self::Worktree { extra, .. } | Self::ReadOnly { extra } => !extra.is_empty(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnershipJournal {
    schema_version: u64,
    scope: Scope,
    binding: StoredWriterBinding,
    #[serde(flatten)]
    extra: ExtraFields,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettlementJournal {
    schema_version: u64,
    scope: Scope,
    #[serde(flatten)]
    extra: ExtraFields,
}

/// The per-slot admission counter is never reset when attempts settle. A
/// stopped writer can therefore distinguish a worktree that stayed private
/// from one that was shared and later became quiet again.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HolderGeneration {
    schema_version: u64,
    generation: u64,
    #[serde(flatten)]
    extra: ExtraFields,
}

/// Snapshot captured while the slot admission gate is held. The first holder
/// in a slot can still learn that a sibling was present when it started.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HolderAdmission {
    schema_version: u64,
    scope: Scope,
    generation: u64,
    shared_at_admission: bool,
    #[serde(flatten)]
    extra: ExtraFields,
}

const HOLDER_GENERATION: &str = "holder-generation.json";
const HOLDER_ADMISSION: &str = "holder-admission.json";

/// Durable attribution for the processes a lane spawns. The identity gate
/// publishes it in the reservation before provider exec, so a later recovery
/// can still name the lane's processes if the guardian dies before an exit.
///
/// `contained` says whether an OS mark is in effect that survives a
/// double-fork and setsid. Without one, group and session identify members of
/// the recorded tree but grant no signal authority: they can only be reported.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProcessAttribution {
    pub boot: String,
    pub group_id: i64,
    pub session_id: i64,
    pub contained: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Reservation {
    pub schema_version: u64,
    pub scope: Scope,
    pub launch_state: LaunchState,
    pub attempt_device: Option<u64>,
    pub attempt_inode: Option<u64>,
    #[serde(default)]
    pub attempt_path: Option<PathBuf>,
    #[serde(default)]
    pub requester: Option<ActorId>,
    pub reservation_device: u64,
    pub reservation_inode: u64,
    pub binding: StoredWriterBinding,
    pub boot: String,
    pub guardian: Option<ProcessIdentity>,
    pub provider: Option<ProcessIdentity>,
    pub unresolved_cleanup: bool,
    pub descendants: Vec<ProcessIdentity>,
    pub census_incomplete: bool,
    pub omitted_descendants: u64,
    /// The provider ran under the attempt's sandbox mark, so a later scan can
    /// find processes the guardian never recorded. Set when the provider is
    /// spawned; absent means no mark was in effect.
    #[serde(default)]
    pub sandboxed: bool,
    /// Attempt tag recorded at provider spawn. Absent means the guardian died
    /// before attribution could be published, so no descendant can be named.
    #[serde(default)]
    pub attribution: Option<ProcessAttribution>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}
impl Reservation {
    pub(crate) fn validate(&self) -> Result<()> {
        self.binding.validate()?;
        if self.attempt_device.is_some() != self.attempt_inode.is_some() {
            return Err(CustodyError::InvalidRecord);
        }
        let known_platform = self
            .boot
            .strip_prefix("linux:")
            .or_else(|| self.boot.strip_prefix("darwin:"));
        let uuid = known_platform.is_some_and(|value| {
            value.len() == 36
                && value.bytes().enumerate().all(|(i, b)| {
                    if matches!(i, 8 | 13 | 18 | 23) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
        });
        #[cfg(feature = "fixture")]
        let fixture = self.boot == "fixture-recorded-boot-v1";
        #[cfg(not(feature = "fixture"))]
        let fixture = false;
        if self.schema_version != 1
            || self.scope.generation == 0
            || !(uuid || fixture)
            || self.descendants.len() > 128
        {
            return Err(CustodyError::InvalidRecord);
        }
        let empty_tree =
            self.provider.is_none() && self.descendants.is_empty() && self.omitted_descendants == 0;
        let valid_state = match self.launch_state {
            LaunchState::Prepared => {
                empty_tree
                    && self.guardian.is_none()
                    && self.attempt_device.is_none()
                    && self.unresolved_cleanup
                    && self.census_incomplete
            }
            LaunchState::NeverSpawned => {
                empty_tree
                    && self.attempt_device.is_some()
                    && !self.unresolved_cleanup
                    && !self.census_incomplete
            }
            LaunchState::HandingOff => {
                empty_tree
                    && self.attempt_device.is_some()
                    && self.unresolved_cleanup
                    && self.census_incomplete
            }
            LaunchState::Settling => {
                self.guardian.is_some()
                    && self.attempt_device.is_some()
                    && !self.unresolved_cleanup
                    && !self.census_incomplete
            }
            LaunchState::Spawned => {
                self.guardian.is_some()
                    && self.attempt_device.is_some()
                    && (self.unresolved_cleanup
                        || (!self.census_incomplete && self.omitted_descendants == 0))
                    && (self.provider.is_some()
                        || (self.unresolved_cleanup && self.census_incomplete))
            }
        };
        if !valid_state {
            return Err(CustodyError::InvalidRecord);
        }
        for identity in self
            .guardian
            .iter()
            .chain(self.provider.iter())
            .chain(self.descendants.iter())
        {
            identity
                .validate()
                .map_err(|_| CustodyError::InvalidRecord)?;
            if identity.boot != self.boot {
                return Err(CustodyError::InvalidRecord);
            }
        }
        if let Some(attribution) = &self.attribution
            && (attribution.boot != self.boot
                || attribution.group_id <= 0
                || attribution.session_id <= 0)
        {
            return Err(CustodyError::InvalidRecord);
        }
        if let Some(guardian) = &self.guardian
            && (guardian.session_id != guardian.pid || guardian.group_id != guardian.pid)
        {
            return Err(CustodyError::InvalidRecord);
        }
        if let Some(provider) = &self.provider
            && self.guardian.as_ref().is_none_or(|guardian| {
                provider.session_id != guardian.session_id || provider.group_id != provider.pid
            })
        {
            return Err(CustodyError::InvalidRecord);
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct CustodyLease {
    pub(crate) directory: Directory,
    pub(crate) lock_directory: Directory,
    pub(crate) lease: Option<LockGuard>,
    pub(crate) holder: Option<LockGuard>,
    pub(crate) reservation: Reservation,
    phase: LaunchPhase,
}
impl CustodyLease {
    pub fn scope(&self) -> Scope {
        self.reservation.scope
    }
    /// How far launch got; only [`LaunchPhase::HandedOff`] may have reached a
    /// guardian that holds the launch message.
    pub fn phase(&self) -> LaunchPhase {
        self.phase
    }
    pub fn lock_directory(&self) -> &Directory {
        &self.lock_directory
    }
    #[cfg(feature = "fixture")]
    pub(crate) fn record_fixture_spawned_provider(
        &mut self,
        attempt: &Directory,
        guardian: crate::census::ProcessIdentity,
        provider: crate::census::ProcessIdentity,
    ) -> Result<()> {
        if self.phase != LaunchPhase::Unlaunched
            || self.lease.is_none()
            || self.reservation.launch_state != LaunchState::Prepared
        {
            return Err(CustodyError::Fenced);
        }
        let metadata = attempt.metadata()?;
        let mut reservation = self.reservation.clone();
        reservation.attempt_device = Some(metadata.device);
        reservation.attempt_inode = Some(metadata.inode);
        reservation.launch_state = LaunchState::Spawned;
        reservation.guardian = Some(guardian);
        reservation.provider = Some(provider);
        reservation.unresolved_cleanup = false;
        reservation.census_incomplete = false;
        reservation.validate()?;
        write_record(
            &self.directory,
            "reservation.json",
            &reservation,
            PublishMode::Replace,
        )?
        .require_durable()?;
        self.reservation = reservation;
        self.phase = LaunchPhase::HandedOff;
        Ok(())
    }
    /// A failure before handoff, including a never-spawned launch, has no
    /// child to fence. Explicit withdrawal keeps crashes conservative:
    /// dropping a lease never clears it. Attempt evidence is retained.
    pub fn withdraw_unlaunched(self) -> Result<()> {
        if !self.phase.withdrawable() || self.lease.is_none() {
            return Err(CustodyError::Fenced);
        }
        indexes::retire_unlaunched_index(&self.directory, &self.reservation)?;
        clear_reservation(&self.directory, self.scope())
    }
    fn ensure_lease(&mut self) -> Result<()> {
        if self.lease.is_none() {
            let clock = provenant_os::clock::SystemClock::new();
            self.lease = Some(lock_writer_occupancy(
                &self.lock_directory,
                WaitBudget::cli(clock.mono_now(), 5).map_err(|_| CustodyError::InvalidRecord)?,
                &CancelToken::new(),
                &clock,
            )?);
        }
        Ok(())
    }
    /// Reacquire shared occupancy after a completed guardian handoff (up to 5 s).
    pub fn into_recovery(mut self) -> Result<Recovery> {
        self.ensure_lease()?;
        let reservation: Reservation =
            read_record(&self.directory, "reservation.json", RECORD_LIMIT)?;
        validate_anchor(&self.directory, &reservation)?;
        Ok(Recovery {
            directory: self.directory,
            lock_directory: self.lock_directory,
            lease: Some(self.lease.ok_or(CustodyError::Fenced)?),
            holder: self.holder.take(),
            reservation,
            guardian_quiescent: false,
            loss_locks: None,
        })
    }
}
/// Read-only custody uses the attempt directory itself, so readers never share
/// or contend for a worktree fence. The same reservation and settlement rules
/// apply: a guardian must prove exit before the reservation is cleared.
pub fn acquire_reader(
    attempt: Directory,
    scope: Scope,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<CustodyLease> {
    acquire_writer(
        attempt,
        scope,
        WriterBinding(StoredWriterBinding::ReadOnly {
            extra: ExtraFields::default(),
        }),
        budget,
        cancel,
        clock,
    )
}
/// The supplied directory is a stable exclusion slot, not a registry.
/// Active holders share worktree occupancy and retain a slot-local per-attempt
/// holder lock through guardian settlement. Recovery evidence remains scoped
/// to its attempt.
pub fn acquire_writer(
    directory: Directory,
    scope: Scope,
    binding: WriterBinding,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<CustodyLease> {
    acquire_writer_with_observer(
        directory,
        scope,
        binding,
        budget,
        cancel,
        clock,
        &SystemProcessObserver,
    )
}
/// Tests and the OS driver inject observation at the native process seam.
pub(crate) fn acquire_writer_with_observer(
    directory: Directory,
    scope: Scope,
    binding: WriterBinding,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
    observer: &dyn ProcessObserver,
) -> Result<CustodyLease> {
    let lease =
        lock_writer_occupancy(&directory, budget, cancel, clock).map_err(|error| match error {
            CustodyError::Filesystem(FsError::LockTimeout { .. }) => CustodyError::Busy,
            other => other,
        })?;
    let lock_directory = directory;
    let directory = create_attempt_evidence(&lock_directory, scope)?;
    let holder = directory
        .lock(
            OsStr::new(HOLDER_LOCK),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => CustodyError::Busy,
            other => other.into(),
        })?;
    ensure_no_exclusive_reservation(&lock_directory)?;
    match directory.metadata_at(OsStr::new("reservation.json"), false) {
        Ok(_) => {
            let reservation: Reservation =
                read_record(&directory, "reservation.json", RECORD_LIMIT)
                    .map_err(|_| CustodyError::Fenced)?;
            validate_anchor(&directory, &reservation).map_err(|_| CustodyError::Fenced)?;
            return Err(
                if classify_reservation(&reservation, observer) == RecoveryState::Reattached {
                    CustodyError::Busy
                } else {
                    CustodyError::Fenced
                },
            );
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    // Serialise the durable history with sibling admission. Keep the gate
    // through reservation publication so every generation increment names an
    // actual admitted holder, even if its owner dies immediately afterwards.
    let _admission = writer_admission_guard(&lock_directory)?;
    match directory.metadata_at(OsStr::new("ownership.json"), false) {
        Ok(_) => {
            let previous: OwnershipJournal =
                read_record(&directory, "ownership.json", RECORD_LIMIT)?;
            let settled: SettlementJournal =
                read_record(&directory, "settlement.json", RECORD_LIMIT)
                    .map_err(|_| CustodyError::Fenced)?;
            // A settled owner no longer holds this path, so a new attempt may
            // bind a worktree recreated there. Only the attempt that settled
            // may be readmitted, and it keeps the worktree it held.
            let same_custody = previous.binding.same_custody(&binding.0);
            if previous.schema_version != 1
                || settled.schema_version != 1
                || settled.scope != previous.scope
                || (scope.generation > 1
                    && (scope.attempt != settled.scope.attempt || !same_custody))
            {
                return Err(CustodyError::Fenced);
            }
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    if scope.generation == 0 {
        return Err(CustodyError::InvalidRecord);
    }
    let metadata = directory.metadata()?;
    let reservation = Reservation {
        schema_version: 1,
        scope,
        launch_state: LaunchState::Prepared,
        attempt_device: None,
        attempt_inode: None,
        attempt_path: None,
        requester: None,
        reservation_device: metadata.device,
        reservation_inode: metadata.inode,
        binding: binding.0,
        boot: observer
            .boot_identity()
            .map_err(|_| CustodyError::UnknownIdentity)?,
        guardian: None,
        provider: None,
        unresolved_cleanup: true,
        descendants: Vec::new(),
        census_incomplete: true,
        omitted_descendants: 0,
        sandboxed: false,
        attribution: None,
        extra: Default::default(),
    };
    reservation.validate()?;
    let siblings = list_writer_reservations_in_slot(&lock_directory)?;
    if siblings.is_fenced() {
        return Err(CustodyError::Fenced);
    }
    let sibling_present = siblings
        .reservations
        .iter()
        .any(|reservation| reservation.scope.attempt != scope.attempt);
    let previous_generation =
        match read_record::<HolderGeneration>(&lock_directory, HOLDER_GENERATION, RECORD_LIMIT) {
            Ok(record) if record.schema_version == 1 => record.generation,
            Ok(_) => return Err(CustodyError::InvalidRecord),
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => 0,
            Err(error) => return Err(error),
        };
    let generation = previous_generation
        .checked_add(1)
        .ok_or(CustodyError::InvalidRecord)?;
    // Publish the fence first: owner death cannot leave an invisible ownership gap.
    // Publication can fail after the reservation becomes visible. From this
    // point an error must not be mistaken for a custody-free refusal.
    let publish = (|| -> Result<()> {
        Publication::require_durable(write_record(
            &directory,
            HOLDER_ADMISSION,
            &HolderAdmission {
                schema_version: 1,
                scope,
                generation,
                shared_at_admission: sibling_present,
                extra: Default::default(),
            },
            PublishMode::Replace,
        )?)?;
        Publication::require_durable(write_record(
            &directory,
            "reservation.json",
            &reservation,
            PublishMode::NoReplace,
        )?)?;
        Publication::require_durable(write_record(
            &lock_directory,
            HOLDER_GENERATION,
            &HolderGeneration {
                schema_version: 1,
                generation,
                extra: Default::default(),
            },
            PublishMode::Replace,
        )?)?;
        Publication::require_durable(write_record(
            &directory,
            "ownership.json",
            &OwnershipJournal {
                schema_version: 1,
                scope,
                binding: reservation.binding.clone(),
                extra: ExtraFields::default(),
            },
            PublishMode::Replace,
        )?)?;
        Ok(())
    })();
    publish.map_err(|error| CustodyError::ReservationRetained(error.to_string()))?;
    Ok(CustodyLease {
        directory,
        lock_directory,
        lease: Some(lease),
        holder: Some(holder),
        reservation,
        phase: LaunchPhase::Unlaunched,
    })
}

/// Open recovery evidence for one attempt without reading the shared lock slot.
pub fn attempt_evidence(
    directory: &Directory,
    scope: Scope,
) -> provenant_os::fs::Result<Directory> {
    directory.open_verified_directory(OsStr::new(&scope.attempt.to_string()))
}

/// Classify a retained import without changing its evidence. Only a matching
/// settlement journal AND Run's inode-bound release attestations retire a
/// reservation left behind by an interrupted removal. Uncertain proof retains
/// custody; a historical marker without a reservation grants no custody.
pub fn retained_import_decision(
    evidence: &Directory,
    expected: Scope,
) -> Result<Option<CustodyDecision>> {
    let reservation: Reservation = match read_record(evidence, "reservation.json", RECORD_LIMIT) {
        Ok(record) => record,
        Err(CustodyError::Filesystem(error)) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error),
    };
    validate_anchor(evidence, &reservation)?;
    if reservation.scope != expected
        || evidence.current_path()?.file_name() != Some(OsStr::new(&expected.attempt.to_string()))
    {
        return Err(CustodyError::InvalidRecord);
    }
    let marker: StoredImportFence =
        match read_record(evidence, "import-retained.json", RECORD_LIMIT) {
            Ok(record) => record,
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
    if marker.schema_version != 1 || marker.scope != expected {
        return Err(CustodyError::InvalidRecord);
    }
    let pending = CustodyDecision::Pending(PendingImport {
        scope: expected,
        fence: evidence.current_path()?.join("reservation.json"),
        lane: marker.lane,
    });
    let settled = (|| -> Result<()> {
        let journal: SettlementJournal = read_record(evidence, "settlement.json", RECORD_LIMIT)?;
        if journal.schema_version != 1 || journal.scope != expected {
            return Err(CustodyError::InvalidRecord);
        }
        let attempt = Directory::open_anchor(&marker.run_attempt)?;
        let metadata = attempt.metadata()?;
        if reservation.attempt_device != Some(metadata.device)
            || reservation.attempt_inode != Some(metadata.inode)
        {
            return Err(CustodyError::InvalidRecord);
        }
        let quarantine = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        verify_release(expected, &attempt, &attempt, &quarantine)
    })();
    Ok(Some(if settled.is_ok() {
        CustodyDecision::Settled(expected)
    } else {
        pending
    }))
}

/// Supervisor owns the format-2 slot scan. Root-slot records are not attempt
/// evidence. Callers retain writer exclusion when using this for mutation.
pub fn pending_import(slot: &Directory) -> Result<Option<PendingImport>> {
    for (index, name) in slot.entries()?.enumerate() {
        if index >= 10_000 {
            return Err(CustodyError::InvalidRecord);
        }
        let name = name?;
        let Some(attempt) = name.to_str().and_then(|name| AttemptId::parse(name).ok()) else {
            continue;
        };
        let evidence = slot.open_verified_directory(&name)?;
        let reservation: Reservation =
            match read_record(&evidence, "reservation.json", RECORD_LIMIT) {
                Ok(record) => record,
                Err(CustodyError::Filesystem(error)) if error.is_not_found() => continue,
                Err(error) => return Err(error),
            };
        if reservation.scope.attempt != attempt {
            return Err(CustodyError::InvalidRecord);
        }
        if let Some(CustodyDecision::Pending(fence)) =
            retained_import_decision(&evidence, reservation.scope)?
        {
            return Ok(Some(fence));
        }
    }
    Ok(None)
}
/// Active sibling holders may coexist; an unresolved reservation whose
/// slot-local holder proof is absent remains a fence until recovery.
fn ensure_no_exclusive_reservation(directory: &Directory) -> Result<()> {
    for (index, name) in directory.entries()?.enumerate() {
        if index >= 10_000 {
            return Err(CustodyError::Fenced);
        }
        let name = name?;
        if name
            .to_str()
            .and_then(|name| AttemptId::parse(name).ok())
            .is_none()
            || directory.metadata_at(&name, false)?.kind != provenant_os::fs::FileKind::Directory
        {
            continue;
        }
        let child = directory.open_verified_directory(&name)?;
        let value: serde_json::Value = match read_record(&child, "reservation.json", RECORD_LIMIT) {
            Ok(value) => value,
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => continue,
            Err(_) => return Err(CustodyError::Fenced),
        };
        let reservation: Reservation =
            serde_json::from_value(value.clone()).map_err(|_| CustodyError::Fenced)?;
        validate_anchor(&child, &reservation).map_err(|_| CustodyError::Fenced)?;
        if name != OsStr::new(&reservation.scope.attempt.to_string()) {
            return Err(CustodyError::Fenced);
        }
        match retained_import_decision(&child, reservation.scope)
            .map_err(|_| CustodyError::Fenced)?
        {
            Some(CustodyDecision::Pending(fence)) => {
                return Err(CustodyError::ImportRetained {
                    fence: Box::new(fence),
                });
            }
            Some(CustodyDecision::Settled(scope)) => {
                // The writer lease is held. Finish only the verified interrupted
                // removal; retain private Git, markers and the run's evidence.
                clear_reservation(&child, scope)?;
                continue;
            }
            None => {}
        }
        // Providers do not inherit writer.lock. Each active reservation instead
        // keeps its slot-local holder lock through guardian settlement. A free,
        // missing or unreadable lock leaves the durable fence in force.
        if (reservation.unresolved_cleanup
            && matches!(
                reservation.launch_state,
                LaunchState::HandingOff | LaunchState::Spawned
            ))
            || reservation.launch_state == LaunchState::Settling
        {
            // Probe shared: concurrent observers never read each other's probe as a
            // live holder, while a holder's exclusive lock still refuses it.
            match child.try_lock_existing(OsStr::new(HOLDER_LOCK), LockMode::Shared) {
                Err(FsError::LockTimeout { .. }) => {}
                Ok(_live_holder) => return Err(CustodyError::Fenced),
                Err(_) => return Err(CustodyError::Fenced),
            }
        }
    }
    Ok(())
}

pub fn reservation_present(directory: &Directory, scope: Scope) -> provenant_os::fs::Result<bool> {
    let evidence = match attempt_evidence(directory, scope) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(error),
    };
    match evidence.metadata_at(OsStr::new("reservation.json"), false) {
        Ok(_) => Ok(true),
        Err(error) if error.is_not_found() => Ok(false),
        Err(error) => Err(error),
    }
}
fn recovery_evidence(directory: &Directory, scope: Scope) -> provenant_os::fs::Result<Directory> {
    let child = attempt_evidence(directory, scope)?;
    child.metadata_at(OsStr::new("reservation.json"), false)?;
    Ok(child)
}
/// Observe a guardian identity from scoped, inode-validated reservation evidence.
pub fn reservation_guardian(
    directory: &Directory,
    scope: Scope,
) -> Result<Option<ProcessIdentity>> {
    let evidence = recovery_evidence(directory, scope)?;
    let reservation: Reservation = read_record(&evidence, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(&evidence, &reservation)?;
    if reservation.scope != scope {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(reservation.guardian)
}

/// Read the exact provider process retained by one attempt's custody record.
/// The caller supplies that attempt's evidence directory; this observation
/// grants no signalling authority.
pub fn provider_identity(evidence: &Directory, scope: Scope) -> Result<Option<ProcessIdentity>> {
    let reservation: Reservation = read_record(evidence, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(evidence, &reservation)?;
    if reservation.scope != scope
        || !matches!(
            reservation.launch_state,
            LaunchState::Spawned | LaunchState::Settling
        )
    {
        return Err(CustodyError::InvalidRecord);
    }
    if let Some(provider) = &reservation.provider {
        provider
            .validate()
            .map_err(|_| CustodyError::InvalidRecord)?;
    }
    Ok(reservation.provider)
}

fn create_attempt_evidence(directory: &Directory, scope: Scope) -> Result<Directory> {
    let name = scope.attempt.to_string();
    match directory.mkdir(OsStr::new(&name), 0o700) {
        Ok(()) => directory.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(attempt_evidence(directory, scope)?)
}

pub(crate) fn write_record<T: Serialize>(
    dir: &Directory,
    name: &str,
    record: &T,
    mode: PublishMode,
) -> Result<Publication> {
    let bytes = serde_json::to_vec(record).map_err(|_| CustodyError::InvalidRecord)?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(dir.atomic_write(OsStr::new(name), ".custody-", &bytes, mode)?)
}
pub(crate) fn read_record<T: serde::de::DeserializeOwned>(
    dir: &Directory,
    name: &str,
    limit: u64,
) -> Result<T> {
    let type_name = std::any::type_name::<T>();
    let clean_type = type_name.rsplit("::").next().unwrap_or(type_name);
    let path = dir.current_path()?.join(name);
    let bytes = match dir.read_bounded(OsStr::new(name), limit) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Err(CustodyError::Filesystem(error)),
        Err(source) => {
            return Err(CustodyError::StoredRecordRead {
                path,
                record_type: clean_type.to_owned(),
                source: Box::new(source),
            });
        }
    };
    provenant_records::decode_stored_record(
        bytes.as_slice(),
        Limits {
            max_bytes: limit,
            ..Limits::CONFIG
        },
        path,
        clean_type,
    )
    .map_err(CustodyError::from)
}

/// Pre-spawn attempt lock and physical publication headroom. Never a launch file.
#[derive(Debug)]
pub struct PublicationReservation {
    pub(crate) directory: Directory,
    pub(crate) custody: LockGuard,
}
impl PublicationReservation {
    /// The attempt directory passed to the guardian at hand-off.
    pub fn directory(&self) -> &Directory {
        &self.directory
    }

    /// Capture custody identity from the held lock, never a lane-writable name.
    pub fn custody_metadata(&self) -> Result<Metadata> {
        Ok(self.custody.metadata()?)
    }

    pub fn prepare(
        directory: Directory,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Self> {
        let custody = directory.lock(
            OsStr::new("custody.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        // Existing exit/launch evidence refuses a second execution of an attempt.
        for name in ["process-exit.json", "launch.json"] {
            match directory.metadata_at(OsStr::new(name), false) {
                Ok(_) => return Err(CustodyError::Fenced),
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut reserve = directory.create_new(OsStr::new("reserve"))?;
        reserve.write_all(&[0_u8; 65536])?;
        reserve.sync()?;
        directory.sync()?;
        Ok(Self { directory, custody })
    }
}

/// Ephemeral launch input. Environment and prompt bytes travel only on a pipe.
#[derive(Debug)]
pub struct ProviderCommand {
    pub policy: provenant_records::policy::Policy,
    /// Immutable, anonymous profile retained through both exec handoffs.
    pub profile_file: Option<crate::ProfileFile>,
    /// Nice value inherited by the provider and its descendants.
    pub priority: u8,
    pub resolved_route: String,
    pub permission_plan_digest: Digest,
    /// The attempt's work identity; the guardian names the launch by it.
    pub key: provenant_records::WorkKey,
    pub requester: ActorId,
    /// Caller-owned config variables passed to the same-binary guardian.
    pub config_environment: Vec<OsString>,
    pub argv: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    /// Port space the guardian leases one block from for the provider's
    /// lifetime; the block is added to the provider's environment.
    pub lane_ports: Option<provenant_os::lane_ports::Plan>,
    /// Process-tree sampling cadence; `None` runs no sampler. The launcher
    /// resolves it: the guardian starts with an empty environment and cannot
    /// read host or project config.
    pub resource_sample_interval: Option<std::time::Duration>,
    pub cwd: PathBuf,
    pub stdin: Vec<u8>,
    pub secret_input: Option<provenant_os::exec::SecretInput>,
    pub publication_values: provenant_os::exec::SecretValues,
    /// Opaque, bounded reference for the composed terminal holder. It travels
    /// on the handoff pipe only and is never written to the launch summary.
    pub holder_context: Vec<u8>,
}
pub const HOLDER_CONTEXT_LIMIT: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamMode {
    Capture {
        limit: u64,
    },
    /// Retain the latest bytes while draining the entire stream.
    Tail {
        limit: u64,
    },
    Discard,
    Tee {
        limit: u64,
    },
}
impl StreamMode {
    pub(crate) fn piped(self) -> bool {
        !matches!(self, Self::Discard)
    }
    pub(crate) fn limit(self) -> u64 {
        match self {
            Self::Capture { limit } | Self::Tail { limit } | Self::Tee { limit } => limit,
            Self::Discard => 0,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePlan {
    pub stdout: StreamMode,
    pub stderr: StreamMode,
    pub wall_ms: u64,
    pub descendant_grace_ms: u64,
    pub root_flush_ms: u64,
    /// Tokens the attempt may use, from the usage its provider reports while
    /// it runs. 0 is no budget.
    #[serde(default)]
    pub token_budget: u64,
    /// The resolved budget action; warnings never authorise a stop.
    #[serde(default)]
    pub budget_action: provenant_records::BudgetAction,
    /// Context size, in tokens one provider call sent, at which a writer is
    /// stopped so a fresh attempt continues from its handoff. 0 is off; a
    /// provider that reports no context figure is never stopped for it.
    #[serde(default)]
    pub context_handoff: u64,
    /// How long a guardian retains custody of an unresolved exit before it
    /// records why and exits, leaving the durable fence to reconcile. The
    /// launcher resolves it from `domains.run.retained_custody_seconds`.
    #[serde(default = "default_retained_custody_ms")]
    pub retained_custody_ms: u64,
}
/// One hour of retained custody: about 120 bounded stops at the 30 s
/// escalation, far beyond any orderly service shutdown, while an orphaned
/// guardian no longer holds a lane for hours (#3572 held one for 2h43m).
pub const DEFAULT_RETAINED_CUSTODY_SECONDS: u64 = 3_600;
/// The largest retention limit a plan may carry: one week.
pub const MAX_RETAINED_CUSTODY_SECONDS: u64 = 7 * 24 * 3_600;
fn default_retained_custody_ms() -> u64 {
    DEFAULT_RETAINED_CUSTODY_SECONDS * 1_000
}
impl Default for CapturePlan {
    fn default() -> Self {
        Self {
            stdout: StreamMode::Capture {
                limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT as u64,
            },
            stderr: StreamMode::Capture {
                limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT as u64,
            },
            wall_ms: 3600000,
            descendant_grace_ms: crate::machine::DEFAULT_DESCENDANT_GRACE_MS,
            root_flush_ms: crate::machine::DEFAULT_ROOT_FLUSH_MS,
            token_budget: 0,
            budget_action: provenant_records::BudgetAction::default(),
            context_handoff: 0,
            retained_custody_ms: default_retained_custody_ms(),
        }
    }
}
pub struct OwnedLaunch<'a> {
    pub command: ProviderCommand,
    pub lease: &'a mut CustodyLease,
    pub handshake: WaitBudget,
    pub cancel: &'a CancelToken,
    pub clock: &'a dyn Clock,
}
/// Read/control capability for one generation, never a numeric process grant.
#[derive(Debug)]
pub struct CustodyHandle {
    pub(crate) directory: Directory,
    pub(crate) scope: Scope,
    pub(crate) requester: ActorId,
    /// Built from the caller's own scope, not from the attempt directory.
    pub(crate) held: bool,
}
impl CustodyHandle {
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Observe guardian exit using the custody identity captured at hand-off.
    /// Missing, replaced or unobservable locks retain the handle. The probe
    /// opens through the held directory and releases its lock on return.
    pub fn custody_released(&self, identity: &Metadata) -> bool {
        self.directory
            .try_lock_existing(OsStr::new("custody.lock"), LockMode::Exclusive)
            .and_then(|guard| guard.metadata())
            .is_ok_and(|metadata| metadata.same_identity(identity))
    }
}
pub fn launch(
    owned: OwnedLaunch<'_>,
    capture: CapturePlan,
    publication: PublicationReservation,
) -> std::result::Result<CustodyHandle, LaunchFailure> {
    let failure = |error, custody| LaunchFailure {
        error: Box::new(error),
        custody,
        pending_exit: None,
    };
    // One lease launches at most once, whatever the previous outcome.
    if owned.lease.phase != LaunchPhase::Unlaunched {
        return Err(failure(CustodyError::Fenced, publication));
    }
    let scope = owned.lease.scope();
    let requester = owned.command.requester;
    let metadata = match publication.directory.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return Err(failure(error.into(), publication)),
    };
    owned.lease.reservation.attempt_device = Some(metadata.device);
    owned.lease.reservation.attempt_inode = Some(metadata.inode);
    owned.lease.reservation.attempt_path = match publication.directory.current_path() {
        Ok(path) => Some(path),
        Err(error) => return Err(failure(error.into(), publication)),
    };
    owned.lease.reservation.requester = Some(requester);
    let invalid_capture = capture.validate().err();
    if owned.cancel.is_cancelled() || invalid_capture.is_some() {
        // No guardian is contacted: the failure carries its own cause and the
        // never-spawned exit, so the caller withdraws instead of fencing.
        let error = invalid_capture.unwrap_or(CustodyError::InvalidRecord);
        return Err(refuse_never_spawned(
            owned.lease,
            owned.cancel,
            publication,
            error,
        ));
    }
    // A host without provider custody refuses before the reservation records
    // a handoff: the lease stays unlaunched, so the caller withdraws it rather
    // than fencing an attempt whose launch can never be confirmed. A launch
    // that never spawns needs no custody and settles above on every host.
    #[cfg(not(target_os = "macos"))]
    if crate::process_os::provider::ensure_supported().is_err() {
        return Err(failure(CustodyError::UnsupportedPlatform, publication));
    }
    // Death evidence precedes every native guardian process. A crash during
    // preparation leaves Prepared custody, which has no child to fence.
    let prepared = match (owned.lease.lease.as_ref(), owned.lease.holder.as_ref()) {
        (Some(writer), Some(holder)) => liveness::prepare_guardian_lifetime(
            &owned.lease.directory,
            &publication.directory,
            scope,
            writer,
            &publication.custody,
            holder,
        ),
        _ => Err(CustodyError::Fenced),
    };
    if let Err(error) = prepared {
        return Err(failure(error, publication));
    }
    owned.lease.phase = LaunchPhase::HandedOff;
    owned.lease.reservation.launch_state = LaunchState::HandingOff;
    if let Err(error) = write_record(
        &owned.lease.directory,
        "reservation.json",
        &owned.lease.reservation,
        PublishMode::Replace,
    )
    .and_then(|publication| publication.require_durable().map_err(Into::into))
    {
        return Err(failure(error, publication));
    }
    let OwnedLaunch {
        command,
        lease,
        handshake,
        cancel,
        clock,
    } = owned;
    let handed = crate::guardian::handoff(
        OwnedLaunch {
            command,
            lease: &mut *lease,
            handshake,
            cancel,
            clock,
        },
        capture,
        &publication,
    );
    match handed {
        Ok(()) => Ok(CustodyHandle {
            directory: publication.directory,
            scope,
            requester,
            held: false,
        }),
        // Only a guardian holding the whole message may have launched.
        Err(HandoffFailure::Unconfirmed(error)) => Err(failure(error, publication)),
        Err(HandoffFailure::Undelivered(error)) => {
            Err(refuse_never_spawned(lease, cancel, publication, error))
        }
    }
}

/// Settle a launch no guardian accepted. The reservation records
/// `NeverSpawned` and the failure carries the never-spawned exit, so the
/// caller withdraws instead of fencing. A cancel request names the stop.
fn refuse_never_spawned(
    lease: &mut CustodyLease,
    cancel: &CancelToken,
    publication: PublicationReservation,
    error: CustodyError,
) -> LaunchFailure {
    let scope = lease.scope();
    lease.reservation.launch_state = LaunchState::NeverSpawned;
    lease.reservation.unresolved_cleanup = false;
    lease.reservation.census_incomplete = false;
    lease.phase = LaunchPhase::NeverSpawned;
    let (reason, error) = if cancel.is_cancelled() {
        (
            Some(StopReason::ExplicitCancellation),
            CustodyError::Cancelled,
        )
    } else {
        (None, error)
    };
    let failure = LaunchFailure {
        error: Box::new(error),
        custody: publication,
        pending_exit: Some(Box::new(ProcessExit::never_spawned(scope, reason, vec![]))),
    };
    // A failed record leaves the exit pending for the caller's retry.
    let _ = write_record(
        &lease.directory,
        "reservation.json",
        &lease.reservation,
        PublishMode::Replace,
    )
    .and_then(|publication| publication.require_durable().map_err(Into::into))
    .and_then(|()| failure.retry_exit_publication())
    .and_then(|publication| publication.require_durable().map_err(Into::into));
    failure
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopIntent {
    pub schema_version: u64,
    pub scope: Scope,
    pub request_id: provenant_core::OperationId,
    pub requester: ActorId,
    pub reason: StopReason,
    #[serde(flatten)]
    pub extra: ExtraFields,
}
/// One interpretation of every host stop. Only explicit cancellation may live
/// in `cancel.json`; a steer there is invalid, never continuation authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopIntentState {
    Absent,
    Requested(StopIntent),
    Invalid,
}

pub fn read_stop_intent(directory: &Directory, scope: Scope) -> Result<StopIntentState> {
    for (file, cancel_only) in [("cancel.json", true), ("stop-intent.json", false)] {
        let request = match read_record::<StopIntent>(directory, file, RECORD_LIMIT) {
            Ok(request) => request,
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => continue,
            Err(error) if error.is_invalid_record() => return Ok(StopIntentState::Invalid),
            Err(error) => return Err(error),
        };
        if request.schema_version != 1
            || request.scope != scope
            || (cancel_only && request.reason != StopReason::ExplicitCancellation)
            || (!cancel_only
                && request.reason == StopReason::Steered
                && request.request_id != stop_operation(scope)?)
        {
            return Ok(StopIntentState::Invalid);
        }
        return Ok(StopIntentState::Requested(request));
    }
    Ok(StopIntentState::Absent)
}

fn stop_operation(scope: Scope) -> Result<provenant_core::OperationId> {
    provenant_core::OperationId::parse(&format!("{}.{}.stop", scope.attempt, scope.generation))
        .map_err(|_| CustodyError::InvalidRecord)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReceipt {
    Requested,
    AlreadyRequested,
}
/// Record a stop during the handoff window without waiting for launch.json.
/// The host supplies the already authorised scope and requester, never a PID.
pub fn request_stop(
    directory: Directory,
    scope: Scope,
    requester: ActorId,
    reason: StopReason,
) -> Result<StopReceipt> {
    stop(
        &CustodyHandle {
            directory,
            scope,
            requester,
            held: false,
        },
        reason,
    )
}

pub fn stop(handle: &CustodyHandle, reason: StopReason) -> Result<StopReceipt> {
    stop_inner(handle, reason, false)
}

/// Request a stop without replacing a different, already recorded stop.
pub fn stop_if_unrequested(handle: &CustodyHandle, reason: StopReason) -> Result<StopReceipt> {
    stop_inner(handle, reason, true)
}

fn stop_inner(
    handle: &CustodyHandle,
    reason: StopReason,
    preserve_existing: bool,
) -> Result<StopReceipt> {
    let request = StopIntent {
        schema_version: 1,
        scope: handle.scope,
        request_id: provenant_core::OperationId::parse(&format!(
            "{}.{}.stop",
            handle.scope.attempt, handle.scope.generation
        ))
        .map_err(|_| CustodyError::InvalidRecord)?,
        requester: handle.requester,
        reason,
        extra: ExtraFields::default(),
    };
    match write_record(
        &handle.directory,
        "stop-intent.json",
        &request,
        PublishMode::NoReplace,
    ) {
        Ok(publication) => {
            Publication::require_durable(publication)?;
            Ok(StopReceipt::Requested)
        }
        Err(CustodyError::Filesystem(error)) if error.is_exists() => {
            match read_record::<StopIntent>(&handle.directory, "stop-intent.json", RECORD_LIMIT) {
                Ok(previous) if previous == request => return Ok(StopReceipt::AlreadyRequested),
                Ok(_) if preserve_existing => return Err(CustodyError::StopConflict),
                Ok(previous)
                    if request.reason == StopReason::Steered
                        && previous.reason != StopReason::Steered =>
                {
                    return Err(CustodyError::StopConflict);
                }
                // A stale reattached handle never displaces a later
                // generation's request. A held scope is the guardian's own,
                // so a later generation beside it can only be forged.
                Ok(previous)
                    if !handle.held
                        && previous.scope.attempt == request.scope.attempt
                        && previous.scope.generation > request.scope.generation =>
                {
                    return Err(CustodyError::InvalidRecord);
                }
                Ok(_) => {}
                Err(error) if error.is_invalid_record() => {}
                // A symlink, FIFO or oversize entry is replaced too: rename
                // swaps the entry without following or opening it. A
                // directory still refuses the rename.
                Err(_) if handle.held => {}
                Err(error) => return Err(error),
            }
            // The guardian honours only its own scope and requester, so a
            // stale, foreign or undecodable request would leave it running.
            // Replace it and confirm the replacement is the one now visible.
            Publication::require_durable(write_record(
                &handle.directory,
                "stop-intent.json",
                &request,
                PublishMode::Replace,
            )?)?;
            let visible: StopIntent =
                read_record(&handle.directory, "stop-intent.json", RECORD_LIMIT)?;
            if visible != request {
                return Err(CustodyError::InvalidRecord);
            }
            Ok(StopReceipt::Requested)
        }
        Err(error) => Err(error),
    }
}
/// A failed attempt to deliver a signal to a pinned provider identity.
#[derive(Clone, Debug, Serialize)]
pub struct FailedSignalDelivery {
    pub identity: Option<ProcessIdentity>,
    pub reason: StopReason,
    pub signal: i64,
    pub failure: crate::machine::DeliveryFailure,
}
/// The observation that permitted cleanup settlement, never a signal authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Darwin cannot prove a nested owner's flock custody with public libproc.
/// Registered nested owners remain protected and can leave settlement unresolved.
pub enum SettlementEvidence {
    #[default]
    Unresolved,
    NeverSpawned,
    DarwinCensusAndCustodyLease,
    /// Recovery found the guardian gone and every named leftover process
    /// absent or ended after proving it belonged to the attempt.
    OrphansEnded,
    /// Recovery settled the attempt but named processes it could not prove
    /// belong to it. They were never signalled and stay in `unresolved_pids`.
    LeftoverNotProven,
}
/// Why a guardian retains custody of an unresolved exit, and how that ended.
/// Status names the holder and its age from this record. Reconcile acts on
/// the unresolved exit once the guardian is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retention {
    /// The guardian that holds the writer lease while it retains custody.
    pub guardian: ProcessIdentity,
    /// When retention began; None when the host clock was unreadable.
    pub since: Option<provenant_core::WallTime>,
    /// The limit the launcher resolved (`domains.run.retained_custody_seconds`).
    pub limit_ms: u64,
    /// Set when the guardian stopped retaining with the exit still unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<RetentionEnd>,
}
impl Retention {
    /// How long ago retention began, as status prints ages.
    pub fn age(&self, now: provenant_core::WallTime) -> Option<String> {
        self.since
            .map(|since| format_age_ns(i128::from(since.as_micros()) * 1_000, now))
    }
    /// The status clause for this record. Only a census that shows the
    /// recorded guardian identity alive proves it still holds the lane.
    pub fn describe(
        &self,
        guardian: GuardianLiveness,
        now: Option<provenant_core::WallTime>,
    ) -> String {
        let pid = self.guardian.pid;
        if let Some(end) = &self.ended {
            return format!(
                "guardian pid {pid} stopped retaining after {} s ({}): {}; run `provenant reconcile`",
                end.retained_ms / 1_000,
                match end.reason {
                    RetentionEndReason::LimitReached => "limit reached",
                    RetentionEndReason::SupervisorSignal => "supervisor signal",
                },
                end.blocker,
            );
        }
        match (guardian, now.and_then(|now| self.age(now))) {
            (GuardianLiveness::Live, Some(age)) => format!("held by guardian pid {pid} for {age}"),
            (GuardianLiveness::Live, None) => format!("held by guardian pid {pid}"),
            (GuardianLiveness::Gone, _) => format!(
                "guardian pid {pid} is gone with the exit unresolved; run `provenant reconcile`"
            ),
            (GuardianLiveness::Unknown, _) => {
                format!("guardian pid {pid} retained custody; its liveness is unknown")
            }
        }
    }
}
/// Whether a census shows a retention record's guardian identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardianLiveness {
    /// The exact recorded identity is present and not a zombie.
    Live,
    /// The PID is absent or missing from a whole listing, a zombie, or reused
    /// by another process.
    Gone,
    /// No census, a partial listing, or a PID the census could not describe.
    Unknown,
}
impl GuardianLiveness {
    pub fn observe(census: Option<&crate::census::Census>, guardian: &ProcessIdentity) -> Self {
        use crate::census::ProcessLookup;
        let Some(census) = census else {
            return Self::Unknown;
        };
        match census.process(guardian.pid) {
            ProcessLookup::Present(fact) if fact.identity == *guardian && !fact.zombie => {
                Self::Live
            }
            ProcessLookup::Present(_) | ProcessLookup::Absent => Self::Gone,
            // A whole listing that omits the PID shows it was not running.
            // This only words status; it never grants settlement authority.
            ProcessLookup::Unknown
                if census.listing_is_complete()
                    && !census.unknown_pids().contains_key(&guardian.pid) =>
            {
                Self::Gone
            }
            ProcessLookup::Unknown => Self::Unknown,
        }
    }
}
/// Why retained custody ended while processes were still unresolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionEnd {
    pub reason: RetentionEndReason,
    /// How long the guardian retained custody, in milliseconds.
    pub retained_ms: u64,
    /// What kept the exit unresolved.
    pub blocker: RetentionBlocker,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionEndReason {
    /// Retention reached `Retention::limit_ms`.
    LimitReached,
    /// TERM, INT or HUP asked the guardian to stop.
    SupervisorSignal,
}
/// What kept an exit unresolved when retention ended. Diagnostic only: it
/// grants no signal authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionBlocker {
    /// The last process census was incomplete.
    CensusIncomplete,
    /// A registered nested owner that could not be proven stays protected.
    ProtectedOwner,
    /// The run registry or host graph forbade signalling descendants.
    StopRefused,
    /// Tracked processes outlived TERM and KILL.
    SurvivedStop,
    /// No tracked process remains, but group or writer-lease quiescence is unproven.
    SettlementUnproven,
}
impl std::fmt::Display for RetentionBlocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CensusIncomplete => "the process census was incomplete",
            Self::ProtectedOwner => "an unproven nested owner stays protected",
            Self::StopRefused => "the run registry or host graph forbade signalling descendants",
            Self::SurvivedStop => "tracked processes outlived TERM and KILL",
            Self::SettlementUnproven => "group or writer-lease quiescence is unproven",
        })
    }
}
/// A guardian that reached its retention limit with processes unresolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionLimitReached {
    pub scope: Scope,
    pub limit_ms: u64,
    pub end: RetentionEnd,
    pub unresolved_pids: Vec<i64>,
}
impl std::fmt::Display for RetentionLimitReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "retained custody of attempt {} reached its {} s limit after {} s: {}; unresolved pids {:?}; the exit stays unresolved and the writer stays fenced; run `provenant reconcile` for the attempt",
            self.scope.attempt,
            self.limit_ms / 1_000,
            self.end.retained_ms / 1_000,
            self.end.blocker,
            self.unresolved_pids,
        )
    }
}
/// Bounded, immutable OS facts. No field certifies its own publication durability.
#[derive(Clone, Debug, Serialize)]
pub struct ProcessExit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    pub schema_version: u64,
    pub scope: Scope,
    pub termination: Option<Termination>,
    pub never_spawned: bool,
    pub stop_reason: Option<StopReason>,
    /// Whether cancellation or a steering interrupt was delivered before exit;
    /// null means ordering was not observed.
    pub cancel_delivered_before_exit: Option<bool>,
    /// The live provider stream contradicted its admitted identity, sealed by
    /// the guardian independently of the bounded retained stdout tail.
    #[serde(default)]
    pub provider_identity_conflict: bool,
    /// Live provider identity evidence was incomplete because a
    /// delimiter-complete frame was dropped or undecodable, output was
    /// oversized or unreadable, or capture drain timed out.
    #[serde(default)]
    pub provider_identity_evidence_incomplete: bool,
    /// The live decoder saw an incomplete provider record in the residual
    /// bytes delivered at actual EOF. This is separate from other loss.
    #[serde(default)]
    pub provider_identity_eof_fragment: bool,
    pub delivered_signals: Vec<i64>,
    pub reaped: Vec<ProcessIdentity>,
    pub omitted_reaped: u64,
    pub spared: Vec<ProcessIdentity>,
    pub omitted_spared: u64,
    pub failed_deliveries: Vec<FailedSignalDelivery>,
    pub omitted_failed_deliveries: u64,
    pub terminal_observed: bool,
    pub unresolved_cleanup: bool,
    /// PIDs of tracked descendants still possibly alive when cleanup gave up.
    pub unresolved_pids: Vec<i64>,
    /// The same descendants with boot and native start time, so recovery after
    /// the guardian is gone can prove a PID still names the process found.
    pub unresolved_identities: Vec<ProcessIdentity>,
    /// Processes observed at the cleanup deadline with the writer lock path
    /// still open. Descriptor occupancy is diagnostic only: it never grants
    /// signal or settlement authority, so a holder found only by its open
    /// descriptor is marked unproven.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_holders: Vec<ObservedHolder>,
    pub settlement_evidence: SettlementEvidence,
    /// Present while a guardian retains custody of this unresolved exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    pub stdout: CaptureTotals,
    pub stderr: CaptureTotals,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_refusal: Option<provenant_records::diagnostic::ResourceFact>,
    pub warnings: Vec<String>,
    pub omitted_warnings: u64,
    #[serde(flatten)]
    pub extra: ExtraFields,
}
/// One process observed holding a lock resource at the cleanup deadline.
/// `ownership_proven` is true only for an exact recorded descendant; a process
/// found by its open descriptor is named for the owner but never signalled on
/// that evidence alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedHolder {
    pub pid: i64,
    #[serde(default)]
    pub command: String,
    pub resource: String,
    #[serde(default)]
    pub ownership_proven: bool,
}

impl ProcessExit {
    /// Construct the guardian's pre-spawn admission evidence in one owner so
    /// retaining its resource fact cannot depend on an adapter's prose.
    pub fn admission_refused(scope: Scope, error: &CustodyError) -> Self {
        let mut exit = Self::never_spawned(scope, None, vec![admission_refusal(error)]);
        exit.launch_refusal = match error {
            CustodyError::Admission(error) => error.resource_fact(),
            _ => None,
        };
        exit
    }
    pub(crate) fn never_spawned(
        scope: Scope,
        stop_reason: Option<StopReason>,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            priority: None,
            schema_version: 1,
            scope,
            termination: None,
            never_spawned: true,
            stop_reason,
            cancel_delivered_before_exit: Some(false),
            provider_identity_conflict: false,
            provider_identity_evidence_incomplete: false,
            provider_identity_eof_fragment: false,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            failed_deliveries: vec![],
            omitted_failed_deliveries: 0,
            terminal_observed: false,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            unresolved_identities: vec![],
            observed_holders: vec![],
            settlement_evidence: SettlementEvidence::NeverSpawned,
            retention: None,
            stdout: CaptureTotals::default(),
            stderr: CaptureTotals::default(),
            launch_refusal: None,
            warnings,
            omitted_warnings: 0,
            extra: ExtraFields::default(),
        }
    }

    /// The observation of a record this guardian has just published durably.
    pub(crate) fn durable_observation(&self) -> ExitObservation {
        ExitObservation {
            publication: PublicationState::Durable,
            scope: self.scope,
            termination: self.termination,
            never_spawned: self.never_spawned,
            stop_reason: self.stop_reason,
            cancel_delivered_before_exit: self.cancel_delivered_before_exit,
            provider_identity_conflict: self.provider_identity_conflict,
            provider_identity_evidence_incomplete: self.provider_identity_evidence_incomplete,
            provider_identity_eof_fragment: self.provider_identity_eof_fragment,
            delivered_signals: self.delivered_signals.clone(),
            reaped: self.reaped.clone(),
            omitted_reaped: self.omitted_reaped,
            spared: self.spared.clone(),
            omitted_spared: self.omitted_spared,
            launch_refusal: self.launch_refusal.clone(),
            warnings: self.warnings.clone(),
            omitted_warnings: self.omitted_warnings,
            terminal_observed: self.terminal_observed,
            unresolved_cleanup: self.unresolved_cleanup,
            unresolved_pids: self.unresolved_pids.clone(),
            settlement_evidence: self.settlement_evidence,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        }
    }
    pub(crate) fn bounded_bytes(&self) -> Result<Vec<u8>> {
        let mut bounded = self.clone();
        loop {
            let bytes = serde_json::to_vec(&bounded).map_err(|_| CustodyError::InvalidRecord)?;
            if bytes.len() as u64 <= EXIT_LIMIT {
                return Ok(bytes);
            }
            if bounded.warnings.pop().is_some() {
                bounded.omitted_warnings = bounded.omitted_warnings.saturating_add(1);
            } else if bounded.failed_deliveries.pop().is_some() {
                bounded.omitted_failed_deliveries =
                    bounded.omitted_failed_deliveries.saturating_add(1);
            } else if bounded.spared.pop().is_some() {
                bounded.omitted_spared = bounded.omitted_spared.saturating_add(1);
            } else if bounded.reaped.pop().is_some() {
                bounded.omitted_reaped = bounded.omitted_reaped.saturating_add(1);
            } else {
                return Err(CustodyError::InvalidRecord);
            }
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CaptureTotals {
    pub bytes: u64,
    pub retained: u64,
    pub oversized_frames: u64,
    pub decoder_lost_frames: u64,
    pub lost_bytes: u64,
    pub read_failed: bool,
    #[serde(flatten)]
    pub extra: ExtraFields,
}
impl From<&CapturedStream> for CaptureTotals {
    fn from(value: &CapturedStream) -> Self {
        Self {
            bytes: value.total_bytes,
            retained: value.bytes.len() as u64,
            oversized_frames: value.oversized_frames,
            decoder_lost_frames: value.decoder_lost_frames,
            lost_bytes: value.sink_lost_bytes,
            read_failed: value.read_failed,
            extra: ExtraFields::default(),
        }
    }
}
// Current storage DTO permits future additive fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredExit {
    pub schema_version: u64,
    #[serde(default)]
    pub priority: Option<i32>,
    pub scope: Scope,
    pub termination: Option<Termination>,
    pub never_spawned: bool,
    pub stop_reason: Option<StopReason>,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub cancel_delivered_before_exit: Option<bool>,
    #[serde(default)]
    pub provider_identity_conflict: bool,
    #[serde(default)]
    pub provider_identity_evidence_incomplete: bool,
    #[serde(default)]
    pub provider_identity_eof_fragment: bool,
    pub delivered_signals: Vec<i64>,
    pub reaped: Vec<ProcessIdentity>,
    pub omitted_reaped: u64,
    pub spared: Vec<ProcessIdentity>,
    pub omitted_spared: u64,
    #[serde(default)]
    pub launch_refusal: Option<provenant_records::diagnostic::ResourceFact>,
    pub warnings: Vec<String>,
    pub omitted_warnings: u64,
    pub terminal_observed: bool,
    pub unresolved_cleanup: bool,
    pub unresolved_pids: Vec<i64>,
    #[serde(default)]
    pub unresolved_identities: Vec<ProcessIdentity>,
    pub settlement_evidence: SettlementEvidence,
    #[serde(default)]
    pub retention: Option<Retention>,
    pub stdout: CaptureTotals,
    pub stderr: CaptureTotals,
    #[serde(flatten)]
    pub extra: ExtraFields,
}
#[derive(Clone, Debug, Serialize)]
pub struct ExitObservation {
    pub publication: PublicationState,
    pub scope: Scope,
    pub termination: Option<Termination>,
    pub never_spawned: bool,
    pub stop_reason: Option<StopReason>,
    /// None means this current observation could not establish delivery ordering.
    pub cancel_delivered_before_exit: Option<bool>,
    pub provider_identity_conflict: bool,
    pub provider_identity_evidence_incomplete: bool,
    pub provider_identity_eof_fragment: bool,
    pub delivered_signals: Vec<i64>,
    pub reaped: Vec<ProcessIdentity>,
    pub omitted_reaped: u64,
    pub spared: Vec<ProcessIdentity>,
    pub omitted_spared: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_refusal: Option<provenant_records::diagnostic::ResourceFact>,
    pub warnings: Vec<String>,
    pub omitted_warnings: u64,
    pub terminal_observed: bool,
    pub unresolved_cleanup: bool,
    /// PIDs of tracked descendants still possibly alive when cleanup gave up.
    pub unresolved_pids: Vec<i64>,
    pub settlement_evidence: SettlementEvidence,
    pub stdout: CaptureTotals,
    pub stderr: CaptureTotals,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    #[default]
    Unknown,
    Durable,
    VisibleDurabilityUnconfirmed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredObservation {
    schema_version: u64,
    scope: Scope,
    guardian: Option<ProcessIdentity>,
    #[serde(default)]
    provider: Option<ProcessIdentity>,
    #[serde(default)]
    descendants: Vec<ProcessIdentity>,
    /// Older checkpoints cannot certify a full process-tree snapshot.
    #[serde(default)]
    census_complete: bool,
    publication_pending: bool,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(flatten)]
    extra: ExtraFields,
}

/// Read-only process presence for the exact identities recorded by one
/// attempt's supervisor. An unknown observation never means the holder died.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HolderPresence {
    Live,
    Gone,
    Unknown,
}

/// Observe the guardian and provider recorded for this exact attempt. Exact
/// identities can prove a live holder, and a changed boot proves the old tree
/// is gone. Same-boot absence stays unknown: the last durable descendant
/// checkpoint can predate a fork into another session or process group.
/// Permission, malformed evidence, incomplete census and any remaining session
/// member also stay unknown.
pub fn observe_holder(
    directory: &Directory,
    scope: Scope,
    observer: &dyn ProcessObserver,
) -> HolderPresence {
    let record = match read_record::<StoredObservation>(directory, "process.json", RECORD_LIMIT) {
        Ok(record) => record,
        // Running state without its scoped native checkpoint cannot prove that
        // no writer survived; a missing file is not a liveness observation.
        Err(_) => return HolderPresence::Unknown,
    };
    observe_holder_record(scope, observer, record, None)
}

/// The bounded census-taking wrapper for one exact supervisor checkpoint.
/// Batch status callers can instead share one host census across all rows.
pub fn observe_holder_with_census(
    directory: &Directory,
    scope: Scope,
    observer: &dyn ProcessObserver,
    census: &crate::census::Census,
) -> HolderPresence {
    let record = match read_record::<StoredObservation>(directory, "process.json", RECORD_LIMIT) {
        Ok(record) => record,
        Err(_) => return HolderPresence::Unknown,
    };
    observe_holder_record(scope, observer, record, Some(census))
}

fn observe_holder_record(
    scope: Scope,
    observer: &dyn ProcessObserver,
    record: StoredObservation,
    census: Option<&crate::census::Census>,
) -> HolderPresence {
    if record.schema_version != 1 || record.scope != scope || record.publication_pending {
        return HolderPresence::Unknown;
    }
    let Some(guardian) = record.guardian.as_ref() else {
        return HolderPresence::Unknown;
    };
    let mut identities = vec![guardian];
    if let Some(provider) = record.provider.as_ref() {
        identities.push(provider);
    }
    identities.extend(record.descendants.iter());
    if identities.iter().any(|identity| {
        identity.validate().is_err() || identity.native_start == 0 || identity.boot != guardian.boot
    }) {
        return HolderPresence::Unknown;
    }
    let boot = match observer.boot_identity() {
        Ok(boot) if !boot.is_empty() => boot,
        _ => return HolderPresence::Unknown,
    };
    if boot != guardian.boot {
        return HolderPresence::Gone;
    }
    let mut unknown = false;
    let root_count = identities.len() - record.descendants.len();
    for (index, expected) in identities.into_iter().enumerate() {
        match observer.observe(expected.pid) {
            ProcessObservation::Present(fact)
                if fact.validate().is_ok() && fact.identity == *expected && !fact.zombie =>
            {
                return if index < root_count {
                    HolderPresence::Live
                } else {
                    HolderPresence::Unknown
                };
            }
            ProcessObservation::Present(_) | ProcessObservation::Absent => {}
            ProcessObservation::Unknown(_) => unknown = true,
        }
    }
    if unknown {
        return HolderPresence::Unknown;
    }
    if !record.census_complete {
        return HolderPresence::Unknown;
    }
    if let Some(census) = census {
        classify_holder_census(&record, census)
    } else {
        match observer.census(crate::census::MAX_CENSUS_ENTRIES) {
            Ok(census) => classify_holder_census(&record, &census),
            Err(_) => HolderPresence::Unknown,
        }
    }
}

fn classify_holder_census(
    record: &StoredObservation,
    census: &crate::census::Census,
) -> HolderPresence {
    use crate::census::ProcessLookup;
    let Some(guardian) = record.guardian.as_ref() else {
        return HolderPresence::Unknown;
    };
    if !census.is_complete() {
        return HolderPresence::Unknown;
    }
    let mut owned_sessions = vec![guardian.session_id];
    let mut owned_groups = vec![guardian.group_id];
    if let Some(provider) = record.provider.as_ref() {
        owned_sessions.push(provider.session_id);
        owned_groups.push(provider.group_id);
    }
    if census.processes().values().any(|fact| {
        !fact.zombie
            && (owned_sessions.contains(&fact.identity.session_id)
                || owned_groups.contains(&fact.identity.group_id))
    }) {
        return HolderPresence::Unknown;
    }
    // A complete current census can confirm that recorded identities and
    // sessions are absent, but it cannot cover a child forked after the
    // asynchronously published checkpoint and escaped from those sessions.
    // Same-boot absence is therefore not quiescence authority.
    for pid in
        std::iter::once(guardian.pid).chain(record.provider.as_ref().map(|provider| provider.pid))
    {
        if matches!(census.process(pid), ProcessLookup::Unknown) {
            return HolderPresence::Unknown;
        }
    }
    HolderPresence::Unknown
}

/// A reboot after both identities were recorded proves the entire old process
/// tree is gone, including descendants which escaped the provider's group.
pub fn checkpoint_before_reboot(
    directory: &Directory,
    scope: Scope,
    observer: &dyn ProcessObserver,
) -> bool {
    let Ok(record) = read_record::<StoredObservation>(directory, "process.json", RECORD_LIMIT)
    else {
        return false;
    };
    let (Some(guardian), Some(provider)) = (record.guardian, record.provider) else {
        return false;
    };
    record.schema_version == 1
        && record.scope == scope
        && !record.publication_pending
        && guardian.validate().is_ok()
        && provider.validate().is_ok()
        && guardian.boot == provider.boot
        && guardian.session_id == guardian.pid
        && guardian.group_id == guardian.pid
        && provider.session_id == guardian.session_id
        && provider.group_id == provider.pid
        && observer
            .boot_identity()
            .is_ok_and(|boot| !boot.is_empty() && boot != guardian.boot)
}
/// Whether a changed boot proves that the guardian and provider recorded for
/// this attempt are gone. Same-boot absence is unknown; unreadable process or
/// record evidence also leaves the attempt open.
pub fn holder_gone(directory: &Directory, scope: Scope, observer: &dyn ProcessObserver) -> bool {
    observe_holder(directory, scope, observer) == HolderPresence::Gone
}
/// Whether supervisor evidence proves this attempt can no longer hold custody.
/// Callers must keep writer exclusion while acting on this proof. A completed
/// exit is sufficient on the same boot only when cleanup finished without
/// unresolved processes or retained custody; an exit still settling is not.
pub fn attempt_quiescent(
    directory: &Directory,
    scope: Scope,
    observer: &dyn ProcessObserver,
) -> bool {
    match read_record::<StoredExit>(directory, "process-exit.json", EXIT_LIMIT) {
        Ok(exit) => {
            exit.schema_version == 1
                && exit.scope == scope
                && !exit.unresolved_cleanup
                && exit.unresolved_pids.is_empty()
                && exit.unresolved_identities.is_empty()
                && exit.retention.is_none()
                && match exit.settlement_evidence {
                    SettlementEvidence::NeverSpawned => exit.never_spawned,
                    SettlementEvidence::DarwinCensusAndCustodyLease
                    | SettlementEvidence::OrphansEnded => !exit.never_spawned,
                    SettlementEvidence::Unresolved | SettlementEvidence::LeftoverNotProven => false,
                }
                && match read_record::<StoredObservation>(directory, "process.json", RECORD_LIMIT) {
                    Ok(record)
                        if record.schema_version == 1
                            && record.scope == scope
                            && !record.publication_pending =>
                    {
                        record
                            .guardian
                            .iter()
                            .chain(record.provider.iter())
                            .chain(record.descendants.iter())
                            .all(|identity| match observer.observe(identity.pid) {
                                ProcessObservation::Present(fact) => {
                                    fact.identity != *identity || fact.zombie
                                }
                                ProcessObservation::Absent => true,
                                ProcessObservation::Unknown(_) => false,
                            })
                    }
                    Err(CustodyError::Filesystem(error)) if error.is_not_found() => true,
                    _ => false,
                }
        }
        Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
            match read_record::<StoredObservation>(directory, "process.json", RECORD_LIMIT) {
                Err(CustodyError::Filesystem(error)) if error.is_not_found() => true,
                Ok(_) => holder_gone(directory, scope, observer),
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}
/// Whether any of these PIDs may still be alive (a signal-0 probe, no effect).
pub fn any_pid_alive(pids: &[i64]) -> bool {
    crate::process_os::provider::any_pid_alive(pids)
}
#[derive(Debug)]
pub enum Observation {
    Live,
    PublicationPending,
    Exited(Box<ExitObservation>),
    Unknown,
}
// Custody observation must expire even if the injected clock stops advancing.
#[allow(clippy::disallowed_methods)]
pub fn observe(
    handle: &CustodyHandle,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<Observation> {
    let remaining = Duration::from_micros(budget.remaining(clock.mono_now()).as_micros());
    let deadline = Instant::now()
        .checked_add(remaining)
        .ok_or(CustodyError::InvalidRecord)?;
    loop {
        if cancel.is_cancelled() {
            return Err(CustodyError::Cancelled);
        }
        match read_record::<StoredExit>(&handle.directory, "process-exit.json", EXIT_LIMIT) {
            Ok(record) if record.schema_version == 1 && record.scope == handle.scope => {
                let publication = if handle.directory.sync().is_ok() {
                    PublicationState::Durable
                } else {
                    PublicationState::VisibleDurabilityUnconfirmed
                };
                return Ok(Observation::Exited(Box::new(ExitObservation {
                    publication,
                    scope: record.scope,
                    termination: record.termination,
                    never_spawned: record.never_spawned,
                    stop_reason: record.stop_reason,
                    cancel_delivered_before_exit: record.cancel_delivered_before_exit,
                    provider_identity_conflict: record.provider_identity_conflict,
                    provider_identity_evidence_incomplete: record
                        .provider_identity_evidence_incomplete,
                    provider_identity_eof_fragment: record.provider_identity_eof_fragment,
                    delivered_signals: record.delivered_signals,
                    reaped: record.reaped,
                    omitted_reaped: record.omitted_reaped,
                    spared: record.spared,
                    omitted_spared: record.omitted_spared,
                    launch_refusal: record.launch_refusal,
                    warnings: record.warnings,
                    omitted_warnings: record.omitted_warnings,
                    terminal_observed: record.terminal_observed,
                    unresolved_cleanup: record.unresolved_cleanup,
                    unresolved_pids: record.unresolved_pids,
                    settlement_evidence: record.settlement_evidence,
                    stdout: record.stdout,
                    stderr: record.stderr,
                })));
            }
            Ok(_) => return Err(CustodyError::InvalidRecord),
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return observe_checkpoint(&handle.directory, handle.scope, &SystemProcessObserver);
        }
        provenant_os::sys::wait_pause(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5)),
        );
    }
}

fn observe_checkpoint(
    directory: &Directory,
    scope: Scope,
    observer: &dyn ProcessObserver,
) -> Result<Observation> {
    let Ok(observation) = read_record::<StoredObservation>(directory, "process.json", RECORD_LIMIT)
    else {
        return Ok(Observation::Unknown);
    };
    if observation.schema_version != 1 || observation.scope != scope {
        return Ok(Observation::Unknown);
    }
    if observation.publication_pending {
        return Ok(Observation::PublicationPending);
    }
    let live = observation.guardian.as_ref().is_some_and(|guardian| {
        matches!(observer.observe(guardian.pid), ProcessObservation::Present(fact)
            if fact.identity == *guardian && !fact.zombie)
    });
    Ok(if live {
        Observation::Live
    } else {
        Observation::Unknown
    })
}

/// Observe other live occupancy holders using the per-attempt proof retained
/// through guardian settlement. The caller's own holder is excluded by name.
pub fn other_holders_live(
    directory: &Directory,
    current_attempt: Option<&str>,
) -> provenant_os::fs::Result<bool> {
    for entry in directory.entries()? {
        let entry = entry?;
        let name = entry.to_string_lossy();
        if current_attempt == Some(name.as_ref()) || name.starts_with('.') {
            continue;
        }
        let child = match directory.open_verified_directory(&entry) {
            Ok(child) => child,
            // Slot files (including writer.lock) are not holder directories.
            Err(_)
                if directory.metadata_at(&entry, false)?.kind
                    != provenant_os::fs::FileKind::Directory =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        // Shared probe: another observer's probe must not read as a live holder.
        match child.try_lock_existing(OsStr::new(HOLDER_LOCK), LockMode::Shared) {
            Err(FsError::LockTimeout { .. }) => return Ok(true),
            Ok(_free) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// Initialise the short observer gate before publishing writer exclusion.
/// Registry mutation owns creation; read-only reports only open this file.
pub fn prepare_writer_observation(directory: &Directory) -> Result<()> {
    drop(writer_admission_guard(directory)?);
    Ok(())
}

/// Try admission once. An observer is busy evidence, never a reason to wait.
pub fn writer_admission_guard(directory: &Directory) -> Result<LockGuard> {
    directory
        .try_lock(OsStr::new("writer-probe.lock"), LockMode::Exclusive)
        .map_err(observation_error)
}

/// Holds the slot admission gate while a caller makes a decision that must
/// remain true until its work completes, such as an unscoped budget commit.
pub struct WorktreeSharingGuard {
    _admission: LockGuard,
    shared: bool,
}

impl WorktreeSharingGuard {
    pub fn was_shared(&self) -> bool {
        self.shared
    }
}

/// Return whether this attempt shared its worktree at admission or any later
/// writer was admitted. The durable slot generation survives sibling
/// settlement, so a point-in-time holder census cannot erase that history.
/// Missing or malformed history is an error and callers must preserve work.
pub fn worktree_was_shared(slot: &Directory, current_attempt: &str) -> Result<bool> {
    Ok(worktree_sharing_guard(slot, current_attempt)?.was_shared())
}

/// Acquire the slot admission gate and inspect this attempt's sharing history.
/// Keep the returned guard alive through the protected operation to prevent a
/// new sibling admission between the check and that operation.
pub fn worktree_sharing_guard(
    slot: &Directory,
    current_attempt: &str,
) -> Result<WorktreeSharingGuard> {
    let admission_guard = writer_admission_guard(slot)?;
    let attempt = AttemptId::parse(current_attempt).map_err(|_| CustodyError::InvalidRecord)?;
    let evidence = slot.open_verified_directory(OsStr::new(current_attempt))?;
    let reservation: Reservation = read_record(&evidence, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(&evidence, &reservation)?;
    if reservation.scope.attempt != attempt
        || evidence.current_path()?.file_name() != Some(OsStr::new(current_attempt))
    {
        return Err(CustodyError::InvalidRecord);
    }
    let admission: HolderAdmission = read_record(&evidence, HOLDER_ADMISSION, RECORD_LIMIT)?;
    if admission.schema_version != 1
        || admission.scope != reservation.scope
        || admission.generation == 0
    {
        return Err(CustodyError::InvalidRecord);
    }
    let generation: HolderGeneration =
        read_record(slot, HOLDER_GENERATION, RECORD_LIMIT).map_err(|error| match error {
            CustodyError::Filesystem(failure) if failure.is_not_found() => {
                CustodyError::InvalidRecord
            }
            error => error,
        })?;
    if generation.schema_version != 1
        || generation.generation == 0
        || generation.generation < admission.generation
    {
        return Err(CustodyError::InvalidRecord);
    }
    let siblings = list_writer_reservations_in_slot(slot)?;
    if siblings.is_fenced() {
        return Err(CustodyError::Fenced);
    }
    let sibling_present = siblings
        .reservations
        .iter()
        .any(|reservation| reservation.scope.attempt != attempt);
    let shared = admission.shared_at_admission
        || generation.generation > admission.generation
        || sibling_present;
    Ok(WorktreeSharingGuard {
        _admission: admission_guard,
        shared,
    })
}

/// Probe an existing gate once without creating state during observation.
pub fn writer_observation_guard(directory: &Directory) -> Result<LockGuard> {
    directory
        .try_lock_existing(OsStr::new("writer-probe.lock"), LockMode::Exclusive)
        .map_err(observation_error)
}

fn observation_error(error: FsError) -> CustodyError {
    match error {
        FsError::LockTimeout { .. } => CustodyError::Busy,
        error => CustodyError::ObservationGate(error.to_string()),
    }
}

/// Recovery carries a verified reservation and shared or exclusive worktree
/// occupancy. Removal and destructive recovery require exclusive occupancy.
#[derive(Debug)]
pub struct Recovery {
    pub(crate) directory: Directory,
    pub(crate) lock_directory: Directory,
    pub(crate) lease: Option<LockGuard>,
    pub(crate) holder: Option<LockGuard>,
    pub(crate) reservation: Reservation,
    // The guardian's durable cleanup or native reboot proof establishes this.
    pub(crate) guardian_quiescent: bool,
    pub(crate) loss_locks: Option<liveness::LossLocks>,
}

/// Both exclusions and native evidence that a reboot ended this attempt's
/// entire process tree. Run commits Lost before releasing this capability.
pub struct RebootRecovery {
    recovery: Recovery,
    _custody: LockGuard,
}
impl RebootRecovery {
    pub fn scope(&self) -> Scope {
        self.recovery.scope()
    }
    /// The trusted Run owner calls this only after its canonical terminal
    /// transaction. No attempt artefacts or worktree contents are removed.
    pub fn release(self) -> Result<()> {
        clear_reservation(&self.recovery.directory, self.scope())
    }
    /// Keep writer exclusion for normal terminal settlement, which reacquires
    /// attempt custody while verifying the canonical release artefacts.
    pub fn into_recovery(mut self) -> Recovery {
        // Reboot ended the guardian; writer exclusion remains while verify_settlement reacquires custody.
        self.recovery.guardian_quiescent = true;
        self.recovery
    }
}
pub fn acquire_recovery(
    directory: Directory,
    scope: Scope,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<Recovery> {
    let lease = lock_writer_exclusion(&directory, budget, cancel, clock)?;
    let lock_directory = directory;
    let directory = recovery_evidence(&lock_directory, scope)?;
    let reservation: Reservation = read_record(&directory, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(&directory, &reservation)?;
    // Callers may inspect a retained generation before deciding whether to
    // settle it; recovery evidence belongs to this run and attempt.
    if reservation.scope.attempt != scope.attempt {
        return Err(CustodyError::Fenced);
    }
    let holder = directory.lock(
        OsStr::new(HOLDER_LOCK),
        LockMode::Exclusive,
        budget,
        cancel,
        clock,
    )?;
    Ok(Recovery {
        directory,
        lock_directory,
        lease: Some(lease),
        holder: Some(holder),
        reservation,
        guardian_quiescent: false,
        loss_locks: None,
    })
}

/// Hold writer exclusion for submission-time mutation only when no attempt
/// reservation remains. The guard must outlive every checkout mutation.
pub fn acquire_unreserved_writer(
    directory: &Directory,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<LockGuard> {
    let lease = lock_writer_exclusion(directory, budget, cancel, clock)?;
    let listing = list_writer_reservations_in_slot(directory)?;
    if !listing.reservations.is_empty() || !listing.issues.is_empty() {
        return Err(CustodyError::Fenced);
    }
    Ok(lease)
}

/// Acquire a reservation for reboot recovery at the expected custody slot.
pub fn acquire_reboot_recovery(
    directory: Directory,
    attempt: &Directory,
    scope: Scope,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<Recovery> {
    acquire_reboot_recovery_with(
        directory,
        attempt,
        scope,
        budget,
        cancel,
        clock,
        &SystemProcessObserver,
    )
}

/// Whether two directories remain on the same filesystem across a device renumbering.
pub fn same_device_rebinding(old_a: u64, old_b: u64, new_a: u64, new_b: u64) -> bool {
    old_a == old_b && new_a == new_b
}

pub(crate) fn acquire_reboot_recovery_with(
    directory: Directory,
    attempt: &Directory,
    scope: Scope,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
    observer: &dyn ProcessObserver,
) -> Result<Recovery> {
    let lease = lock_writer_exclusion(&directory, budget, cancel, clock)?;
    let lock_directory = directory;
    let directory = recovery_evidence(&lock_directory, scope)?;
    let reservation: Reservation = read_record(&directory, "reservation.json", RECORD_LIMIT)?;
    reservation.validate()?;
    let slot = directory.metadata()?;
    let target = attempt.metadata()?;
    let boot = observer
        .boot_identity()
        .map_err(|e| CustodyError::Io(io::Error::other(e.to_string())))?;
    if boot.is_empty()
        || boot == reservation.boot
        || reservation.scope != scope
        || reservation.reservation_inode != slot.inode
        || reservation.attempt_inode != Some(target.inode)
        || !reservation.attempt_device.is_some_and(|device| {
            same_device_rebinding(
                reservation.reservation_device,
                device,
                slot.device,
                target.device,
            )
        })
    {
        return Err(CustodyError::InvalidRecord);
    }
    let holder = directory.lock(
        OsStr::new(HOLDER_LOCK),
        LockMode::Exclusive,
        budget,
        cancel,
        clock,
    )?;
    Ok(Recovery {
        directory,
        lock_directory,
        lease: Some(lease),
        holder: Some(holder),
        reservation,
        guardian_quiescent: false,
        loss_locks: None,
    })
}
/// Writer exclusion over an attempt directory that holds no reservation, launch
/// summary or exit. Reservations precede every guardian handoff, so none began;
/// while this lease lives no reservation can be created.
#[derive(Debug)]
pub struct Unreserved {
    _lease: LockGuard,
}
/// `None` when any reservation, launch or exit evidence exists; recover those
/// with [`acquire_recovery`] or [`reattach`].
pub fn prove_unreserved(
    directory: &Directory,
    scope: Scope,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<Option<Unreserved>> {
    let lease = lock_writer_exclusion(directory, budget, cancel, clock)?;
    match ensure_no_exclusive_reservation(directory) {
        Ok(()) => {}
        Err(CustodyError::Fenced) => return Ok(None),
        Err(error) => return Err(error),
    }
    for name in ["launch.json", "process-exit.json"] {
        match directory.metadata_at(OsStr::new(name), false) {
            Ok(_) => return Ok(None),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    if reservation_present(directory, scope)? {
        return Ok(None);
    }
    Ok(Some(Unreserved { _lease: lease }))
}
fn validate_anchor(directory: &Directory, reservation: &Reservation) -> Result<()> {
    reservation.validate()?;
    let metadata = directory.metadata()?;
    if reservation.reservation_device != metadata.device
        || reservation.reservation_inode != metadata.inode
    {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct WriterReservation {
    pub scope: Scope,
    pub reservation: PathBuf,
    pub branch: Option<String>,
    /// Native presence of the recorded guardian; None means unobservable.
    pub live: Option<bool>,
    pub age: Option<String>,
    /// Schema-1 evidence with no holder to stop: listed so reconcile and GC
    /// can clear it, but still fencing until they do. Kept out of the report
    /// so the stable serialized listing does not grow a policy-only field.
    #[serde(skip)]
    pub legacy_fence: bool,
    /// The launch window: no guardian recorded yet, launch neither spawned nor
    /// failed, so the attempt is nonterminal though unobservable. Not reported.
    #[serde(skip)]
    pub launching: bool,
}
impl WriterReservation {
    /// The stable lock slot containing this attempt's journal.
    pub fn custody_directory(&self) -> Option<&std::path::Path> {
        let parent = self.reservation.parent()?;
        if parent.file_name() != Some(OsStr::new(&self.scope.attempt.to_string())) {
            return None;
        }
        parent.parent()
    }
}

// Holder presence is diagnostic identity evidence, not cleanup eligibility.
// A settling guardian may still live; recovery may hold its old writer lock.
fn reservation_holder_live(
    reservation: &Reservation,
    observer: &dyn ProcessObserver,
) -> Option<bool> {
    let boot = observer.boot_identity().ok()?;
    if boot != reservation.boot {
        return Some(false);
    }
    let guardian = reservation.guardian.as_ref()?;
    match observer.observe(guardian.pid) {
        ProcessObservation::Present(fact) => Some(fact.identity == *guardian && !fact.zombie),
        ProcessObservation::Absent => Some(false),
        ProcessObservation::Unknown(_) => None,
    }
}

/// A pre-lease reservation whose launch never produced a holder has no process
/// for a previous version to finish. This version can withdraw or settle it
/// through reconcile, so it is listed as a fence with a current-version remedy
/// instead of being reported as unresolvable legacy evidence.
fn holderless_pre_lease_reservation(reservation: &Reservation) -> bool {
    reservation.guardian.is_none()
        && reservation.provider.is_none()
        && matches!(
            reservation.launch_state,
            LaunchState::Prepared | LaunchState::NeverSpawned
        )
}

fn format_age_ns(modified_ns: i128, now: provenant_core::WallTime) -> String {
    let now_ns = i128::from(now.as_micros()) * 1_000;
    let diff_ns = (now_ns - modified_ns).max(0);
    let secs = (diff_ns / 1_000_000_000) as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}
#[derive(Debug, Serialize)]
pub struct ReservationIssue {
    pub reservation: PathBuf,
    pub reason: String,
    /// Typed cause for internal policy decisions; the human report remains
    /// the stable serialized `reason` field.
    #[serde(skip)]
    pub kind: ReservationIssueKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservationIssueKind {
    Other,
    LegacyEvidence,
}
#[derive(Debug, Default, Serialize)]
pub struct ReservationListing {
    pub reservations: Vec<WriterReservation>,
    pub issues: Vec<ReservationIssue>,
}
impl ReservationListing {
    /// Whether this listing holds custody evidence that keeps the slot fenced
    /// until reconcile or GC clears it. Legacy holderless reservations are
    /// listed rather than reported as issues so recovery can name and withdraw
    /// them, but they must not open the slot to a new holder first.
    pub fn is_fenced(&self) -> bool {
        !self.issues.is_empty() || self.reservations.iter().any(|r| r.legacy_fence)
    }
}
/// Malformed or inaccessible entries are reported without hiding other
/// reservations. Only supervisor interprets its private reservation format.
pub fn list_writer_reservations(directory: &Directory) -> Result<ReservationListing> {
    let mut listing = ReservationListing::default();
    for (index, name) in directory.entries()?.enumerate() {
        if index >= 10_000 {
            return Err(CustodyError::InvalidRecord);
        }
        let name = name?;
        let mut scan = || -> Result<()> {
            if directory.metadata_at(&name, false)?.kind != provenant_os::fs::FileKind::Directory {
                return Ok(());
            }
            let slot = directory.open_verified_directory(&name)?;
            let local = list_writer_reservations_in_slot(&slot)?;
            for mut reservation in local.reservations {
                reservation.reservation = PathBuf::from(&name).join(reservation.reservation);
                listing.reservations.push(reservation);
            }
            for mut issue in local.issues {
                issue.reservation = PathBuf::from(&name).join(issue.reservation);
                listing.issues.push(issue);
            }
            Ok(())
        };
        if let Err(error) = scan() {
            listing.issues.push(ReservationIssue {
                reservation: PathBuf::from(&name).join("reservation.json"),
                reason: error.to_string(),
                kind: ReservationIssueKind::Other,
            });
        }
    }
    Ok(listing)
}

/// List guardian records that predate schema 2 under a project worktree-state
/// directory. Paths are relative to `directory`. This inventory deliberately
/// does not require a readable reservation record: a malformed or absent
/// reservation must not hide retained legacy guardian evidence from preflight.
#[derive(Debug)]
pub struct LegacyGuardianRecordIssue {
    pub path: PathBuf,
    pub reason: String,
}
#[derive(Debug, Default)]
pub struct LegacyGuardianRecordListing {
    pub records: Vec<PathBuf>,
    pub issues: Vec<LegacyGuardianRecordIssue>,
}
pub fn list_legacy_guardian_records(directory: &Directory) -> Result<LegacyGuardianRecordListing> {
    let mut listing = LegacyGuardianRecordListing::default();
    for (slot_index, slot_name) in directory.entries()?.enumerate() {
        if slot_index >= 10_000 {
            listing.issues.push(LegacyGuardianRecordIssue {
                path: PathBuf::new(),
                reason: "worktree slot inventory exceeded its entry limit".into(),
            });
            break;
        }
        let slot_name = match slot_name {
            Ok(name) => name,
            Err(error) => {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: PathBuf::new(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let slot_path = PathBuf::from(&slot_name);
        let metadata = match directory.metadata_at(&slot_name, false) {
            Ok(metadata) => metadata,
            Err(error) => {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: slot_path,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        if metadata.kind != provenant_os::fs::FileKind::Directory {
            continue;
        }
        let slot = match directory.open_verified_directory(&slot_name) {
            Ok(slot) => slot,
            Err(error) => {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: slot_path,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let attempts = match slot.entries() {
            Ok(attempts) => attempts,
            Err(error) => {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: slot_path,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        for (attempt_index, attempt_name) in attempts.enumerate() {
            if attempt_index >= 10_000 {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: slot_path.clone(),
                    reason: "guardian record inventory exceeded its entry limit".into(),
                });
                break;
            }
            let attempt_name = match attempt_name {
                Ok(name) => name,
                Err(error) => {
                    listing.issues.push(LegacyGuardianRecordIssue {
                        path: slot_path.clone(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            let attempt_path = slot_path.join(&attempt_name);
            let metadata = match slot.metadata_at(&attempt_name, false) {
                Ok(metadata) => metadata,
                Err(error) => {
                    listing.issues.push(LegacyGuardianRecordIssue {
                        path: attempt_path,
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            if metadata.kind != provenant_os::fs::FileKind::Directory {
                continue;
            }
            let attempt = match slot.open_verified_directory(&attempt_name) {
                Ok(attempt) => attempt,
                Err(error) => {
                    listing.issues.push(LegacyGuardianRecordIssue {
                        path: attempt_path,
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            let guardian_path = attempt_path.join("guardian-lock.json");
            match liveness::is_legacy_record(&attempt) {
                Ok(true) => listing.records.push(guardian_path),
                Ok(false) => {}
                Err(error) => listing.issues.push(LegacyGuardianRecordIssue {
                    path: guardian_path,
                    reason: error.to_string(),
                }),
            }
        }
    }
    Ok(listing)
}

/// List legacy guardian records under every project-state directory in the
/// host's `worktrees` root, including records whose project is no longer
/// registered. Paths are relative to `directory`.
pub fn list_legacy_guardian_records_in_worktrees(
    directory: &Directory,
) -> Result<LegacyGuardianRecordListing> {
    let mut listing = LegacyGuardianRecordListing::default();
    for (project_index, project_name) in directory.entries()?.enumerate() {
        if project_index >= 10_000 {
            listing.issues.push(LegacyGuardianRecordIssue {
                path: PathBuf::new(),
                reason: "project worktree inventory exceeded its entry limit".into(),
            });
            break;
        }
        let project_name = match project_name {
            Ok(name) => name,
            Err(error) => {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: PathBuf::new(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let project_path = PathBuf::from(&project_name);
        let metadata = match directory.metadata_at(&project_name, false) {
            Ok(metadata) => metadata,
            Err(error) => {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: project_path,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        if metadata.kind != provenant_os::fs::FileKind::Directory {
            continue;
        }
        let project = match directory.open_verified_directory(&project_name) {
            Ok(project) => project,
            Err(error) => {
                listing.issues.push(LegacyGuardianRecordIssue {
                    path: project_path,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        match list_legacy_guardian_records(&project) {
            Ok(project_listing) => {
                listing.records.extend(
                    project_listing
                        .records
                        .into_iter()
                        .map(|path| project_path.join(path)),
                );
                listing
                    .issues
                    .extend(project_listing.issues.into_iter().map(|issue| {
                        LegacyGuardianRecordIssue {
                            path: project_path.join(issue.path),
                            reason: issue.reason,
                        }
                    }));
            }
            Err(error) => listing.issues.push(LegacyGuardianRecordIssue {
                path: project_path,
                reason: error.to_string(),
            }),
        }
    }
    Ok(listing)
}

/// Read only one worktree's predecessor reservations. The caller holds that
/// slot's writer exclusion; sibling worktrees need no observation or lock.
/// Paths are relative to the slot, including malformed evidence.
pub fn list_writer_reservations_in_slot(slot: &Directory) -> Result<ReservationListing> {
    let now = provenant_os::clock::SystemClock::new().wall_now().ok();
    let mut listing = ReservationListing::default();
    for (index, attempt) in slot.entries()?.enumerate() {
        if index >= 10_000 {
            return Err(CustodyError::InvalidRecord);
        }
        let attempt = attempt?;
        let path = PathBuf::from(&attempt).join("reservation.json");
        let entry = || -> Result<Option<WriterReservation>> {
            // Other retired registry files do not hold recovery evidence.
            if slot.metadata_at(&attempt, false)?.kind != provenant_os::fs::FileKind::Directory {
                return Ok(None);
            }
            let child = slot.open_verified_directory(&attempt)?;
            let reservation: Reservation =
                match read_record(&child, "reservation.json", RECORD_LIMIT) {
                    Ok(reservation) => reservation,
                    Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                };
            validate_anchor(&child, &reservation)?;
            if attempt != OsStr::new(&reservation.scope.attempt.to_string()) {
                return Err(CustodyError::InvalidRecord);
            }
            let legacy = liveness::is_legacy_record(&child)?;
            let holderless = holderless_pre_lease_reservation(&reservation);
            // A schema-1 record whose launch never produced a holder is stale
            // evidence this version can reconcile, not work for an older
            // version. List it so reconcile and GC name the attempt and offer a
            // remedy this version can carry out. Keep the fence for records
            // that may still have a live holder.
            if legacy && !holderless {
                return Err(CustodyError::LegacyEvidence);
            }
            let live = if legacy && holderless {
                Some(false)
            } else {
                reservation_holder_live(&reservation, boot_observer(&reservation.boot))
            };
            let branch = match &reservation.binding {
                StoredWriterBinding::Worktree { branch, .. } => Some(branch.clone()),
                _ => None,
            };
            let age = child
                .metadata_at(OsStr::new("reservation.json"), false)
                .ok()
                .zip(now)
                .map(|(m, now)| format_age_ns(m.modified_ns, now));
            Ok(Some(WriterReservation {
                scope: reservation.scope,
                reservation: path.clone(),
                branch,
                live,
                age,
                legacy_fence: legacy && holderless,
                launching: reservation.guardian.is_none()
                    && matches!(
                        reservation.launch_state,
                        LaunchState::Prepared | LaunchState::HandingOff
                    ),
            }))
        };
        match entry() {
            Ok(Some(reservation)) => listing.reservations.push(reservation),
            Ok(None) => {}
            Err(error) => listing.issues.push(ReservationIssue {
                reservation: path,
                reason: error.to_string(),
                kind: match error {
                    CustodyError::LegacyEvidence => ReservationIssueKind::LegacyEvidence,
                    _ => ReservationIssueKind::Other,
                },
            }),
        }
    }
    Ok(listing)
}
#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryState {
    Reattached,
    Absent,
    Unknown,
}
impl Recovery {
    pub fn scope(&self) -> Scope {
        self.reservation.scope
    }
    /// The provider process identity retained by this verified custody
    /// reservation. This observation grants no signalling authority.
    pub fn provider_identity(&self) -> Option<&crate::census::ProcessIdentity> {
        self.reservation.provider.as_ref()
    }
    /// Whether this recovery holds exclusion for the given writer directory.
    pub fn holds_writer_lock(&self, directory: &Directory) -> Result<bool> {
        let held = self.lock_directory.metadata()?;
        let requested = directory.metadata()?;
        Ok(
            self.lease.is_some()
                && held.device == requested.device
                && held.inode == requested.inode,
        )
    }
    /// Durable NeverSpawned evidence plus native absence permits Run to reject
    /// an interrupted launch without inventing an exit or removing private Git.
    pub fn never_spawned(&self, attempt: &Directory) -> Result<bool> {
        let metadata = attempt.metadata()?;
        Ok(self.reservation.launch_state == LaunchState::NeverSpawned
            && self.reservation.attempt_device == Some(metadata.device)
            && self.reservation.attempt_inode == Some(metadata.inode)
            && self.observe() == RecoveryState::Absent)
    }
    /// Prove the provider absent before host settlement. Worktree occupancy
    /// spans import and publication: a reservation is evidence, not a substitute
    /// for the flock. Read-only attempts may release their lock.
    pub fn prepare_settlement(&mut self) -> Result<()> {
        if self.observe() != RecoveryState::Absent {
            return Err(CustodyError::Fenced);
        }
        if matches!(
            self.reservation.binding,
            StoredWriterBinding::ReadOnly { .. }
        ) && self.loss_locks.is_none()
        {
            drop(self.lease.take());
        }
        Ok(())
    }
    /// Reacquire released read-only occupancy for the final storage transaction. Re-read the scope
    /// under the lock rather than trusting a detached recovery's snapshot.
    fn ensure_writer_lock(&mut self) -> Result<()> {
        if self.lease.is_none() {
            let observation = writer_admission_guard(&self.lock_directory)?;
            let lease = self
                .lock_directory
                .try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
            drop(observation);
            let current: Reservation =
                read_record(&self.directory, "reservation.json", RECORD_LIMIT)?;
            validate_anchor(&self.directory, &current)?;
            if current.scope != self.scope()
                || current.attempt_device != self.reservation.attempt_device
                || current.attempt_inode != self.reservation.attempt_inode
                || !current.binding.same_custody(&self.reservation.binding)
            {
                return Err(CustodyError::Fenced);
            }
            self.lease = Some(lease);
        }
        if self.holder.is_none() {
            let clock = provenant_os::clock::SystemClock::new();
            self.holder = Some(self.directory.lock(
                OsStr::new(HOLDER_LOCK),
                LockMode::Exclusive,
                WaitBudget::cli(clock.mono_now(), 5).map_err(|_| CustodyError::InvalidRecord)?,
                &CancelToken::new(),
                &clock,
            )?);
        }
        Ok(())
    }
    /// Clear an already verified release while retaining writer occupancy.
    pub(crate) fn clear_settled_reservation(&mut self) -> Result<()> {
        self.ensure_writer_lock()?;
        if let Some(proof) = &self.loss_locks {
            proof.validate(self)?;
        }
        clear_reservation(&self.directory, self.scope())
    }
    /// Release custody of an attempt the Run owner has committed Lost after
    /// `holder_gone`, or an explicit chair abandonment with non-live presence
    /// and positive worktree non-occupancy. No attempt artefacts or worktree contents are removed.
    pub fn release_lost(mut self) -> Result<()> {
        self.clear_settled_reservation()
    }
    pub fn observe(&self) -> RecoveryState {
        self.observe_with(&SystemProcessObserver)
    }
    pub fn after_reboot(self, attempt: &Directory, scope: Scope) -> Result<Option<RebootRecovery>> {
        self.after_reboot_with(attempt, scope, &SystemProcessObserver)
    }
    pub(crate) fn after_reboot_with(
        self,
        attempt: &Directory,
        scope: Scope,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<RebootRecovery>> {
        self.reservation.validate()?;
        if self.reservation.attempt_device.is_none() {
            return Ok(None);
        }
        let metadata = attempt.metadata()?;
        if self.scope() != scope || self.reservation.attempt_inode != Some(metadata.inode) {
            return Ok(None);
        }
        let boot = observer
            .boot_identity()
            .map_err(|error| CustodyError::Io(io::Error::other(BootIdentityUnavailable(error))))?;
        if boot == self.reservation.boot {
            return Ok(None);
        }
        if self.reservation.attempt_device != Some(metadata.device) {
            let slot = self.directory.metadata()?;
            if self.reservation.reservation_inode != slot.inode
                || !self.reservation.attempt_device.is_some_and(|device| {
                    same_device_rebinding(
                        self.reservation.reservation_device,
                        device,
                        slot.device,
                        metadata.device,
                    )
                })
            {
                return Ok(None);
            }
        }
        let custody = match attempt.try_lock(OsStr::new("custody.lock"), LockMode::Exclusive) {
            Ok(custody) => custody,
            Err(FsError::LockTimeout { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(RebootRecovery {
            recovery: self,
            _custody: custody,
        }))
    }
    /// Classify custody with the same process observer used by the caller.
    pub fn observe_with(&self, observer: &dyn ProcessObserver) -> RecoveryState {
        if self.guardian_quiescent {
            return RecoveryState::Absent;
        }
        classify_reservation(&self.reservation, observer)
    }
    /// Match held worktree/Git identities and branch to the recorded launch
    /// binding. The returned base was pinned before launch, never re-resolved.
    pub fn writer_base(
        &self,
        worktree: &Directory,
        git: &Directory,
        branch: &str,
    ) -> Result<GitOid> {
        let root = worktree.metadata()?;
        let git = git.metadata()?;
        match &self.reservation.binding {
            StoredWriterBinding::Worktree {
                worktree_device,
                worktree_inode,
                git_device,
                git_inode,
                branch: recorded,
                base,
                ..
            } if root.device == *worktree_device
                && root.inode == *worktree_inode
                && git.device == *git_device
                && git.inode == *git_inode
                && recorded == branch =>
            {
                Ok(*base)
            }
            _ => Err(CustodyError::InvalidRecord),
        }
    }
}

fn classify_reservation(
    reservation: &Reservation,
    observer: &dyn ProcessObserver,
) -> RecoveryState {
    if reservation.validate().is_err() {
        return RecoveryState::Unknown;
    }
    if reservation.launch_state == LaunchState::Settling {
        return RecoveryState::Absent;
    }
    if matches!(
        reservation.launch_state,
        LaunchState::Prepared | LaunchState::NeverSpawned
    ) && reservation.guardian.is_none()
    {
        return RecoveryState::Absent;
    }
    if previous_boot(reservation, observer) {
        return RecoveryState::Absent;
    }
    if observer.boot_identity().is_err() {
        return RecoveryState::Unknown;
    }
    let Some(guardian) = &reservation.guardian else {
        return RecoveryState::Unknown;
    };
    match observer.observe(guardian.pid) {
        ProcessObservation::Present(fact)
            if fact.identity == *guardian
                && fact.identity.group_id == guardian.group_id
                && fact.identity.session_id == guardian.session_id
                && !fact.zombie =>
        {
            RecoveryState::Reattached
        }
        // A durable unresolved fence survives guardian death in this boot.
        ProcessObservation::Unknown(_) => RecoveryState::Unknown,
        _ => crate::guardian::session_absence(reservation),
    }
}

fn previous_boot(reservation: &Reservation, observer: &dyn ProcessObserver) -> bool {
    observer
        .boot_identity()
        .is_ok_and(|boot| boot != reservation.boot)
}

/// A storage-verified release receipt. It cannot be made from a scope alone.
#[derive(Debug)]
pub struct SettlementCommit {
    scope: Scope,
    _custody: std::sync::Arc<LockGuard>,
}
#[derive(Serialize, Deserialize)]
struct ReleaseReceipt {
    schema_version: u64,
    scope: Scope,
    terminal_digest: provenant_records::FileDigest,
    quarantine_digest: provenant_records::FileDigest,
    #[serde(flatten)]
    extra: ExtraFields,
}
#[derive(Serialize, Deserialize)]
struct SettlementArtifact {
    schema_version: u64,
    scope: Scope,
    kind: SettlementKind,
    #[serde(flatten)]
    extra: ExtraFields,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SettlementKind {
    CanonicalTerminal,
    QuarantineManifest,
}
impl SettlementCommit {
    /// Run publishes these small release artifacts only after its canonical
    /// terminal transaction and quarantine have committed. Supervisor validates
    /// the exact scoped bytes and syncs held handles before minting release.
    fn verify(
        scope: Scope,
        receipt: &Directory,
        terminal: &Directory,
        quarantine: &Directory,
        custody: LockGuard,
    ) -> Result<Self> {
        verify_release(scope, receipt, terminal, quarantine)?;
        Ok(Self {
            scope,
            _custody: std::sync::Arc::new(custody),
        })
    }
}
/// The caller holds the attempt custody lock and the writer lease.
pub(crate) fn verify_release(
    scope: Scope,
    receipt: &Directory,
    terminal: &Directory,
    quarantine: &Directory,
) -> Result<()> {
    let release: ReleaseReceipt = read_record(receipt, "release.json", RECORD_LIMIT)?;
    if release.schema_version != 1 || release.scope != scope {
        return Err(CustodyError::InvalidRecord);
    }
    for (directory, name, digest, expected_terminal) in [
        (
            terminal,
            "terminal-commit.json",
            release.terminal_digest,
            true,
        ),
        (
            quarantine,
            "manifest.json",
            release.quarantine_digest,
            false,
        ),
    ] {
        let mut file = directory.open_read(OsStr::new(name))?;
        let bytes = file.read_bounded(RECORD_LIMIT)?;
        let artifact: SettlementArtifact =
            decode(bytes.as_slice(), Limits::CONFIG).map_err(|_| CustodyError::InvalidRecord)?;
        if artifact.schema_version != 1
            || artifact.scope != scope
            || expected_terminal != matches!(artifact.kind, SettlementKind::CanonicalTerminal)
            || provenant_records::CanonicalV1::file_digest(&bytes) != digest
        {
            return Err(CustodyError::InvalidRecord);
        }
        file.sync()?;
        directory.sync()?;
    }
    receipt.open_read(OsStr::new("release.json"))?.sync()?;
    receipt.sync()?;
    Ok(())
}
/// Publish Run's release attestations for one committed terminal attempt:
/// `terminal-commit.json`, `quarantine/manifest.json`, then `release.json`
/// naming both digests. Repeating it with the same scope is idempotent;
/// conflicting bytes are never replaced.
pub fn publish_release(attempt: &Directory, scope: Scope) -> Result<()> {
    fn seal(dir: &Directory, name: &str, bytes: &[u8]) -> Result<()> {
        match dir.atomic_write(OsStr::new(name), ".release-", bytes, PublishMode::NoReplace) {
            Ok(publication) => publication.require_durable().map_err(Into::into),
            Err(error) if error.is_exists() => {
                if dir.read_bounded(OsStr::new(name), RECORD_LIMIT)? != bytes {
                    return Err(CustodyError::InvalidRecord);
                }
                Ok(dir.sync()?)
            }
            Err(error) => Err(error.into()),
        }
    }
    fn encode<T: Serialize>(record: &T) -> Result<Vec<u8>> {
        serde_json::to_vec(record).map_err(|_| CustodyError::InvalidRecord)
    }
    let terminal = encode(&SettlementArtifact {
        schema_version: 1,
        scope,
        kind: SettlementKind::CanonicalTerminal,
        extra: ExtraFields::default(),
    })?;
    match attempt.mkdir(OsStr::new("quarantine"), 0o700) {
        Ok(()) => attempt.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let quarantine = attempt.open_verified_directory(OsStr::new("quarantine"))?;
    seal(attempt, "terminal-commit.json", &terminal)?;
    let manifest = quarantine.read_bounded(OsStr::new("manifest.json"), RECORD_LIMIT)?;
    let header: SettlementArtifact =
        decode(manifest.as_slice(), Limits::CONFIG).map_err(|_| CustodyError::InvalidRecord)?;
    if header.schema_version != 1
        || header.scope != scope
        || !matches!(header.kind, SettlementKind::QuarantineManifest)
    {
        return Err(CustodyError::InvalidRecord);
    }
    let release = encode(&ReleaseReceipt {
        schema_version: 1,
        scope,
        terminal_digest: provenant_records::CanonicalV1::file_digest(&terminal),
        quarantine_digest: provenant_records::CanonicalV1::file_digest(&manifest),
        extra: ExtraFields::default(),
    })?;
    seal(attempt, "release.json", &release)
}
/// Clear a verified, proven-absent fence while the writer lease is held.
pub(crate) fn clear_reservation(directory: &Directory, scope: Scope) -> Result<()> {
    Publication::require_durable(write_record(
        directory,
        "settlement.json",
        &SettlementJournal {
            schema_version: 1,
            scope,
            extra: ExtraFields::default(),
        },
        PublishMode::Replace,
    )?)?;
    directory.unlink(OsStr::new("reservation.json"))?;
    match directory.metadata_at(OsStr::new(HOLDER_ADMISSION), false) {
        Ok(_) => directory.unlink(OsStr::new(HOLDER_ADMISSION))?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    directory.sync()?;
    Ok(())
}
impl Recovery {
    /// Prepared is durable before handoff. Writer exclusion and this state
    /// prove no guardian or provider launched, even without attempt binding.
    pub fn withdraw_prepared(self) -> Result<()> {
        if self.reservation.launch_state != LaunchState::Prepared
            || self.reservation.guardian.is_some()
            || self.reservation.provider.is_some()
            || self.reservation.attempt_device.is_some()
            || self.reservation.attempt_inode.is_some()
            || !self.reservation.descendants.is_empty()
        {
            return Err(CustodyError::Fenced);
        }
        // A crash may have published the new fence but left the previous
        // generation's owner behind. Settlement must describe this reservation.
        Publication::require_durable(write_record(
            &self.directory,
            "ownership.json",
            &OwnershipJournal {
                schema_version: 1,
                scope: self.scope(),
                binding: self.reservation.binding.clone(),
                extra: ExtraFields::default(),
            },
            PublishMode::Replace,
        )?)?;
        indexes::retire_unlaunched_index(&self.directory, &self.reservation)?;
        clear_reservation(&self.directory, self.scope())
    }
    /// Withdraw a holderless orphan after Run has proved that no attempt row
    /// exists while this recovery holds both writer and holder exclusion.
    /// Bound evidence must still name the exact private attempt directory.
    pub fn withdraw_orphan(
        self,
        attempt: Option<&Directory>,
        unbound_store_proven: bool,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<bool> {
        if self.reservation.guardian.is_some()
            || self.reservation.provider.is_some()
            || !self.reservation.descendants.is_empty()
            || self.reservation.omitted_descendants != 0
        {
            return Ok(false);
        }
        match self.reservation.launch_state {
            LaunchState::Prepared if unbound_store_proven => {
                self.withdraw_prepared()?;
            }
            LaunchState::NeverSpawned => {
                let Some(attempt) = attempt else {
                    return Ok(false);
                };
                let metadata = attempt.metadata()?;
                if self.reservation.attempt_device != Some(metadata.device)
                    || self.reservation.attempt_inode != Some(metadata.inode)
                    || self.reservation.unresolved_cleanup
                    || self.reservation.census_incomplete
                {
                    return Ok(false);
                }
                let _custody = attempt.lock(
                    OsStr::new("custody.lock"),
                    LockMode::Exclusive,
                    budget,
                    cancel,
                    clock,
                )?;
                // No provider ran. Retire only a verified private index and keep
                // all attempt artefacts for inspection.
                indexes::retire_unlaunched_index(&self.directory, &self.reservation)?;
                clear_reservation(&self.directory, self.scope())?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    pub fn verify_settlement(&self, attempt: &Directory) -> Result<SettlementCommit> {
        let metadata = attempt.metadata()?;
        if self.reservation.attempt_device != Some(metadata.device)
            || self.reservation.attempt_inode != Some(metadata.inode)
        {
            return Err(CustodyError::InvalidRecord);
        }
        let quarantine = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        if let Some(proof) = &self.loss_locks {
            proof.validate(self)?;
            verify_release(self.scope(), attempt, attempt, &quarantine)?;
            return Ok(SettlementCommit {
                scope: self.scope(),
                _custody: std::sync::Arc::clone(&proof.custody),
            });
        }
        let clock = provenant_os::clock::SystemClock::new();
        let budget =
            WaitBudget::cli(clock.mono_now(), 1).map_err(|_| CustodyError::InvalidRecord)?;
        let custody = attempt.lock(
            OsStr::new("custody.lock"),
            LockMode::Exclusive,
            budget,
            &CancelToken::new(),
            &clock,
        )?;
        SettlementCommit::verify(
            self.reservation.scope,
            attempt,
            attempt,
            &quarantine,
            custody,
        )
    }
    pub fn settle(
        self,
        commit: SettlementCommit,
        scratch: &mut provenant_os::scratch::Scratch,
    ) -> Result<()> {
        if commit.scope != self.reservation.scope {
            return Err(CustodyError::InvalidRecord);
        }
        if self.observe() != RecoveryState::Absent {
            return Err(CustodyError::Fenced);
        }
        // Retain exclusion through scratch settlement and fence clearing.
        self.finish_settlement(scratch)
    }
}

/// Reattach only a generation already named by the durable redacted summary.
pub fn reattach(directory: Directory, scope: Scope) -> Result<CustodyHandle> {
    #[derive(Deserialize)]
    struct Summary {
        schema_version: u64,
        scope: Scope,
        requester: ActorId,
    }
    let summary: Summary = read_record(&directory, "launch.json", RECORD_LIMIT)?;
    if summary.schema_version != 1 || summary.scope != scope || scope.generation == 0 {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(CustodyHandle {
        directory,
        scope,
        requester: summary.requester,
        held: false,
    })
}
/// A handle from the scope and requester the caller handed the guardian, read
/// from nothing in the attempt directory. A host whose attempt directory a lane
/// can write stops through this, so no summary the lane rewrites or deletes
/// decides for whom it stops. The stop request itself still lands in that
/// directory, where the lane can pre-empt it with a directory of that name or
/// remove it after the host writes it.
pub fn hold(directory: Directory, scope: Scope, requester: ActorId) -> CustodyHandle {
    CustodyHandle {
        directory,
        scope,
        requester,
        held: true,
    }
}

/// Trusted Run service composition seam for its permissions owner. This trait
/// is not an authentication boundary against Rust callers in the same process;
/// Run must not expose an unconditional implementation to untrusted records.
/// Supervisor checks actor/scope/generation/evidence and records the decision
/// before changing a fence. No implementation grants numeric-PID rights.
pub trait ReconcileAuthority {
    fn permits(&self, decision: &ReconcileDecision) -> Result<bool>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileDisposition {
    RetainFence,
    AcceptLostCustody,
}
#[derive(Clone, Debug, Serialize)]
pub struct ReconcileDecision {
    pub schema_version: u64,
    pub actor: ActorId,
    pub scope: Scope,
    pub evidence: Digest,
    pub disposition: ReconcileDisposition,
}
/// Minted only after matching authorisation and durable decision publication.
#[derive(Debug)]
pub struct ReconcileReceipt {
    scope: Scope,
    disposition: ReconcileDisposition,
}
impl Recovery {
    pub fn reconcile(
        &self,
        decision: ReconcileDecision,
        authority: &dyn ReconcileAuthority,
    ) -> Result<ReconcileReceipt> {
        if decision.schema_version != 1
            || decision.scope != self.scope()
            || !authority.permits(&decision)?
        {
            return Err(CustodyError::Fenced);
        }
        match write_record(
            &self.directory,
            "reconcile.json",
            &decision,
            PublishMode::NoReplace,
        ) {
            Ok(publication) => Publication::require_durable(publication)?,
            Err(CustodyError::Filesystem(error)) if error.is_exists() => {
                let previous = self
                    .directory
                    .read_bounded(OsStr::new("reconcile.json"), RECORD_LIMIT)?;
                let expected =
                    serde_json::to_vec(&decision).map_err(|_| CustodyError::InvalidRecord)?;
                if previous != expected {
                    return Err(CustodyError::Fenced);
                }
                self.directory.sync()?;
            }
            Err(error) => return Err(error),
        }
        Ok(ReconcileReceipt {
            scope: decision.scope,
            disposition: decision.disposition,
        })
    }
    /// Even an explicit lost-custody disposition requires Run's committed
    /// terminal/quarantine receipt. The recorded decision changes no PID rights.
    pub fn settle_reconciled(
        self,
        decision: ReconcileReceipt,
        commit: SettlementCommit,
        scratch: &mut provenant_os::scratch::Scratch,
    ) -> Result<()> {
        if decision.scope != self.scope()
            || commit.scope != self.scope()
            || decision.disposition != ReconcileDisposition::AcceptLostCustody
        {
            return Err(CustodyError::Fenced);
        }
        self.finish_settlement(scratch)
    }
    /// Release a refused import's reservation after canonical terminal
    /// evidence, retaining its private Git directory for explicit host recovery.
    pub fn release_preserving_private(mut self, commit: SettlementCommit) -> Result<()> {
        if commit.scope != self.scope() {
            return Err(CustodyError::Fenced);
        }
        self.clear_settled_reservation()
    }
    fn finish_settlement(mut self, scratch: &mut provenant_os::scratch::Scratch) -> Result<()> {
        self.ensure_writer_lock()?;
        scratch.settle()?;
        if let Some(proof) = &self.loss_locks {
            proof.validate(&self)?;
        }
        clear_reservation(&self.directory, self.scope())?;
        drop(self.lease);
        Ok(())
    }
}

mod orphans;
pub use orphans::OrphanReport;

#[cfg(test)]
#[path = "custody_tests.rs"]
mod tests;

/// A fixed work key for fixtures that need a custody command.
#[cfg(any(test, feature = "fixture"))]
#[doc(hidden)]
#[allow(clippy::expect_used)]
pub fn fixture_key(seed: u8) -> provenant_records::WorkKey {
    provenant_records::WorkKey::new(
        provenant_core::ProjectId::from_bytes([seed; 16]),
        "fixture",
        "node",
        1,
    )
    .expect("fixture key is valid")
}

#[cfg(test)]
mod exit_shape_tests {
    use super::{ProcessExit, Scope, StoredExit};
    use provenant_core::AttemptId;

    #[test]
    fn current_exit_requires_cancellation_ordering_member_but_allows_unobserved() {
        let mut exit = ProcessExit::never_spawned(
            Scope {
                attempt: AttemptId::from_bytes([7; 16]),
                generation: 1,
            },
            None,
            vec![],
        );
        exit.cancel_delivered_before_exit = None;
        exit.provider_identity_eof_fragment = true;
        assert!(exit.durable_observation().provider_identity_eof_fragment);
        let mut value = serde_json::to_value(exit).unwrap();
        assert!(value["cancel_delivered_before_exit"].is_null());
        let parsed: StoredExit = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed.cancel_delivered_before_exit, None);
        assert!(parsed.provider_identity_eof_fragment);

        value
            .as_object_mut()
            .unwrap()
            .remove("provider_identity_eof_fragment");
        let legacy: StoredExit = serde_json::from_value(value.clone()).unwrap();
        assert!(!legacy.provider_identity_eof_fragment);

        value
            .as_object_mut()
            .unwrap()
            .remove("cancel_delivered_before_exit");
        assert!(serde_json::from_value::<StoredExit>(value).is_err());
    }

    #[test]
    fn current_pending_checkpoint_requires_publication_state_member() {
        let current_pending = serde_json::json!({
            "schema_version": 1,
            "scope": Scope {
                attempt: AttemptId::from_bytes([8; 16]),
                generation: 2,
            },
            "publication_pending": true,
            "priority": null,
        });
        let parsed: super::StoredObservation =
            serde_json::from_value(current_pending.clone()).unwrap();
        assert!(parsed.publication_pending);
        assert!(parsed.guardian.is_none());
        assert!(parsed.provider.is_none());

        let mut missing = current_pending;
        missing
            .as_object_mut()
            .unwrap()
            .remove("publication_pending");
        assert!(serde_json::from_value::<super::StoredObservation>(missing).is_err());
    }
}
