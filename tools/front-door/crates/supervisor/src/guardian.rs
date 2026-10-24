//! Dedicated same-binary guardian. Run composes provider decoding at this edge.
mod cleanup;
#[cfg(test)]
use cleanup::{darwin_census_clear, try_writer_lease};

use crate::census::{ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver};
use crate::custody::*;
use crate::exec::Termination;
use crate::fs::{Directory, Publication, PublishMode};
use crate::machine::{
    self, Budgets, DeliveryFailure, DeliveryOutcome, DeliveryReceipt, Effect, Event, Phase, State,
    Tick,
};
use provenant_core::CancelToken;
use provenant_records::{Limits, decode_v1};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    schema_version: u64,
    scope: Scope,
    resolved_route: String,
    permission_plan_digest: provenant_core::Digest,
    task: provenant_core::TaskId,
    requester: provenant_core::ActorId,
    binding: StoredWriterBinding,
    executable_snapshot: ExecutableSnapshot,
    protected_commands: Vec<String>,
    argv: Vec<Vec<u8>>,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    cwd: Vec<u8>,
    stdin: Vec<u8>,
    holder_context: Vec<u8>,
    capture: CapturePlan,
}
pub(crate) fn handoff(
    owned: OwnedLaunch<'_>,
    capture: CapturePlan,
    publication: &PublicationReservation,
) -> Result<()> {
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
    let executable_snapshot = snapshot_executable(
        owned
            .command
            .argv
            .first()
            .ok_or(CustodyError::InvalidRecord)?
            .as_os_str(),
    )?;
    let message = LaunchMessage {
        schema_version: 1,
        scope: owned.lease.scope(),
        resolved_route: owned.command.resolved_route,
        permission_plan_digest: owned.command.permission_plan_digest,
        task: owned.command.task,
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
        cwd: owned.command.cwd.as_os_str().as_bytes().to_vec(),
        stdin: owned.command.stdin,
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
    crate::sys::handoff::spawn(
        &owned.lease.directory,
        &publication.directory,
        owned.lease.lease.as_ref().ok_or(CustodyError::Fenced)?,
        &publication.custody,
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
    pub children: Option<&'a dyn ChildRegistry>,
    /// Run's terminal transaction. Without one, custody stays fenced after
    /// exit until an explicit recovery verifies Run's release attestations.
    pub holder: Option<&'a dyn Holder>,
    #[cfg(feature = "fixture")]
    pub observer: Option<&'a dyn ProcessObserver>,
    #[cfg(feature = "fixture")]
    pub exit_publisher: Option<&'a dyn ExitPublisher>,
    pub stdout_tee: Option<&'a dyn crate::exec::NonblockingSink>,
    pub stderr_tee: Option<&'a dyn crate::exec::NonblockingSink>,
    pub stdout_decoder: Option<&'a dyn crate::capture::FrameDecoder>,
    pub stderr_decoder: Option<&'a dyn crate::capture::FrameDecoder>,
    pub stdout_filter: Option<&'a mut dyn crate::exec::CaptureFilter>,
    pub stderr_filter: Option<&'a mut dyn crate::exec::CaptureFilter>,
}
/// The holder role: the composed terminal publisher runs inside the guardian,
/// after durable exit and completed cleanup, while the guardian still holds
/// the writer lease and attempt custody. It must durably publish Run's
/// `terminal-commit.json`, `quarantine/manifest.json` and `release.json` in the
/// attempt directory before returning. Supervisor then verifies those exact
/// scoped bytes and clears the fence; any failure leaves the fence for
/// recovery. Supervisor never interprets the context or the terminal outcome.
///
/// `admit` runs first, after custody is accepted and before the provider
/// spawns, so a queued attempt waits for host admission inside its own
/// guardian. An error publishes a never-spawned exit and still calls `terminal`.
pub trait Holder {
    fn admit(&self, context: &[u8], attempt: &Directory) -> Result<()>;
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
    custody: crate::fs::FileHandle,
}
impl RegisteredChild {
    pub fn new(identity: ProcessIdentity, custody: crate::fs::FileHandle) -> Result<Self> {
        identity
            .validate()
            .map_err(|_| CustodyError::InvalidRecord)?;
        Ok(Self {
            registration: crate::census::OwnerRegistration::from_validated_registration(identity),
            custody,
        })
    }
}
/// Only `internal supervise` calls this, with descriptors inherited by handoff.
/// It cannot read a launch specification from a public file or stdin.
pub fn supervise(args: &[OsString], hooks: GuardianHooks<'_>) -> Result<()> {
    let mut inherited = crate::sys::handoff::inherit(args)?;
    let payload = crate::sys::handoff::receive(&mut inherited.channel, Duration::from_secs(10))?;
    let message: LaunchMessage = decode_v1(
        payload.as_slice(),
        Limits {
            max_bytes: 2 * 1024 * 1024,
            max_members: 2 * 1024 * 1024,
            ..Limits::CONFIG
        },
    )
    .map_err(|_| CustodyError::InvalidRecord)?;
    message.capture.validate()?;
    let mut reservation: Reservation = read_record(
        &inherited.writer_directory,
        "reservation.json",
        RECORD_LIMIT,
    )?;
    reservation.validate()?;
    if !valid_resolved_route(&message.resolved_route) {
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
    require_durable(write_record(
        &inherited.writer_directory,
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
        task: &'a provenant_core::TaskId,
        resolved_route: &'a str,
        permission_plan_digest: &'a provenant_core::Digest,
        requester: provenant_core::ActorId,
        binding: &'a StoredWriterBinding,
        executable_snapshot: ExecutableSnapshot,
        executable_native_bytes: &'a [u8],
        environment_names_native_bytes: Vec<&'a [u8]>,
        capture: CapturePlan,
    }
    // Omit all argv values, stdin, environment values and the replayable plan.
    let summary = Summary {
        schema_version: 1,
        scope: message.scope,
        task: &message.task,
        resolved_route: &message.resolved_route,
        permission_plan_digest: &message.permission_plan_digest,
        requester: message.requester,
        binding: &message.binding,
        executable_snapshot: message.executable_snapshot,
        executable_native_bytes: argv
            .first()
            .map(|arg| arg.as_bytes())
            .ok_or(CustodyError::InvalidRecord)?,
        environment_names_native_bytes: env.iter().map(|(name, _)| name.as_bytes()).collect(),
        capture: message.capture,
    };
    require_durable(write_record(
        &inherited.attempt_directory,
        "launch.json",
        &summary,
        PublishMode::NoReplace,
    )?)?;
    let reactor = crate::sys::provider::GuardianReactor::install()?;
    #[cfg(target_os = "linux")]
    let subreaper = crate::sys::provider::SubreaperGuard::enable()?;
    #[cfg(feature = "fixture")]
    let exit_publisher = hooks.exit_publisher;
    #[cfg(not(feature = "fixture"))]
    let exit_publisher = None;
    #[cfg(target_os = "macos")]
    let writer_identity =
        crate::fs::Metadata::from_std(inherited.writer.handoff_file().metadata()?);
    // Both locks and the durable pre-spawn reservation are held before ACK.
    crate::sys::handoff::send(
        &mut inherited.channel,
        b"custody-accepted-v1",
        Duration::from_secs(10),
        &CancelToken::new(),
    )?;
    drop(inherited.channel);
    let mut driver = Driver {
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
        attempt: inherited.attempt_directory,
        writer_dir: inherited.writer_directory,
        reservation,
        writer: Some(inherited.writer),
        #[cfg(target_os = "macos")]
        writer_identity,
        _custody: inherited.custody,
        guardian,
        requester: message.requester,
        executable_snapshot: message.executable_snapshot,
        reactor,
        observer,
        exit_publisher,
        holder: hooks.holder,
        holder_context: message.holder_context,
    };
    let result = driver.run(&argv, &env, &cwd, &message.stdin, message.capture, hooks);
    #[cfg(target_os = "linux")]
    if result.is_ok() {
        subreaper.restore()?;
    }
    result
}

struct Driver<'a> {
    attempt: Directory,
    writer_dir: Directory,
    reservation: Reservation,
    writer: Option<crate::fs::LockGuard>,
    #[cfg(target_os = "macos")]
    writer_identity: crate::fs::Metadata,
    _custody: crate::fs::LockGuard,
    guardian: ProcessIdentity,
    requester: provenant_core::ActorId,
    executable_snapshot: ExecutableSnapshot,
    reactor: crate::sys::provider::GuardianReactor,
    observer: &'a dyn ProcessObserver,
    exit_publisher: Option<&'a dyn ExitPublisher>,
    holder: Option<&'a dyn Holder>,
    holder_context: Vec<u8>,
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
}
#[derive(Default)]
struct Events {
    terminal: std::sync::atomic::AtomicBool,
    rejected: std::sync::atomic::AtomicBool,
}
impl crate::capture::DecodedEventSink for Events {
    fn try_send(
        &self,
        event: crate::capture::FrameEvent,
    ) -> std::result::Result<(), crate::exec::QueueLoss> {
        use std::sync::atomic::Ordering;
        match event {
            crate::capture::FrameEvent::Terminal => self.terminal.store(true, Ordering::Relaxed),
            crate::capture::FrameEvent::ProtocolRejected => {
                self.rejected.store(true, Ordering::Relaxed)
            }
            crate::capture::FrameEvent::Progress => {}
        }
        Ok(())
    }
}
impl Driver<'_> {
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
        let warn = |warnings: &mut Vec<String>, omitted: &mut u64, label: &str| {
            if warnings.len() < 24 {
                warnings.push(label.to_owned());
            } else {
                *omitted = omitted.saturating_add(1);
            }
        };
        if let Some(holder) = self.holder
            && holder.admit(&self.holder_context, &self.attempt).is_err()
        {
            return self.never_spawned("admission refused");
        }
        let started = Instant::now();
        let budgets =
            Budgets::with_cleanup(plan.wall_ms, plan.root_flush_ms, plan.descendant_grace_ms);
        let mut state = State::prepared(budgets);
        let spawn_result = self.spawn_provider(argv, env, cwd, input, plan);
        let mut child = match spawn_result {
            Ok(child) => child,
            Err(error) => {
                return self.never_spawned(if error.kind() == io::ErrorKind::InvalidData {
                    "provider executable snapshot changed"
                } else {
                    "provider spawn failed"
                });
            }
        };
        self.reservation.launch_state = LaunchState::Spawned;
        self.reservation.provider = child.identity().cloned();
        if write_record(
            &self.writer_dir,
            "reservation.json",
            &self.reservation,
            PublishMode::Replace,
        )
        .and_then(require_durable)
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
                .map(|decoder| crate::capture::FrameDelivery::Provider {
                    decoder,
                    events: &out_events,
                });
        let err_decoder =
            hooks
                .stderr_decoder
                .map(|decoder| crate::capture::FrameDelivery::Provider {
                    decoder,
                    events: &err_events,
                });
        let mut out = crate::capture::StreamCapture::new(
            stream_policy(plan.stdout, hooks.stdout_tee),
            out_decoder,
            hooks.stdout_filter,
        )
        .map_err(|_| CustodyError::InvalidRecord)?;
        let mut err = crate::capture::StreamCapture::new(
            stream_policy(plan.stderr, hooks.stderr_tee),
            err_decoder,
            hooks.stderr_filter,
        )
        .map_err(|_| CustodyError::InvalidRecord)?;
        let mut input_offset = 0;
        let mut stop_reason = None;
        let mut exit = None;
        let mut stop_at = None;
        let mut killed = false;
        let mut terminal = false;
        let mut last_checkpoint = None;
        let mut last_observation_ms: Option<u64> = None;
        let mut last_cpu_observation_ms = 0;
        let mut last_cancel_ms = None;
        let mut signal_pending = false;
        let mut last_stdout_bytes = 0_u64;
        let mut last_stderr_bytes = 0_u64;
        let mut last_output_ms = None;
        let mut provider_cpu_micros = None;
        let mut previous_cpu_micros = None;
        let mut last_cpu_progress_ms = None;
        let mut census_warning_active = false;
        let cleanup_complete;
        let mut drain_deadline = None;
        let mut cleanup_pending = false;
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
            out.read(&mut child.stdout);
            err.read(&mut child.stderr);
            let stdout_bytes = out.outcome().total_bytes;
            let stderr_bytes = err.outcome().total_bytes;
            let output_progress =
                stdout_bytes > last_stdout_bytes || stderr_bytes > last_stderr_bytes;
            if output_progress {
                last_output_ms = Some(now);
            }
            last_stdout_bytes = stdout_bytes;
            last_stderr_bytes = stderr_bytes;
            let cpu_sample_due = now.saturating_sub(last_cpu_observation_ms) >= 100;
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
            if let Some(stdin) = child.stdin.as_mut() {
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
            let cancel_due = last_cancel_ms.is_none_or(|last| now.saturating_sub(last) >= 100);
            let request = if cancel_due {
                last_cancel_ms = Some(now);
                match read_record::<CancelRequest>(&self.attempt, "cancel.json", RECORD_LIMIT) {
                    Ok(request)
                        if request.scope == self.reservation.scope
                            && request.requester == self.requester
                            && provenant_core::OperationId::parse(request.request_id.as_str())
                                .is_ok() =>
                    {
                        Some(request.reason)
                    }
                    Ok(_) => {
                        warn(
                            &mut warnings,
                            &mut omitted_warnings,
                            "stale cancellation ignored",
                        );
                        None
                    }
                    Err(CustodyError::Filesystem(error)) if error.is_not_found() => None,
                    Err(_) => {
                        warn(
                            &mut warnings,
                            &mut omitted_warnings,
                            "cancel request unreadable",
                        );
                        None
                    }
                }
            } else {
                None
            };
            let polled_signal = match self.reactor.wait(&[], Duration::ZERO) {
                Ok(event) => event.supervisor_signal,
                Err(_) => {
                    warn(
                        &mut warnings,
                        &mut omitted_warnings,
                        "signal observation failed",
                    );
                    false
                }
            };
            let signal = std::mem::replace(&mut signal_pending, false) || polled_signal;
            use std::sync::atomic::Ordering;
            let newly_terminal = !terminal
                && (out_events.terminal.load(Ordering::Relaxed)
                    || err_events.terminal.load(Ordering::Relaxed));
            terminal |= newly_terminal;
            let rejected = out_events.rejected.load(Ordering::Relaxed)
                || err_events.rejected.load(Ordering::Relaxed);
            let event = if signal {
                Event::SupervisorSignal
            } else if rejected {
                Event::ProtocolRejected
            } else if let Some(reason) = request {
                match reason {
                    machine::StopReason::ExplicitCancellation => Event::Cancel,
                    machine::StopReason::ProtocolRejected => Event::ProtocolRejected,
                    machine::StopReason::SupervisorSignal => Event::SupervisorSignal,
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
            } else if newly_terminal {
                Event::Terminal
            } else if output_progress || cpu_progress {
                Event::Progress
            } else {
                Event::Silence
            };
            let mut tick = Tick::at(now);
            tick.observed_exit = exit.map(|value| match value {
                Termination::Exit(code) => machine::Termination::Exit(code),
                Termination::Signal(sig) => machine::Termination::Signal(sig),
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
            // Keep safety history at 100 ms; slowing census can miss a short
            // lived parent before an escaped descendant changes session/group.
            if cpu_sample_due {
                if self.refresh_census(&mut child).is_none() {
                    if !census_warning_active {
                        warn(
                            &mut warnings,
                            &mut omitted_warnings,
                            "live process census incomplete",
                        );
                        census_warning_active = true;
                    }
                } else {
                    census_warning_active = false;
                }
                self.persist_tracking(&mut warnings, &mut omitted_warnings);
            }
            // Process checkpoint durability is independent of census cadence.
            if last_observation_ms.is_none_or(|last| now.saturating_sub(last) >= 1_000) {
                #[derive(Serialize)]
                struct Observed<'a> {
                    schema_version: u64,
                    scope: Scope,
                    guardian: &'a ProcessIdentity,
                    provider: Option<&'a ProcessIdentity>,
                    phase: Phase,
                    stdout_bytes: u64,
                    stderr_bytes: u64,
                    last_output_ms: Option<u64>,
                    last_progress_ms: Option<u64>,
                    silent: bool,
                    provider_cpu_micros: Option<u64>,
                    last_cpu_progress_ms: Option<u64>,
                    publication_pending: bool,
                }
                let observed = Observed {
                    schema_version: 1,
                    scope: self.reservation.scope,
                    guardian: &self.guardian,
                    provider: self.reservation.provider.as_ref(),
                    phase: state.phase(),
                    stdout_bytes: out.outcome().total_bytes,
                    stderr_bytes: err.outcome().total_bytes,
                    last_output_ms,
                    last_progress_ms: state.last_progress_ms(),
                    silent: plan.silent(now, state.last_progress_ms()),
                    provider_cpu_micros,
                    last_cpu_progress_ms,
                    publication_pending: false,
                };
                let system_publisher = SystemExitPublisher;
                let checkpoint =
                    serde_json::to_vec(&observed).map_err(|_| CustodyError::InvalidRecord)?;
                if last_checkpoint.as_ref() != Some(&checkpoint) {
                    if self
                        .exit_publisher
                        .unwrap_or(&system_publisher)
                        .checkpoint(&self.attempt, &checkpoint)
                        .and_then(require_durable)
                        .is_err()
                    {
                        warn(
                            &mut warnings,
                            &mut omitted_warnings,
                            "process observation publication failed",
                        );
                    } else {
                        last_checkpoint = Some(checkpoint);
                    }
                }
                last_observation_ms = Some(now);
            }
            let watches = child.watches();
            match self.reactor.wait(&watches, Duration::from_millis(5)) {
                Ok(event) => signal_pending |= event.supervisor_signal,
                Err(_) => warn(&mut warnings, &mut omitted_warnings, "guardian wait failed"),
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
                .and_then(require_durable)
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
            // Intentionally unbounded: dropping custody before durable proof
            // would admit a second writer. Signals cannot waive this invariant.
            while !self.persist_reservation() {
                let _ = self.reactor.wait(&[], Duration::from_millis(25));
            }
        }
        let mut totals = totals.into_iter();
        let record = ProcessExit {
            schema_version: 1,
            scope: self.reservation.scope,
            termination: exit,
            never_spawned: false,
            stop_reason,
            delivered_signals: self.delivered_signals.clone(),
            reaped: self.reaped.clone(),
            omitted_reaped: self.omitted_reaped,
            spared: self.spared.spared_identities().take(16).cloned().collect(),
            omitted_spared: self.spared.spared_identities().count().saturating_sub(16) as u64,
            failed_deliveries: self.failed_deliveries.clone(),
            omitted_failed_deliveries: self.omitted_failed_deliveries,
            terminal_observed: terminal,
            unresolved_cleanup: unresolved,
            settlement_evidence: self.settlement_evidence,
            stdout: totals.next().unwrap_or_default(),
            stderr: totals.next().unwrap_or_default(),
            warnings: warnings.clone(),
            omitted_warnings,
        };
        self.publish_until_durable(&record, &mut state)?;
        if cleanup_complete {
            self.release(&record);
        }
        if !cleanup_complete {
            // A durable unresolved fence permits recovery to reconcile after
            // the guardian exits. It does not certify descendant absence.
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
    fn spawn_provider(
        &self,
        argv: &[OsString],
        env: &[(OsString, OsString)],
        cwd: &Path,
        input: &[u8],
        plan: CapturePlan,
    ) -> io::Result<crate::sys::provider::ProviderGroup> {
        match argv.first() {
            Some(executable) => match snapshot_executable(executable.as_os_str()) {
                Ok(snapshot) if snapshot == self.executable_snapshot => {
                    crate::sys::provider::ProviderGroup::spawn(
                        argv,
                        env,
                        cwd,
                        !input.is_empty(),
                        plan,
                        self.writer
                            .as_ref()
                            .ok_or_else(|| io::Error::other("writer custody missing"))?,
                        &self.reservation.boot,
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
    /// Publish a launch refusal while retaining the scoped custody proof.
    fn never_spawned(&mut self, warning: &str) -> Result<()> {
        self.reservation.launch_state = LaunchState::NeverSpawned;
        self.reservation.unresolved_cleanup = false;
        self.reservation.census_incomplete = false;
        self.reservation.descendants.clear();
        self.reservation.omitted_descendants = 0;
        require_durable(write_record(
            &self.writer_dir,
            "reservation.json",
            &self.reservation,
            PublishMode::Replace,
        )?)?;
        let record = ProcessExit::never_spawned(self.reservation.scope, None, vec![warning.into()]);
        self.publish_until_durable(&record, &mut State::never_spawned())?;
        self.release(&record);
        Ok(())
    }
    /// Hand the durable exit to the holder and clear the fence only after its
    /// release attestations verify. Failures keep the fence for recovery;
    /// they never alter the published exit or become a stop reason.
    fn release(&mut self, record: &ProcessExit) {
        let Some(holder) = self.holder else {
            return;
        };
        if record.unresolved_cleanup || self.writer.is_none() {
            return;
        }
        let exit = record.durable_observation();
        let Some(lease) = self.writer.take() else {
            return;
        };
        let Ok(directory) = self.writer_dir.try_clone() else {
            self.writer = Some(lease);
            return;
        };
        let recovery = crate::custody::Recovery {
            directory,
            lease,
            reservation: self.reservation.clone(),
            guardian_quiescent: true,
        };
        // About 30 s in all, so a contended store does not strand custody.
        let mut delay = Duration::from_millis(50);
        for attempt in 0..12 {
            if attempt > 0 {
                let _ = self.reactor.wait(&[], delay);
                delay = delay.saturating_mul(2).min(Duration::from_secs(5));
            }
            if holder
                .terminal(&self.holder_context, &self.attempt, &exit, &recovery)
                .is_err()
            {
                continue;
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
            if verified.is_ok() {
                let _ = clear_reservation(&self.writer_dir, self.reservation.scope);
                self.writer = Some(recovery.lease);
                return;
            }
        }
        self.writer = Some(recovery.lease);
    }
    /// Retry without a time limit: custody survives ENOSPC and signals until
    /// the immutable exit is durable. process.json exposes publication_pending.
    fn publish_until_durable(&mut self, record: &ProcessExit, state: &mut State) -> Result<()> {
        let bytes = record.bounded_bytes()?;
        if bytes.len() as u64 > EXIT_LIMIT {
            return Err(CustodyError::InvalidRecord);
        }
        let mut released_reserve = false;
        let mut visible = false;
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
            let mut tick = Tick::at(0);
            tick.publication = Some(match &result {
                Ok(Publication::Durable) => machine::PublicationOutcome::Durable,
                Ok(Publication::VisibleDurabilityUnconfirmed { .. }) => {
                    machine::PublicationOutcome::VisibleDurabilityUnconfirmed(
                        machine::PublicationFailure::IoError(libc::EIO),
                    )
                }
                Err(error) => machine::PublicationOutcome::Pending(
                    match filesystem_error(error).and_then(crate::fs::FsError::raw_os_error) {
                        Some(libc::ENOSPC) => machine::PublicationFailure::NoSpace,
                        Some(code) => machine::PublicationFailure::IoError(code),
                        None => machine::PublicationFailure::IoError(libc::EIO),
                    },
                ),
            });
            let (next, effects) = machine::step(state.clone(), Event::Silence, tick);
            *state = next;
            if state.phase() == Phase::Published {
                return Ok(());
            }
            if effects.contains(&Effect::RetryPublishedDirectorySync) {
                visible = true;
            }
            match result {
                Ok(_) => {}
                Err(error)
                    if filesystem_error(&error).is_some_and(crate::fs::FsError::is_exists) =>
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
                    if filesystem_error(&error).and_then(crate::fs::FsError::raw_os_error)
                        == Some(crate::sys::errno::ENOSPC)
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
            }
            let _ = write_record(
                &self.attempt,
                "process.json",
                &Pending {
                    schema_version: 1,
                    scope: self.reservation.scope,
                    publication_pending: true,
                },
                PublishMode::Replace,
            );
            let _ = self.reactor.wait(&[], Duration::from_millis(50));
        }
    }
}

fn filesystem_error(error: &CustodyError) -> Option<&crate::fs::FsError> {
    match error {
        CustodyError::Filesystem(error) => Some(error),
        _ => None,
    }
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
impl crate::exec::NonblockingSink for ClosedTee {
    fn try_send(&self, _: &[u8]) -> std::result::Result<(), crate::exec::QueueLoss> {
        Err(crate::exec::QueueLoss::Closed)
    }
}
fn stream_policy(
    mode: StreamMode,
    sink: Option<&dyn crate::exec::NonblockingSink>,
) -> crate::exec::StreamPolicy<'_> {
    match mode {
        StreamMode::Capture { limit } => crate::exec::StreamPolicy::Capture {
            limit: limit as usize,
        },
        StreamMode::Discard => crate::exec::StreamPolicy::Discard,
        StreamMode::Tee { limit } => crate::exec::StreamPolicy::Tee {
            limit: limit as usize,
            sink: sink.unwrap_or(&ClosedTee),
        },
    }
}

#[cfg(test)]
mod tests;
