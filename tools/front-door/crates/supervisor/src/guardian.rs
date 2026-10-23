//! Dedicated same-binary guardian. Run composes provider decoding at this edge.
use crate::census::{ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver};
use crate::custody::*;
use crate::exec::Termination;
use crate::fs::{Directory, Publication, PublishMode};
use crate::machine::{self, Budgets, Effect, Event, Phase, State, Tick};
use provenant_core::CancelToken;
use provenant_records::{Limits, decode_v1};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchMessage {
    schema_version: u64,
    scope: Scope,
    argv: Vec<Vec<u8>>,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    cwd: Vec<u8>,
    stdin: Vec<u8>,
    capture: CapturePlan,
}
pub(crate) fn handoff(
    owned: OwnedLaunch<'_>,
    capture: CapturePlan,
    publication: &PublicationReservation,
) -> Result<()> {
    let message = LaunchMessage {
        schema_version: 1,
        scope: owned.writer.scope(),
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
        &owned.writer.directory,
        &publication.directory,
        &owned.writer.lease,
        &publication.custody,
        &owned.command.environment,
        &payload,
        Duration::from_micros(
            owned
                .handshake
                .remaining(owned.clock.mono_now())
                .as_micros(),
        ),
        owned.cancel,
    )
}

/// Composition seam for Run's bounded provider decoder and confinement filter.
#[derive(Default)]
pub struct GuardianHooks<'a> {
    pub children: Option<&'a dyn ChildRegistry>,
    #[cfg(feature = "fixture")]
    pub observer: Option<&'a dyn ProcessObserver>,
    pub stdout_tee: Option<&'a dyn crate::exec::NonblockingSink>,
    pub stderr_tee: Option<&'a dyn crate::exec::NonblockingSink>,
    pub stdout_decoder: Option<&'a dyn crate::capture::FrameDecoder>,
    pub stderr_decoder: Option<&'a dyn crate::capture::FrameDecoder>,
    pub stdout_filter: Option<&'a mut dyn crate::exec::CaptureFilter>,
    pub stderr_filter: Option<&'a mut dyn crate::exec::CaptureFilter>,
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
    if reservation.scope != message.scope || reservation.guardian.is_some() {
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
        executable_native_bytes: &'a [u8],
        environment_names_native_bytes: Vec<&'a [u8]>,
        capture: CapturePlan,
    }
    // Omit all argv values, stdin, environment values and the replayable plan.
    let summary = Summary {
        schema_version: 1,
        scope: message.scope,
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
        protected: std::collections::BTreeSet::new(),
        registry: hooks.children,
        attempt: inherited.attempt_directory,
        writer_dir: inherited.writer_directory,
        reservation,
        _writer: inherited.writer,
        _custody: inherited.custody,
        guardian,
        reactor,
        observer,
    };
    let result = driver.run(&argv, &env, &cwd, &message.stdin, message.capture, hooks);
    if result.is_ok() {
        #[cfg(target_os = "linux")]
        subreaper.restore()?;
        driver.reactor.restore()?;
    }
    result
}

struct Driver<'a> {
    attempt: Directory,
    writer_dir: Directory,
    reservation: Reservation,
    _writer: crate::fs::LockGuard,
    _custody: crate::fs::LockGuard,
    guardian: ProcessIdentity,
    reactor: crate::sys::provider::GuardianReactor,
    observer: &'a dyn ProcessObserver,
    spared: crate::census::SparingTracker,
    tracked: std::collections::BTreeSet<ProcessIdentity>,
    protected: std::collections::BTreeSet<ProcessIdentity>,
    registry: Option<&'a dyn ChildRegistry>,
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
        let started = Instant::now();
        let budgets =
            Budgets::with_cleanup(plan.wall_ms, plan.root_flush_ms, plan.descendant_grace_ms);
        let mut state = State::prepared(budgets);
        let mut child = match crate::sys::provider::ProviderGroup::spawn(
            argv,
            env,
            cwd,
            !input.is_empty(),
            plan.stdout.piped(),
            plan.stderr.piped(),
        ) {
            Ok(child) => child,
            Err(_) => {
                let record = ProcessExit {
                    schema_version: 1,
                    scope: self.reservation.scope,
                    termination: None,
                    never_spawned: true,
                    stop_reason: None,
                    delivered_signals: vec![],
                    terminal_observed: false,
                    unresolved_cleanup: false,
                    stdout: CaptureTotals::default(),
                    stderr: CaptureTotals::default(),
                    warnings: vec!["provider spawn failed".into()],
                    omitted_warnings: 0,
                };
                return self.publish_until_durable(&record);
            }
        };
        if let ProcessObservation::Present(fact) = self.observer.observe(child.id()) {
            self.reservation.provider = Some(fact.identity);
            self.reservation.launch_state = LaunchState::Spawned;
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
        let mut delivered = Vec::new();
        let mut exit = None;
        let mut stop_at = None;
        let mut killed = false;
        let mut terminal = false;
        let mut last_observation_ms = 0;
        let unresolved;
        let mut drain_deadline = None;
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
            // Decoder facts are held in a bounded local queue, not a worker that
            // can outlive capture. Reading the flags is added below after capture.
            let request =
                match read_record::<CancelRequest>(&self.attempt, "cancel.json", RECORD_LIMIT) {
                    Ok(request) if request.scope == self.reservation.scope => Some(request.reason),
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
                };
            let signal = match self.reactor.wait(&[], Duration::ZERO) {
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
                    machine::StopReason::WallDeadline => Event::RetryStop(reason),
                    machine::StopReason::PostTerminalCleanup => Event::RetryStop(reason),
                }
            } else if newly_terminal {
                Event::Terminal
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
                    Effect::DeliverStop(reason) if exit.is_none() => {
                        stop_reason.get_or_insert(reason);
                        stop_at.get_or_insert(now);
                        match child.stop(reason, false) {
                            Ok(true) => delivered.push(15),
                            Ok(false) => {}
                            Err(_) => {
                                warn(&mut warnings, &mut omitted_warnings, "TERM delivery failed")
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let Some(at) = stop_at
                && !killed
                && now >= at.saturating_add(plan.root_flush_ms.max(plan.descendant_grace_ms))
            {
                match child.stop(
                    stop_reason.unwrap_or(machine::StopReason::PostTerminalCleanup),
                    true,
                ) {
                    Ok(true) => delivered.push(9),
                    Ok(false) => {}
                    Err(_) => warn(&mut warnings, &mut omitted_warnings, "KILL delivery failed"),
                }
                killed = true;
            }
            if let Some(status) = exit {
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
                    unresolved = !self
                        .cleanup(&mut child, plan, &mut warnings, &mut omitted_warnings)
                        .unwrap_or(false);
                    let _ = status;
                    break;
                }
            }
            if now.saturating_sub(last_observation_ms) >= 100 {
                #[derive(Serialize)]
                struct Observed<'a> {
                    schema_version: u64,
                    scope: Scope,
                    guardian: &'a ProcessIdentity,
                    provider: Option<&'a ProcessIdentity>,
                    phase: Phase,
                    stdout_bytes: u64,
                    stderr_bytes: u64,
                    publication_pending: bool,
                }
                self.persist_tracking();
                let observed = Observed {
                    schema_version: 1,
                    scope: self.reservation.scope,
                    guardian: &self.guardian,
                    provider: self.reservation.provider.as_ref(),
                    phase: state.phase(),
                    stdout_bytes: out.outcome().total_bytes,
                    stderr_bytes: err.outcome().total_bytes,
                    publication_pending: false,
                };
                if write_record(
                    &self.attempt,
                    "process.json",
                    &observed,
                    PublishMode::Replace,
                )
                .and_then(require_durable)
                .is_err()
                {
                    warn(
                        &mut warnings,
                        &mut omitted_warnings,
                        "process observation publication failed",
                    );
                }
                last_observation_ms = now;
            }
            let watches = child.watches();
            if self
                .reactor
                .wait(&watches, Duration::from_millis(5))
                .is_err()
            {
                warn(&mut warnings, &mut omitted_warnings, "guardian wait failed");
            }
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
                read_failed: false,
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
        self.reservation.unresolved_cleanup = unresolved;
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
                "cleanup reservation publication failed",
            );
        }
        let mut totals = totals.into_iter();
        let record = ProcessExit {
            schema_version: 1,
            scope: self.reservation.scope,
            termination: exit,
            never_spawned: false,
            stop_reason,
            delivered_signals: delivered,
            terminal_observed: terminal,
            unresolved_cleanup: unresolved,
            stdout: totals.next().unwrap_or_default(),
            stderr: totals.next().unwrap_or_default(),
            warnings,
            omitted_warnings,
        };
        self.publish_until_durable(&record)
    }
    fn persist_tracking(&mut self) {
        self.reservation.descendants = self.tracked.iter().take(128).cloned().collect();
        self.reservation.omitted_descendants = self.tracked.len().saturating_sub(128) as u64;
        let _ = write_record(
            &self.writer_dir,
            "reservation.json",
            &self.reservation,
            PublishMode::Replace,
        )
        .and_then(require_durable);
    }
    fn refresh_census(
        &mut self,
        child: &mut crate::sys::provider::ProviderGroup,
    ) -> Option<crate::census::Census> {
        let census = match self.observer.census(crate::census::MAX_CENSUS_ENTRIES) {
            Ok(census) => census,
            Err(_) => {
                child.allow_group(false);
                self.reservation.census_incomplete = true;
                return None;
            }
        };
        child.record_census(&census);
        let root = self.reservation.provider.as_ref()?;
        self.spared.refresh(&census);
        let mut registry_unknown = false;
        if let Some(registry) = self.registry {
            match registry.registered_children() {
                Ok(children) if children.len() <= 4096 => {
                    for registered in children {
                        let identity = registered.registration.identity();
                        match crate::sys::census::prove_nested_custody(
                            self.observer,
                            &registered.registration,
                            registered.custody.handoff_file(),
                        ) {
                            Ok(proof) => {
                                let _ = self.spared.spare_registered_owner(
                                    &census,
                                    root,
                                    &registered.registration,
                                    &proof,
                                );
                            }
                            Err(_) => {
                                registry_unknown = true;
                                self.protected.insert(identity.clone());
                            }
                        }
                    }
                }
                _ => registry_unknown = true,
            }
        }
        for fact in census.facts() {
            if fact.identity.pid == child.id() {
                continue;
            }
            if census.ancestry(&fact.identity, root) == crate::census::Ancestry::Yes
                || fact.parent_pid == child.id()
                || fact.identity.group_id == child.id()
            {
                self.tracked.insert(fact.identity.clone());
            }
            if matches!(
                fact.command.as_deref(),
                Some("codex-code-mode-host" | "codex-code-mode")
            ) && (self.tracked.contains(&fact.identity) || fact.identity.group_id == child.id())
            {
                self.protected.insert(fact.identity.clone());
            }
        }
        self.tracked.retain(|identity|!matches!(census.process(identity.pid),crate::census::ProcessLookup::Absent) && !matches!(census.process(identity.pid),crate::census::ProcessLookup::Present(fact) if fact.identity!=*identity));
        self.protected.retain(|identity|!matches!(census.process(identity.pid),crate::census::ProcessLookup::Absent) && !matches!(census.process(identity.pid),crate::census::ProcessLookup::Present(fact) if fact.identity!=*identity));
        let group_safe = census.is_complete()
            && !registry_unknown
            && census
                .facts()
                .filter(|fact| fact.identity.group_id == child.id())
                .all(|fact| {
                    fact.identity.pid == child.id()
                        || (self.tracked.contains(&fact.identity)
                            && !self.spared.is_spared(&fact.identity)
                            && !self.protected.contains(&fact.identity))
                });
        child.allow_group(group_safe);
        self.reservation.census_incomplete = !census.is_complete();
        Some(census)
    }
    fn cleanup(
        &mut self,
        child: &mut crate::sys::provider::ProviderGroup,
        plan: CapturePlan,
        _warnings: &mut Vec<String>,
        _omitted: &mut u64,
    ) -> Result<bool> {
        let deadline = Instant::now()
            + Duration::from_millis(
                plan.root_flush_ms
                    .saturating_add(plan.descendant_grace_ms)
                    .saturating_add(100),
            );
        let began = Instant::now();
        loop {
            let Some(census) = self.refresh_census(child) else {
                return Ok(false);
            };
            let mut spared: std::collections::BTreeSet<_> =
                self.spared.spared_identities().cloned().collect();
            spared.extend(self.protected.iter().cloned());
            let reaped = child.reap_adopted(&census, &spared)?;
            for identity in reaped.reaped {
                self.tracked.remove(&identity);
            }
            let outstanding =
                self.tracked
                    .iter()
                    .any(|identity| match census.process(identity.pid) {
                        crate::census::ProcessLookup::Absent => false,
                        crate::census::ProcessLookup::Present(fact) => {
                            fact.identity == *identity && !fact.zombie
                        }
                        crate::census::ProcessLookup::Unknown => true,
                    });
            let complete = census.is_complete()
                && !reaped.unresolved
                && !outstanding
                && session_absence_except_leader(&self.reservation, child.id(), self.observer);
            if complete {
                let _ = child.cleanup(true)?;
                self.persist_tracking();
                return Ok(true);
            }
            // Root exit does not authorise an arbitrary PID. The pinned provider
            // group and kernel-confirmed adopted children are the only targets.
            let escalation = began.elapsed() >= Duration::from_millis(plan.descendant_grace_ms);
            let _ = child.stop(machine::StopReason::PostTerminalCleanup, escalation);
            #[cfg(target_os = "linux")]
            if let Ok(deliveries) = child.stop_adopted(
                machine::StopReason::PostTerminalCleanup,
                escalation,
                &census,
                &spared,
            ) {
                for delivery in deliveries {
                    let _ = (delivery.identity, delivery.outcome);
                }
            }
            self.persist_tracking();
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let _ = self.reactor.wait(
                &[],
                Duration::from_millis(plan.descendant_grace_ms.min(20).max(1)),
            );
        }
    }
    fn publish_until_durable(&mut self, record: &ProcessExit) -> Result<()> {
        let bytes = serde_json::to_vec(record).map_err(|_| CustodyError::InvalidRecord)?;
        if bytes.len() as u64 > EXIT_LIMIT {
            return Err(CustodyError::InvalidRecord);
        }
        let mut released_reserve = false;
        let mut visible = false;
        loop {
            let result = if visible {
                self.attempt.sync().map(|()| Publication::Durable)
            } else {
                self.attempt.atomic_write(
                    OsStr::new("process-exit.json"),
                    ".exit-",
                    &bytes,
                    PublishMode::NoReplace,
                )
            };
            match result {
                Ok(Publication::Durable) => return Ok(()),
                Ok(Publication::VisibleDurabilityUnconfirmed { .. }) => visible = true,
                Err(error) if error.is_exists() => {
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
                    if error.raw_os_error() == Some(crate::sys::errno::ENOSPC)
                        && !released_reserve =>
                {
                    if self.attempt.unlink(OsStr::new("reserve")).is_ok() {
                        released_reserve = true;
                        let _ = self.attempt.sync();
                    }
                    continue;
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

pub(crate) fn session_absence(
    reservation: &Reservation,
    observer: &dyn ProcessObserver,
) -> RecoveryState {
    if reservation.omitted_descendants > 0 || reservation.census_incomplete {
        return RecoveryState::Unknown;
    }
    if session_absence_except_leader(reservation, -1, observer) {
        RecoveryState::Absent
    } else {
        RecoveryState::Unknown
    }
}
fn session_absence_except_leader(
    reservation: &Reservation,
    leader: i64,
    observer: &dyn ProcessObserver,
) -> bool {
    let Some(guardian) = &reservation.guardian else {
        return false;
    };
    let Ok(census) = observer.census(65536) else {
        return false;
    };
    // Implemented against the native census accessors below.
    census.is_complete()
        && reservation.omitted_descendants==0
        && reservation.descendants.iter().all(|identity|matches!(census.process(identity.pid),crate::census::ProcessLookup::Absent)||matches!(census.process(identity.pid),crate::census::ProcessLookup::Present(fact) if fact.identity!=*identity||fact.zombie))
        && !census.facts().any(|fact| {
            fact.identity.session_id == guardian.session_id
                && fact.identity.pid != guardian.pid
                && fact.identity.pid != leader
                && !fact.zombie
        })
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
