// Integration tests use real time to bound process and filesystem coordination.
#![allow(clippy::disallowed_methods)]

use provenant_core::{AttemptId, CancelToken, RunId, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, FileHandle, LockMode},
};
use provenant_supervisor::{
    ChildRegistry, ExitPublisher, GuardianHooks, NestedChildStore, RegisteredChild,
    census::{Census, CensusError, ProcessIdentity, ProcessObservation, ProcessObserver},
    custody::*,
    fixture,
    machine::StopReason,
};
use std::error::Error;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const S2_CENSUS_GATE_ONCE: &str = "s2-census-gate-once";
const S2_CENSUS_PAUSED: &str = "s2-census-paused";
const S2_CENSUS_RELEASE: &str = "s2-census-release";
const S2_PRODUCTION_SHAPE: &str = "s2-production-shape";
const S2_REFUSE_ADMISSION: &str = "s2-refuse-admission";
const S2_IGNORE_CHILD_REGISTRY: &str = "s2-ignore-child-registry";
const S2_SUPPRESSED_REGISTRY_READ: &str = "s2-registry-suppressed-read";

const S2_DISK_PRESSURE: &str = "s2-disk-pressure";
/// Seconds the guardian's wall clock jumps forward after launch, as a sleeping
/// host's does while its monotonic clock stands still.
const S2_WALL_JUMP: &str = "s2-wall-jump-seconds";
/// Present once the jump has happened.
const S2_WALL_JUMP_NOW: &str = "s2-wall-jump-now";
const S2_DISK_SAMPLED: &str = "s2-disk-sampled";

struct MarkerDiskPressure {
    state: PathBuf,
}
impl provenant_supervisor::DiskPressure for MarkerDiskPressure {
    fn below_floor(&self, _: &[u8]) -> io::Result<bool> {
        let decision = std::fs::read(self.state.join(S2_DISK_PRESSURE))?;
        std::fs::write(self.state.join(S2_DISK_SAMPLED), &decision)?;
        match decision.as_slice() {
            b"low" => Ok(true),
            b"high" => Ok(false),
            _ => Err(io::Error::other("injected disk sample failure")),
        }
    }
}

struct CensusGateObserver;
impl ProcessObserver for CensusGateObserver {
    fn boot_identity(&self) -> std::result::Result<String, CensusError> {
        fixture::RecordedBootObserver.boot_identity()
    }

    fn observe(&self, pid: i64) -> ProcessObservation {
        if pid != i64::from(std::process::id())
            && std::env::var_os("PROVENANT_STATE_DIR")
                .is_some_and(|root| PathBuf::from(root).join("s2-provider-exit-first").exists())
        {
            // Hold the guardian at its first post-spawn lookup until the
            // immediate-exit child is a zombie. Darwin reports that as Absent.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match fixture::RecordedBootObserver.observe(pid) {
                    ProcessObservation::Present(fact) if !fact.zombie => {}
                    ProcessObservation::Present(_) | ProcessObservation::Absent => {
                        let Some(state) =
                            std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from)
                        else {
                            return ProcessObservation::Unknown(
                                provenant_supervisor::census::ObservationIssue::Io,
                            );
                        };
                        if std::fs::write(state.join("s2-provider-observed-absent"), b"exited")
                            .is_err()
                        {
                            return ProcessObservation::Unknown(
                                provenant_supervisor::census::ObservationIssue::Io,
                            );
                        }
                        return ProcessObservation::Absent;
                    }
                    other => return other,
                }
                assert!(Instant::now() < deadline, "provider did not exit");
                std::thread::yield_now();
            }
        }
        fixture::RecordedBootObserver.observe(pid)
    }

    fn census(&self, max_entries: usize) -> std::result::Result<Census, CensusError> {
        if let Some(root) = std::env::var_os("PROVENANT_STATE_DIR") {
            let counter = PathBuf::from(root).join("s2-census-errors-left");
            if let Ok(bytes) = std::fs::read(&counter) {
                let left: u32 = serde_json::from_slice(&bytes)
                    .map_err(|error| CensusError::ProcessList(io::Error::other(error)))?;
                if left > 0 {
                    std::fs::write(&counter, (left - 1).to_string())
                        .map_err(CensusError::ProcessList)?;
                    return Err(CensusError::ProcessList(io::Error::other(
                        "transient census failure",
                    )));
                }
            }
        }
        let census = fixture::RecordedBootObserver.census(max_entries)?;
        let census = if std::env::var_os("PROVENANT_STATE_DIR")
            .is_some_and(|root| PathBuf::from(root).join("s2-provider-exit-first").exists())
        {
            // Omit the provider's row even when Linux lists its zombie, so
            // refresh_census must obtain the explicit Absent observation.
            Census::from_observations(
                census
                    .facts()
                    .filter(|fact| fact.parent_pid != i64::from(std::process::id()))
                    .cloned(),
                census
                    .unknown_pids()
                    .iter()
                    .map(|(pid, issue)| (*pid, *issue)),
                [],
                census.is_complete(),
                false,
                max_entries,
            )?
        } else {
            census
        };
        if let Some(state) = std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from)
            && std::fs::remove_file(state.join("s2-tracking-failure")).is_ok()
        {
            let writer = state
                .parent()
                .ok_or_else(|| CensusError::ProcessList(io::Error::other("missing root")))?
                .join("tracking-writer");
            std::fs::rename(
                writer
                    .join(scope().attempt.to_string())
                    .join("reservation.json"),
                writer.join("reservation.saved"),
            )
            .map_err(CensusError::ProcessList)?;
            std::fs::create_dir(
                writer
                    .join(scope().attempt.to_string())
                    .join("reservation.json"),
            )
            .map_err(CensusError::ProcessList)?;
        }

        let Some(state_dir) = std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from) else {
            return Ok(census);
        };
        if let Some(root) = state_dir.parent()
            && root
                .join("tracking-writer")
                .join(scope().attempt.to_string())
                .join("reservation.json")
                .is_dir()
        {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(state_dir.join("s2-tracking-failures"))
                .and_then(|mut file| file.write_all(b"x"))
                .map_err(CensusError::ProcessList)?;
        }
        if std::fs::remove_file(state_dir.join("s2-census-skip-gate-once")).is_ok() {
            return Ok(census);
        }
        let gate = state_dir.join(S2_CENSUS_GATE_ONCE);
        match std::fs::remove_file(&gate) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(census),
            Err(error) => return Err(CensusError::ProcessList(error)),
        }
        std::fs::write(state_dir.join(S2_CENSUS_PAUSED), b"snapshot captured")
            .map_err(CensusError::ProcessList)?;
        let release = state_dir.join(S2_CENSUS_RELEASE);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if release.is_file() {
                return Ok(census);
            }
            if Instant::now() >= deadline {
                return Err(CensusError::ProcessList(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "fixture census release was not published",
                )));
            }
            std::thread::yield_now();
        }
    }
}

fn scope() -> Scope {
    Scope {
        run: RunId::from_bytes([1; 16]),
        attempt: AttemptId::from_bytes([2; 16]),
        generation: 1,
    }
}
fn writer_is_fenced_after_every_descriptor_closes(root: &Path) -> Result<()> {
    let path = root.join("writer");
    std::fs::create_dir(&path)?;
    let clock = SystemClock::new();
    let token = CancelToken::new();
    let lease = fixture::acquire_writer(
        Directory::open_anchor(&path)?,
        scope(),
        WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
        &token,
        &clock,
    )?;
    assert!(
        path.join(scope().attempt.to_string())
            .join("reservation.json")
            .is_file()
    );
    drop(lease);
    let result = fixture::acquire_writer(
        Directory::open_anchor(&path)?,
        scope(),
        WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
        &token,
        &clock,
    );
    assert!(matches!(result, Err(CustodyError::Fenced)));
    assert!(
        Directory::open_anchor(&path)?
            .metadata_at(OsStr::new("writer.lock"), false)?
            .links
            == 1
    );
    // A lost submitter never handed this Prepared reservation to a guardian.
    // Explicit recovery can clear it without fabricating an exit receipt.
    acquire_recovery(
        Directory::open_anchor(&path)?,
        scope(),
        WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
        &token,
        &clock,
    )?
    .withdraw_prepared()?;
    assert!(
        !path
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .exists()
    );
    let lease = fixture::acquire_writer(
        Directory::open_anchor(&path)?,
        scope(),
        WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
        &token,
        &clock,
    )?;
    lease.withdraw_unlaunched()?;
    let prior_owner = std::fs::read(
        path.join(scope().attempt.to_string())
            .join("ownership.json"),
    )?;
    let next = Scope {
        run: RunId::from_bytes([3; 16]),
        attempt: AttemptId::from_bytes([4; 16]),
        generation: 1,
    };
    drop(fixture::acquire_writer(
        Directory::open_anchor(&path)?,
        next,
        budget(&clock)?,
        &token,
        &clock,
    )?);
    // Crash after reservation publication, before replacing the reused owner.
    std::fs::write(
        path.join(next.attempt.to_string()).join("ownership.json"),
        prior_owner,
    )?;
    acquire_recovery(
        Directory::open_anchor(&path)?,
        next,
        budget(&clock)?,
        &token,
        &clock,
    )?
    .withdraw_prepared()?;
    fixture::acquire_writer(
        Directory::open_anchor(&path)?,
        scope(),
        budget(&clock)?,
        &token,
        &clock,
    )?
    .withdraw_unlaunched()?;
    Ok(())
}
struct Decoder;
impl provenant_os::capture::FrameDecoder for Decoder {
    fn decode(
        &self,
        frame: &[u8],
    ) -> std::result::Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure>
    {
        if frame == b"crash-guardian" {
            std::process::exit(70);
        }
        Ok(if frame == b"terminal" {
            provenant_os::capture::FrameEvent::Terminal
        } else {
            provenant_os::capture::FrameEvent::Progress
        })
    }
}
fn budget(clock: &SystemClock) -> Result<WaitBudget> {
    use provenant_core::Clock;
    Ok(WaitBudget::cli(clock.mono_now(), 5)?)
}
fn start(
    root: &Path,
    name: &str,
    scenario: &str,
    extra: &[OsString],
    plan: CapturePlan,
) -> Result<(CustodyLease, CustodyHandle)> {
    let (writer, handle, _) = start_with_custody_identity(root, name, scenario, extra, plan)?;
    Ok((writer, handle))
}
fn start_with_custody_identity(
    root: &Path,
    name: &str,
    scenario: &str,
    extra: &[OsString],
    plan: CapturePlan,
) -> Result<(CustodyLease, CustodyHandle, provenant_os::fs::Metadata)> {
    let writer = root.join(format!("{name}-writer"));
    let attempt = root.join(format!("{name}-attempt"));
    std::fs::create_dir(&writer)?;
    std::fs::create_dir(&attempt)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut writer = fixture::acquire_writer(
        Directory::open_anchor(&writer)?,
        scope(),
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(&attempt)?,
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let identity = publication.custody_metadata()?;
    let command = ProviderCommand {
        priority: 0,
        provider_lease: None,
        requester: provenant_core::ActorId::from_bytes([4; 16]),
        resolved_route: "fixture/provider".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([0; 32]),
        task: provenant_core::TaskId::from_bytes([3; 16]),
        protected_commands: fixture::protected_commands(),
        config_environment: fixture::config_environment(),
        argv: fixture::argv_with_args(scenario, extra)?,
        environment: fixture::environment(root),
        cwd: root.into(),
        stdin: vec![],
        secret_input: None,
        holder_context: Vec::new(),
    };
    let handle = launch(
        OwnedLaunch {
            command,
            lease: &mut writer,
            handshake: budget(&clock)?,
            cancel: &cancel,
            clock: &clock,
        },
        plan,
        publication,
    )?;
    Ok((writer, handle, identity))
}
fn exited(handle: &CustodyHandle) -> Result<ExitObservation> {
    let clock = SystemClock::new();
    match observe(handle, budget(&clock)?, &CancelToken::new(), &clock)? {
        Observation::Exited(exit) => Ok(*exit),
        other => Err(io::Error::other(format!("expected exit, got {other:?}")).into()),
    }
}
fn byte(stream: &mut UnixStream, expected: u8) -> Result<()> {
    assert_eq!(
        fixture::fixture_read_byte(stream, Duration::from_secs(5))?,
        expected
    );
    Ok(())
}
fn process_identity(pid: i64) -> Result<ProcessIdentity> {
    match fixture::RecordedBootObserver.observe(pid) {
        ProcessObservation::Present(fact) => Ok(fact.identity),
        other => Err(io::Error::other(format!("process identity unavailable: {other:?}")).into()),
    }
}
fn read_pid_line(stream: &mut UnixStream) -> Result<i64> {
    let mut bytes = Vec::new();
    loop {
        let byte = fixture::fixture_read_byte(stream, Duration::from_secs(5))?;
        if byte == b'\n' {
            break;
        }
        if !byte.is_ascii_digit() || bytes.len() >= 20 {
            return Err(io::Error::other("invalid fixture PID line").into());
        }
        bytes.push(byte);
    }
    let text = std::str::from_utf8(&bytes)?;
    Ok(text.parse()?)
}
fn reservation_has_descendant(path: &Path, expected: &ProcessIdentity) -> Result<bool> {
    let bytes = std::fs::read(path)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let Some(descendants) = value
        .get("descendants")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(false);
    };
    for value in descendants {
        let identity: ProcessIdentity = serde_json::from_value(value.clone())?;
        if identity == *expected {
            return Ok(true);
        }
    }
    Ok(false)
}
fn wait_for_descendant(path: &Path, identity: &ProcessIdentity) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if reservation_has_descendant(path, identity)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("provider descendant was not durably tracked").into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}
fn wait_for_registry_reads(home: &Path, minimum: u64) -> Result<()> {
    let path = home.join("s2-registry-reads");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let reads = std::fs::read_to_string(&path)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        if reads >= minimum {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("guardian did not revisit nested registration").into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}
fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if path.is_file() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("fixture marker was not published").into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}
fn wait_for_identity_absence(identity: &ProcessIdentity) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fixture::RecordedBootObserver.observe(identity.pid) {
            ProcessObservation::Absent => return Ok(()),
            ProcessObservation::Present(fact) if fact.identity != *identity => return Ok(()),
            ProcessObservation::Present(_) | ProcessObservation::Unknown(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("fixture descendant did not exit").into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}
fn immediate_exit_identity_survives_absent_observation(root: &Path) -> Result<()> {
    std::fs::create_dir(root.join("PROVENANT_STATE_DIR"))?;
    std::fs::write(
        root.join("PROVENANT_STATE_DIR/s2-provider-exit-first"),
        b"gate",
    )?;
    let (_writer, handle) = start(root, "immediate", "bytes", &[], CapturePlan::default())?;
    let exit = exited(&handle)?;
    let reservation = std::fs::read_to_string(
        root.join("immediate-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json"),
    )?;
    assert!(
        !exit.unresolved_cleanup,
        "exit={exit:?}, reservation={reservation}"
    );
    let record: serde_json::Value = serde_json::from_str(&reservation)?;
    assert!(record["provider"].is_object(), "{reservation}");
    assert!(
        root.join("PROVENANT_STATE_DIR/s2-provider-observed-absent")
            .exists()
    );
    Ok(())
}
fn cleanup_retries_a_transient_census_failure(root: &Path) -> Result<()> {
    std::fs::create_dir(root.join("PROVENANT_STATE_DIR"))?;
    std::fs::write(
        root.join("PROVENANT_STATE_DIR/s2-census-errors-left"),
        b"30",
    )?;
    let (_writer, handle) = start(root, "transient", "bytes", &[], CapturePlan::default())?;
    let exit = exited(&handle)?;
    assert!(!exit.unresolved_cleanup, "{exit:?}");
    assert_eq!(
        exit.warnings
            .iter()
            .filter(|warning| *warning == "cleanup census unavailable; retrying")
            .count(),
        1,
        "{exit:?}"
    );
    assert_eq!(exit.omitted_warnings, 0);
    Ok(())
}
fn descendant_journal_failure_is_reported(root: &Path) -> Result<()> {
    std::fs::create_dir(root.join("PROVENANT_STATE_DIR"))?;
    std::fs::write(
        root.join("PROVENANT_STATE_DIR/s2-tracking-failure"),
        b"fail",
    )?;
    let socket = root.join("tracking.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "tracking",
        "wait-on-socket",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read(root.join("PROVENANT_STATE_DIR/s2-tracking-failures"))
        .map_or(true, |bytes| bytes.len() < 4)
    {
        assert!(
            Instant::now() < deadline,
            "journal failures were not retried"
        );
        std::thread::park_timeout(Duration::from_millis(5));
    }
    std::fs::remove_dir(
        root.join("tracking-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json"),
    )?;
    std::fs::rename(
        root.join("tracking-writer/reservation.saved"),
        root.join("tracking-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json"),
    )?;
    drop(held);
    let exit = exited(&handle)?;
    assert_eq!(exit.stop_reason, None);
    assert_eq!(
        exit.warnings
            .iter()
            .filter(|warning| *warning == "descendant journal publication failed")
            .count(),
        1,
        "{exit:?}"
    );
    Ok(())
}
fn live_provider_publishes_initial_checkpoint(root: &Path) -> Result<()> {
    let state = root.join("PROVENANT_STATE_DIR");
    std::fs::create_dir(&state)?;
    std::fs::write(state.join("s2-census-skip-gate-once"), b"skip first census")?;
    std::fs::write(state.join(S2_CENSUS_GATE_ONCE), b"hold second live census")?;
    let socket = root.join("initial.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "initial",
        "wait-on-socket",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    wait_for_file(&state.join(S2_CENSUS_PAUSED))?;
    // Allow one loop turn even if spawning took more than the census interval.
    // Pause the second census before the periodic checkpoint; Run must already
    // have an initial observation of the healthy provider.
    let checkpoint = std::fs::read(root.join("initial-attempt/process.json"));
    std::fs::write(state.join(S2_CENSUS_RELEASE), b"resume")?;
    drop(held);
    let exit = exited(&handle)?;
    let checkpoint: serde_json::Value = serde_json::from_slice(&checkpoint?)?;
    assert!(checkpoint["provider"].is_object());
    assert_eq!(checkpoint["phase"], "running");
    assert_eq!(exit.stop_reason, None);
    Ok(())
}
fn held_custody_is_released_only_after_guardian_exit(root: &Path) -> Result<()> {
    for index in 0..3 {
        let name = format!("custody-{index}");
        let socket = root.join(format!("custody-{index}.sock"));
        let listener = UnixListener::bind(&socket)?;
        let (_writer, handle, identity) = start_with_custody_identity(
            root,
            &name,
            "socket-blocked",
            &[socket.into_os_string()],
            CapturePlan::default(),
        )?;
        let mut provider = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
        byte(&mut provider, b'R')?;
        assert!(!handle.custody_released(&identity));
        let guardian = reservation_identity(&root.join(format!("{name}-writer")), "guardian")?;
        stop(&handle, StopReason::ExplicitCancellation)?;
        exited(&handle)?;
        fixture::guardian_exit_status(u32::try_from(guardian.pid)?, Duration::from_secs(5))?;
        assert!(handle.custody_released(&identity));
    }
    Ok(())
}
fn normal_exit_and_terminal_beyond_capture_cap(root: &Path) -> Result<()> {
    let (_writer, handle) = start(
        root,
        "normal",
        "bytes",
        &[],
        CapturePlan {
            stdout: StreamMode::Capture { limit: 3 },
            ..CapturePlan::default()
        },
    )?;
    let exit = exited(&handle)?;
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    assert!(exit.terminal_observed);
    assert!(
        !exit.unresolved_cleanup,
        "{exit:?}; reservation={:?}",
        std::fs::read_to_string(
            root.join("normal-writer")
                .join(scope().attempt.to_string())
                .join("reservation.json")
        )
    );
    #[cfg(target_os = "macos")]
    {
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(
            root.join("normal-attempt/process-exit.json"),
        )?)?;
        assert_eq!(
            record["settlement_evidence"],
            "darwin_census_and_custody_lease"
        );
    }
    assert_eq!(exit.stdout.bytes, 14);
    assert_eq!(
        std::fs::read(root.join("normal-attempt/stdout.log"))?,
        b"a\0b"
    );
    assert!(
        root.join("normal-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .is_file()
    );
    Ok(())
}
fn cancelled_reader_preserves_healthy_custody_and_cancel_escalates(root: &Path) -> Result<()> {
    let socket = root.join("cancel.sock");
    let listener = UnixListener::bind(&socket)?;
    let (writer, handle) = start(
        root,
        "cancel",
        "socket-blocked",
        &[socket.into_os_string()],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(matches!(
        observe(&handle, budget(&clock)?, &cancel, &clock),
        Err(CustodyError::Cancelled)
    ));
    assert!(!root.join("cancel-attempt/process-exit.json").exists());
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::Requested
    );
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::AlreadyRequested
    );
    let exit = exited(&handle)?;
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Signal(9))
    );
    assert_eq!(exit.stop_reason, Some(StopReason::ExplicitCancellation));
    drop(writer);
    assert!(
        root.join("cancel-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .is_file()
    );
    Ok(())
}
fn held_stop_ignores_lane_written_summary_and_cancel_request(root: &Path) -> Result<()> {
    let socket = root.join("held.sock");
    let listener = UnixListener::bind(&socket)?;
    let (writer, _handle) = start(
        root,
        "held",
        "socket-blocked",
        &[socket.into_os_string()],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    // A lane that can write the attempt directory deletes the launch summary
    // and plants a stop for another requester, which the guardian ignores.
    let attempt = root.join("held-attempt");
    std::fs::remove_file(attempt.join("launch.json"))?;
    let scope = scope();
    let planted = serde_json::json!({
        "schema_version": 1,
        "scope": scope,
        "request_id": format!("{}.{}.{}.cancel", scope.run, scope.attempt, scope.generation),
        "requester": provenant_core::ActorId::from_bytes([66; 16]),
        "reason": "explicit_cancellation",
    });
    std::fs::write(attempt.join("cancel.json"), serde_json::to_vec(&planted)?)?;
    let handle = hold(
        Directory::open_anchor(&attempt)?,
        scope,
        provenant_core::ActorId::from_bytes([4; 16]),
    );
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::Requested
    );
    let exit = exited(&handle)?;
    assert_eq!(exit.stop_reason, Some(StopReason::ExplicitCancellation));
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::AlreadyRequested
    );
    drop(writer);
    Ok(())
}
fn cancel_request(scope: Scope, requester: provenant_core::ActorId) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "scope": scope,
        "request_id": format!("{}.{}.{}.cancel", scope.run, scope.attempt, scope.generation),
        "requester": requester,
        "reason": "explicit_cancellation",
    })
}
// The test cannot reach the supervisor's private mkfifo binding.
#[allow(clippy::disallowed_methods)]
fn mkfifo(path: &Path) -> Result<()> {
    if !std::process::Command::new("/usr/bin/mkfifo")
        .arg(path)
        .status()?
        .success()
    {
        return Err("mkfifo failed".into());
    }
    Ok(())
}
fn held_stop_replaces_what_a_lane_left_but_reattach_keeps_a_later_generation(
    root: &Path,
) -> Result<()> {
    let requester = provenant_core::ActorId::from_bytes([4; 16]);
    let later = Scope {
        generation: 2,
        ..scope()
    };
    let outside = root.join("outside");
    std::fs::write(&outside, b"untouched")?;
    type Plant<'a> = &'a dyn Fn(&Path) -> Result<()>;
    let plants: [(&str, Plant<'_>); 5] = [
        ("garbage", &|path| {
            Ok(std::fs::write(path, b"not a request")?)
        }),
        ("later", &|path| {
            Ok(std::fs::write(
                path,
                serde_json::to_vec(&cancel_request(later, requester))?,
            )?)
        }),
        ("symlink", &|path| {
            Ok(std::os::unix::fs::symlink(&outside, path)?)
        }),
        ("fifo", &mkfifo),
        ("oversize", &|path| {
            Ok(std::fs::write(path, vec![b' '; 2 << 20])?)
        }),
    ];
    // A held handle replaces whatever a lane left, since the guardian would
    // ignore it, and the replacement is a regular file naming its requester.
    for (name, plant) in plants {
        let attempt = root.join(format!("{name}-attempt"));
        std::fs::create_dir(&attempt)?;
        plant(&attempt.join("cancel.json"))?;
        let handle = hold(Directory::open_anchor(&attempt)?, scope(), requester);
        assert_eq!(
            stop(&handle, StopReason::ExplicitCancellation)?,
            StopReceipt::Requested,
            "{name}"
        );
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(attempt.join("cancel.json"))?)?;
        assert_eq!(written, cancel_request(scope(), requester), "{name}");
    }
    assert_eq!(std::fs::read(&outside)?, b"untouched");
    // A directory refuses the rename, so it stays an accepted limit.
    let blocked = root.join("directory-attempt");
    std::fs::create_dir_all(blocked.join("cancel.json"))?;
    assert!(
        stop(
            &hold(Directory::open_anchor(&blocked)?, scope(), requester),
            StopReason::ExplicitCancellation
        )
        .is_err()
    );
    // A reattached handle may be stale, so it never displaces a later
    // generation's request.
    let attempt = root.join("reattach-attempt");
    std::fs::create_dir(&attempt)?;
    let summary =
        serde_json::json!({"schema_version": 1, "scope": scope(), "requester": requester});
    std::fs::write(attempt.join("launch.json"), serde_json::to_vec(&summary)?)?;
    std::fs::write(
        attempt.join("cancel.json"),
        serde_json::to_vec(&cancel_request(later, requester))?,
    )?;
    let handle = reattach(Directory::open_anchor(&attempt)?, scope())?;
    assert!(matches!(
        stop(&handle, StopReason::ExplicitCancellation),
        Err(CustodyError::InvalidRecord)
    ));
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(attempt.join("cancel.json"))?)?;
    assert_eq!(kept, cancel_request(later, requester));
    Ok(())
}
fn publication_failure_retains_the_lease_and_retries_without_overwrite(root: &Path) -> Result<()> {
    let socket = root.join("pending.sock");
    let listener = UnixListener::bind(&socket)?;
    let (writer, handle) = start(
        root,
        "pending",
        "wait-on-socket",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    let destination = root.join("pending-attempt/process-exit.json");
    std::fs::create_dir(&destination)?;
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observation = std::fs::read(root.join("pending-attempt/process.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        if observation
            .as_ref()
            .and_then(|v| v.get("publication_pending"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("publication pending was not observable").into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    drop(writer);
    let clock = SystemClock::new();
    assert!(
        Directory::open_anchor(&root.join("pending-writer"))?
            .lock(
                OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
                WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
                &CancelToken::new(),
                &clock
            )
            .is_err()
    );
    std::fs::remove_dir(&destination)?;
    let exit = exited(&handle)?;
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    assert_eq!(exit.stop_reason, None);
    let bytes = std::fs::read(&destination)?;
    let directory = Directory::open_anchor(&root.join("pending-attempt"))?;
    assert!(
        directory
            .atomic_write(
                OsStr::new("process-exit.json"),
                ".late-",
                b"{}",
                provenant_os::fs::PublishMode::NoReplace
            )
            .is_err()
    );
    assert_eq!(std::fs::read(destination)?, bytes);
    Ok(())
}
fn never_spawned_cancel_is_published(root: &Path) -> Result<()> {
    let writer_path = root.join("prelaunch-writer");
    let attempt_path = root.join("prelaunch-attempt");
    std::fs::create_dir(&writer_path)?;
    std::fs::create_dir(&attempt_path)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut writer = fixture::acquire_writer(
        Directory::open_anchor(&writer_path)?,
        scope(),
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(&attempt_path)?,
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let command = ProviderCommand {
        priority: 0,
        provider_lease: None,
        requester: provenant_core::ActorId::from_bytes([4; 16]),
        resolved_route: "fixture/provider".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([0; 32]),
        task: provenant_core::TaskId::from_bytes([3; 16]),
        protected_commands: fixture::protected_commands(),
        config_environment: fixture::config_environment(),
        argv: fixture::argv("exit")?,
        environment: fixture::environment(root),
        cwd: root.into(),
        stdin: vec![],
        secret_input: None,
        holder_context: Vec::new(),
    };
    cancel.cancel();
    let failure = launch(
        OwnedLaunch {
            command,
            lease: &mut writer,
            handshake: budget(&clock)?,
            cancel: &cancel,
            clock: &clock,
        },
        CapturePlan::default(),
        publication,
    )
    .err()
    .ok_or_else(|| io::Error::other("cancelled launch unexpectedly succeeded"))?;
    assert!(matches!(failure.error, CustodyError::Cancelled));
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(attempt_path.join("process-exit.json"))?)?;
    assert_eq!(record["never_spawned"], true);
    assert!(!attempt_path.join("launch.json").exists());
    Ok(())
}
fn terminal_frame_closes_provider_stdin(root: &Path) -> Result<()> {
    let writer_path = root.join("stdin-writer");
    let attempt_path = root.join("stdin-attempt");
    std::fs::create_dir(&writer_path)?;
    std::fs::create_dir(&attempt_path)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut writer = fixture::acquire_writer(
        Directory::open_anchor(&writer_path)?,
        scope(),
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(&attempt_path)?,
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let command = ProviderCommand {
        priority: 0,
        provider_lease: None,
        requester: provenant_core::ActorId::from_bytes([4; 16]),
        resolved_route: "fixture/provider".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([0; 32]),
        task: provenant_core::TaskId::from_bytes([3; 16]),
        protected_commands: fixture::protected_commands(),
        config_environment: fixture::config_environment(),
        argv: fixture::argv("terminal-stdin")?,
        environment: fixture::environment(root),
        cwd: root.into(),
        stdin: vec![b'p'; 256 * 1024],
        secret_input: None,
        holder_context: Vec::new(),
    };
    let handle = launch(
        OwnedLaunch {
            command,
            lease: &mut writer,
            handshake: budget(&clock)?,
            cancel: &cancel,
            clock: &clock,
        },
        CapturePlan::default(),
        publication,
    )?;
    let exit = exited(&handle)?;
    assert!(exit.terminal_observed);
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    assert_eq!(
        std::fs::read(attempt_path.join("stdout.log"))?,
        b"terminal\nEOF\n"
    );
    Ok(())
}
fn owner_process(root: &Path) -> Result<()> {
    let (writer, _handle) = start(
        root,
        "orphan",
        "wait-on-socket",
        &[root.join("orphan.sock").into_os_string()],
        CapturePlan::default(),
    )?;
    let mut peer = UnixStream::connect(root.join("owner-ready.sock"))?;
    use std::io::{Read, Write};
    peer.write_all(b"R")?;
    let mut bytes = [0];
    let _ = peer.read(&mut bytes)?;
    drop(writer);
    Ok(())
}
fn setsid_descendant_is_tracked_before_exit_and_cleanup_is_conservative(root: &Path) -> Result<()> {
    let ready_path = root.join("setsid-ready.sock");
    let gate_path = root.join("setsid-gate.sock");
    let ready_listener = UnixListener::bind(&ready_path)?;
    let gate_listener = UnixListener::bind(&gate_path)?;
    let (_writer, handle) = start(
        root,
        "setsid",
        "s2-setsid-provider",
        &[ready_path.into_os_string(), gate_path.into_os_string()],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut descendant_stream = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
    byte(&mut descendant_stream, b'R')?;
    let pid = read_pid_line(&mut descendant_stream)?;
    let descendant = process_identity(pid)?;
    assert_eq!(descendant.session_id, pid);
    assert_eq!(descendant.group_id, pid);
    let mut gate = fixture::fixture_accept(&gate_listener, Duration::from_secs(5))?;
    byte(&mut gate, b'R')?;
    wait_for_descendant(
        &root
            .join("setsid-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json"),
        &descendant,
    )?;
    drop(gate);
    let exit = exited(&handle)?;
    assert!(
        exit.unresolved_cleanup,
        "a live setsid descendant still holds custody"
    );
    drop(descendant_stream);
    wait_for_identity_absence(&descendant)?;
    Ok(())
}
/// A lane with an MCP child registers a nested owner whose custody macOS cannot
/// prove; that must not stop cleanup ending a different, exact escapee, and
/// cleanup must not repeat a signal to a provider that is already gone.
#[cfg(target_os = "macos")]
fn escapee_is_ended_beside_an_unprovable_nested_owner_without_repeated_failures(
    root: &Path,
) -> Result<()> {
    let home = root.join("HOME");
    std::fs::create_dir_all(&home)?;
    let ready_path = root.join("esc-ready.sock");
    let owner_path = root.join("esc-owner.sock");
    let ready_listener = UnixListener::bind(&ready_path)?;
    let owner_listener = UnixListener::bind(&owner_path)?;
    let (_writer, handle) = start(
        root,
        "esc",
        "s2-registered-detach-provider",
        &[
            ready_path.into_os_string(),
            scope().attempt.sandbox_lookup_name().into(),
            owner_path.into_os_string(),
        ],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut stream = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
    byte(&mut stream, b'R')?;
    let escapee = process_identity(read_pid_line(&mut stream)?)?;
    let mut owner = fixture::fixture_accept(&owner_listener, Duration::from_secs(5))?;
    byte(&mut owner, b'R')?;
    let owner_identity: ProcessIdentity =
        serde_json::from_slice(&std::fs::read(home.join("s2-owner.json"))?)?;
    let exit = exited(&handle)?;
    // The owner is protected and still holds custody, so the exit is unresolved
    // for the whole cleanup budget; the escapee is nevertheless gone.
    assert!(exit.unresolved_cleanup, "{exit:?}");
    wait_for_identity_absence(&escapee)?;
    assert!(!exit.unresolved_pids.contains(&escapee.pid), "{exit:?}");
    process_identity(owner_identity.pid)?;
    // Every failed provider signal also warns, so none means none repeated.
    assert!(
        !exit
            .warnings
            .iter()
            .any(|warning| warning.contains("signal failed")),
        "{exit:?}"
    );
    assert_eq!(exit.omitted_warnings, 0, "{exit:?}");
    drop(owner);
    wait_for_identity_absence(&owner_identity)?;
    Ok(())
}
/// A host holder whose admission always fails, as the refusal its marker names.
struct RefusingHolder {
    clock: bool,
}
impl provenant_supervisor::Holder for RefusingHolder {
    fn admit(
        &self,
        _context: &[u8],
        _attempt: &Directory,
    ) -> std::result::Result<(), CustodyError> {
        Err(if self.clock {
            CustodyError::AdmissionClock
        } else {
            CustodyError::Io(io::Error::other("/private/state/admission.sqlite3"))
        })
    }
    fn terminal(
        &self,
        _context: &[u8],
        attempt: &Directory,
        exit: &ExitObservation,
        _custody: &Recovery,
    ) -> std::result::Result<(), CustodyError> {
        // Settle at once, so the guardian does not retry release for long.
        attempt.mkdir(OsStr::new("quarantine"), 0o700)?;
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "scope": exit.scope,
            "kind": "quarantine_manifest",
        }))
        .map_err(|_| CustodyError::InvalidRecord)?;
        attempt
            .open_verified_directory(OsStr::new("quarantine"))?
            .atomic_write(
                OsStr::new("manifest.json"),
                ".test-",
                &manifest,
                provenant_os::fs::PublishMode::NoReplace,
            )?
            .require_durable()?;
        publish_release(attempt, exit.scope)
    }
}
/// The guardian publishes the reason for a refused admission: an unreadable
/// host clock is named, and any other error leaves only the generic reason,
/// with none of its text.
#[cfg(target_os = "macos")]
fn the_guardian_names_an_unreadable_host_clock_in_its_admission_refusal(root: &Path) -> Result<()> {
    let home = root.join("HOME");
    std::fs::create_dir_all(&home)?;
    for (marker, name, reason) in [
        (
            "clock",
            "unreadable-clock",
            "startup spacing cannot be judged: host clock or boot identity unreadable",
        ),
        ("io", "other-error", "admission refused"),
    ] {
        std::fs::write(home.join(S2_REFUSE_ADMISSION), marker)?;
        let (_writer, handle) = start(root, name, "exit", &[], CapturePlan::default())?;
        let exit = exited(&handle)?;
        assert!(exit.never_spawned, "{exit:?}");
        assert_eq!(exit.warnings, vec![reason.to_owned()], "{exit:?}");
    }
    Ok(())
}
/// The production shape: no registry is wired, so the host's child store
/// decides. A store with no nested run ends the escapee; one with a run, which
/// could hold a nested owner, leaves it alive and reported.
#[cfg(target_os = "macos")]
fn escapee_is_ended_only_when_the_host_reports_no_nested_runs(root: &Path) -> Result<()> {
    let home = root.join("HOME");
    std::fs::create_dir_all(&home)?;
    std::fs::write(home.join(S2_PRODUCTION_SHAPE), b"no registry")?;
    let case = |name: &str| -> Result<(ExitObservation, ProcessIdentity, UnixStream)> {
        let ready_path = root.join(format!("{name}.sock"));
        let ready_listener = UnixListener::bind(&ready_path)?;
        let (_writer, handle) = start(
            root,
            name,
            "s2-detach-provider",
            &[
                ready_path.into_os_string(),
                scope().attempt.sandbox_lookup_name().into(),
            ],
            CapturePlan {
                root_flush_ms: 30,
                descendant_grace_ms: 10,
                ..CapturePlan::default()
            },
        )?;
        let mut stream = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
        byte(&mut stream, b'R')?;
        let escapee = process_identity(read_pid_line(&mut stream)?)?;
        let exit = exited(&handle)?;
        Ok((exit, escapee, stream))
    };
    // The lane's MCP creates its store at startup, with no nested run.
    std::fs::create_dir_all(home.join("children/attempt/tmp"))?;
    std::fs::write(home.join("children/attempt/run.sqlite3"), b"")?;
    let (exit, escapee, _stream) = case("empty")?;
    assert!(!exit.unresolved_cleanup, "{exit:?}");
    wait_for_identity_absence(&escapee)?;
    std::fs::create_dir_all(home.join("children/attempt/runs/nested-run"))?;
    let (exit, escapee, stream) = case("records")?;
    assert!(exit.unresolved_cleanup, "{exit:?}");
    assert_eq!(exit.unresolved_pids, vec![escapee.pid], "{exit:?}");
    process_identity(escapee.pid)?;
    // Only the process this test started: it exits when its socket closes.
    drop(stream);
    wait_for_identity_absence(&escapee)?;
    Ok(())
}
#[cfg(target_os = "macos")]
fn detached_descendant_in_the_attempt_sandbox_is_found_and_ended(root: &Path) -> Result<()> {
    let ready_path = root.join("mark-ready.sock");
    let ready_listener = UnixListener::bind(&ready_path)?;
    let (_writer, handle) = start(
        root,
        "mark",
        "s2-detach-provider",
        &[
            ready_path.into_os_string(),
            scope().attempt.sandbox_lookup_name().into(),
        ],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut stream = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
    byte(&mut stream, b'R')?;
    let descendant = process_identity(read_pid_line(&mut stream)?)?;
    assert_eq!(descendant.session_id, descendant.pid);
    // The descendant ignores TERM and left the group before any census; its
    // inherited sandbox is the only way cleanup can reach it. It must be gone before a clean settle.
    let exit = exited(&handle)?;
    assert!(!exit.unresolved_cleanup, "{exit:?}");
    assert!(exit.unresolved_pids.is_empty(), "{exit:?}");
    wait_for_identity_absence(&descendant)?;
    Ok(())
}
fn cancel_kills_the_whole_group_and_settles(root: &Path) -> Result<()> {
    let ready_path = root.join("group-ready.sock");
    let gate_path = root.join("group-gate.sock");
    let ready_listener = UnixListener::bind(&ready_path)?;
    let gate_listener = UnixListener::bind(&gate_path)?;
    let (_writer, handle) = start(
        root,
        "group",
        "s2-group-provider",
        &[ready_path.into_os_string(), gate_path.into_os_string()],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    // The child ignores TERM and shares the provider's process group.
    let mut child_stream = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
    byte(&mut child_stream, b'R')?;
    let child = process_identity(read_pid_line(&mut child_stream)?)?;
    let mut gate = fixture::fixture_accept(&gate_listener, Duration::from_secs(5))?;
    byte(&mut gate, b'R')?;
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::Requested
    );
    // The provider stays gated, so cancellation (not a natural exit) ends it.
    let exit = exited(&handle)?;
    drop(gate);
    assert_eq!(exit.stop_reason, Some(StopReason::ExplicitCancellation));
    assert!(!exit.unresolved_cleanup, "{exit:?}");
    assert!(exit.unresolved_pids.is_empty());
    wait_for_identity_absence(&child)?;
    Ok(())
}
fn natural_exit_ends_same_group_leftovers_and_settles(root: &Path) -> Result<()> {
    let ready_path = root.join("left-ready.sock");
    let gate_path = root.join("left-gate.sock");
    let ready_listener = UnixListener::bind(&ready_path)?;
    let gate_listener = UnixListener::bind(&gate_path)?;
    let (_writer, handle) = start(
        root,
        "left",
        "s2-group-provider",
        &[ready_path.into_os_string(), gate_path.into_os_string()],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    // The child ignores TERM and shares the provider's process group.
    let mut child_stream = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
    byte(&mut child_stream, b'R')?;
    let child = process_identity(read_pid_line(&mut child_stream)?)?;
    let mut gate = fixture::fixture_accept(&gate_listener, Duration::from_secs(5))?;
    byte(&mut gate, b'R')?;
    // The provider exits on its own; cleanup must still end the leftover.
    drop(gate);
    let exit = exited(&handle)?;
    assert_eq!(exit.stop_reason, None, "{exit:?}");
    assert!(!exit.unresolved_cleanup, "{exit:?}");
    assert!(exit.unresolved_pids.is_empty());
    wait_for_identity_absence(&child)?;
    Ok(())
}
fn cancel_after_unresolved_exit_republishes_a_settled_exit(root: &Path) -> Result<()> {
    let ready_path = root.join("late-ready.sock");
    let gate_path = root.join("late-gate.sock");
    let ready_listener = UnixListener::bind(&ready_path)?;
    let gate_listener = UnixListener::bind(&gate_path)?;
    let (_writer, handle) = start(
        root,
        "late",
        "s2-group-provider",
        &[ready_path.into_os_string(), gate_path.into_os_string()],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut child_stream = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
    byte(&mut child_stream, b'R')?;
    let child = process_identity(read_pid_line(&mut child_stream)?)?;
    let mut gate = fixture::fixture_accept(&gate_listener, Duration::from_secs(5))?;
    byte(&mut gate, b'R')?;
    // The provider exits on its own, leaving the TERM-ignoring child behind.
    drop(gate);
    let first = exited(&handle)?;
    // A live detached descendant can keep the attempt fenced.
    if first.unresolved_cleanup {
        assert!(first.unresolved_pids.contains(&child.pid), "{first:?}");
        assert_eq!(
            stop(&handle, StopReason::ExplicitCancellation)?,
            StopReceipt::Requested
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let settled = loop {
            let exit = exited(&handle)?;
            if !exit.unresolved_cleanup {
                break exit;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("cancel did not settle the exit").into());
            }
            std::thread::park_timeout(Duration::from_millis(50));
        };
        assert_eq!(settled.stop_reason, Some(StopReason::ExplicitCancellation));
        assert!(settled.unresolved_pids.is_empty());
    }
    wait_for_identity_absence(&child)?;
    Ok(())
}

struct FileNestedChildren {
    home: PathBuf,
}
impl NestedChildStore for FileNestedChildren {
    fn has_no_nested_runs(&self, _: &[u8]) -> bool {
        match std::fs::read_dir(self.home.join("children/attempt/runs")) {
            Ok(mut entries) => entries.next().is_none(),
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        }
    }
}
struct FileChildRegistry {
    home: PathBuf,
}
impl ChildRegistry for FileChildRegistry {
    fn registered_children(&self) -> io::Result<Vec<RegisteredChild>> {
        if let Some(state_dir) = std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from)
            && state_dir.join(S2_IGNORE_CHILD_REGISTRY).is_file()
        {
            std::fs::write(
                state_dir.join(S2_SUPPRESSED_REGISTRY_READ),
                b"stale-census registry snapshot",
            )?;
            return Ok(Vec::new());
        }
        let identity_path = self.home.join("s2-owner.json");
        let bytes = match std::fs::read(identity_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let identity: ProcessIdentity = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let custody =
            FileHandle::open_source(&self.home.join("s2-owner.lock")).map_err(io::Error::other)?;
        let registered = RegisteredChild::new(identity, custody).map_err(io::Error::other)?;
        let reads_path = self.home.join("s2-registry-reads");
        let reads = std::fs::read_to_string(&reads_path)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_add(1);
        std::fs::write(reads_path, reads.to_string())?;
        Ok(vec![registered])
    }
}

fn nested_owner_stays_protected_when_native_custody_proof_is_uncertain(root: &Path) -> Result<()> {
    let home = root.join("HOME");
    std::fs::create_dir_all(&home)?;
    let ready_path = root.join("owner-ready-s2.sock");
    let gate_path = root.join("owner-gate-s2.sock");
    let ready_listener = UnixListener::bind(&ready_path)?;
    let gate_listener = UnixListener::bind(&gate_path)?;
    let (_writer, handle) = start(
        root,
        "nested-owner",
        "s2-registered-provider",
        &[ready_path.into_os_string(), gate_path.into_os_string()],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut owner = fixture::fixture_accept(&ready_listener, Duration::from_secs(5))?;
    byte(&mut owner, b'R')?;
    let bytes = std::fs::read(home.join("s2-owner.json"))?;
    let identity: ProcessIdentity = serde_json::from_slice(&bytes)?;
    let mut gate = fixture::fixture_accept(&gate_listener, Duration::from_secs(5))?;
    byte(&mut gate, b'R')?;
    wait_for_descendant(
        &root
            .join("nested-owner-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json"),
        &identity,
    )?;
    wait_for_registry_reads(&home, 2)?;
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::Requested
    );
    drop(gate);
    let exit = exited(&handle)?;
    assert!(exit.unresolved_cleanup);
    let clock = SystemClock::new();
    assert!(
        Directory::open_anchor(&home)?
            .lock(
                OsStr::new("s2-owner.lock"),
                LockMode::Exclusive,
                WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 1)?,
                &CancelToken::new(),
                &clock,
            )
            .is_err()
    );
    drop(owner);
    wait_for_identity_absence(&identity)?;
    Ok(())
}

fn stale_complete_census_does_not_signal_a_late_shared_group_owner(root: &Path) -> Result<()> {
    let state_dir = root.join("PROVENANT_STATE_DIR");
    let home = root.join("HOME");
    std::fs::create_dir_all(&state_dir)?;
    std::fs::create_dir_all(&home)?;
    std::fs::write(state_dir.join(S2_CENSUS_GATE_ONCE), b"pause first census")?;
    std::fs::write(
        state_dir.join(S2_IGNORE_CHILD_REGISTRY),
        b"hide late registration",
    )?;

    let owner_ready_path = root.join("late-owner-ready.sock");
    let launch_gate_path = root.join("late-owner-launch.sock");
    let hold_gate_path = root.join("late-owner-hold.sock");
    let owner_ready_listener = UnixListener::bind(&owner_ready_path)?;
    let launch_gate_listener = UnixListener::bind(&launch_gate_path)?;
    let hold_gate_listener = UnixListener::bind(&hold_gate_path)?;
    let (_writer, handle) = start(
        root,
        "late-owner",
        "s2-late-registered-provider",
        &[
            owner_ready_path.into_os_string(),
            launch_gate_path.into_os_string(),
            hold_gate_path.into_os_string(),
        ],
        CapturePlan {
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;

    let mut launch_gate = fixture::fixture_accept(&launch_gate_listener, Duration::from_secs(5))?;
    byte(&mut launch_gate, b'R')?;
    wait_for_file(&state_dir.join(S2_CENSUS_PAUSED))?;
    drop(launch_gate);

    let mut owner_channel = fixture::fixture_accept(&owner_ready_listener, Duration::from_secs(5))?;
    byte(&mut owner_channel, b'R')?;
    let owner_bytes = std::fs::read(home.join("s2-owner.json"))?;
    let owner: ProcessIdentity = serde_json::from_slice(&owner_bytes)?;
    let reservation_bytes = std::fs::read(
        root.join("late-owner-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json"),
    )?;
    let reservation: serde_json::Value = serde_json::from_slice(&reservation_bytes)?;
    let provider: ProcessIdentity = serde_json::from_value(
        reservation
            .get("provider")
            .filter(|identity| !identity.is_null())
            .cloned()
            .ok_or_else(|| io::Error::other("provider identity was not published"))?,
    )?;
    assert_eq!(
        owner.group_id, provider.group_id,
        "owner shares provider PGID"
    );
    assert_eq!(owner.session_id, provider.session_id);

    let mut hold_gate = fixture::fixture_accept(&hold_gate_listener, Duration::from_secs(5))?;
    byte(&mut hold_gate, b'R')?;
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::Requested
    );
    std::fs::write(state_dir.join(S2_CENSUS_RELEASE), b"resume old snapshot")?;
    wait_for_file(&state_dir.join(S2_SUPPRESSED_REGISTRY_READ))?;
    std::fs::remove_file(state_dir.join(S2_IGNORE_CHILD_REGISTRY))?;

    let exit = exited(&handle)?;
    assert!(exit.unresolved_cleanup);
    let clock = SystemClock::new();
    assert!(
        Directory::open_anchor(&home)?
            .lock(
                OsStr::new("s2-owner.lock"),
                LockMode::Exclusive,
                WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 1)?,
                &CancelToken::new(),
                &clock,
            )
            .is_err(),
        "late healthy owner must retain its custody lock"
    );

    drop(owner_channel);
    drop(hold_gate);
    wait_for_identity_absence(&owner)?;
    Ok(())
}

fn reservation_identity(writer_path: &Path, field: &str) -> Result<ProcessIdentity> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(
        writer_path
            .join(scope().attempt.to_string())
            .join("reservation.json"),
    )?)?;
    serde_json::from_value(
        value
            .get(field)
            .filter(|identity| !identity.is_null())
            .cloned()
            .ok_or_else(|| io::Error::other(format!("reservation omitted {field} identity")))?,
    )
    .map_err(Into::into)
}

fn assert_peer_open(stream: &mut UnixStream) -> Result<()> {
    stream.set_nonblocking(true)?;
    let mut byte = [0];
    let result = stream.read(&mut byte);
    stream.set_nonblocking(false)?;
    match result {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Ok(0) => Err(io::Error::other("provider closed its control socket").into()),
        Ok(_) => Err(io::Error::other("provider wrote unexpected control data").into()),
        Err(error) => Err(error.into()),
    }
}

fn checkpoint_enospc_setup(root: &Path) -> Result<PathBuf> {
    let state_dir = root.join("PROVENANT_STATE_DIR");
    std::fs::create_dir_all(&state_dir)?;
    std::fs::write(
        state_dir.join(fixture::S2_EXIT_PUBLICATION_CONFIG),
        b"enospc-post-link-sync-v1\n",
    )?;
    std::fs::write(
        state_dir.join(fixture::S2_PROCESS_CHECKPOINT_ENOSPC),
        b"inject checkpoint ENOSPC",
    )?;
    Ok(state_dir)
}

fn checkpoint_failure_preserves_terminal_publication(root: &Path) -> Result<()> {
    let state_dir = checkpoint_enospc_setup(root)?;
    let socket = root.join("terminal-checkpoint.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "terminal-checkpoint",
        "terminal-blocked",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    wait_for_file(&state_dir.join(fixture::S2_PROCESS_CHECKPOINT_FAILED))?;
    assert_peer_open(&mut held)?;
    drop(held);

    let exit = exited(&handle)?;
    assert!(
        exit.terminal_observed,
        "terminal frame must survive checkpoint failure"
    );
    assert_eq!(exit.stop_reason, None);
    assert!(exit.delivered_signals.is_empty());
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    assert!(
        root.join("terminal-checkpoint-attempt/process-exit.json")
            .is_file()
    );
    Ok(())
}

fn checkpoint_enospc_while_provider_lives_never_signals_it(root: &Path) -> Result<()> {
    let state_dir = checkpoint_enospc_setup(root)?;
    let socket = root.join("checkpoint-live.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "checkpoint-live",
        "wait-on-socket",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    wait_for_file(&state_dir.join(fixture::S2_PROCESS_CHECKPOINT_FAILED))?;
    assert_peer_open(&mut held)?;
    drop(held);

    let exit = exited(&handle)?;
    assert_eq!(exit.stop_reason, None);
    assert!(exit.delivered_signals.is_empty());
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    Ok(())
}

struct DiskBuild {
    _writer: CustodyLease,
    handle: CustodyHandle,
    build: ProcessIdentity,
    channel: UnixStream,
    gate: UnixStream,
    state: PathBuf,
}

fn disk_build(root: &Path) -> Result<DiskBuild> {
    let state = root.join("PROVENANT_STATE_DIR");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join(S2_DISK_PRESSURE), b"high")?;
    let build_socket = root.join("disk-build.sock");
    let gate_socket = root.join("disk-gate.sock");
    let build_listener = UnixListener::bind(&build_socket).map_err(|error| {
        io::Error::new(error.kind(), format!("bind disk build socket: {error}"))
    })?;
    let gate_listener = UnixListener::bind(&gate_socket)
        .map_err(|error| io::Error::new(error.kind(), format!("bind disk gate socket: {error}")))?;
    let (writer, handle) = start(
        root,
        "disk",
        "s2-disk-build-provider",
        &[build_socket.into_os_string(), gate_socket.into_os_string()],
        CapturePlan {
            descendant_grace_ms: 500,
            ..CapturePlan::default()
        },
    )?;
    let mut channel = fixture::fixture_accept(&build_listener, Duration::from_secs(5))?;
    byte(&mut channel, b'R')?;
    let build = process_identity(read_pid_line(&mut channel)?)?;
    let mut gate = fixture::fixture_accept(&gate_listener, Duration::from_secs(5))?;
    byte(&mut gate, b'R')?;
    channel.write_all(b"I")?;
    byte(&mut channel, b'I')?;
    set_disk_sample(&state, b"low")?;
    wait_for_paused_build(&mut channel)?;
    Ok(DiskBuild {
        _writer: writer,
        handle,
        build,
        channel,
        gate,
        state,
    })
}

fn wait_for_paused_build(channel: &mut UnixStream) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        channel.write_all(b"A")?;
        match fixture::fixture_read_byte(channel, Duration::from_millis(250)) {
            Err(error) if error.kind() == io::ErrorKind::TimedOut => return Ok(()),
            Ok(byte) => assert_eq!(byte, b'A'),
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("owned cargo descendant was not SIGSTOPped").into());
        }
    }
}

fn set_disk_sample(state: &Path, sample: &[u8]) -> Result<()> {
    std::fs::write(state.join(S2_DISK_PRESSURE), sample)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read(state.join(S2_DISK_SAMPLED)).is_ok_and(|seen| seen == sample) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("guardian did not sample disk decision").into());
        }
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

fn disk_floor_pauses_and_recovery_resumes_builds(root: &Path) -> Result<()> {
    let mut fixture = disk_build(root)?;

    set_disk_sample(&fixture.state, b"high")?;
    byte(&mut fixture.channel, b'A')?;
    drop(fixture.gate);
    // The build lives in its own session, so cleanup may not signal it; EOF
    // lets it exit so the census can settle.
    drop(fixture.channel);
    assert!(!exited(&fixture.handle)?.unresolved_cleanup);
    Ok(())
}

fn disk_sample_failure_resumes_paused_builds(root: &Path) -> Result<()> {
    let mut fixture = disk_build(root)?;
    set_disk_sample(&fixture.state, b"error-paused")?;
    // The pending heartbeat proves CONT was delivered without a good sample.
    byte(&mut fixture.channel, b'A')?;
    set_disk_sample(&fixture.state, b"error-running")?;
    fixture.channel.write_all(b"B")?;
    byte(&mut fixture.channel, b'B')?;
    drop(fixture.gate);
    drop(fixture.channel);
    assert!(!exited(&fixture.handle)?.unresolved_cleanup);
    Ok(())
}

fn cancel_resumes_paused_builds_before_delivering_stop(root: &Path) -> Result<()> {
    let mut fixture = disk_build(root)?;

    assert_eq!(
        stop(&fixture.handle, StopReason::ExplicitCancellation)?,
        StopReceipt::Requested
    );
    byte(&mut fixture.channel, b'A')?;
    drop(fixture.channel);
    let exit = exited(&fixture.handle)?;
    assert_eq!(exit.stop_reason, Some(StopReason::ExplicitCancellation));
    assert!(!exit.unresolved_cleanup);
    Ok(())
}

fn provider_exit_resumes_paused_builds(root: &Path) -> Result<()> {
    let mut fixture = disk_build(root)?;

    drop(fixture.gate);
    byte(&mut fixture.channel, b'A')?;
    drop(fixture.channel);
    let exit = exited(&fixture.handle)?;
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    assert!(!exit.unresolved_cleanup);
    Ok(())
}

fn guardian_sigkill_recovery_resumes_a_build_after_ancestry_is_lost(root: &Path) -> Result<()> {
    let fixture = disk_build(root)?;
    let guardian = reservation_identity(&root.join("disk-writer"), "guardian")?;
    let provider = reservation_identity(&root.join("disk-writer"), "provider")?;
    let attempt = root.join("disk-attempt");
    let paused: serde_json::Value =
        serde_json::from_slice(&std::fs::read(attempt.join("disk-paused.json"))?)?;
    assert!(
        paused["identities"].as_array().is_some_and(|identities| {
            identities
                .iter()
                .any(|identity| identity["pid"] == fixture.build.pid)
        }),
        "pause identity must be durable before SIGSTOP"
    );
    fixture::signal_recorded_process(&guardian, libc::SIGKILL)?;
    fixture::guardian_exit_status(u32::try_from(guardian.pid)?, Duration::from_secs(5))?;
    fixture::signal_recorded_process(&provider, libc::SIGKILL)?;
    wait_for_identity_absence(&provider)?;
    // The kernel reparents the build only after the provider is gone; wait for it.
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(
        fixture::RecordedBootObserver.observe(fixture.build.pid),
        ProcessObservation::Present(fact) if fact.parent_pid == provider.pid
    ) && Instant::now() < deadline
    {
        std::thread::park_timeout(Duration::from_millis(5));
    }
    match fixture::RecordedBootObserver.observe(fixture.build.pid) {
        ProcessObservation::Present(fact) => {
            assert_ne!(
                fact.parent_pid, provider.pid,
                "dead provider ancestry must be lost"
            );
        }
        other => return Err(io::Error::other(format!("paused build vanished: {other:?}")).into()),
    }
    let DiskBuild {
        _writer,
        mut channel,
        gate,
        build,
        ..
    } = fixture;
    drop(_writer);
    drop(gate);
    fixture::resume_orphaned_builds(&Directory::open_anchor(&attempt)?, scope())?;
    byte(&mut channel, b'A')?;
    assert!(!attempt.join("disk-paused.json").exists());
    let checkpoint: serde_json::Value =
        serde_json::from_slice(&std::fs::read(attempt.join("process.json"))?)?;
    assert_eq!(checkpoint["disk_paused"], false);
    drop(channel);
    wait_for_identity_absence(&build)?;
    Ok(())
}

fn guardian_signal_requests_provider_stop(root: &Path, signal: i32, label: &str) -> Result<()> {
    let socket = root.join(format!("guardian-{label}.sock"));
    let listener = UnixListener::bind(&socket)?;
    let name = format!("guardian-{label}");
    let (_writer, handle) = start(
        root,
        &name,
        "socket-blocked",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    let writer = root.join(format!("{name}-writer"));
    let guardian = reservation_identity(&writer, "guardian")?;
    fixture::signal_recorded_process(&guardian, signal)?;

    let exit = exited(&handle)?;
    assert_eq!(exit.stop_reason, Some(StopReason::SupervisorSignal));
    assert!(exit.delivered_signals.contains(&i64::from(libc::SIGTERM)));
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Signal(i64::from(
            libc::SIGKILL
        )))
    );
    let status =
        fixture::guardian_exit_status(u32::try_from(guardian.pid)?, Duration::from_secs(5))?;
    assert_eq!(
        status.code(),
        Some(0),
        "guardian exited abnormally: {status}"
    );
    wait_for_identity_absence(&guardian)?;
    Ok(())
}

fn guardian_sigterm_uses_supervisor_signal_path(root: &Path) -> Result<()> {
    guardian_signal_requests_provider_stop(root, libc::SIGTERM, "term")
}
fn guardian_sigint_uses_supervisor_signal_path(root: &Path) -> Result<()> {
    guardian_signal_requests_provider_stop(root, libc::SIGINT, "int")
}
fn guardian_sighup_uses_supervisor_signal_path(root: &Path) -> Result<()> {
    guardian_signal_requests_provider_stop(root, libc::SIGHUP, "hup")
}

fn wait_for_zombie(identity: &ProcessIdentity) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fixture::RecordedBootObserver.observe(identity.pid) {
            ProcessObservation::Present(fact) if fact.identity == *identity && fact.zombie => {
                return Ok(());
            }
            ProcessObservation::Absent => return Ok(()),
            _ if Instant::now() < deadline => std::thread::park_timeout(Duration::from_millis(5)),
            other => {
                return Err(io::Error::other(format!("provider did not exit: {other:?}")).into());
            }
        }
    }
}

fn natural_exit_races_cancel(root: &Path) -> Result<()> {
    let state_dir = root.join("PROVENANT_STATE_DIR");
    std::fs::create_dir_all(&state_dir)?;
    std::fs::write(state_dir.join(S2_CENSUS_GATE_ONCE), b"hold guardian census")?;
    let socket = root.join("natural-cancel-race.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "natural-cancel-race",
        "wait-on-socket",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    wait_for_file(&state_dir.join(S2_CENSUS_PAUSED))?;

    let provider = reservation_identity(&root.join("natural-cancel-race-writer"), "provider")?;
    assert_eq!(
        stop(&handle, StopReason::ExplicitCancellation)?,
        StopReceipt::Requested
    );
    drop(held);
    wait_for_zombie(&provider)?;
    std::fs::write(
        state_dir.join(S2_CENSUS_RELEASE),
        b"resume after natural exit",
    )?;

    let exit = exited(&handle)?;
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    assert_eq!(exit.stop_reason, None);
    assert!(exit.delivered_signals.is_empty());
    Ok(())
}

fn bounded_reap(child: &mut std::process::Child) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(io::Error::other("fixture did not reap").into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}
fn owner_death_keeps_guardian_and_writer_live(root: &Path) -> Result<()> {
    let listener = UnixListener::bind(root.join("orphan.sock"))?;
    let owner_listener = UnixListener::bind(root.join("owner-ready.sock"))?;
    let mut owner = fixture::ChildGuard::new(fixture::command("s2-owner", root)?.spawn()?);
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    let mut ready = fixture::fixture_accept(&owner_listener, Duration::from_secs(5))?;
    byte(&mut ready, b'R')?;
    owner.kill()?;
    bounded_reap(&mut owner)?;
    drop(ready);
    let clock = SystemClock::new();
    let lock = Directory::open_anchor(&root.join("orphan-writer"))?.lock(
        OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
        WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
        &CancelToken::new(),
        &clock,
    );
    assert!(matches!(
        lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. })
    ));
    assert!(!root.join("orphan-attempt/process-exit.json").exists());
    drop(held);
    let handle = reattach(
        Directory::open_anchor(&root.join("orphan-attempt"))?,
        scope(),
    )?;
    assert_eq!(
        exited(&handle)?.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    let guardian = reservation_identity(&root.join("orphan-writer"), "guardian")?;
    wait_for_identity_absence(&guardian)?;
    assert!(matches!(
        fixture::acquire_writer(
            Directory::open_anchor(&root.join("orphan-writer"))?,
            scope(),
            budget(&clock)?,
            &CancelToken::new(),
            &clock
        ),
        Err(CustodyError::Fenced)
    ));
    Ok(())
}
fn retry_is_busy_while_a_descendant_of_a_killed_guardian_is_alive(root: &Path) -> Result<()> {
    let descendant_socket = root.join("descendant.sock");
    let provider_socket = root.join("provider.sock");
    let descendant_listener = UnixListener::bind(&descendant_socket)?;
    let provider_listener = UnixListener::bind(&provider_socket)?;
    let (writer, _handle) = start(
        root,
        "inherited",
        "writer-lock-provider",
        &[
            descendant_socket.into_os_string(),
            provider_socket.into_os_string(),
        ],
        CapturePlan::default(),
    )?;
    let mut descendant_gate =
        fixture::fixture_accept(&descendant_listener, Duration::from_secs(5))?;
    byte(&mut descendant_gate, b'R')?;
    let descendant = process_identity(read_pid_line(&mut descendant_gate)?)?;
    let mut provider_gate = fixture::fixture_accept(&provider_listener, Duration::from_secs(5))?;
    byte(&mut provider_gate, b'R')?;
    let writer_path = root.join("inherited-writer");
    let guardian = reservation_identity(&writer_path, "guardian")?;
    let provider = reservation_identity(&writer_path, "provider")?;
    drop(writer);
    fixture::signal_recorded_process(&guardian, libc::SIGKILL)?;
    wait_for_identity_absence(&guardian)?;
    drop(provider_gate);
    wait_for_identity_absence(&provider)?;
    let clock = SystemClock::new();
    let retry = Scope {
        attempt: AttemptId::from_bytes([9; 16]),
        ..scope()
    };
    let admit = || {
        fixture::acquire_writer(
            Directory::open_anchor(&writer_path)?,
            retry,
            WaitBudget::nonblocking(provenant_core::Clock::mono_now(&clock)),
            &CancelToken::new(),
            &clock,
        )
    };
    assert!(matches!(admit(), Err(CustodyError::Busy)));
    assert!(!root.join("inherited-attempt/process-exit.json").exists());
    drop(descendant_gate);
    wait_for_identity_absence(&descendant)?;
    admit()?.withdraw_unlaunched()?;
    assert!(
        writer_path
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .exists()
    );
    Ok(())
}

fn guardian_loss_never_invents_exit_or_releases_the_reservation(root: &Path) -> Result<()> {
    let path = root.join("crash.sock");
    let listener = UnixListener::bind(&path)?;
    let (writer, _handle) = start(
        root,
        "crash",
        "crash-guardian-provider",
        &[path.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    drop(writer);
    let clock = SystemClock::new();
    // The live provider retains the inherited lease after guardian loss.
    let writer_path = root.join("crash-writer");
    let provider = process_identity(read_pid_line(&mut held)?)?;
    let locked = acquire_recovery(
        Directory::open_anchor(&writer_path)?,
        scope(),
        WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
        &CancelToken::new(),
        &clock,
    );
    assert!(matches!(
        locked,
        Err(CustodyError::Filesystem(
            provenant_os::fs::FsError::LockTimeout { .. }
        ))
    ));
    assert!(!root.join("crash-attempt/process-exit.json").exists());
    drop(held);
    wait_for_identity_absence(&provider)?;
    let recovery = acquire_recovery(
        Directory::open_anchor(&root.join("crash-writer"))?,
        scope(),
        budget(&clock)?,
        &CancelToken::new(),
        &clock,
    )?;
    assert_eq!(
        fixture::recovery_observation(&recovery),
        RecoveryState::Unknown
    );
    assert!(!root.join("crash-attempt/process-exit.json").exists());
    let unrelated = tempfile::tempdir_in(root)?;
    assert!(
        recovery
            .verify_settlement(&Directory::open_anchor(unrelated.path())?)
            .is_err()
    );
    drop(recovery);
    assert!(
        root.join("crash-writer")
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .exists()
    );
    Ok(())
}
fn capture_failure_never_requests_a_stop(root: &Path) -> Result<()> {
    let socket = root.join("capture-fault.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "capture-fault",
        "wait-on-socket",
        &[socket.into_os_string()],
        CapturePlan::default(),
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    std::fs::create_dir(root.join("capture-fault-attempt/stdout.log"))?;
    drop(held);
    let exit = exited(&handle)?;
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Exit(0))
    );
    assert_eq!(exit.stop_reason, None);
    Ok(())
}
fn wall_deadline_is_the_execution_bound(root: &Path) -> Result<()> {
    let socket = root.join("wall.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "wall",
        "socket-blocked",
        &[socket.into_os_string()],
        CapturePlan {
            wall_ms: 1000,
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    let exit = exited(&handle)?;
    assert_eq!(exit.stop_reason, Some(StopReason::WallDeadline));
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Signal(9))
    );
    Ok(())
}

/// A host that slept through most of the deadline: the monotonic clock saw a
/// few seconds, the wall clock hours. The attempt still stops as a timeout.
fn a_host_sleep_cannot_extend_the_wall_deadline(root: &Path) -> Result<()> {
    let home = root.join("HOME");
    std::fs::create_dir_all(&home)?;
    std::fs::write(home.join(S2_WALL_JUMP), b"7200")?;
    let socket = root.join("sleep.sock");
    let listener = UnixListener::bind(&socket)?;
    let (_writer, handle) = start(
        root,
        "sleep",
        "socket-blocked",
        &[socket.into_os_string()],
        CapturePlan {
            // An hour of monotonic time would not pass inside this test.
            wall_ms: 3_600_000,
            root_flush_ms: 30,
            descendant_grace_ms: 10,
            ..CapturePlan::default()
        },
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    byte(&mut held, b'R')?;
    // The host sleeps: the wall clock moves two hours, the monotonic one not at all.
    std::fs::write(home.join(S2_WALL_JUMP_NOW), b"asleep")?;
    let exit = exited(&handle)?;
    assert_eq!(exit.stop_reason, Some(StopReason::WallDeadline));
    assert_eq!(
        exit.termination,
        Some(provenant_os::exec::Termination::Signal(9))
    );
    Ok(())
}

type Case = (&'static str, fn(&Path) -> Result<()>);
const CASES: &[Case] = &[
    (
        "guardian_sigkill_recovery_resumes_a_build_after_ancestry_is_lost",
        guardian_sigkill_recovery_resumes_a_build_after_ancestry_is_lost,
    ),
    (
        "disk_floor_pauses_and_recovery_resumes_builds",
        disk_floor_pauses_and_recovery_resumes_builds,
    ),
    (
        "disk_sample_failure_resumes_paused_builds",
        disk_sample_failure_resumes_paused_builds,
    ),
    (
        "cancel_resumes_paused_builds_before_delivering_stop",
        cancel_resumes_paused_builds_before_delivering_stop,
    ),
    (
        "provider_exit_resumes_paused_builds",
        provider_exit_resumes_paused_builds,
    ),
    (
        "live_provider_publishes_initial_checkpoint",
        live_provider_publishes_initial_checkpoint,
    ),
    (
        "held_stop_ignores_lane_written_summary_and_cancel_request",
        held_stop_ignores_lane_written_summary_and_cancel_request,
    ),
    (
        "held_stop_replaces_what_a_lane_left_but_reattach_keeps_a_later_generation",
        held_stop_replaces_what_a_lane_left_but_reattach_keeps_a_later_generation,
    ),
    (
        "descendant_journal_failure_is_reported",
        descendant_journal_failure_is_reported,
    ),
    (
        "writer_is_fenced_after_every_descriptor_closes",
        writer_is_fenced_after_every_descriptor_closes,
    ),
    (
        "held_custody_is_released_only_after_guardian_exit",
        held_custody_is_released_only_after_guardian_exit,
    ),
    (
        "normal_exit_and_terminal_beyond_capture_cap",
        normal_exit_and_terminal_beyond_capture_cap,
    ),
    (
        "cancelled_reader_preserves_healthy_custody_and_cancel_escalates",
        cancelled_reader_preserves_healthy_custody_and_cancel_escalates,
    ),
    (
        "publication_failure_retains_the_lease_and_retries_without_overwrite",
        publication_failure_retains_the_lease_and_retries_without_overwrite,
    ),
    (
        "never_spawned_cancel_is_published",
        never_spawned_cancel_is_published,
    ),
    (
        "terminal_frame_closes_provider_stdin",
        terminal_frame_closes_provider_stdin,
    ),
    (
        "owner_death_keeps_guardian_and_writer_live",
        owner_death_keeps_guardian_and_writer_live,
    ),
    (
        "retry_is_busy_while_a_descendant_of_a_killed_guardian_is_alive",
        retry_is_busy_while_a_descendant_of_a_killed_guardian_is_alive,
    ),
    (
        "guardian_loss_never_invents_exit_or_releases_the_reservation",
        guardian_loss_never_invents_exit_or_releases_the_reservation,
    ),
    (
        "capture_failure_never_requests_a_stop",
        capture_failure_never_requests_a_stop,
    ),
    (
        "wall_deadline_is_the_execution_bound",
        wall_deadline_is_the_execution_bound,
    ),
    (
        "setsid_descendant_is_tracked_before_exit_and_cleanup_is_conservative",
        setsid_descendant_is_tracked_before_exit_and_cleanup_is_conservative,
    ),
    #[cfg(target_os = "macos")]
    (
        "escapee_is_ended_beside_an_unprovable_nested_owner_without_repeated_failures",
        escapee_is_ended_beside_an_unprovable_nested_owner_without_repeated_failures,
    ),
    #[cfg(target_os = "macos")]
    (
        "the_guardian_names_an_unreadable_host_clock_in_its_admission_refusal",
        the_guardian_names_an_unreadable_host_clock_in_its_admission_refusal,
    ),
    #[cfg(target_os = "macos")]
    (
        "escapee_is_ended_only_when_the_host_reports_no_nested_runs",
        escapee_is_ended_only_when_the_host_reports_no_nested_runs,
    ),
    #[cfg(target_os = "macos")]
    (
        "detached_descendant_in_the_attempt_sandbox_is_found_and_ended",
        detached_descendant_in_the_attempt_sandbox_is_found_and_ended,
    ),
    (
        "cancel_kills_the_whole_group_and_settles",
        cancel_kills_the_whole_group_and_settles,
    ),
    (
        "natural_exit_ends_same_group_leftovers_and_settles",
        natural_exit_ends_same_group_leftovers_and_settles,
    ),
    (
        "cancel_after_unresolved_exit_republishes_a_settled_exit",
        cancel_after_unresolved_exit_republishes_a_settled_exit,
    ),
    (
        "nested_owner_stays_protected_when_native_custody_proof_is_uncertain",
        nested_owner_stays_protected_when_native_custody_proof_is_uncertain,
    ),
    (
        "stale_complete_census_does_not_signal_a_late_shared_group_owner",
        stale_complete_census_does_not_signal_a_late_shared_group_owner,
    ),
    (
        "guardian_sigterm_uses_supervisor_signal_path",
        guardian_sigterm_uses_supervisor_signal_path,
    ),
    (
        "guardian_sigint_uses_supervisor_signal_path",
        guardian_sigint_uses_supervisor_signal_path,
    ),
    (
        "guardian_sighup_uses_supervisor_signal_path",
        guardian_sighup_uses_supervisor_signal_path,
    ),
    (
        "checkpoint_failure_preserves_terminal_publication",
        checkpoint_failure_preserves_terminal_publication,
    ),
    (
        "checkpoint_enospc_while_provider_lives_never_signals_it",
        checkpoint_enospc_while_provider_lives_never_signals_it,
    ),
    (
        "a_host_sleep_cannot_extend_the_wall_deadline",
        a_host_sleep_cannot_extend_the_wall_deadline,
    ),
    ("natural_exit_races_cancel", natural_exit_races_cancel),
    (
        "immediate_exit_identity_survives_absent_observation",
        immediate_exit_identity_survives_absent_observation,
    ),
    (
        "cleanup_retries_a_transient_census_failure",
        cleanup_retries_a_transient_census_failure,
    ),
];

fn settle_writers(root: &Path) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with("-writer"))
        {
            let clock = SystemClock::new();
            let _settled = Directory::open_anchor(&entry.path())?.lock(
                OsStr::new("writer.lock"),
                LockMode::Exclusive,
                budget(&clock)?,
                &CancelToken::new(),
                &clock,
            )?;
        }
    }
    Ok(())
}

fn run_case(name: &str) -> Result<()> {
    let root = provenant_os::fixture::short_tempdir("terminal-checkpoint.sock", |root| {
        tempfile::Builder::new().prefix("s").tempdir_in(root)
    })?;
    let cleanup = fixture::ProcessCleanupGuard::new(root.path());
    let case = CASES
        .iter()
        .find(|(label, _)| *label == name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown custody test"))?;
    let result = (case.1)(root.path());
    if result.is_ok() {
        settle_writers(root.path())?;
        cleanup.disarm();
    }
    result
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "internal")
        && args.get(1).is_some_and(|a| a == "supervise")
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        // The production shape wires no registry; only the host's child store.
        let production_shape = home.join(S2_PRODUCTION_SHAPE).is_file();
        let store = FileNestedChildren { home: home.clone() };
        let refusal_marker = home.join(S2_REFUSE_ADMISSION);
        let jump_marker = home.join(S2_WALL_JUMP);
        let jump = std::fs::read_to_string(&jump_marker)
            .ok()
            .and_then(|text| text.trim().parse::<i64>().ok());
        // The jump starts once the test has the provider connected: a stop at
        // once would end it before it could.
        let sleeping = home.join(S2_WALL_JUMP_NOW);
        let jumping = move || {
            let now = provenant_core::Clock::wall_now(&SystemClock::new()).ok()?;
            let offset = if sleeping.exists() {
                jump.unwrap_or(0)
            } else {
                0
            };
            provenant_core::WallTime::from_micros(now.as_micros() + offset * 1_000_000).ok()
        };
        let registry = FileChildRegistry { home };
        let observer = CensusGateObserver;
        let disk = std::env::var_os("PROVENANT_STATE_DIR")
            .map(PathBuf::from)
            .filter(|state| state.join(S2_DISK_PRESSURE).is_file())
            .map(|state| MarkerDiskPressure { state });
        let publisher = match std::env::var_os("PROVENANT_STATE_DIR") {
            Some(path) => fixture::ExitPublicationFaults::from_state_dir(&PathBuf::from(path))?,
            None => None,
        };
        let refusal = match std::fs::read(&refusal_marker) {
            Ok(marker) => Some(RefusingHolder {
                clock: marker == b"clock",
            }),
            Err(_) => None,
        };
        provenant_supervisor::supervise(
            &args[2..],
            GuardianHooks {
                holder: refusal
                    .as_ref()
                    .map(|holder| holder as &dyn provenant_supervisor::Holder),
                children: (!production_shape).then_some(&registry as &dyn ChildRegistry),
                nested_children: production_shape.then_some(&store as &dyn NestedChildStore),
                observer: Some(&observer),
                wall_clock: jump.is_some().then_some(&jumping as &dyn Fn() -> Option<_>),
                disk: disk
                    .as_ref()
                    .map(|disk| disk as &dyn provenant_supervisor::DiskPressure),
                exit_publisher: publisher.as_ref().map(|item| item as &dyn ExitPublisher),
                stdout_decoder: Some(&Decoder),
                ..Default::default()
            },
        )?;
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--fixture")
        && args.get(1).is_some_and(|a| a == "s2-owner")
    {
        return owner_process(&std::env::current_dir()?);
    }
    if fixture::dispatch()? {
        return Ok(());
    }
    let names: Vec<_> = CASES
        .iter()
        .filter(|_| cfg!(target_os = "macos"))
        .map(|(name, _)| *name)
        .collect();
    fixture::run_libtest(&args, &names, |name| {
        run_case(name).map_err(|error| io::Error::other(error.to_string()))
    })?;
    Ok(())
}
