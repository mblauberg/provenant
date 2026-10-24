//! Guardian channels and persistent writer reservations. A PID is never a handle.
use crate::census::{ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver};
use crate::exec::{CapturedStream, Termination};
use crate::fs::{Directory, FsError, LockGuard, LockMode, Publication, PublishMode, SafeDir};
use crate::machine::StopReason;
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, Digest, GitOid, RunId, WaitBudget};
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
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub run: RunId,
    pub attempt: AttemptId,
    pub generation: u64,
}
#[derive(Debug, Error)]
pub enum CustodyError {
    #[error("worktree is busy")]
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
    #[error("custody I/O failed")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Filesystem(#[from] FsError),
}
pub type Result<T> = std::result::Result<T, CustodyError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LaunchState {
    Prepared,
    HandingOff,
    NeverSpawned,
    Spawned,
}
/// Confinement supplies verified worktree and Git directory handles, never a
/// caller-selected PID or a path that supervisor resolves again. A read-only
/// binding holds custody in the attempt directory and grants no worktree.
#[derive(Clone, Debug, Serialize)]
pub struct WriterBinding(StoredWriterBinding);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
    pub fn from_verified(
        worktree: &dyn SafeDir,
        git: &dyn SafeDir,
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
#[serde(deny_unknown_fields)]
struct OwnershipJournal {
    schema_version: u64,
    scope: Scope,
    binding: StoredWriterBinding,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettlementJournal {
    schema_version: u64,
    scope: Scope,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
            let clock = crate::clock::SystemClock::new();
            self.lease = Some(self.directory.lock(
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
            lease: self.lease.ok_or(CustodyError::Fenced)?,
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
/// The supplied directory belongs to confinement's verified worktree identity.
/// Existing reservations always require recovery, even with a free kernel lock.
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
            if previous.schema_version != 1
                || (settled.scope.run != previous.scope.run
                    || settled.scope.generation < previous.scope.generation)
                || !previous.binding.same_custody(&binding.0)
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
    require_durable(write_record(
        &directory,
        "reservation.json",
        &reservation,
        PublishMode::NoReplace,
    )?)?;
    require_durable(write_record(
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
        lease: Some(lease),
        reservation,
        launched: false,
    })
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
pub(crate) fn require_durable(publication: Publication) -> Result<()> {
    match publication {
        Publication::Durable => Ok(()),
        Publication::VisibleDurabilityUnconfirmed { sync_error } => Err(sync_error.into()),
    }
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
    pub cwd: PathBuf,
    pub stdin: Vec<u8>,
    /// Opaque, bounded reference for the composed terminal holder. It travels
    /// on the handoff pipe only and is never written to the launch summary.
    pub holder_context: Vec<u8>,
}
pub const HOLDER_CONTEXT_LIMIT: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamMode {
    Capture { limit: u64 },
    Discard,
    Tee { limit: u64 },
}
impl StreamMode {
    pub(crate) fn piped(self) -> bool {
        !matches!(self, Self::Discard)
    }
    pub(crate) fn limit(self) -> u64 {
        match self {
            Self::Capture { limit } | Self::Tee { limit } => limit,
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
    pub terminal_ms: u64,
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
                limit: 8 * 1024 * 1024,
            },
            stderr: StreamMode::Capture {
                limit: 8 * 1024 * 1024,
            },
            wall_ms: 3600000,
            terminal_ms: 500,
            descendant_grace_ms: 500,
            root_flush_ms: 2000,
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
            || self.terminal_ms > 60000
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
}
impl CustodyHandle {
    pub fn scope(&self) -> Scope {
        self.scope
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
            .is_err_and(|error| error.raw_os_error() == Some(crate::sys::errno::ENOSPC))
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
        let record = ProcessExit {
            schema_version: 1,
            scope,
            termination: None,
            never_spawned: true,
            stop_reason: reason,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            failed_deliveries: vec![],
            omitted_failed_deliveries: 0,
            terminal_observed: false,
            unresolved_cleanup: false,
            settlement_evidence: SettlementEvidence::NeverSpawned,
            stdout: CaptureTotals::default(),
            stderr: CaptureTotals::default(),
            warnings: vec![],
            omitted_warnings: 0,
        };
        let publication_result = write_record(
            &owned.lease.directory,
            "reservation.json",
            &owned.lease.reservation,
            PublishMode::Replace,
        )
        .and_then(require_durable)
        .and_then(|()| {
            write_record(
                &publication.directory,
                "process-exit.json",
                &record,
                PublishMode::NoReplace,
            )
        })
        .and_then(require_durable);
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
    .and_then(require_durable)
    {
        return Err(failure(error, publication));
    }
    match crate::guardian::handoff(owned, capture, &publication) {
        Ok(()) => Ok(CustodyHandle {
            directory: publication.directory,
            scope,
            requester,
        }),
        Err(error) => Err(failure(error, publication)),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
            require_durable(publication)?;
            Ok(StopReceipt::Requested)
        }
        Err(CustodyError::Filesystem(error)) if error.is_exists() => {
            let previous: CancelRequest =
                read_record(&handle.directory, "cancel.json", RECORD_LIMIT)?;
            if previous.scope != handle.scope {
                return Err(CustodyError::InvalidRecord);
            }
            Ok(StopReceipt::AlreadyRequested)
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
pub enum SettlementEvidence {
    #[default]
    Unresolved,
    NeverSpawned,
    LinuxKernelChildren,
    DarwinCensusAndCustodyLease,
}
/// Bounded, immutable OS facts. No field certifies its own publication durability.
#[derive(Clone, Debug, Serialize)]
pub struct ProcessExit {
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
    pub settlement_evidence: SettlementEvidence,
    pub stdout: CaptureTotals,
    pub stderr: CaptureTotals,
    pub warnings: Vec<String>,
    pub omitted_warnings: u64,
}
impl ProcessExit {
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
    publication_pending: bool,
}
#[derive(Debug)]
pub enum Observation {
    Live,
    PublicationPending,
    Exited(Box<ExitObservation>),
    Unknown,
}
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
            return observe_checkpoint(&handle.directory, handle.scope);
        }
        crate::sys::wait_pause(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5)),
        );
    }
}

fn observe_checkpoint(directory: &Directory, scope: Scope) -> Result<Observation> {
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
        matches!(SystemProcessObserver.observe(guardian.pid), ProcessObservation::Present(fact)
            if fact.identity == *guardian && !fact.zombie)
    });
    Ok(if live {
        Observation::Live
    } else {
        Observation::Unknown
    })
}

/// Recovery holds the same writer exclusion; no decision relies on free flock.
#[derive(Debug)]
pub struct Recovery {
    pub(crate) directory: Directory,
    pub(crate) lease: LockGuard,
    pub(crate) reservation: Reservation,
    // Only the guardian can mint this after its durable exit and cleanup.
    pub(crate) guardian_quiescent: bool,
}
pub fn acquire_recovery(
    directory: Directory,
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
    let reservation: Reservation = read_record(&directory, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(&directory, &reservation)?;
    Ok(Recovery {
        directory,
        lease,
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
    for name in ["reservation.json", "launch.json", "process-exit.json"] {
        match directory.metadata_at(OsStr::new(name), false) {
            Ok(_) => return Ok(None),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
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
/// Malformed or inaccessible entries remain fenced, but cannot hide other
/// reservations. Only supervisor interprets its private reservation format.
pub fn list_writer_reservations(directory: &Directory) -> Result<ReservationListing> {
    let mut listing = ReservationListing::default();
    for (index, name) in directory.entries()?.enumerate() {
        if index >= 10_000 {
            return Err(CustodyError::InvalidRecord);
        }
        let name = name?;
        let path = PathBuf::from(&name).join("reservation.json");
        let entry = || -> Result<Option<WriterReservation>> {
            if directory.metadata_at(&name, false)?.kind != crate::fs::FileKind::Directory {
                return Ok(None);
            }
            let child = directory.open_verified_directory(&name)?;
            let reservation: Reservation =
                match read_record(&child, "reservation.json", RECORD_LIMIT) {
                    Ok(reservation) => reservation,
                    Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                };
            validate_anchor(&child, &reservation)?;
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
    pub fn observe(&self) -> RecoveryState {
        self.observe_with(&SystemProcessObserver)
    }
    pub(crate) fn observe_with(&self, observer: &dyn ProcessObserver) -> RecoveryState {
        if self.guardian_quiescent {
            return RecoveryState::Absent;
        }
        classify_reservation(&self.reservation, observer)
    }
    /// Match held worktree/Git identities and branch to the recorded launch
    /// binding. The returned base was pinned before launch, never re-resolved.
    pub fn writer_base(
        &self,
        worktree: &dyn SafeDir,
        git: &dyn SafeDir,
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
    if matches!(
        reservation.launch_state,
        LaunchState::Prepared | LaunchState::NeverSpawned
    ) && reservation.guardian.is_none()
    {
        return RecoveryState::Absent;
    }
    let Ok(boot) = observer.boot_identity() else {
        return RecoveryState::Unknown;
    };
    if boot != reservation.boot {
        return RecoveryState::Absent;
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

/// A storage-verified release receipt. It cannot be made from a scope alone.
#[derive(Debug)]
pub struct SettlementCommit {
    scope: Scope,
    _custody: LockGuard,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
/// Clear a verified, proven-absent fence while the writer lease is held.
/// Publish Run's release attestations for one committed terminal attempt:
/// `terminal-commit.json`, `quarantine/manifest.json`, then `release.json`
/// naming both digests. Repeating it with the same scope is idempotent;
/// conflicting bytes are never replaced.
pub fn publish_release(attempt: &Directory, scope: Scope) -> Result<()> {
    fn seal(dir: &Directory, name: &str, bytes: &[u8]) -> Result<()> {
        match dir.atomic_write(OsStr::new(name), ".release-", bytes, PublishMode::NoReplace) {
            Ok(publication) => require_durable(publication),
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
pub(crate) fn clear_reservation(directory: &Directory, scope: Scope) -> Result<()> {
    require_durable(write_record(
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
        require_durable(write_record(
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
        let clock = crate::clock::SystemClock::new();
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
        scratch: &mut crate::scratch::Scratch,
    ) -> Result<()> {
        if commit.scope != self.reservation.scope {
            return Err(CustodyError::InvalidRecord);
        }
        if self.observe() != RecoveryState::Absent {
            return Err(CustodyError::Fenced);
        }
        // The same writer lease spans scratch settlement and fence clearing.
        self.finish_settlement(scratch)
    }
}

impl CustodyLease {
    pub fn advance_attempt(&mut self, next: Scope) -> Result<()> {
        self.advance_with(next, &SystemProcessObserver)
    }
    pub(crate) fn advance_with(
        &mut self,
        next: Scope,
        observer: &dyn ProcessObserver,
    ) -> Result<()> {
        self.ensure_lease()?;
        let previous: Reservation = read_record(&self.directory, "reservation.json", RECORD_LIMIT)?;
        previous.validate()?;
        if previous.scope.run != next.run
            || previous.scope.generation.checked_add(1) != Some(next.generation)
            || previous.scope.attempt == next.attempt
        {
            return Err(CustodyError::InvalidRecord);
        }
        if classify_reservation(&previous, observer) != RecoveryState::Absent {
            return Err(CustodyError::Fenced);
        }
        let mut reservation = previous;
        reservation.scope = next;
        reservation.launch_state = LaunchState::Prepared;
        reservation.guardian = None;
        reservation.provider = None;
        reservation.attempt_device = None;
        reservation.attempt_inode = None;
        reservation.descendants.clear();
        reservation.omitted_descendants = 0;
        reservation.census_incomplete = true;
        reservation.unresolved_cleanup = true;
        // Poison launch before publication: an error may mean the new
        // generation is visible but not yet durable. Recovery reloads it.
        self.launched = true;
        self.reservation = reservation;
        require_durable(write_record(
            &self.directory,
            "reservation.json",
            &self.reservation,
            PublishMode::Replace,
        )?)?;
        self.launched = false;
        Ok(())
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
    })
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
            Ok(publication) => require_durable(publication)?,
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
        scratch: &mut crate::scratch::Scratch,
    ) -> Result<()> {
        if decision.scope != self.scope()
            || commit.scope != self.scope()
            || decision.disposition != ReconcileDisposition::AcceptLostCustody
        {
            return Err(CustodyError::Fenced);
        }
        self.finish_settlement(scratch)
    }
    fn finish_settlement(self, scratch: &mut crate::scratch::Scratch) -> Result<()> {
        scratch.settle()?;
        clear_reservation(&self.directory, self.scope())?;
        drop(self.lease);
        Ok(())
    }
}

#[cfg(test)]
#[path = "custody_tests.rs"]
mod tests;
