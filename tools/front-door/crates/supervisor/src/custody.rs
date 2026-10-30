//! Guardian channels and persistent writer reservations. A PID is never a handle.
use crate::census::{ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver};
use crate::machine::StopReason;
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, Digest, GitOid, RunId, WaitBudget};
use provenant_os::{
    exec::{CapturedStream, Termination},
    fs::{Directory, FsError, LockGuard, LockMode, Metadata, Publication, PublishMode},
};
use provenant_records::{Limits, decode_v1};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use thiserror::Error;

pub(crate) const RECORD_LIMIT: u64 = 1024 * 1024;
pub(crate) const EXIT_LIMIT: u64 = 16 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub run: RunId,
    pub attempt: AttemptId,
    pub generation: u64,
}
#[derive(Debug, Error)]
pub enum CustodyError {
    #[error("worktree writer lock busy")]
    Busy,
    #[error("writer reservation requires recovery")]
    Fenced,
    #[error("custody identity is unknown")]
    UnknownIdentity,
    #[error("record is invalid or belongs to another generation")]
    InvalidRecord,
    #[error("launch handoff is unconfirmed; reservation remains fenced")]
    HandoffUnknown,
    #[error("operation was cancelled; custody is retained")]
    Cancelled,
    #[error("wait budget expired; custody is retained")]
    Timeout,
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    #[error("custody I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Filesystem(#[from] FsError),
}
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
    #[error(
        "invalid host build settings: check domains.fabric.lanes.max_concurrent_builds, cargo_build_jobs and cargo_debug"
    )]
    BuildSettings,
    #[error("guardian process identity unavailable; retry when process observation is available")]
    GuardianIdentity,
    /// Numeric host observations are safe to publish as the lane's refusal.
    #[error(
        "insufficient disk space: {free} bytes available, gc.free_floor {floor} bytes; run `provenant gc --apply`"
    )]
    Disk { free: u64, floor: u64 },
    #[error("memory pressure is {level}; wait for host memory pressure to fall")]
    MemoryPressure { level: String },
    #[error(
        "user process count {used} of {limit}; wait for processes to exit or raise the per-user process limit"
    )]
    Processes { used: u64, limit: u64 },
    #[error(
        "writer lane cap reached: {active} of {limit} {scope} writer lanes are active; wait for a writer to finish or raise domains.fabric.lanes.{key}",
        key = if *scope == "project" { "max_project_writer_lanes" } else { "max_writer_lanes" }
    )]
    LaneCap {
        active: u64,
        limit: u64,
        scope: &'static str,
    },
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
    },
    ReadOnly,
}
impl WriterBinding {
    /// The binding of a read-only attempt, which holds no worktree.
    pub fn read_only() -> Self {
        Self(StoredWriterBinding::ReadOnly)
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
            (Self::ReadOnly, Self::ReadOnly) => true,
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
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnershipJournal {
    schema_version: u64,
    scope: Scope,
    binding: StoredWriterBinding,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettlementJournal {
    schema_version: u64,
    scope: Scope,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Reservation {
    pub schema_version: u64,
    pub scope: Scope,
    pub launch_state: LaunchState,
    pub attempt_device: Option<u64>,
    pub attempt_inode: Option<u64>,
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
    pub(crate) reservation: Reservation,
    launched: bool,
}
impl CustodyLease {
    pub fn scope(&self) -> Scope {
        self.reservation.scope
    }
    /// A preparation failure before handoff has no child to fence. Explicit
    /// withdrawal keeps crashes conservative: dropping a lease never clears it.
    pub fn withdraw_unlaunched(self) -> Result<()> {
        if self.launched || self.lease.is_none() {
            return Err(CustodyError::Fenced);
        }
        clear_reservation(&self.directory, self.scope())
    }
    fn ensure_lease(&mut self) -> Result<()> {
        if self.lease.is_none() {
            let clock = provenant_os::clock::SystemClock::new();
            self.lease = Some(self.lock_directory.lock(
                OsStr::new("writer.lock"),
                LockMode::Exclusive,
                WaitBudget::cli(clock.mono_now(), 5).map_err(|_| CustodyError::InvalidRecord)?,
                &CancelToken::new(),
                &clock,
            )?);
        }
        Ok(())
    }
    /// Reacquire writer exclusion after a completed guardian handoff (up to 5 s).
    pub fn into_recovery(mut self) -> Result<Recovery> {
        self.ensure_lease()?;
        let reservation: Reservation =
            read_record(&self.directory, "reservation.json", RECORD_LIMIT)?;
        validate_anchor(&self.directory, &reservation)?;
        Ok(Recovery {
            directory: self.directory,
            lock_directory: self.lock_directory,
            lease: Some(self.lease.ok_or(CustodyError::Fenced)?),
            reservation,
            guardian_quiescent: false,
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
        WriterBinding(StoredWriterBinding::ReadOnly),
        budget,
        cancel,
        clock,
    )
}
/// The supplied directory is a stable exclusion slot, not a registry.
/// The flock is inherited by the provider and its descendants. Recovery evidence
/// belongs to the requested attempt; predecessor reservations do not exclude writers.
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
    let lease = directory
        .lock(
            OsStr::new("writer.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => CustodyError::Busy,
            other => other.into(),
        })?;
    let lock_directory = directory;
    // Old guardians kept their evidence in the slot itself. Do not admit a
    // successor over that fence before recovery has retained its writer head.
    match lock_directory.metadata_at(OsStr::new("reservation.json"), false) {
        Ok(_) => return Err(CustodyError::Fenced),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    let directory = create_attempt_evidence(&lock_directory, scope)?;
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
    match directory.metadata_at(OsStr::new("ownership.json"), false) {
        Ok(_) => {
            let previous: OwnershipJournal =
                read_record(&directory, "ownership.json", RECORD_LIMIT)?;
            let settled: SettlementJournal =
                read_record(&directory, "settlement.json", RECORD_LIMIT)
                    .map_err(|_| CustodyError::Fenced)?;
            // A settled owner no longer holds this path, so a new run may bind
            // a worktree recreated there. Only the attempt that settled may be
            // readmitted, and a run keeps the worktree it held.
            let same_custody = previous.binding.same_custody(&binding.0);
            if previous.schema_version != 1
                || settled.schema_version != 1
                || (settled.scope.run != previous.scope.run
                    || settled.scope.generation < previous.scope.generation)
                || (scope.run == previous.scope.run && !same_custody)
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
    };
    reservation.validate()?;
    // Publish the fence first: owner death cannot leave an invisible ownership gap.
    Publication::require_durable(write_record(
        &directory,
        "reservation.json",
        &reservation,
        PublishMode::NoReplace,
    )?)?;
    Publication::require_durable(write_record(
        &directory,
        "ownership.json",
        &OwnershipJournal {
            schema_version: 1,
            scope,
            binding: reservation.binding.clone(),
        },
        PublishMode::Replace,
    )?)?;
    Ok(CustodyLease {
        directory,
        lock_directory,
        lease: Some(lease),
        reservation,
        launched: false,
    })
}

/// Open recovery evidence for one attempt without reading the shared lock slot.
pub fn attempt_evidence(
    directory: &Directory,
    scope: Scope,
) -> provenant_os::fs::Result<Directory> {
    directory.open_verified_directory(OsStr::new(&scope.attempt.to_string()))
}
/// Test for this attempt's retained reservation without interpreting shared-slot records.
pub fn reservation_present(directory: &Directory, scope: Scope) -> provenant_os::fs::Result<bool> {
    match directory.metadata_at(OsStr::new("reservation.json"), false) {
        // Legacy writers kept one reservation at the slot root; it fences only
        // the attempt it names. An unreadable or undecodable record fails
        // safe as retained. Another attempt's record leaves this attempt's
        // own evidence to decide.
        Ok(_) => {
            let legacy: Result<Reservation> =
                read_record(directory, "reservation.json", RECORD_LIMIT);
            if legacy.map_or(true, |record| record.scope.attempt == scope.attempt) {
                return Ok(true);
            }
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error),
    }
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
/// Legacy evidence stays in place so its recorded slot inode remains valid.
/// Callers must hold the slot's writer lock and validate the returned record.
fn recovery_evidence(directory: &Directory, scope: Scope) -> provenant_os::fs::Result<Directory> {
    match attempt_evidence(directory, scope) {
        Ok(child) => match child.metadata_at(OsStr::new("reservation.json"), false) {
            Ok(_) => return Ok(child),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error),
        },
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error),
    }
    directory.metadata_at(OsStr::new("reservation.json"), false)?;
    directory.try_clone()
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
    let bytes = dir.read_bounded(OsStr::new(name), limit)?;
    decode_v1(
        bytes.as_slice(),
        Limits {
            max_bytes: limit,
            ..Limits::CONFIG
        },
    )
    .map_err(|_| CustodyError::InvalidRecord)
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
    /// Nice value inherited by the provider and its descendants.
    pub priority: u8,
    pub resolved_route: String,
    pub permission_plan_digest: Digest,
    pub task: provenant_core::TaskId,
    pub requester: ActorId,
    /// Native diagnostic command names that must be spared during cleanup.
    pub protected_commands: Vec<String>,
    /// Caller-owned config variables passed to the same-binary guardian.
    pub config_environment: Vec<OsString>,
    pub argv: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    /// Port space the guardian leases one block from for the provider's
    /// lifetime; the block is added to the provider's environment.
    pub lane_ports: Option<provenant_os::lane_ports::Plan>,
    pub cwd: PathBuf,
    pub stdin: Vec<u8>,
    pub secret_input: Option<provenant_os::exec::SecretInput>,
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
    #[serde(default = "default_silence_ms")]
    pub silence_ms: u64,
    pub stdout: StreamMode,
    pub stderr: StreamMode,
    pub wall_ms: u64,
    pub descendant_grace_ms: u64,
    pub root_flush_ms: u64,
}
// Registry default for lanes.silence_seconds, expressed in milliseconds.
fn default_silence_ms() -> u64 {
    300_000
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
            silence_ms: default_silence_ms(),
        }
    }
}
impl CapturePlan {
    /// Silence is an observation only and never authorises a stop.
    pub fn silent(&self, now_ms: u64, last_progress_ms: Option<u64>) -> bool {
        now_ms.saturating_sub(last_progress_ms.unwrap_or(0)) >= self.silence_ms
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.stdout.limit() > 64 * 1024 * 1024
            || self.stderr.limit() > 64 * 1024 * 1024
            || self.silence_ms == 0
            || self.silence_ms > 3_600_000
            || self.wall_ms == 0
            || self.wall_ms > 86400000
            || self.descendant_grace_ms > 60000
            || self.root_flush_ms > 60000
        {
            return Err(CustodyError::InvalidRecord);
        }
        Ok(())
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
#[derive(Debug)]
pub struct LaunchFailure {
    pub error: CustodyError,
    pub custody: PublicationReservation,
    pending_exit: Option<Box<ProcessExit>>,
}
impl std::fmt::Display for LaunchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for LaunchFailure {}
impl LaunchFailure {
    /// Retry the bounded never-spawned outcome while retaining attempt custody.
    /// Conflicting terminal bytes are never replaced.
    pub fn retry_exit_publication(&self) -> Result<Publication> {
        let record = self
            .pending_exit
            .as_ref()
            .ok_or(CustodyError::HandoffUnknown)?;
        let bytes = serde_json::to_vec(record).map_err(|_| CustodyError::InvalidRecord)?;
        if bytes.len() as u64 > EXIT_LIMIT {
            return Err(CustodyError::InvalidRecord);
        }
        let publish = || {
            self.custody.directory.atomic_write(
                OsStr::new("process-exit.json"),
                ".custody-",
                &bytes,
                PublishMode::NoReplace,
            )
        };
        let mut result = publish();
        if result
            .as_ref()
            .is_err_and(|error| error.raw_os_error() == Some(provenant_os::sys::errno::ENOSPC))
        {
            match self.custody.directory.unlink(OsStr::new("reserve")) {
                Ok(()) => {
                    let _ = self.custody.directory.sync();
                    result = publish();
                }
                Err(error) if error.is_not_found() => {
                    result = publish();
                }
                Err(_) => {}
            }
        }
        match result {
            Ok(publication) => Ok(publication),
            Err(error) if error.is_exists() => {
                let previous = self
                    .custody
                    .directory
                    .read_bounded(OsStr::new("process-exit.json"), EXIT_LIMIT)?;
                if previous != bytes {
                    return Err(CustodyError::InvalidRecord);
                }
                Ok(match self.custody.directory.sync() {
                    Ok(()) => Publication::Durable,
                    Err(sync_error) => Publication::VisibleDurabilityUnconfirmed { sync_error },
                })
            }
            Err(error) => Err(error.into()),
        }
    }
}
pub fn launch(
    owned: OwnedLaunch<'_>,
    capture: CapturePlan,
    publication: PublicationReservation,
) -> std::result::Result<CustodyHandle, LaunchFailure> {
    let failure = |error, custody| LaunchFailure {
        error,
        custody,
        pending_exit: None,
    };
    if owned.lease.launched {
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
    if owned.cancel.is_cancelled() || capture.validate().is_err() {
        owned.lease.reservation.launch_state = LaunchState::NeverSpawned;
        owned.lease.reservation.unresolved_cleanup = false;
        owned.lease.reservation.census_incomplete = false;
        owned.lease.launched = true;
        let reason = if owned.cancel.is_cancelled() {
            Some(StopReason::ExplicitCancellation)
        } else {
            None
        };
        let record = ProcessExit::never_spawned(scope, reason, vec![]);
        let publication_result = write_record(
            &owned.lease.directory,
            "reservation.json",
            &owned.lease.reservation,
            PublishMode::Replace,
        )
        .and_then(|publication| publication.require_durable().map_err(Into::into))
        .and_then(|()| {
            write_record(
                &publication.directory,
                "process-exit.json",
                &record,
                PublishMode::NoReplace,
            )
        })
        .and_then(|publication| publication.require_durable().map_err(Into::into));
        let error = match publication_result {
            Err(error) => error,
            Ok(()) if reason.is_some() => CustodyError::Cancelled,
            Ok(()) => CustodyError::InvalidRecord,
        };
        return Err(LaunchFailure {
            error,
            custody: publication,
            pending_exit: Some(Box::new(record)),
        });
    }
    owned.lease.launched = true;
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
    match crate::guardian::handoff(owned, capture, &publication) {
        Ok(()) => Ok(CustodyHandle {
            directory: publication.directory,
            scope,
            requester,
            held: false,
        }),
        Err(error) => Err(failure(error, publication)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CancelRequest {
    pub schema_version: u64,
    pub scope: Scope,
    pub request_id: provenant_core::OperationId,
    pub requester: ActorId,
    pub reason: StopReason,
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
    let request = CancelRequest {
        schema_version: 1,
        scope: handle.scope,
        request_id: provenant_core::OperationId::parse(&format!(
            "{}.{}.{}.cancel",
            handle.scope.run, handle.scope.attempt, handle.scope.generation
        ))
        .map_err(|_| CustodyError::InvalidRecord)?,
        requester: handle.requester,
        reason,
    };
    match write_record(
        &handle.directory,
        "cancel.json",
        &request,
        PublishMode::NoReplace,
    ) {
        Ok(publication) => {
            Publication::require_durable(publication)?;
            Ok(StopReceipt::Requested)
        }
        Err(CustodyError::Filesystem(error)) if error.is_exists() => {
            match read_record::<CancelRequest>(&handle.directory, "cancel.json", RECORD_LIMIT) {
                Ok(previous) if previous == request => return Ok(StopReceipt::AlreadyRequested),
                // A stale reattached handle never displaces a later
                // generation's request. A held scope is the guardian's own,
                // so a later generation beside it can only be forged.
                Ok(previous)
                    if !handle.held
                        && previous.scope.run == request.scope.run
                        && previous.scope.attempt == request.scope.attempt
                        && previous.scope.generation > request.scope.generation =>
                {
                    return Err(CustodyError::InvalidRecord);
                }
                Ok(_) | Err(CustodyError::InvalidRecord) => {}
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
                "cancel.json",
                &request,
                PublishMode::Replace,
            )?)?;
            let visible: CancelRequest =
                read_record(&handle.directory, "cancel.json", RECORD_LIMIT)?;
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
    pub settlement_evidence: SettlementEvidence,
    pub stdout: CaptureTotals,
    pub stderr: CaptureTotals,
    pub warnings: Vec<String>,
    pub omitted_warnings: u64,
}
impl ProcessExit {
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
            settlement_evidence: SettlementEvidence::NeverSpawned,
            stdout: CaptureTotals::default(),
            stderr: CaptureTotals::default(),
            warnings,
            omitted_warnings: 0,
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
            delivered_signals: self.delivered_signals.clone(),
            reaped: self.reaped.clone(),
            omitted_reaped: self.omitted_reaped,
            spared: self.spared.clone(),
            omitted_spared: self.omitted_spared,
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
        }
    }
}
// Read-only storage DTO permits added fields within v1.
#[derive(Deserialize)]
struct StoredExit {
    scope: Scope,
    termination: Option<Termination>,
    never_spawned: bool,
    stop_reason: Option<StopReason>,
    delivered_signals: Vec<i64>,
    reaped: Vec<ProcessIdentity>,
    omitted_reaped: u64,
    spared: Vec<ProcessIdentity>,
    omitted_spared: u64,
    warnings: Vec<String>,
    omitted_warnings: u64,
    terminal_observed: bool,
    unresolved_cleanup: bool,
    #[serde(default)]
    unresolved_pids: Vec<i64>,
    #[serde(default)]
    settlement_evidence: SettlementEvidence,
    stdout: CaptureTotals,
    stderr: CaptureTotals,
}
#[derive(Clone, Debug, Serialize)]
pub struct ExitObservation {
    pub publication: PublicationState,
    pub scope: Scope,
    pub termination: Option<Termination>,
    pub never_spawned: bool,
    pub stop_reason: Option<StopReason>,
    pub delivered_signals: Vec<i64>,
    pub reaped: Vec<ProcessIdentity>,
    pub omitted_reaped: u64,
    pub spared: Vec<ProcessIdentity>,
    pub omitted_spared: u64,
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
#[derive(Deserialize)]
struct StoredObservation {
    scope: Scope,
    guardian: Option<ProcessIdentity>,
    #[serde(default)]
    provider: Option<ProcessIdentity>,
    #[serde(default)]
    publication_pending: bool,
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
    record.scope == scope
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
/// Whether the guardian and provider recorded for this attempt are both gone
/// in this boot. Only an observed absence counts; an unreadable process or
/// record leaves the attempt open.
pub fn holder_gone(directory: &Directory, scope: Scope, observer: &dyn ProcessObserver) -> bool {
    let record = match read_record::<StoredObservation>(directory, "process.json", RECORD_LIMIT) {
        Ok(record) => record,
        // No process was ever recorded, so the reservation alone speaks for it.
        Err(CustodyError::Filesystem(error)) if error.is_not_found() => return true,
        Err(_) => return false,
    };
    let Some(guardian) = record.guardian else {
        return false;
    };
    let gone = |identity: &ProcessIdentity| match observer.observe(identity.pid) {
        ProcessObservation::Absent => true,
        ProcessObservation::Present(fact) => fact.identity != *identity || fact.zombie,
        ProcessObservation::Unknown(_) => false,
    };
    record.scope == scope
        && !record.publication_pending
        && gone(&guardian)
        && record.provider.as_ref().is_none_or(gone)
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
    resume_orphaned_builds(&handle.directory, handle.scope)?;
    let remaining = Duration::from_micros(budget.remaining(clock.mono_now()).as_micros());
    let deadline = Instant::now()
        .checked_add(remaining)
        .ok_or(CustodyError::InvalidRecord)?;
    loop {
        if cancel.is_cancelled() {
            return Err(CustodyError::Cancelled);
        }
        match read_record::<StoredExit>(&handle.directory, "process-exit.json", EXIT_LIMIT) {
            Ok(record) if record.scope == handle.scope => {
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
                    delivered_signals: record.delivered_signals,
                    reaped: record.reaped,
                    omitted_reaped: record.omitted_reaped,
                    spared: record.spared,
                    omitted_spared: record.omitted_spared,
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
    if observation.scope != scope {
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

/// Recovery carries a verified reservation and optional writer exclusion.
/// Worktree settlement retains the kernel lock through import and publication.
#[derive(Debug)]
pub struct Recovery {
    pub(crate) directory: Directory,
    pub(crate) lock_directory: Directory,
    pub(crate) lease: Option<LockGuard>,
    pub(crate) reservation: Reservation,
    // The guardian's durable cleanup or native reboot proof establishes this.
    pub(crate) guardian_quiescent: bool,
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
    let lease = directory.lock(
        OsStr::new("writer.lock"),
        LockMode::Exclusive,
        budget,
        cancel,
        clock,
    )?;
    let lock_directory = directory;
    let directory = recovery_evidence(&lock_directory, scope)?;
    let reservation: Reservation = read_record(&directory, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(&directory, &reservation)?;
    // Per-attempt callers may inspect a retained generation before deciding
    // whether to settle it. A shared legacy record must match the full scope.
    if reservation.scope.run != scope.run
        || reservation.scope.attempt != scope.attempt
        || (directory
            .metadata()?
            .same_identity(&lock_directory.metadata()?)
            && reservation.scope != scope)
    {
        return Err(CustodyError::Fenced);
    }
    Ok(Recovery {
        directory,
        lock_directory,
        lease: Some(lease),
        reservation,
        guardian_quiescent: false,
    })
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
    let lease = directory.lock(
        OsStr::new("writer.lock"),
        LockMode::Exclusive,
        budget,
        cancel,
        clock,
    )?;
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
    Ok(Recovery {
        directory,
        lock_directory,
        lease: Some(lease),
        reservation,
        guardian_quiescent: false,
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
    let lease = directory.lock(
        OsStr::new("writer.lock"),
        LockMode::Exclusive,
        budget,
        cancel,
        clock,
    )?;
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

#[derive(Debug, Serialize)]
pub struct WriterReservation {
    pub scope: Scope,
    pub reservation: PathBuf,
}
impl WriterReservation {
    /// The stable lock slot, for both historical and per-attempt journals.
    pub fn custody_directory(&self) -> Option<&std::path::Path> {
        let parent = self.reservation.parent()?;
        if parent.file_name() == Some(OsStr::new(&self.scope.attempt.to_string())) {
            parent.parent()
        } else {
            Some(parent)
        }
    }
}
#[derive(Debug, Serialize)]
pub struct ReservationIssue {
    pub reservation: PathBuf,
    pub reason: String,
}
#[derive(Debug, Default, Serialize)]
pub struct ReservationListing {
    pub reservations: Vec<WriterReservation>,
    pub issues: Vec<ReservationIssue>,
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
            for (index, attempt) in slot.entries()?.enumerate() {
                if index >= 10_000 {
                    return Err(CustodyError::InvalidRecord);
                }
                let attempt = attempt?;
                if attempt == OsStr::new("reservation.json") {
                    let path = PathBuf::from(&name).join(&attempt);
                    let legacy = || -> Result<WriterReservation> {
                        let reservation: Reservation =
                            read_record(&slot, "reservation.json", RECORD_LIMIT)?;
                        validate_anchor(&slot, &reservation)?;
                        Ok(WriterReservation {
                            scope: reservation.scope,
                            reservation: path.clone(),
                        })
                    };
                    match legacy() {
                        Ok(reservation) => listing.reservations.push(reservation),
                        Err(error) => listing.issues.push(ReservationIssue {
                            reservation: path,
                            reason: format!("legacy slot-root writer reservation: {error}"),
                        }),
                    }
                    continue;
                }
                let path = PathBuf::from(&name).join(&attempt).join("reservation.json");
                let entry = || -> Result<Option<WriterReservation>> {
                    // Other retired registry files do not hold recovery evidence.
                    if slot.metadata_at(&attempt, false)?.kind
                        != provenant_os::fs::FileKind::Directory
                    {
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
                    Ok(Some(WriterReservation {
                        scope: reservation.scope,
                        reservation: path.clone(),
                    }))
                };
                match entry() {
                    Ok(Some(reservation)) => listing.reservations.push(reservation),
                    Ok(None) => {}
                    Err(error) => listing.issues.push(ReservationIssue {
                        reservation: path,
                        reason: error.to_string(),
                    }),
                }
            }
            Ok(())
        };
        if let Err(error) = scan() {
            listing.issues.push(ReservationIssue {
                reservation: PathBuf::from(&name).join("reservation.json"),
                reason: error.to_string(),
            });
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
    /// Durable NeverSpawned evidence plus native absence permits Run to reject
    /// an interrupted launch without inventing an exit or removing private Git.
    pub fn never_spawned(&self, attempt: &Directory) -> Result<bool> {
        let metadata = attempt.metadata()?;
        Ok(self.reservation.launch_state == LaunchState::NeverSpawned
            && self.reservation.attempt_device == Some(metadata.device)
            && self.reservation.attempt_inode == Some(metadata.inode)
            && self.observe() == RecoveryState::Absent)
    }
    /// Prove the provider absent before host settlement. Worktree exclusion
    /// spans import and publication: a reservation is recovery evidence, not
    /// a substitute for the flock. Read-only attempts may release their lock.
    pub fn prepare_settlement(&mut self) -> Result<()> {
        if self.observe() != RecoveryState::Absent {
            return Err(CustodyError::Fenced);
        }
        if matches!(self.reservation.binding, StoredWriterBinding::ReadOnly) {
            drop(self.lease.take());
        }
        Ok(())
    }
    /// Reacquire released read-only exclusion for the final storage transaction. Re-read the scope
    /// under the lock rather than trusting a detached recovery's snapshot.
    fn ensure_writer_lock(&mut self) -> Result<()> {
        if self.lease.is_none() {
            let lease = self
                .lock_directory
                .try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
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
        Ok(())
    }
    /// Clear an already verified release while retaining writer exclusion.
    pub(crate) fn clear_settled_reservation(&mut self) -> Result<()> {
        self.ensure_writer_lock()?;
        clear_reservation(&self.directory, self.scope())
    }
    /// Release custody of an attempt the Run owner has committed Lost after
    /// `holder_gone`. No attempt artefacts or worktree contents are removed.
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
        let boot = observer.boot_identity().map_err(|error| {
            CustodyError::Io(io::Error::other(format!(
                "boot identity unavailable: {error}"
            )))
        })?;
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
        orphans::resume_paused_with(attempt, scope, observer)?;
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
    _custody: LockGuard,
}
#[derive(Serialize, Deserialize)]
struct ReleaseReceipt {
    schema_version: u64,
    scope: Scope,
    terminal_digest: provenant_records::FileDigest,
    quarantine_digest: provenant_records::FileDigest,
}
#[derive(Serialize, Deserialize)]
struct SettlementArtifact {
    schema_version: u64,
    scope: Scope,
    kind: SettlementKind,
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
            _custody: custody,
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
    if release.scope != scope {
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
            decode_v1(bytes.as_slice(), Limits::CONFIG).map_err(|_| CustodyError::InvalidRecord)?;
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
        decode_v1(manifest.as_slice(), Limits::CONFIG).map_err(|_| CustodyError::InvalidRecord)?;
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
        },
        PublishMode::Replace,
    )?)?;
    directory.unlink(OsStr::new("reservation.json"))?;
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
            },
            PublishMode::Replace,
        )?)?;
        clear_reservation(&self.directory, self.scope())
    }
    pub fn verify_settlement(&self, attempt: &Directory) -> Result<SettlementCommit> {
        let metadata = attempt.metadata()?;
        if self.reservation.attempt_device != Some(metadata.device)
            || self.reservation.attempt_inode != Some(metadata.inode)
        {
            return Err(CustodyError::InvalidRecord);
        }
        let quarantine = attempt.open_verified_directory(OsStr::new("quarantine"))?;
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
        self.resume_paused_builds(attempt)?;
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
        scope: Scope,
        requester: ActorId,
    }
    let summary: Summary = read_record(&directory, "launch.json", RECORD_LIMIT)?;
    if summary.scope != scope || scope.generation == 0 {
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
        clear_reservation(&self.directory, self.scope())?;
        drop(self.lease);
        Ok(())
    }
}

mod orphans;
#[cfg(feature = "fixture")]
pub(crate) use orphans::resume_orphaned_builds_with;
pub use orphans::{OrphanReport, resume_orphaned_builds};

#[cfg(test)]
#[path = "custody_tests.rs"]
mod tests;
