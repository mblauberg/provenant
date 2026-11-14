//! Dedicated same-binary guardian. Run composes provider decoding at this edge.
mod cleanup;
#[cfg(test)]
use cleanup::{darwin_census_clear, try_unlinked_writer_lease, try_writer_lease};

use crate::census::{ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver};
use crate::custody::*;
use crate::machine::{
    self, Budgets, DeliveryFailure, DeliveryOutcome, DeliveryReceipt, Effect, Event, Phase, State,
    Tick,
};
use provenant_core::{CancelToken, Clock, WallTime};
use provenant_os::fs::{Directory, Publication, PublishMode};
use provenant_records::{Limits, decode_v1};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn census_fallback_notice(fallback: bool, recorded: &mut bool) -> Option<&'static str> {
    if fallback && !*recorded {
        *recorded = true;
        Some("process events or scoped census unavailable; using full scan")
    } else {
        None
    }
}

// Bound live walks to the former 10 Hz census rate, even during fork storms.
// A five-second backstop finds missed events; wait errors back off by one tick.
const CENSUS_INTERVAL_MS: u64 = 100;
const FULL_CENSUS_INTERVAL_MS: u64 = 5_000;
const EVENT_DRAIN_BACKOFF_MS: u64 = 10;
const JOURNAL_RETRY_INTERVAL_MS: u64 = 100;
const TRACKING_INTERVAL_MS: u64 = 1_000;

fn tracking_publication_due(now: u64, last_attempt: Option<u64>, failed: bool) -> bool {
    last_attempt.is_none_or(|last| {
        now.saturating_sub(last)
            >= if failed {
                JOURNAL_RETRY_INTERVAL_MS
            } else {
                TRACKING_INTERVAL_MS
            }
    })
}

fn census_full_due(now: u64, last_full: Option<u64>, event_failure: bool) -> bool {
    event_failure
        || last_full.is_none_or(|last| now.saturating_sub(last) >= FULL_CENSUS_INTERVAL_MS)
}

fn census_walk_due(now: u64, last_walk: Option<u64>) -> bool {
    last_walk.is_none_or(|last| now.saturating_sub(last) >= CENSUS_INTERVAL_MS)
}

/// Whether this turn walks at all. Every known PID carries a fork and exit
/// filter, so with healthy watches the tree can only change after an event;
/// a walk with no event repeats the last one. An idle lane therefore walks
/// only at the full backstop. A pending event, a newly added watch, failed
/// watches or a failed reactor keep the former 100 ms rate.
fn census_turn_due(
    now: u64,
    last_walk: Option<u64>,
    last_full: Option<u64>,
    reason_to_walk: bool,
) -> bool {
    census_walk_due(now, last_walk)
        && (last_walk.is_none() || reason_to_walk || census_full_due(now, last_full, false))
}

/// Custody retains zombies until absence is proven, but dead processes cannot
/// fork. Re-adding their one-shot exit filters can manufacture a new event and
/// another census. Unknown rows still require a watch or full-scan fallback.
fn census_watch_pids<'a>(
    census: &crate::census::Census,
    identities: impl Iterator<Item = &'a ProcessIdentity>,
) -> std::collections::BTreeSet<i64> {
    identities
        .filter(|identity| match census.process(identity.pid) {
            crate::census::ProcessLookup::Present(fact) => {
                fact.identity == **identity && !fact.zombie
            }
            crate::census::ProcessLookup::Absent => false,
            crate::census::ProcessLookup::Unknown => true,
        })
        .map(|identity| identity.pid)
        .collect()
}

// Directory notifications wake cancellation promptly. The periodic read also
// covers in-place writes to an existing cancel file and lost notifications.
fn cancel_poll_interval(watched: bool) -> u64 {
    if watched { 1_000 } else { 100 }
}

/// Clone failure must leave the driver's writer lease available for recovery.
fn settlement_recovery(
    writer: &mut Option<provenant_os::fs::LockGuard>,
    directory: Directory,
    lock_directory: provenant_os::fs::Result<Directory>,
    reservation: &Reservation,
) -> Result<Recovery> {
    let lock_directory = lock_directory.map_err(|error| {
        CustodyError::Io(io::Error::other(format!(
            "cannot clone writer lock directory for settlement of attempt {}; run `provenant reconcile`: {error}",
            reservation.scope.attempt
        )))
    })?;
    Ok(Recovery {
        directory,
        lock_directory,
        lease: Some(writer.take().ok_or(CustodyError::Fenced)?),
        reservation: reservation.clone(),
        guardian_quiescent: true,
    })
}

/// Advisory identity snapshot only; confinement does not yet supply an executable pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutableSnapshot {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchMessage {
    policy: provenant_records::policy::Policy,
    schema_version: u64,
    priority: u8,
    scope: Scope,
    resolved_route: String,
    permission_plan_digest: provenant_core::Digest,
    key: provenant_records::WorkKey,
    requester: provenant_core::ActorId,
    binding: StoredWriterBinding,
    executable_snapshot: ExecutableSnapshot,
    protected_commands: Vec<String>,
    profile: Option<crate::ProfileIdentity>,
    argv: Vec<Vec<u8>>,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    lane_ports: Option<provenant_os::lane_ports::Plan>,
    cwd: Vec<u8>,
    stdin: Vec<u8>,
    secret_input: Option<provenant_os::exec::SecretInput>,
    holder_context: Vec<u8>,
    capture: CapturePlan,
}
const LAUNCH_LIMITS: Limits = Limits {
    max_bytes: crate::process_os::handoff::MESSAGE_LIMIT as u64,
    max_members: 2 * 1024 * 1024,
    ..Limits::CONFIG
};
pub(crate) fn handoff(
    owned: OwnedLaunch<'_>,
    capture: CapturePlan,
    publication: &PublicationReservation,
) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    crate::process_os::provider::ensure_supported()?;
    if owned.command.protected_commands.len() > 128
        || owned
            .command
            .protected_commands
            .iter()
            .any(|name| name.is_empty() || name.len() > 512)
        || owned.command.config_environment.len() > 128
        || owned.command.holder_context.len() > HOLDER_CONTEXT_LIMIT
        || !valid_resolved_route(&owned.command.resolved_route)
    {
        return Err(CustodyError::InvalidRecord);
    }
    validate_profile_command(owned.command.profile_file.as_ref(), &owned.command.argv)?;
    let executable_snapshot = snapshot_executable(
        owned
            .command
            .argv
            .first()
            .ok_or(CustodyError::InvalidRecord)?
            .as_os_str(),
    )?;
    let message = LaunchMessage {
        policy: owned.command.policy,
        schema_version: 1,
        profile: owned
            .command
            .profile_file
            .as_ref()
            .map(|p| p.identity().clone()),
        priority: owned.command.priority,
        scope: owned.lease.scope(),
        resolved_route: owned.command.resolved_route,
        permission_plan_digest: owned.command.permission_plan_digest,
        key: owned.command.key,
        requester: owned.command.requester,
        binding: owned.lease.reservation.binding.clone(),
        executable_snapshot,
        protected_commands: owned.command.protected_commands,
        argv: owned
            .command
            .argv
            .iter()
            .map(|arg| arg.as_bytes().to_vec())
            .collect(),
        environment: owned
            .command
            .environment
            .iter()
            .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect(),
        lane_ports: owned.command.lane_ports,
        cwd: owned.command.cwd.as_os_str().as_bytes().to_vec(),
        stdin: owned.command.stdin,
        secret_input: owned.command.secret_input,
        holder_context: owned.command.holder_context,
        capture,
    };
    if message.argv.is_empty()
        || message.stdin.len() > 1024 * 1024
        || !owned.command.cwd.is_absolute()
    {
        return Err(CustodyError::InvalidRecord);
    }
    let payload = serde_json::to_vec(&message).map_err(|_| CustodyError::InvalidRecord)?;
    crate::process_os::handoff::spawn(
        &owned.lease.lock_directory,
        &publication.directory,
        owned.lease.lease.as_ref().ok_or(CustodyError::Fenced)?,
        &publication.custody,
        owned
            .command
            .profile_file
            .as_ref()
            .map(crate::ProfileFile::file),
        &owned.command.environment,
        &owned.command.config_environment,
        &payload,
        Duration::from_micros(
            owned
                .handshake
                .remaining(owned.clock.mono_now())
                .as_micros(),
        ),
        owned.cancel,
    )?;
    // The guardian owns custody after ACK. Retaining this duplicate would make
    // the descendant lease-absence proof depend on requester lifetime.
    drop(owned.lease.lease.take());
    Ok(())
}

/// Composition seam for Run's bounded provider decoder and confinement filter.
#[derive(Default)]
pub struct GuardianHooks<'a> {
    /// Host disk policy, sampled only while this attempt has a live provider.
    pub disk: Option<&'a dyn DiskPressure>,
    /// Resource sampling cadence selected from `[domains.run]`; `None` uses the compiled default.
    pub resource_sample_interval: Option<Duration>,
    pub children: Option<&'a dyn ChildRegistry>,
    /// Run's terminal transaction. Without one, custody stays fenced after
    /// exit until an explicit recovery verifies Run's release attestations.
    pub holder: Option<&'a dyn Holder>,
    #[cfg(feature = "fixture")]
    pub observer: Option<&'a dyn ProcessObserver>,
    #[cfg(feature = "fixture")]
    pub exit_publisher: Option<&'a dyn ExitPublisher>,
    /// Replaces the system wall clock that bounds the attempt, so a test can
    /// move it as a sleeping host does.
    #[cfg(feature = "fixture")]
    pub wall_clock: Option<&'a dyn Clock>,
    pub stdout_tee: Option<&'a dyn provenant_os::exec::NonblockingSink>,
    pub stderr_tee: Option<&'a dyn provenant_os::exec::NonblockingSink>,
    pub stdout_decoder: Option<&'a dyn provenant_os::capture::FrameDecoder>,
    pub stderr_decoder: Option<&'a dyn provenant_os::capture::FrameDecoder>,
    pub stdout_filter: Option<&'a mut dyn provenant_os::exec::CaptureFilter>,
    pub stderr_filter: Option<&'a mut dyn provenant_os::exec::CaptureFilter>,
}
/// The host resolves the same free-space floor used by admission and GC.
/// Samples are advisory and never authorise signalling a running build.
pub trait DiskPressure {
    fn below_floor(&self, context: &[u8]) -> io::Result<bool>;
}

/// The holder role: the composed terminal publisher runs inside the guardian,
/// after durable exit and completed cleanup. The writer reservation remains,
/// but its kernel lock is released before external settlement commands run.
/// It must durably publish Run's
/// `terminal-commit.json`, `quarantine/manifest.json` and `release.json` in the
/// attempt directory before returning. Supervisor then verifies those exact
/// scoped bytes and clears the fence; any failure leaves the fence for
/// recovery. Supervisor never interprets the context or the terminal outcome.
///
/// `admit` runs first, after custody is accepted and before the provider
/// spawns, so a queued attempt waits for host admission inside its own
/// guardian. An error publishes a never-spawned exit and still calls `terminal`,
/// except when the attempt directory is provably removed: there is nowhere to
/// publish the exit or settle, so only the writer reservation remains.
pub trait Holder {
    /// Host graph evidence: unknown or live nested work must prevent escapee stops.
    fn has_live_children(&self, _context: &[u8]) -> Result<bool> {
        Ok(false)
    }
    fn admit(&self, context: &[u8], attempt: &Directory) -> Result<()>;
    /// Stop host-side submission services before descendant cleanup begins.
    /// Failure is bookkeeping evidence, never a provider stop reason.
    fn provider_exited(&self, _context: &[u8], _reason: Option<machine::StopReason>) -> Result<()> {
        Ok(())
    }
    /// Called when live token usage reaches 90% of the token budget, so the
    /// lane can be told to commit its work before reaching the budget stop.
    fn budget_warning(&self, _context: &[u8], _tokens: u64, _budget: u64) -> Result<()> {
        Ok(())
    }
    /// Read-only clean-point evidence. Unknown state is not a clean point.
    fn budget_clean_point(&self, _context: &[u8]) -> Result<bool> {
        Ok(false)
    }
    /// Durable overrun evidence, also used by status and chair notification.
    fn budget_crossed(&self, _context: &[u8], _tokens: u64, _budget: u64) -> Result<()> {
        Ok(())
    }
    fn terminal(
        &self,
        context: &[u8],
        attempt: &Directory,
        exit: &ExitObservation,
        custody: &crate::custody::Recovery,
    ) -> Result<()>;
}
/// Final reservation and immutable exit publication share one storage seam.
pub trait ExitPublisher {
    fn tracking(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
        if bytes.len() as u64 > crate::custody::RECORD_LIMIT {
            return Err(CustodyError::InvalidRecord);
        }
        Ok(directory.atomic_write(
            OsStr::new("reservation.json"),
            ".custody-",
            bytes,
            PublishMode::Replace,
        )?)
    }

    fn commit_reservation(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
        Ok(directory.atomic_write(
            OsStr::new("reservation.json"),
            ".reservation-",
            bytes,
            PublishMode::Replace,
        )?)
    }

    fn checkpoint(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
        Ok(directory.atomic_write(
            OsStr::new("process.json"),
            ".checkpoint-",
            bytes,
            PublishMode::Replace,
        )?)
    }

    fn publish(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication>;

    fn sync_visible(&self, directory: &Directory) -> Result<()> {
        directory.sync().map_err(Into::into)
    }
}
struct SystemExitPublisher;
impl ExitPublisher for SystemExitPublisher {
    fn publish(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
        Ok(directory.atomic_write(
            OsStr::new("process-exit.json"),
            ".exit-",
            bytes,
            PublishMode::NoReplace,
        )?)
    }
}

/// Run reads its generation-bound children journal at this real storage seam.
pub trait ChildRegistry {
    fn registered_children(&self) -> io::Result<Vec<RegisteredChild>>;
}
/// Registration alone never proves custody; sys must verify the held lock too.
pub struct RegisteredChild {
    registration: crate::census::OwnerRegistration,
    custody: provenant_os::fs::FileHandle,
}
impl RegisteredChild {
    pub fn new(identity: ProcessIdentity, custody: provenant_os::fs::FileHandle) -> Result<Self> {
        identity
            .validate()
            .map_err(|_| CustodyError::InvalidRecord)?;
        Ok(Self {
            registration: crate::census::OwnerRegistration::from_validated_registration(identity),
            custody,
        })
    }
}
fn adopt_profile(
    file: Option<std::fs::File>,
    identity: Option<crate::ProfileIdentity>,
    argv: &[OsString],
) -> io::Result<Option<crate::ProfileFile>> {
    let profile = match (file, identity) {
        (Some(file), Some(identity)) => Some(crate::ProfileFile::inherit(file, identity)?),
        (None, None) => None,
        _ => {
            return Err(io::Error::other(
                "sandbox profile descriptor missing from handoff; launch refused",
            ));
        }
    };
    validate_profile_command(profile.as_ref(), argv)?;
    Ok(profile)
}

fn validate_profile_command(
    profile: Option<&crate::ProfileFile>,
    argv: &[OsString],
) -> io::Result<()> {
    match profile {
        Some(profile) => profile.verify_command(argv),
        None if argv.first().is_some_and(|p| p == "/usr/bin/sandbox-exec") => Err(
            io::Error::other("sandbox profile descriptor missing; launch refused"),
        ),
        None => Ok(()),
    }
}

/// Only `internal supervise` calls this, with descriptors inherited by handoff.
/// It cannot read a launch specification from a public file or stdin.
pub fn supervise(args: &[OsString], hooks: GuardianHooks<'_>) -> Result<()> {
    let mut inherited = crate::process_os::handoff::inherit(args)?;
    let payload =
        crate::process_os::handoff::receive(&mut inherited.channel, Duration::from_secs(10))?;
    let mut message: LaunchMessage =
        decode_v1(payload.as_slice(), LAUNCH_LIMITS).map_err(|_| CustodyError::InvalidRecord)?;
    let argv_check: Vec<_> = message
        .argv
        .iter()
        .cloned()
        .map(OsString::from_vec)
        .collect();
    let profile_file = adopt_profile(
        inherited.profile.take(),
        message.profile.take(),
        &argv_check,
    );
    let profile_file = match profile_file {
        Ok(profile) => profile,
        Err(error) => {
            crate::process_os::handoff::send(
                &mut inherited.channel,
                b"sandbox-profile-transport-refused-v1",
                Duration::from_secs(10),
                &CancelToken::new(),
            )?;
            return Err(error.into());
        }
    };
    message.capture.validate()?;
    let evidence = crate::custody::attempt_evidence(&inherited.writer_directory, message.scope)?;
    let mut reservation: Reservation = read_record(&evidence, "reservation.json", RECORD_LIMIT)?;
    reservation.validate()?;
    if message.priority > 19 || !valid_resolved_route(&message.resolved_route) {
        return Err(CustodyError::InvalidRecord);
    }
    let message_binding =
        serde_json::to_value(&message.binding).map_err(|_| CustodyError::InvalidRecord)?;
    let reservation_binding =
        serde_json::to_value(&reservation.binding).map_err(|_| CustodyError::InvalidRecord)?;
    if reservation.scope != message.scope
        || reservation.guardian.is_some()
        || message_binding != reservation_binding
    {
        return Err(CustodyError::Fenced);
    }
    #[cfg(feature = "fixture")]
    let observer = hooks.observer.unwrap_or(&SystemProcessObserver);
    #[cfg(not(feature = "fixture"))]
    let observer: &dyn ProcessObserver = &SystemProcessObserver;
    if observer
        .boot_identity()
        .map_err(|_| CustodyError::UnknownIdentity)?
        != reservation.boot
    {
        return Err(CustodyError::UnknownIdentity);
    }
    let guardian = match observer.observe(i64::from(std::process::id())) {
        ProcessObservation::Present(fact)
            if fact.identity.session_id == i64::from(std::process::id()) =>
        {
            fact.identity
        }
        _ => return Err(CustodyError::UnknownIdentity),
    };
    reservation.guardian = Some(guardian.clone());
    Publication::require_durable(write_record(
        &evidence,
        "reservation.json",
        &reservation,
        PublishMode::Replace,
    )?)?;
    let argv: Vec<_> = message.argv.into_iter().map(OsString::from_vec).collect();
    let env: Vec<_> = message
        .environment
        .into_iter()
        .map(|(name, value)| (OsString::from_vec(name), OsString::from_vec(value)))
        .collect();
    let cwd = PathBuf::from(OsString::from_vec(message.cwd));
    if !cwd.is_absolute() || argv.is_empty() {
        return Err(CustodyError::InvalidRecord);
    }
    #[derive(Serialize)]
    struct Summary<'a> {
        schema_version: u64,
        scope: Scope,
        key: &'a provenant_records::WorkKey,
        resolved_route: &'a str,
        permission_plan_digest: &'a provenant_core::Digest,
        requester: provenant_core::ActorId,
        binding: &'a StoredWriterBinding,
        executable_snapshot: ExecutableSnapshot,
        profile: Option<&'a crate::ProfileIdentity>,
        executable_native_bytes: &'a [u8],
        environment_names_native_bytes: Vec<&'a [u8]>,
        capture: CapturePlan,
    }
    // Omit all argv values, stdin, environment values and the replayable plan.
    let summary = Summary {
        schema_version: 1,
        scope: message.scope,
        key: &message.key,
        resolved_route: &message.resolved_route,
        permission_plan_digest: &message.permission_plan_digest,
        requester: message.requester,
        binding: &message.binding,
        executable_snapshot: message.executable_snapshot,
        profile: profile_file.as_ref().map(crate::ProfileFile::identity),
        executable_native_bytes: argv
            .first()
            .map(|arg| arg.as_bytes())
            .ok_or(CustodyError::InvalidRecord)?,
        environment_names_native_bytes: env.iter().map(|(name, _)| name.as_bytes()).collect(),
        capture: message.capture,
    };
    Publication::require_durable(write_record(
        &inherited.attempt_directory,
        "launch.json",
        &summary,
        PublishMode::NoReplace,
    )?)?;
    let reactor = crate::process_os::provider::GuardianReactor::install()?;
    #[cfg(feature = "fixture")]
    let exit_publisher = hooks.exit_publisher;
    #[cfg(not(feature = "fixture"))]
    let exit_publisher = None;
    #[cfg(target_os = "macos")]
    let writer_identity = inherited.writer.metadata()?;
    // Open a separate description while the original entry is still linked.
    // It can prove inherited descriptions are gone even if the namespace is
    // later removed; it cannot authorise releasing or clearing the fence.
    #[cfg(target_os = "macos")]
    let writer_retirement_probe = inherited
        .writer_directory
        .open_read(OsStr::new("writer.lock"))
        .ok()
        .filter(|probe| {
            probe.metadata().is_ok_and(|metadata| {
                metadata.links == 1 && metadata.same_identity(&writer_identity)
            })
        });
    // Both locks and the durable pre-spawn reservation are held before ACK.
    crate::process_os::handoff::send(
        &mut inherited.channel,
        b"custody-accepted-v1",
        Duration::from_secs(10),
        &CancelToken::new(),
    )?;
    drop(inherited.channel);
    let mut driver = Driver {
        policy: message.policy,
        spared: crate::census::SparingTracker::default(),
        tracked: std::collections::BTreeSet::new(),
        history: std::collections::BTreeSet::new(),
        history_complete: true,
        last_tracking: None,
        journal_warning_active: false,
        protected_commands: message.protected_commands,
        settlement_evidence: SettlementEvidence::Unresolved,
        reaped: Vec::new(),
        omitted_reaped: 0,
        delivered_signals: Vec::new(),
        failed_deliveries: Vec::new(),
        omitted_failed_deliveries: 0,
        protected: std::collections::BTreeSet::new(),
        registry: hooks.children,
        descendant_stop_allowed: false,
        escapee_stop_allowed: false,
        attempt: inherited.attempt_directory,
        writer_dir: evidence,
        lock_dir: inherited.writer_directory,
        reservation,
        writer: Some(inherited.writer),
        #[cfg(target_os = "macos")]
        writer_identity,
        #[cfg(target_os = "macos")]
        writer_retirement_probe,
        _custody: Some(inherited.custody),
        guardian,
        requester: message.requester,
        executable_snapshot: message.executable_snapshot,
        reactor,
        observer,
        exit_publisher,
        holder: hooks.holder,
        holder_context: message.holder_context,
        lane_ports: message.lane_ports,
        secret_input: message.secret_input,
        profile_file,
        priority: message.priority,
        #[cfg(feature = "fixture")]
        fixture_owner: std::env::var(crate::FIXTURE_OWNER_ENV)
            .ok()
            .and_then(|value| serde_json::from_str(&value).ok()),
    };
    driver.run(&argv, &env, &cwd, &message.stdin, message.capture, hooks)
}

struct Driver<'a> {
    policy: provenant_records::policy::Policy,
    attempt: Directory,
    writer_dir: Directory,
    lock_dir: Directory,
    reservation: Reservation,
    writer: Option<provenant_os::fs::LockGuard>,
    #[cfg(target_os = "macos")]
    writer_identity: provenant_os::fs::Metadata,
    #[cfg(target_os = "macos")]
    writer_retirement_probe: Option<provenant_os::fs::FileHandle>,
    _custody: Option<provenant_os::fs::LockGuard>,
    guardian: ProcessIdentity,
    requester: provenant_core::ActorId,
    executable_snapshot: ExecutableSnapshot,
    reactor: crate::process_os::provider::GuardianReactor,
    observer: &'a dyn ProcessObserver,
    exit_publisher: Option<&'a dyn ExitPublisher>,
    holder: Option<&'a dyn Holder>,
    holder_context: Vec<u8>,
    lane_ports: Option<provenant_os::lane_ports::Plan>,
    secret_input: Option<provenant_os::exec::SecretInput>,
    priority: u8,
    profile_file: Option<crate::ProfileFile>,
    #[cfg(feature = "fixture")]
    fixture_owner: Option<ProcessIdentity>,
    spared: crate::census::SparingTracker,
    tracked: std::collections::BTreeSet<ProcessIdentity>,
    history: std::collections::BTreeSet<ProcessIdentity>,
    history_complete: bool,
    last_tracking: Option<Vec<u8>>,
    journal_warning_active: bool,
    protected_commands: Vec<String>,
    settlement_evidence: SettlementEvidence,
    reaped: Vec<ProcessIdentity>,
    omitted_reaped: u64,
    delivered_signals: Vec<i64>,
    failed_deliveries: Vec<FailedSignalDelivery>,
    omitted_failed_deliveries: u64,
    protected: std::collections::BTreeSet<ProcessIdentity>,
    registry: Option<&'a dyn ChildRegistry>,
    descendant_stop_allowed: bool,
    /// The registry permits an exact sandbox escapee stop this tick.
    /// The host graph-child check is deferred until a control action needs it.
    /// Unproven registered owners stay protected. Independent of
    /// `descendant_stop_allowed`.
    escapee_stop_allowed: bool,
}
/// Whether a writer's latest reported context has reached its context handoff
/// threshold (`lanes.context_handoff_tokens`). 0 turns the threshold off, and a provider with no context figure
/// (`None`) is never stopped for it.
fn context_handoff_due(threshold: u64, context: Option<u64>) -> bool {
    threshold > 0 && context.is_some_and(|context| context >= threshold)
}
/// Calls tracked by key before the sink stops de-duplicating and only adds.
const USAGE_KEYS: usize = 65_536;
#[derive(Default)]
struct Events {
    terminal: std::sync::atomic::AtomicBool,
    rejected: std::sync::atomic::AtomicBool,
    usage: std::sync::Mutex<Usage>,
}
/// Live token usage: one figure per provider call, summed, and the context
/// the latest call that reported one sent.
#[derive(Default)]
struct Usage {
    calls: std::collections::HashMap<u64, u64>,
    total: u64,
    context: Option<u64>,
}
impl Usage {
    fn report(&mut self, key: u64, tokens: u64, context: Option<u64>) {
        if context.is_some() {
            self.context = context;
        }
        let before = if self.calls.len() < USAGE_KEYS || self.calls.contains_key(&key) {
            self.calls.insert(key, tokens).unwrap_or(0)
        } else {
            0
        };
        self.total = self.total.saturating_sub(before).saturating_add(tokens);
    }
}
impl Events {
    /// Tokens the provider has reported so far.
    fn tokens(&self) -> u64 {
        self.usage.lock().map_or(0, |usage| usage.total)
    }
    /// The context the latest call sent, where the provider reports it.
    fn context(&self) -> Option<u64> {
        self.usage.lock().ok().and_then(|usage| usage.context)
    }
}
impl provenant_os::capture::DecodedEventSink for Events {
    fn try_send(
        &self,
        event: provenant_os::capture::FrameEvent,
    ) -> std::result::Result<(), provenant_os::exec::QueueLoss> {
        use std::sync::atomic::Ordering;
        match event {
            provenant_os::capture::FrameEvent::Terminal => {
                self.terminal.store(true, Ordering::Relaxed)
            }
            provenant_os::capture::FrameEvent::ProtocolRejected => {
                self.rejected.store(true, Ordering::Relaxed)
            }
            provenant_os::capture::FrameEvent::Usage {
                key,
                tokens,
                context,
            } => {
                if let Ok(mut usage) = self.usage.lock() {
                    usage.report(key, tokens, context);
                }
            }
            provenant_os::capture::FrameEvent::Progress => {}
        }
        Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
#[error("no free lane port block; domains.fabric.lanes.lane_port_blocks={blocks} ({}); raise or disable this setting", match origin { provenant_records::diagnostic::PortSettingSource::Configuration => "host configuration", provenant_records::diagnostic::PortSettingSource::ShippedDefault => "compiled default", provenant_records::diagnostic::PortSettingSource::RecordedPlan => "recorded plan" })]
struct LanePortRefusal {
    blocks: u32,
    origin: provenant_records::diagnostic::PortSettingSource,
}
impl LanePortRefusal {
    fn fact(&self) -> provenant_records::diagnostic::ResourceFact {
        provenant_records::diagnostic::ResourceFact::LanePorts {
            blocks: self.blocks,
            origin: self.origin,
        }
    }
}

fn lane_port_allocation(
    policy: &provenant_records::policy::Policy,
    plan: Option<&provenant_os::lane_ports::Plan>,
) -> std::result::Result<(Option<provenant_os::lane_ports::Lease>, Option<String>), LanePortRefusal>
{
    let Some(plan) = plan else {
        return Ok((None, None));
    };
    match plan.try_acquire() {
        Ok(Some(lease)) => Ok((Some(lease), None)),
        Ok(None) => {
            use provenant_records::policy::{Effect, Facts};
            let effect = policy.check("PORT_BLOCKS", Facts::violation("lane port pool exhausted"));
            match effect {
                Effect::Proceed => return Ok((None, None)),
                Effect::Warn(message) => return Ok((None, Some(message))),
                Effect::Refuse(_) => {}
            }
            use provenant_records::diagnostic::PortSettingSource;
            let origin = match plan.blocks_origin {
                Some(provenant_os::lane_ports::BlocksOrigin::CompiledDefault) => {
                    PortSettingSource::ShippedDefault
                }
                Some(provenant_os::lane_ports::BlocksOrigin::HostConfig) => {
                    PortSettingSource::Configuration
                }
                None => PortSettingSource::RecordedPlan,
            };
            Err(LanePortRefusal {
                blocks: plan.blocks,
                origin,
            })
        }
        Err(_) => {
            let effect = policy.check(
                "GUARDIAN_LANE_PORT_ALLOCATION_NONE",
                provenant_records::policy::Facts::violation(
                    "lane port allocation failed; continuing without a block",
                ),
            );
            let warning = match effect {
                provenant_records::policy::Effect::Warn(message) => Some(message),
                _ => None,
            };
            Ok((None, warning))
        }
    }
}

fn without_lane_ports(env: &[(OsString, OsString)]) -> Vec<(OsString, OsString)> {
    env.iter()
        .filter(|(name, _)| {
            name != provenant_os::lane_ports::BASE_ENV
                && name != provenant_os::lane_ports::COUNT_ENV
        })
        .cloned()
        .collect()
}

/// `env` with the lane's port block set, replacing any value it carried.
fn with_lane_ports(
    env: &[(OsString, OsString)],
    lease: &provenant_os::lane_ports::Lease,
) -> Vec<(OsString, OsString)> {
    let ours = lease.environment();
    let mut out: Vec<_> = env
        .iter()
        .filter(|(name, _)| !ours.iter().any(|(own, _)| name == *own))
        .cloned()
        .collect();
    out.extend(
        ours.into_iter()
            .map(|(name, value)| (name.into(), value.into())),
    );
    out
}

fn with_process_tree_path(
    env: &[(OsString, OsString)],
    path: Option<&Path>,
) -> Vec<(OsString, OsString)> {
    let mut env = env.to_vec();
    env.retain(|(name, _)| name != crate::process_tree::ENV);
    if let Some(path) = path {
        env.push((crate::process_tree::ENV.into(), path.as_os_str().to_owned()));
    }
    env
}
impl Driver<'_> {
    // The guardian owns process lifecycle and its elapsed-time budgets.
    #[allow(clippy::disallowed_methods)]
    fn run(
        &mut self,
        argv: &[OsString],
        env: &[(OsString, OsString)],
        cwd: &std::path::Path,
        input: &[u8],
        plan: CapturePlan,
        hooks: GuardianHooks<'_>,
    ) -> Result<()> {
        let mut warnings = Vec::new();
        let mut omitted_warnings = 0_u64;
        let resource_sample_interval = hooks
            .resource_sample_interval
            .unwrap_or(crate::process_tree::DEFAULT_SAMPLE_INTERVAL)
            .max(Duration::from_millis(1));
        let resource_sample_interval_ms =
            u64::try_from(resource_sample_interval.as_millis()).unwrap_or(u64::MAX);
        // Keep the original inode and path even if the caller later removes
        // its state. A rename alone must not end a publication retry.
        let writer_evidence_identity = DirectoryIdentity::of(&self.writer_dir).ok();
        let writer_evidence_path = self.writer_dir.current_path().ok();
        let warn = |warnings: &mut Vec<String>, omitted: &mut u64, label: &str| {
            if warnings.len() < 24 {
                warnings.push(label.to_owned());
            } else {
                *omitted = omitted.saturating_add(1);
            }
        };
        if let Some(holder) = self.holder
            && let Err(error) = holder.admit(&self.holder_context, &self.attempt)
        {
            return self.publish_never_spawned(ProcessExit::admission_refused(
                self.reservation.scope,
                &error,
            ));
        }
        // Held until this function returns, which is after the provider and
        // its descendants are settled; the kernel drops it if we die first.
        let (ports, port_warning) =
            match lane_port_allocation(&self.policy, self.lane_ports.as_ref()) {
                Ok(result) => result,
                Err(reason) => return self.never_spawned(&reason.to_string(), Some(reason.fact())),
            };
        if let Some(warning) = &port_warning {
            warn(&mut warnings, &mut omitted_warnings, warning);
        }
        let adjusted_env;
        let env = if let Some(lease) = &ports {
            adjusted_env = with_lane_ports(env, lease);
            adjusted_env.as_slice()
        } else if port_warning.is_some() {
            adjusted_env = without_lane_ports(env);
            adjusted_env.as_slice()
        } else {
            env
        };
        // Replace any caller value with this guardian's own held attempt path.
        let memory_path = crate::process_tree::snapshot_path(&self.attempt).ok();
        let env = with_process_tree_path(env, memory_path.as_deref());
        if memory_path.is_some() {
            let _ = crate::process_tree::publish(&self.attempt, None);
        }
        let env = env.as_slice();
        let started = Instant::now();
        let system = provenant_os::clock::SystemClock::new();
        #[cfg(feature = "fixture")]
        let clock = hooks.wall_clock.unwrap_or(&system);
        #[cfg(not(feature = "fixture"))]
        let clock: &dyn Clock = &system;
        let wall_now = || clock.wall_now().ok();
        let wall_started = wall_now();
        let budgets =
            Budgets::with_cleanup(plan.wall_ms, plan.root_flush_ms, plan.descendant_grace_ms);
        let mut state = State::prepared(budgets);
        let mut budget_warned = false;
        let mut budget_stop_sent = false;
        let mut budget_crossed = false;
        let mut budget_sample = None;
        let mut budget_clean = false;
        let mut context_stop_sent = false;
        let spawn_result = self.spawn_provider(
            argv,
            env,
            cwd,
            input,
            plan,
            ports.as_ref().map(provenant_os::lane_ports::Lease::guard),
        );
        let mut child = match spawn_result {
            Ok(child) => child,
            Err(error) => {
                return self.never_spawned(
                    if crate::process_os::profile::is_transport_error(&error) {
                        "sandbox profile transport refused before provider launch"
                    } else if error.kind() == io::ErrorKind::InvalidData {
                        "provider executable snapshot changed"
                    } else {
                        "provider spawn failed"
                    },
                    None,
                );
            }
        };
        if let Some(error) = child.priority_error {
            warn(
                &mut warnings,
                &mut omitted_warnings,
                &format!("provider priority unavailable or unchanged: OS error {error}"),
            );
        } else if child.priority.is_none() {
            warn(
                &mut warnings,
                &mut omitted_warnings,
                "provider priority evidence unavailable",
            );
        }
        self.reservation.launch_state = LaunchState::Spawned;
        self.reservation.provider = child.identity().cloned();
        let mut memory = memory_path
            .as_ref()
            .and(self.reservation.provider.as_ref())
            .and_then(|root| {
                crate::process_tree::Sampler::start(
                    &self.attempt,
                    root,
                    self.reservation.scope.attempt,
                    // Without a profile the lane has no sandbox mark to test.
                    self.profile_file.is_some(),
                    resource_sample_interval,
                )
            });
        if write_record(
            &self.writer_dir,
            "reservation.json",
            &self.reservation,
            PublishMode::Replace,
        )
        .and_then(|publication| publication.require_durable().map_err(Into::into))
        .is_err()
        {
            warn(
                &mut warnings,
                &mut omitted_warnings,
                "provider identity publication failed",
            );
        }
        state = machine::step(state, Event::Started, Tick::at(0)).0;
        let out_events = Events::default();
        let err_events = Events::default();
        let out_decoder =
            hooks
                .stdout_decoder
                .map(|decoder| provenant_os::capture::FrameDelivery::Provider {
                    decoder,
                    events: &out_events,
                });
        let err_decoder =
            hooks
                .stderr_decoder
                .map(|decoder| provenant_os::capture::FrameDelivery::Provider {
                    decoder,
                    events: &err_events,
                });
        let mut out = provenant_os::capture::StreamCapture::new(
            stream_policy(plan.stdout, hooks.stdout_tee),
            out_decoder,
            hooks.stdout_filter,
        )
        .map_err(|_| CustodyError::InvalidRecord)?;
        let mut err = provenant_os::capture::StreamCapture::new(
            stream_policy(plan.stderr, hooks.stderr_tee),
            err_decoder,
            hooks.stderr_filter,
        )
        .map_err(|_| CustodyError::InvalidRecord)?;
        let mut input_offset = 0;
        let mut stop_reason = None;
        let mut cancel_delivered_before_exit = false;
        let mut exit = None;
        let mut host_closed = false;
        let mut stop_at = None;
        let mut killed = false;
        let mut terminal = false;
        let mut last_checkpoint = None;
        let mut last_observation_ms: Option<u64> = None;
        let mut last_cpu_observation_ms = 0;
        let mut last_disk_sample_ms = None;
        let mut disk_warning_active = false;
        let mut last_cancel_ms = None;
        let mut cancel_pending = false;
        let mut cancel_watched = self
            .reactor
            .watch_cancellation_directory(&self.attempt)
            .is_ok();
        let mut signal_pending = false;
        let mut last_stdout_bytes = 0_u64;
        let mut last_stderr_bytes = 0_u64;
        let mut last_output_ms = None;
        let mut last_output_checkpoint_ms = None;
        let mut last_output_at: Option<WallTime> = None;
        let mut provider_cpu_micros = None;
        let mut previous_cpu_micros = None;
        let mut last_cpu_progress_ms = None;
        let mut census_warning_active = false;
        let mut last_full_census_ms: Option<u64> = None;
        let mut last_census_ms: Option<u64> = None;
        let mut scoped_fallback_recorded = false;
        let mut census_event_pending = false;
        let mut census_event_failed = false;
        let mut census_reactor_failed = false;
        let mut reactor_warning_recorded = false;
        let mut process_watch_unavailable = false;
        let mut census_retry = false;
        let mut last_journal_attempt_ms = None;
        let mut cleanup_complete;
        let mut drain_deadline = None;
        let mut cleanup_pending = false;
        // Try once at launch, then only after readiness. A failed reactor
        // retains the former nonblocking IO fallback.
        let mut io_ready = true;
        loop {
            let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            // WNOWAIT always runs before cancellation, signals or deadlines.
            match child.observe() {
                Ok(Some(status)) => {
                    exit = Some(status);
                    drain_deadline.get_or_insert(now.saturating_add(plan.root_flush_ms));
                }
                Ok(None) => {}
                Err(_) => warn(
                    &mut warnings,
                    &mut omitted_warnings,
                    "child exit observation failed",
                ),
            }
            if exit.is_some() && !host_closed {
                drop(memory.take());
                host_closed = true;
                if let Some(holder) = self.holder
                    && holder
                        .provider_exited(&self.holder_context, stop_reason)
                        .is_err()
                {
                    warn(
                        &mut warnings,
                        &mut omitted_warnings,
                        "host child launcher failed",
                    );
                }
            }
            // Exit can race a timer-only wake. Keep the final drain even
            // when the last poll saw no bytes, including a zero flush budget.
            if io_ready || exit.is_some() {
                out.read(&mut child.stdout);
                err.read(&mut child.stderr);
            }
            let stdout_bytes = out.outcome().total_bytes;
            let stderr_bytes = err.outcome().total_bytes;
            let output_progress =
                stdout_bytes > last_stdout_bytes || stderr_bytes > last_stderr_bytes;
            if output_progress {
                last_output_ms = Some(now);
                if last_output_checkpoint_ms.is_none_or(|last| now.saturating_sub(last) >= 5_000) {
                    last_output_at = wall_now();
                    last_output_checkpoint_ms = Some(now);
                }
            }
            last_stdout_bytes = stdout_bytes;
            last_stderr_bytes = stderr_bytes;
            let cpu_sample_due =
                now.saturating_sub(last_cpu_observation_ms) >= resource_sample_interval_ms;
            let cpu_progress = if cpu_sample_due {
                last_cpu_observation_ms = now;
                provider_cpu_micros = self.provider_cpu_micros();
                let progressed = provider_cpu_micros
                    .zip(previous_cpu_micros)
                    .is_some_and(|(current, previous)| current > previous);
                if let Some(current) = provider_cpu_micros {
                    previous_cpu_micros = Some(current);
                    if progressed {
                        last_cpu_progress_ms = Some(now);
                    }
                }
                progressed
            } else {
                false
            };
            if io_ready && let Some(stdin) = child.stdin.as_mut() {
                if input_offset == input.len() {
                    child.stdin = None;
                } else {
                    match stdin.write(
                        input
                            .get(input_offset..)
                            .ok_or(CustodyError::InvalidRecord)?,
                    ) {
                        Ok(count) => input_offset += count,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => {
                            child.stdin = None;
                            warn(
                                &mut warnings,
                                &mut omitted_warnings,
                                "provider stdin closed early",
                            );
                        }
                    }
                }
            }
            let polled_signal = match self.reactor.wait(&[], Duration::ZERO) {
                Ok(event) => {
                    cancel_pending |= event.cancel_changed;
                    cancel_watched &= !event.cancel_watch_failed;
                    census_event_pending |= event.process_changed;
                    census_event_failed |= event.process_overflow;
                    event.supervisor_signal
                }
                Err(_) => {
                    cancel_watched = false;
                    census_event_failed = true;
                    census_reactor_failed = true;
                    if !reactor_warning_recorded {
                        warn(
                            &mut warnings,
                            &mut omitted_warnings,
                            "signal observation failed",
                        );
                        reactor_warning_recorded = true;
                    }
                    false
                }
            };
            let cancel_due = std::mem::take(&mut cancel_pending)
                || last_cancel_ms.is_none_or(|last| {
                    now.saturating_sub(last) >= cancel_poll_interval(cancel_watched)
                });
            #[cfg(feature = "fixture")]
            let fixture_owner_gone = self.fixture_owner.as_ref().is_some_and(|owner| {
                match self.observer.observe(owner.pid) {
                    ProcessObservation::Present(fact) => fact.identity != *owner,
                    ProcessObservation::Absent => true,
                    ProcessObservation::Unknown(_) => false,
                }
            });
            #[cfg(feature = "fixture")]
            let request = if fixture_owner_gone {
                last_cancel_ms = Some(now);
                Some(machine::StopReason::ExplicitCancellation)
            } else if cancel_due {
                last_cancel_ms = Some(now);
                self.read_cancel(&mut warnings, &mut omitted_warnings)
            } else {
                None
            };
            #[cfg(not(feature = "fixture"))]
            let request = if cancel_due {
                last_cancel_ms = Some(now);
                self.read_cancel(&mut warnings, &mut omitted_warnings)
            } else {
                None
            };
            let signal = std::mem::replace(&mut signal_pending, false) || polled_signal;
            use std::sync::atomic::Ordering;
            let newly_terminal = !terminal
                && (out_events.terminal.load(Ordering::Relaxed)
                    || err_events.terminal.load(Ordering::Relaxed));
            terminal |= newly_terminal;
            let rejected = out_events.rejected.load(Ordering::Relaxed)
                || err_events.rejected.load(Ordering::Relaxed);
            let tokens = out_events.tokens().saturating_add(err_events.tokens());
            if plan.token_budget > 0
                && !budget_warned
                && tokens >= plan.token_budget / 10 * 9
                && tokens <= plan.token_budget
            {
                budget_warned = true;
                warn(
                    &mut warnings,
                    &mut omitted_warnings,
                    "token budget 90% used",
                );
                if let Some(holder) = self.holder {
                    let _ = holder.budget_warning(&self.holder_context, tokens, plan.token_budget);
                }
            }
            let crossed = plan.token_budget > 0 && tokens > plan.token_budget;
            if crossed && !budget_crossed {
                budget_crossed = true;
                warn(&mut warnings, &mut omitted_warnings, "token budget crossed");
                if let Some(holder) = self.holder
                    && holder
                        .budget_crossed(&self.holder_context, tokens, plan.token_budget)
                        .is_err()
                {
                    warn(
                        &mut warnings,
                        &mut omitted_warnings,
                        "budget overrun notice could not be delivered",
                    );
                }
            }
            // Inspect Git only when usage changes, not on every reactor wake.
            if crossed
                && plan.budget_action != provenant_records::BudgetAction::Warn
                && budget_sample != Some(tokens)
                && !terminal
                && exit.is_none()
            {
                budget_sample = Some(tokens);
                budget_clean = self.holder.is_some_and(|holder| {
                    holder
                        .budget_clean_point(&self.holder_context)
                        .unwrap_or(false)
                });
            }
            let over_budget = budget_boundary(
                plan.token_budget,
                plan.budget_action,
                tokens,
                budget_clean,
                terminal || exit.is_some(),
            )
            .is_some()
                && !budget_stop_sent;
            let context_reached = context_handoff_due(
                plan.context_handoff,
                out_events.context().or_else(|| err_events.context()),
            ) && !context_stop_sent
                && !budget_stop_sent
                && !over_budget
                && !terminal
                && exit.is_none();
            let event = if signal {
                Event::SupervisorSignal
            } else if rejected {
                Event::ProtocolRejected
            } else if let Some(reason) = request {
                match reason {
                    machine::StopReason::ExplicitCancellation => Event::Cancel,
                    machine::StopReason::ProtocolRejected => Event::ProtocolRejected,
                    machine::StopReason::SupervisorSignal => Event::SupervisorSignal,
                    machine::StopReason::TokenBudget => Event::TokenBudget,
                    machine::StopReason::ContextHandoff => Event::ContextHandoff,
                    machine::StopReason::WallDeadline
                    | machine::StopReason::PostTerminalCleanup
                        if state.stop_attempts().iter().any(|attempt| {
                            attempt.reason == reason
                                && attempt.delivery == Some(DeliveryOutcome::Delivered)
                        }) =>
                    {
                        Event::Silence
                    }
                    machine::StopReason::WallDeadline
                    | machine::StopReason::PostTerminalCleanup => Event::RetryStop(reason),
                }
            } else if over_budget {
                budget_stop_sent = true;
                Event::TokenBudget
            } else if context_reached {
                context_stop_sent = true;
                warn(
                    &mut warnings,
                    &mut omitted_warnings,
                    "context handoff threshold reached",
                );
                Event::ContextHandoff
            } else if newly_terminal {
                Event::Terminal
            } else if output_progress || cpu_progress {
                Event::Progress
            } else {
                Event::Silence
            };
            let mut tick = Tick::at(now);
            tick.observed_exit = exit;
            // The monotonic clock stops while the host sleeps; the wall
            // deadline must not. A wall clock set backwards only adds nothing.
            tick.wall_ms = wall_started.zip(wall_now()).map_or(0, |(began, current)| {
                u64::try_from(current.as_micros().saturating_sub(began.as_micros()) / 1000)
                    .unwrap_or(0)
            });
            let (next, effects) = machine::step(state, event, tick);
            state = next;
            for effect in effects {
                match effect {
                    Effect::CloseStdin => child.stdin = None,
                    Effect::BeginCleanup(_) => cleanup_pending = true,
                    Effect::DeliverStop(reason) if exit.is_none() => {
                        stop_reason.get_or_insert(reason);
                        stop_at.get_or_insert(now);
                        let outcome = stop_delivery_outcome(child.stop(false));
                        if reason == machine::StopReason::ExplicitCancellation
                            && outcome == DeliveryOutcome::Delivered
                        {
                            cancel_delivered_before_exit = true;
                        }
                        self.record_delivery(
                            self.reservation.provider.clone(),
                            reason,
                            libc::SIGTERM,
                            outcome,
                        );
                        if outcome != DeliveryOutcome::Delivered {
                            warn(&mut warnings, &mut omitted_warnings, "TERM delivery failed");
                        }
                        let mut receipt_tick = Tick::at(now);
                        receipt_tick.delivery = Some(DeliveryReceipt { reason, outcome });
                        let (next, receipt_effects) =
                            machine::step(state, Event::Silence, receipt_tick);
                        state = next;
                        if !receipt_effects.is_empty() {
                            warn(
                                &mut warnings,
                                &mut omitted_warnings,
                                "TERM delivery receipt rejected",
                            );
                        }
                    }
                    _ => {}
                }
            }
            if let Some(at) = stop_at
                && !killed
                && now >= at.saturating_add(plan.root_flush_ms.max(plan.descendant_grace_ms))
            {
                let reason = stop_reason.unwrap_or(machine::StopReason::PostTerminalCleanup);
                let outcome = stop_delivery_outcome(child.stop(true));
                if exit.is_none()
                    && reason == machine::StopReason::ExplicitCancellation
                    && outcome == DeliveryOutcome::Delivered
                {
                    cancel_delivered_before_exit = true;
                }
                self.record_delivery(
                    self.reservation.provider.clone(),
                    reason,
                    libc::SIGKILL,
                    outcome,
                );
                if outcome != DeliveryOutcome::Delivered {
                    warn(&mut warnings, &mut omitted_warnings, "KILL delivery failed");
                }
                let mut receipt_tick = Tick::at(now);
                receipt_tick.delivery = Some(DeliveryReceipt { reason, outcome });
                let (next, receipt_effects) = machine::step(state, Event::Silence, receipt_tick);
                state = next;
                if !receipt_effects.is_empty() {
                    warn(
                        &mut warnings,
                        &mut omitted_warnings,
                        "KILL delivery receipt rejected",
                    );
                }
                killed = true;
            }
            if cleanup_pending && exit.is_some() {
                let pipes_done = child.stdout.is_none() && child.stderr.is_none();
                if pipes_done || drain_deadline.is_some_and(|deadline| now >= deadline) {
                    if !pipes_done {
                        warn(
                            &mut warnings,
                            &mut omitted_warnings,
                            "capture drain truncated",
                        );
                    }
                    // Census grants no PID signal authority. The driver holds the
                    // leader until the group census and adopted children settle.
                    cleanup_complete = match self.cleanup(
                        &mut child,
                        plan,
                        stop_reason,
                        &mut warnings,
                        &mut omitted_warnings,
                    ) {
                        Ok(complete) => complete,
                        Err(_) => {
                            warn(
                                &mut warnings,
                                &mut omitted_warnings,
                                "provider cleanup observation failed",
                            );
                            false
                        }
                    };
                    break;
                }
            }
            // The host is sampled only during this running lane. Reuse the
            // existing native custody census to find builds, never host-wide
            // command-line matching. No independent timer survives this lane.
            if exit.is_none()
                && stop_reason.is_none()
                && last_disk_sample_ms.is_none_or(|last| now.saturating_sub(last) >= 1_000)
            {
                last_disk_sample_ms = Some(now);
                if let Some(disk) = hooks.disk {
                    let warning = match disk.below_floor(&self.holder_context) {
                        Ok(true) => Some(
                            "disk space below gc.free_floor; avoid starting new builds; running builds continue",
                        ),
                        Ok(false) => None,
                        Err(_) => Some("disk floor observation failed; running builds continue"),
                    };
                    if let Some(message) = warning {
                        if !disk_warning_active {
                            warn(&mut warnings, &mut omitted_warnings, message);
                        }
                        disk_warning_active = true;
                    } else {
                        disk_warning_active = false;
                    }
                }
            }
            // Process events wake the scoped tree and provider-group walk, at
            // most every 100 ms. Without an event nothing watched has forked
            // or exited, so an idle lane walks only at the five-second full
            // backstop, which is also mandatory for cleanup. Failed events,
            // overflow, failed watches or a racing walk require a full scan.
            if census_turn_due(
                now,
                last_census_ms,
                last_full_census_ms,
                census_event_pending
                    || census_event_failed
                    || census_reactor_failed
                    || process_watch_unavailable
                    || census_retry,
            ) {
                last_census_ms = Some(now);
                census_event_pending = false;
                let event_fallback =
                    std::mem::take(&mut census_event_failed) || process_watch_unavailable;
                let full_due = census_full_due(now, last_full_census_ms, event_fallback);
                let (mut census, mut fallback) = if full_due {
                    (self.refresh_census(&mut child), false)
                } else {
                    self.refresh_live_census(&mut child)
                };
                // A saturated drain is normal during a burst; only failed watches
                // and failed scoped walks produce a fallback warning.
                fallback |= process_watch_unavailable || std::mem::take(&mut census_reactor_failed);
                if let Some(snapshot) = census.as_ref() {
                    let pids = census_watch_pids(
                        snapshot,
                        self.reservation
                            .provider
                            .iter()
                            .chain(self.history.iter())
                            .chain(self.tracked.iter())
                            .chain(self.protected.iter())
                            .chain(self.spared.spared_identities()),
                    );
                    match self.reactor.watch_processes(&pids) {
                        Ok((added, failures)) => {
                            process_watch_unavailable = failures != 0;
                            // Close the gap between observing a new process
                            // and registering its fork filter.
                            census_event_pending |= added;
                        }
                        Err(_) => process_watch_unavailable = true,
                    }
                    if process_watch_unavailable {
                        if !full_due && !fallback {
                            census = self.refresh_census(&mut child);
                        }
                        fallback = true;
                    }
                }
                if let Some(notice) =
                    census_fallback_notice(fallback, &mut scoped_fallback_recorded)
                {
                    warn(&mut warnings, &mut omitted_warnings, notice);
                }
                if census.is_some() && (full_due || fallback) {
                    last_full_census_ms = Some(now);
                }
                // A failed census keeps retrying at the walk rate.
                census_retry = census.is_none();
                if let Some(census) = census {
                    if full_due || fallback {
                        if !census.is_complete() && !census_warning_active {
                            warn(
                                &mut warnings,
                                &mut omitted_warnings,
                                "live process census incomplete",
                            );
                        }
                        census_warning_active = !census.is_complete();
                    }
                } else if !census_warning_active {
                    warn(
                        &mut warnings,
                        &mut omitted_warnings,
                        "live process census incomplete",
                    );
                    census_warning_active = true;
                }
            }
            if tracking_publication_due(now, last_journal_attempt_ms, self.journal_warning_active) {
                // Batch live history writes; cleanup still flushes synchronously.
                // Custody and the full census remain authoritative during this window.
                self.persist_tracking(&mut warnings, &mut omitted_warnings);
                last_journal_attempt_ms = Some(now);
            }
            // Process checkpoint durability is independent of census cadence.
            if last_observation_ms.is_none_or(|last| now.saturating_sub(last) >= 1_000) {
                #[derive(Serialize)]
                struct Observed<'a> {
                    schema_version: u64,
                    scope: Scope,
                    guardian: &'a ProcessIdentity,
                    provider: Option<&'a ProcessIdentity>,
                    priority: Option<i32>,
                    phase: Phase,
                    stdout_bytes: u64,
                    stderr_bytes: u64,
                    last_output_ms: Option<u64>,
                    last_output_at: Option<WallTime>,
                    observed_at: Option<WallTime>,
                    last_progress_ms: Option<u64>,
                    silent: bool,
                    silent_for_ms: Option<u64>,
                    provider_cpu_micros: Option<u64>,
                    last_cpu_progress_ms: Option<u64>,
                    publication_pending: bool,
                }
                let silent_for_ms = plan.silent_for_ms(now, state.last_progress_ms());
                let mut observed = Observed {
                    schema_version: 1,
                    scope: self.reservation.scope,
                    guardian: &self.guardian,
                    provider: self.reservation.provider.as_ref(),
                    priority: child.priority,
                    phase: state.phase(),
                    stdout_bytes: out.outcome().total_bytes,
                    stderr_bytes: err.outcome().total_bytes,
                    last_output_ms,
                    last_output_at,
                    observed_at: None,
                    last_progress_ms: state.last_progress_ms(),
                    silent: silent_for_ms.is_some(),
                    silent_for_ms,
                    provider_cpu_micros,
                    last_cpu_progress_ms,
                    publication_pending: false,
                };
                let system_publisher = SystemExitPublisher;
                // Compare facts without the observation timestamp. Quiet lanes
                // keep their last observation rather than flushing a heartbeat.
                let facts =
                    serde_json::to_vec(&observed).map_err(|_| CustodyError::InvalidRecord)?;
                if last_checkpoint.as_ref() != Some(&facts) {
                    observed.observed_at = wall_now();
                    let checkpoint =
                        serde_json::to_vec(&observed).map_err(|_| CustodyError::InvalidRecord)?;
                    if self
                        .exit_publisher
                        .unwrap_or(&system_publisher)
                        .checkpoint(&self.attempt, &checkpoint)
                        .and_then(|publication| publication.require_durable().map_err(Into::into))
                        .is_err()
                    {
                        warn(
                            &mut warnings,
                            &mut omitted_warnings,
                            "process observation publication failed",
                        );
                    } else {
                        last_checkpoint = Some(facts);
                    }
                }
                last_observation_ms = Some(now);
            }
            let watches = child.watches();
            // Pipes, signals and process events wake the reactor. Timer-only
            // work wakes at its next deadline: the cancellation backstop,
            // the one-second CPU sample and the five-second full census.
            let wait_now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let wall_ms = wall_started.zip(wall_now()).map_or(0, |(began, current)| {
                u64::try_from(current.as_micros().saturating_sub(began.as_micros()) / 1000)
                    .unwrap_or(0)
            });
            let mut wait_ms = last_cpu_observation_ms
                .saturating_add(resource_sample_interval_ms)
                .saturating_sub(wait_now);
            if let Some(last) = last_full_census_ms {
                wait_ms = wait_ms.min(
                    last.saturating_add(FULL_CENSUS_INTERVAL_MS)
                        .saturating_sub(wait_now),
                );
            }
            if let Some(last) = last_cancel_ms {
                wait_ms = wait_ms.min(
                    last.saturating_add(cancel_poll_interval(cancel_watched))
                        .saturating_sub(wait_now),
                );
            }
            if let Some(deadline) = state.deadline_in_ms(wait_now, wall_ms) {
                wait_ms = wait_ms.min(deadline);
            }
            if let Some(at) = stop_at.filter(|_| !killed) {
                wait_ms = wait_ms.min(
                    at.saturating_add(plan.root_flush_ms.max(plan.descendant_grace_ms))
                        .saturating_sub(wait_now),
                );
            }
            if cleanup_pending && let Some(deadline) = drain_deadline {
                wait_ms = wait_ms.min(deadline.saturating_sub(wait_now));
            }
            if census_event_pending
                || census_event_failed
                || census_reactor_failed
                || process_watch_unavailable
                || census_retry
            {
                wait_ms = wait_ms.min(
                    last_census_ms
                        .unwrap_or(0)
                        .saturating_add(CENSUS_INTERVAL_MS)
                        .saturating_sub(wait_now),
                );
            }
            if self.journal_warning_active {
                wait_ms = wait_ms.min(
                    last_journal_attempt_ms
                        .unwrap_or(wait_now)
                        .saturating_add(JOURNAL_RETRY_INTERVAL_MS)
                        .saturating_sub(wait_now),
                );
            }
            match self.reactor.wait(&watches, Duration::from_millis(wait_ms)) {
                Ok(event) => {
                    io_ready = event.io_ready;
                    cancel_pending |= event.cancel_changed;
                    cancel_watched &= !event.cancel_watch_failed;
                    signal_pending |= event.supervisor_signal;
                    census_event_pending |= event.process_changed;
                    census_event_failed |= event.process_overflow;
                    if event.process_changed && !census_walk_due(wait_now, last_census_ms) {
                        provenant_os::sys::wait_pause(Duration::from_millis(
                            EVENT_DRAIN_BACKOFF_MS,
                        ));
                    }
                }
                Err(_) => {
                    cancel_watched = false;
                    census_event_failed = true;
                    census_reactor_failed = true;
                    io_ready = true;
                    provenant_os::sys::wait_pause(Duration::from_millis(CENSUS_INTERVAL_MS));
                    if !reactor_warning_recorded {
                        warn(&mut warnings, &mut omitted_warnings, "guardian wait failed");
                        reactor_warning_recorded = true;
                    }
                }
            }
        }
        let mut settled =
            Tick::at(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        settled.cleanup = Some(if cleanup_complete {
            machine::CleanupObservation::Complete
        } else {
            machine::CleanupObservation::Unresolved
        });
        let (next, effects) = machine::step(state, Event::Silence, settled);
        state = next;
        if !effects.contains(&Effect::PublishExit) {
            return Err(CustodyError::InvalidRecord);
        }
        out.finish();
        err.finish();
        use std::sync::atomic::Ordering;
        terminal |= out_events.terminal.load(Ordering::Relaxed)
            || err_events.terminal.load(Ordering::Relaxed);
        let out = out.into_outcome();
        let err = err.into_outcome();
        let mut totals = Vec::new();
        for (name, capture) in [("stdout.log", out), ("stderr.log", err)] {
            let mut total = CaptureTotals {
                bytes: capture.total_bytes,
                retained: capture.bytes.len() as u64,
                oversized_frames: capture.oversized_frames,
                decoder_lost_frames: capture.decoder_lost_frames,
                lost_bytes: capture.sink_lost_bytes,
                read_failed: capture.read_failed,
            };
            if self
                .attempt
                .atomic_write(
                    OsStr::new(name),
                    ".capture-",
                    &capture.bytes,
                    PublishMode::NoReplace,
                )
                .map_err(CustodyError::from)
                .and_then(|publication| publication.require_durable().map_err(Into::into))
                .is_err()
            {
                total.lost_bytes = total.lost_bytes.saturating_add(total.retained);
                total.retained = 0;
                warn(
                    &mut warnings,
                    &mut omitted_warnings,
                    "capture publication failed",
                );
            }
            totals.push(total);
        }
        let unresolved = !cleanup_complete;
        self.reservation.unresolved_cleanup = unresolved;
        if !self.persist_reservation() {
            warn(
                &mut warnings,
                &mut omitted_warnings,
                "cleanup reservation publication failed",
            );
            // The exit must agree with a durable reservation. A visible
            // replacement followed by failed directory sync is not a durable
            // transition; retain the locks and retry the same proven facts.
            // A linked evidence directory may recover from ENOSPC or a sync
            // error, so keep custody and retry indefinitely. An unlinked one
            // cannot accept a reservation at all. In that case only, finish
            // the same full-census and fresh-lease proof before retiring; do
            // not publish an exit or clear a fence without its reservation.
            while !self.persist_reservation() {
                if writer_evidence_identity.is_some_and(|identity| {
                    attempt_removed(&self.writer_dir, writer_evidence_path.as_deref(), identity)
                }) {
                    if !cleanup_complete {
                        cleanup_complete = self
                            .cleanup(
                                &mut child,
                                plan,
                                stop_reason,
                                &mut warnings,
                                &mut omitted_warnings,
                            )
                            .unwrap_or(false);
                    }
                    #[cfg(target_os = "macos")]
                    if !cleanup_complete && self.retire_unlinked_writer(&mut child) {
                        return Err(CustodyError::Io(io::Error::new(
                            io::ErrorKind::NotFound,
                            "writer evidence and original lock removed before exit publication",
                        )));
                    }
                    if cleanup_complete {
                        return Err(CustodyError::Io(io::Error::new(
                            io::ErrorKind::NotFound,
                            "writer evidence directory removed before exit publication",
                        )));
                    }
                }
                let _ = self.reactor.wait(&[], Duration::from_millis(25));
            }
        }
        let mut totals = totals.into_iter();
        let record = ProcessExit {
            schema_version: 1,
            scope: self.reservation.scope,
            termination: exit,
            never_spawned: false,
            launch_refusal: None,
            priority: child.priority,
            stop_reason,
            cancel_delivered_before_exit: Some(cancel_delivered_before_exit),
            delivered_signals: self.delivered_signals.clone(),
            reaped: self.reaped.clone(),
            omitted_reaped: self.omitted_reaped,
            spared: self.spared.spared_identities().take(16).cloned().collect(),
            omitted_spared: self.spared.spared_identities().count().saturating_sub(16) as u64,
            failed_deliveries: self.failed_deliveries.clone(),
            omitted_failed_deliveries: self.omitted_failed_deliveries,
            terminal_observed: terminal,
            unresolved_cleanup: unresolved,
            unresolved_pids: if unresolved {
                self.tracked
                    .iter()
                    .take(16)
                    .map(|identity| identity.pid)
                    .collect()
            } else {
                Vec::new()
            },
            unresolved_identities: if unresolved {
                self.tracked.iter().take(16).cloned().collect()
            } else {
                Vec::new()
            },
            settlement_evidence: self.settlement_evidence,
            stdout: totals.next().unwrap_or_default(),
            stderr: totals.next().unwrap_or_default(),
            warnings: warnings.clone(),
            omitted_warnings,
        };
        let published = self.publish_until_durable(&record, &mut state)?;
        // When the attempt directory is provably removed there is nowhere to
        // publish the exit or the holder's terminal records, so settlement is
        // deliberately skipped: `terminal` would only retry against a missing
        // directory. The writer reservation remains as recovery evidence in
        // the lock slot. A rename is not a removal and still settles.
        if cleanup_complete && published {
            self.release(&record)?;
        }
        if !cleanup_complete {
            // Keep the guardian (and the unreaped
            // leader) alive while group members remain: poll for cancellation
            // and for the group emptying, and republish a settled exit.
            let mut cancelled = false;
            loop {
                cancelled |= self.read_cancel(&mut warnings, &mut omitted_warnings)
                    == Some(machine::StopReason::ExplicitCancellation);
                if let Ok(true) = self.cleanup(
                    &mut child,
                    plan,
                    if cancelled {
                        Some(machine::StopReason::ExplicitCancellation)
                    } else {
                        record.stop_reason
                    },
                    &mut warnings,
                    &mut omitted_warnings,
                ) {
                    if published {
                        self.republish_settled(record, cancelled)?;
                    }
                    break;
                }
                // The old lock inode can be proven idle even after its name is
                // removed. Leave the durable unresolved fence untouched.
                #[cfg(target_os = "macos")]
                if self.retire_unlinked_writer(&mut child) {
                    break;
                }
                match self.reactor.wait(&[], Duration::from_millis(100)) {
                    Ok(event) if event.supervisor_signal => {
                        // Never leave `unresolved` without one more bounded
                        // stop attempt: TERM, the descendant grace, then KILL.
                        if let Ok(true) = self.cleanup(
                            &mut child,
                            plan,
                            if cancelled {
                                Some(machine::StopReason::ExplicitCancellation)
                            } else {
                                Some(machine::StopReason::SupervisorSignal)
                            },
                            &mut warnings,
                            &mut omitted_warnings,
                        ) && published
                        {
                            self.republish_settled(record, cancelled)?;
                        }
                        break;
                    }
                    _ => {}
                }
            }
            loop {
                match child.finish_fenced() {
                    Ok(_) => break,
                    Err(failure) => {
                        let (retained, _) = *failure;
                        child = retained;
                        let _ = self.reactor.wait(&[], Duration::from_millis(25));
                    }
                }
            }
        }
        Ok(())
    }
    /// Replace the unresolved exit with the settled facts once cleanup proves
    /// the group gone. The reservation is made resolved first, so a crash in
    /// between leaves only the conservative unresolved exit visible.
    fn republish_settled(&mut self, mut record: ProcessExit, cancelled: bool) -> Result<()> {
        self.reservation.unresolved_cleanup = false;
        if !self.persist_reservation() {
            self.reservation.unresolved_cleanup = true;
            return Ok(());
        }
        record.unresolved_cleanup = false;
        record.unresolved_pids = Vec::new();
        record.unresolved_identities = Vec::new();
        record.settlement_evidence = self.settlement_evidence;
        record.reaped = self.reaped.clone();
        record.omitted_reaped = self.omitted_reaped;
        record.delivered_signals = self.delivered_signals.clone();
        record.failed_deliveries = self.failed_deliveries.clone();
        record.omitted_failed_deliveries = self.omitted_failed_deliveries;
        if cancelled && record.stop_reason.is_none() {
            record.stop_reason = Some(machine::StopReason::ExplicitCancellation);
        }
        let Ok(bytes) = record.bounded_bytes() else {
            return Ok(());
        };
        let published = self
            .attempt
            .atomic_write(
                OsStr::new("process-exit.json"),
                ".exit-",
                &bytes,
                PublishMode::Replace,
            )
            .map_err(CustodyError::from)
            .and_then(|publication| publication.require_durable().map_err(Into::into));
        if published.is_ok() {
            self.release(&record)?;
        }
        Ok(())
    }
    fn spawn_provider(
        &self,
        argv: &[OsString],
        env: &[(OsString, OsString)],
        cwd: &Path,
        input: &[u8],
        plan: CapturePlan,
        ports: Option<&provenant_os::fs::LockGuard>,
    ) -> io::Result<crate::process_os::provider::ProviderGroup> {
        validate_profile_command(self.profile_file.as_ref(), argv)?;
        match argv.first() {
            Some(executable) => match snapshot_executable(executable.as_os_str()) {
                Ok(snapshot) if snapshot == self.executable_snapshot => {
                    crate::process_os::provider::ProviderGroup::spawn(
                        provenant_os::sys::raw_exec::SpawnSpec {
                            argv,
                            env: Some(env),
                            cwd,
                            input: !input.is_empty(),
                            secret_input: self.secret_input.as_ref(),
                            inherited_lease: None,
                            inherited_profile: self.profile_file.as_ref().map(|profile| {
                                profile as &dyn provenant_os::sys::profile::ProfileSource
                            }),
                            inherited_ports: ports,
                            process_group: false,
                            capture_stdout: plan.stdout.piped(),
                            capture_stderr: plan.stderr.piped(),
                        },
                        self.writer
                            .as_ref()
                            .ok_or_else(|| io::Error::other("writer custody missing"))?,
                        &self.reservation.boot,
                        self.priority,
                    )
                }
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "provider executable snapshot changed",
                )),
                Err(error) => Err(error),
            },
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "provider executable path is missing",
            )),
        }
    }
    /// Publish a launch refusal while retaining the scoped custody proof. If
    /// the attempt directory is provably removed there is no exit to publish
    /// and no terminal settlement to attempt; the refusal stays in the writer
    /// reservation for recovery.
    fn never_spawned(
        &mut self,
        warning: &str,
        launch_refusal: Option<provenant_records::diagnostic::ResourceFact>,
    ) -> Result<()> {
        let mut record =
            ProcessExit::never_spawned(self.reservation.scope, None, vec![warning.into()]);
        record.launch_refusal = launch_refusal;
        self.publish_never_spawned(record)
    }
    fn publish_never_spawned(&mut self, record: ProcessExit) -> Result<()> {
        self.reservation.launch_state = LaunchState::NeverSpawned;
        self.reservation.unresolved_cleanup = false;
        self.reservation.census_incomplete = false;
        self.reservation.descendants.clear();
        self.reservation.omitted_descendants = 0;
        Publication::require_durable(write_record(
            &self.writer_dir,
            "reservation.json",
            &self.reservation,
            PublishMode::Replace,
        )?)?;
        // A provably removed attempt has nowhere to publish the refusal, so
        // skip release too: the durable reservation above is the evidence.
        if self.publish_until_durable(&record, &mut State::never_spawned())? {
            self.release(&record)?;
        }
        Ok(())
    }
    /// Hand the durable exit to the holder while retaining writer.lock through
    /// import, publication and verified terminal settlement.
    /// Failures leave the durable fence available to a resumed supervisor.
    fn release(&mut self, record: &ProcessExit) -> Result<()> {
        let Some(holder) = self.holder else {
            return Ok(());
        };
        if record.unresolved_cleanup || self.writer.is_none() {
            return Ok(());
        }
        self.reservation.launch_state = LaunchState::Settling;
        if write_record(
            &self.writer_dir,
            "reservation.json",
            &self.reservation,
            PublishMode::Replace,
        )
        .and_then(|publication| publication.require_durable().map_err(CustodyError::from))
        .is_err()
        {
            return Ok(());
        }
        let exit = record.durable_observation();
        let Ok(directory) = self.writer_dir.try_clone() else {
            return Ok(());
        };
        let mut recovery = settlement_recovery(
            &mut self.writer,
            directory,
            self.lock_dir.try_clone(),
            &self.reservation,
        )?;
        if recovery.prepare_settlement().is_err() {
            self.writer = recovery.lease.take();
            return Ok(());
        }
        drop(self._custody.take());
        // Worktree exclusion spans settlement retries; attempt custody is free.
        let settled = (|| {
            if holder
                .terminal(&self.holder_context, &self.attempt, &exit, &recovery)
                .is_err()
            {
                return false;
            }
            let verified = self
                .attempt
                .open_verified_directory(OsStr::new("quarantine"))
                .map_err(CustodyError::from)
                .and_then(|quarantine| {
                    verify_release(
                        self.reservation.scope,
                        &self.attempt,
                        &self.attempt,
                        &quarantine,
                    )
                });
            verified.is_ok()
        })();
        if settled {
            let _ = recovery.clear_settled_reservation();
        } else {
            let _ = writeln!(
                io::stderr(),
                "terminal publication incomplete; retaining reservation for reconciliation"
            );
        }
        Ok(())
    }
    /// Retry until the exit is durable or the attempt is provably removed.
    /// Custody survives ENOSPC and signals; process.json exposes
    /// publication_pending. A removed attempt keeps its held descriptor usable,
    /// so removal is judged by directory identity, never by a pathname or an
    /// errno alone.
    fn publish_until_durable(&mut self, record: &ProcessExit, state: &mut State) -> Result<bool> {
        let bytes = record.bounded_bytes()?;
        if bytes.len() as u64 > EXIT_LIMIT {
            return Err(CustodyError::InvalidRecord);
        }
        let mut released_reserve = false;
        let mut visible = false;
        // A held attempt descriptor keeps working after the directory is
        // removed, so cache its identity and current pathname while linked.
        let attempt_identity = DirectoryIdentity::of(&self.attempt)?;
        let attempt_path = self.attempt.current_path().ok();
        let system_exit_publisher = SystemExitPublisher;
        let exit_publisher: &dyn ExitPublisher =
            self.exit_publisher.unwrap_or(&system_exit_publisher);
        loop {
            let result = if visible {
                exit_publisher
                    .sync_visible(&self.attempt)
                    .map(|()| Publication::Durable)
            } else {
                exit_publisher.publish(&self.attempt, &bytes)
            };
            // A removed attempt is terminal for publication even when the
            // operation's errno is not ENOENT. Renames and replacements are not
            // removals, so the held descriptor decides when the pathname is
            // missing or names another inode.
            if attempt_removed(&self.attempt, attempt_path.as_deref(), attempt_identity) {
                return Ok(false);
            }
            let mut tick = Tick::at(0);
            tick.publication = Some(match &result {
                Ok(Publication::Durable) => machine::PublicationOutcome::Durable,
                Ok(Publication::VisibleDurabilityUnconfirmed { .. }) => {
                    machine::PublicationOutcome::VisibleDurabilityUnconfirmed(
                        machine::PublicationFailure::IoError(libc::EIO),
                    )
                }
                Err(error) => machine::PublicationOutcome::Pending(
                    match filesystem_error(error).and_then(provenant_os::fs::FsError::raw_os_error)
                    {
                        Some(libc::ENOSPC) => machine::PublicationFailure::NoSpace,
                        Some(code) => machine::PublicationFailure::IoError(code),
                        None => machine::PublicationFailure::IoError(libc::EIO),
                    },
                ),
            });
            let (next, effects) = machine::step(state.clone(), Event::Silence, tick);
            *state = next;
            if state.phase() == Phase::Published {
                return Ok(true);
            }
            if effects.contains(&Effect::RetryPublishedDirectorySync) {
                visible = true;
            }
            match result {
                Ok(_) => {}
                Err(error)
                    if filesystem_error(&error)
                        .is_some_and(provenant_os::fs::FsError::is_exists) =>
                {
                    // A recovery publisher may have won. Only identical bytes
                    // can share the durable publication; never overwrite facts.
                    if self
                        .attempt
                        .read_bounded(OsStr::new("process-exit.json"), EXIT_LIMIT)
                        .is_ok_and(|other| other == bytes)
                    {
                        visible = true;
                    }
                }
                Err(error)
                    if filesystem_error(&error)
                        .and_then(provenant_os::fs::FsError::raw_os_error)
                        == Some(provenant_os::sys::errno::ENOSPC)
                        && !released_reserve =>
                {
                    match self.attempt.unlink(OsStr::new("reserve")) {
                        Ok(()) => {
                            released_reserve = true;
                            let _ = self.attempt.sync();
                            continue;
                        }
                        Err(error) if error.is_not_found() => {
                            released_reserve = true;
                            continue;
                        }
                        Err(_) => {}
                    }
                }
                Err(_) => {}
            }
            #[derive(Serialize)]
            struct Pending {
                schema_version: u64,
                scope: Scope,
                publication_pending: bool,
                priority: Option<i32>,
            }
            let _ = write_record(
                &self.attempt,
                "process.json",
                &Pending {
                    schema_version: 1,
                    scope: self.reservation.scope,
                    publication_pending: true,
                    priority: record.priority,
                },
                PublishMode::Replace,
            );
            let _ = self.reactor.wait(&[], Duration::from_millis(50));
        }
    }
}

fn filesystem_error(error: &CustodyError) -> Option<&provenant_os::fs::FsError> {
    match error {
        CustodyError::Filesystem(error) => Some(error),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}
impl DirectoryIdentity {
    fn of(directory: &Directory) -> Result<Self> {
        let metadata = directory.metadata()?;
        Ok(Self {
            device: metadata.device,
            inode: metadata.inode,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DescriptorLink {
    Linked,
    Unlinked,
    Unknown,
}

/// The held directory is present only when its cached pathname still names the
/// same inode. A missing or replaced pathname is ambiguous between removal and
/// a rename, so the held descriptor decides; without reliable evidence the
/// directory is not confirmed gone and publication keeps retrying.
fn attempt_removed(attempt: &Directory, path: Option<&Path>, identity: DirectoryIdentity) -> bool {
    if path.is_some_and(|path| path_identifies(path, identity)) {
        return false;
    }
    matches!(descriptor_link(attempt, identity), DescriptorLink::Unlinked)
}

fn path_identifies(path: &Path, identity: DirectoryIdentity) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.dev() == identity.device && metadata.ino() == identity.inode)
}

#[cfg(target_os = "linux")]
fn descriptor_link(attempt: &Directory, _: DirectoryIdentity) -> DescriptorLink {
    match attempt.metadata() {
        Ok(metadata) if metadata.links == 0 => DescriptorLink::Unlinked,
        Ok(_) => DescriptorLink::Linked,
        Err(_) => DescriptorLink::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn descriptor_link(attempt: &Directory, identity: DirectoryIdentity) -> DescriptorLink {
    // F_GETPATH reports the descriptor's current pathname, including after an
    // ancestor rename. A removed directory keeps reporting its old name, so
    // that name must still resolve to the held inode to prove it is linked.
    match attempt.current_path() {
        Ok(path) => match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.dev() == identity.device && metadata.ino() == identity.inode =>
            {
                DescriptorLink::Linked
            }
            Ok(_) => DescriptorLink::Unlinked,
            Err(error) if error.kind() == io::ErrorKind::NotFound => DescriptorLink::Unlinked,
            Err(_) => DescriptorLink::Unknown,
        },
        Err(_) => DescriptorLink::Unknown,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn descriptor_link(_: &Directory, _: DirectoryIdentity) -> DescriptorLink {
    DescriptorLink::Unknown
}

fn push_warning(warnings: &mut Vec<String>, omitted: &mut u64, label: &str) {
    if warnings.len() < 24 {
        warnings.push(label.to_owned());
    } else {
        *omitted = omitted.saturating_add(1);
    }
}

fn snapshot_executable(path: &OsStr) -> io::Result<ExecutableSnapshot> {
    if !Path::new(path).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "provider executable path must be absolute",
        ));
    }
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "provider executable is not a regular file",
        ));
    }
    let modified_nanoseconds = metadata.mtime_nsec();
    if !(0..1_000_000_000).contains(&modified_nanoseconds) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "provider executable timestamp is invalid",
        ));
    }
    Ok(ExecutableSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds,
    })
}

fn valid_resolved_route(route: &str) -> bool {
    !route.is_empty() && route.len() <= 512 && !route.bytes().any(|byte| byte.is_ascii_control())
}

fn stop_delivery_outcome(result: io::Result<bool>) -> DeliveryOutcome {
    match result {
        Ok(true) => DeliveryOutcome::Delivered,
        Ok(false) => DeliveryOutcome::Failed(DeliveryFailure::NoSuchProcess),
        Err(error) => {
            let failure = match error.raw_os_error() {
                Some(libc::EPERM) | Some(libc::EACCES) => DeliveryFailure::PermissionDenied,
                Some(libc::ESRCH) => DeliveryFailure::NoSuchProcess,
                Some(code) => DeliveryFailure::OsError(code),
                None if error.kind() == io::ErrorKind::PermissionDenied => {
                    DeliveryFailure::PermissionDenied
                }
                None => DeliveryFailure::OsError(libc::EIO),
            };
            DeliveryOutcome::Failed(failure)
        }
    }
}

pub(crate) fn session_absence(reservation: &Reservation) -> RecoveryState {
    if reservation.unresolved_cleanup {
        RecoveryState::Unknown
    } else {
        RecoveryState::Absent
    }
}

struct ClosedTee;
impl provenant_os::exec::NonblockingSink for ClosedTee {
    fn try_send(&self, _: &[u8]) -> std::result::Result<(), provenant_os::exec::QueueLoss> {
        Err(provenant_os::exec::QueueLoss::Closed)
    }
}
fn stream_policy(
    mode: StreamMode,
    sink: Option<&dyn provenant_os::exec::NonblockingSink>,
) -> provenant_os::exec::StreamPolicy<'_> {
    match mode {
        StreamMode::Capture { limit } => provenant_os::exec::StreamPolicy::Capture {
            limit: limit as usize,
        },
        StreamMode::Tail { limit } => provenant_os::exec::StreamPolicy::Tail {
            limit: limit as usize,
        },
        StreamMode::Discard => provenant_os::exec::StreamPolicy::Discard,
        StreamMode::Tee { limit } => provenant_os::exec::StreamPolicy::Tee {
            limit: limit as usize,
            sink: sink.unwrap_or(&ClosedTee),
        },
    }
}

#[cfg(test)]
mod tests;

/// A clean stop preserves the lane's own handoff/outcome; a dirty stop either
/// resumes to wrap up or enforces the hard cap during settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BudgetBoundary {
    Clean,
    Dirty,
}

fn budget_boundary(
    budget: u64,
    action: provenant_records::BudgetAction,
    tokens: u64,
    clean: bool,
    completed: bool,
) -> Option<BudgetBoundary> {
    if budget == 0
        || tokens <= budget
        || completed
        || action == provenant_records::BudgetAction::Warn
    {
        None
    } else {
        Some(if clean {
            BudgetBoundary::Clean
        } else {
            BudgetBoundary::Dirty
        })
    }
}
